//! Shell chrome: the floating-card layout (canvas, rail, sidebar card, editor card, dock card),
//! the motion state of the shell (dock / sidebar slide, the Git | AI pill, hover of rail, title
//! bar and read-out chips) and the pass that rounds the cards' corners and rings them.
//!
//! The geometry is a pure function ([`solve`]) so it can be tested without a window; everything
//! that moves is an [`Anim`] / [`Hot`] stepped by the frame time in `App::render` and reported
//! through the `busy` flag, so an idle editor still sleeps.

use crate::{
    anim::{Anim, Hot, dur, fade},
    app::{App, CARD_M},
    ds,
    theme::{Rgba, Theme},
    ui::{Canvas, Rect, sc},
};

/// Buttons of the activity rail (see `RailBtn`).
const RAIL_BTNS: usize = 9;
/// Title-bar buttons: project picker, branch picker, memory read-out, the nine menu titles and
/// the three buttons of the run cluster (see `app_menubar`).
const TOP_BTNS: usize = 15;
/// Read-out chips and badges (see `StatusBtn`).
const CHIPS: usize = 20;

/// Which hover animation a widget owns.
#[derive(Clone, Copy, Debug)]
pub enum HotSlot {
    Rail(usize),
    Top(usize),
    Chip(usize),
}

/// Motion state of the shell. Lives in `Layout`, which is `Default`.
#[derive(Clone, Copy, Debug)]
pub struct Shell {
    /// True once the window shows frames: before that (and in the headless runner and the tests)
    /// every animation snaps to its target so the layout is exact.
    pub live: bool,
    /// How far the right-hand dock is in (0 closed, 1 open).
    pub dock: Anim,
    /// How far the sidebar is in.
    pub side: Anim,
    /// The Git | AI pill: x relative to the switch, and width.
    pub tab_x: Anim,
    pub tab_w: Anim,
    pub tab_seen: bool,
    rail: [Hot; RAIL_BTNS],
    top: [Hot; TOP_BTNS],
    chip: [Hot; CHIPS],
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            live: false,
            dock: Anim::new(0.0, dur::SLOW),
            side: Anim::new(0.0, dur::SLOW),
            tab_x: Anim::new(0.0, dur::BASE),
            tab_w: Anim::new(0.0, dur::BASE),
            tab_seen: false,
            rail: Default::default(),
            top: Default::default(),
            chip: Default::default(),
        }
    }
}

impl Shell {
    fn hot_mut(&mut self, slot: HotSlot) -> &mut Hot {
        match slot {
            HotSlot::Rail(i) => &mut self.rail[i.min(RAIL_BTNS - 1)],
            HotSlot::Top(i) => &mut self.top[i.min(TOP_BTNS - 1)],
            HotSlot::Chip(i) => &mut self.chip[i.min(CHIPS - 1)],
        }
    }

    /// Advance every animation by `dt`; true while any still moves.
    pub fn step(&mut self, dt: f64) -> bool {
        let mut m = self.dock.step(dt);
        m |= self.side.step(dt);
        m |= self.tab_x.step(dt);
        m |= self.tab_w.step(dt);
        for h in self.rail.iter_mut().chain(self.top.iter_mut()).chain(self.chip.iter_mut()) {
            m |= h.step(dt);
        }
        m
    }

    /// Anything still heading to a target (checked after drawing, which sets the targets).
    pub fn moving(&self) -> bool {
        self.dock.moving()
            || self.side.moving()
            || self.tab_x.moving()
            || self.tab_w.moving()
            || self.rail.iter().chain(self.top.iter()).chain(self.chip.iter()).any(|h| h.0.moving())
    }
}

/// Inputs of the layout math (logical pixels).
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub win: (i32, i32),
    /// Height of the title bar (0 in zen mode).
    pub top: i32,
    /// Height of the status bar under the cards (0 when hidden).
    pub bottom: i32,
    /// Width of the rail (0 in zen mode).
    pub rail: i32,
    /// Nominal width of the sidebar and of the dock, whether or not they are open.
    pub side_w: i32,
    pub dock_w: i32,
    pub tab_h: i32,
    pub bread_h: i32,
    /// Wanted height of the bottom panel (0 closed).
    pub panel_h: i32,
    /// Gap between cards and around them (0 in zen mode).
    pub gap: i32,
    /// How far the sidebar and the dock are in, 0..1.
    pub side_t: f32,
    pub dock_t: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Solved {
    pub sidebar: Rect,
    /// The whole editor card: tab bar, breadcrumbs, document and bottom panel.
    pub outer: Rect,
    pub tabbar: Rect,
    pub bread: Rect,
    pub card: Rect,
    pub panel: Rect,
}

/// Room a sliding column takes (its width and the gap after it) at progress `t`.
fn visible(w: i32, gap: i32, t: f32) -> i32 {
    if w <= 0 { 0 } else { (((w + gap) as f32) * t.clamp(0.0, 1.0)).round() as i32 }
}

/// The right-hand dock card ("as if open" at `t == 1`), sliding in from the window's right edge.
pub fn dock_rect(win: (i32, i32), top: i32, bottom: i32, w: i32, gap: i32, t: f32) -> Rect {
    Rect::new(win.0 - visible(w, gap, t), top, w, (win.1 - top - bottom - gap).max(0))
}

pub fn solve(p: &Plan) -> Solved {
    let g = p.gap;
    let body_h = (p.win.1 - g - p.top - p.bottom).max(0);
    let side_vis = visible(p.side_w, g, p.side_t);
    let dock = dock_rect(p.win, p.top, p.bottom, p.dock_w, g, p.dock_t);
    let x0 = p.rail + side_vis;
    let right = if p.dock_w > 0 { dock.x - g } else { p.win.0 - g };
    let ew = (right - x0).max(0);
    let avail = (body_h - p.tab_h - p.bread_h).max(0);
    // The panel animates its own height and caps it (half the card, or most of it when maximized).
    let panel_h = p.panel_h.min(avail).max(0);
    let card_h = avail - panel_h;
    let y_card = p.top + p.tab_h + p.bread_h;
    Solved {
        sidebar: Rect::new(p.rail - (p.side_w + g - side_vis).max(0), p.top, p.side_w, body_h),
        outer: Rect::new(x0, p.top, ew, body_h),
        tabbar: Rect::new(x0, p.top, ew, p.tab_h),
        bread: Rect::new(x0, p.top + p.tab_h, ew, p.bread_h),
        card: Rect::new(x0, y_card, ew, card_h),
        panel: Rect::new(x0, y_card + card_h, ew, panel_h),
    }
}

/// The canvas the cards float on: the theme background, a step darker.
pub fn canvas_color(t: &Theme) -> Rgba {
    let k = if t.background.luminance() < 0.5 { 0.38 } else { 0.06 };
    t.background.mix(Rgba(0, 0, 0, 255), k)
}

impl Canvas<'_> {
    /// Round the corners of an already painted rectangle: the pixels outside the arc go back to
    /// `bg` (antialiased), so any content drawn square inside a card ends up clipped to it.
    pub fn round_off(&mut self, r: Rect, rad: i32, bg: Rgba) {
        let d = self.to_dev(r);
        let rad = sc(rad).min(d.w / 2).min(d.h / 2);
        if rad <= 0 {
            return;
        }
        let rf = rad as f32;
        for (cx, cy, x0, y0) in [
            (d.x + rad, d.y + rad, d.x, d.y),
            (d.right() - rad, d.y + rad, d.right() - rad, d.y),
            (d.x + rad, d.bottom() - rad, d.x, d.bottom() - rad),
            (d.right() - rad, d.bottom() - rad, d.right() - rad, d.bottom() - rad),
        ] {
            for y in y0..y0 + rad {
                for x in x0..x0 + rad {
                    let (dx, dy) = (x as f32 + 0.5 - cx as f32, y as f32 + 0.5 - cy as f32);
                    let outside = 1.0 - (rf - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
                    if outside > 0.0 {
                        self.dot_dev(x, y, Rgba(bg.0, bg.1, bg.2, (255.0 * outside) as u8));
                    }
                }
            }
        }
    }
}

impl App {
    /// Gap between the floating cards: none in zen mode.
    pub fn shell_gap(&self) -> i32 {
        if self.zen.active() { 0 } else { CARD_M }
    }

    /// Progress of the dock, as the layout sees it (fully in whenever the shell does not animate).
    pub fn shell_dock_t(&self) -> f32 {
        if self.layout.shell.live { self.layout.shell.dock.value } else { 1.0 }
    }

    /// Point the slide animations at the current state (snapping when the shell is not live).
    pub fn shell_sync(&mut self) {
        let dock = if self.slot_shown() { 1.0 } else { 0.0 };
        let side = if self.sidebar_open { 1.0 } else { 0.0 };
        let sh = &mut self.layout.shell;
        sh.dock.to(dock);
        sh.side.to(side);
        if !sh.live {
            sh.dock.snap(dock);
            sh.side.snap(side);
        }
    }

    /// Hover amount (0..1) of a widget occupying `r`; also points its animation at the pointer.
    pub fn hot_amount(&mut self, slot: HotSlot, r: Rect) -> f32 {
        let over = self.overlay.is_none() && !self.dragging && r.contains(self.mouse.0, self.mouse.1);
        let h = self.layout.shell.hot_mut(slot);
        h.set(over);
        h.v()
    }

    /// A faint foreground veil over a badge that painted itself: the hover for widgets that do
    /// not own a tint.
    pub fn hot_veil(&mut self, cv: &mut Canvas, slot: HotSlot, r: Rect, rad: i32) {
        let v = self.hot_amount(slot, r);
        if v > 0.01 {
            let a = if self.theme.background.luminance() < 0.5 { 0x16 } else { 0x12 };
            cv.fill_round(r, rad, fade(self.theme.foreground.with_alpha(a), v));
        }
    }

    /// Fill `r` with the animated hover (or the active) tint of a button; returns the hover amount
    /// (0..1) so the caller can brighten its icon or text. `rest` is the tint when idle (0..1 of
    /// the hover color: 0 for ghost buttons, more for pills that should read at rest).
    pub fn hot_fill(&mut self, cv: &mut Canvas, slot: HotSlot, r: Rect, rad: i32, on: bool, rest: f32) -> f32 {
        let c = self.theme.chrome;
        let v = self.hot_amount(slot, r);
        if on {
            cv.fill_round(r, rad, c.list_active_bg);
        } else {
            let k = rest + (1.0 - rest) * v;
            if k > 0.01 {
                cv.fill_round(r, rad, fade(c.list_hover_bg, k));
            }
        }
        v
    }

    /// The sliding pill behind the active tab of the Git | AI switch. `active` is the tab's rect
    /// this frame, `strip` the switch.
    pub fn dock_pill(&mut self, cv: &mut Canvas, strip: Rect, active: Rect) {
        let (rel, w) = ((active.x - strip.x) as f32, active.w as f32);
        let sh = &mut self.layout.shell;
        if !sh.tab_seen || !sh.live {
            sh.tab_x.snap(rel);
            sh.tab_w.snap(w);
            sh.tab_seen = true;
        } else {
            sh.tab_x.to(rel);
            sh.tab_w.to(w);
        }
        let pill =
            Rect::new(strip.x + sh.tab_x.value.round() as i32, active.y, sh.tab_w.value.round() as i32, active.h);
        cv.fill_round_clip(pill, ds::R_ROW, self.theme.chrome.tab_active_fg.with_alpha(0x1e), strip);
    }

    /// After the cards drew their (square) content: round their corners and ring them. The
    /// editor card is the hero and gets the only shadow (drawn earlier, under everything).
    pub fn draw_cards(&mut self, cv: &mut Canvas) {
        if self.shell_gap() == 0 {
            return;
        }
        let bg = canvas_color(&self.theme);
        let border = self.theme.chrome.border;
        let mut cards = vec![self.layout.card_outer];
        if self.sidebar_open && self.layout.sidebar.w > 0 && !self.layout.shell.side.moving() {
            cards.push(self.layout.sidebar);
        }
        if self.slot_shown() {
            cards.push(self.slot_rect());
        }
        for r in cards {
            cv.round_off(r, ds::R_CARD, bg);
            cv.ring(r, ds::R_CARD, border);
        }
    }

    /// The editor card's soft shadow, painted right after the canvas.
    pub fn draw_card_shadow(&mut self, cv: &mut Canvas) {
        if self.shell_gap() > 0 && self.layout.card_outer.w > 0 {
            cv.shadow(self.layout.card_outer, ds::R_CARD);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan {
            win: (1400, 900),
            top: 38,
            bottom: 0,
            rail: 44,
            side_w: 256,
            dock_w: 420,
            tab_h: 40,
            bread_h: 26,
            panel_h: 0,
            gap: ds::GAP,
            side_t: 1.0,
            dock_t: 1.0,
        }
    }

    fn dock_of(p: &Plan) -> Rect {
        dock_rect(p.win, p.top, p.bottom, p.dock_w, p.gap, p.dock_t)
    }

    #[test]
    fn cards_float_with_gaps_and_do_not_overlap() {
        let s = solve(&plan());
        let dock = dock_of(&plan());
        let g = ds::GAP;
        assert_eq!(s.sidebar.x, 44);
        assert_eq!(s.outer.x, s.sidebar.right() + g, "gap between sidebar and editor");
        assert_eq!(dock.x, s.outer.right() + g, "gap between editor and dock");
        assert_eq!(dock.right(), 1400 - g, "margin right of the dock");
        assert_eq!(s.outer.bottom(), 900 - g, "margin under the cards");
        assert_eq!(dock.bottom(), 900 - g);
        assert_eq!(s.card.y, s.outer.y + 40 + 26, "tab bar and breadcrumbs sit inside the card");
        assert_eq!(s.card.bottom(), s.outer.bottom());
    }

    #[test]
    fn status_bar_reserves_the_bottom_for_every_card() {
        let p = Plan { bottom: 22, ..plan() };
        let s = solve(&p);
        let dock = dock_of(&p);
        let want = 900 - 22 - ds::GAP;
        assert_eq!(s.outer.bottom(), want, "the cards end above the bar, with the gap");
        assert_eq!(s.sidebar.bottom(), want);
        assert_eq!(dock.bottom(), want);
        assert_eq!(s.outer.h, solve(&plan()).outer.h - 22, "the editor card gives the bar its row");
        let tall = solve(&Plan { bottom: 22, panel_h: 5000, ..plan() });
        assert_eq!(tall.panel.bottom(), want, "the bottom panel stays inside the card");
    }

    #[test]
    fn closed_columns_give_their_room_to_the_editor() {
        let open = solve(&plan());
        let closed = solve(&Plan { side_t: 0.0, dock_t: 0.0, ..plan() });
        assert_eq!(closed.outer.x, 44);
        assert_eq!(closed.outer.right(), 1400 - ds::GAP);
        assert_eq!(dock_of(&Plan { dock_t: 0.0, ..plan() }).x, 1400, "the dock waits outside the window");
        assert!(closed.outer.w > open.outer.w);
        assert!(closed.sidebar.right() <= 44, "the sidebar hides under the rail");
    }

    #[test]
    fn sliding_is_continuous() {
        let mut last = solve(&Plan { dock_t: 0.0, ..plan() }).outer.w;
        for i in 1..=10 {
            let w = solve(&Plan { dock_t: i as f32 / 10.0, ..plan() }).outer.w;
            assert!(w <= last, "the editor only shrinks while the dock comes in");
            last = w;
        }
        let end = solve(&plan());
        assert_eq!(last, end.outer.w);
    }

    #[test]
    fn panel_lives_inside_the_editor_card() {
        let s = solve(&Plan { panel_h: 200, ..plan() });
        assert_eq!(s.panel.h, 200);
        assert_eq!(s.card.bottom(), s.panel.y);
        assert_eq!(s.panel.bottom(), s.outer.bottom());
        let tall = solve(&Plan { panel_h: 5000, ..plan() });
        assert!(tall.panel.h <= tall.outer.h, "never taller than the card");
    }

    #[test]
    fn zen_has_no_gaps() {
        let s = solve(&Plan { top: 0, rail: 0, gap: 0, side_t: 0.0, dock_t: 0.0, tab_h: 0, bread_h: 0, ..plan() });
        assert_eq!(s.outer, Rect::new(0, 0, 1400, 900));
        assert_eq!(s.card, s.outer);
    }

    #[test]
    fn hot_state_settles_and_reports_motion() {
        let mut sh = Shell::default();
        assert!(!sh.step(0.016), "nothing moves at rest");
        sh.hot_mut(HotSlot::Rail(2)).set(true);
        assert!(sh.moving());
        let mut n = 0;
        while sh.step(0.016) && n < 200 {
            n += 1;
        }
        assert!(n < 200 && (sh.hot_mut(HotSlot::Rail(2)).v() - 1.0).abs() < 0.01);
    }
}
