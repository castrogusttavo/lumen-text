//! Multibuffer tabs in the app: opening one (project search results, LSP references), drawing it
//! with the editor's code rows and syntax colours, and its input (arrows between matches, Enter or a
//! click to jump to the file, wheel and Page keys to scroll). The model is `multibuf`.
//!
//! The tab is a document without a path and with a read-only, never-written buffer (like the image
//! tabs), carrying a `MultiBuf`; `Doc::draw` leaves it blank and `App::draw_multibuf` paints it.
//! It is read-only: there is no in-place editing, a jump opens the real file.

use crate::{
    app::{App, Focus, KeyIn},
    app_gitpanel::fit,
    buffer::Buffer,
    doc::{Doc, uri_to_path},
    highlight::{SharedTokenizer, highlight_text, language_for, make_tokenizer},
    multibuf::{self, Excerpt, Kind, Loc, MultiBuf, Row, Runs},
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
        let unsaved = self.unsaved_texts();
        let groups = multibuf::build(&locs, multibuf::CONTEXT, |p| self.rel(p), |p| read_lines(p, &unsaved));
        if groups.is_empty() {
            return self.say("nothing to show");
        }
        let mb = Box::new(MultiBuf::new(kind, title, groups));
        let summary = mb.summary();
        let name = mb.title.clone();
        let at = self.docs.iter().position(|d| d.mb.as_ref().is_some_and(|m| m.kind == kind));
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
        let App { docs, active, tx, theme, tokenizers, assets, mouse, .. } = self;
        let Some(mb) = docs.get_mut(*active).and_then(|d| d.mb.as_deref_mut()) else { return false };
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
            let right = format!("{n} result{}", if n == 1 { "" } else { "s" });
            let rw = tx.ui_width(&right, false);
            let label = fit(tx, &grp.label, (area.w - 36 - rw).max(40), true);
            let ty = y + (lh - 18) / 2;
            tx.draw_ui(cv, area.x + 12, ty, &label, c.tab_active_fg, area, true);
            tx.draw_ui(cv, area.right() - 12 - rw, ty, &right, dim, area, false);
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

    /// Press on the multibuffer: select the row and jump to it. True when it was over the tab.
    pub fn mb_mouse_down(&mut self, x: f64, y: f64) -> bool {
        let lh = line_h() as i32;
        let Some(mb) = self.docs.get_mut(self.active).and_then(|d| d.mb.as_deref_mut()) else { return false };
        if !mb.rect.contains(x, y) {
            return false;
        }
        let row = mb.row_at(y - mb.rect.y as f64, lh);
        if let Some(r) = row {
            mb.click_row(r);
        }
        let open = row.filter(|r| mb.loc_at(*r).is_some());
        self.focus = Focus::Editor;
        if let Some(r) = open {
            self.mb_open(r);
        }
        true
    }

    /// Wheel over the multibuffer scrolls it.
    pub fn mb_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let lh = line_h() as i32;
        let Some(mb) =
            self.docs.get_mut(self.active).and_then(|d| d.mb.as_deref_mut()).filter(|m| m.rect.contains(x, y))
        else {
            return false;
        };
        mb.scroll_by(crate::scroller::wheel_px(delta, lh as f64), lh);
        true
    }

    /// A hand over rows that jump somewhere.
    pub fn mb_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let mb = self.docs.get(self.active)?.mb.as_deref()?;
        if !mb.rect.contains(x, y) {
            return None;
        }
        let r = mb.row_at(y - mb.rect.y as f64, line_h() as i32)?;
        Some(if mb.loc_at(r).is_some() { CursorIcon::Pointer } else { CursorIcon::Default })
    }

    /// Keys of the multibuffer tab: Up / Down between matches, `Ctrl` + Up / Down between excerpts,
    /// Home / End, Page Up / Page Down, Enter to jump. False for any other key (shortcuts, typing).
    pub fn mb_key(&mut self, ev: &KeyIn) -> bool {
        let (ctrl, alt) = (self.mods.control_key(), self.mods.alt_key());
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
            Key::Named(NamedKey::ArrowDown) => mb.step_hit(1),
            Key::Named(NamedKey::ArrowUp) => mb.step_hit(-1),
            Key::Named(NamedKey::Home) => mb.select_first(),
            Key::Named(NamedKey::End) => mb.select_last(),
            Key::Named(NamedKey::PageDown) => mb.scroll_by(page, lh),
            Key::Named(NamedKey::PageUp) => mb.scroll_by(-page, lh),
            Key::Named(NamedKey::Enter) => open = true,
            _ => return false,
        }
        let row = mb.sel;
        if open {
            self.mb_open(row);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

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

        let enter = KeyIn { key: Key::Named(NamedKey::Enter), text: None };
        assert!(app.mb_key(&enter));
        let d = &app.docs[app.active];
        assert!(d.mb.is_none());
        assert_eq!(
            d.path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).as_deref(),
            Some("a.txt")
        );
        assert_eq!(d.buf.line_col(d.ed.head()), (2, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
