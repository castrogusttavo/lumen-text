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
    app_overlay::Overlay,
    scroller::{self, Scroller},
    ui::{Canvas, Rect, VBar, line_h},
};
use winit::event::MouseScrollDelta;

/// Width of the drag/hit strip at the right edge of a region.
const TRACK_W: i32 = 11;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Reg {
    Output,
    Terminal,
    DebugStack,
    DebugVars,
    DebugOut,
    Tests,
    TestsDetail,
    GitChanges,
    GitLog,
    Picker,
    TopMenu,
    Completion,
    Md,
    Diff,
    /// The list of the open settings / shortcuts modal.
    Modal,
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
    /// `pos` is a whole number of rows (picker, menus, terminal): the position is floored on write.
    pub int: bool,
    /// Pixel height of one row of this pane: a wheel notch scrolls 3 of them. Row regions use their
    /// `unit`, pixel regions the text line height (the Keyboard shortcuts modal's 3 x line_h).
    pub step: f64,
}

/// Glide state of one animated region (see [`animated`]).
pub struct Anim {
    pub id: Reg,
    pub sc: Scroller,
    /// Fraction of a row the list is scrolled past its integer `scroll` (row regions only; pixel
    /// regions keep the whole position in their owner).
    pub frac: f64,
    /// `total` the glide was started for: a different list (typing in a picker) restarts it.
    total: f64,
}

#[derive(Default)]
pub struct Regions {
    pub list: Vec<Region>,
    /// The previous frame's `list`, kept while the new frame is being drawn (animation reads it).
    pub prev: Vec<Region>,
    pub anim: Vec<Anim>,
    /// Index in `list` where the open overlay started registering.
    pub modal_from: usize,
    /// Bar being dragged: (region, pointer offset inside the thumb).
    pub drag: Option<(Reg, f64)>,
}

impl Regions {
    pub fn clear(&mut self) {
        self.prev = std::mem::take(&mut self.list);
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

impl App {
    /// Register a scrollable pane and paint its bar. `total`/`visible`/`pos` share one unit (px or
    /// rows); `unit` is the pixel size of that unit. Returns the bar when the content overflows.
    pub fn region_bar(
        &mut self,
        cv: &mut Canvas,
        id: Reg,
        area: Rect,
        total: f64,
        visible: f64,
        pos: f64,
        unit: f64,
        int: bool,
    ) -> Option<VBar> {
        let track = Rect::new(area.right() - TRACK_W, area.y, TRACK_W, area.h);
        let bar = VBar::new(track, total, visible, pos);
        let step = if unit > 1.5 { unit } else { line_h() as f64 };
        self.regions.list.push(Region { id, area, bar, total, visible, pos, unit, int, step });
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
    /// Every pane scrolls through the one shared [`Scroller`] (see `scroller.rs`).
    pub fn regions_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let Some(r) = self.regions.at(self.modal_open(), x, y) else { return false };
        self.region_glide(r, delta);
        true
    }

    /// The owner's current scroll position of an animated region (in the region's unit).
    fn region_value(&self, id: Reg) -> Option<f64> {
        match id {
            Reg::Picker => match &self.overlay {
                Overlay::Picker(p) => Some(p.scroll as f64),
                _ => None,
            },
            Reg::TopMenu => match &self.overlay {
                Overlay::TopMenu(m) => Some(m.scroll as f64),
                _ => None,
            },
            Reg::Completion => self.completion.as_ref().map(|c| c.scroll as f64),
            Reg::GitChanges => self.gitpanel.as_ref().map(|g| g.scroll[0]),
            Reg::GitLog => self.gitpanel.as_ref().map(|g| g.scroll[1]),
            Reg::Md => self.docs.get(self.active)?.md.as_ref().map(|m| m.scroll),
            Reg::Diff => self.diff.as_ref().map(|d| d.scroll),
            Reg::Output => Some(self.panel.scroll),
            Reg::DebugStack => Some(self.panel.dbg_scroll[0]),
            Reg::DebugVars => Some(self.panel.dbg_scroll[1]),
            Reg::DebugOut => Some(self.panel.dbg_scroll[2]),
            Reg::Tests => Some(self.tests.scroll),
            Reg::TestsDetail => Some(self.tests.detail_scroll),
            Reg::Terminal => {
                let t = self.term.as_ref()?;
                Some((t.grid.history_len() - t.scroll_offset.min(t.grid.history_len())) as f64)
            }
            Reg::Modal => match &self.overlay {
                Overlay::Settings(m) => Some(m.scroll()),
                _ => None,
            },
        }
    }

    /// Index of the glide state for `id`, created on first use.
    fn anim_slot(&mut self, id: Reg) -> usize {
        if let Some(i) = self.regions.anim.iter().position(|a| a.id == id) {
            return i;
        }
        self.regions.anim.push(Anim { id, sc: Scroller::new(0.01), frac: 0.0, total: -1.0 });
        self.regions.anim.len() - 1
    }

    /// Bring the glide state in line with the owner (somebody else moved the list: keyboard
    /// selection, a jump, a new list) and return its slot.
    fn anim_sync(&mut self, r: &Region) -> usize {
        let i = self.anim_slot(r.id);
        let owner = self.region_value(r.id).unwrap_or(r.pos);
        let a = &mut self.regions.anim[i];
        a.sc.eps = (0.5 / r.unit.max(1.0)).min(0.05);
        let moved = if r.int { a.sc.pos.floor() != owner } else { (a.sc.pos - owner).abs() > 1e-6 };
        if moved || (r.int && a.total != r.total) {
            a.sc.reset(owner);
            a.frac = 0.0;
        }
        a.total = r.total;
        i
    }

    /// Write the glide position to the owner (whole rows + remembered fraction for row regions).
    fn anim_apply(&mut self, i: usize, r: &Region) {
        let max = (r.total - r.visible).max(0.0);
        let pos = self.regions.anim[i].sc.pos.clamp(0.0, max);
        let (v, frac) = if r.int {
            let f = pos.floor();
            (f, pos - f)
        } else {
            (pos, 0.0)
        };
        self.regions.anim[i].frac = frac;
        self.set_region(r.id, v, max);
    }

    /// Wheel / touchpad over an animated region: notches glide (with acceleration), pixel deltas
    /// follow the fingers exactly.
    fn region_glide(&mut self, r: Region, delta: MouseScrollDelta) {
        let w = self.wheel_in(delta);
        let d = if w.dy != 0.0 {
            w.dy
        } else if self.mods.shift_key() {
            w.dx
        } else {
            0.0
        };
        if d == 0.0 {
            return;
        }
        let max = (r.total - r.visible).max(0.0);
        let unit = r.unit.max(1.0);
        let i = self.anim_sync(&r);
        let now = scroller::now();
        let sc = &mut self.regions.anim[i].sc;
        if w.pixels {
            sc.pixels(d / unit, max, now);
        } else {
            // One notch = 3 rows of the pane, expressed in the region's unit.
            sc.wheel(d, scroller::WHEEL_ROWS * r.step / unit, max, now);
        }
        self.anim_apply(i, &r);
    }

    /// Fingers lifted over an animated region: let it coast.
    pub fn regions_fling(&mut self) -> bool {
        let (x, y) = self.mouse;
        let Some(r) = self.regions.at(self.modal_open(), x, y) else { return false };
        let i = self.anim_sync(&r);
        self.regions.anim[i].sc.fling(scroller::now());
        true
    }

    /// Per-frame step of every gliding region (called while a frame starts, before drawing).
    /// True while any is still moving.
    pub fn regions_frame(&mut self, dt: f64) -> bool {
        let mut busy = false;
        let mut i = 0;
        while i < self.regions.anim.len() {
            let id = self.regions.anim[i].id;
            let Some(r) = self.regions.prev.iter().rev().find(|r| r.id == id).copied() else {
                // The region is not on screen any more: forget its glide.
                self.regions.anim.remove(i);
                continue;
            };
            let i2 = self.anim_sync(&r);
            let max = (r.total - r.visible).max(0.0);
            let before = self.regions.anim[i2].sc.pos;
            if self.regions.anim[i2].sc.step(dt, max) {
                busy = true;
            }
            if self.regions.anim[i2].sc.pos != before {
                self.anim_apply(i2, &r);
            }
            i += 1;
        }
        busy
    }

    /// One-line description of the gliding regions for the headless `state` token.
    pub fn regions_state(&self) -> String {
        self.regions
            .anim
            .iter()
            .map(|a| {
                format!("{:?}={:.2}(frac {:.2}{})", a.id, a.sc.pos, a.frac, if a.sc.settled() { "" } else { ",moving" })
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Pixel offset of the fractional row an animated row-region is scrolled past (draw the rows
    /// that much higher and one more row at the bottom). 0 when it is not mid-glide.
    pub fn region_off(&self, id: Reg, row_px: i32) -> i32 {
        self.regions.anim.iter().find(|a| a.id == id).map_or(0, |a| (a.frac * row_px as f64).round() as i32)
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
            Reg::Tests => self.tests.scroll = v,
            Reg::TestsDetail => self.tests.detail_scroll = v,
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
                if let Overlay::Picker(p) = &mut self.overlay {
                    p.scroll = v as usize;
                }
            }
            Reg::TopMenu => {
                if let Overlay::TopMenu(m) = &mut self.overlay {
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
            Reg::Modal => {
                if let Overlay::Settings(m) = &mut self.overlay {
                    m.set_scroll(v);
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
        Region {
            id: Reg::Picker,
            area: Rect::new(0, 0, 100, 100),
            bar: VBar::new(track, total, visible, pos),
            total,
            visible,
            pos,
            unit,
            int,
            step: unit.max(20.0),
        }
    }

    #[test]
    fn nothing_scrolls_when_content_fits() {
        let r = region(5.0, 10.0, 0.0, 34.0, true);
        assert!(r.bar.is_none());
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
