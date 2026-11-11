//! Replace in project: the replace row of the project search picker (`Ctrl+Shift+H`, or `Ctrl+H`
//! inside the picker), a preview of every replacement (old text struck through, new text
//! highlighted), and replacing one match (Enter in the replace field), every match of a file
//! (`Ctrl+Enter`) or everything (`Ctrl+Alt+Enter`, after a "Replace 42 matches in 7 files?"
//! question). Open documents are edited in their buffer (one undo step per file, left unsaved);
//! other files are rewritten on disk atomically by `replace.rs`, which keeps their line breaks.
//! The files are the ones project search walks (`.gitignore` and the skip list respected).

use super::*;
use crate::{
    buffer::Sel,
    replace::{self, Matcher},
    ui::TextCtx,
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

/// Height of the replace row under the search input.
pub const REP_H: i32 = 44;

pub struct ReplaceUi {
    pub field: TextField,
    /// Typing goes to the replace field (Tab switches).
    pub focus: bool,
    /// The search results behind `Picker::items`, index for index (one row per match).
    pub hits: Vec<Hit>,
    /// The query `hits` answer (replacing uses it, not fields edited since).
    pub query: search::Query,
    /// The search behind `hits` finished (`hits` is complete unless it hit the cap).
    pub done: Arc<AtomicBool>,
    /// "Replace all" asked: (matches, files, more than shown) until Enter / Esc.
    pub confirm: Option<(usize, usize, bool)>,
    /// Row to select again when refreshed results come in: (path, line, start byte).
    pub want: Option<(PathBuf, usize, usize)>,
}

impl ReplaceUi {
    fn new(focus: bool) -> Box<ReplaceUi> {
        Box::new(ReplaceUi {
            field: TextField::default(),
            focus,
            hits: Vec::new(),
            query: search::Query::default(),
            done: Arc::new(AtomicBool::new(true)),
            confirm: None,
            want: None,
        })
    }
}

/// The question before replacing everything: "Replace 42 matches in 7 files?".
pub fn confirm_text(n: usize, files: usize, more: bool) -> String {
    let plus = if more { "+" } else { "" };
    let m = if n == 1 && !more { "match" } else { "matches" };
    let f = if files == 1 && !more { "file" } else { "files" };
    format!("Replace {n}{plus} {m} in {files}{plus} {f}?")
}

/// Position `p` after `edits` (ascending, non-overlapping char ranges with their new text); a
/// position inside a replaced range moves to its start.
fn map_pos(p: usize, edits: &[(usize, usize, String)]) -> usize {
    let mut shift: isize = 0;
    for (a, b, t) in edits {
        if *b <= p {
            shift += t.chars().count() as isize - (*b - *a) as isize;
        } else if *a < p {
            return (*a as isize + shift).max(0) as usize;
        } else {
            break;
        }
    }
    (p as isize + shift).max(0) as usize
}

/// Apply `edits` (ascending char ranges) to a document as one undo step; the cursor follows.
fn edit_doc(d: &mut crate::doc::Doc, edits: &[(usize, usize, String)]) {
    if edits.is_empty() {
        return;
    }
    let before = d.ed.sel;
    d.buf.begin_compound(before);
    for (a, b, t) in edits.iter().rev() {
        d.buf.replace(*a, *b, t, EditKind::Other, before);
    }
    d.ed.clear_extra();
    d.ed.sel = Sel { anchor: map_pos(before.anchor, edits), head: map_pos(before.head, edits) };
    d.buf.end_compound(d.ed.sel);
    d.sync_edits();
}

/// Line `l` of a document as search sees it (a leading BOM is not text) and the char index it starts at.
fn doc_line(d: &crate::doc::Doc, l: usize) -> (String, usize) {
    let line = d.buf.line(l);
    let ls = d.buf.line_start(l);
    match line.strip_prefix('\u{feff}') {
        Some(rest) if l == 0 => (rest.to_string(), ls + 1),
        _ => (line, ls),
    }
}

/// Every match of a document as char-range edits.
fn doc_edits(d: &crate::doc::Doc, m: &Matcher, rep: &str) -> Vec<(usize, usize, String)> {
    let eol = d.buf.eol();
    let mut edits = Vec::new();
    for l in 0..d.buf.line_count() {
        let (line, ls) = doc_line(d, l);
        let (mut chars, mut last) = (0, 0);
        for x in m.matches(&line, rep) {
            chars += line[last..x.start].chars().count();
            let len = line[x.start..x.end].chars().count();
            edits.push((ls + chars, ls + chars + len, replace::fit_eol(&x.rep, eol)));
            chars += len;
            last = x.end;
        }
    }
    edits
}

impl App {
    /// Text of the documents with unsaved changes by path (also canonical), searched instead of the disk.
    pub fn unsaved_texts(&self) -> HashMap<PathBuf, String> {
        let mut out = HashMap::new();
        for d in self.docs.iter().filter(|d| d.buf.is_dirty()) {
            let Some(p) = d.path() else { continue };
            let t = d.buf.to_string();
            if let Ok(c) = crate::paths::canonical(p)
                && c != p
            {
                out.insert(c, t.clone());
            }
            out.insert(p.to_path_buf(), t);
        }
        out
    }

    /// The open document of `path`, if any.
    fn doc_for(&self, path: &Path) -> Option<usize> {
        self.docs.iter().position(|d| d.path() == Some(path)).or_else(|| {
            let c = crate::paths::canonical(path).ok()?;
            self.docs.iter().position(|d| d.path().and_then(|p| crate::paths::canonical(p).ok()).as_ref() == Some(&c))
        })
    }

    /// `Ctrl+Shift+H`: project search with the replace row open.
    pub fn open_project_replace(&mut self) {
        self.open_project_search();
        if let Overlay::Picker(p) = &mut self.overlay {
            p.rep = Some(ReplaceUi::new(!p.field.text.is_empty()));
        }
        self.start_search();
    }

    /// Keys of the project search picker beyond plain field editing: `Ctrl+H` toggles the replace
    /// row, `Alt+W` whole words, Tab switches fields, Enter / `Ctrl+Enter` / `Ctrl+Alt+Enter`
    /// replace, and the replace field's own editing. Returns true when consumed.
    pub(super) fn project_search_key(&mut self, ev: &KeyIn) -> bool {
        let (ctrl, alt) = (self.mods.control_key(), self.mods.alt_key());
        let Overlay::Picker(p) = &mut self.overlay else { return false };
        if p.kind != PickKind::ProjectSearch {
            return false;
        }
        let key = &ev.key;
        let ch = match key {
            Key::Character(c) => c.to_lowercase(),
            _ => String::new(),
        };
        // The "replace all" question owns the keyboard until it is answered.
        if p.rep.as_ref().is_some_and(|r| r.confirm.is_some()) {
            match key {
                Key::Named(NamedKey::Enter) => self.replace_all_confirmed(),
                _ if ch == "y" || ch == "s" => self.replace_all_confirmed(),
                Key::Named(NamedKey::Escape) => {
                    if let Some(r) = p.rep.as_mut() {
                        r.confirm = None;
                    }
                }
                _ if ch == "n" => {
                    if let Some(r) = p.rep.as_mut() {
                        r.confirm = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if ctrl && !alt && ch == "h" {
            p.rep = if p.rep.is_some() { None } else { Some(ReplaceUi::new(!p.field.text.is_empty())) };
            self.start_search();
            return true;
        }
        if alt && !ctrl && ch == "w" {
            p.word = !p.word;
            self.search_due = Some(Instant::now() + std::time::Duration::from_millis(180));
            return true;
        }
        let Some(r) = p.rep.as_mut() else { return false };
        match key {
            Key::Named(NamedKey::Tab) => {
                r.focus = !r.focus;
                return true;
            }
            Key::Named(NamedKey::Enter) if ctrl && alt => {
                self.replace_all_ask();
                return true;
            }
            Key::Named(NamedKey::Enter) if ctrl => {
                self.replace_selected_file();
                return true;
            }
            Key::Named(NamedKey::Enter) if r.focus => {
                self.replace_selected();
                return true;
            }
            _ => {}
        }
        if !r.focus {
            return false;
        }
        let paste = (ctrl && ch == "v").then(|| self.clip.get());
        let Overlay::Picker(p) = &mut self.overlay else { return false };
        let Some(r) = p.rep.as_mut() else { return false };
        let f = &mut r.field;
        let changed = if let Some(t) = paste {
            f.insert(&t.replace('\n', " "));
            true
        } else {
            match key {
                // The picker keeps Esc (close) and the list keys.
                Key::Named(
                    NamedKey::Escape | NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::PageUp | NamedKey::PageDown,
                ) => return false,
                Key::Named(NamedKey::Backspace) => {
                    if ctrl {
                        f.delete_word()
                    } else {
                        f.backspace()
                    }
                }
                Key::Named(NamedKey::Delete) => f.delete(),
                Key::Named(NamedKey::ArrowLeft) => {
                    f.left();
                    false
                }
                Key::Named(NamedKey::ArrowRight) => {
                    f.right();
                    false
                }
                Key::Named(NamedKey::Home) => {
                    f.home();
                    false
                }
                Key::Named(NamedKey::End) => {
                    f.end();
                    false
                }
                _ if ctrl && ch == "a" => {
                    f.select_all();
                    false
                }
                // Alt+C / Alt+R toggle the options in either field.
                _ if alt && !ctrl && (ch == "c" || ch == "r") => return false,
                _ if !ctrl && !alt => match &ev.text {
                    Some(t) if !t.is_empty() && !t.chars().any(char::is_control) => {
                        f.insert(t);
                        true
                    }
                    _ => false,
                },
                _ => false,
            }
        };
        if changed {
            // New previews: the search runs again (debounced) and keeps the selected row.
            r.want = r.hits.get(p.sel).map(|h| (h.path.clone(), h.line, h.start));
            self.search_due = Some(Instant::now() + std::time::Duration::from_millis(180));
        }
        true
    }

    /// Matcher and replacement for the shown results. When the search fields changed since the
    /// results came in, the search runs again instead (nothing is replaced from stale rows).
    fn replace_ctx(&mut self) -> Option<(Matcher, String)> {
        let Overlay::Picker(p) = &self.overlay else { return None };
        let r = p.rep.as_ref()?;
        let q = &r.query;
        if (q.text.as_str(), q.case, q.regex, q.word) != (p.field.text.as_str(), p.case, p.regex, p.word) {
            self.search_due = None;
            self.start_search();
            self.say("the search changed: results refreshed");
            return None;
        }
        let rep = r.field.text.clone();
        match Matcher::new(&q.text, q.case, q.regex, q.word) {
            Ok(m) => Some((m, rep)),
            Err(e) => {
                self.say(format!("invalid search: {e}"));
                None
            }
        }
    }

    /// Replace the match of one result row. `Ok(None)`: it is no longer there.
    fn replace_hit(&mut self, m: &Matcher, rep: &str, hit: &Hit) -> std::io::Result<Option<String>> {
        let Some(i) = self.doc_for(&hit.path) else {
            return replace::replace_one_in_file(&hit.path, m, rep, hit.line, hit.start, hit.end);
        };
        let d = &mut self.docs[i];
        if hit.line >= d.buf.line_count() {
            return Ok(None);
        }
        let (line, ls) = doc_line(d, hit.line);
        let Some(x) = m.matches(&line, rep).into_iter().find(|x| x.start == hit.start && x.end == hit.end) else {
            return Ok(None);
        };
        let text = replace::fit_eol(&x.rep, d.buf.eol());
        let a = ls + line[..x.start].chars().count();
        let b = a + line[x.start..x.end].chars().count();
        edit_doc(d, &[(a, b, text.clone())]);
        Ok(Some(text))
    }

    /// Replace every match of a file: in its buffer when open, on disk otherwise.
    fn replace_path(&mut self, path: &Path, m: &Matcher, rep: &str) -> std::io::Result<usize> {
        match self.doc_for(path) {
            Some(i) => {
                let d = &mut self.docs[i];
                let edits = doc_edits(d, m, rep);
                edit_doc(d, &edits);
                Ok(edits.len())
            }
            None => replace::replace_file(path, m, rep),
        }
    }

    /// Enter in the replace field: replace the selected match, then select the next one.
    pub(super) fn replace_selected(&mut self) {
        let Some((m, rep)) = self.replace_ctx() else { return };
        let Overlay::Picker(p) = &self.overlay else { return };
        let sel = p.sel;
        let Some(hit) = p.rep.as_ref().and_then(|r| r.hits.get(sel)).cloned() else { return };
        match self.replace_hit(&m, &rep, &hit) {
            Ok(Some(new)) => {
                let delta = new.len() as isize - (hit.end - hit.start) as isize;
                let cdelta = new.chars().count() as isize - (hit.mark.1 - hit.mark.0) as isize;
                if let Overlay::Picker(p) = &mut self.overlay
                    && let Some(r) = p.rep.as_mut()
                {
                    r.hits.remove(sel);
                    p.items.remove(sel);
                    // Later matches of the same line moved by the length change.
                    for (i, h) in r.hits.iter_mut().enumerate() {
                        if h.path == hit.path && h.line == hit.line && h.start >= hit.end {
                            h.start = (h.start as isize + delta) as usize;
                            h.end = (h.end as isize + delta) as usize;
                            h.col = (h.col as isize + cdelta).max(0) as usize;
                            p.items[i].action = Action::Open(h.path.clone(), Some((h.line, h.col)));
                        }
                    }
                    r.want = None;
                    p.refilter();
                    p.sel = sel.min(p.filtered.len().saturating_sub(1));
                    p.keep_visible();
                }
                self.say(format!("replaced 1 match in {}", self.rel(&hit.path)));
            }
            Ok(None) => {
                self.start_search();
                self.say("the file changed since the search: results refreshed");
            }
            Err(e) => self.say(format!("could not replace in {}: {e}", self.rel(&hit.path))),
        }
    }

    /// `Ctrl+Enter`: replace every match of the selected row's file.
    pub(super) fn replace_selected_file(&mut self) {
        let Some((m, rep)) = self.replace_ctx() else { return };
        let Overlay::Picker(p) = &self.overlay else { return };
        let Some(path) = p.rep.as_ref().and_then(|r| r.hits.get(p.sel)).map(|h| h.path.clone()) else { return };
        match self.replace_path(&path, &m, &rep) {
            Ok(n) => {
                if let Overlay::Picker(p) = &mut self.overlay
                    && let Some(r) = p.rep.as_mut()
                {
                    let keep: Vec<bool> = r.hits.iter().map(|h| h.path != path).collect();
                    let mut k = keep.iter();
                    r.hits.retain(|_| *k.next().unwrap_or(&true));
                    let mut k = keep.iter();
                    p.items.retain(|_| *k.next().unwrap_or(&true));
                    r.want = None;
                    let sel = p.sel;
                    p.refilter();
                    p.sel = sel.min(p.filtered.len().saturating_sub(1));
                    p.keep_visible();
                }
                let s = if n == 1 { "" } else { "s" };
                self.say(format!("{n} replacement{s} in {}", self.rel(&path)));
            }
            Err(e) => self.say(format!("could not replace in {}: {e}", self.rel(&path))),
        }
    }

    /// `Ctrl+Alt+Enter`: ask "Replace 42 matches in 7 files?" (answered in `project_search_key`).
    pub(super) fn replace_all_ask(&mut self) {
        if self.replace_ctx().is_none() {
            return;
        }
        let Overlay::Picker(p) = &mut self.overlay else { return };
        let Some(r) = p.rep.as_mut() else { return };
        if !r.done.load(Ordering::SeqCst) {
            return self.say("the search is still running");
        }
        if r.hits.is_empty() {
            return self.say("nothing to replace");
        }
        let files = {
            let mut v: Vec<&Path> = r.hits.iter().map(|h| h.path.as_path()).collect();
            v.dedup();
            v.len()
        };
        r.confirm = Some((r.hits.len(), files, r.hits.len() >= search::MAX_REPLACE_HITS));
    }

    /// The question was answered yes: replace in every file with matches (open documents in their
    /// buffer). With more matches than the list holds, every file the search walks is visited.
    pub(super) fn replace_all_confirmed(&mut self) {
        let Overlay::Picker(p) = &mut self.overlay else { return };
        let Some((_, _, more)) = p.rep.as_mut().and_then(|r| r.confirm.take()) else { return };
        let Some((m, rep)) = self.replace_ctx() else { return };
        let paths: Vec<PathBuf> = if more {
            search::list_files(&self.root, usize::MAX)
        } else {
            let Overlay::Picker(p) = &self.overlay else { return };
            let mut v: Vec<PathBuf> = Vec::new();
            for h in p.rep.as_ref().map_or(&[][..], |r| &r.hits[..]) {
                if v.last() != Some(&h.path) {
                    v.push(h.path.clone());
                }
            }
            v
        };
        let (mut n, mut files, mut failed) = (0, 0, Vec::new());
        for path in paths {
            if more
                && self.doc_for(&path).is_none()
                && !matches!(std::fs::metadata(&path), Ok(md) if md.len() <= search::MAX_FILE)
            {
                continue;
            }
            match self.replace_path(&path, &m, &rep) {
                Ok(0) => {}
                Ok(k) => {
                    n += k;
                    files += 1;
                }
                // Walking everything meets binary and non-UTF-8 files: those are simply skipped.
                Err(_) if more => {}
                Err(e) => failed.push(format!("{} ({e})", self.rel(&path))),
            }
        }
        let mut msg = format!(
            "replaced {n} {} in {files} {}",
            if n == 1 { "match" } else { "matches" },
            if files == 1 { "file" } else { "files" }
        );
        if !failed.is_empty() {
            msg.push_str(&format!("; could not change {}", failed.join(", ")));
        }
        self.say(msg);
        // Whatever is left (or matches the new text) shows up again.
        self.start_search();
    }
}

/// The replace row under the search input: the replace field, or the "replace all" question.
pub(super) fn draw_replace_row(cv: &mut Canvas, tx: &mut TextCtx, theme: &crate::theme::Theme, r: Rect, p: &Picker) {
    let Some(rp) = p.rep.as_ref() else { return };
    let c = theme.chrome;
    let (fg, dim) = (theme.foreground, c.tab_inactive_fg);
    let y0 = r.y + INPUT_H;
    let input = Rect::new(r.x + 12, y0 + 4, r.w - 24, REP_H - 10);
    let ty = input.y + (input.h - 18) / 2;
    if let Some((n, files, more)) = rp.confirm {
        cv.card(input, 8, c.accent.with_alpha(0x30), c.accent);
        tx.draw_ui(cv, input.x + 12, ty, &confirm_text(n, files, more), fg, input, true);
        let hint = "Enter replace · Esc cancel";
        let hw = tx.ui_width(hint, false);
        tx.draw_ui(cv, input.right() - hw - 12, ty, hint, dim, input, false);
        return;
    }
    cv.card(input, 8, c.input_bg, if rp.focus { c.accent } else { c.border });
    let f = &rp.field;
    let tw = tx.ui_width(&f.text, false);
    if f.text.is_empty() {
        tx.draw_ui(cv, input.x + 10, ty, "Replace", dim, input, false);
    } else {
        if f.sel_all {
            cv.fill_round(Rect::new(input.x + 8, ty - 2, tw + 4, 22), 4, c.accent.with_alpha(0x50));
        }
        tx.draw_ui(cv, input.x + 10, ty, &f.text, fg, input.inset(1), false);
    }
    if rp.focus {
        let prefix: String = f.text.chars().take(f.cursor).collect();
        let x = input.x + 10 + tx.ui_width(&prefix, false);
        cv.fill_clip(Rect::new(x, ty, 1, 18), fg, input);
    }
    let hint = "Enter replace · Ctrl+Enter file · Ctrl+Alt+Enter all";
    let hw = tx.ui_width(hint, false);
    if tw + hw + 40 < input.w {
        tx.draw_ui(cv, input.right() - hw - 10, ty, hint, dim, input, false);
    }
}

/// One result row in replace mode: the line with the old text struck through and the new text
/// highlighted, the file and line on the right.
pub(super) fn draw_preview(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    theme: &crate::theme::Theme,
    rr: Rect,
    ly: i32,
    h: &Hit,
    detail: &str,
) {
    let c = theme.chrome;
    let (fg, dim) = (theme.foreground, c.tab_inactive_fg);
    let chars: Vec<char> = h.text.chars().collect();
    let b = h.mark.1.min(chars.len());
    let a = h.mark.0.min(b);
    let old: String = chars[a..b].iter().collect();
    let new = h.rep.as_deref().unwrap_or("").replace("\r\n", "↵").replace('\n', "↵");
    let after: String = chars[b..].iter().collect();
    let dw = tx.ui_width(detail, false).min(rr.w * 2 / 5);
    let dx = rr.right() - 10 - dw;
    let x0 = rr.x + 14;
    let room = dx - 16 - x0;
    let (ow, nw) = (tx.ui_width(&old, false), tx.ui_width(&new, false));
    // Keep the change in view: drop leading context until it fits.
    let before = |skip: usize| -> String {
        let mut s = if skip > 0 { "…".to_string() } else { String::new() };
        s.extend(&chars[skip..a]);
        s
    };
    let mut skip = 0;
    while skip < a && tx.ui_width(&before(skip), false) + ow + nw + 8 > room {
        skip += (a - skip).div_ceil(8).max(1);
    }
    let before = before(skip.min(a));
    let clip = Rect::new(rr.x, rr.y, (dx - 8 - rr.x).max(0), rr.h).intersect(&rr);
    let mut x = x0;
    tx.draw_ui(cv, x, ly, &before, fg, clip, false);
    x += tx.ui_width(&before, false);
    cv.fill_round_clip(Rect::new(x - 1, ly - 1, ow + 2, 20), 3, c.diff_del_word, clip);
    tx.draw_ui(cv, x, ly, &old, dim, clip, false);
    cv.fill_clip(Rect::new(x, ly + 9, ow, 1), fg, clip);
    x += ow + 3;
    if !new.is_empty() {
        cv.fill_round_clip(Rect::new(x - 1, ly - 1, nw + 2, 20), 3, c.diff_add_word, clip);
        tx.draw_ui(cv, x, ly, &new, fg, clip, false);
        x += nw + 3;
    }
    tx.draw_ui(cv, x, ly, &after, fg, clip, false);
    let mut det = detail.to_string();
    while tx.ui_width(&det, false) > dw && det.chars().count() > 3 {
        det.remove(0);
    }
    tx.draw_ui(cv, dx, ly, &det, dim, rr, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use winit::keyboard::{ModifiersState, SmolStr};

    #[test]
    fn question_and_cursor_mapping() {
        assert_eq!(confirm_text(42, 7, false), "Replace 42 matches in 7 files?");
        assert_eq!(confirm_text(1, 1, false), "Replace 1 match in 1 file?");
        assert_eq!(confirm_text(5000, 300, true), "Replace 5000+ matches in 300+ files?");
        let edits = vec![(2, 4, "xyz".to_string()), (10, 11, String::new())];
        assert_eq!(map_pos(1, &edits), 1);
        assert_eq!(map_pos(3, &edits), 2, "inside a replaced range: its start");
        assert_eq!(map_pos(4, &edits), 5);
        assert_eq!(map_pos(12, &edits), 12);
    }

    fn key(app: &mut App, mods: ModifiersState, k: Key, text: Option<&str>) {
        app.mods = mods;
        app.handle_key(&KeyIn { key: k, text: text.map(str::to_string) });
        app.mods = ModifiersState::empty();
    }

    fn typ(app: &mut App, s: &str) {
        for ch in s.chars() {
            let t = ch.to_string();
            key(app, ModifiersState::empty(), Key::Character(SmolStr::new(&t)), Some(&t));
        }
    }

    /// Run the debounced search now and wait for its results.
    fn settle_search(app: &mut App) {
        app.search_due = None;
        app.start_search();
        for _ in 0..500 {
            app.pump_background();
            if let Overlay::Picker(p) = &app.overlay
                && p.rep.as_ref().is_none_or(|r| r.done.load(Ordering::SeqCst))
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
                app.pump_background();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("search did not finish");
    }

    fn rows(app: &App) -> Vec<(String, usize, usize, String)> {
        let Overlay::Picker(p) = &app.overlay else { panic!("picker closed") };
        let r = p.rep.as_ref().expect("replace row");
        r.hits
            .iter()
            .map(|h| {
                (h.path.file_name().unwrap().to_string_lossy().into_owned(), h.line, h.start, h.rep.clone().unwrap())
            })
            .collect()
    }

    #[test]
    fn replace_in_project_end_to_end() {
        let _g = lock();
        let dir = tmp("preplace");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("ignored")).unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(dir.join("src/a.txt"), "let foo_1 = foo_2;\r\nfoo_3\r\n").unwrap();
        std::fs::write(dir.join("src/b.txt"), "foo_9\n").unwrap();
        std::fs::write(dir.join("src/c.txt"), "x foo_4 foo_5\n").unwrap();
        std::fs::write(dir.join("ignored/i.txt"), "foo_7\n").unwrap();
        let mut app = test_app(&dir);
        // b.rs is open with unsaved changes: its buffer is what gets searched and edited.
        app.open_path(&dir.join("src/b.txt"), None);
        let b = app.active;
        app.docs[b].replace_text("foo_8 foo_9\n");

        let none = ModifiersState::empty();
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        key(&mut app, cs, Key::Character("H".into()), None);
        let Overlay::Picker(p) = &app.overlay else { panic!("Ctrl+Shift+H opens project search") };
        assert!(p.rep.is_some() && p.kind == PickKind::ProjectSearch);
        typ(&mut app, r"foo_(\d)");
        key(&mut app, ModifiersState::ALT, Key::Character("r".into()), None); // regex on
        key(&mut app, none, Key::Named(NamedKey::Tab), None); // to the replace field
        typ(&mut app, "bar$1");
        settle_search(&mut app);
        let mut got = rows(&app);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("a.txt".into(), 0, 4, "bar1".into()),
                ("a.txt".into(), 0, 12, "bar2".into()),
                ("a.txt".into(), 1, 0, "bar3".into()),
                ("b.txt".into(), 0, 0, "bar8".into()),
                ("b.txt".into(), 0, 6, "bar9".into()),
                ("c.txt".into(), 0, 2, "bar4".into()),
                ("c.txt".into(), 0, 8, "bar5".into()),
            ],
            "every match, the buffer's text for b.rs, nothing from ignored/"
        );

        // Enter in the replace field: the selected match only.
        let pick = |app: &mut App, file: &str, start: usize| {
            let Overlay::Picker(p) = &mut app.overlay else { unreachable!() };
            let r = p.rep.as_ref().unwrap();
            p.sel = r.hits.iter().position(|h| h.path.ends_with(file) && h.start == start).unwrap();
        };
        pick(&mut app, "c.txt", 2);
        key(&mut app, none, Key::Named(NamedKey::Enter), None);
        assert_eq!(std::fs::read_to_string(dir.join("src/c.txt")).unwrap(), "x bar4 foo_5\n");
        // The other match of that line moved with the edit and still replaces correctly.
        pick(&mut app, "c.txt", 7);
        key(&mut app, none, Key::Named(NamedKey::Enter), None);
        assert_eq!(std::fs::read_to_string(dir.join("src/c.txt")).unwrap(), "x bar4 bar5\n");

        // Ctrl+Enter: a whole open file, in its buffer, as one undo step; the disk is untouched.
        pick(&mut app, "b.txt", 0);
        key(&mut app, ModifiersState::CONTROL, Key::Named(NamedKey::Enter), None);
        assert_eq!(app.docs[b].buf.to_string(), "bar8 bar9\n");
        assert!(app.docs[b].buf.is_dirty());
        assert_eq!(std::fs::read_to_string(dir.join("src/b.txt")).unwrap(), "foo_9\n");
        assert!(rows(&app).iter().all(|r| r.0 != "b.txt"), "its rows are gone");

        // Ctrl+Alt+Enter asks first; Esc keeps everything.
        let ca = ModifiersState::CONTROL | ModifiersState::ALT;
        key(&mut app, ca, Key::Named(NamedKey::Enter), None);
        let Overlay::Picker(p) = &app.overlay else { unreachable!() };
        assert_eq!(p.rep.as_ref().unwrap().confirm, Some((3, 1, false)));
        key(&mut app, none, Key::Named(NamedKey::Escape), None);
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.rep.as_ref().unwrap().confirm.is_none()));
        assert_eq!(std::fs::read_to_string(dir.join("src/a.txt")).unwrap(), "let foo_1 = foo_2;\r\nfoo_3\r\n");
        // ...and Enter replaces: the closed file is rewritten keeping its CRLF breaks.
        key(&mut app, ca, Key::Named(NamedKey::Enter), None);
        key(&mut app, none, Key::Named(NamedKey::Enter), None);
        assert_eq!(std::fs::read_to_string(dir.join("src/a.txt")).unwrap(), "let bar1 = bar2;\r\nbar3\r\n");
        assert_eq!(std::fs::read_to_string(dir.join("ignored/i.txt")).unwrap(), "foo_7\n", ".gitignore respected");
        settle_search(&mut app);
        assert!(rows(&app).is_empty(), "nothing left");

        // One undo restores the open file.
        app.overlay = Overlay::None;
        app.activate(b);
        key(&mut app, ModifiersState::CONTROL, Key::Character("z".into()), None);
        assert_eq!(app.docs[b].buf.to_string(), "foo_8 foo_9\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn whole_word_toggle_and_stale_rows() {
        let _g = lock();
        let dir = tmp("preplace-word");
        std::fs::write(dir.join("w.txt"), "cat catalog cat\n").unwrap();
        let mut app = test_app(&dir);
        app.open_project_replace();
        typ(&mut app, "cat");
        key(&mut app, ModifiersState::ALT, Key::Character("w".into()), None);
        key(&mut app, ModifiersState::empty(), Key::Named(NamedKey::Tab), None);
        typ(&mut app, "dog");
        settle_search(&mut app);
        let got: Vec<usize> = rows(&app).iter().filter(|r| r.0 == "w.txt").map(|r| r.2).collect();
        assert_eq!(got, vec![0, 12], "whole words only");
        // The file changes behind the list: the row no longer matches and nothing is replaced.
        std::fs::write(dir.join("w.txt"), "a cat catalog cat\n").unwrap();
        let Overlay::Picker(p) = &mut app.overlay else { unreachable!() };
        p.sel = p.rep.as_ref().unwrap().hits.iter().position(|h| h.path.ends_with("w.txt")).unwrap();
        key(&mut app, ModifiersState::empty(), Key::Named(NamedKey::Enter), None);
        assert_eq!(std::fs::read_to_string(dir.join("w.txt")).unwrap(), "a cat catalog cat\n");
        settle_search(&mut app);
        let got: Vec<usize> = rows(&app).iter().filter(|r| r.0 == "w.txt").map(|r| r.2).collect();
        assert_eq!(got, vec![2, 14], "refreshed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
