//! Reordering tabs by dragging them along the tab strip.
//!
//! A press on a tab activates it at once and arms a drag; once the pointer has travelled
//! `DRAG_START` px the tab lifts: a floating copy follows the pointer, its slot in the strip stays as
//! a faint placeholder, an accent marker shows where it will land, and the strip scrolls when the
//! pointer nears either end. Releasing drops it there (`move_tab`); Esc, or releasing away from the
//! strip (the marker disappears there), cancels. Pinned tabs move among the pinned ones and the
//! others among the others.
//!
//! The order of `App::docs` is the tab order (with pinned tabs shown first, see `tab_order`), so a
//! move is a remove + insert in `docs`, and every document index kept elsewhere (the active tab,
//! split panes, ...) goes through the one mapping `remap`.

use crate::{
    app::App,
    app_ctxmenu::pinned_first,
    ui::{Canvas, Rect},
};
use winit::keyboard::{Key, NamedKey};

/// Pointer travel (px) that turns a press on a tab into a drag.
const DRAG_START: f64 = 5.0;
/// Releasing farther than this above or below the tab bar cancels the drag.
const DROP_BAND: i32 = 48;
/// Auto-scroll zone at each end of the strip (px) and top speed (px/s).
const EDGE: f64 = 36.0;
const EDGE_SPEED: f64 = 900.0;

/// A press on a tab that may become a drag.
#[derive(Clone, Copy, Debug)]
pub struct TabDrag {
    pub doc: usize,
    pub press: (f64, f64),
    /// Pointer offset from the tab's left edge at the press (the floating tab keeps it).
    pub grab: f64,
    /// Past the threshold: the tab is being dragged.
    pub active: bool,
}

/// Tab strip state that the drag needs (lives in `InputState`).
#[derive(Default)]
pub struct TabStrip {
    pub drag: Option<TabDrag>,
    /// Every tab in display order from the last draw: (doc index, x in strip coordinates, width).
    pub geo: Vec<(usize, i32, i32)>,
}

/// Where document index `i` ends up after the document at `from` moved to `to` (remove + insert).
pub fn remap(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if (from + 1..=to).contains(&i) {
        i - 1 // was between the old and the new place: slides left
    } else if (to..from).contains(&i) {
        i + 1 // slides right
    } else {
        i
    }
}

/// Drop of tab `from` at insertion index `k` among the *other* tabs in display order. Returns `k`
/// clamped to `from`'s group (pinned or not) and the `to` for `move_tab`, or None when the tab
/// would stay where it is.
pub fn drop_target(pinned: &[bool], from: usize, k: usize) -> (usize, Option<usize>) {
    if from >= pinned.len() {
        return (k, None);
    }
    let order = pinned_first(pinned);
    let p = order.iter().position(|&d| d == from).unwrap_or(0);
    let rest: Vec<usize> = order.into_iter().filter(|&d| d != from).collect();
    let np = rest.iter().filter(|&&d| pinned[d]).count();
    let (lo, hi) = if pinned[from] { (0, np) } else { (np, rest.len()) };
    let k = k.clamp(lo, hi);
    if k == p {
        return (k, None);
    }
    // Index of a document once `from` is out of the list.
    let without = |d: usize| if d > from { d - 1 } else { d };
    // Before the group's tab at `k`, or right after the group's last tab.
    let to = if k < hi { without(rest[k]) } else { without(rest[hi - 1]) + 1 };
    (k, Some(to))
}

impl App {
    /// Move the document at `from` to index `to` of `docs` (the tab order), keeping every
    /// document index that other state holds pointing at the same document.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        let n = self.docs.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let d = self.docs.remove(from);
        self.docs.insert(to, d);
        let m = |i: usize| remap(i, from, to);
        self.active = m(self.active);
        self.close_armed = self.close_armed.map(m);
        self.tabs_seen = self.tabs_seen.map(m);
        self.tab_hover = self.tab_hover.map(|(i, c)| (m(i), c));
        self.blame_for = self.blame_for.map(|(i, l, t)| (m(i), l, t));
        if let Some(sp) = self.split.as_mut() {
            for p in &mut sp.panes {
                p.doc = m(p.doc);
            }
        }
        if let Some(dr) = self.inp.tabs.drag.as_mut() {
            dr.doc = m(dr.doc);
        }
        self.request_redraw();
    }

    /// A tab is being dragged (past the threshold).
    pub fn tab_dragging(&self) -> bool {
        self.inp.tabs.drag.is_some_and(|d| d.active)
    }

    /// Left press on tab `doc` (already activated): arm a drag.
    pub fn tab_drag_press(&mut self, doc: usize, x: f64, y: f64) {
        let left = self.tab_screen_x(doc).map_or(x, |(l, _)| l as f64);
        self.inp.tabs.drag = Some(TabDrag { doc, press: (x, y), grab: x - left, active: false });
    }

    /// Screen x and width of tab `doc` in the last drawn strip (scrolled).
    fn tab_screen_x(&self, doc: usize) -> Option<(i32, i32)> {
        let off = self.tabs_scroll.round() as i32;
        let bar = self.layout.tabbar;
        self.inp.tabs.geo.iter().find(|g| g.0 == doc).map(|&(_, x, w)| (bar.x + x - off, w))
    }

    /// Pointer moved with the button down. True when a tab drag consumed the move.
    pub fn tab_drag_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let Some(d) = self.inp.tabs.drag.as_mut() else { return false };
        if !d.active {
            if (x - d.press.0).hypot(y - d.press.1) < DRAG_START {
                return false;
            }
            d.active = true;
            self.tab_hover = None;
            self.hover_rect = None;
            self.dismiss_tooltip();
        }
        self.request_redraw();
        true
    }

    /// Where the dragged tab would land now: (insertion index among the other tabs, `to` for
    /// `move_tab`). None away from the strip (a release there cancels).
    fn tab_drop(&self) -> Option<(usize, Option<usize>)> {
        let d = self.inp.tabs.drag.filter(|d| d.active)?;
        let bar = self.layout.tabbar;
        let y = self.mouse.1 as i32;
        if y < bar.y - DROP_BAND || y > bar.bottom() + DROP_BAND || d.doc >= self.docs.len() {
            return None;
        }
        let (_, w) = self.tab_screen_x(d.doc)?;
        // The floating tab's center against the other tabs' centers.
        let cx = self.mouse.0 - d.grab + w as f64 / 2.0;
        let off = self.tabs_scroll.round() as i32;
        let k = self
            .inp
            .tabs
            .geo
            .iter()
            .filter(|g| g.0 != d.doc)
            .filter(|&&(_, gx, gw)| ((bar.x + gx - off) as f64 + gw as f64 / 2.0) < cx)
            .count();
        let pinned: Vec<bool> = self.docs.iter().map(|d| d.pinned).collect();
        Some(drop_target(&pinned, d.doc, k))
    }

    /// Button released: drop the dragged tab. True when a drag (not a plain click) ended.
    pub fn tab_drag_release(&mut self) -> bool {
        let drop = self.tab_drop();
        let Some(d) = self.inp.tabs.drag.take() else { return false };
        if !d.active {
            return false;
        }
        if let Some((_, Some(to))) = drop {
            self.move_tab(d.doc, to);
        }
        self.request_redraw();
        true
    }

    /// Esc while dragging a tab puts it back. True when the key was used.
    pub fn tab_drag_key(&mut self, key: &Key) -> bool {
        if self.tab_dragging() && matches!(key, Key::Named(NamedKey::Escape)) {
            self.inp.tabs.drag = None;
            self.request_redraw();
            return true;
        }
        false
    }

    /// Per frame: scroll the strip while a dragged tab rests near (or past) either end. True while
    /// scrolling.
    pub fn tab_drag_frame(&mut self, dt: f64) -> bool {
        if !self.tab_dragging() || self.tabs_max <= 0.0 {
            return false;
        }
        let bar = self.layout.tabbar;
        let x = self.mouse.0;
        let (l, r) = (bar.x as f64 + EDGE, bar.right() as f64 - EDGE);
        let depth = if x < l {
            -((l - x) / EDGE).min(1.5)
        } else if x > r {
            ((x - r) / EDGE).min(1.5)
        } else {
            0.0
        };
        if depth == 0.0 {
            return false;
        }
        let before = self.tabs_scroll;
        self.tabs_scroll = (self.tabs_scroll + depth * EDGE_SPEED * dt).clamp(0.0, self.tabs_max);
        // Keep the strip where the drag put it instead of jumping back to the active tab.
        self.tabs_seen = Some(self.active);
        self.tabs_scroll != before
    }

    /// Placeholder, drop marker and floating tab, drawn on top of the strip.
    pub fn draw_tab_drag(&mut self, cv: &mut Canvas) {
        let Some(d) = self.inp.tabs.drag.filter(|d| d.active) else { return };
        let Some(doc) = self.docs.get(d.doc) else { return };
        let Some((sx, w)) = self.tab_screen_x(d.doc) else { return };
        let c = self.theme.chrome;
        let bar = self.layout.tabbar;
        let strip = Rect::new(bar.x + 2, bar.y, bar.w - 4, bar.h);
        // The tab's own slot: covered, with a faint dashed-looking outline where it came from.
        let slot = Rect::new(sx + 2, bar.y + 5, w - 4, bar.h - 10);
        cv.fill_clip(Rect::new(sx, bar.y, w, bar.h), c.tabbar_bg, strip);
        cv.fill_round_clip(slot, 8, self.theme.foreground.with_alpha(0x0c), strip);
        // Drop marker: a short accent bar with rounded ends between the two tabs it would land in.
        if let Some((k, to)) = self.tab_drop() {
            let off = self.tabs_scroll.round() as i32;
            let others: Vec<(usize, i32, i32)> = self.inp.tabs.geo.iter().copied().filter(|g| g.0 != d.doc).collect();
            let mx = match (to, others.get(k), k.checked_sub(1).and_then(|j| others.get(j))) {
                (None, _, _) => None,
                (Some(_), Some(&(_, gx, _)), _) => Some(bar.x + gx - off),
                (Some(_), None, Some(&(_, gx, gw))) => Some(bar.x + gx + gw - off),
                _ => None,
            };
            if let Some(mx) = mx {
                let m = Rect::new(mx - 1, bar.y + 8, 3, bar.h - 16);
                cv.fill_round_clip(m, 1, c.accent, strip);
            }
        }
        // Floating tab: follows the pointer along the strip, lifted with a soft shadow.
        let fx = (self.mouse.0 - d.grab).round() as i32;
        let fx = fx.min(strip.right() - w).max(strip.x);
        let fr = Rect::new(fx + 2, bar.y + 5, w - 4, bar.h - 10);
        for i in 1..=4 {
            let g = Rect::new(fr.x - i, fr.y - i + 3, fr.w + 2 * i, fr.h + 2 * i);
            cv.fill_round(g, 8 + i, crate::theme::Rgba(0, 0, 0, 10));
        }
        cv.card(fr, 8, c.tab_active_bg, c.border);
        let fg = c.tab_active_fg;
        let icon = self.tx.icons.file_icon(&doc.name);
        let name = doc.name.clone();
        self.tx.icon(cv, &icon, fx + 12, bar.y + (bar.h - 16) / 2, 16, fg, fr);
        self.tx.draw_ui(cv, fx + 34, bar.y + (bar.h - 18) / 2, &name, fg, fr, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_follows_a_remove_and_insert() {
        // [a b c d e]: b (1) -> 3 gives [a c d b e].
        let after: Vec<usize> = (0..5).map(|i| remap(i, 1, 3)).collect();
        assert_eq!(after, vec![0, 3, 1, 2, 4]);
        // d (3) -> 0 gives [d a b c e].
        let after: Vec<usize> = (0..5).map(|i| remap(i, 3, 0)).collect();
        assert_eq!(after, vec![1, 2, 3, 0, 4]);
        assert_eq!(remap(2, 2, 2), 2);
        // It is a permutation: applying it to a list matches remove + insert.
        for (from, to) in [(0, 4), (4, 0), (1, 2), (2, 1), (3, 3)] {
            let mut v: Vec<usize> = (0..5).collect();
            let x = v.remove(from);
            v.insert(to, x);
            for old in 0..5 {
                assert_eq!(v[remap(old, from, to)], old, "{from}->{to}");
            }
        }
    }

    /// Display order after applying the drop of `from` at `k`.
    fn shown_after(pinned: &[bool], from: usize, k: usize) -> Vec<usize> {
        let (_, to) = drop_target(pinned, from, k);
        let mut ids: Vec<usize> = (0..pinned.len()).collect();
        let mut pins = pinned.to_vec();
        if let Some(to) = to {
            let x = ids.remove(from);
            ids.insert(to, x);
            let p = pins.remove(from);
            pins.insert(to, p);
        }
        pinned_first(&pins).into_iter().map(|i| ids[i]).collect()
    }

    #[test]
    fn drops_land_where_the_marker_was() {
        let none = [false; 4];
        // Others of 1 are [0 2 3]; k = 3 is the end.
        assert_eq!(shown_after(&none, 1, 3), vec![0, 2, 3, 1]);
        assert_eq!(shown_after(&none, 1, 0), vec![1, 0, 2, 3]);
        assert_eq!(shown_after(&none, 3, 1), vec![0, 3, 1, 2]);
        assert_eq!(drop_target(&none, 1, 1), (1, None), "its own slot: no move");
    }

    #[test]
    fn pinned_tabs_stay_in_their_group() {
        // Docs [A* B C* D]: shown A C B D.
        let pins = [true, false, true, false];
        // D dragged to the very front stops at the start of the unpinned group.
        assert_eq!(drop_target(&pins, 3, 0).0, 2);
        assert_eq!(shown_after(&pins, 3, 0), vec![0, 2, 3, 1]);
        // A dragged to the very end stops after the last pinned tab.
        assert_eq!(drop_target(&pins, 0, 9).0, 1);
        assert_eq!(shown_after(&pins, 0, 9), vec![2, 0, 1, 3]);
        // B to the end of everything.
        assert_eq!(shown_after(&pins, 1, 3), vec![0, 2, 3, 1]);
        // A lone pinned tab has nowhere to go.
        assert_eq!(drop_target(&[true, false, false], 0, 2), (0, None));
    }

    #[test]
    fn move_tab_remaps_active_panes_and_pins() {
        use crate::app_unsaved::testkit;
        let _g = testkit::lock();
        let dir = testkit::tmp("tabdrag");
        let mut app = testkit::test_app(&dir);
        // Docs: 0 seed.txt, 1 b, 2 c, 3 d.
        for t in ["b", "c", "d"] {
            testkit::untitled(&mut app, t);
        }
        let text = |app: &App, i: usize| app.docs[i].buf.slice(0, app.docs[i].buf.len_chars());
        app.docs[3].pinned = true;
        app.layout.card = Rect::new(0, 0, 1400, 800);
        app.activate(2);
        app.split_right(); // focused pane 1 on doc 2, pane 0 keeps doc 2 as well
        app.activate(1); // focused pane now shows b
        app.close_armed = Some(0);
        let panes_before: Vec<String> = app
            .split
            .as_ref()
            .map(|sp| (0..sp.panes.len()).map(|i| text(&app, sp.doc_of(i, app.active))).collect())
            .unwrap_or_default();
        assert_eq!(panes_before.len(), 2, "split opened");

        app.move_tab(1, 3); // b to the end: [seed c d* b]
        assert_eq!(text(&app, app.active), "b", "the active tab is still b");
        assert_eq!(app.active, 3);
        assert_eq!(app.close_armed, Some(0));
        let panes_after: Vec<String> = app
            .split
            .as_ref()
            .map(|sp| (0..sp.panes.len()).map(|i| text(&app, sp.doc_of(i, app.active))).collect())
            .unwrap();
        assert_eq!(panes_after, panes_before, "every pane shows the same document");
        assert!(app.docs[2].pinned && text(&app, 2) == "d", "the pin moved with its document");

        app.move_tab(0, 2); // seed between d and b: [c d* seed b]
        assert_eq!(text(&app, app.active), "b");
        assert_eq!(app.close_armed, Some(2), "the armed close follows seed.txt");
        // The pinned tab is still shown first.
        let shown: Vec<String> = crate::app_ctxmenu::tab_order(&app.docs).iter().map(|&i| text(&app, i)).collect();
        assert_eq!(shown[0], "d");
        app.move_tab(9, 0); // out of range: nothing happens
        assert_eq!(app.docs.len(), 4);
        std::fs::remove_dir_all(&dir).ok();
    }
}
