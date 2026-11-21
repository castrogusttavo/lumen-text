//! Dropping a file or a tab onto the editor: VS Code style drop zones.
//!
//! While a file from the explorer or a tab is dragged over the editor, the pane under the pointer
//! shows where it would land: the left / right / top / bottom half (a new pane on that side, which
//! splits the editor, a pane that is already one of several included) or the whole pane (open it
//! there). Releasing builds the split. A dragged tab *moves*: the pane it came from shows another
//! tab, or closes when no other tab is left. A file dropped from the OS lands the same way.
//!
//! The geometry (zone of a point, the grid that results) is pure and lives in `app_split`
//! (`zone_at`, `zone_rect`, `Grid::drop_split`); this file is the glue and the painting.

use crate::{
    app::{App, Focus},
    app_ctxmenu::tab_order,
    app_split::{Pane, Split, Zone, focus_to, pick_replacement, remove_pane, zone_at, zone_rect},
    ui::{Canvas, Rect},
};
use std::path::Path;

/// A pane and the side of it that a drag is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropSpot {
    pub pane: usize,
    pub zone: Zone,
}

impl App {
    /// A file or a tab is being dragged (the pointer shows a grabbing hand).
    pub fn drop_dragging(&self) -> bool {
        self.tab_dragging() || self.tree.drag.as_ref().is_some_and(|d| d.active)
    }

    /// Name of the item being dragged, if it can be dropped on the editor: a tab, or a single file
    /// of the explorer (folders and multi-selections only move inside the tree).
    pub fn drag_item(&self) -> Option<String> {
        if self.tab_dragging() {
            let d = self.inp.tabs.drag?.doc;
            let doc = self.docs.get(d)?;
            return Some(doc.name.clone());
        }
        let d = self.tree.drag.as_ref().filter(|d| d.active)?;
        let [p] = d.paths.as_slice() else { return None };
        if self.tree.rows.iter().any(|r| &r.path == p && r.is_dir) {
            return None;
        }
        Some(p.file_name()?.to_string_lossy().into_owned())
    }

    /// Pane and zone under (x, y) when something dropped there would land in the editor.
    pub fn drop_spot(&self, x: f64, y: f64) -> Option<DropSpot> {
        let card = self.layout.card;
        if !self.overlay.is_none() || self.docs.is_empty() || !card.contains(x, y) {
            return None;
        }
        let (grid, found) = match self.split.as_deref() {
            Some(sp) if !sp.rects.is_empty() && sp.rects.len() == sp.panes.len() => {
                (sp.grid.clone(), sp.rects.iter().position(|r| r.contains(x, y)).map(|i| (i, sp.rects[i])))
            }
            // No split, or no room to draw one: the focused pane fills the card.
            Some(sp) => (sp.grid.clone(), Some((sp.focus, card))),
            None => (crate::app_split::Grid::single(), Some((0, card))),
        };
        let (pane, rect) = found?;
        let zone = zone_at(rect, x, y, |z| grid.fits_drop(pane, z, card));
        Some(DropSpot { pane, zone })
    }

    /// Rectangle of pane `pane` (the whole card without a split).
    fn drop_pane_rect(&self, pane: usize) -> Rect {
        match self.split.as_deref() {
            Some(sp) if sp.rects.len() == sp.panes.len() => sp.rects.get(pane).copied().unwrap_or(self.layout.card),
            _ => self.layout.card,
        }
    }

    /// Put document `d` where `spot` says. `from_tab`: the document was dragged by its tab, so the
    /// pane that was showing it shows another tab (or closes when there is none).
    pub fn drop_doc(&mut self, d: usize, spot: DropSpot, from_tab: bool) {
        if d >= self.docs.len() {
            return;
        }
        let DropSpot { pane, zone } = spot;
        let fresh = self.split.is_none();
        if fresh {
            if zone == Zone::Center {
                return self.activate(d);
            }
            let view = self.docs[self.active].pane_view();
            self.split = Some(Box::new(Split::new(self.active, view)));
        }
        let Some(mut sp) = self.split.take() else { return };
        if pane >= sp.panes.len() {
            self.split = if fresh { None } else { Some(sp) };
            return;
        }
        let src = from_tab.then_some(sp.focus);
        let (target, src) = if zone == Zone::Center {
            if pane == sp.focus {
                self.split = Some(sp);
                return self.activate(d);
            }
            sp.panes[pane] = Pane { doc: d, view: self.docs[d].pane_view() };
            focus_to(&mut self.docs, &mut self.active, &mut sp, pane);
            (pane, src)
        } else {
            let mut grid = sp.grid.clone();
            let new = if grid.fits_drop(pane, zone, self.layout.card) { grid.drop_split(pane, zone) } else { None };
            let Some(new) = new else {
                self.say("No room to split here — widen the window");
                self.split = if fresh { None } else { Some(sp) };
                return;
            };
            // The focused pane's state goes to its slot; the new pane starts from the document's.
            let a = self.active;
            sp.panes[sp.focus].doc = a;
            sp.panes[sp.focus].view = self.docs[a].pane_view();
            sp.panes.insert(new, Pane { doc: d, view: self.docs[d].pane_view() });
            if sp.focus >= new {
                sp.focus += 1;
            }
            if let Some(x) = sp.diff_at.as_mut().filter(|x| **x >= new) {
                *x += 1;
            }
            sp.grid = grid;
            sp.drag = None;
            focus_to(&mut self.docs, &mut self.active, &mut sp, new);
            (new, src.map(|s| if s >= new { s + 1 } else { s }))
        };
        // A moved tab leaves its old pane: another tab takes its place, or the pane closes.
        if let Some(s) = src.filter(|&s| s != target) {
            match pick_replacement(&tab_order(&self.docs), d) {
                Some(r) => sp.panes[s] = Pane { doc: r, view: self.docs[r].pane_view() },
                None if zone == Zone::Center => remove_pane(&mut sp, s),
                // A lone tab dragged to an edge of its own editor is shown in both panes.
                None => {}
            }
        }
        let n = sp.panes.len();
        self.split = (n > 1).then_some(sp);
        self.hover = None;
        self.completion = None;
        self.focus = Focus::Editor;
        self.split_layout();
        self.request_redraw();
        if n > 1 && zone != Zone::Center {
            self.say(format!("Editor split into {n} panes"));
        }
    }

    /// Open file `path` and drop it where `spot` says, without taking the focused pane's document.
    pub fn drop_file(&mut self, path: &Path, spot: DropSpot) {
        let prev = self.active;
        let Some(d) = self.open_path(path, None) else { return };
        let into_focus = spot.zone == Zone::Center && self.split.as_ref().is_none_or(|sp| sp.focus == spot.pane);
        if !into_focus && prev != d && prev < self.docs.len() {
            self.active = prev; // `open_path` activated it in the focused pane
        }
        self.drop_doc(d, spot, false);
    }

    /// A file dropped from the OS onto the window: at the pointer's spot over the editor, else as a tab.
    pub fn drop_os_file(&mut self, path: &Path) {
        if !path.is_file() {
            return;
        }
        match self.drop_spot(self.mouse.0, self.mouse.1) {
            Some(spot) => self.drop_file(path, spot),
            None => {
                self.open_path(path, None);
                self.focus = Focus::Editor;
            }
        }
        self.request_redraw();
    }

    /// Zone overlay over the pane under the pointer, and the ghost of the dragged item beside the
    /// pointer. True when it painted (the caller then skips its own floating label).
    pub fn draw_drop_overlay(&mut self, cv: &mut Canvas) -> bool {
        let Some(name) = self.drag_item() else { return false };
        let (mx, my) = self.mouse;
        let Some(spot) = self.drop_spot(mx, my) else { return false };
        let c = self.theme.chrome;
        let card = self.layout.card;
        let pane = self.drop_pane_rect(spot.pane).intersect(&card);
        let zr = zone_rect(pane, spot.zone).inset(3);
        cv.fill_round_clip(zr, 8, c.accent.with_alpha(0x30), card);
        cv.ring(zr, 8, c.accent.with_alpha(0xd0));
        // Ghost: icon and file name.
        let icon = self.tx.icons.file_icon(&name);
        let w = self.tx.ui_width(&name, false).min(260) + 44;
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        let x = (mx as i32 + 14).min(sw - w - 6).max(6);
        let y = (my as i32 + 16).min(sh - 34).max(6);
        let r = Rect::new(x, y, w, 28);
        cv.shadow(r, 8);
        cv.card(r, 8, c.widget_bg, c.accent);
        self.tx.icon(cv, &icon, r.x + 10, r.y + 6, 16, self.theme.foreground, r);
        self.tx.draw_ui(cv, r.x + 32, r.y + 5, &name, self.theme.foreground, r, false);
        true
    }
}
