//! Toasts: short messages that slide in at the bottom right, wait, and fade away.
//!
//! * `App::notify(kind, text, action)` shows one; an optional action button (for example "Undo")
//!   runs a command and closes it. Up to [`MAX`] stack; the oldest leaves when a fourth arrives.
//! * Hovering a toast keeps it alive; Esc is not needed, the close button and a click on the body
//!   dismiss it. Failures and warnings stay longer than confirmations.
//! * Failures and warnings that the app reports with `say` / `say_for` also appear here
//!   (see [`classify`]); the status line keeps its text as before.
//! * Every animated value is stepped by the frame time and reported through `busy`; a resting
//!   toast costs no redraws (`App::toast_tick` wakes the loop when one is due to leave).

use crate::{
    anim::{self, Anim, Hot, dur},
    app::App,
    app_overlay::Cmd,
    ds,
    theme::Rgba,
    ui::{Canvas, Rect},
};
use std::time::{Duration, Instant};
use winit::window::CursorIcon;

/// Toasts on screen at once.
pub const MAX: usize = 3;
const H: i32 = 44;
const STACK_GAP: i32 = ds::S2;
const SLIDE: i32 = 28;
const MAX_TEXT: i32 = 320;
const BTN_H: i32 = 26;
const CLOSE: i32 = 20;
const LIFE_OK: Duration = Duration::from_secs(4);
const LIFE_ACTION: Duration = Duration::from_secs(7);
const LIFE_BAD: Duration = Duration::from_secs(8);
/// A hovered toast stays at least this much longer.
const HOVER_GRACE: Duration = Duration::from_secs(2);

/// Kinds callers pick from (not every one has a caller yet).
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

impl ToastKind {
    fn life(self, has_action: bool) -> Duration {
        match self {
            ToastKind::Warn | ToastKind::Error => LIFE_BAD,
            _ if has_action => LIFE_ACTION,
            _ => LIFE_OK,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            ToastKind::Info => "ui/info",
            ToastKind::Success => "ui/check",
            ToastKind::Warn | ToastKind::Error => "ui/warning",
        }
    }
}

/// Which messages of `say` deserve a toast: failures. (`say_for` is always a warning.)
pub fn classify(msg: &str) -> Option<ToastKind> {
    let m = msg.to_lowercase();
    const BAD: [&str; 8] =
        ["failed", "error", "cannot", "can't", "could not", "unable", "denied", "not a git repository"];
    BAD.iter().any(|w| m.contains(*w)).then_some(ToastKind::Error)
}

/// Which window corner the stack grows from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Corner {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

impl Corner {
    fn right(self) -> bool {
        matches!(self, Corner::BottomRight | Corner::TopRight)
    }

    fn bottom(self) -> bool {
        matches!(self, Corner::BottomRight | Corner::BottomLeft)
    }
}

pub struct Toast {
    kind: ToastKind,
    text: String,
    action: Option<(String, Cmd)>,
    deadline: Instant,
    leaving: bool,
    /// Entrance / exit, 0..1.
    grow: Anim,
    /// Position in the stack (0 = bottom), fractional while the stack reflows.
    slot: Anim,
    hot_act: Hot,
    hot_close: Hot,
    /// Rects drawn last frame (window coordinates).
    card: Rect,
    act: Rect,
    close: Rect,
}

#[derive(Default)]
pub struct Toasts {
    list: Vec<Toast>,
}

impl Toasts {
    /// The cards drawn last frame, for the sweep tests.
    #[cfg(test)]
    pub(crate) fn cards(&self) -> Vec<Rect> {
        self.list.iter().filter(|t| !t.leaving && t.card.w > 0).map(|t| t.card).collect()
    }

    /// Add a toast at `now`: the same message again only renews it; a fourth pushes the oldest out.
    pub fn push(&mut self, kind: ToastKind, text: &str, action: Option<(&str, Cmd)>, now: Instant) {
        let text = text.replace('\n', " ");
        let life = kind.life(action.is_some());
        if let Some(t) = self.list.iter_mut().find(|t| !t.leaving && t.kind == kind && t.text == text) {
            t.deadline = now + life;
            return;
        }
        while self.list.iter().filter(|t| !t.leaving).count() >= MAX {
            if let Some(t) = self.list.iter_mut().find(|t| !t.leaving) {
                t.leaving = true;
            }
        }
        self.list.push(Toast {
            kind,
            text,
            action: action.map(|(l, c)| (l.to_string(), c)),
            deadline: now + life,
            leaving: false,
            grow: Anim::new(0.0, dur::SLOW),
            slot: Anim::new(0.0, dur::BASE),
            hot_act: Hot::default(),
            hot_close: Hot::default(),
            card: Rect::default(),
            act: Rect::default(),
            close: Rect::default(),
        });
    }

    /// A toast under the pointer stays at least [`HOVER_GRACE`] longer.
    fn hold(&mut self, mouse: (f64, f64), now: Instant) {
        for t in self.list.iter_mut().filter(|t| !t.leaving && t.card.contains(mouse.0, mouse.1)) {
            t.deadline = t.deadline.max(now + HOVER_GRACE);
        }
    }

    /// Mark what is due as leaving; returns the next deadline among those still waiting.
    pub fn expire(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for t in self.list.iter_mut().filter(|t| !t.leaving) {
            if t.deadline <= now {
                t.leaving = true;
            } else {
                next = Some(next.map_or(t.deadline, |n| n.min(t.deadline)));
            }
        }
        next
    }

    fn live(&self) -> usize {
        self.list.iter().filter(|t| !t.leaving).count()
    }

    /// Advance every animation; true while anything moves.
    fn step(&mut self, dt: f64, mouse: (f64, f64), now: Instant) -> bool {
        let mut busy = false;
        self.hold(mouse, now);
        for t in &mut self.list {
            if !t.leaving && t.deadline <= now {
                t.leaving = true;
            }
            t.grow.to(if t.leaving { 0.0 } else { 1.0 });
            t.hot_act.set(t.act.contains(mouse.0, mouse.1));
            t.hot_close.set(t.close.contains(mouse.0, mouse.1));
            busy |= t.grow.step(dt) | t.hot_act.step(dt) | t.hot_close.step(dt);
        }
        self.list.retain(|t| !(t.leaving && !t.grow.moving()));
        // The newest toast sits at the bottom (slot 0), older ones above it.
        for (k, t) in self.list.iter_mut().rev().filter(|t| !t.leaving).enumerate() {
            t.slot.to(k as f32);
            busy |= t.slot.step(dt);
        }
        busy
    }
}

impl App {
    /// Show a toast; `action` is a button label and the command it runs (then the toast closes).
    pub fn notify(&mut self, kind: ToastKind, text: impl AsRef<str>, action: Option<(&str, Cmd)>) {
        self.toasts.push(kind, text.as_ref(), action, Instant::now());
        self.request_redraw();
    }

    /// Per frame: expire, animate; true while anything moves.
    pub fn toast_frame(&mut self, dt: f64) -> bool {
        if self.toasts.list.is_empty() {
            return false;
        }
        self.toasts.step(dt, self.mouse, Instant::now())
    }

    /// Per event-loop turn: wake when the next toast is due to leave.
    pub fn toast_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let before = self.toasts.live();
        self.toasts.hold(self.mouse, now);
        if let Some(next) = self.toasts.expire(now) {
            want(next);
        }
        if self.toasts.live() != before {
            self.request_redraw();
        }
    }

    /// A press on a toast: its button runs the command, anywhere else on the toast closes it.
    pub fn toast_mouse(&mut self, x: f64, y: f64) -> bool {
        let Some(i) = self.toasts.list.iter().position(|t| !t.leaving && t.card.contains(x, y)) else {
            return false;
        };
        let t = &mut self.toasts.list[i];
        t.leaving = true;
        let cmd = if t.act.contains(x, y) { t.action.as_ref().map(|(_, c)| *c) } else { None };
        if let Some(cmd) = cmd {
            self.run_cmd(cmd);
        }
        true
    }

    /// Pointer over a toast's buttons.
    pub fn toast_cursor(&self) -> Option<CursorIcon> {
        let (x, y) = self.mouse;
        self.toasts.list.iter().find(|t| !t.leaving && t.card.contains(x, y)).map(|_| CursorIcon::Pointer)
    }

    /// Rects a toast must not cover: the controls of whatever modal is up (the tour's buttons,
    /// a menu or sheet's rows, the settings window) and the update card.
    fn toast_avoid(&self) -> Vec<Rect> {
        let mut v: Vec<Rect> = self.layout.overlay_rows.iter().map(|(r, _)| *r).collect();
        if let Some(o) = &self.onboarding {
            v.extend(o.rects.iter().map(|(r, _)| *r));
        }
        if let crate::app_overlay::Overlay::Settings(m) = &self.overlay {
            v.push(m.rect());
        }
        v
    }

    /// The window corner the stack lives in: bottom right, unless that would sit on a control of
    /// an open modal, then bottom left, top right, top left.
    fn toast_corner(&self) -> Corner {
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        let avoid = self.toast_avoid();
        if avoid.is_empty() {
            return Corner::BottomRight;
        }
        let fw = (MAX_TEXT + 6 * ds::S6).min(sw / 2);
        let fh = MAX as i32 * (H + STACK_GAP);
        let top = self.layout.topbar.bottom() + ds::S2;
        let free = |c: Corner| {
            let x = if c.right() { sw - ds::S4 - fw } else { ds::S4 };
            let y = if c.bottom() { sh - ds::S4 - fh } else { top };
            let foot = Rect::new(x, y, fw, fh);
            !avoid.iter().any(|r| r.intersect(&foot).w > 0 && r.intersect(&foot).h > 0)
        };
        [Corner::BottomRight, Corner::BottomLeft, Corner::TopRight, Corner::TopLeft]
            .into_iter()
            .find(|c| free(*c))
            .unwrap_or(Corner::BottomRight)
    }

    pub fn draw_toasts(&mut self, cv: &mut Canvas) {
        if self.toasts.list.is_empty() {
            return;
        }
        let c = self.theme.chrome;
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        let corner = self.toast_corner();
        let mut bottom = sh - ds::S4;
        if self.upd.card_rect.h > 0 && corner == Corner::BottomRight {
            bottom = bottom.min(self.upd.card_rect.y - ds::S2);
        }
        let top = self.layout.topbar.bottom() + ds::S2;
        let right = sw - ds::S4;
        let mut list = std::mem::take(&mut self.toasts.list);
        for t in list.iter_mut() {
            let a = t.grow.value.clamp(0.0, 1.0);
            let tint = match t.kind {
                ToastKind::Info => c.info,
                ToastKind::Success => c.git_added,
                ToastKind::Warn => c.warning,
                ToastKind::Error => c.error,
            };
            let act_w = t.action.as_ref().map_or(0, |(l, _)| self.tx.ui_width(l, true) + 2 * ds::S3);
            let fixed =
                ds::S3 + ds::ICON_M + ds::S2 + if act_w > 0 { ds::S3 + act_w } else { 0 } + ds::S2 + CLOSE + ds::S2;
            let room = (sw - 2 * ds::S4 - fixed).clamp(80, MAX_TEXT);
            let line = crate::ui::fit_ellipsis(&mut self.tx, &t.text, room, false);
            let w = fixed + self.tx.ui_width(&line, false);
            let step = (t.slot.value * (H + STACK_GAP) as f32).round() as i32;
            let y = if corner.bottom() { bottom - H - step } else { top + step };
            let slide = ((1.0 - a) * SLIDE as f32).round() as i32;
            let x = if corner.right() { right - w + slide } else { ds::S4 - slide };
            let r = Rect::new(x, y, w, H);
            t.card = r;
            let f = |col: Rgba| anim::fade(col, a);
            if a > 0.6 {
                cv.shadow(r, ds::R_CARD);
            }
            cv.card(r, ds::R_CARD, f(c.raised), f(c.border_glow));
            cv.fill_round(Rect::new(r.x + 1, r.y + ds::S3, 3, H - 2 * ds::S3), 1, f(tint));
            self.tx.icon(cv, t.kind.icon(), r.x + ds::S3, r.y + (H - ds::ICON_M) / 2, ds::ICON_M, f(tint), r);
            let tx0 = r.x + ds::S3 + ds::ICON_M + ds::S2;
            self.tx.draw_ui(cv, tx0, r.y + (H - 18) / 2, &line, f(c.tab_active_fg), r, false);
            let mut edge = r.right() - ds::S2 - CLOSE;
            t.close = Rect::new(edge, r.y + (H - CLOSE) / 2, CLOSE, CLOSE);
            let hc = t.hot_close.v();
            if hc > 0.0 {
                cv.fill_round(t.close, ds::R_CTRL, f(anim::fade(c.hover_overlay, hc)));
            }
            let ic = anim::lerp_rgba(c.tab_inactive_fg, c.tab_active_fg, hc);
            self.tx.icon(cv, "ui/close", t.close.x + 4, t.close.y + 4, ds::ICON_S - 2, f(ic), t.close);
            t.act = Rect::default();
            if let Some((label, _)) = &t.action {
                edge -= ds::S3 + act_w;
                let b = Rect::new(edge, r.y + (H - BTN_H) / 2, act_w, BTN_H);
                t.act = b;
                let ha = t.hot_act.v();
                cv.fill_round(b, ds::R_CTRL, f(c.accent_text.with_alpha((28.0 + ha * 48.0) as u8)));
                let lw = self.tx.ui_width(label, true);
                self.tx.draw_ui(cv, b.x + (b.w - lw) / 2, b.y + (BTN_H - 18) / 2, label, f(c.accent_text), b, true);
            }
        }
        self.toasts.list = list;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_toasts_push_the_oldest_out() {
        let now = Instant::now();
        let mut t = Toasts::default();
        for i in 0..4 {
            t.push(ToastKind::Info, &format!("m{i}"), None, now);
        }
        assert_eq!(t.live(), MAX);
        assert!(t.list[0].leaving && !t.list[3].leaving);
    }

    #[test]
    fn the_same_message_renews_instead_of_stacking() {
        let now = Instant::now();
        let mut t = Toasts::default();
        t.push(ToastKind::Error, "boom", None, now);
        t.push(ToastKind::Error, "boom", None, now + Duration::from_secs(3));
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.list[0].deadline, now + Duration::from_secs(3) + LIFE_BAD);
    }

    #[test]
    fn toasts_leave_when_due_and_failures_last_longer() {
        let now = Instant::now();
        let mut t = Toasts::default();
        t.push(ToastKind::Success, "saved", None, now);
        t.push(ToastKind::Error, "failed", None, now);
        let next = t.expire(now + Duration::from_secs(1));
        assert_eq!(next, Some(now + LIFE_OK));
        let next = t.expire(now + LIFE_OK + Duration::from_millis(1));
        assert_eq!(next, Some(now + LIFE_BAD));
        assert!(t.list[0].leaving && !t.list[1].leaving);
        assert!(LIFE_ACTION > LIFE_OK, "a toast with a button waits longer");
    }

    #[test]
    fn a_toast_slides_in_and_is_removed_after_it_fades() {
        let now = Instant::now();
        let mut t = Toasts::default();
        t.push(ToastKind::Info, "hello", None, now);
        let mut n = 0;
        while t.step(0.016, (-1.0, -1.0), now) {
            n += 1;
            assert!(n < 200);
        }
        assert_eq!(t.list[0].grow.value, 1.0);
        t.list[0].leaving = true;
        while t.step(0.016, (-1.0, -1.0), now) {
            n += 1;
            assert!(n < 400);
        }
        assert!(t.list.is_empty());
    }

    #[test]
    fn failures_are_told_from_plain_messages() {
        assert_eq!(classify("docker: failed to start"), Some(ToastKind::Error));
        assert_eq!(classify("could not run: nope"), Some(ToastKind::Error));
        assert_eq!(classify("bookmark added"), None);
    }
}
