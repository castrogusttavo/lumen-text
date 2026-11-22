//! Presentation state of one terminal grid: the soft cursor blink and the bell flash. It lives
//! in the `Term` (field `fx`) but never touches the emulator; `termview::draw` reads it while
//! drawing and `App::term_tick` uses it to wake the event loop only when the blink flips, so an
//! idle editor still sleeps. Motion comes from `anim.rs` and is driven by the real time between
//! frames (the grid is drawn through `&Term`, hence the interior mutability).

use crate::anim::{Anim, Tween, dur, ease_out_cubic};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// The cursor stays solid this long after the last key or output, so it never blinks away while
/// typing.
const SOLID_FOR: f32 = 0.5;
/// Length of one half of the blink (on, then off).
const HALF: f32 = 0.55;
/// Dimmest the cursor gets in the "off" half: it fades, it does not vanish.
const FLOOR: f32 = 0.18;
/// How often the event loop checks whether the blink flipped.
pub const POLL: std::time::Duration = std::time::Duration::from_millis(120);

/// Set by a draw that left something moving; the frame loop folds it into `busy`.
static MOVING: AtomicBool = AtomicBool::new(false);

/// True once if a terminal drawn since the last call is still animating (keeps redrawing).
pub fn take_moving() -> bool {
    MOVING.swap(false, Ordering::Relaxed)
}

/// Is the cursor in the lit half of its blink, `idle` seconds after the last activity?
pub fn blink_on(idle: f32) -> bool {
    idle < SOLID_FOR || (((idle - SOLID_FOR) / HALF) as u32).is_multiple_of(2)
}

/// What a draw needs from the effects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Cursor opacity 0..1 (1 solid).
    pub cursor: f32,
    /// Bell flash strength 0..1 (0 none).
    pub bell: f32,
}

struct State {
    last: Option<Instant>,
    cursor: Anim,
    bell: Tween,
    bells_seen: Option<u64>,
    /// Blink half shown at the last draw, and whether that terminal had the keyboard.
    on: bool,
    focused: bool,
}

pub struct Fx(Mutex<State>);

impl Default for Fx {
    fn default() -> Self {
        Fx(Mutex::new(State {
            last: None,
            cursor: Anim::new(1.0, dur::BASE),
            bell: Tween::new(dur::SLOW),
            bells_seen: None,
            on: true,
            focused: false,
        }))
    }
}

impl Fx {
    /// Advance to `now` and return what to draw. `bells` is the emulator's bell counter,
    /// `idle` the seconds since the last key or output, `focused` whether the terminal has the
    /// keyboard (an unfocused cursor is a steady hollow box).
    pub fn frame(&self, now: Instant, bells: u64, idle: f32, focused: bool) -> Frame {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let s = &mut *guard;
        let dt = s.last.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64().min(0.1));
        s.last = Some(now);
        if s.bells_seen.is_some_and(|b| b != bells) {
            s.bell.start();
        }
        s.bells_seen = Some(bells);
        s.focused = focused;
        s.on = !focused || blink_on(idle);
        s.cursor.to(if s.on { 1.0 } else { FLOOR });
        let mut moving = s.cursor.step(dt);
        moving |= s.bell.step(dt);
        if moving {
            MOVING.store(true, Ordering::Relaxed);
        }
        Frame {
            cursor: s.cursor.value.clamp(FLOOR, 1.0),
            bell: if s.bell.running() { 1.0 - s.bell.eased() } else { 0.0 },
        }
    }

    /// For the event loop: does this terminal's blink half differ from the one on screen, and does
    /// it blink at all (focused at its last draw)? `(needs_redraw, keeps_blinking)`.
    pub fn due(&self, idle: f32) -> (bool, bool) {
        let s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !s.focused {
            return (false, false);
        }
        (blink_on(idle) != s.on, true)
    }
}

/// Dim a selection color when its terminal does not have the keyboard.
pub fn inactive_selection(sel: crate::theme::Rgba) -> crate::theme::Rgba {
    crate::theme::Rgba(sel.0, sel.1, sel.2, (sel.3 as f32 * 0.6).round() as u8)
}

/// The flash color: the foreground at a faint alpha scaled by the bell strength.
pub fn bell_tint(fg: crate::theme::Rgba, k: f32) -> crate::theme::Rgba {
    crate::anim::fade(fg.with_alpha(0x2a), ease_out_cubic(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn blink_is_solid_while_active_then_alternates() {
        assert!(blink_on(0.0) && blink_on(0.4));
        assert!(blink_on(0.6), "first half after the grace period is lit");
        assert!(!blink_on(0.5 + HALF + 0.05));
        assert!(blink_on(0.5 + 2.0 * HALF + 0.05));
    }

    #[test]
    fn cursor_fades_between_halves_without_vanishing() {
        let fx = Fx::default();
        let t0 = Instant::now();
        assert_eq!(fx.frame(t0, 0, 0.0, true).cursor, 1.0);
        // Idle in the dark half: it fades toward the floor over a few frames.
        let mut t = t0;
        let mut last = 1.0;
        for _ in 0..60 {
            t += Duration::from_millis(16);
            last = fx.frame(t, 0, 0.5 + HALF + 0.1, true).cursor;
        }
        assert!(last < 0.3 && last >= FLOOR, "settles near the floor, got {last}");
        // Unfocused: steady and solid.
        let mut u = t;
        for _ in 0..60 {
            u += Duration::from_millis(16);
            last = fx.frame(u, 0, 5.0, false).cursor;
        }
        assert!(last > 0.99);
        assert_eq!(fx.due(5.0), (false, false), "an unfocused cursor asks for no wake-ups");
    }

    #[test]
    fn due_reports_a_flip_only_for_focused_terminals() {
        let fx = Fx::default();
        let t0 = Instant::now();
        fx.frame(t0, 0, 0.0, true);
        assert_eq!(fx.due(0.1), (false, true));
        assert_eq!(fx.due(0.5 + HALF + 0.05), (true, true));
    }

    #[test]
    fn bell_flashes_once_per_new_bell() {
        let fx = Fx::default();
        let t0 = Instant::now();
        assert_eq!(fx.frame(t0, 3, 0.0, false).bell, 0.0, "bells before the first draw do not flash");
        let f = fx.frame(t0 + Duration::from_millis(16), 4, 0.0, false);
        assert!(f.bell > 0.5, "a new bell starts the flash, got {}", f.bell);
        let mut t = t0;
        let mut b = 1.0;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            b = fx.frame(t, 4, 0.0, false).bell;
        }
        assert_eq!(b, 0.0, "the flash ends");
    }

    #[test]
    fn selection_dims_when_inactive() {
        let c = crate::theme::Rgba(10, 20, 30, 200);
        assert!(inactive_selection(c).3 < c.3);
    }
}
