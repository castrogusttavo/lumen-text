//! Split editor: two panes side by side (two documents, or the same one twice).
//!
//! Model: `App::active` is always the document of the *focused* pane, so everything that anchors on
//! it (find, completion, hover, LSP requests, status bar, breadcrumbs, `layout.editor`) follows the
//! focus for free. `Split::other` is the document of the other pane. The scroll/cursor of the
//! unfocused pane live in `Split::view` (a few words, so the second pane costs nothing when the
//! split is closed: `App::split` is `None`). The focused pane's scroll/cursor live in the document,
//! exactly as without a split. Focusing the other pane swaps the two (works for the same document
//! too, which is how each pane gets its own scroll and cursor).
//!
//! Conflicts: the Markdown reader and the diff view take the whole editor area, so while either is
//! open the split closes (`split_layout`).

use crate::{
    app::{App, Focus},
    buffer::Sel,
    doc::{Doc, DrawOpts},
    ui::{Canvas, Rect},
};
use winit::event::MouseScrollDelta;

/// Narrowest editor area that still splits, and the narrowest pane.
const MIN_AREA: i32 = 360;
const MIN_PANE: i32 = 160;
/// Pixels around the 1px divider that grab the mouse.
const GRAB: i32 = 3;

/// Scroll and cursor of one pane.
#[derive(Clone, Debug, Default)]
pub struct PaneView {
    pub top: f64,
    pub target: f64,
    pub left: f64,
    pub left_target: f64,
    pub sel: Option<Sel>,
    pub extra: Vec<Sel>,
    /// Where the pane was last drawn (so a click right after a focus change hits the right cells).
    pub rect: Rect,
}

impl Doc {
    pub fn pane_view(&self) -> PaneView {
        PaneView { top: self.top, target: self.target, left: self.left, left_target: self.left_target, sel: Some(self.ed.sel), extra: self.ed.extra.clone(), rect: self.rect }
    }

    /// Adopt a pane's scroll/cursor, clamped to the current text (it may have changed meanwhile).
    pub fn set_pane_view(&mut self, v: &PaneView) {
        let n = self.buf.len_chars();
        let fix = |s: Sel| Sel { anchor: s.anchor.min(n), head: s.head.min(n) };
        if let Some(s) = v.sel {
            self.ed.sel = fix(s);
        }
        self.ed.extra = v.extra.iter().map(|&s| fix(s)).collect();
        if v.rect.w > 0 {
            self.rect = v.rect;
        }
        let max = self.max_top();
        self.top = v.top.clamp(0.0, max);
        self.target = v.target.clamp(0.0, max);
        self.left = v.left.max(0.0);
        self.left_target = v.left_target.max(0.0);
        // a pane that was still gliding keeps gliding (the scrollers re-adopt these values)
        self.animating = (self.top - self.target).abs() > 1e-9 || (self.left - self.left_target).abs() > 1e-9;
    }

    /// Forget what the last draw said about clickable areas (they belonged to another pane).
    fn forget_hits(&mut self) {
        self.minimap_layout = None;
        self.fold_pills.clear();
        self.lens_hits.clear();
        self.conflict_hits.clear();
    }
}

pub struct Split {
    /// Document shown in the unfocused pane.
    pub other: usize,
    /// Scroll/cursor of the unfocused pane.
    pub view: PaneView,
    /// The focused pane is the right one.
    pub right_focused: bool,
    /// Share of the width given to the left pane.
    pub ratio: f32,
    pub dragging: bool,
}

/// (focused pane, other pane, divider) inside `card`; `None` when there is no room to split.
pub fn pane_rects(card: Rect, ratio: f32, right_focused: bool) -> Option<(Rect, Rect, Rect)> {
    if card.w < MIN_AREA {
        return None;
    }
    let avail = card.w - 1;
    let lw = ((avail as f32 * ratio).round() as i32).clamp(MIN_PANE, avail - MIN_PANE);
    let left = Rect::new(card.x, card.y, lw, card.h);
    let div = Rect::new(card.x + lw, card.y, 1, card.h);
    let right = Rect::new(card.x + lw + 1, card.y, avail - lw, card.h);
    Some(if right_focused { (right, left, div) } else { (left, right, div) })
}

/// Left-pane share that puts the divider under window x.
pub fn ratio_at(card: Rect, x: f64) -> f32 {
    (((x - card.x as f64) / (card.w - 1).max(1) as f64) as f32).clamp(0.1, 0.9)
}

/// Exchange which pane has the focus: `active` takes the other pane's document and view.
pub fn swap_focus(docs: &mut [Doc], active: &mut usize, sp: &mut Split) {
    if sp.other >= docs.len() || *active >= docs.len() {
        return;
    }
    let mine = docs[*active].pane_view();
    let theirs = std::mem::replace(&mut sp.view, mine);
    let new = sp.other;
    docs[new].set_pane_view(&theirs);
    if new == *active {
        docs[new].forget_hits();
    }
    sp.other = *active;
    *active = new;
    sp.right_focused = !sp.right_focused;
}

/// A document was removed from the tab list: keep `other` pointing at the same one, or close the
/// split when that document is the one that went away (or none is left).
pub fn after_remove(split: &mut Option<Box<Split>>, removed: usize, remaining: usize) {
    let Some(sp) = split.as_mut() else { return };
    if remaining == 0 || sp.other == removed {
        *split = None;
    } else if removed < sp.other {
        sp.other -= 1;
    }
}

impl App {
    pub fn split_other_doc(&self) -> Option<usize> {
        self.split.as_ref().map(|s| s.other)
    }

    /// Called by `compute_layout` in place of `md_split`: sets `layout.editor` (the focused pane),
    /// `layout.other` and `layout.divider`.
    pub fn split_layout(&mut self) {
        self.layout.other = Rect::default();
        self.layout.divider = Rect::default();
        self.layout.editor = self.layout.card;
        if self.split.is_some() {
            let conflict = self.diff.is_some() || self.docs.is_empty() || self.docs.get(self.active).is_some_and(|d| d.md.is_some());
            if conflict {
                self.split = None;
            }
        }
        let Some(sp) = self.split.as_deref() else { return self.md_split() };
        self.layout.md = Rect::default();
        if let Some((focused, other, div)) = pane_rects(self.layout.card, sp.ratio, sp.right_focused) {
            self.layout.editor = focused;
            self.layout.other = other;
            self.layout.divider = div;
        }
    }

    /// "Dividir editor à direita": the new right pane starts on the same document and has the focus;
    /// click another tab to put a different document in it.
    pub fn split_right(&mut self) {
        if self.docs.is_empty() {
            return self.say("Abra um arquivo para dividir o editor");
        }
        if self.split.is_some() {
            return self.say("Editor já dividido — Ctrl+1 / Ctrl+2 trocam o foco; \"Fechar painel dividido\" na paleta");
        }
        self.diff = None;
        for d in &mut self.docs {
            d.md = None;
        }
        self.layout.md = Rect::default();
        let view = self.docs[self.active].pane_view();
        self.split = Some(Box::new(Split { other: self.active, view, right_focused: true, ratio: 0.5, dragging: false }));
        self.hover = None;
        self.completion = None;
        self.split_layout();
        self.say("Editor dividido — clique numa aba para escolher o documento do painel focado");
        self.request_redraw();
    }

    /// "Fechar painel dividido": the focused pane's document stays.
    pub fn split_close(&mut self) {
        if self.split.take().is_none() {
            return self.say("O editor não está dividido");
        }
        self.hover = None;
        self.completion = None;
        self.split_layout();
        self.request_redraw();
    }

    fn split_swap_now(&mut self) {
        let Some(mut sp) = self.split.take() else { return };
        swap_focus(&mut self.docs, &mut self.active, &mut sp);
        self.split = Some(sp);
        self.hover = None;
        self.hover_probe = None;
        self.completion = None;
        self.close_armed = None;
        self.focus = Focus::Editor;
        self.split_layout();
        self.request_redraw();
    }

    /// Focus the left (`false`) or right (`true`) pane.
    pub fn focus_pane(&mut self, right: bool) {
        let Some(sp) = self.split.as_ref() else { return self.say("O editor não está dividido (Ctrl+\\ divide)") };
        if sp.right_focused != right {
            self.split_swap_now();
        }
        self.focus = Focus::Editor;
        self.request_redraw();
    }

    pub fn focus_other_pane(&mut self) {
        match self.split.as_ref() {
            Some(sp) => self.focus_pane(!sp.right_focused),
            None => self.say("O editor não está dividido (Ctrl+\\ divide)"),
        }
    }

    /// `close_tab` removed document `idx`.
    pub fn split_doc_removed(&mut self, idx: usize) {
        after_remove(&mut self.split, idx, self.docs.len());
    }

    /// Draw the unfocused pane and the divider. Runs BEFORE the focused pane, so that when both show
    /// the same document the geometry it keeps (rect, minimap, fold markers) is the focused pane's.
    pub fn draw_split_other(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        let r = self.layout.other;
        if r.w <= 0 {
            return false;
        }
        let Some(mut sp) = self.split.take() else { return false };
        let mut busy = false;
        let shared = sp.other == self.active;
        let (find, find_cur) = match (&self.find, shared) {
            (Some(f), true) => (f.matches.clone(), f.current),
            _ => (Vec::new(), None),
        };
        if let Some(d) = self.docs.get_mut(sp.other) {
            d.cell_w = self.tx.cell_w;
            let saved = d.pane_view();
            let reveal = std::mem::replace(&mut d.reveal, false);
            d.set_pane_view(&sp.view);
            busy |= d.draw(cv, &mut self.tx, &self.theme, r, &DrawOpts { dt, find: &find, find_cur, focused: false });
            sp.view = d.pane_view();
            d.set_pane_view(&saved);
            d.reveal = reveal;
        } else {
            cv.fill(r, self.theme.background);
        }
        let dv = self.layout.divider;
        cv.fill(dv, self.theme.chrome.border);
        self.split = Some(sp);
        busy
    }

    /// Mouse press: grabs the divider, or moves the focus when the press is in the other pane (the
    /// press then continues as a normal editor click: returns false).
    pub fn split_mouse_down(&mut self, x: f64, y: f64) -> bool {
        let dv = self.layout.divider;
        let Some(sp) = self.split.as_mut() else { return false };
        if dv.w > 0 && Rect::new(dv.x - GRAB, dv.y, dv.w + 2 * GRAB, dv.h).contains(x, y) {
            sp.dragging = true;
            return true;
        }
        if self.layout.other.w > 0 && self.layout.other.contains(x, y) {
            self.split_swap_now();
        }
        false
    }

    /// Mouse move while dragging the divider.
    pub fn split_drag(&mut self, x: f64) -> bool {
        let card = self.layout.card;
        let Some(sp) = self.split.as_mut().filter(|s| s.dragging) else { return false };
        sp.ratio = ratio_at(card, x);
        self.split_layout();
        true
    }

    pub fn split_release(&mut self) {
        if let Some(sp) = self.split.as_mut() {
            sp.dragging = false;
        }
    }

    /// Wheel over the unfocused pane scrolls it without moving the focus.
    pub fn split_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        if self.layout.other.w <= 0 || !self.layout.other.contains(x, y) {
            return false;
        }
        let w = self.wheel_in(delta);
        let Some(mut sp) = self.split.take() else { return false };
        if let Some(d) = self.docs.get_mut(sp.other) {
            let saved = d.pane_view();
            d.set_pane_view(&sp.view);
            d.wheel(w, crate::scroller::now());
            sp.view = d.pane_view();
            d.set_pane_view(&saved);
        }
        self.split = Some(sp);
        true
    }
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

    fn put_cursor(d: &mut Doc, line: usize, col: usize) {
        let p = d.buf.pos(line, col);
        d.ed.click(&mut d.buf, p, false);
    }

    fn mk_split(other: usize, d: &Doc) -> Split {
        Split { other, view: d.pane_view(), right_focused: true, ratio: 0.5, dragging: false }
    }

    #[test]
    fn rects_tile_the_area() {
        let card = Rect::new(256, 66, 944, 500);
        for rf in [false, true] {
            let (f, o, div) = pane_rects(card, 0.5, rf).unwrap();
            let (l, r) = if rf { (o, f) } else { (f, o) };
            assert_eq!(l.x, card.x);
            assert_eq!(l.right(), div.x);
            assert_eq!(div.w, 1);
            assert_eq!(r.x, div.right());
            assert_eq!(r.right(), card.right());
            assert_eq!((l.h, r.h, div.h), (500, 500, 500));
            assert!((l.w - r.w).abs() <= 1);
        }
    }

    #[test]
    fn rects_respect_minimum_and_narrow_areas() {
        let card = Rect::new(0, 0, 800, 300);
        let (f, o, _) = pane_rects(card, 0.01, false).unwrap();
        assert_eq!(f.w, MIN_PANE);
        assert_eq!(o.w, 800 - 1 - MIN_PANE);
        let (f, o, _) = pane_rects(card, 0.99, false).unwrap();
        assert_eq!(o.w, MIN_PANE);
        assert_eq!(f.w + o.w + 1, 800);
        assert!(pane_rects(Rect::new(0, 0, 359, 300), 0.5, false).is_none());
    }

    #[test]
    fn divider_ratio_follows_the_mouse() {
        let card = Rect::new(256, 0, 1001, 100);
        assert!((ratio_at(card, 256.0 + 500.0) - 0.5).abs() < 0.001);
        assert_eq!(ratio_at(card, 0.0), 0.1);
        assert_eq!(ratio_at(card, 5000.0), 0.9);
        // dragging to x puts the divider at x (within a pixel)
        let (_, _, div) = pane_rects(card, ratio_at(card, 700.0), false).unwrap();
        assert!((div.x - 700).abs() <= 1);
    }

    #[test]
    fn two_documents_keep_their_own_scroll_and_cursor() {
        let mut docs = vec![doc(&lines(200)), doc(&lines(300))];
        docs[0].top = 10.0;
        docs[0].target = 10.0;
        put_cursor(&mut docs[0], 12, 2);
        docs[1].top = 50.0;
        docs[1].target = 50.0;
        put_cursor(&mut docs[1], 60, 1);
        let mut active = 0;
        let mut sp = mk_split(1, &docs[0]);
        sp.view = docs[1].pane_view();
        sp.right_focused = false;
        swap_focus(&mut docs, &mut active, &mut sp);
        assert_eq!((active, sp.other, sp.right_focused), (1, 0, true));
        assert_eq!(docs[1].top, 50.0);
        assert_eq!(docs[1].buf.line_col(docs[1].ed.head()), (60, 1));
        assert_eq!(sp.view.top, 10.0);
        // scroll the focused one: the other pane's stash is untouched
        docs[1].scroll_by(5.0);
        assert_eq!(sp.view.top, 10.0);
        swap_focus(&mut docs, &mut active, &mut sp);
        assert_eq!((active, sp.other, sp.right_focused), (0, 1, false));
        assert_eq!(docs[0].top, 10.0);
        assert_eq!(docs[0].buf.line_col(docs[0].ed.head()), (12, 2));
        assert_eq!(sp.view.top, 55.0);
    }

    #[test]
    fn same_document_twice_has_independent_scroll_and_cursor() {
        let mut docs = vec![doc(&lines(400))];
        put_cursor(&mut docs[0], 5, 3);
        let mut active = 0;
        let mut sp = mk_split(0, &docs[0]); // both panes start identical
        // the focused pane (right) goes far down
        docs[0].scroll_by(120.0);
        put_cursor(&mut docs[0], 150, 4);
        assert_eq!(sp.view.top, 0.0);
        swap_focus(&mut docs, &mut active, &mut sp);
        // now the document shows the left pane's state
        assert_eq!(docs[0].top, 0.0);
        assert_eq!(docs[0].buf.line_col(docs[0].ed.head()), (5, 3));
        // and the stash holds the right pane's
        assert_eq!(sp.view.top, 120.0);
        assert_eq!(docs[0].buf.line_col(sp.view.sel.unwrap().head), (150, 4));
        // moving the cursor in the left pane then going back keeps both
        put_cursor(&mut docs[0], 9, 0);
        swap_focus(&mut docs, &mut active, &mut sp);
        assert_eq!(docs[0].top, 120.0);
        assert_eq!(docs[0].buf.line_col(docs[0].ed.head()), (150, 4));
        assert_eq!(docs[0].buf.line_col(sp.view.sel.unwrap().head), (9, 0));
    }

    #[test]
    fn stale_views_are_clamped_to_the_text() {
        let mut docs = vec![doc(&lines(100))];
        let end = docs[0].buf.len_chars();
        let v = PaneView { top: 99.0, target: 99.0, left: 0.0, left_target: 0.0, sel: Some(Sel { anchor: end, head: end }), extra: vec![Sel::caret(end - 3)], rect: Rect::default() };
        docs[0].buf = Buffer::from_str("short\n");
        docs[0].set_pane_view(&v);
        let n = docs[0].buf.len_chars();
        assert!(docs[0].ed.sel.head <= n && docs[0].ed.sel.anchor <= n);
        assert!(docs[0].ed.extra.iter().all(|s| s.head <= n));
        assert_eq!(docs[0].top, 0.0);
    }

    #[test]
    fn removing_documents_keeps_or_closes_the_split() {
        let d = doc("x");
        let mut s = Some(Box::new(mk_split(3, &d)));
        after_remove(&mut s, 1, 5);
        assert_eq!(s.as_ref().unwrap().other, 2);
        after_remove(&mut s, 4, 4);
        assert_eq!(s.as_ref().unwrap().other, 2);
        after_remove(&mut s, 2, 3); // the document of the other pane closed
        assert!(s.is_none());
        let mut s = Some(Box::new(mk_split(0, &d)));
        after_remove(&mut s, 0, 0);
        assert!(s.is_none());
    }
}
