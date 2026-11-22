//! Loading skeletons that shimmer, and the shared animation clock. Callers fold "is something
//! animating" into their `busy` flag; with "reduce motion" on the bars stay still.

use crate::{
    anim, ds, motion,
    theme::{Rgba, Theme},
    ui::{Canvas, Rect},
};
use std::{sync::OnceLock, time::Instant};

/// Seconds one shimmer wave takes to travel through a skeleton.
const SWEEP_SECS: f32 = 1.4;
/// Phase shift per pixel of height: bars lower down light up slightly later (a wave, not a blink).
const WAVE_PER_PX: f32 = 0.003;
/// Height of a skeleton bar and the gap to the next one.
pub const BAR_H: i32 = 10;
pub const BAR_GAP: i32 = 12;

/// Seconds since the first call: the clock of every shimmer and pulse (frames only differ by `dt`).
pub fn clock() -> f32 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// Shimmer amount 0..1 of a bar at height `y` at time `t`; a constant midpoint when motion is reduced.
pub fn skeleton_k(t: f32, y: i32) -> f32 {
    if motion::reduced() {
        return 0.5;
    }
    wave_k(t, y)
}

fn wave_k(t: f32, y: i32) -> f32 {
    let phase = (t / SWEEP_SECS - y as f32 * WAVE_PER_PX).rem_euclid(1.0);
    0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()
}

/// Colors a skeleton breathes between (visible on both light and dark cards).
fn skeleton_colors(th: &Theme) -> (Rgba, Rgba) {
    let c = th.chrome;
    (th.background.mix(c.widget_bg, 0.7), c.widget_bg.mix(th.foreground, 0.10))
}

/// `n` widths (fractions of `w`) of the lines a skeleton paragraph uses: the last one is short.
pub fn line_widths(n: usize, w: i32) -> Vec<i32> {
    (0..n)
        .map(|i| {
            let frac = if i + 1 == n && n > 1 { 0.55 } else { 1.0 - (i % 3) as f32 * 0.12 };
            ((w as f32 * frac).round() as i32).max(ds::HIT_MIN)
        })
        .collect()
}

impl Canvas<'_> {
    /// One shimmering placeholder bar.
    pub fn skeleton_bar(&mut self, th: &Theme, r: Rect, clip: Rect, t: f32) {
        let (a, b) = skeleton_colors(th);
        let col = anim::lerp_rgba(a, b, skeleton_k(t, r.y));
        self.fill_round_clip(r, ds::R_CTRL.min(r.h / 2).max(1), col, clip);
    }

    /// A stack of `n` placeholder lines filling the top of `area` (its height is ignored);
    /// returns the height used.
    pub fn skeleton_lines(&mut self, th: &Theme, area: Rect, n: usize, clip: Rect, t: f32) -> i32 {
        for (i, lw) in line_widths(n, area.w).into_iter().enumerate() {
            let r = Rect::new(area.x, area.y + i as i32 * (BAR_H + BAR_GAP), lw, BAR_H);
            self.skeleton_bar(th, r, clip, t);
        }
        n as i32 * (BAR_H + BAR_GAP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shimmer_stays_in_range_and_moves() {
        let mut seen = Vec::new();
        for i in 0..40 {
            let k = wave_k(i as f32 * 0.05, 100);
            assert!((0.0..=1.0).contains(&k), "k={k}");
            seen.push(k);
        }
        let (lo, hi) = seen.iter().fold((1.0f32, 0.0f32), |(l, h), &k| (l.min(k), h.max(k)));
        assert!(hi - lo > 0.9, "a full wave goes through the clock");
    }

    #[test]
    fn lower_bars_are_a_little_behind() {
        assert!((wave_k(0.5, 0) - wave_k(0.5, 200)).abs() > 0.01);
    }

    #[test]
    fn skeleton_lines_end_short() {
        let w = line_widths(3, 300);
        assert_eq!(w.len(), 3);
        assert!(w[2] < w[0] && w[2] >= ds::HIT_MIN);
        assert_eq!(line_widths(1, 300), vec![300]);
        assert!(line_widths(0, 300).is_empty());
    }
}
