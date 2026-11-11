//! THE scroll physics of the app (every scrollable surface goes through this module; the pattern is
//! the one of the Keyboard shortcuts modal): a mouse-wheel notch moves a fixed 3 rows (times the
//! `scroll_speed` setting) at once, linearly, with no acceleration and no animation; a touchpad
//! follows the fingers exactly (plus an optional fling when they lift); jumps (page, go to line,
//! reveal) take a short ease-out glide. Positions are clamped to `0..=max`, never overshoot.
//!
//! The conversion from winit's `MouseScrollDelta` is [`wheel_px`] / [`wheel_rows`] /
//! [`wheel_rows_int`]; a notch is the same 3 rows everywhere.
//!
//! Units are whatever the caller scrolls in (rows for the editor, pixels for the sidebar). Time is
//! passed in explicitly (`now`, `dt` in seconds), so everything is testable with simulated time.
//!
//! Integration pattern: the container keeps its plain `pos` field (so the rest of the code can read
//! and write it as before) and the `Scroller` mirrors it. `adopt` notices when somebody else moved
//! the container (reveal the caret, jump to the top, scrollbar drag) and drops the animation.

use winit::event::MouseScrollDelta;

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};

/// Time constant (s) of the exponential approach used when somebody moves `target` away from `pos`
/// (`adopt_both`, e.g. the editor's caret reveal). Wheel notches do not use it: they are immediate.
pub const WHEEL_TAU: f64 = 0.025;
/// Short glide for jumps (page, home/end, tree reveal): an ease-out of this many seconds.
pub const GLIDE_SECS: f64 = 0.08;
/// Rows per mouse-wheel notch (Zed: 3).
pub const WHEEL_ROWS: f64 = 3.0;
/// Touchpad fling: decay time constant (s) and the minimum lift-off velocity (units/s).
pub const FLING_TAU: f64 = 0.32;
pub const FLING_MIN: f64 = 180.0;
pub const FLING_CAP: f64 = 9000.0;

static SPEED: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0f32

/// User wheel-speed multiplier (`scroll_speed` setting; 1.0 = Zed-like 3 rows per notch).
pub fn speed() -> f64 {
    f32::from_bits(SPEED.load(Ordering::Relaxed)) as f64
}

/// Pixels one wheel notch scrolls on a surface whose rows are `row_px` tall: 3 rows x `scroll_speed`.
fn notch_px_at(row_px: f64, speed: f64) -> f64 {
    WHEEL_ROWS * row_px * speed
}

/// A wheel event as (dx, dy) in pixels, positive = right / down. Notches (`LineDelta`) are
/// 3 rows x `scroll_speed` each; touchpad pixels are exact (`scroll_speed` does not touch them).
pub fn wheel_px_xy(delta: MouseScrollDelta, row_px: f64) -> (f64, f64) {
    wheel_px_xy_at(delta, row_px, speed())
}

fn wheel_px_xy_at(delta: MouseScrollDelta, row_px: f64, speed: f64) -> (f64, f64) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            let n = notch_px_at(row_px, speed);
            (-(x as f64) * n, -(y as f64) * n)
        }
        MouseScrollDelta::PixelDelta(p) => (-p.x, -p.y),
    }
}

/// Vertical part of [`wheel_px_xy`].
pub fn wheel_px(delta: MouseScrollDelta, row_px: f64) -> f64 {
    wheel_px_xy(delta, row_px).1
}

/// Vertical wheel in (fractional) rows of `row_px`.
pub fn wheel_rows(delta: MouseScrollDelta, row_px: f64) -> f64 {
    wheel_px(delta, row_px) / row_px.max(1.0)
}

/// Vertical wheel in whole rows for lists that only scroll by rows: rounded, but any real movement
/// is at least one row.
pub fn wheel_rows_int(delta: MouseScrollDelta, row_px: f64) -> isize {
    let r = wheel_rows(delta, row_px);
    if r == 0.0 { 0 } else { (r.abs().round() as isize).max(1) * r.signum() as isize }
}

pub fn set_speed(v: f32) {
    SPEED.store(v.clamp(0.1, 5.0).to_bits(), Ordering::Relaxed);
}

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
        Self { pos: 0.0, target: 0.0, tau: WHEEL_TAU, eps, vel: 0.0, pvel: 0.0, last_px_t: -10.0, glide: None }
    }

    /// Follow a container that was moved from outside: if `pos` is not where this scroller left it,
    /// forget the animation and continue from there.
    /// Keep everything inside `0..=max` (content shrank, window resized).
    #[cfg(test)]
    pub fn clamp(&mut self, max: f64) {
        let max = max.max(0.0);
        self.pos = self.pos.clamp(0.0, max);
        self.target = self.target.clamp(0.0, max);
    }

    pub fn adopt(&mut self, pos: f64) {
        if (pos - self.pos).abs() > 1e-6 {
            self.pos = pos;
            self.target = pos;
            self.vel = 0.0;
            self.glide = None;
        }
    }

    /// Forget everything (animation, fling, pending travel) and sit at `pos`.
    pub fn reset(&mut self, pos: f64) {
        self.pos = pos;
        self.target = pos;
        self.vel = 0.0;
        self.glide = None;
        self.pvel = 0.0;
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

    /// Mouse wheel: `notches` (positive = forward in the positive direction) of `step` units each,
    /// times the `scroll_speed` setting. Immediate and linear (the Keyboard shortcuts modal feel):
    /// no glide, no acceleration; it stops any running glide or fling and continues from `pos`.
    /// `_now` is kept so callers feed every input kind the same clock.
    pub fn wheel(&mut self, notches: f64, step: f64, max: f64, _now: f64) {
        self.wheel_at(notches, step, max, speed());
    }

    fn wheel_at(&mut self, notches: f64, step: f64, max: f64, speed: f64) {
        if notches == 0.0 {
            return;
        }
        self.vel = 0.0;
        self.glide = None;
        self.pos = (self.pos + notches * step * speed).clamp(0.0, max.max(0.0));
        self.target = self.pos;
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
    fn wheel_is_immediate_linear_and_has_nothing_pending() {
        let mut s = Scroller::new(0.05);
        s.wheel(1.0, 60.0, 10_000.0, 0.0);
        assert_eq!((s.pos, s.target), (60.0, 60.0), "the notch lands at once");
        assert!(s.settled(), "no animation left, no redraw needed");
        assert!(!s.step(DT, 10_000.0));
        assert_eq!(s.pos, 60.0);
    }

    #[test]
    fn fast_spin_does_not_accelerate() {
        let mut slow = Scroller::new(0.05);
        let mut fast = Scroller::new(0.05);
        for i in 0..6 {
            slow.wheel(1.0, 3.0, 1000.0, i as f64 * 0.5);
            fast.wheel(1.0, 3.0, 1000.0, i as f64 * 0.01);
        }
        assert_eq!(slow.pos, 18.0);
        assert_eq!(fast.pos, 18.0, "same step however fast the notches come");
    }

    #[test]
    fn clamps_at_both_ends() {
        let mut s = Scroller::new(0.05);
        s.wheel(10.0, 3.0, 20.0, 0.0);
        assert_eq!(s.pos, 20.0);
        s.wheel(-100.0, 3.0, 20.0, 5.0);
        assert_eq!(s.pos, 0.0);
        s.wheel(1.0, 3.0, -5.0, 6.0);
        assert_eq!(s.pos, 0.0, "nothing to scroll");
        s.pos = 15.0;
        s.target = 15.0;
        s.clamp(5.0);
        assert_eq!((s.pos, s.target), (5.0, 5.0));
    }

    #[test]
    fn reversing_is_symmetric_and_stops_a_glide() {
        let mut s = Scroller::new(0.05);
        s.glide_to(500.0, 1000.0);
        s.step(DT, 1000.0);
        let p = s.pos;
        assert!(p > 0.0 && p < 500.0);
        s.wheel(-1.0, 3.0, 1000.0, 0.1);
        assert_eq!(s.pos, p - 3.0, "continues from where the glide was");
        assert!(s.settled());
        s.wheel(1.0, 3.0, 1000.0, 0.2);
        assert_eq!(s.pos, p);
    }

    #[test]
    fn scroll_speed_scales_notches_not_touchpad_pixels() {
        let mut s = Scroller::new(0.05);
        s.wheel_at(1.0, 10.0, 1000.0, 2.0);
        assert_eq!(s.pos, 20.0);
        s.pixels(7.0, 1000.0, 1.0);
        assert_eq!(s.pos, 27.0);
        assert_eq!(notch_px_at(20.0, 2.0), 120.0);
        use winit::dpi::PhysicalPosition;
        let px = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -7.0));
        assert_eq!(wheel_px_xy_at(px, 20.0, 2.0), (0.0, 7.0), "touchpads ignore scroll_speed");
    }

    #[test]
    fn delta_conversion_is_three_rows_per_notch_and_exact_pixels() {
        use winit::dpi::PhysicalPosition;
        assert_eq!(speed(), 1.0);
        assert_eq!(wheel_px(MouseScrollDelta::LineDelta(0.0, -1.0), 34.0), 102.0);
        assert_eq!(wheel_px(MouseScrollDelta::LineDelta(0.0, 2.0), 20.0), -120.0);
        assert_eq!(wheel_px(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -7.5)), 34.0), 7.5);
        assert_eq!(wheel_px_xy(MouseScrollDelta::LineDelta(1.0, 0.0), 20.0), (-60.0, 0.0));
        assert_eq!(wheel_rows(MouseScrollDelta::LineDelta(0.0, -1.0), 34.0), 3.0);
        assert_eq!(wheel_rows_int(MouseScrollDelta::LineDelta(0.0, -1.0), 34.0), 3);
        assert_eq!(wheel_rows_int(MouseScrollDelta::LineDelta(0.0, 1.0), 34.0), -3);
        // a tiny touchpad movement still steps one row in whole-row lists
        assert_eq!(wheel_rows_int(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -2.0)), 34.0), 1);
        assert_eq!(wheel_rows_int(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.0)), 34.0), 0);
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
        s.glide_to(9.0, 100.0);
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

    #[test]
    fn notch_is_three_rows() {
        // One X11/Wayland notch (LineDelta 1) = 3 rows of 20 px, at once.
        let mut s = Scroller::new(0.15);
        s.wheel(1.0, WHEEL_ROWS * 20.0, 10_000.0, 0.0);
        assert_eq!(s.pos, 60.0);
    }

    #[test]
    fn touchpad_pixels_are_exact() {
        let mut s = Scroller::new(0.15);
        s.pixels(7.0, 1000.0, 0.0);
        assert_eq!(s.pos, 7.0);
        assert_eq!(speed(), 1.0);
    }

    /// Every scrollable surface uses the shared physics: no file outside the allow-list converts a
    /// raw `MouseScrollDelta::LineDelta` itself, and each surface's wheel path calls the shared
    /// functions.
    #[test]
    fn every_surface_uses_the_shared_scroll_physics() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // Only these may look inside a LineDelta: the shared converter, `App::wheel_in` (notches for
        // the Scroller), and the script/HiDPI plumbing that builds deltas.
        let allowed = ["scroller.rs", "app_input.rs", "app.rs", "script.rs"];
        // Every .rs file under src/, directory modules included (`name` = path relative to src/).
        fn walk(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, std::path::PathBuf)>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let path = e.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                let rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                if path.is_dir() {
                    walk(&path, &rel, out);
                } else if name.ends_with(".rs") {
                    out.push((rel, path));
                }
            }
        }
        let mut files = Vec::new();
        walk(&dir, "", &mut files);
        for (name, path) in &files {
            // `app.rs` may be a directory module: `app/mod.rs` and its children count as `app.rs`.
            let top = name.split('/').next().unwrap();
            let module = if name.contains('/') { format!("{top}.rs") } else { name.clone() };
            if allowed.contains(&module.as_str()) {
                continue;
            }
            let src = std::fs::read_to_string(path).unwrap();
            let code = src.split("#[cfg(test)]").next().unwrap();
            assert!(!code.contains("LineDelta("), "{name} converts wheel notches itself; use scroller::wheel_*");
        }
        // A surface may live in `file` or, once split, in the directory module of the same name.
        let source_of = |file: &str| -> String {
            let stem = file.trim_end_matches(".rs");
            files
                .iter()
                .filter(|(n, _)| n == file || n.starts_with(&format!("{stem}/")))
                .map(|(_, p)| std::fs::read_to_string(p).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        };
        // (file, what its wheel path must call)
        let surfaces = [
            ("doc_scroll.rs", "scr.v.wheel("),          // editor, vertical and horizontal
            ("app_input.rs", "tree_v.wheel("),          // explorer / outline
            ("app_input.rs", "panel_v.wheel("),         // output panel fallback
            ("app_input.rs", "scroller::wheel_px_xy"),  // tab strip
            ("app_hover.rs", "h.scr.wheel("),           // hover popup
            ("scrollreg.rs", "sc.wheel("), // modal, git, diff, md, picker, menus, completion, terminal, debug, output
            ("app_settings.rs", "scroller::wheel_px("), // settings / shortcuts modal outside its list
            ("app_md.rs", "scroller::wheel_px("),
            ("app_gitpanel.rs", "scroller::wheel_px("),
            ("app_gitx.rs", "scroller::wheel_rows("),
            ("app_topbar.rs", "scroller::wheel_rows_int("),
            ("app_overlay.rs", "scroller::wheel_rows_int("),
            ("app_termio.rs", "scroller::wheel_rows_int("), // both terminals (bottom panel, AI panel)
        ];
        for (file, needle) in surfaces {
            let src = source_of(file);
            assert!(src.contains(needle), "{file} must scroll through `{needle}`");
        }
    }
}
