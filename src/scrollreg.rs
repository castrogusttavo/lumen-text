//! Scroll regions: one registry so every list that can overflow gets the mouse wheel and a thin,
//! draggable scrollbar (`ui::VBar`) without each panel re-implementing hit tests.
//!
//! Draw code calls [`App::region_bar`] once per frame for each scrollable area; that records the
//! area (wheel hit box), the bar track (drag hit box) and paints the bar. Input code asks the
//! registry which region is under the pointer ([`App::regions_wheel`], [`App::regions_press`],
//! [`App::regions_drag`]) and writes the new position back to the owner via [`App::set_region`].
//! The registry is rebuilt every frame, so a closed panel simply stops being a region. While a modal
//! overlay is open only the regions registered after it started (`modal_from`) answer.

use crate::{
    app::App,
    ui::{Canvas, Rect, VBar, line_h},
};
use winit::event::MouseScrollDelta;

/// Width of the drag/hit strip at the right edge of a region.
const TRACK_W: i32 = 11;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reg {
    Output,
    Terminal,
    DebugStack,
    DebugVars,
    DebugOut,
    GitChanges,
    GitLog,
    Picker,
    TopMenu,
    Completion,
    Md,
    Diff,
}

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub id: Reg,
    /// Wheel hit box (the whole scrollable pane).
    pub area: Rect,
    pub bar: Option<VBar>,
    pub total: f64,
    pub visible: f64,
    pub pos: f64,
    /// Pixels per unit of `pos` (1.0 when `pos` is in pixels, the row height when in rows).
    pub unit: f64,
    /// `pos` is a whole number of rows (picker, menus): the wheel moves by at least one row.
    pub int: bool,
}

#[derive(Default)]
pub struct Regions {
    pub list: Vec<Region>,
    /// Index in `list` where the open overlay started registering.
    pub modal_from: usize,
    /// Bar being dragged: (region, pointer offset inside the thumb).
    pub drag: Option<(Reg, f64)>,
}

impl Regions {
    pub fn clear(&mut self) {
        self.list.clear();
        self.modal_from = 0;
    }

    fn visible_slice(&self, modal: bool) -> &[Region] {
        if modal { &self.list[self.modal_from.min(self.list.len())..] } else { &self.list }
    }

    /// Topmost region whose area contains the point.
    pub fn at(&self, modal: bool, x: f64, y: f64) -> Option<Region> {
        self.visible_slice(modal).iter().rev().find(|r| r.area.contains(x, y)).copied()
    }

    /// Topmost region whose bar track contains the point.
    pub fn bar_at(&self, modal: bool, x: f64, y: f64) -> Option<Region> {
        self.visible_slice(modal).iter().rev().find(|r| r.bar.is_some_and(|b| b.track.contains(x, y))).copied()
    }

    pub fn get(&self, id: Reg) -> Option<Region> {
        self.list.iter().rev().find(|r| r.id == id).copied()
    }
}

/// New position after the wheel: `px` pixels, clamped to `[0, total - visible]`.
pub fn wheel_pos(r: &Region, px: f64) -> f64 {
    let mut d = px / r.unit.max(1.0);
    if r.int && d != 0.0 && d.abs() < 1.0 {
        d = d.signum();
    }
    if r.int {
        d = d.round();
    }
    (r.pos + d).clamp(0.0, (r.total - r.visible).max(0.0))
}

impl App {
    /// Register a scrollable pane and paint its bar. `total`/`visible`/`pos` share one unit (px or
    /// rows); `unit` is the pixel size of that unit. Returns the bar when the content overflows.
    pub fn region_bar(&mut self, cv: &mut Canvas, id: Reg, area: Rect, total: f64, visible: f64, pos: f64, unit: f64, int: bool) -> Option<VBar> {
        let track = Rect::new(area.right() - TRACK_W, area.y, TRACK_W, area.h);
        let bar = VBar::new(track, total, visible, pos);
        self.regions.list.push(Region { id, area, bar, total, visible, pos, unit, int });
        if let Some(b) = bar {
            let (mx, my) = self.mouse;
            let active = self.regions.drag.is_some_and(|(d, _)| d == id);
            b.draw(cv, self.theme.foreground, track.contains(mx, my), active);
        }
        bar
    }

    fn modal_open(&self) -> bool {
        !self.overlay.is_none()
    }

    /// Mouse wheel over a registered pane. False when the pointer is over none (callers fall through).
    pub fn regions_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let Some(r) = self.regions.at(self.modal_open(), x, y) else { return false };
        let px = match delta {
            MouseScrollDelta::LineDelta(_, dy) => -(dy as f64) * 3.0 * if r.unit > 1.5 { r.unit } else { line_h() as f64 },
            MouseScrollDelta::PixelDelta(p) => -p.y,
        };
        let v = wheel_pos(&r, px);
        self.set_region(r.id, v, (r.total - r.visible).max(0.0));
        true
    }

    /// Press on a bar: starts a drag (jumping there when the track, not the thumb, was hit).
    pub fn regions_press(&mut self, x: f64, y: f64) -> bool {
        let Some(r) = self.regions.bar_at(self.modal_open(), x, y) else { return false };
        let b = r.bar.unwrap();
        let grab = if b.thumb.contains(x, y) { y - b.thumb.y as f64 } else { b.thumb.h as f64 / 2.0 };
        self.regions.drag = Some((r.id, grab));
        self.drag_region(r, y, grab);
        true
    }

    /// Pointer moved while a bar is held. True when a drag is in progress.
    pub fn regions_drag(&mut self) -> bool {
        let Some((id, grab)) = self.regions.drag else { return false };
        if let Some(r) = self.regions.get(id) {
            let y = self.mouse.1;
            self.drag_region(r, y, grab);
        }
        true
    }

    fn drag_region(&mut self, r: Region, y: f64, grab: f64) {
        let Some(b) = r.bar else { return };
        let mut v = b.pos_for(y, grab, r.total, r.visible);
        if r.int {
            v = v.round();
        }
        self.set_region(r.id, v, (r.total - r.visible).max(0.0));
    }

    /// True when the pointer is over a bar's track (arrow cursor, hover highlight).
    pub fn over_region_bar(&self, x: f64, y: f64) -> bool {
        self.regions.drag.is_some() || self.regions.bar_at(self.modal_open(), x, y).is_some()
    }

    /// Write a new scroll position (`v` in the region's unit, already clamped) to its owner.
    pub fn set_region(&mut self, id: Reg, v: f64, max: f64) {
        match id {
            Reg::Output => {
                self.panel.scroll = v;
                self.panel.follow = v >= max - 1.0;
            }
            Reg::Terminal => {
                // Bar position counts from the oldest line; the terminal's offset from the live screen.
                if let Some(t) = self.term.as_mut() {
                    t.scroll_offset = ((max - v).round().max(0.0) as usize).min(t.grid.history_len());
                }
            }
            Reg::DebugStack => self.panel.dbg_scroll[0] = v,
            Reg::DebugVars => self.panel.dbg_scroll[1] = v,
            Reg::DebugOut => {
                self.panel.dbg_scroll[2] = v;
                self.panel.dbg_follow = v >= max - 1.0;
            }
            Reg::GitChanges => {
                if let Some(gp) = self.gitpanel.as_mut() {
                    gp.scroll[0] = v;
                }
            }
            Reg::GitLog => {
                if let Some(gp) = self.gitpanel.as_mut() {
                    gp.scroll[1] = v;
                }
            }
            Reg::Picker => {
                if let crate::app_overlay::Overlay::Picker(p) = &mut self.overlay {
                    p.scroll = v as usize;
                }
            }
            Reg::TopMenu => {
                if let crate::app_overlay::Overlay::TopMenu(m) = &mut self.overlay {
                    m.scroll = v as usize;
                }
            }
            Reg::Completion => {
                if let Some(c) = self.completion.as_mut() {
                    c.scroll = v as usize;
                }
            }
            Reg::Md => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    let max_top = d.max_top();
                    if let Some(st) = d.md.as_mut() {
                        st.scroll = v;
                        let top = crate::app_md::scroll_to_top(&st.view, v, max_top, max);
                        st.top_seen = top;
                        d.top = top;
                        d.target = top;
                        d.animating = false;
                    }
                }
            }
            Reg::Diff => {
                if let Some(dv) = self.diff.as_mut() {
                    dv.scroll = v;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(total: f64, visible: f64, pos: f64, unit: f64, int: bool) -> Region {
        let track = Rect::new(90, 0, TRACK_W, 100);
        Region { id: Reg::Picker, area: Rect::new(0, 0, 100, 100), bar: VBar::new(track, total, visible, pos), total, visible, pos, unit, int }
    }

    #[test]
    fn wheel_clamps_to_both_ends() {
        let r = region(100.0, 10.0, 5.0, 34.0, true);
        assert_eq!(wheel_pos(&r, -34.0 * 50.0), 0.0);
        assert_eq!(wheel_pos(&r, 34.0 * 500.0), 90.0);
        let px = region(1000.0, 200.0, 0.0, 1.0, false);
        assert_eq!(wheel_pos(&px, 50.0), 50.0);
        assert_eq!(wheel_pos(&px, 5000.0), 800.0);
    }

    #[test]
    fn row_regions_always_move_at_least_one_row() {
        let r = region(100.0, 10.0, 5.0, 34.0, true);
        assert_eq!(wheel_pos(&r, 3.0), 6.0, "a tiny pixel delta still steps one row down");
        assert_eq!(wheel_pos(&r, -3.0), 4.0);
        assert_eq!(wheel_pos(&r, 0.0), 5.0);
    }

    #[test]
    fn nothing_scrolls_when_content_fits() {
        let r = region(5.0, 10.0, 0.0, 34.0, true);
        assert!(r.bar.is_none());
        assert_eq!(wheel_pos(&r, 340.0), 0.0);
    }

    #[test]
    fn modal_hides_regions_drawn_before_it() {
        let mut g = Regions::default();
        g.list.push(region(100.0, 10.0, 0.0, 34.0, true));
        g.modal_from = 1;
        g.list.push(Region { id: Reg::TopMenu, ..region(100.0, 10.0, 0.0, 34.0, true) });
        assert_eq!(g.at(true, 50.0, 50.0).unwrap().id, Reg::TopMenu);
        assert_eq!(g.at(false, 50.0, 50.0).unwrap().id, Reg::TopMenu, "topmost wins");
        g.list.pop();
        assert!(g.at(true, 50.0, 50.0).is_none(), "the modal drew nothing there");
        assert!(g.at(false, 50.0, 50.0).is_some());
        assert!(g.bar_at(false, 95.0, 50.0).is_some());
        assert!(g.bar_at(false, 10.0, 50.0).is_none());
    }
}
