//! Motion primitives for the interface. Every animated value is advanced by the frame time `dt`
//! that `draw_all` already receives and reports whether it is still moving, so callers fold that
//! into the `busy` flag (the window only keeps redrawing while something moves; at rest the
//! event loop sleeps and the editor costs no CPU).
//!
//! `Anim` is a critically damped spring: retargeting in mid-flight never jumps (hover that ends
//! while it is still fading in simply turns around), and it settles without overshoot, which is
//! what a code editor wants. `Tween` is for one-shot, fixed-length effects (modal open, toast).

#![allow(dead_code)]

use crate::theme::Rgba;

/// Standard durations in seconds. Pick one of these instead of inventing a number.
pub mod dur {
    /// Hover, press, focus ring: should feel instant.
    pub const FAST: f32 = 0.12;
    /// Menus, tooltips, tab switches, list selection.
    pub const BASE: f32 = 0.18;
    /// Modals, dock panels, sidebars opening.
    pub const SLOW: f32 = 0.28;
    /// Onboarding steps and other once-in-a-session moments.
    pub const STAGE: f32 = 0.45;
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Per-channel blend including alpha (unlike `Rgba::mix`, which forces opaque).
pub fn lerp_rgba(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let f = |x: u8, y: u8| lerp(x as f32, y as f32, t).round().clamp(0.0, 255.0) as u8;
    Rgba(f(a.0, b.0), f(a.1, b.1), f(a.2, b.2), f(a.3, b.3))
}

/// `c` with its alpha scaled by `k` (0..1): the cheap way to fade anything that has a color.
pub fn fade(c: Rgba, k: f32) -> Rgba {
    Rgba(c.0, c.1, c.2, (c.3 as f32 * k.clamp(0.0, 1.0)).round() as u8)
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// Slight overshoot, for things that "land" (a card appearing, a checkmark).
pub fn ease_out_back(t: f32) -> f32 {
    let (c1, c3) = (1.70158, 2.70158);
    let t = t.clamp(0.0, 1.0) - 1.0;
    1.0 + c3 * t * t * t + c1 * t * t
}

/// A value that chases a target with a critically damped spring.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    pub value: f32,
    vel: f32,
    target: f32,
    /// Natural frequency in rad/s: how quickly it settles (~ 4.6 / settle-time).
    omega: f32,
}

impl Anim {
    /// Starts at rest on `v`, settling in about `settle` seconds (use `dur::*`).
    pub fn new(v: f32, settle: f32) -> Self {
        Self { value: v, vel: 0.0, target: v, omega: 4.6 / settle.max(0.01) }
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Move toward `t`; a no-op when already heading there.
    pub fn to(&mut self, t: f32) {
        self.target = t;
    }

    /// Jump with no motion (first frame, a different file, reduced motion).
    pub fn snap(&mut self, v: f32) {
        self.value = v;
        self.target = v;
        self.vel = 0.0;
    }

    pub fn moving(&self) -> bool {
        (self.value - self.target).abs() > 0.001 || self.vel.abs() > 0.01
    }

    /// Advance by `dt` seconds (exact solution, stable at any frame rate); true while moving.
    pub fn step(&mut self, dt: f64) -> bool {
        if !self.moving() {
            self.value = self.target;
            self.vel = 0.0;
            return false;
        }
        let dt = (dt as f32).clamp(0.0, 0.05);
        let w = self.omega;
        let x = self.value - self.target;
        let e = (-w * dt).exp();
        let t = self.vel + w * x;
        self.value = self.target + (x + t * dt) * e;
        self.vel = (self.vel - w * t * dt) * e;
        if !self.moving() {
            self.value = self.target;
            self.vel = 0.0;
            return false;
        }
        true
    }
}

/// Hover/press/focus amount in 0..1 for one widget: `set(true)` while the pointer is over it.
#[derive(Clone, Copy, Debug)]
pub struct Hot(pub Anim);

impl Default for Hot {
    fn default() -> Self {
        Self(Anim::new(0.0, dur::FAST))
    }
}

impl Hot {
    pub fn set(&mut self, on: bool) {
        self.0.to(if on { 1.0 } else { 0.0 });
    }
    pub fn step(&mut self, dt: f64) -> bool {
        self.0.step(dt)
    }
    pub fn v(&self) -> f32 {
        self.0.value.clamp(0.0, 1.0)
    }
}

/// A fixed-length one-shot animation (progress 0..1 over `len` seconds, eased on read).
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    t: f32,
    len: f32,
    running: bool,
}

impl Tween {
    pub fn new(len: f32) -> Self {
        Self { t: 0.0, len: len.max(0.01), running: false }
    }
    pub fn start(&mut self) {
        self.t = 0.0;
        self.running = true;
    }
    pub fn finish(&mut self) {
        self.t = self.len;
        self.running = false;
    }
    pub fn running(&self) -> bool {
        self.running
    }
    /// Raw 0..1 progress.
    pub fn raw(&self) -> f32 {
        (self.t / self.len).clamp(0.0, 1.0)
    }
    /// Progress with ease-out-cubic applied (the default feel).
    pub fn eased(&self) -> f32 {
        ease_out_cubic(self.raw())
    }
    pub fn step(&mut self, dt: f64) -> bool {
        if !self.running {
            return false;
        }
        self.t += (dt as f32).clamp(0.0, 0.05);
        if self.t >= self.len {
            self.finish();
            return false;
        }
        true
    }
}

/// Offsets a staggered entrance: item `i` of `n` starts `i * gap` seconds after the first.
/// Returns that item's eased 0..1 given the overall elapsed seconds and per-item length.
pub fn stagger(elapsed: f32, i: usize, gap: f32, item_len: f32) -> f32 {
    ease_out_cubic((elapsed - i as f32 * gap) / item_len.max(0.01))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_settles_without_overshoot() {
        let mut a = Anim::new(0.0, dur::BASE);
        a.to(1.0);
        let mut frames = 0;
        let mut max = 0.0f32;
        while a.step(1.0 / 60.0) {
            max = max.max(a.value);
            frames += 1;
            assert!(frames < 120, "did not settle");
        }
        assert!(max <= 1.0 + 1e-4);
        assert_eq!(a.value, 1.0);
        assert!(frames > 3, "should take more than a couple of frames");
    }

    #[test]
    fn retarget_midway_is_continuous() {
        let mut a = Anim::new(0.0, dur::SLOW);
        a.to(1.0);
        for _ in 0..6 {
            a.step(1.0 / 60.0);
        }
        let before = a.value;
        a.to(0.0);
        a.step(1.0 / 60.0);
        assert!((a.value - before).abs() < 0.2, "no jump on retarget");
    }

    #[test]
    fn rest_reports_idle() {
        let mut a = Anim::new(0.5, dur::FAST);
        assert!(!a.step(0.016));
        a.snap(2.0);
        assert!(!a.moving());
    }

    #[test]
    fn tween_runs_once() {
        let mut t = Tween::new(0.2);
        assert!(!t.step(0.016));
        t.start();
        let mut n = 0;
        while t.step(1.0 / 60.0) {
            n += 1;
            assert!(n < 100);
        }
        assert_eq!(t.raw(), 1.0);
        assert!(!t.running());
    }

    #[test]
    fn lerp_rgba_blends_alpha() {
        let c = lerp_rgba(Rgba(0, 0, 0, 0), Rgba(200, 100, 50, 255), 0.5);
        assert_eq!(c, Rgba(100, 50, 25, 128));
        assert_eq!(fade(Rgba(1, 2, 3, 200), 0.5).3, 100);
    }

    #[test]
    fn stagger_orders_items() {
        assert!(stagger(0.1, 0, 0.05, 0.2) > stagger(0.1, 3, 0.05, 0.2));
        assert_eq!(stagger(0.0, 2, 0.05, 0.2), 0.0);
    }
}
