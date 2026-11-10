//! Split editor: up to six panes, in a row (up to four columns) and/or stacked (up to three columns
//! of two panes once anything is stacked, so 3x2 / 2x3), each with its own document, scroll, cursor,
//! folds and word wrap.
//!
//! Model: `App::active` is always the document of the *focused* pane, so everything that anchors on
//! it (find, completion, hover, LSP requests, status bar, breadcrumbs, `layout.editor`) follows the
//! focus for free. The state of the focused pane lives in the document, exactly as without a split;
//! every other pane keeps its state in a [`PaneView`] (scroll, cursor, folds, wrap, a few words each,
//! so panes cost nothing when the split is closed: `App::split` is `None`). Focusing another pane
//! swaps its view into the document, which is how two panes on the same file get separate scroll,
//! cursor, folds and word wrap.
//!
//! Edits: while a document has a stashed pane its buffer keeps an edit log; a stashed view catches
//! up with the edits made since it was stashed (`PaneView::catch_up`), moving its cursors, folds,
//! wrap rows and scroll anchor, so typing in one pane keeps the same file in another pane in step.
//!
//! Reader and diff: the Markdown reader lives inside the focused pane (that pane's rectangle is
//! split into source and preview, `App::md_place`) and the diff view covers the pane that was
//! focused when it opened (`App::diff_area`), so neither closes the split.
//!
//! Session: the grid, the divider ratios and the document / caret / scroll of every pane are saved
//! with the session and restored by `split_restore`.

use crate::{
    app::{App, Focus},
    buffer::{Buffer, Sel},
    doc::{Doc, DrawOpts},
    docview::ViewMap,
    ui::{Canvas, Rect},
};
use winit::{event::MouseScrollDelta, window::CursorIcon};

/// Most panes at once.
pub const MAX_PANES: usize = 6;
/// Most columns: in a plain row, and once any column is stacked.
const MAX_ROW_COLS: usize = 4;
const MAX_GRID_COLS: usize = 3;
/// Narrowest editor area that still splits, the narrowest pane and the shortest stacked pane.
const MIN_AREA: i32 = 360;
const MIN_PANE: i32 = 160;
const MIN_PANE_H: i32 = 80;
/// Pixels around the 1px divider that grab the mouse.
const GRAB: i32 = 3;

// ── geometry (pure) ──

/// One column of the grid: one pane, or two stacked (`vratio` = share of the height of the top one).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Col {
    pub n: usize,
    pub vratio: f32,
}

/// Columns left to right, each with one or two panes. Panes are numbered column by column, top to
/// bottom (that is the order of `Split::panes`).
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub cols: Vec<Col>,
    /// Share of the width of each column (sums to 1).
    pub widths: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DivKind {
    /// Vertical bar between columns `i` and `i + 1`.
    Col(usize),
    /// Horizontal bar between the two panes stacked in column `i`.
    Row(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct Div {
    pub rect: Rect,
    pub kind: DivKind,
}

impl Grid {
    pub fn single() -> Grid {
        Grid { cols: vec![Col { n: 1, vratio: 0.5 }], widths: vec![1.0] }
    }

    pub fn panes(&self) -> usize {
        self.cols.iter().map(|c| c.n).sum()
    }

    pub fn stacked(&self) -> bool {
        self.cols.iter().any(|c| c.n > 1)
    }

    /// Index of the first pane of column `col`.
    pub fn first_of(&self, col: usize) -> usize {
        self.cols[..col].iter().map(|c| c.n).sum()
    }

    /// (column, row) of pane `pane`.
    pub fn locate(&self, pane: usize) -> (usize, usize) {
        let mut left = pane;
        for (i, c) in self.cols.iter().enumerate() {
            if left < c.n {
                return (i, left);
            }
            left -= c.n;
        }
        (self.cols.len().saturating_sub(1), 0)
    }

    pub fn can_split_right(&self) -> bool {
        self.panes() < MAX_PANES && self.cols.len() < if self.stacked() { MAX_GRID_COLS } else { MAX_ROW_COLS }
    }

    pub fn can_split_below(&self, pane: usize) -> bool {
        let (c, _) = self.locate(pane);
        self.panes() < MAX_PANES && self.cols[c].n == 1 && self.cols.len() <= MAX_GRID_COLS
    }

    /// The layout as JSON for the session.
    pub fn to_json(&self) -> serde_json::Value {
        let cols: Vec<_> = self.cols.iter().map(|c| serde_json::json!({"n": c.n, "v": c.vratio})).collect();
        serde_json::json!({"cols": cols, "widths": self.widths})
    }

    /// A grid from the session; `None` for anything that could not have been made by splitting.
    pub fn from_json(v: &serde_json::Value) -> Option<Grid> {
        let cols: Vec<Col> = v["cols"]
            .as_array()?
            .iter()
            .map(|c| {
                let n = c["n"].as_u64()? as usize;
                let vratio = c["v"].as_f64().unwrap_or(0.5) as f32;
                ((1..=2).contains(&n) && vratio.is_finite()).then(|| Col { n, vratio: vratio.clamp(0.05, 0.95) })
            })
            .collect::<Option<_>>()?;
        let widths: Vec<f32> = v["widths"]
            .as_array()?
            .iter()
            .map(|w| w.as_f64().map(|w| w as f32).filter(|w| w.is_finite() && *w > 0.0))
            .collect::<Option<_>>()?;
        let g = Grid { cols, widths: Vec::new() };
        let stacked = g.stacked();
        if g.cols.is_empty()
            || widths.len() != g.cols.len()
            || g.panes() > MAX_PANES
            || g.cols.len() > if stacked { MAX_GRID_COLS } else { MAX_ROW_COLS }
        {
            return None;
        }
        let sum: f32 = widths.iter().sum();
        Some(Grid { widths: widths.iter().map(|w| w / sum).collect(), ..g })
    }

    /// New column right of `pane`'s, taking half of that column's width. Returns the new pane's index.
    pub fn split_right(&mut self, pane: usize) -> usize {
        let (c, _) = self.locate(pane);
        let w = self.widths[c] / 2.0;
        self.widths[c] = w;
        self.widths.insert(c + 1, w);
        self.cols.insert(c + 1, Col { n: 1, vratio: 0.5 });
        self.first_of(c + 1)
    }

    /// Stack a new pane under `pane` (which must be alone in its column). Returns its index.
    pub fn split_below(&mut self, pane: usize) -> usize {
        let (c, _) = self.locate(pane);
        self.cols[c] = Col { n: 2, vratio: 0.5 };
        pane + 1
    }

    /// Take pane `pane` out: its column shrinks to the other pane, or disappears (the neighbor
    /// takes the space).
    pub fn remove(&mut self, pane: usize) {
        let (c, _) = self.locate(pane);
        if self.cols[c].n > 1 {
            self.cols[c] = Col { n: 1, vratio: 0.5 };
        } else if self.cols.len() > 1 {
            let w = self.widths.remove(c);
            self.cols.remove(c);
            let to = c.saturating_sub(1);
            self.widths[to] += w;
        }
    }

    /// Pixel offset (from the left of the area, dividers excluded) where each column ends.
    fn edges(&self, avail: i32) -> Vec<i32> {
        let nc = self.cols.len() as i32;
        let mut out = Vec::with_capacity(self.cols.len());
        let mut cum = 0.0f32;
        let mut prev = 0;
        for i in 0..self.cols.len() {
            let e = if i + 1 == self.cols.len() {
                avail
            } else {
                cum += self.widths[i];
                let lo = prev + MIN_PANE;
                let hi = (avail - (nc - 1 - i as i32) * MIN_PANE).max(lo);
                ((avail as f32 * cum).round() as i32).clamp(lo, hi)
            };
            out.push(e);
            prev = e;
        }
        out
    }

    /// Widest area the grid needs.
    fn min_width(&self) -> i32 {
        let nc = self.cols.len() as i32;
        if nc <= 1 { 0 } else { MIN_AREA.max(nc * MIN_PANE + nc - 1) }
    }

    /// Rectangles of the panes (in pane order) and the dividers inside `card`; `None` when there
    /// is no room (the editor then shows the focused pane alone).
    pub fn layout(&self, card: Rect) -> Option<(Vec<Rect>, Vec<Div>)> {
        let nc = self.cols.len();
        if card.w < self.min_width() || (self.stacked() && card.h < 2 * MIN_PANE_H + 1) {
            return None;
        }
        let avail = card.w - (nc as i32 - 1);
        let edges = self.edges(avail);
        let mut rects = Vec::with_capacity(self.panes());
        let mut divs = Vec::new();
        let mut start = 0;
        for (i, col) in self.cols.iter().enumerate() {
            let x = card.x + start + i as i32;
            let w = edges[i] - start;
            if col.n == 1 {
                rects.push(Rect::new(x, card.y, w, card.h));
            } else {
                let th = (((card.h - 1) as f32 * col.vratio).round() as i32).clamp(MIN_PANE_H, card.h - 1 - MIN_PANE_H);
                rects.push(Rect::new(x, card.y, w, th));
                rects.push(Rect::new(x, card.y + th + 1, w, card.h - th - 1));
                divs.push(Div { rect: Rect::new(x, card.y + th, w, 1), kind: DivKind::Row(i) });
            }
            if i + 1 < nc {
                divs.push(Div { rect: Rect::new(x + w, card.y, 1, card.h), kind: DivKind::Col(i) });
            }
            start = edges[i];
        }
        Some((rects, divs))
    }

    /// Drag divider `kind` to window coordinate `pos` (x for a column bar, y for a row bar).
    pub fn drag(&mut self, card: Rect, kind: DivKind, pos: f64) {
        match kind {
            DivKind::Col(i) => {
                if i + 1 >= self.cols.len() {
                    return;
                }
                let avail = card.w - (self.cols.len() as i32 - 1);
                let edges = self.edges(avail);
                let s = if i == 0 { 0 } else { edges[i - 1] };
                let t = edges[i + 1];
                if t - s < 2 * MIN_PANE {
                    return;
                }
                let e = ((pos - card.x as f64) as i32 - i as i32).clamp(s + MIN_PANE, t - MIN_PANE);
                let pair = self.widths[i] + self.widths[i + 1];
                self.widths[i] = pair * (e - s) as f32 / (t - s) as f32;
                self.widths[i + 1] = pair - self.widths[i];
            }
            DivKind::Row(i) => {
                let h = (card.h - 1).max(1) as f32;
                let lo = MIN_PANE_H as f32 / h;
                let r = (((pos - card.y as f64) as f32) / h).clamp(lo, (1.0 - lo).max(lo));
                if let Some(c) = self.cols.get_mut(i) {
                    c.vratio = r;
                }
            }
        }
    }
}

/// Tab the pane left behind falls back to when its document moves to another pane: the next tab in
/// display order, else the previous one.
pub fn pick_replacement(order: &[usize], moved: usize) -> Option<usize> {
    let pos = order.iter().position(|&d| d == moved)?;
    order.get(pos + 1).or_else(|| pos.checked_sub(1).and_then(|p| order.get(p))).copied()
}

// ── per-pane state ──

/// State of one pane while it is not the focused one.
#[derive(Clone, Default)]
pub struct PaneView {
    pub top: f64,
    pub target: f64,
    pub left: f64,
    pub left_target: f64,
    pub sel: Option<Sel>,
    pub extra: Vec<Sel>,
    /// Where the pane was last drawn (so a click right after a focus change hits the right cells).
    pub rect: Rect,
    /// Folds, word wrap and the row map of this pane.
    pub view: ViewMap,
    /// Buffer this state describes and how many of its logged edits it has already followed.
    buf_gen: u64,
    seen: u64,
}

impl PaneView {
    /// Follow the edits made to `buf` since this view was stashed: cursors, folds, wrap rows and
    /// the scroll anchor move with the text. A replaced buffer (reload from disk) resets the view.
    pub fn catch_up(&mut self, buf: &Buffer) {
        if self.buf_gen == buf.gen_id() && self.seen == buf.log_seq() {
            return;
        }
        let n = buf.len_chars();
        match (self.buf_gen == buf.gen_id()).then(|| buf.edits_since(self.seen)).flatten() {
            Some(recs) if !recs.is_empty() => {
                let map = |mut p: usize| {
                    for r in recs {
                        p = r.map_pos(p);
                    }
                    p
                };
                let fix = |s: Sel| Sel { anchor: map(s.anchor).min(n), head: map(s.head).min(n) };
                self.sel = self.sel.map(fix);
                for s in &mut self.extra {
                    *s = fix(*s);
                }
                // Keep the line that was at the top of the pane at the top.
                let old_row = self.top.max(0.0) as usize;
                let live = self.view.active() && self.view.map.line_count() == self.view.tracked_lines();
                let (aline, asub) = if live { self.view.map.line_at_row(old_row) } else { (old_row, 0) };
                let mut line = aline;
                for r in recs {
                    line = r.map_line(line);
                }
                let first = recs.iter().map(|r| r.line).min().unwrap_or(0);
                self.view.on_edit(recs, first, buf);
                let nl = buf.line_count();
                let line = line.min(nl - 1);
                let (new_row, total) = if self.view.active() && self.view.map.line_count() == nl {
                    let h = self
                        .view
                        .folds
                        .hidden_ranges()
                        .iter()
                        .find(|r| r.0 <= line && line <= r.1)
                        .map_or(line, |r| r.0 - 1);
                    (
                        self.view.map.row_of(h) + asub.min(self.view.map.rows_in(h).saturating_sub(1)),
                        self.view.map.total_rows(),
                    )
                } else {
                    (line, nl)
                };
                if new_row != old_row {
                    self.top = new_row as f64;
                    self.target = self.top;
                }
                let vis = (self.rect.h as f32 / crate::ui::line_h()).floor().max(1.0) as f64;
                let max = (total as f64 - vis).max(0.0);
                self.top = self.top.clamp(0.0, max);
                self.target = self.target.clamp(0.0, max);
            }
            _ => {
                let fix = |s: Sel| Sel { anchor: s.anchor.min(n), head: s.head.min(n) };
                self.sel = self.sel.map(fix);
                for s in &mut self.extra {
                    *s = fix(*s);
                }
                self.view.folds.unfold_all();
                self.view.avail_ver = -1;
                self.view.lsp_ver = -1;
                self.view.reset();
                self.top = self.top.min(buf.line_count().saturating_sub(1) as f64);
                self.target = self.top;
            }
        }
        self.buf_gen = buf.gen_id();
        self.seen = buf.log_seq();
    }
}

impl Doc {
    /// The focused pane's state, copied (the document keeps showing it).
    pub fn pane_view(&mut self) -> PaneView {
        self.sync_edits();
        self.buf.log_start();
        PaneView {
            top: self.top,
            target: self.target,
            left: self.left,
            left_target: self.left_target,
            sel: Some(self.ed.sel),
            extra: self.ed.extra.clone(),
            rect: self.rect,
            view: self.view.clone(),
            buf_gen: self.buf.gen_id(),
            seen: self.buf.log_seq(),
        }
    }

    /// Exchange the document's pane state with `v`: the pane in `v` is shown, and `v` gets what was
    /// shown. `v` first catches up with the edits it missed; the clamping covers anything left.
    pub fn swap_pane_view(&mut self, v: &mut PaneView) {
        self.sync_edits(); // edits still pending belong to the view being put away
        self.buf.log_start();
        v.catch_up(&self.buf);
        let n = self.buf.len_chars();
        let fix = |s: Sel| Sel { anchor: s.anchor.min(n), head: s.head.min(n) };
        std::mem::swap(&mut self.top, &mut v.top);
        std::mem::swap(&mut self.target, &mut v.target);
        std::mem::swap(&mut self.left, &mut v.left);
        std::mem::swap(&mut self.left_target, &mut v.left_target);
        std::mem::swap(&mut self.view, &mut v.view);
        std::mem::swap(&mut self.ed.extra, &mut v.extra);
        let mine = self.ed.sel;
        if let Some(s) = v.sel.replace(mine) {
            self.ed.sel = fix(s);
        }
        self.ed.extra.iter_mut().for_each(|s| *s = fix(*s));
        if v.rect.w > 0 {
            std::mem::swap(&mut self.rect, &mut v.rect);
        } else {
            v.rect = self.rect;
        }
        v.buf_gen = self.buf.gen_id();
        v.seen = self.buf.log_seq();
        let max = self.max_top();
        self.top = self.top.clamp(0.0, max);
        self.target = self.target.clamp(0.0, max);
        self.left = self.left.max(0.0);
        self.left_target = self.left_target.max(0.0);
        // a pane that was still gliding keeps gliding (the scrollers re-adopt these values)
        self.animating = (self.top - self.target).abs() > 1e-9 || (self.left - self.left_target).abs() > 1e-9;
    }

    /// Show the pane state `v` (dropping what was shown).
    #[allow(dead_code)]
    pub fn set_pane_view(&mut self, mut v: PaneView) {
        self.swap_pane_view(&mut v);
    }

    /// Forget what the last draw said about clickable areas (they belonged to another pane).
    fn forget_hits(&mut self) {
        self.minimap_layout = None;
        self.fold_pills.clear();
        self.lens_hits.clear();
        self.conflict_hits.clear();
    }
}

pub struct Pane {
    /// Document shown. Meaningless for the focused pane (its document is `App::active`).
    pub doc: usize,
    pub view: PaneView,
}

pub struct Split {
    pub panes: Vec<Pane>,
    pub focus: usize,
    pub grid: Grid,
    /// Pane rectangles and dividers of the last layout (empty when the area is too small).
    pub rects: Vec<Rect>,
    pub divs: Vec<Div>,
    pub drag: Option<DivKind>,
    /// Pane the open diff view covers (set when a diff appears, follows pane removals).
    pub diff_at: Option<usize>,
}

impl Split {
    /// A split of one pane on `doc` (the caller adds the second one).
    fn new(doc: usize, view: PaneView) -> Split {
        Split {
            panes: vec![Pane { doc, view }],
            focus: 0,
            grid: Grid::single(),
            rects: Vec::new(),
            divs: Vec::new(),
            drag: None,
            diff_at: None,
        }
    }

    /// Document shown by pane `i` when `active` is the focused pane's.
    pub fn doc_of(&self, i: usize, active: usize) -> usize {
        if i == self.focus { active } else { self.panes[i].doc }
    }
}

/// Focus pane `new`: the focused pane's state is put away and the new pane's state shown, and
/// `active` becomes its document.
pub fn focus_to(docs: &mut [Doc], active: &mut usize, sp: &mut Split, new: usize) {
    if new >= sp.panes.len() || new == sp.focus || *active >= docs.len() {
        return;
    }
    let nd = sp.panes[new].doc;
    if nd >= docs.len() {
        return;
    }
    let old = sp.focus;
    let od = *active;
    sp.panes[old].doc = od;
    sp.panes[old].view = docs[od].pane_view();
    let mut theirs = std::mem::take(&mut sp.panes[new].view);
    docs[nd].swap_pane_view(&mut theirs);
    *active = nd;
    sp.focus = new;
    if nd == od {
        docs[nd].forget_hits();
    }
}

/// Take pane `i` out of the split (it must not be the focused one: move the focus first).
fn remove_pane(sp: &mut Split, i: usize) {
    sp.grid.remove(i);
    sp.panes.remove(i);
    if sp.focus > i {
        sp.focus -= 1;
    }
    sp.diff_at = match sp.diff_at {
        Some(d) if d == i => None,
        Some(d) if d > i => Some(d - 1),
        d => d,
    };
    sp.drag = None;
}

/// A document was removed from the tab list: unfocused panes on it close, the others keep pointing
/// at the same document. The split goes away when a single pane is left.
pub fn after_remove(split: &mut Option<Box<Split>>, removed: usize, remaining: usize) {
    let Some(sp) = split.as_mut() else { return };
    if remaining == 0 {
        *split = None;
        return;
    }
    let mut i = sp.panes.len();
    while i > 0 {
        i -= 1;
        if i == sp.focus {
            continue;
        }
        if sp.panes[i].doc == removed {
            remove_pane(sp, i);
        } else if sp.panes[i].doc > removed {
            sp.panes[i].doc -= 1;
        }
    }
    if sp.panes.len() < 2 {
        *split = None;
    }
}

/// Copy the fold ranges the language server gave document `di` (they live in the view of the
/// pane that is focused) into every other pane showing it.
fn share_lsp_folds(docs: &[Doc], sp: &mut Split, di: usize) {
    let v = &docs[di].view;
    for (i, p) in sp.panes.iter_mut().enumerate() {
        if i != sp.focus && p.doc == di {
            p.view.view.lsp_regions = v.lsp_regions.clone();
            p.view.view.lsp_ver = v.lsp_ver;
            p.view.view.avail_ver = -1;
        }
    }
}

/// Trim the edit logs: every buffer keeps only what some stashed pane still has to follow.
fn trim_logs(docs: &mut [Doc], sp: &Split) {
    for (di, d) in docs.iter_mut().enumerate() {
        let min =
            sp.panes.iter().enumerate().filter(|&(i, p)| i != sp.focus && p.doc == di).map(|(_, p)| p.view.seen).min();
        d.buf.log_trim(min);
    }
}

impl App {
    /// Documents of the panes, in pane order, with the focused one flagged: for the tab bar.
    pub fn split_pane_docs(&self) -> Vec<(usize, bool)> {
        match self.split.as_ref() {
            Some(sp) => (0..sp.panes.len()).map(|i| (sp.doc_of(i, self.active), i == sp.focus)).collect(),
            None => Vec::new(),
        }
    }

    /// `close_tab` removed document `idx`.
    pub fn split_doc_removed(&mut self, idx: usize) {
        after_remove(&mut self.split, idx, self.docs.len());
    }

    /// The split layout for the session file. `file_ix[doc]` is the document's entry in the saved
    /// file list; `None` (no split saved) when a pane shows a document without a file.
    pub fn split_session(&self, file_ix: &[Option<usize>]) -> Option<serde_json::Value> {
        let sp = self.split.as_ref()?;
        let mut panes = Vec::with_capacity(sp.panes.len());
        for i in 0..sp.panes.len() {
            let di = sp.doc_of(i, self.active);
            let d = self.docs.get(di)?;
            let file = (*file_ix.get(di)?)?;
            let (head, top) = if i == sp.focus {
                (d.ed.sel.head, d.target)
            } else {
                // follow the edits made since the pane was put away (on a copy: saving is read-only)
                let mut v = sp.panes[i].view.clone();
                v.catch_up(&d.buf);
                (v.sel.map_or(d.ed.sel.head, |s| s.head), v.target)
            };
            let (line, col) = d.buf.line_col(head.min(d.buf.len_chars()));
            panes.push(serde_json::json!({"file": file, "line": line, "col": col, "top": top}));
        }
        let mut v = sp.grid.to_json();
        v["focus"] = serde_json::json!(sp.focus);
        v["panes"] = serde_json::Value::Array(panes);
        Some(v)
    }

    /// Rebuild the split saved by `split_session`. `opened[file]` is the document each saved file
    /// became (`None` when it could not be opened); anything inconsistent restores no split.
    pub fn split_restore(&mut self, v: &serde_json::Value, opened: &[Option<usize>]) {
        let Some(grid) = Grid::from_json(v) else { return };
        let Some(items) = v["panes"].as_array().filter(|a| a.len() == grid.panes() && a.len() >= 2) else { return };
        let focus = v["focus"].as_u64().unwrap_or(0) as usize;
        if focus >= items.len() {
            return;
        }
        let mut shown = Vec::with_capacity(items.len());
        for it in items {
            let doc = it["file"]
                .as_u64()
                .and_then(|f| opened.get(f as usize).copied().flatten())
                .filter(|&d| d < self.docs.len());
            let Some(doc) = doc else { return };
            let n = |k: &str| it[k].as_u64().unwrap_or(0) as usize;
            shown.push((doc, n("line"), n("col"), it["top"].as_f64().unwrap_or(0.0).max(0.0)));
        }
        let mut panes = Vec::with_capacity(shown.len());
        for &(doc, line, col, top) in &shown {
            let d = &mut self.docs[doc];
            let mut view = d.pane_view();
            let at = d.buf.pos(line.min(d.buf.line_count() - 1), col);
            view.sel = Some(Sel::caret(at));
            view.extra.clear();
            (view.top, view.target, view.left, view.left_target) = (top, top, 0.0, 0.0);
            panes.push(Pane { doc, view });
        }
        // The focused pane's state lives in its document.
        let fd = panes[focus].doc;
        let mut theirs = std::mem::take(&mut panes[focus].view);
        self.docs[fd].swap_pane_view(&mut theirs);
        self.docs[fd].reveal = false;
        panes[focus].view = theirs;
        self.activate(fd);
        self.split = Some(Box::new(Split {
            panes,
            focus,
            grid,
            rects: Vec::new(),
            divs: Vec::new(),
            drag: None,
            diff_at: None,
        }));
        self.split_layout();
    }

    /// The language server's fold ranges for document `di` reach every pane that shows it (the
    /// focused pane's view lives in the document, the others in their `PaneView`).
    pub fn set_lsp_folds_all(&mut self, di: usize, regions: Vec<(usize, usize)>) {
        let Some(d) = self.docs.get_mut(di) else { return };
        d.set_lsp_folds(regions);
        if let Some(sp) = self.split.as_mut() {
            share_lsp_folds(&self.docs, sp, di);
        }
    }

    /// Called by `compute_layout` in place of `md_split`: sets `layout.editor` (the focused pane).
    pub fn split_layout(&mut self) {
        self.layout.other = Rect::default();
        self.layout.divider = Rect::default();
        self.layout.editor = self.layout.card;
        if self.split.is_some() && self.docs.is_empty() {
            self.split = None;
        }
        let card = self.layout.card;
        let Some(sp) = self.split.as_deref_mut() else {
            // No panes: no edit log to keep for them.
            for d in &mut self.docs {
                d.buf.log_trim(None);
            }
            return self.md_split();
        };
        self.layout.md = Rect::default();
        match sp.grid.layout(card) {
            Some((rects, divs)) => {
                self.layout.editor = rects[sp.focus.min(rects.len() - 1)];
                sp.rects = rects;
                sp.divs = divs;
            }
            None => {
                sp.rects.clear();
                sp.divs.clear();
            }
        }
        // The diff covers the pane that was focused when it opened.
        if self.diff.is_none() {
            sp.diff_at = None;
        } else if sp.diff_at.is_none_or(|i| i >= sp.panes.len()) {
            sp.diff_at = Some(sp.focus);
        }
        // The Markdown reader takes the right part of the focused pane.
        let pane = self.layout.editor;
        self.md_place(pane);
    }

    /// Where the diff view draws: the pane it opened in, or the whole editor area without a split.
    pub fn diff_area(&self) -> Rect {
        let Some(sp) = self.split.as_ref() else { return self.layout.editor };
        match sp.diff_at.and_then(|i| sp.rects.get(i)) {
            Some(r) => *r,
            None => self.layout.editor,
        }
    }

    /// Is the pane the diff covers the focused one (so keys belong to the diff)?
    pub fn diff_pane_focused(&self) -> bool {
        self.split.as_ref().is_none_or(|sp| sp.diff_at.is_none_or(|i| i == sp.focus))
    }

    /// Open a new pane (right of the focused one, or below it) on the focused document and focus it.
    fn split_add(&mut self, below: bool) {
        if self.docs.is_empty() {
            return self.say("Open a file to split the editor");
        }
        let fresh = self.split.is_none();
        if fresh {
            let view = self.docs[self.active].pane_view();
            self.split = Some(Box::new(Split::new(self.active, view)));
        }
        let card = self.layout.card;
        let (active, mut sp) = (self.active, self.split.take().unwrap());
        if self.diff.is_some() && sp.diff_at.is_none() {
            sp.diff_at = Some(sp.focus); // the open diff stays in the pane it was opened in
        }
        let mut grid = sp.grid.clone();
        let msg = if sp.panes.len() >= MAX_PANES {
            Some("Maximum of 6 panes")
        } else if below && !grid.can_split_below(sp.focus) {
            Some(if grid.cols.len() > MAX_GRID_COLS {
                "Splitting down only works with up to 3 columns (grid of at most 3x2)"
            } else {
                "This pane is already split below"
            })
        } else if !below && !grid.can_split_right() {
            Some(if grid.stacked() {
                "With stacked panes, splitting right only works up to 3 columns (grid of at most 3x2)"
            } else {
                "Maximum of 4 columns"
            })
        } else {
            None
        };
        let new = if msg.is_none() {
            Some(if below { grid.split_below(sp.focus) } else { grid.split_right(sp.focus) })
        } else {
            None
        };
        let fits = new.is_some() && grid.layout(card).is_some();
        let (Some(new), true) = (new, fits) else {
            self.say(msg.unwrap_or("No room to split here — widen the window"));
            self.split = if fresh { None } else { Some(sp) };
            return;
        };
        let view = self.docs[active].pane_view();
        sp.panes[sp.focus].doc = active;
        sp.panes[sp.focus].view = view.clone();
        sp.panes.insert(new, Pane { doc: active, view });
        if let Some(d) = sp.diff_at.as_mut().filter(|d| **d >= new) {
            *d += 1;
        }
        sp.grid = grid;
        sp.focus = new;
        sp.drag = None;
        let n = sp.panes.len();
        self.split = Some(sp);
        self.hover = None;
        self.completion = None;
        self.focus = Focus::Editor;
        self.split_layout();
        self.say(format!("Editor split into {n} panes — click a tab to choose the focused pane's document"));
        self.request_redraw();
    }

    /// "Dividir editor à direita": the new pane starts on the same document and has the focus;
    /// click another tab to put a different document in it.
    pub fn split_right(&mut self) {
        self.split_add(false);
    }

    /// "Dividir editor abaixo".
    pub fn split_down(&mut self) {
        self.split_add(true);
    }

    /// "Close split": back to a single editor on the focused pane's document.
    pub fn split_close(&mut self) {
        if self.split.take().is_none() {
            return self.say("The editor is not split");
        }
        self.hover = None;
        self.completion = None;
        self.split_layout();
        self.request_redraw();
    }

    /// "Close focused pane": the others stay; the focus goes to the previous pane (or the next).
    pub fn close_focused_pane(&mut self) {
        let Some(mut sp) = self.split.take() else { return self.say("The editor is not split") };
        let old = sp.focus;
        let to = if old > 0 { old - 1 } else { 1 };
        self.split_focus_in(&mut sp, to);
        remove_pane(&mut sp, old);
        self.split = (sp.panes.len() > 1).then_some(sp);
        self.after_focus_change();
    }

    fn split_focus_in(&mut self, sp: &mut Split, i: usize) {
        focus_to(&mut self.docs, &mut self.active, sp, i);
    }

    fn after_focus_change(&mut self) {
        self.hover = None;
        self.hover_probe = None;
        self.completion = None;
        self.close_armed = None;
        self.focus = Focus::Editor;
        self.split_layout();
        self.request_redraw();
    }

    /// Focus pane `i` (0-based) by index.
    pub fn split_focus_idx(&mut self, i: usize) {
        let Some(mut sp) = self.split.take() else { return };
        if i != sp.focus {
            self.split_focus_in(&mut sp, i);
        }
        self.split = Some(sp);
        self.after_focus_change();
    }

    /// Ctrl+1..4: focus pane number `n` (0-based).
    pub fn focus_pane_n(&mut self, n: usize) {
        let Some(count) = self.split.as_ref().map(|s| s.panes.len()) else {
            return self.say("The editor is not split (Ctrl+\\ splits)");
        };
        if n >= count {
            return self.say(format!("There is no pane {} (the editor has {count})", n + 1));
        }
        self.split_focus_idx(n);
    }

    /// Focus the next (`delta` 1) or previous (-1) pane, wrapping around.
    pub fn focus_next_pane(&mut self, delta: isize) {
        let Some((f, n)) = self.split.as_ref().map(|s| (s.focus, s.panes.len())) else {
            return self.say("The editor is not split (Ctrl+\\ splits)");
        };
        self.split_focus_idx((f as isize + delta).rem_euclid(n as isize) as usize);
    }

    /// Move the focused tab's document into the next/previous pane (and follow it there). The pane
    /// it leaves shows a neighboring tab, or closes when there is none.
    pub fn move_to_pane(&mut self, delta: isize) {
        let Some(mut sp) = self.split.take() else { return self.say("The editor is not split (Ctrl+\\ splits)") };
        let n = sp.panes.len() as isize;
        let (src, d) = (sp.focus, self.active);
        let target = (src as isize + delta).rem_euclid(n) as usize;
        if target == src || d >= self.docs.len() {
            self.split = Some(sp);
            return;
        }
        let order = crate::app_ctxmenu::tab_order(&self.docs);
        let repl = pick_replacement(&order, d);
        let view = self.docs[d].pane_view();
        sp.panes[target] = Pane { doc: d, view };
        match repl {
            Some(r) => {
                let v = self.docs[r].pane_view();
                sp.panes[src] = Pane { doc: r, view: v };
                sp.focus = target;
            }
            None => {
                sp.focus = target;
                remove_pane(&mut sp, src);
            }
        }
        self.split = (sp.panes.len() > 1).then_some(sp);
        self.after_focus_change();
        self.say("Tab moved to the other pane");
    }

    /// "Open to the side" and friends: put document `idx` in the pane after the focused one.
    pub fn split_set_other_doc(&mut self, idx: usize) {
        if idx >= self.docs.len() {
            return;
        }
        let Some(sp) = self.split.as_mut() else { return };
        let to = (sp.focus + 1) % sp.panes.len();
        sp.panes[to] = Pane { doc: idx, view: self.docs[idx].pane_view() };
    }

    /// Word wrap command with a split open: only the focused pane changes (a pane's wrap is its own).
    pub fn split_toggle_wrap(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let on = !d.view.wrap;
        d.set_word_wrap(on);
        self.say(if on { "word wrap for this pane: on" } else { "word wrap for this pane: off" });
    }

    /// The global word wrap setting changed: every pane follows it (stashed panes included).
    pub fn split_wrap_all(&mut self, on: bool) {
        if let Some(sp) = self.split.as_mut() {
            for p in &mut sp.panes {
                p.view.view.wrap = on;
                if on {
                    p.view.left = 0.0;
                    p.view.left_target = 0.0;
                }
            }
        }
    }

    /// Cursor to show at (x, y) when it is over a pane divider.
    pub fn split_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let sp = self.split.as_ref()?;
        if let Some(k) = sp.drag {
            return Some(if matches!(k, DivKind::Col(_)) { CursorIcon::ColResize } else { CursorIcon::RowResize });
        }
        sp.divs
            .iter()
            .find(|d| grab_rect(d).contains(x, y))
            .map(|d| if matches!(d.kind, DivKind::Col(_)) { CursorIcon::ColResize } else { CursorIcon::RowResize })
    }

    /// Is (x, y) inside a pane that is not focused?
    pub fn split_in_other(&self, x: f64, y: f64) -> bool {
        self.split
            .as_ref()
            .is_some_and(|sp| sp.rects.iter().enumerate().any(|(i, r)| i != sp.focus && r.contains(x, y)))
    }

    /// Draw the unfocused panes and the dividers. Runs BEFORE the focused pane, so that when two
    /// panes show the same document the geometry it keeps (rect, minimap, fold markers) is the
    /// focused pane's.
    pub fn draw_split_other(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        let Some(mut sp) = self.split.take() else { return false };
        let mut busy = false;
        if sp.rects.len() == sp.panes.len() {
            for i in 0..sp.panes.len() {
                let r = sp.rects[i];
                if i == sp.focus || r.w <= 0 {
                    continue;
                }
                let di = sp.panes[i].doc;
                let (find, find_cur) = match (&self.find, di == self.active) {
                    (Some(f), true) => (f.matches.clone(), f.current),
                    _ => (Vec::new(), None),
                };
                if let Some(d) = self.docs.get_mut(di) {
                    d.cell_w = self.tx.cell_w;
                    let reveal = std::mem::replace(&mut d.reveal, false);
                    d.swap_pane_view(&mut sp.panes[i].view);
                    busy |= d.draw(
                        cv,
                        &mut self.tx,
                        &self.theme,
                        r,
                        &DrawOpts { dt, find: &find, find_cur, focused: false },
                    );
                    d.swap_pane_view(&mut sp.panes[i].view);
                    d.reveal = reveal;
                } else {
                    cv.fill(r, self.theme.background);
                }
            }
            for dv in &sp.divs {
                cv.fill(dv.rect, self.theme.chrome.border);
            }
        }
        trim_logs(&mut self.docs, &sp);
        self.split = Some(sp);
        busy
    }

    /// Mouse press: grabs a divider, or moves the focus when the press is in another pane (the
    /// press then continues as a normal editor click: returns false).
    pub fn split_mouse_down(&mut self, x: f64, y: f64) -> bool {
        let Some(sp) = self.split.as_mut() else { return false };
        if let Some(d) = sp.divs.iter().find(|d| grab_rect(d).contains(x, y)) {
            sp.drag = Some(d.kind);
            return true;
        }
        let hit = sp.rects.iter().enumerate().find(|&(i, r)| i != sp.focus && r.contains(x, y)).map(|(i, _)| i);
        if let Some(i) = hit {
            self.split_focus_idx(i);
        }
        false
    }

    /// Mouse move while dragging a divider.
    pub fn split_drag(&mut self, x: f64, y: f64) -> bool {
        let card = self.layout.card;
        let Some(sp) = self.split.as_mut().filter(|s| s.drag.is_some()) else { return self.md_drag(x) };
        match sp.drag {
            Some(k @ DivKind::Col(_)) => sp.grid.drag(card, k, x),
            Some(k @ DivKind::Row(_)) => sp.grid.drag(card, k, y),
            None => {}
        }
        self.split_layout();
        true
    }

    pub fn split_release(&mut self) {
        self.md_release();
        if let Some(sp) = self.split.as_mut() {
            sp.drag = None;
        }
    }

    /// Wheel over an unfocused pane scrolls it without moving the focus.
    pub fn split_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let Some(i) = self.split.as_ref().and_then(|sp| {
            sp.rects.iter().enumerate().find(|&(i, r)| i != sp.focus && r.contains(x, y)).map(|(i, _)| i)
        }) else {
            return false;
        };
        let w = self.wheel_in(delta);
        let Some(mut sp) = self.split.take() else { return false };
        if let Some(d) = self.docs.get_mut(sp.panes[i].doc) {
            d.swap_pane_view(&mut sp.panes[i].view);
            d.wheel(w, crate::scroller::now());
            d.swap_pane_view(&mut sp.panes[i].view);
        }
        self.split = Some(sp);
        true
    }
}

fn grab_rect(d: &Div) -> Rect {
    Rect::new(d.rect.x - GRAB, d.rect.y - GRAB, d.rect.w + 2 * GRAB, d.rect.h + 2 * GRAB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    fn doc(text: &str) -> Doc {
        let mut d = Doc::new(Buffer::from_str(text), None, None, "//", 8.0);
        d.rect = Rect::new(0, 0, 400, 200);
        d
    }

    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    fn code() -> String {
        // 0 fn a() {   1     x();   2 }   3 fn b() {   4     y();   5 }   6 fn c() {   7     z();   8 }
        "fn a() {\n    x();\n}\nfn b() {\n    y();\n}\nfn c() {\n    z();\n}\n".to_string()
    }

    fn put_cursor(d: &mut Doc, line: usize, col: usize) {
        let p = d.buf.pos(line, col);
        d.ed.click(&mut d.buf, p, false);
    }

    fn caret(d: &Doc) -> (usize, usize) {
        d.buf.line_col(d.ed.head())
    }

    /// Two panes on `docs`: pane 0 shows `a` and is focused, pane 1 shows `b`.
    fn two(docs: &mut [Doc], a: usize, b: usize) -> (usize, Split) {
        let view = docs[b].pane_view();
        let mut sp = Split::new(a, docs[a].pane_view());
        let new = sp.grid.split_right(0);
        sp.panes.insert(new, Pane { doc: b, view });
        (a, sp)
    }

    // ── grid geometry ──

    #[test]
    fn rows_of_up_to_four_panes_tile_the_area() {
        let card = Rect::new(256, 66, 1000, 500);
        let mut g = Grid::single();
        for k in 2..=4 {
            let p = g.split_right(g.panes() - 1);
            assert_eq!(p, k - 1);
            let (r, d) = g.layout(card).unwrap();
            assert_eq!(r.len(), k);
            assert_eq!(d.len(), k - 1);
            assert_eq!(r[0].x, card.x);
            for i in 0..k - 1 {
                assert_eq!(r[i].right(), d[i].rect.x);
                assert_eq!(d[i].rect.w, 1);
                assert_eq!(r[i + 1].x, d[i].rect.right());
                assert_eq!((r[i].y, r[i].h), (card.y, card.h));
            }
            assert_eq!(r[k - 1].right(), card.right());
        }
        assert!(!g.can_split_right(), "four panes is the limit");
    }

    #[test]
    fn splitting_below_makes_a_grid_of_up_to_three_by_two() {
        let card = Rect::new(0, 0, 1200, 600);
        let mut g = Grid::single();
        assert!(g.can_split_below(0));
        let p = g.split_below(0);
        assert_eq!(p, 1);
        assert!(!g.can_split_below(0) && !g.can_split_below(1), "a column holds two panes at most");
        assert!(g.can_split_right());
        let n = g.split_right(1);
        assert_eq!(n, 2, "the new column comes after the stacked one");
        let m = g.split_below(2);
        assert_eq!(m, 3);
        assert_eq!(g.panes(), 4);
        let (r, d) = g.layout(card).unwrap();
        assert_eq!(r.len(), 4);
        // left column stacked: top over bottom, same x and width, 1px bar between
        assert_eq!((r[0].x, r[0].w), (r[1].x, r[1].w));
        assert_eq!(r[1].y, r[0].bottom() + 1);
        assert_eq!(r[1].bottom(), card.bottom());
        assert!(d.iter().any(|d| d.kind == DivKind::Row(0)) && d.iter().any(|d| d.kind == DivKind::Row(1)));
        assert!(d.iter().any(|d| d.kind == DivKind::Col(0)));
        // a third column of two: the 3x2 grid, six panes, nothing more
        assert!(g.can_split_right(), "stacked grids may have three columns");
        let c = g.split_right(3);
        assert_eq!(c, 4);
        assert_eq!(g.cols.len(), 3);
        assert!(!g.can_split_right(), "no fourth column once anything is stacked");
        let last = g.split_below(4);
        assert_eq!((last, g.panes()), (5, 6));
        assert!(!g.can_split_below(0) && !g.can_split_right(), "six panes is the limit");
        let (r, d) = g.layout(card).unwrap();
        assert_eq!(r.len(), 6);
        assert_eq!(d.iter().filter(|d| matches!(d.kind, DivKind::Col(_))).count(), 2);
        assert_eq!(d.iter().filter(|d| matches!(d.kind, DivKind::Row(_))).count(), 3);
        assert_eq!(r[5].right(), card.right());
        assert_eq!(r[5].bottom(), card.bottom());
        // stacking is also possible under a row of three, but not under a row of four
        let mut g = Grid::single();
        g.split_right(0);
        g.split_right(1);
        assert_eq!(g.cols.len(), 3);
        assert!(g.can_split_below(0), "stacking under a row of three gives 3x2");
        g.split_right(2);
        assert_eq!(g.cols.len(), 4);
        assert!(!g.can_split_below(0), "a row of four cannot be stacked");
        // a plain row still stops at four panes
        assert!(!g.can_split_right());
        let mut g = Grid::single();
        g.split_below(0);
        g.split_right(0);
        assert!(g.can_split_right(), "a second column leaves room for a third");
    }

    #[test]
    fn the_grid_round_trips_through_the_session() {
        let mut g = Grid::single();
        g.split_below(0);
        g.split_right(1);
        g.split_right(2);
        g.split_below(2);
        g.cols[0].vratio = 0.3;
        g.widths = vec![0.5, 0.2, 0.3];
        let back = Grid::from_json(&g.to_json()).unwrap();
        assert_eq!(back, g);
        // junk is refused
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"cols": [], "widths": []}),
            serde_json::json!({"cols": [{"n": 3, "v": 0.5}], "widths": [1.0]}),
            serde_json::json!({"cols": [{"n": 1, "v": 0.5}], "widths": [1.0, 2.0]}),
            serde_json::json!({"cols": [{"n": 1, "v": 0.5}, {"n": 1, "v": 0.5}], "widths": [0.0, 1.0]}),
            serde_json::json!({"cols": [{"n": 2, "v": 0.5}, {"n": 2, "v": 0.5}, {"n": 2, "v": 0.5}, {"n": 2, "v": 0.5}], "widths": [1.0, 1.0, 1.0, 1.0]}),
        ] {
            assert!(Grid::from_json(&bad).is_none(), "{bad}");
        }
        // widths are renormalized and ratios kept inside the divider limits
        let g = Grid::from_json(&serde_json::json!({"cols": [{"n": 2, "v": 7.0}, {"n": 1}], "widths": [3.0, 1.0]}))
            .unwrap();
        assert!((g.widths[0] - 0.75).abs() < 1e-6 && (g.widths.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(g.cols[0].vratio <= 0.95 && g.cols[1].vratio == 0.5);
    }

    #[test]
    fn too_small_areas_do_not_split() {
        let mut g = Grid::single();
        g.split_right(0);
        assert!(g.layout(Rect::new(0, 0, 359, 300)).is_none());
        assert!(g.layout(Rect::new(0, 0, 360, 300)).is_some());
        g.split_right(1);
        g.split_right(2);
        assert!(g.layout(Rect::new(0, 0, 600, 300)).is_none(), "four columns need 4 x 160 plus bars");
        assert!(g.layout(Rect::new(0, 0, 643, 300)).is_some());
        let mut g = Grid::single();
        g.split_below(0);
        assert!(g.layout(Rect::new(0, 0, 500, 160)).is_none());
        assert!(g.layout(Rect::new(0, 0, 500, 161)).is_some());
    }

    #[test]
    fn dragging_dividers_resizes_neighbors_within_minimums() {
        let card = Rect::new(100, 50, 1001, 600);
        let mut g = Grid::single();
        g.split_right(0);
        g.split_right(1);
        let (_, d) = g.layout(card).unwrap();
        let first = d.iter().find(|d| d.kind == DivKind::Col(0)).unwrap().rect.x;
        let second = d.iter().find(|d| d.kind == DivKind::Col(1)).unwrap().rect.x;
        // drag the first bar to x = 300: the divider lands there (within a pixel), the second does not move
        g.drag(card, DivKind::Col(0), 300.0);
        let (r, d) = g.layout(card).unwrap();
        let x0 = d.iter().find(|d| d.kind == DivKind::Col(0)).unwrap().rect.x;
        let x1 = d.iter().find(|d| d.kind == DivKind::Col(1)).unwrap().rect.x;
        assert!((x0 - 300).abs() <= 1, "{first} -> {x0}");
        assert!((x1 - second).abs() <= 1);
        assert!(r.iter().all(|r| r.w >= MIN_PANE));
        // far past its neighbor: clamped to the minimum width
        g.drag(card, DivKind::Col(0), 5000.0);
        let (r, _) = g.layout(card).unwrap();
        assert_eq!(r[1].w, MIN_PANE);
        g.drag(card, DivKind::Col(0), -50.0);
        let (r, _) = g.layout(card).unwrap();
        assert_eq!(r[0].w, MIN_PANE);
        // stacked: the bar moves to y, within the minimum heights
        let mut g = Grid::single();
        g.split_below(0);
        g.drag(card, DivKind::Row(0), 200.0);
        let (r, _) = g.layout(card).unwrap();
        assert!((r[0].h - 150).abs() <= 1);
        g.drag(card, DivKind::Row(0), 10_000.0);
        let (r, _) = g.layout(card).unwrap();
        assert_eq!(r[1].h, MIN_PANE_H);
    }

    #[test]
    fn removing_panes_gives_the_space_to_a_neighbor() {
        let card = Rect::new(0, 0, 901, 400);
        let mut g = Grid::single();
        g.split_right(0);
        g.split_right(1);
        g.remove(1); // middle column
        assert_eq!(g.panes(), 2);
        let (r, _) = g.layout(card).unwrap();
        assert_eq!(r[1].right(), card.right());
        let mut g = Grid::single();
        g.split_below(0);
        g.remove(0);
        assert_eq!((g.panes(), g.cols.len()), (1, 1));
        assert_eq!(g.locate(0), (0, 0));
    }

    #[test]
    fn replacement_tab_prefers_the_next_one() {
        assert_eq!(pick_replacement(&[3, 1, 2], 1), Some(2));
        assert_eq!(pick_replacement(&[3, 1, 2], 2), Some(1));
        assert_eq!(pick_replacement(&[5], 5), None);
    }

    // ── per-pane state ──

    #[test]
    fn two_documents_keep_their_own_scroll_and_cursor() {
        let mut docs = vec![doc(&lines(200)), doc(&lines(300))];
        docs[0].top = 10.0;
        docs[0].target = 10.0;
        put_cursor(&mut docs[0], 12, 2);
        docs[1].top = 50.0;
        docs[1].target = 50.0;
        put_cursor(&mut docs[1], 60, 1);
        let (mut active, mut sp) = two(&mut docs, 0, 1);
        focus_to(&mut docs, &mut active, &mut sp, 1);
        assert_eq!((active, sp.focus), (1, 1));
        assert_eq!(docs[1].top, 50.0);
        assert_eq!(caret(&docs[1]), (60, 1));
        assert_eq!(sp.panes[0].view.top, 10.0);
        docs[1].scroll_by(5.0);
        assert_eq!(sp.panes[0].view.top, 10.0, "the other pane's stash is untouched");
        focus_to(&mut docs, &mut active, &mut sp, 0);
        assert_eq!((active, sp.focus), (0, 0));
        assert_eq!(docs[0].top, 10.0);
        assert_eq!(caret(&docs[0]), (12, 2));
        assert_eq!(sp.panes[1].view.top, 55.0);
    }

    #[test]
    fn same_document_twice_has_independent_scroll_cursor_folds_and_wrap() {
        let mut docs = vec![doc(&code())];
        put_cursor(&mut docs[0], 3, 2);
        let (mut active, mut sp) = two(&mut docs, 0, 0);
        // pane 0 (focused): fold fn b, wrap on, cursor at line 6
        docs[0].fold_at_cursor();
        docs[0].set_word_wrap(true);
        put_cursor(&mut docs[0], 6, 1);
        docs[0].view_prepare(40);
        assert_eq!(docs[0].view.folds.regions(), &[(3, 4)]);
        focus_to(&mut docs, &mut active, &mut sp, 1);
        // pane 1 has no folds, no wrap and its own cursor
        assert!(docs[0].view.folds.is_empty());
        assert!(!docs[0].view.wrap);
        assert_eq!(caret(&docs[0]), (3, 2));
        // and pane 0's state is stashed untouched
        assert_eq!(sp.panes[0].view.view.folds.regions(), &[(3, 4)]);
        assert!(sp.panes[0].view.view.wrap);
        // fold something else in pane 1: pane 0 is unaffected
        put_cursor(&mut docs[0], 6, 0);
        docs[0].fold_at_cursor();
        assert_eq!(docs[0].view.folds.regions(), &[(6, 7)]);
        assert_eq!(sp.panes[0].view.view.folds.regions(), &[(3, 4)]);
        focus_to(&mut docs, &mut active, &mut sp, 0);
        assert_eq!(docs[0].view.folds.regions(), &[(3, 4)]);
        assert!(docs[0].view.wrap);
        assert_eq!(caret(&docs[0]), (6, 1));
        assert_eq!(sp.panes[1].view.view.folds.regions(), &[(6, 7)]);
    }

    #[test]
    fn an_edit_in_one_pane_moves_the_other_panes_cursor_and_folds() {
        let mut docs = vec![doc(&code())];
        // pane 1 (stashed): folds fn c (lines 6..7), cursor on line 7 col 4... on the header, selection on line 4
        put_cursor(&mut docs[0], 6, 0);
        docs[0].fold_at_cursor();
        put_cursor(&mut docs[0], 4, 4);
        let (mut active, mut sp) = two(&mut docs, 0, 0);
        // pane 0 (focused): type two new lines at the top
        put_cursor(&mut docs[0], 0, 0);
        {
            let d = &mut docs[0];
            d.ed.paste(&mut d.buf, "// a\n// b\n");
        }
        assert!(docs[0].sync_edits());
        // while stashed, pane 1 has not noticed yet; focusing it catches it up
        focus_to(&mut docs, &mut active, &mut sp, 1);
        assert_eq!(docs[0].view.folds.regions(), &[(8, 9)], "the fold stays on fn c");
        assert_eq!(caret(&docs[0]), (6, 4), "the cursor stays on the same text");
        assert!(docs[0].view.active());
        assert_eq!(docs[0].total_rows(), docs[0].buf.line_count() - 1);
        // the other direction: delete those lines again from pane 1; pane 0 follows
        put_cursor(&mut docs[0], 0, 0);
        {
            let d = &mut docs[0];
            d.ed.delete_lines(&mut d.buf, 0, 1);
        }
        docs[0].sync_edits();
        focus_to(&mut docs, &mut active, &mut sp, 0);
        assert_eq!(docs[0].view.folds.regions(), &[(6, 7)]);
        assert_eq!(caret(&docs[0]), (0, 0));
    }

    #[test]
    fn the_scroll_anchor_of_a_stashed_pane_follows_edits_above_it() {
        let mut docs = vec![doc(&lines(300))];
        docs[0].top = 100.0;
        docs[0].target = 100.0;
        let (mut active, mut sp) = two(&mut docs, 0, 0);
        put_cursor(&mut docs[0], 0, 0);
        {
            let d = &mut docs[0];
            d.ed.paste(&mut d.buf, "a\nb\nc\n");
        }
        docs[0].sync_edits();
        focus_to(&mut docs, &mut active, &mut sp, 1);
        assert_eq!(docs[0].top, 103.0, "same text at the top of the pane");
    }

    #[test]
    fn edits_through_undo_and_line_moves_reach_stashed_panes() {
        let mut docs = vec![doc(&code())];
        put_cursor(&mut docs[0], 6, 0);
        docs[0].fold_at_cursor(); // fold fn c
        put_cursor(&mut docs[0], 0, 0);
        let (mut active, mut sp) = two(&mut docs, 0, 0);
        // pane 0: move line 0 down twice (a swap with `    x();`, then with `}`)
        docs[0].move_lines_cmd(true);
        docs[0].move_lines_cmd(true);
        docs[0].sync_edits();
        focus_to(&mut docs, &mut active, &mut sp, 1);
        assert_eq!(docs[0].view.folds.regions(), &[(6, 7)], "a swap above the fold does not move it");
        focus_to(&mut docs, &mut active, &mut sp, 0);
        // undo in pane 0
        {
            let d = &mut docs[0];
            d.ed.undo(&mut d.buf);
        }
        docs[0].sync_edits();
        focus_to(&mut docs, &mut active, &mut sp, 1);
        assert_eq!(docs[0].view.folds.regions(), &[(6, 7)]);
        assert_eq!(docs[0].buf.line(0), "    x();");
    }

    #[test]
    fn a_reloaded_document_resets_stashed_views() {
        let mut docs = vec![doc(&code())];
        put_cursor(&mut docs[0], 3, 0);
        docs[0].fold_at_cursor();
        let (mut active, mut sp) = two(&mut docs, 0, 0);
        docs[0].buf = Buffer::from_str("short\n");
        focus_to(&mut docs, &mut active, &mut sp, 1);
        assert!(docs[0].view.folds.is_empty());
        assert!(docs[0].ed.sel.head <= docs[0].buf.len_chars());
        assert_eq!(docs[0].top, 0.0);
    }

    #[test]
    fn stale_views_are_clamped_to_the_text() {
        let mut docs = vec![doc(&lines(100))];
        let end = docs[0].buf.len_chars();
        let mut v = docs[0].pane_view();
        v.top = 99.0;
        v.target = 99.0;
        v.sel = Some(Sel { anchor: end, head: end });
        v.extra = vec![Sel::caret(end - 3)];
        docs[0].buf = Buffer::from_str("short\n");
        docs[0].set_pane_view(v);
        let n = docs[0].buf.len_chars();
        assert!(docs[0].ed.sel.head <= n && docs[0].ed.sel.anchor <= n);
        assert!(docs[0].ed.extra.iter().all(|s| s.head <= n));
        assert_eq!(docs[0].top, 0.0);
    }

    #[test]
    fn language_server_folds_reach_every_pane_showing_the_document() {
        // a comment block the indentation cannot see: lines 1..2
        let text = "// a\n// b\n// c\nfn x() {\n    y();\n}\n";
        let mut docs = vec![doc(text), doc(text)];
        let (mut active, mut sp) = two(&mut docs, 0, 0);
        assert!(!docs[0].is_foldable(0), "no ranges yet");
        docs[0].set_lsp_folds(vec![(0, 2)]);
        share_lsp_folds(&docs, &mut sp, 0);
        assert!(docs[0].is_foldable(0) && docs[0].fold_regions().contains(&(0, 2)));
        // the other pane (stashed) has them too, for the same text version
        focus_to(&mut docs, &mut active, &mut sp, 1);
        assert!(docs[0].is_foldable(0), "the second pane sees the language server's range");
        assert!(docs[0].fold_regions().contains(&(0, 2)));
        assert!(
            docs[0].fold_at_cursor() || {
                put_cursor(&mut docs[0], 0, 0);
                docs[0].fold_at_cursor()
            }
        );
        assert_eq!(docs[0].view.folds.regions(), &[(0, 2)]);
        // a pane on another document is left alone
        let mut sp2 = Split::new(1, docs[1].pane_view());
        let n = sp2.grid.split_right(0);
        sp2.panes.insert(n, Pane { doc: 0, view: docs[0].pane_view() });
        sp2.focus = 0;
        share_lsp_folds(&docs, &mut sp2, 1);
        assert_eq!(sp2.panes[1].view.view.lsp_regions, vec![(0, 2)], "untouched: it shows another document");
    }

    #[test]
    fn logs_are_trimmed_once_every_pane_has_caught_up() {
        let mut docs = vec![doc(&lines(10))];
        let (mut active, mut sp) = two(&mut docs, 0, 0);
        put_cursor(&mut docs[0], 0, 0);
        {
            let d = &mut docs[0];
            d.ed.paste(&mut d.buf, "x");
        }
        docs[0].sync_edits();
        assert!(docs[0].buf.log_seq() > 0);
        trim_logs(&mut docs, &sp);
        assert!(docs[0].buf.edits_since(sp.panes[0].view.seen).is_some());
        focus_to(&mut docs, &mut active, &mut sp, 1);
        trim_logs(&mut docs, &sp);
        assert_eq!(docs[0].buf.edits_since(docs[0].buf.log_seq()).map(|r| r.len()), Some(0));
        // no stashed pane on a document: logging stops
        let one = Split::new(0, docs[0].pane_view());
        trim_logs(&mut docs, &one);
        assert!(docs[0].buf.edits_since(0).is_none(), "the log is gone");
    }

    // ── panes and documents ──

    #[test]
    fn removing_documents_closes_or_keeps_panes() {
        let mk = |docs: &[usize], focus: usize| {
            let mut sp = Split::new(docs[0], PaneView::default());
            for (i, &x) in docs.iter().enumerate().skip(1) {
                let p = sp.grid.split_right(i - 1);
                sp.panes.insert(p, Pane { doc: x, view: PaneView::default() });
            }
            sp.focus = focus;
            Some(Box::new(sp))
        };
        // panes on docs 0, 3, 1 (focus on the first): removing doc 1 closes the third pane, doc 3 becomes 2
        let mut s = mk(&[0, 3, 1], 0);
        after_remove(&mut s, 1, 5);
        let sp = s.as_ref().unwrap();
        assert_eq!(sp.panes.len(), 2);
        assert_eq!(sp.panes[1].doc, 2);
        assert_eq!(sp.grid.panes(), 2);
        // the document of the focused pane is not touched here (App adjusts `active`)
        let mut s = mk(&[0, 3], 0);
        after_remove(&mut s, 0, 3);
        assert!(s.is_some());
        // the only other pane goes away: no split left
        let mut s = mk(&[0, 3], 0);
        after_remove(&mut s, 3, 3);
        assert!(s.is_none());
        // closing a pane before the focused one keeps the focus on the same pane
        let mut s = mk(&[4, 5, 6], 2);
        after_remove(&mut s, 4, 3);
        let sp = s.as_ref().unwrap();
        assert_eq!((sp.panes.len(), sp.focus), (2, 1));
        // nothing left at all
        let mut s = mk(&[0, 3], 0);
        after_remove(&mut s, 0, 0);
        assert!(s.is_none());
    }

    #[test]
    fn the_diff_pane_follows_pane_removals() {
        let mut sp = Split::new(0, PaneView::default());
        for i in 1..3 {
            let p = sp.grid.split_right(i - 1);
            sp.panes.insert(p, Pane { doc: i, view: PaneView::default() });
        }
        sp.focus = 1;
        sp.diff_at = Some(2);
        remove_pane(&mut sp, 0);
        assert_eq!((sp.focus, sp.diff_at), (0, Some(1)), "indexes shift down with the removed pane");
        remove_pane(&mut sp, 1);
        assert_eq!(sp.diff_at, None, "the pane under the diff is gone");
    }

    #[test]
    fn four_panes_cycle_focus_through_their_own_states() {
        let mut docs = vec![doc(&lines(100)), doc(&lines(100)), doc(&lines(100)), doc(&lines(100))];
        for (i, d) in docs.iter_mut().enumerate() {
            put_cursor(d, 10 + i, 0);
        }
        let mut sp = Split::new(0, docs[0].pane_view());
        for i in 1..4 {
            let p = sp.grid.split_right(i - 1);
            sp.panes.insert(p, Pane { doc: i, view: docs[i].pane_view() });
        }
        let mut active = 0;
        for k in [2usize, 3, 1, 0, 3] {
            focus_to(&mut docs, &mut active, &mut sp, k);
            assert_eq!((active, sp.focus), (k, k));
            assert_eq!(caret(&docs[k]), (10 + k, 0));
        }
    }
}
