//! Dragging a terminal's tab: along the strip to reorder, or onto the left / right half of the
//! panel body to show it there, side by side with the other one.
//!
//! A press on a tab activates it at once and arms the drag; after `DRAG_START` px the tab lifts
//! (a floating copy follows the pointer). Over the strip an accent bar shows where it would land;
//! over the body the half it would take is tinted. Releasing drops it; Esc, or releasing anywhere
//! else, puts everything back. At most two terminals are on screen (see `app_terms`).

use crate::{
    app::App,
    app_terms::TermHit,
    ui::{Canvas, Rect},
};
use winit::keyboard::{Key, NamedKey};

/// Pointer travel (px) that turns a press on a tab into a drag.
const DRAG_START: f64 = 5.0;

/// A press on a terminal tab that may become a drag.
#[derive(Clone, Copy, Debug)]
pub struct TermDrag {
    pub id: u32,
    pub press: (f64, f64),
    /// Pointer offset from the tab's left edge at the press.
    pub grab: f64,
    pub active: bool,
}

/// Which side of the body a terminal is dropped on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// Where the dragged terminal would land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropAt {
    /// Index among the other tabs (the strip without the dragged one).
    Reorder(usize),
    Side(Side),
}

/// The panes after terminal `id` is dropped on `side`: `panes` are the ones on screen, left to
/// right. Dropping on the pane that already shows it is a no-op.
pub fn dropped_panes<T: Copy + PartialEq>(panes: &[T], id: T, side: Side) -> Vec<T> {
    match (panes, side) {
        ([], _) => vec![id],
        ([one], _) if *one == id => vec![id],
        ([one], Side::Left) => vec![id, *one],
        ([one], Side::Right) => vec![*one, id],
        ([a, b], s) => {
            let (slot, other) = if s == Side::Left { (*a, *b) } else { (*b, *a) };
            if id == slot {
                vec![*a, *b]
            } else if id == other {
                vec![*b, *a] // swap the two
            } else if s == Side::Left {
                vec![id, *b]
            } else {
                vec![*a, id]
            }
        }
        (_, _) => panes.to_vec(),
    }
}

/// Moves the item at `from` so it sits at index `to` of the list without it.
pub fn move_to<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from >= v.len() {
        return;
    }
    let item = v.remove(from);
    v.insert(to.min(v.len()), item);
}

impl App {
    /// Left press on terminal tab `id` (already shown): arm a drag.
    pub fn term_strip_drag_press(&mut self, id: u32, x: f64, y: f64) {
        let left = self.terms.hits.iter().find(|(_, h)| *h == TermHit::Tab(id)).map_or(x, |(r, _)| r.x as f64);
        self.terms.drag = Some(TermDrag { id, press: (x, y), grab: x - left, active: false });
    }

    /// A terminal tab is being dragged (past the threshold).
    pub fn term_strip_dragging(&self) -> bool {
        self.terms.drag.is_some_and(|d| d.active)
    }

    /// Pointer moved with the button down. True when a terminal tab drag consumed the move.
    pub fn term_strip_drag_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let Some(d) = self.terms.drag.as_mut() else { return false };
        if !d.active {
            if (x - d.press.0).hypot(y - d.press.1) < DRAG_START {
                return false;
            }
            d.active = true;
            self.dismiss_tooltip();
        }
        self.request_redraw();
        true
    }

    /// The panel body (below the header) of the last draw.
    fn term_body(&self) -> Rect {
        let r = self.layout.panel;
        let top = r.y + crate::app_panel::PANEL_HEADER + 1;
        Rect::new(r.x, top, r.w, (r.bottom() - top).max(0))
    }

    /// Where the dragged terminal would land now; `None` away from both strip and body.
    pub fn term_drop(&self) -> Option<DropAt> {
        let d = self.terms.drag.filter(|d| d.active)?;
        let (x, y) = self.mouse;
        let r = self.layout.panel;
        if !self.panel.open || !r.contains(x, y) {
            return None;
        }
        if y < (r.y + crate::app_panel::PANEL_HEADER) as f64 {
            // The floating tab's center against the other tabs' centers.
            let w = self.terms.hits.iter().find(|(_, h)| *h == TermHit::Tab(d.id)).map_or(0, |(r, _)| r.w);
            let cx = x - d.grab + w as f64 / 2.0;
            let k = self
                .terms
                .hits
                .iter()
                .filter_map(|(r, h)| match h {
                    TermHit::Tab(id) if *id != d.id => Some(r),
                    _ => None,
                })
                .filter(|r| (r.x as f64 + r.w as f64 / 2.0) < cx)
                .count();
            return Some(DropAt::Reorder(k));
        }
        let body = self.term_body();
        if !body.contains(x, y) {
            return None;
        }
        Some(DropAt::Side(if x < (body.x + body.w / 2) as f64 { Side::Left } else { Side::Right }))
    }

    /// Button released: drop the dragged terminal. True when a drag (not a plain click) ended.
    pub fn term_strip_drag_release(&mut self) -> bool {
        let drop = self.term_drop();
        let Some(d) = self.terms.drag.take() else { return false };
        if !d.active {
            return false;
        }
        match drop {
            Some(DropAt::Reorder(k)) => {
                if let Some(from) = self.terms.tabs.iter().position(|t| t.id == d.id) {
                    move_to(&mut self.terms.tabs, from, k);
                }
            }
            Some(DropAt::Side(side)) => {
                let panes = dropped_panes(&self.terms.panes, d.id, side);
                self.terms.panes = panes;
                self.terms.active = d.id;
            }
            None => {}
        }
        self.request_redraw();
        true
    }

    /// Esc while dragging a terminal tab puts it back. True when the key was used.
    pub fn term_strip_drag_key(&mut self, key: &Key) -> bool {
        if self.term_strip_dragging() && matches!(key, Key::Named(NamedKey::Escape)) {
            self.terms.drag = None;
            self.request_redraw();
            return true;
        }
        false
    }

    /// DropAt marker, tinted half and floating tab, drawn on top of the panel.
    pub fn draw_term_strip_drag(&mut self, cv: &mut Canvas) {
        let Some(d) = self.terms.drag.filter(|d| d.active) else { return };
        let Some(i) = self.terms.tabs.iter().position(|t| t.id == d.id) else { return };
        let title = self.terms.tabs[i].title(i + 1);
        let c = self.theme.chrome;
        let r = self.layout.panel;
        let head = Rect::new(r.x, r.y, r.w, crate::app_panel::PANEL_HEADER);
        match self.term_drop() {
            Some(DropAt::Reorder(k)) => {
                let others: Vec<Rect> = self
                    .terms
                    .hits
                    .iter()
                    .filter_map(|(r, h)| match h {
                        TermHit::Tab(id) if *id != d.id => Some(*r),
                        _ => None,
                    })
                    .collect();
                let mx = match (others.get(k), k.checked_sub(1).and_then(|j| others.get(j))) {
                    (Some(n), _) => Some(n.x - 2),
                    (None, Some(p)) => Some(p.right() + 1),
                    _ => None,
                };
                if let Some(mx) = mx {
                    cv.fill_round_clip(Rect::new(mx, head.y + 7, 3, head.h - 14), 1, c.accent, head);
                }
            }
            Some(DropAt::Side(side)) => {
                let b = self.term_body();
                let half = b.w / 2;
                let z = if side == Side::Left {
                    Rect::new(b.x + 4, b.y + 4, half - 6, b.h - 8)
                } else {
                    Rect::new(b.x + half + 2, b.y + 4, b.w - half - 6, b.h - 8)
                };
                cv.card(z, 8, c.accent.with_alpha(0x26), c.accent.with_alpha(0xa0));
                let label = if side == Side::Left { "Show on the left" } else { "Show on the right" };
                let lw = self.tx.ui_width(label, true);
                self.tx.draw_ui(cv, z.x + (z.w - lw) / 2, z.y + z.h / 2 - 9, label, c.tab_active_fg, z, true);
            }
            None => {}
        }
        // Floating tab, lifted with a soft shadow.
        let w = (self.tx.ui_width(&title, true) + 48).min(220);
        let fx = ((self.mouse.0 - d.grab).round() as i32).clamp(r.x, (r.right() - w).max(r.x));
        let fr = Rect::new(fx, r.y + 4, w, crate::app_panel::PANEL_HEADER - 7);
        for i in 1..=4 {
            let g = Rect::new(fr.x - i, fr.y - i + 3, fr.w + 2 * i, fr.h + 2 * i);
            cv.fill_round(g, 8 + i, crate::theme::Rgba(0, 0, 0, 10));
        }
        cv.card(fr, 8, c.tab_active_bg, c.border);
        let fg = c.tab_active_fg;
        self.tx.icon(cv, "ui/terminal", fr.x + 8, fr.y + (fr.h - 14) / 2, 14, fg, fr);
        let text = crate::ui::fit_ellipsis(&mut self.tx, &title, w - 36, true);
        self.tx.draw_ui(cv, fr.x + 28, fr.y + (fr.h - 18) / 2 + 1, &text, fg, fr, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_on_a_side_shows_the_terminal_there() {
        assert_eq!(dropped_panes(&[1], 2, Side::Right), vec![1, 2]);
        assert_eq!(dropped_panes(&[1], 2, Side::Left), vec![2, 1]);
        assert_eq!(dropped_panes(&[1], 1, Side::Left), vec![1], "already alone there");
        assert_eq!(dropped_panes(&[1, 2], 3, Side::Left), vec![3, 2], "replaces the left one");
        assert_eq!(dropped_panes(&[1, 2], 3, Side::Right), vec![1, 3], "replaces the right one");
        assert_eq!(dropped_panes(&[1, 2], 2, Side::Left), vec![2, 1], "swaps the two");
        assert_eq!(dropped_panes(&[1, 2], 1, Side::Left), vec![1, 2], "stays");
    }

    #[test]
    fn reordering_moves_one_item() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        move_to(&mut v, 0, 2);
        assert_eq!(v, vec!['b', 'c', 'a', 'd']);
        move_to(&mut v, 3, 0);
        assert_eq!(v, vec!['d', 'b', 'c', 'a']);
        move_to(&mut v, 1, 1);
        assert_eq!(v, vec!['d', 'b', 'c', 'a']);
    }
}
