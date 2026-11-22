//! Chrome of the bottom panel: the animated open / close / maximize, the tab pills with a sliding
//! indicator and status badges, the icon toolbar on the right of the header and the friendly empty
//! state of the Output tab. The views inside the panel (terminal, tests, debug, build, docker) keep
//! their own modules; the dock frame belongs to the shell.
//!
//! Motion: everything is a spring from `anim` stepped with the frame time in `panel_motion_step`
//! (called from `input_frame`), which reports whether the panel is still moving so an idle editor
//! keeps sleeping.

use crate::{
    anim::{Anim, dur, lerp},
    app::App,
    app_panel::{PANEL_HEADER, PanelTab},
    app_tooltip::TipText,
    ds,
    theme::Rgba,
    ui::{Canvas, Rect},
};

/// Below this body height (px) the panel is mid-animation: only the header is drawn.
const MIN_BODY: i32 = 40;
/// A maximized panel takes this share (percent) of the room between the tab bar and the window end.
const TALL_PERCENT: i32 = 70;

/// A button of the panel header or of an empty state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelBtn {
    Clear,
    Rerun,
    Stop,
    Maximize,
    Close,
    RunTask,
}

/// What a tab pill shows after its label.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Badge {
    None,
    Dot(Rgba),
    Count(usize, Rgba),
}

/// Animation state and hit regions of the panel chrome.
pub struct PanelMotion {
    /// 0 = closed, 1 = fully open (the panel's height scales with it).
    pub open: Anim,
    /// 0 = normal height, 1 = maximized.
    pub max: Anim,
    pub maximized: bool,
    /// The next open / close flip animates (set by the toggle; anything else snaps).
    pub arm: bool,
    seen_open: bool,
    ind_x: Anim,
    ind_w: Anim,
    ind_ready: bool,
    /// Buttons at the last draw.
    pub btns: Vec<(Rect, PanelBtn)>,
}

impl PanelMotion {
    pub fn new() -> Self {
        Self {
            open: Anim::new(0.0, dur::SLOW),
            max: Anim::new(0.0, dur::SLOW),
            maximized: false,
            arm: false,
            seen_open: false,
            ind_x: Anim::new(0.0, dur::BASE),
            ind_w: Anim::new(0.0, dur::BASE),
            ind_ready: false,
            btns: Vec::new(),
        }
    }
}

/// Panel height for the open / maximize progress: `base` normal, `tall` maximized, scaled by `open_v`.
pub fn slide_h(base: i32, tall: i32, max_v: f32, open_v: f32) -> i32 {
    let h = lerp(base as f32, tall as f32, max_v.clamp(0.0, 1.0));
    (h * open_v.clamp(0.0, 1.0)).round().max(0.0) as i32
}

/// Width of the badge slot after a label (0 for none).
fn badge_w(count_w: i32, b: Badge) -> i32 {
    match b {
        Badge::None => 0,
        Badge::Dot(_) => 6 + 6,
        Badge::Count(..) => 6 + count_w,
    }
}

const TABS: [(&str, PanelTab); 6] = [
    ("Output", PanelTab::Output),
    ("Tests", PanelTab::Tests),
    ("Build", PanelTab::Build),
    ("Docker", PanelTab::Docker),
    ("Terminal", PanelTab::Terminal),
    ("Debug", PanelTab::Debug),
];

impl App {
    // ───────────── motion ─────────────

    /// Follow `panel.open`: animate when the toggle armed it, snap for any other change.
    fn panel_motion_sync(&mut self) {
        let open = self.panel.open;
        let m = &mut self.panel.motion;
        if m.seen_open != open {
            m.seen_open = open;
            let t = if open { 1.0 } else { 0.0 };
            if m.arm {
                m.open.to(t);
            } else {
                m.open.snap(t);
            }
            m.arm = false;
        }
    }

    /// Height of the panel for this layout pass (`avail` is the room under the breadcrumbs).
    pub fn panel_slide_h(&mut self, avail: i32) -> i32 {
        self.panel_motion_sync();
        let base = self.panel.height.min(avail / 2).max(0);
        let tall = (avail * TALL_PERCENT / 100).max(base);
        let m = &self.panel.motion;
        slide_h(base, tall, m.max.value, m.open.value)
    }

    /// Is the panel on screen, including while it slides shut?
    pub fn panel_visible(&self) -> bool {
        self.panel.open || self.panel.motion.open.value > 0.001
    }

    /// True while the height is changing (the terminals keep their size until it settles).
    pub fn panel_animating(&self) -> bool {
        let m = &self.panel.motion;
        m.open.moving() || m.max.moving()
    }

    /// Advance the panel's springs by `dt`; true while anything moves.
    pub fn panel_motion_step(&mut self, dt: f64) -> bool {
        self.panel_motion_sync();
        let active = self.layout.panel_tabs.iter().find(|(_, t)| *t == self.panel.tab).map(|(r, _)| *r);
        let visible = self.panel_visible();
        let m = &mut self.panel.motion;
        m.max.to(if m.maximized { 1.0 } else { 0.0 });
        let mut busy = m.open.step(dt);
        busy |= m.max.step(dt);
        match active {
            Some(r) if visible => {
                if m.ind_ready {
                    m.ind_x.to(r.x as f32);
                    m.ind_w.to(r.w as f32);
                } else {
                    m.ind_x.snap(r.x as f32);
                    m.ind_w.snap(r.w as f32);
                    m.ind_ready = true;
                }
            }
            _ => m.ind_ready = false,
        }
        busy |= m.ind_x.step(dt);
        busy |= m.ind_w.step(dt);
        busy
    }

    // ───────────── buttons ─────────────

    /// The enabled header / empty-state button under (x, y).
    pub fn panel_btn_at(&self, x: f64, y: f64) -> Option<(Rect, PanelBtn)> {
        if !self.panel.open {
            return None;
        }
        self.panel.motion.btns.iter().rev().find(|(r, _)| r.contains(x, y)).copied()
    }

    pub fn panel_btn_click(&mut self, b: PanelBtn) {
        match b {
            PanelBtn::Clear => {
                let p = &mut self.panel;
                p.lines.clear();
                p.plain.clear();
                p.scroll = 0.0;
                p.follow = true;
            }
            PanelBtn::Rerun => self.repeat_task(),
            PanelBtn::Stop => self.stop_task(),
            PanelBtn::Maximize => self.panel.motion.maximized = !self.panel.motion.maximized,
            PanelBtn::Close => self.toggle_panel(),
            PanelBtn::RunTask => self.open_task_picker(),
        }
    }

    pub fn panel_btn_tip(&self, b: PanelBtn) -> TipText {
        let keys = |id: &str| self.shortcut_text(id);
        let (title, k, detail) = match b {
            PanelBtn::Clear => ("Clear output", None, None),
            PanelBtn::Rerun => ("Run again", keys("RepeatTask"), self.panel.cmd.clone()),
            PanelBtn::Stop => ("Stop the task", None, None),
            PanelBtn::Maximize if self.panel.motion.maximized => ("Restore panel size", None, None),
            PanelBtn::Maximize => ("Maximize panel", None, None),
            PanelBtn::Close => ("Close panel", keys("ToggleOutput"), None),
            PanelBtn::RunTask => ("Run a task", None, Some("Pick a script from the project".to_string())),
        };
        TipText { title: title.to_string(), keys: k, detail, more: Vec::new() }
    }

    // ───────────── drawing ─────────────

    fn tab_badge(&self, tab: PanelTab) -> Badge {
        let c = self.theme.chrome;
        match tab {
            PanelTab::Output => match (&self.panel.running, self.panel.exit) {
                (Some(_), _) => Badge::Dot(c.accent),
                (None, Some(0)) => Badge::Dot(c.git_added),
                (None, Some(_)) => Badge::Dot(c.error),
                _ => Badge::None,
            },
            PanelTab::Tests => {
                let n = crate::testrun::Counts::of(&self.tests.cases);
                if n.failed > 0 {
                    Badge::Count(n.failed, c.error)
                } else if n.running > 0 {
                    Badge::Dot(c.accent)
                } else {
                    Badge::None
                }
            }
            PanelTab::Build if self.build.running() => Badge::Dot(c.accent),
            PanelTab::Terminal if self.terms.tabs.len() > 1 => Badge::Count(self.terms.tabs.len(), c.accent_text),
            PanelTab::Debug if self.dbg.is_some() => Badge::Dot(c.git_added),
            _ => Badge::None,
        }
    }

    /// The tab pills with the sliding indicator; returns the x where they end.
    pub(crate) fn draw_panel_tabs(&mut self, cv: &mut Canvas, r: Rect, head: Rect, ty: i32) -> i32 {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        let mut x = r.x + ds::S2;
        let mut items: Vec<(&str, PanelTab, Rect, Badge, i32, i32)> = Vec::with_capacity(TABS.len());
        for (label, tab) in TABS {
            let badge = self.tab_badge(tab);
            let count_w = match badge {
                Badge::Count(n, _) => self.tx.ui_width_sized(&n.min(99).to_string(), true, 10.5) + ds::S2,
                _ => 0,
            };
            let lw = self.tx.ui_width(label, true);
            let w = lw + 20 + badge_w(count_w, badge);
            let pill = Rect::new(x, r.y + 4, w, PANEL_HEADER - 7);
            items.push((label, tab, pill, badge, lw, count_w));
            self.layout.panel_tabs.push((pill, tab));
            x += w + ds::S1;
        }
        // The indicator glides from the old tab to the new one.
        if let Some((_, _, pill, ..)) = items.iter().find(|i| i.1 == self.panel.tab) {
            let m = &self.panel.motion;
            let ind = if m.ind_ready {
                Rect::new(m.ind_x.value.round() as i32, pill.y, m.ind_w.value.round() as i32, pill.h)
            } else {
                *pill
            };
            cv.fill_round(ind, ds::R_ROW, c.tab_active_fg.with_alpha(0x1e));
        }
        for (label, tab, pill, badge, lw, count_w) in items {
            let active = self.panel.tab == tab;
            let tw = self.tx.ui_width(label, active);
            let tx = pill.x + 10 + (lw - tw) / 2;
            self.tx.draw_ui(cv, tx, ty, label, if active { c.tab_active_fg } else { dim }, head, active);
            let bx = pill.x + 10 + lw + 6;
            match badge {
                Badge::None => {}
                Badge::Dot(col) => {
                    cv.fill_round(Rect::new(bx, pill.y + (pill.h - 6) / 2, 6, 6), 3, col);
                }
                Badge::Count(n, col) => {
                    let text = if n > 99 { "99+".to_string() } else { n.to_string() };
                    let chip = Rect::new(bx, pill.y + (pill.h - 14) / 2, count_w, 14);
                    cv.fill_round(chip, 7, col.with_alpha(0x2a));
                    let w = self.tx.ui_width_sized(&text, true, 10.5);
                    self.tx.draw_ui_sized(cv, chip.x + (chip.w - w) / 2, chip.y - 1, &text, col, chip, true, 10.5);
                }
            }
        }
        x
    }

    /// The icon buttons at the right end of the header; returns the x where they start.
    pub(crate) fn draw_panel_toolbar(&mut self, cv: &mut Canvas, r: Rect) -> i32 {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        self.panel.motion.btns.clear();
        let size = ds::HIT_MIN;
        let y = r.y + 1 + (PANEL_HEADER - 1 - size) / 2;
        let mut x = r.right() - ds::S2 - size;
        let maxed = self.panel.motion.maximized;
        let mut tail: Vec<(PanelBtn, &str, bool, Rgba)> = vec![
            (PanelBtn::Close, "ui/close", true, dim),
            (PanelBtn::Maximize, if maxed { "ui/chevron_down" } else { "ui/chevron_up" }, true, dim),
        ];
        if self.panel.tab == PanelTab::Output {
            // Rightmost first: the list is drawn right to left.
            tail.push(if self.panel.running.is_some() {
                (PanelBtn::Stop, "ui/stop", true, c.error)
            } else {
                (PanelBtn::Rerun, "ui/rerun", self.panel.cmd.is_some(), dim)
            });
            tail.push((PanelBtn::Clear, "ui/eraser", !self.panel.lines.is_empty(), dim));
        }
        let split = tail.len() > 2;
        let mut left = r.right() - ds::S2;
        for (i, (btn, icon, enabled, col)) in tail.into_iter().enumerate() {
            let rect = Rect::new(x, y, size, size);
            let col = if enabled { col } else { col.with_alpha(0x55) };
            self.tx.icon(
                cv,
                icon,
                rect.x + (size - ds::ICON_S) / 2,
                rect.y + (size - ds::ICON_S) / 2,
                ds::ICON_S,
                col,
                rect,
            );
            if enabled {
                self.panel.motion.btns.push((rect, btn));
            }
            left = rect.x;
            x -= size + ds::S1;
            if i == 1 && split {
                // A hairline between the panel's own buttons and the view's.
                cv.fill(Rect::new(left - ds::S1 / 2 - 1, y + 5, 1, size - 10), c.border);
                x -= ds::S1;
            }
        }
        left
    }

    /// Friendly Output tab with nothing in it: what it is for and the one action that fills it.
    pub(crate) fn draw_output_empty(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let title = "No output yet";
        let hint = "Task and test output shows up here, with clickable errors";
        let content_h = ds::ICON_L + ds::S3 + 18 + ds::S1 + 18 + ds::S4 + 28;
        let mut y = body.y + ((body.h - content_h) / 2).max(ds::S2);
        let cx = body.x + body.w / 2;
        self.tx.icon(cv, "ui/terminal", cx - ds::ICON_L / 2, y, ds::ICON_L, c.accent_text, body);
        y += ds::ICON_L + ds::S3;
        let tw = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, cx - tw / 2, y, title, strong, body, true);
        y += 18 + ds::S1;
        let hw = self.tx.ui_width(hint, false);
        self.tx.draw_ui(cv, cx - hw / 2, y, hint, dim, body, false);
        y += 18 + ds::S4;
        let label = "Run task";
        let lw = self.tx.ui_width(label, true);
        let w = lw + ds::ICON_S + ds::S2 + 2 * ds::S4;
        let btn = Rect::new(cx - w / 2, y, w, 28);
        let (mx, my) = self.mouse;
        let fill = if btn.contains(mx, my) { c.accent.mix(Rgba(255, 255, 255, 255), 0.14) } else { c.accent };
        cv.fill_round_clip(btn, ds::R_CTRL, fill, body);
        self.tx.icon(cv, "ui/play_filled", btn.x + ds::S4, btn.y + (28 - ds::ICON_S) / 2, ds::ICON_S, c.on_accent, btn);
        self.tx.draw_ui(cv, btn.x + ds::S4 + ds::ICON_S + ds::S2, btn.y + 5, label, c.on_accent, btn, true);
        self.panel.motion.btns.push((btn, PanelBtn::RunTask));
    }
}

/// Is the body tall enough to draw (not in the first frames of the slide)?
pub fn body_fits(body: Rect) -> bool {
    body.h >= MIN_BODY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slide_scales_with_open_and_maximize() {
        assert_eq!(slide_h(260, 500, 0.0, 1.0), 260);
        assert_eq!(slide_h(260, 500, 1.0, 1.0), 500);
        assert_eq!(slide_h(260, 500, 0.0, 0.0), 0);
        assert_eq!(slide_h(260, 500, 0.0, 0.5), 130);
        assert_eq!(slide_h(260, 500, 0.5, 1.0), 380);
        assert_eq!(slide_h(260, 500, 2.0, 3.0), 500, "inputs are clamped");
    }

    #[test]
    fn badge_slot_is_zero_without_badge() {
        assert_eq!(badge_w(20, Badge::None), 0);
        assert!(badge_w(0, Badge::Dot(Rgba(1, 2, 3, 255))) > 0);
        assert!(badge_w(20, Badge::Count(3, Rgba(1, 2, 3, 255))) > badge_w(0, Badge::Count(3, Rgba(1, 2, 3, 255))));
    }

    #[test]
    fn motion_starts_closed_and_at_rest() {
        let m = PanelMotion::new();
        assert_eq!(m.open.value, 0.0);
        assert!(!m.open.moving() && !m.max.moving());
        assert!(!m.maximized && !m.arm);
    }
}
