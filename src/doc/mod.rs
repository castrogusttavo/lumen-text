//! One open document: text, cursor, highlighting, scroll/caret animation and the
//! per-file state that LSP and git attach to it.

use crate::{
    buffer::Buffer,
    docview::{FOLD_W, row_range as row_range_of},
    editor::{Editor, col_from_visual},
    highlight::{Highlighter, Spans},
    rowmap::wrap_breaks,
    theme::{Rgba, Theme},
    ui::{Canvas, Rect, TextCtx, line_h},
};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

mod draw;
mod paths;
mod rows;
mod symbols;
#[cfg(test)]
mod tests;

pub use paths::*;
pub use rows::*;
pub use symbols::*;

const CURSOR_TAU_Y: f64 = 0.055;
const CURSOR_TAU_X: f64 = 0.040;
const CURSOR_W: i32 = 2;
const MIN_GUTTER_DIGITS: usize = 3;

#[derive(Clone, Debug)]
pub struct Diag {
    pub sl: usize,
    pub sc: usize,
    pub el: usize,
    pub ec: usize,
    pub severity: u8, // 1 error, 2 warning, 3 info, 4 hint
    pub message: String,
    pub code: String,
    /// Which server produced it (registry servers only; empty for tsgo, Biome and ESLint).
    pub source: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Copy, Debug)]
pub struct GitMark {
    pub line: usize,
    pub len: usize,
    pub kind: GitKind,
}

/// One entry of the document outline (flattened `documentSymbol` tree).
#[derive(Clone, Debug, PartialEq)]
pub struct Sym {
    pub name: String,
    pub kind: u32,
    pub depth: usize,
    pub sl: usize,
    pub el: usize,
    pub sel_l: usize,
    pub sel_c: usize, // UTF-16 column
}

#[derive(Clone, Debug)]
pub struct Lens {
    pub line: usize,
    pub col: usize, // UTF-16
    pub title: Option<String>,
    pub raw: serde_json::Value,
}

pub struct Doc {
    pub buf: Buffer,
    pub ed: Editor,
    pub hl: Option<Highlighter>,
    pub name: String,
    pub lang: Option<&'static str>,

    pub top: f64,    // current (animated) first visible line, fractional
    pub target: f64, // where the animation is heading
    pub animating: bool,
    pub left: f64, // horizontal scroll in pixels
    pub reveal: bool,
    caret: (f64, f64),
    caret_init: bool,
    caret_animating: bool,
    caret_snap: bool,

    pub rect: Rect,
    pub cell_w: f32,

    // LSP / git attachments
    /// Merged view of both sources, sorted by position; what drawing and navigation use.
    pub diags: Vec<Diag>,
    diags_ts: Vec<Diag>,
    diags_lint: Vec<Diag>,
    diags_eslint: Vec<Diag>,
    /// Diagnostics of the registry servers, by server id (merged and deduplicated into `diags`).
    diags_ext: Vec<(String, Vec<Diag>)>,
    pub eslint_due: Option<Instant>,
    /// Test outcomes from the last run: (0-based line, passed). Drawn in the gutter.
    pub test_marks: Vec<(usize, Option<bool>)>,
    /// Selections to return to when shrinking after "expand selection".
    pub sel_stack: Vec<crate::buffer::Sel>,
    pub symbols: Vec<Sym>,
    pub bps: Vec<usize>,
    /// Bookmarked lines (0-based, sorted); they follow edits (see `bookmarks`).
    pub marks: Vec<usize>,
    pub minimap: bool,
    /// Scrollbar of the last drawn frame, and whether the pointer is near it.
    pub sb: Option<crate::ui::VBar>,
    pub sb_hot: bool,
    pub sb_drag: bool,
    /// Horizontal scrollbar of the last drawn frame (only with wide lines and no word wrap).
    pub hb: Option<crate::ui::HBar>,
    pub hb_hot: bool,
    pub hb_drag: bool,
    /// Where the horizontal scroll is heading (equals `left` when still).
    pub left_target: f64,
    /// Wheel dynamics (see doc_scroll.rs).
    pub scr: crate::doc_scroll::DocScroll,
    pub indent_guides: bool,
    pub sticky: bool,
    /// Word wrap + folding state (see docview.rs).
    pub view: crate::docview::ViewMap,
    /// Folded-region "..." markers drawn last frame: (rect, header line).
    pub fold_pills: Vec<(Rect, usize)>,
    pub minimap_layout: Option<crate::minimap::Layout>,
    /// Minimap row infos of rows without token colors (those only depend on the text): (layout key, buffer
    /// version described, row -> info).
    mm_cache: (u64, u64, std::collections::HashMap<usize, crate::minimap::LineInfo>),
    /// Buffer version at the end of the last `sync_edits` that consumed edits.
    synced_ver: u64,
    pub conflicts: Vec<crate::gitx::Conflict>,
    pub conflicts_due: Option<Instant>,
    pub conflict_hits: Vec<(Rect, usize, crate::gitx::Pick)>,
    pub debug_line: Option<usize>,
    pub sem: std::collections::HashMap<usize, Vec<(usize, usize, u8, bool)>>,
    pub sem_valid_until: usize,
    pub sem_due: Option<Instant>,
    /// Windowed semantic tokens and the cheap outline of big files (see `bigfile`).
    pub big: crate::bigfile::Big,
    pub lenses: Vec<Lens>,
    pub lens_due: Option<Instant>,
    pub lens_hits: Vec<(Rect, usize)>,
    pub lens_refs: bool,
    pub lens_impls: bool,
    pub symbols_due: Option<Instant>,
    pub biome_open: bool,
    pub biome_root: Option<Option<PathBuf>>,
    pub lint_changed_at: Option<Instant>,
    pub git: Vec<GitMark>,
    pub git_base: Option<String>,
    pub version: i32,
    pub lsp_open: bool,
    pub lsp_root: Option<PathBuf>,
    pub changed_at: Option<Instant>, // last edit not yet sent to the language server
    pub git_stale: bool,
    pub disk_mtime: Option<std::time::SystemTime>,
    pub conflict: bool, // changed on disk while we hold unsaved edits
    /// Active snippet: remaining/all stops (absolute char ranges), current index, buffer length when the
    /// current stop was selected (to shift later stops by what the user typed).
    pub snippet: Option<(Vec<(usize, usize)>, usize, usize)>,
    pub blame: Option<(usize, String)>,
    /// Side-by-side Markdown reader (None = closed, costs nothing).
    pub md: Option<Box<crate::app_md::MdState>>,
    /// LSP inlay hints for the lines around the viewport (empty = no cost anywhere).
    pub inlay: crate::inlay::InlayState,
    /// Pinned tab (drawn first, kept by "Close all").
    pub pinned: bool,
}

#[derive(Clone, Debug)]
pub struct LspEdit {
    pub sl: usize,
    pub sc: usize,
    pub el: usize,
    pub ec: usize,
    pub text: String,
}

/// An inlay hint under the pointer: the line, its index in `inlay.hints.hints_on(line)` and the label
/// char under the pointer (`None` over the paddings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HintHit {
    pub line: usize,
    pub idx: usize,
    pub off: Option<usize>,
}

pub struct DrawOpts<'a> {
    pub dt: f64,
    pub find: &'a [(usize, usize)],
    pub find_cur: Option<usize>,
    /// False for the unfocused pane of a split editor: no current-line band, dim caret.
    pub focused: bool,
}

impl Doc {
    pub fn new(buf: Buffer, hl: Option<Highlighter>, lang: Option<&'static str>, comment: &str, cell_w: f32) -> Self {
        let name = buf
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "untitled".to_string(), |n| n.to_string_lossy().into_owned());
        let mut ed = Editor::new(&buf, comment);
        ed.jsx = matches!(lang, Some("tsx" | "jsx" | "html" | "vue" | "svelte"));
        let disk_mtime = buf.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok());
        Self {
            buf,
            ed,
            hl,
            name,
            lang,
            top: 0.0,
            target: 0.0,
            animating: false,
            left: 0.0,
            reveal: false,
            caret: (0.0, 0.0),
            caret_init: false,
            caret_animating: false,
            caret_snap: false,
            rect: Rect::new(0, 0, 800, 600),
            cell_w,
            diags: Vec::new(),
            diags_ts: Vec::new(),
            diags_lint: Vec::new(),
            diags_eslint: Vec::new(),
            diags_ext: Vec::new(),
            eslint_due: None,
            test_marks: Vec::new(),
            sel_stack: Vec::new(),
            symbols: Vec::new(),
            bps: Vec::new(),
            marks: Vec::new(),
            minimap: true,
            sb: None,
            sb_hot: false,
            sb_drag: false,
            hb: None,
            hb_hot: false,
            hb_drag: false,
            left_target: 0.0,
            scr: Default::default(),
            indent_guides: true,
            sticky: true,
            view: Default::default(),
            fold_pills: Vec::new(),
            minimap_layout: None,
            mm_cache: (0, u64::MAX, Default::default()),
            synced_ver: 0,
            conflicts: Vec::new(),
            conflicts_due: Some(Instant::now()),
            conflict_hits: Vec::new(),
            debug_line: None,
            sem: Default::default(),
            sem_valid_until: usize::MAX,
            sem_due: Some(Instant::now()),
            big: Default::default(),
            lenses: Vec::new(),
            lens_due: Some(Instant::now()),
            lens_hits: Vec::new(),
            lens_refs: true,
            lens_impls: true,
            symbols_due: Some(Instant::now()),
            biome_open: false,
            biome_root: None,
            lint_changed_at: None,
            git: Vec::new(),
            git_base: None,
            version: 1,
            lsp_open: false,
            lsp_root: None,
            changed_at: None,
            git_stale: true,
            disk_mtime,
            conflict: false,
            snippet: None,
            blame: None,
            md: None,
            inlay: Default::default(),
            pinned: false,
        }
    }

    /// Insert snippet `body` replacing chars [from, to) and start its tab-stop session.
    pub fn insert_snippet(&mut self, from: usize, to: usize, body: &str) {
        use crate::buffer::{EditKind, Sel};
        let (l, _) = self.buf.line_col(from);
        let indent: String = self.buf.line(l).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let ex = crate::snippets::expand(body, &indent);
        let before = self.ed.sel;
        self.buf.close_group();
        self.buf.replace(from, to, &ex.text, EditKind::Other, before);
        self.buf.close_group();
        let stops: Vec<(usize, usize)> = ex.stops.iter().map(|(a, b)| (from + a, from + b)).collect();
        let first = stops[0];
        self.ed.sel = Sel { anchor: first.0, head: first.1 };
        self.buf.set_after(self.ed.sel);
        self.snippet = if stops.len() > 1 { Some((stops, 0, self.buf.len_chars())) } else { None };
        self.reveal = true;
        self.sync_edits();
    }

    /// Tab inside a snippet: jump to the next stop. Returns false when no snippet is active.
    pub fn snippet_next(&mut self) -> bool {
        use crate::buffer::Sel;
        let Some((stops, idx, last_len)) = self.snippet.take() else { return false };
        let delta = self.buf.len_chars() as isize - last_len as isize;
        let cur_start = stops[idx].0;
        let mut stops = stops;
        // What was typed inside the current stop shifts every later stop.
        for s in stops.iter_mut().skip(idx + 1) {
            if s.0 >= cur_start {
                s.0 = (s.0 as isize + delta).max(0) as usize;
                s.1 = (s.1 as isize + delta).max(0) as usize;
            }
        }
        let next = idx + 1;
        if next >= stops.len() {
            return true;
        }
        let len = self.buf.len_chars();
        let (a, b) = (stops[next].0.min(len), stops[next].1.min(len));
        self.ed.sel = Sel { anchor: a, head: b };
        self.reveal = true;
        self.snippet = if next + 1 < stops.len() { Some((stops, next, self.buf.len_chars())) } else { None };
        true
    }

    /// Symbols that contain `line`, outermost first (the breadcrumb chain / sticky headers).
    pub fn symbol_chain(&self, line: usize) -> Vec<&Sym> {
        let mut chain: Vec<&Sym> = Vec::new();
        for s in &self.symbols {
            if s.sl <= line && line <= s.el && s.depth == chain.len() {
                chain.push(s);
            } else if s.depth < chain.len() && s.sl <= line && line <= s.el {
                chain.truncate(s.depth);
                chain.push(s);
            }
        }
        chain
    }

    /// Click/drag on the minimap: scroll so the pointer's line is centered.
    /// Drag the scrollbar thumb: pointer at `y`, grabbed `grab` px below the thumb's top.
    pub fn scrollbar_drag(&mut self, y: f64, grab: f64) {
        if let Some(b) = self.sb {
            let total = self.total_rows() as f64;
            let pos = b.pos_for(y, grab, total, self.visible_lines()).min(self.max_top());
            self.scroll_direct(pos - self.target);
        }
    }

    pub fn minimap_scroll_to(&mut self, y: f64) {
        if let Some(l) = &self.minimap_layout {
            // The minimap is drawn by visual rows (wrap and folds included), so its unit is a row.
            let top = crate::minimap::drag_to_top_line(l, y, self.total_rows(), self.visible_lines() as usize);
            self.scroll_direct(top as f64 - self.target);
        }
    }

    pub fn stat_mtime(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.path()?).ok()?.modified().ok()
    }

    /// Replace the text with what is on disk, keeping cursor (clamped) and scroll. Clears undo.
    pub fn reload_from_disk(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path().map(Path::to_path_buf) else { return Ok(()) };
        let fresh = Buffer::open(&path)?;
        let sel = self.ed.sel;
        self.buf = fresh;
        let len = self.buf.len_chars();
        self.ed.sel = crate::buffer::Sel { anchor: sel.anchor.min(len), head: sel.head.min(len) };
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(0);
        }
        self.buf.take_dirty_line();
        self.view.folds.unfold_all();
        self.view.avail_ver = -1;
        self.view.reset();
        self.version += 1;
        self.inlay.reset();
        self.changed_at = Some(Instant::now());
        self.git_stale = true;
        self.disk_mtime = self.stat_mtime();
        self.conflict = false;
        self.caret_snap = true;
        self.target = self.target.clamp(0.0, self.max_top());
        self.top = self.top.clamp(0.0, self.max_top());
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.buf.path.as_deref()
    }

    /// Diagnostics from one source replace that source's previous ones; the merged list is rebuilt.
    pub fn set_source_diags(&mut self, lint: bool, v: Vec<Diag>) {
        if lint {
            self.diags_lint = v;
        } else {
            self.diags_ts = v;
        }
        self.rebuild_diags();
    }

    pub fn set_eslint_diags(&mut self, v: Vec<Diag>) {
        self.diags_eslint = v;
        self.rebuild_diags();
    }

    /// Diagnostics of one registry server (empty list: it has none / the document closed there).
    pub fn set_ext_diags(&mut self, server: &str, v: Vec<Diag>) {
        match self.diags_ext.iter().position(|(n, _)| n == server) {
            Some(i) if v.is_empty() => {
                self.diags_ext.remove(i);
            }
            Some(i) => self.diags_ext[i].1 = v,
            None if v.is_empty() => return,
            None => self.diags_ext.push((server.to_string(), v)),
        }
        self.rebuild_diags();
    }

    fn rebuild_diags(&mut self) {
        let mut all = self.diags_ts.clone();
        all.extend(self.diags_lint.iter().cloned());
        all.extend(self.diags_eslint.iter().cloned());
        if !self.diags_ext.is_empty() {
            let mut srcs: Vec<&[Diag]> = vec![&all];
            srcs.extend(self.diags_ext.iter().map(|(_, v)| v.as_slice()));
            all = crate::lspmerge::dedupe_diags(&srcs);
        }
        all.sort_by_key(|d| (d.sl, d.sc));
        self.diags = all;
    }

    /// URI as the project sees it (never the language-server mirror).
    pub fn real_uri(&self) -> Option<String> {
        self.path().map(path_to_uri)
    }

    pub fn uri(&self) -> Option<String> {
        self.path().map(|p| path_to_uri(&to_mirror(p)))
    }

    // ── layout ──

    pub fn gutter_digits(&self) -> usize {
        self.buf.line_count().to_string().len().max(MIN_GUTTER_DIGITS)
    }

    pub fn gutter_w(&self) -> i32 {
        if !crate::display::show_line_numbers() {
            return crate::display::NUMLESS_MARKS_W + crate::docview::FOLD_W;
        }
        ((self.gutter_digits() + 3) as f32 * self.cell_w).round() as i32 + crate::docview::FOLD_W
    }

    pub fn visible_lines(&self) -> f64 {
        (self.rect.h as f32 / line_h()).floor().max(1.0) as f64
    }

    pub fn max_top(&self) -> f64 {
        (self.total_rows() as f64 - self.visible_lines()).max(0.0)
    }

    // ── edits ──

    /// Call after anything that may have changed the text. Returns true if it did.
    pub fn sync_edits(&mut self) -> bool {
        let Some(l) = self.buf.take_dirty_line() else { return false };
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(l);
        }
        self.caret_snap = true;
        self.version += 1;
        self.changed_at = Some(Instant::now());
        self.lint_changed_at = Some(Instant::now());
        self.eslint_due = Some(Instant::now() + std::time::Duration::from_millis(700));
        self.sel_stack.clear();
        let recs = self.view_on_edit(l);
        crate::bookmarks::on_edits(&mut self.marks, &recs);
        let buf = &self.buf;
        self.inlay.on_edits(&recs, l, buf.line_count(), &|p| buf.line_col(p));
        // Edits that add or remove no line break leave every other line's width and minimap row as they were.
        if !recs.is_empty() && recs.iter().all(|r| r.rem_lines == 0 && r.ins_lines == 0) {
            let mut lines: Vec<usize> = recs.iter().map(|r| r.line).collect();
            lines.sort_unstable();
            lines.dedup();
            self.keep_widest_after_edit(&lines, self.version - 1);
            if !self.vm_live() && self.mm_cache.1 == self.synced_ver {
                for &ln in &lines {
                    self.mm_cache.2.remove(&ln);
                }
                self.mm_cache.1 = self.buf.version();
            }
        }
        self.synced_ver = self.buf.version();
        self.symbols_due = Some(Instant::now() + std::time::Duration::from_millis(600));
        self.conflicts_due = Some(Instant::now() + std::time::Duration::from_millis(400));
        self.sem_due = Some(Instant::now() + std::time::Duration::from_millis(450));
        self.lens_due = Some(Instant::now() + std::time::Duration::from_millis(1200));
        self.sem_valid_until = self.sem_valid_until.min(l);
        self.git_stale = true;
        self.target = self.target.clamp(0.0, self.max_top());
        self.top = self.top.clamp(0.0, self.max_top());
        true
    }

    // ── scrolling ──

    /// Standard scrolling: no easing, the view moves exactly where asked.
    pub fn scroll_by(&mut self, lines: f64) {
        self.scroll_direct(lines);
    }

    /// Touchpads already smooth their own deltas: follow them 1:1, no extra easing.
    pub fn scroll_direct(&mut self, lines: f64) {
        self.target = (self.target + lines).clamp(0.0, self.max_top());
        self.top = self.target;
        self.animating = false;
    }

    /// Keep the cursor inside the viewport.
    pub fn apply_reveal(&mut self) {
        self.view_reveal_caret();
        let (row, vcol) = self.caret_geom(self.ed.head());
        let vis = self.visible_lines();
        let l = row as f64;
        let before = self.target;
        if l < self.target {
            self.target = l;
        } else if l + 1.0 > self.target + vis {
            self.target = l + 1.0 - vis;
        }
        self.target = self.target.clamp(0.0, self.max_top());
        if (self.target - before).abs() > f64::EPSILON {
            let top_was = self.top;
            self.top = self.target;
            self.animating = false;
            if self.scr.glide_next {
                // Ctrl+Home/End: glide there instead of jumping (the caret glides with the view).
                self.top = top_was.clamp(0.0, self.max_top());
                self.start_reveal_glide();
            }
        }
        self.scr.glide_next = false;
        if self.view.cols > 0 {
            self.left = 0.0; // wrapped text never scrolls sideways
            self.left_target = 0.0;
            return;
        }
        let cw = self.cell_w as f64;
        let cx = vcol as f64 * cw;
        let view_w = self.text_view_w();
        let margin = cw * 4.0;
        if cx < self.left {
            self.left = (cx - margin).max(0.0);
        } else if cx + cw > self.left + view_w {
            self.left = cx + cw - view_w + margin;
        }
        self.left_target = self.left;
    }

    // ── coordinates ──

    /// Char index under window position (x, y).
    pub fn pos_at(&self, x: f64, y: f64) -> usize {
        self.hit_at(x, y).0
    }

    /// Char index under window position (x, y) and the inlay hint the pointer is over, if any
    /// (the char index is then the hint's own column). Works on wrapped rows and with folds.
    pub fn hit_at(&self, x: f64, y: f64) -> (usize, Option<HintHit>) {
        let lh = line_h() as f64;
        let rel_y = y - self.rect.y as f64;
        let row = ((rel_y + self.top * lh) / lh).floor().max(0.0) as usize;
        let tx = (self.rect.x + self.gutter_w()) as f64;
        let cw = self.cell_w as f64;
        let docx = x - tx + self.left;
        let vcol = (docx / cw).round().max(0.0) as usize;
        let cell = (docx / cw).floor().max(0.0) as usize;
        let tab_w = self.ed.tab_w;
        // (char column, hint hit) on `text` with the row's hints (row-relative columns); `idx` maps back to the line's hints.
        let hinted = |line: usize, text: &str, hs: &[crate::inlay::Hint], idx: Option<&[usize]>| {
            if hs.is_empty() {
                return (col_from_visual(text, vcol, tab_w), None);
            }
            let (col, _) = crate::inlay::hit_in(hs, text, vcol, tab_w);
            let hit = crate::inlay::hit_in(hs, text, cell, tab_w).1.map(|(i, start)| HintHit {
                line,
                idx: idx.map_or(i, |m| m[i]),
                off: hs[i].char_at_px((docx - start as f64 * cw) as f32, self.cell_w),
            });
            (col, hit)
        };
        if !self.vm_live() {
            let line = row.min(self.buf.line_count() - 1);
            let text = self.buf.line(line);
            let (col, hit) = hinted(line, &text, self.inlay.hints.hints_on(line), None);
            return (self.buf.pos(line, col), hit);
        }
        let (line, sub) = self.view.map.line_at_row(row);
        let text = self.buf.line(line);
        if self.view.cols == 0 {
            let (col, hit) = hinted(line, &text, self.inlay.hints.hints_on(line), None);
            return (self.buf.pos(line, col), hit);
        }
        let breaks = self.view.breaks(&text);
        let len = text.chars().count();
        let (c0, c1) = crate::docview::row_range(&breaks, sub, len);
        let rowtext: String = text.chars().skip(c0).take(c1 - c0).collect();
        let rh = crate::inlay::row_hints(self.inlay.hints.hints_on(line), len, c0, c1, sub >= breaks.len());
        let (rc, hit) = hinted(line, &rowtext, &rh.hints, Some(&rh.idx));
        let mut c = c0 + rc;
        if sub < breaks.len() {
            c = c.min(c1.saturating_sub(1).max(c0)); // not the last row: stay before the wrap point
        }
        (self.buf.pos(line, c), hit)
    }

    /// Window position (x, y of the cell's top-left) of a char index, if visible.
    pub fn screen_pos(&self, c: usize) -> Option<(i32, i32)> {
        let (row, v) = self.caret_geom(c);
        let x = self.rect.x + self.gutter_w() + (v as f32 * self.cell_w).round() as i32 - self.left.round() as i32;
        let y = self.rect.y + (row as f64 * line_h() as f64 - self.top * line_h() as f64).round() as i32;
        (y >= self.rect.y - line_h() as i32 && y < self.rect.bottom()).then_some((x, y))
    }

    // ── LSP positions (UTF-16 columns) ──

    pub fn lsp_pos(&self, c: usize) -> (usize, usize) {
        let (l, col) = self.buf.line_col(c);
        let u16col: usize = self.buf.line(l).chars().take(col).map(char::len_utf16).sum();
        (l, u16col)
    }

    pub fn pos_from_lsp(&self, line: usize, u16col: usize) -> usize {
        let line = line.min(self.buf.line_count() - 1);
        let mut n = 0;
        let mut col = 0;
        for ch in self.buf.line(line).chars() {
            if n >= u16col {
                break;
            }
            n += ch.len_utf16();
            col += 1;
        }
        self.buf.pos(line, col)
    }

    /// Replace the whole text with `new` as a single undo step, touching only the part that
    /// differs so the cursor and highlighting of the untouched prefix/suffix survive.
    pub fn replace_text(&mut self, new: &str) {
        use crate::buffer::{EditKind, Sel};
        let old = self.buf.to_string();
        if old == new {
            return;
        }
        let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
        let pre = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
        let suf = o[pre..].iter().rev().zip(n[pre..].iter().rev()).take_while(|(a, b)| a == b).count();
        let (oe, ne) = (o.len() - suf, n.len() - suf);
        let mid: String = n[pre..ne].iter().collect();
        let before = self.ed.sel;
        let shift = |p: usize| -> usize {
            if p <= pre {
                p
            } else if p >= oe {
                p + ne - oe
            } else {
                pre + (ne - pre).min(p - pre)
            }
        };
        self.buf.close_group();
        self.buf.begin_compound(before);
        self.buf.replace(pre, oe, &mid, EditKind::Other, before);
        let len = self.buf.len_chars();
        self.ed.sel = Sel { anchor: shift(before.anchor).min(len), head: shift(before.head).min(len) };
        self.buf.end_compound(self.ed.sel);
        self.sync_edits();
    }

    /// Apply LSP text edits (positions in the *current* text) as one undo step.
    /// The selection is shifted by edits that happen before it.
    pub fn apply_lsp_edits(&mut self, edits: &[LspEdit]) {
        let ranges: Vec<(usize, usize, String)> = edits
            .iter()
            .map(|e| (self.pos_from_lsp(e.sl, e.sc), self.pos_from_lsp(e.el, e.ec), e.text.clone()))
            .collect();
        self.apply_char_edits(ranges);
    }

    /// Apply `(start, end, text)` replacements (char positions in the *current* text) as one undo step.
    pub fn apply_char_edits(&mut self, mut ranges: Vec<(usize, usize, String)>) {
        use crate::buffer::{EditKind, Sel};
        if ranges.is_empty() {
            return;
        }
        // Last edit first so earlier positions stay valid; edits at the same spot keep their list
        // order in the result (the later one is applied first), as LSP requires.
        let mut order: Vec<usize> = (0..ranges.len()).collect();
        order.sort_by(|&a, &b| ranges[b].0.cmp(&ranges[a].0).then(ranges[b].1.cmp(&ranges[a].1)).then(b.cmp(&a)));
        ranges = order.into_iter().map(|k| ranges[k].clone()).collect();
        let before = self.ed.sel;
        let shift = |p: usize| {
            let mut np = p as isize;
            for (s, e, t) in &ranges {
                let (s, e) = (*s, *e);
                let n = t.chars().count() as isize;
                if e <= p {
                    np += n - (e - s) as isize;
                } else if s < p {
                    np = s as isize + n;
                }
            }
            np.max(0) as usize
        };
        let sel = Sel { anchor: shift(before.anchor), head: shift(before.head) };
        self.buf.close_group();
        self.buf.begin_compound(before);
        for (s, e, t) in &ranges {
            self.buf.replace(*s, *e, t, EditKind::Other, before);
        }
        let len = self.buf.len_chars();
        self.ed.sel = Sel { anchor: sel.anchor.min(len), head: sel.head.min(len) };
        self.buf.end_compound(self.ed.sel);
        self.sync_edits();
    }

    /// Double click on an inlay hint: apply its text edits (or the type annotation synthesized from
    /// a `: T` hint) as one undo step. False when the hint carries nothing to apply.
    pub fn apply_hint_edits(&mut self, hit: HintHit) -> bool {
        let Some(h) = self.inlay.hints.hints_on(hit.line).get(hit.idx) else { return false };
        let edits = h.annotation_edits();
        if edits.is_empty() {
            return false;
        }
        let (hl, hc) = (h.line as i64, h.col as i64);
        let n = self.buf.line_count() as i64;
        let at = |dl: i64, c: i64| -> Option<usize> {
            let l = hl + dl;
            if l < 0 || l >= n {
                return None;
            }
            let col = if dl == 0 { hc + c } else { c };
            let len = self.buf.line_len(l as usize) as i64;
            Some(self.buf.pos(l as usize, col.clamp(0, len) as usize))
        };
        let ranges: Vec<(usize, usize, String)> =
            edits.iter().filter_map(|e| Some((at(e.dl0, e.c0)?, at(e.dl1, e.c1)?, e.text.clone()))).collect();
        if ranges.is_empty() {
            return false;
        }
        self.apply_char_edits(ranges);
        self.ed.sel = crate::buffer::Sel { anchor: self.ed.sel.head, head: self.ed.sel.head };
        true
    }

    /// The link of the hint label under the pointer (Ctrl+click target), if any.
    pub fn hint_link(&self, hit: HintHit) -> Option<crate::inlay::HintLink> {
        let h = self.inlay.hints.hints_on(hit.line).get(hit.idx)?;
        h.link_at(hit.off?).cloned()
    }
}
