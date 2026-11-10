//! Scroll dynamics shared by the editor, the sidebar and the lists: a target, an animated position
//! that approaches it exponentially (no overshoot), acceleration for fast wheel spins, exact
//! pixels for touchpads (plus an optional fling when the fingers lift) and short glides for jumps.
//!
//! Units are whatever the caller scrolls in (rows for the editor, pixels for the sidebar). Time is
//! passed in explicitly (`now`, `dt` in seconds), so everything is testable with simulated time.
//!
//! Integration pattern: the container keeps its plain `pos` field (so the rest of the code can read
//! and write it as before) and the `Scroller` mirrors it. `adopt` notices when somebody else moved
//! the container (reveal the caret, jump to the top, scrollbar drag) and drops the animation.

use std::{sync::OnceLock, time::Instant};

/// Time constant of the wheel glide (s). ~4 tau (160 ms) to settle; the first frames already move
/// ~30% of the way, so it feels immediate yet smooth.
pub const WHEEL_TAU: f64 = 0.04;
/// Short glide for jumps (page, home/end, tree reveal): an ease-out of this many seconds.
pub const GLIDE_SECS: f64 = 0.08;
/// Rows per mouse-wheel notch (Zed: 3).
pub const WHEEL_ROWS: f64 = 3.0;
/// Notches closer than this (s) and in the same direction form a "spin".
pub const SPIN_GAP: f64 = 0.14;
/// Extra step per consecutive notch of a spin, and the cap of the multiplier.
pub const SPIN_GAIN: f64 = 0.3;
pub const SPIN_MAX: f64 = 3.4;
/// Touchpad fling: decay time constant (s) and the minimum lift-off velocity (units/s).
pub const FLING_TAU: f64 = 0.32;
pub const FLING_MIN: f64 = 180.0;
pub const FLING_CAP: f64 = 9000.0;

/// Seconds since the first call (monotonic): the clock the app feeds into `Scroller`.
pub fn now() -> f64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[derive(Clone, Debug)]
pub struct Scroller {
    pub pos: f64,
    pub target: f64,
    pub tau: f64,
    /// Distance below which the animation snaps to the target.
    pub eps: f64,
    vel: f64,
    spin: f64,
    dir: f64,
    last_t: f64,
    /// Touchpad velocity estimate (units/s) and the time of the last pixel delta.
    pvel: f64,
    last_px_t: f64,
    /// Running glide: (from, elapsed, duration).
    glide: Option<(f64, f64, f64)>,
}

impl Default for Scroller {
    fn default() -> Self {
        Self::new(0.15)
    }
}

impl Scroller {
    pub fn new(eps: f64) -> Self {
        Self { pos: 0.0, target: 0.0, tau: WHEEL_TAU, eps, vel: 0.0, spin: 0.0, dir: 0.0, last_t: -10.0, pvel: 0.0, last_px_t: -10.0, glide: None }
    }

    /// Follow a container that was moved from outside: if `pos` is not where this scroller left it,
    /// forget the animation and continue from there.
    pub fn adopt(&mut self, pos: f64) {
        if (pos - self.pos).abs() > 1e-6 {
            self.pos = pos;
            self.target = pos;
            self.vel = 0.0;
            self.glide = None;
        }
    }

    /// Like `adopt` for containers that also keep the destination (`Doc::target`).
    pub fn adopt_both(&mut self, pos: f64, target: f64) {
        if (pos - self.pos).abs() > 1e-6 || (target - self.target).abs() > 1e-6 {
            self.pos = pos;
            self.target = target;
            self.vel = 0.0;
            self.glide = None;
        }
    }

    pub fn settled(&self) -> bool {
        self.vel == 0.0 && self.glide.is_none() && (self.target - self.pos).abs() < self.eps.max(1e-9)
    }

    /// Keep everything inside `0..=max` (content shrank, window resized).
    pub fn clamp(&mut self, max: f64) {
        let max = max.max(0.0);
        self.pos = self.pos.clamp(0.0, max);
        self.target = self.target.clamp(0.0, max);
    }

    /// Mouse wheel: `notches` (positive = forward in the positive direction) of `step` units each.
    /// Notches in quick succession and in the same direction scale the step; a reversal drops what
    /// was still pending in the old direction (the glide continues from where it is).
    pub fn wheel(&mut self, notches: f64, step: f64, max: f64, now: f64) {
        if notches == 0.0 {
            return;
        }
        let dir = notches.signum();
        let mut base = self.target;
        if dir != self.dir {
            base = self.pos; // reversal: cancel the pending travel
            self.spin = 0.0;
        } else if now - self.last_t > SPIN_GAP {
            self.spin = 0.0;
        }
        let mult = (1.0 + SPIN_GAIN * self.spin.floor()).min(SPIN_MAX);
        self.spin += notches.abs().min(1.0);
        self.dir = dir;
        self.last_t = now;
        self.vel = 0.0;
        self.glide = None;
        self.tau = WHEEL_TAU;
        self.target = (base + notches * step * mult).clamp(0.0, max.max(0.0));
    }

    /// Touchpad: follow the fingers exactly (the OS already smooths them) and remember the velocity.
    pub fn pixels(&mut self, px: f64, max: f64, now: f64) {
        if px == 0.0 {
            return;
        }
        let dt = now - self.last_px_t;
        if dt > 0.0005 && dt < 0.12 {
            let inst = px / dt;
            self.pvel = if self.pvel.signum() == inst.signum() { self.pvel * 0.6 + inst * 0.4 } else { inst };
        } else {
            self.pvel = 0.0;
        }
        self.last_px_t = now;
        self.vel = 0.0;
        self.glide = None;
        self.dir = px.signum();
        self.spin = 0.0;
        self.pos = (self.pos + px).clamp(0.0, max.max(0.0));
        self.target = self.pos;
    }

    /// Fingers lifted: keep going with the last velocity, decaying. `now` is the lift time.
    pub fn fling(&mut self, now: f64) {
        let fresh = now - self.last_px_t < 0.12;
        if fresh && self.pvel.abs() >= FLING_MIN {
            self.vel = self.pvel.clamp(-FLING_CAP, FLING_CAP);
        }
        self.pvel = 0.0;
    }

    /// Instant move (scrollbar drag, minimap, Ctrl+Home).
    pub fn jump(&mut self, to: f64, max: f64) {
        self.pos = to.clamp(0.0, max.max(0.0));
        self.target = self.pos;
        self.vel = 0.0;
        self.glide = None;
    }

    /// Short glide to `to` (80 ms ease-out, whatever the distance): PageUp/PageDown, go to line,
    /// tree reveal.
    pub fn glide_to(&mut self, to: f64, max: f64) {
        self.target = to.clamp(0.0, max.max(0.0));
        self.glide = Some((self.pos, 0.0, GLIDE_SECS));
        self.vel = 0.0;
        self.spin = 0.0;
        self.dir = 0.0;
    }

    /// Advance by `dt` seconds. True while still moving (the caller should keep drawing).
    pub fn step(&mut self, dt: f64, max: f64) -> bool {
        let max = max.max(0.0);
        if let Some((from, el, dur)) = self.glide {
            let el = el + dt;
            let u = (el / dur).min(1.0);
            self.pos = from + (self.target - from) * (1.0 - (1.0 - u).powi(3));
            if u >= 1.0 {
                self.pos = self.target;
                self.glide = None;
                return false;
            }
            self.glide = Some((from, el, dur));
            return true;
        }
        if self.vel != 0.0 {
            let decay = (-dt / FLING_TAU).exp();
            self.pos += self.vel * FLING_TAU * (1.0 - decay);
            self.vel *= decay;
            if self.pos <= 0.0 || self.pos >= max {
                self.pos = self.pos.clamp(0.0, max);
                self.vel = 0.0;
            }
            if self.vel.abs() < 20.0 {
                self.vel = 0.0;
            }
            self.target = self.pos;
            return self.vel != 0.0;
        }
        let diff = self.target - self.pos;
        if diff.abs() < self.eps {
            self.pos = self.target;
            return false;
        }
        self.pos += diff * (1.0 - (-dt / self.tau.max(1e-4)).exp());
        if (self.target - self.pos).abs() < self.eps {
            self.pos = self.target;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 60.0;

    /// Frames until the scroller settles (max 2000), advancing simulated time.
    fn settle(s: &mut Scroller, max: f64) -> usize {
        let mut n = 0;
        while s.step(DT, max) && n < 2000 {
            n += 1;
        }
        n
    }

    #[test]
    fn converges_without_overshoot() {
        let mut s = Scroller::new(0.05);
        s.wheel(1.0, 3.0, 100.0, 0.0);
        let mut prev = s.pos;
        while s.step(DT, 100.0) {
            assert!(s.pos >= prev && s.pos <= 3.0 + 1e-9, "monotonic, never past the target");
            prev = s.pos;
        }
        assert_eq!(s.pos, 3.0);
        assert!(s.settled());
    }

    #[test]
    fn single_notch_settles_in_about_a_fifth_of_a_second() {
        let mut s = Scroller::new(0.05);
        s.wheel(1.0, 3.0, 100.0, 0.0);
        let frames = settle(&mut s, 100.0);
        assert!((6..=16).contains(&frames), "frames = {frames}");
        // and it is already moving substantially on the first frame
        let mut s = Scroller::new(0.05);
        s.wheel(1.0, 3.0, 100.0, 0.0);
        s.step(DT, 100.0);
        assert!(s.pos > 0.5, "first frame moved {}", s.pos);
    }

    #[test]
    fn fast_spin_accelerates_the_step() {
        let mut slow = Scroller::new(0.05);
        let mut fast = Scroller::new(0.05);
        for i in 0..6 {
            slow.wheel(1.0, 3.0, 1000.0, i as f64 * 0.5); // far apart: no acceleration
            fast.wheel(1.0, 3.0, 1000.0, i as f64 * 0.05); // a spin
        }
        assert_eq!(slow.target, 18.0);
        assert!(fast.target > 30.0, "spin target {}", fast.target);
        assert!(fast.target <= 3.0 * SPIN_MAX * 6.0);
        // after a pause the step is back to normal
        let before = fast.target;
        fast.wheel(1.0, 3.0, 1000.0, 5.0);
        assert_eq!(fast.target, before + 3.0);
    }

    #[test]
    fn clamps_at_both_ends() {
        let mut s = Scroller::new(0.05);
        s.wheel(10.0, 3.0, 20.0, 0.0);
        assert_eq!(s.target, 20.0);
        settle(&mut s, 20.0);
        assert_eq!(s.pos, 20.0);
        s.wheel(-100.0, 3.0, 20.0, 5.0);
        assert_eq!(s.target, 0.0);
        settle(&mut s, 20.0);
        assert_eq!(s.pos, 0.0);
        s.clamp(5.0);
        assert_eq!(s.pos, 0.0);
    }

    #[test]
    fn reversing_cancels_pending_travel_smoothly() {
        let mut s = Scroller::new(0.05);
        s.wheel(5.0, 3.0, 1000.0, 0.0);
        for _ in 0..3 {
            s.step(DT, 1000.0);
        }
        let p = s.pos;
        assert!(p > 0.0 && p < 15.0);
        s.wheel(-1.0, 3.0, 1000.0, 0.1);
        assert_eq!(s.pos, p, "the position does not jump");
        assert_eq!(s.target, p - 3.0, "the old destination is dropped");
        settle(&mut s, 1000.0);
        assert!((s.pos - (p - 3.0)).abs() < 1e-9);
    }

    #[test]
    fn touchpad_pixels_are_exact_and_unanimated() {
        let mut s = Scroller::new(0.05);
        s.pixels(7.0, 500.0, 0.0);
        s.pixels(5.0, 500.0, 0.016);
        assert_eq!(s.pos, 12.0);
        assert_eq!(s.target, 12.0);
        assert!(!s.step(DT, 500.0));
        s.pixels(-100.0, 500.0, 0.03);
        assert_eq!(s.pos, 0.0, "clamped");
    }

    #[test]
    fn fling_continues_and_decays_after_lift_off() {
        let mut s = Scroller::new(0.05);
        let mut t = 0.0;
        for _ in 0..8 {
            s.pixels(16.0, 100000.0, t); // ~1000 px/s
            t += 0.016;
        }
        let at_lift = s.pos;
        s.fling(t);
        let mut frames = 0;
        let mut last = s.pos;
        while s.step(DT, 100000.0) {
            assert!(s.pos >= last);
            last = s.pos;
            frames += 1;
            assert!(frames < 600);
        }
        assert!(s.pos > at_lift + 100.0, "coasted {}", s.pos - at_lift);
        assert!(frames > 15);
        // a slow lift-off (or one after a pause) does not fling
        let mut s = Scroller::new(0.05);
        s.pixels(1.0, 1000.0, 0.0);
        s.pixels(1.0, 1000.0, 0.1);
        let p = s.pos;
        s.fling(0.1);
        assert!(!s.step(DT, 1000.0));
        assert_eq!(s.pos, p);
    }

    #[test]
    fn fling_stops_at_the_edge() {
        let mut s = Scroller::new(0.05);
        let mut t = 0.0;
        for _ in 0..8 {
            s.pixels(30.0, 300.0, t);
            t += 0.016;
        }
        s.fling(t);
        settle(&mut s, 300.0);
        assert_eq!(s.pos, 300.0);
    }

    #[test]
    fn glide_is_short() {
        let mut s = Scroller::new(0.15);
        s.glide_to(500.0, 1000.0);
        let frames = settle(&mut s, 1000.0);
        assert_eq!(s.pos, 500.0);
        assert!(frames as f64 * DT <= 0.1, "glide took {} frames", frames);
        s.jump(2000.0, 1000.0);
        assert_eq!(s.pos, 1000.0);
        assert!(s.settled());
    }

    #[test]
    fn adopt_drops_the_animation_when_somebody_else_moved_the_container() {
        let mut s = Scroller::new(0.05);
        s.wheel(3.0, 3.0, 100.0, 0.0);
        s.step(DT, 100.0);
        let mine = s.pos;
        s.adopt(mine); // untouched: keeps animating
        assert_eq!(s.target, 9.0);
        s.adopt(42.0); // moved from outside
        assert_eq!((s.pos, s.target), (42.0, 42.0));
        assert!(s.settled());
        s.adopt_both(10.0, 20.0);
        assert_eq!((s.pos, s.target), (10.0, 20.0));
    }
}
