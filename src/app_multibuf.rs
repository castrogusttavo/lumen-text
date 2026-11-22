//! Multibuffer tabs in the app: opening one (project search results, LSP references), drawing it
//! with the editor's code rows and syntax colours (in the focused pane and in the other panes of a
//! split), and its input: a caret in the code of every excerpt (typing, Enter, Backspace, Delete,
//! arrows, Ctrl+Z / Ctrl+Y), a click on a header or Ctrl+Click / Ctrl+Enter to jump to the file,
//! wheel and Page keys to scroll. Ctrl+S writes the edited excerpts back to their files. The model
//! is `multibuf`.
//!
//! The tab is a document without a path and with a read-only, never-written buffer (like the image
//! tabs), carrying a `MultiBuf`; `Doc::draw` leaves it blank and `App::draw_mb_doc` paints it.
//! A file that is open in a tab gets the change as one undo step of its own buffer (and is saved
//! unless it already had unsaved edits); any other file is rewritten on disk atomically. A file
//! that no longer has the lines the excerpt was read with is left alone.

use crate::{
    app::{App, Focus, KeyIn},
    app_gitpanel::fit,
    buffer::Buffer,
    doc::{Doc, uri_to_path},
    highlight::{SharedTokenizer, highlight_text, language_for, make_tokenizer},
    multibuf::{self, Edit, Excerpt, Kind, Loc, MultiBuf, Row, Runs},
    search::{self, Hit, Query},
    theme::{Rgba, Theme},
    ui::{Canvas, CodeRows, Rect, line_h},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

/// A "Go: References in multibuffer" answer is only taken for this long after the request.
const REFS_WAIT: Duration = Duration::from_secs(30);
/// Files bigger than this are not read for excerpts.
const MAX_READ: usize = 4_000_000;

/// What the app keeps for multibuffers (the tabs hold the rest).
#[derive(Default)]
pub struct MbApp {
    /// The last project search typed, for "Search: Open results in multibuffer".
    pub last_query: Option<Query>,
    /// The next references answer opens a multibuffer instead of the picker.
    pub want_refs: Option<Instant>,
    job: Option<Job>,
}

/// A project search running for a multibuffer.
struct Job {
    rx: Receiver<(usize, Vec<Hit>)>,
    done: Arc<AtomicBool>,
    hits: Vec<Hit>,
    text: String,
}

/// The lines of a file: the unsaved text of an open buffer, else the disk (`None` when unreadable).
fn read_lines(path: &Path, unsaved: &HashMap<PathBuf, String>) -> Option<Vec<String>> {
    let text = match unsaved.get(path) {
        Some(t) => t.clone(),
        None => {
            let bytes = std::fs::read(path).ok()?;
            if bytes.len() > MAX_READ {
                return None;
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
    };
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).lines().map(str::to_string).collect())
}

fn tokenizer(map: &mut HashMap<String, SharedTokenizer>, assets: &Path, scope: String) -> Option<SharedTokenizer> {
    if let Some(t) = map.get(&scope) {
        return Some(t.clone());
    }
    let t = make_tokenizer(assets, &scope)?;
    map.insert(scope, t.clone());
    Some(t)
}

/// Highlight an excerpt (once per theme). Lines that cannot be tokenized stay plain.
fn ensure_runs(
    ex: &mut Excerpt,
    path: &Path,
    theme: &Theme,
    assets: &Path,
    tokenizers: &mut HashMap<String, SharedTokenizer>,
) {
    let key = theme.foreground;
    if ex.runs.as_ref().is_some_and(|(k, _)| *k == key) {
        return;
    }
    let text = ex.lines.join("\n");
    let runs: Runs = language_for(&path.to_string_lossy())
        .and_then(|lang| crate::app_md::scope_of(assets, lang))
        .and_then(|scope| tokenizer(tokenizers, assets, scope))
        .map(|tk| highlight_text(&tk, theme, &text))
        .unwrap_or_default();
    ex.runs = Some((key, runs));
}

/// One row of coloured pieces: `runs` (byte ranges of `text`) coloured, the gaps in `fg`.
fn push_code(rows: &mut CodeRows, text: &str, runs: Option<&Vec<(usize, usize, Rgba, u8)>>, fg: Rgba) {
    let mut at = 0;
    for &(s, e, color, flags) in runs.into_iter().flatten() {
        let (s, e) = (s.min(text.len()), e.min(text.len()));
        if s < at || s >= e {
            continue;
        }
        let (Some(gap), Some(piece)) = (text.get(at..s), text.get(s..e)) else { continue };
        rows.push(gap, fg, 0);
        rows.push(piece, color, flags);
        at = e;
    }
    if let Some(rest) = text.get(at..) {
        rows.push(rest, fg, 0);
    }
    rows.end_row();
}

impl App {
    /// Open (or refresh) the multibuffer tab of `kind` showing `locs`.
    pub fn open_multibuf(&mut self, kind: Kind, title: String, locs: Vec<Loc>) {
        let at = self.docs.iter().position(|d| d.mb.as_ref().is_some_and(|m| m.kind == kind));
        if at.is_some_and(|i| self.docs[i].mb.as_ref().is_some_and(|m| m.is_dirty())) {
            return self.say("The multibuffer has unsaved edits: save them (Ctrl+S) or undo them first");
        }
        let unsaved = self.unsaved_texts();
        let groups = multibuf::build(&locs, multibuf::CONTEXT, |p| self.rel(p), |p| read_lines(p, &unsaved));
        if groups.is_empty() {
            return self.say("nothing to show");
        }
        let mb = Box::new(MultiBuf::new(kind, title, groups));
        let summary = mb.summary();
        let name = mb.title.clone();
        let i = match at {
            Some(i) => {
                self.docs[i].mb = Some(mb);
                self.docs[i].name = name;
                i
            }
            None => {
                let mut d = Doc::new(Buffer::from_rope(ropey::Rope::new(), None), None, None, "//", self.tx.cell_w);
                d.name = name;
                d.buf.read_only = true;
                d.buf.no_write = true;
                d.minimap = false;
                d.mb = Some(mb);
                self.docs.push(d);
                self.docs.len() - 1
            }
        };
        self.activate(i);
        self.focus = Focus::Editor;
        self.say(summary);
        self.request_redraw();
    }

    /// "Search: Open results in multibuffer": run the last project search again, one line per match,
    /// and show every result as excerpts in one tab.
    pub fn multibuffer_search(&mut self) {
        let Some(mut q) = self.mb.last_query.clone() else {
            return self.say("Run a project search first (Ctrl+Shift+F)");
        };
        if crate::remote::session_for(&self.root).is_some() {
            return self.say("Multibuffer search is not available on a remote folder");
        }
        q.replace = None;
        let (overrides, roots, wake) = (self.unsaved_texts(), self.roots(), self.wake());
        let (tx, rx) = channel();
        let done = Arc::new(AtomicBool::new(false));
        let (done2, text) = (done.clone(), q.text.clone());
        std::thread::spawn(move || {
            search::search_in(&roots, &q, &overrides, Arc::new(AtomicUsize::new(0)), 0, tx, || wake());
            done2.store(true, Ordering::SeqCst);
            wake();
        });
        self.mb.job = Some(Job { rx, done, hits: Vec::new(), text });
        self.say("Searching…");
    }

    /// Called every pump: when the multibuffer search finished, open its tab.
    pub fn mb_pump(&mut self) {
        let Some(job) = self.mb.job.as_mut() else { return };
        if !job.done.load(Ordering::SeqCst) {
            return;
        }
        while let Ok((_, hits)) = job.rx.try_recv() {
            job.hits.extend(hits);
        }
        let Some(job) = self.mb.job.take() else { return };
        if job.hits.is_empty() {
            return self.say("no results");
        }
        let locs: Vec<Loc> = job
            .hits
            .iter()
            .map(|h| Loc { path: h.path.clone(), line: h.line, col: h.col, len: h.mark.1.saturating_sub(h.mark.0) })
            .collect();
        let shown: String = job.text.chars().take(30).collect();
        self.open_multibuf(Kind::Search, format!("Search: {shown}"), locs);
    }

    /// "Go: References in multibuffer": ask the language server; the answer opens the tab.
    pub fn multibuffer_references(&mut self) {
        if self.with_server_pub().is_none() {
            return self.say("no language server for this file");
        }
        self.mb.want_refs = Some(Instant::now());
        self.find_references();
    }

    /// A references answer arrived: true when it is for a multibuffer.
    pub fn mb_wants_refs(&mut self) -> bool {
        self.mb.want_refs.take().is_some_and(|t| t.elapsed() < REFS_WAIT)
    }

    /// Show the references `locs` (LSP URI, line, column) as a multibuffer.
    pub fn open_refs_multibuf(&mut self, locs: &[(String, usize, usize)]) {
        let list: Vec<Loc> = locs
            .iter()
            .filter_map(|(uri, line, col)| Some(Loc { path: uri_to_path(uri)?, line: *line, col: *col, len: 0 }))
            .collect();
        self.open_multibuf(Kind::References, "References".to_string(), list);
    }

    // ── drawing ──

    /// Paint the active tab's multibuffer over the editor area `area` (nothing for other tabs).
    pub fn draw_multibuf(&mut self, cv: &mut Canvas, area: Rect) -> bool {
        let di = self.active;
        self.draw_mb_doc(cv, di, area, true)
    }

    /// Paint the multibuffer of document `di` over `area`: the focused pane (`focused`, with the
    /// caret) or another pane of a split. Nothing for a document that is not a multibuffer.
    pub fn draw_mb_doc(&mut self, cv: &mut Canvas, di: usize, area: Rect, focused: bool) -> bool {
        let App { docs, tx, theme, tokenizers, assets, mouse, focus, .. } = self;
        let caret_on = focused && *focus == Focus::Editor;
        let Some(mb) = docs.get_mut(di).and_then(|d| d.mb.as_deref_mut()) else { return false };
        let c = theme.chrome;
        let (fg, dim) = (theme.foreground, c.tab_inactive_fg);
        let lh = line_h() as i32;
        cv.fill(area, theme.background);
        mb.rect = area;
        mb.clamp_scroll(lh);
        if mb.follow {
            mb.ensure_visible(lh);
            mb.follow = false;
        }
        let digits = mb.max_line.max(1).to_string().len();
        let cell = tx.cell_w;
        let gw = (digits as f32 * cell).ceil() as i32 + 28;
        let code_x = area.x + gw;
        let first = (mb.scroll / lh as f64) as usize;
        let off = -((mb.scroll - first as f64 * lh as f64) as i32);
        let end = (first + (area.h / lh) as usize + 2).min(mb.rows.len());
        mb.code_x = code_x;
        mb.cell = cell;
        let code_clip = Rect::new(code_x, area.y, (area.w - gw).max(0), area.h);
        let num_clip = Rect::new(area.x, area.y, gw, area.h);
        let (mut code, mut nums) = (CodeRows::default(), CodeRows::default());
        let mut headers: Vec<(i32, usize)> = Vec::new();
        for r in first..end {
            let y = area.y + off + (r - first) as i32 * lh;
            let row = mb.rows[r];
            let band = Rect::new(area.x, y, area.w, lh);
            if r == mb.sel {
                cv.fill(band, c.list_active_bg);
            } else if matches!(row, Row::File(_)) {
                cv.fill(band, c.sidebar_bg);
            } else if row != Row::Gap && band.contains(mouse.0, mouse.1) {
                cv.fill(band, c.list_hover_bg);
            }
            match row {
                Row::File(g) => {
                    headers.push((y, g));
                    code.end_row();
                    nums.end_row();
                }
                Row::Gap => {
                    code.push("⋯", dim, 0);
                    code.end_row();
                    nums.end_row();
                }
                Row::Line { g, e, i } => {
                    {
                        let grp = &mut mb.groups[g];
                        let (path, excerpts) = (&grp.path, &mut grp.excerpts);
                        ensure_runs(&mut excerpts[e], path, theme, assets.as_path(), tokenizers);
                    }
                    let ex = &mb.groups[g].excerpts[e];
                    let line_no = ex.start + i;
                    let mark = ex.marks.iter().find(|m| m.line == line_no);
                    if let Some(m) = mark {
                        let x = code_x + (m.dcol as f32 * cell).round() as i32;
                        let w = ((m.len as f32 * cell).round() as i32).max(2);
                        cv.fill_clip(Rect::new(x, y + 1, w, lh - 2), c.find_match, code_clip);
                    }
                    if r == mb.sel && caret_on {
                        let col = mb.col.min(ex.raw[i].chars().count());
                        let x = code_x + (multibuf::display_col(&ex.raw[i], col) as f32 * cell).round() as i32;
                        cv.fill_clip(Rect::new(x, y + 1, 2, lh - 2), theme.cursor, code_clip);
                    }
                    push_code(&mut code, &ex.lines[i], ex.runs.as_ref().and_then(|(_, r)| r.get(i)), fg);
                    let num_color = if mark.is_some() { c.tab_active_fg } else { dim };
                    nums.push_fmt(format_args!("{:>w$}", line_no + 1, w = digits), num_color, 0);
                    nums.end_row();
                }
            }
        }
        tx.draw_rows(cv, code_x, area.y + off, &code, code_clip);
        tx.draw_rows(cv, area.x + 12, area.y + off, &nums, num_clip);
        for (y, g) in headers {
            let grp = &mb.groups[g];
            let n = grp.match_count();
            let right = crate::ui_viewer::plural(n, "result", "results");
            // Sticky-looking pill behind the file name, the count as an accent chip on the right.
            let band = Rect::new(area.x, y, area.w, lh);
            let cw = crate::ui_kit::chip::chip_w(tx, &right);
            let chip_y = y + (lh - crate::ui_kit::chip::CHIP_H) / 2;
            crate::ui_kit::chip::chip_on_right(
                cv,
                tx,
                (area.right() - 12, chip_y),
                &right,
                c.accent,
                c.sidebar_bg,
                band,
            );
            let label = fit(tx, &grp.label, (area.w - 48 - cw).max(40), true);
            let ty = y + (lh - 18) / 2;
            let lw = tx.ui_width(&label, true);
            let pill = Rect::new(area.x + 6, y + 1, lw + 2 * crate::ds::S3, (lh - 2).max(0));
            cv.fill_round_clip(pill, crate::ds::R_CTRL, c.widget_bg, band);
            tx.draw_ui(cv, pill.x + crate::ds::S3, ty, &label, c.tab_active_fg, pill, true);
        }
        false
    }

    // ── input ──

    fn mb_open(&mut self, row: usize) {
        let Some(loc) = self.docs.get(self.active).and_then(|d| d.mb.as_deref()).and_then(|m| m.loc_at(row)) else {
            return;
        };
        self.push_nav();
        self.open_path(&loc.0, Some((loc.1, loc.2)));
    }

    /// Press on the multibuffer: put the caret there; a header, or Ctrl+Click on a line, also jumps
    /// to the file. True when it was over the tab.
    pub fn mb_mouse_down(&mut self, x: f64, y: f64) -> bool {
        let lh = line_h() as i32;
        let ctrl = self.mods.control_key();
        let Some(mb) = self.docs.get_mut(self.active).and_then(|d| d.mb.as_deref_mut()) else { return false };
        if !mb.rect.contains(x, y) {
            return false;
        }
        let row = mb.row_at(y - mb.rect.y as f64, lh);
        if let Some(r) = row {
            let dcol = if mb.cell > 0.0 { ((x - mb.code_x as f64) / mb.cell as f64).max(0.0) as usize } else { 0 };
            mb.click_at(r, dcol);
        }
        let jump = row.filter(|r| match mb.rows.get(*r) {
            Some(Row::File(_)) => true,
            Some(Row::Line { .. }) => ctrl,
            _ => false,
        });
        self.focus = Focus::Editor;
        if let Some(r) = jump {
            self.mb_open(r);
        }
        true
    }

    /// Documents whose multibuffer is on screen: the active one and the other panes of a split.
    fn mb_shown(&self) -> Vec<usize> {
        let mut v = vec![self.active];
        if let Some(sp) = &self.split {
            v.extend(sp.panes.iter().enumerate().filter(|(i, _)| *i != sp.focus).map(|(_, p)| p.doc));
        }
        v
    }

    /// Wheel over a multibuffer (in any pane) scrolls it.
    pub fn mb_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let lh = line_h() as i32;
        let shown = self.mb_shown();
        let Some(mb) = self
            .docs
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| shown.contains(i))
            .filter_map(|(_, d)| d.mb.as_deref_mut())
            .find(|m| m.rect.contains(x, y))
        else {
            return false;
        };
        mb.scroll_by(crate::scroller::wheel_px(delta, lh as f64), lh);
        true
    }

    /// A hand over the headers (they jump), the text cursor over the code.
    pub fn mb_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let mb = self.docs.get(self.active)?.mb.as_deref()?;
        if !mb.rect.contains(x, y) {
            return None;
        }
        let r = mb.row_at(y - mb.rect.y as f64, line_h() as i32)?;
        Some(match mb.rows.get(r) {
            Some(Row::File(_)) => CursorIcon::Pointer,
            Some(Row::Line { .. }) => CursorIcon::Text,
            _ => CursorIcon::Default,
        })
    }

    /// Undo / redo in the active multibuffer. False when the active tab is not one (the text
    /// editor handles it); true otherwise, even with nothing to undo.
    pub fn mb_undo(&mut self, undo: bool) -> bool {
        let Some(mb) = self.docs.get_mut(self.active).and_then(|d| d.mb.as_deref_mut()) else { return false };
        let done = if undo { mb.undo() } else { mb.redo() };
        if !done {
            self.say(if undo { "Nothing to undo" } else { "Nothing to redo" });
        }
        true
    }

    /// Keys of the multibuffer tab: typing, Enter, Backspace, Delete and Tab edit the line under
    /// the caret; the arrows, Home and End move it (`Ctrl` + Up / Down between excerpts, `Ctrl` +
    /// Home / End to the first / last match, F8 / Shift+F8 to the next / previous match); Page Up /
    /// Page Down scroll; Ctrl+Enter, or Enter on a header, jumps to the file. False for any other
    /// key (shortcuts).
    pub fn mb_key(&mut self, ev: &KeyIn) -> bool {
        let (ctrl, alt, shift) = (self.mods.control_key(), self.mods.alt_key(), self.mods.shift_key());
        let lh = line_h() as i32;
        let Some(mb) = self.docs.get_mut(self.active).and_then(|d| d.mb.as_deref_mut()) else { return false };
        if alt {
            return false;
        }
        let page = mb.rect.h as f64 * 0.9;
        let mut open = false;
        match &ev.key {
            Key::Named(NamedKey::ArrowDown) if ctrl => mb.step_excerpt(1),
            Key::Named(NamedKey::ArrowUp) if ctrl => mb.step_excerpt(-1),
            Key::Named(NamedKey::ArrowDown) => mb.step_row(1),
            Key::Named(NamedKey::ArrowUp) => mb.step_row(-1),
            Key::Named(NamedKey::ArrowLeft) => mb.step_col(-1),
            Key::Named(NamedKey::ArrowRight) => mb.step_col(1),
            Key::Named(NamedKey::Home) if ctrl => mb.select_first(),
            Key::Named(NamedKey::End) if ctrl => mb.select_last(),
            Key::Named(NamedKey::Home) => mb.line_home(),
            Key::Named(NamedKey::End) => mb.line_end(),
            Key::Named(NamedKey::F8) => mb.step_hit(if shift { -1 } else { 1 }),
            Key::Named(NamedKey::PageDown) => mb.scroll_by(page, lh),
            Key::Named(NamedKey::PageUp) => mb.scroll_by(-page, lh),
            Key::Named(NamedKey::Enter) if ctrl || matches!(mb.rows.get(mb.sel), Some(Row::File(_))) => open = true,
            Key::Named(NamedKey::Enter) => {
                mb.newline();
            }
            Key::Named(NamedKey::Backspace) => {
                mb.backspace();
            }
            Key::Named(NamedKey::Delete) => {
                mb.delete();
            }
            Key::Named(NamedKey::Tab) if !ctrl => {
                mb.type_str("    ");
            }
            _ => {
                let typed: String = match (&ev.text, ctrl) {
                    (Some(t), false) => t.chars().filter(|c| !c.is_control()).collect(),
                    _ => String::new(),
                };
                if typed.is_empty() {
                    return false;
                }
                mb.type_str(&typed);
            }
        }
        let row = mb.sel;
        if open {
            self.mb_open(row);
        }
        true
    }

    // ── saving ──

    /// The open document of `path`, if any.
    fn mb_doc_for(&self, path: &Path) -> Option<usize> {
        self.docs.iter().position(|d| d.path() == Some(path)).or_else(|| {
            let c = crate::paths::canonical(path).ok()?;
            self.docs.iter().position(|d| d.path().and_then(|p| crate::paths::canonical(p).ok()).as_ref() == Some(&c))
        })
    }

    /// Write `edits` into the file `path`: into its open buffer (saved unless it had unsaved edits
    /// of its own: then `Ok(true)`), or on disk. `Err` says why the file was left alone.
    fn mb_write_file(&mut self, path: &Path, edits: &[Edit]) -> Result<bool, String> {
        const STALE: &str = "changed since the results were taken";
        if let Some(i) = self.mb_doc_for(path) {
            let d = &mut self.docs[i];
            if d.buf.read_only {
                return Err("read-only".to_string());
            }
            let mut lines: Vec<String> = (0..d.buf.line_count()).map(|l| d.buf.line(l)).collect();
            let bom = lines.first().is_some_and(|l| l.starts_with('\u{feff}'));
            if let Some(first) = lines.first_mut()
                && bom
            {
                first.remove(0);
            }
            multibuf::check_edits(&lines, edits).map_err(|_| STALE.to_string())?;
            let eol = d.buf.eol();
            let ranges: Vec<(usize, usize, String)> = edits
                .iter()
                .map(|e| {
                    let last = e.start + e.old.len().max(1) - 1;
                    let a = d.buf.line_start(e.start) + usize::from(bom && e.start == 0);
                    let b = d.buf.line_start(last) + d.buf.line(last).chars().count();
                    (a, b.max(a), e.new.join(eol))
                })
                .collect();
            let was_dirty = d.buf.is_dirty();
            crate::app_overlay::preplace::edit_doc(d, &ranges);
            d.reveal = true;
            if !was_dirty {
                self.write_saved(i, None);
            }
            return Ok(was_dirty);
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let text = String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
        let out = multibuf::apply_text(&text, edits).map_err(|_| STALE.to_string())?;
        crate::replace::write_atomic(path, out.as_bytes()).map_err(|e| e.to_string())?;
        Ok(false)
    }

    /// Ctrl+S in a multibuffer: write every edited excerpt back to its file.
    pub fn mb_save(&mut self) {
        let di = self.active;
        let Some(mb) = self.docs.get(di).and_then(|d| d.mb.as_deref()) else { return };
        let jobs: Vec<(usize, PathBuf, Vec<Edit>)> = mb
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.is_dirty())
            .map(|(i, g)| (i, g.path.clone(), g.edits()))
            .collect();
        if jobs.is_empty() {
            return self.say("No edits to save in the multibuffer");
        }
        let (mut done, mut open_dirty) = (Vec::new(), 0);
        let mut failed: Vec<String> = Vec::new();
        for (g, path, edits) in jobs {
            match self.mb_write_file(&path, &edits) {
                Ok(left) => {
                    done.push(g);
                    open_dirty += usize::from(left);
                }
                Err(why) => failed.push(format!("{} ({why})", self.rel(&path))),
            }
        }
        if let Some(mb) = self.docs.get_mut(di).and_then(|d| d.mb.as_deref_mut()) {
            for g in &done {
                mb.groups[*g].mark_saved();
            }
        }
        let n = done.len();
        let mut msg = format!("Saved {n} file{}", if n == 1 { "" } else { "s" });
        if open_dirty > 0 {
            msg.push_str(&format!(" ({open_dirty} open with other unsaved edits: changes are in its tab, unsaved)"));
        }
        if !failed.is_empty() {
            msg.push_str(&format!(" — not saved: {}", failed.join(", ")));
        }
        self.say(msg);
        self.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use winit::keyboard::ModifiersState;

    fn loc(path: &Path, line: usize, col: usize, len: usize) -> Loc {
        Loc { path: path.to_path_buf(), line, col, len }
    }

    #[test]
    fn results_open_as_one_tab_and_enter_jumps_to_the_file() {
        let _g = lock();
        let dir = tmp("mb-tab");
        let mut app = test_app(&dir);
        let a = dir.join("a.txt");
        std::fs::write(&a, "one\ntwo\nthree\nfour\nfive\nsix\nseven\n").unwrap();
        let b = dir.join("b.txt");
        std::fs::write(&b, "x\ny\n").unwrap();
        app.open_multibuf(Kind::Search, "Search: t".into(), vec![loc(&b, 1, 0, 1), loc(&a, 2, 1, 2)]);
        assert!(app.docs[app.active].mb.is_some());
        assert_eq!(app.docs[app.active].name, "Search: t");

        let (w, h) = (900, 500);
        app.size = (w as u32, h as u32);
        let mut px = vec![0u32; (w * h) as usize];
        app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
        let mb = app.docs[app.active].mb.as_ref().unwrap();
        assert!(mb.rect.w > 0, "drawn");
        assert_eq!(mb.line_no(mb.sel), Some(2), "starts on the first match, in path order");

        // Showing the results again reuses the tab.
        app.open_multibuf(Kind::Search, "Search: u".into(), vec![loc(&a, 2, 1, 2)]);
        assert_eq!(app.docs.iter().filter(|d| d.mb.is_some()).count(), 1);

        // Plain Enter breaks the line under the caret; Ctrl+Enter jumps to the file.
        let enter = KeyIn { key: Key::Named(NamedKey::Enter), text: None };
        app.mods = ModifiersState::CONTROL;
        assert!(app.mb_key(&enter));
        app.mods = ModifiersState::empty();
        let d = &app.docs[app.active];
        assert!(d.mb.is_none());
        assert_eq!(
            d.path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).as_deref(),
            Some("a.txt")
        );
        assert_eq!(d.buf.line_col(d.ed.head()), (2, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn typed(app: &mut App, text: &str) {
        assert!(app.mb_key(&KeyIn { key: Key::Character(text.into()), text: Some(text.to_string()) }));
    }

    fn files(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = tmp(name);
        let a = dir.join("a.txt");
        std::fs::write(&a, "one\ntwo\nthree\nfour\nfive\nsix\nseven\n").unwrap();
        let b = dir.join("b.txt");
        std::fs::write(&b, "x\ny\n").unwrap();
        (dir, a, b)
    }

    #[test]
    fn edits_are_saved_to_files_that_are_not_open_and_other_files_are_untouched() {
        let _g = lock();
        let (dir, a, b) = files("mb-save");
        let long: String = (0..20).map(|i| format!("line{i}\n")).collect();
        std::fs::write(&a, &long).unwrap();
        let mut app = test_app(&dir);
        app.open_multibuf(Kind::Search, "S".into(), vec![loc(&a, 2, 1, 2), loc(&a, 15, 0, 4), loc(&b, 1, 0, 1)]);
        typed(&mut app, "X");
        assert!(app.docs[app.active].mb.as_ref().unwrap().is_dirty());
        // A line break above the second excerpt must not move its place in the file.
        assert!(app.mb_key(&KeyIn { key: Key::Named(NamedKey::Enter), text: None }));
        app.mb_key(&KeyIn { key: Key::Named(NamedKey::Home), text: None });
        app.mb_key(&KeyIn { key: Key::Named(NamedKey::ArrowUp), text: None });
        typed(&mut app, ">");
        app.mods = ModifiersState::CONTROL;
        app.mb_key(&KeyIn { key: Key::Named(NamedKey::ArrowDown), text: None });
        app.mods = ModifiersState::empty();
        app.mb_key(&KeyIn { key: Key::Named(NamedKey::Home), text: None });
        typed(&mut app, "!");
        app.save_active();
        assert!(!app.docs[app.active].mb.as_ref().unwrap().is_dirty());
        let text = std::fs::read_to_string(&a).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 21, "one line more");
        assert_eq!((lines[2], lines[3]), (">lX", "ine2"));
        assert_eq!(lines[16], "!line15");
        assert_eq!(lines[1], "line1");
        assert_eq!(lines[17], "line16");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "x\ny\n");
        // Saved: a second save has nothing to write, and undo makes it dirty again.
        app.save_active();
        assert!(app.mb_undo(true));
        assert!(app.docs[app.active].mb.as_ref().unwrap().is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_changed_since_the_results_were_taken_is_left_alone() {
        let _g = lock();
        let (dir, a, _b) = files("mb-stale");
        let mut app = test_app(&dir);
        app.open_multibuf(Kind::Search, "S".into(), vec![loc(&a, 2, 1, 2)]);
        typed(&mut app, "X");
        std::fs::write(&a, "one\ntwo\nCHANGED\nfour\n").unwrap();
        app.save_active();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one\ntwo\nCHANGED\nfour\n");
        assert!(app.docs[app.active].mb.as_ref().unwrap().is_dirty(), "the edit is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_open_file_gets_the_edit_in_its_buffer_and_is_saved() {
        let _g = lock();
        let (dir, a, _b) = files("mb-open");
        let mut app = test_app(&dir);
        app.open_path(&a, None);
        app.open_multibuf(Kind::Search, "S".into(), vec![loc(&a, 2, 1, 2)]);
        typed(&mut app, "X");
        app.save_active();
        let d = app.docs.iter().find(|d| d.path() == Some(a.as_path())).unwrap();
        assert!(d.buf.to_string().contains("tXhree"));
        assert!(!d.buf.is_dirty(), "it had no edits of its own: saved");
        assert!(std::fs::read_to_string(&a).unwrap().contains("tXhree"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_open_file_with_its_own_unsaved_edits_keeps_them_unsaved() {
        let _g = lock();
        let (dir, a, _b) = files("mb-open-dirty");
        let mut app = test_app(&dir);
        app.open_path(&a, None);
        let i = app.active;
        let before = app.docs[i].ed.sel;
        app.docs[i].buf.replace(0, 0, "zero\n", crate::buffer::EditKind::Other, before);
        app.open_multibuf(Kind::Search, "S".into(), vec![loc(&a, 3, 0, 4)]);
        // The buffer now has "zero" on top: the excerpt (read from it) matches.
        typed(&mut app, "X");
        app.save_active();
        let d = &app.docs[i];
        assert!(d.buf.to_string().contains("Xthree"));
        assert!(d.buf.is_dirty(), "left for the user to save");
        assert!(!std::fs::read_to_string(&a).unwrap().contains("Xthree"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_multibuffer_in_the_other_pane_of_a_split_is_drawn() {
        let _g = lock();
        let (dir, a, _b) = files("mb-split");
        let mut app = test_app(&dir);
        app.open_path(&a, None);
        let ai = app.active;
        let (w, h) = (1000, 500);
        app.size = (w as u32, h as u32);
        let mut px = vec![0u32; (w * h) as usize];
        app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
        app.split_right();
        app.open_multibuf(Kind::Search, "S".into(), vec![loc(&a, 2, 1, 2)]);
        let mi = app.active;
        app.split_set_other_doc(mi);
        app.activate(ai);
        app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
        let sp = app.split.as_ref().unwrap();
        let other = sp.rects[1 - sp.focus];
        let mb = app.docs[mi].mb.as_ref().unwrap();
        assert!(other.w > 0);
        assert_eq!(mb.rect, other, "drawn in the pane that is not focused");
        assert!(mb.cell > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
