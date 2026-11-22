//! Shared chrome of the viewers (image, diff, merge, coverage...): a segmented control, the
//! marching dashed outline of a drop target and coverage levels (chips and empty states live in
//! `ui_kit`). Geometry
//! comes from `ds`, colors from the theme chrome, so every viewer wears the same clothes.

use crate::{
    ds,
    theme::{Chrome, Rgba},
    ui::{Canvas, Rect, TextCtx},
};

/// Spans `(start, end)` of the dashes of a dashed line `len` long: dashes `dash` long, `gap`
/// apart, shifted forward by `phase` (any value; it wraps). Spans are clipped to `0..len`.
pub fn dash_spans(len: i32, dash: i32, gap: i32, phase: i32) -> Vec<(i32, i32)> {
    let period = (dash + gap).max(1);
    let mut out = Vec::new();
    let mut s = -phase.rem_euclid(period);
    while s < len {
        let (a, b) = (s.max(0), (s + dash).min(len));
        if b > a {
            out.push((a, b));
        }
        s += period;
    }
    out
}

/// Seconds since the first call, as a dash phase in logical px (`speed` px per second).
pub fn march_phase(speed: f32) -> i32 {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let t = T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f32();
    (t * speed) as i32
}

impl Canvas<'_> {
    /// Dashed outline that marches with `phase`: a faint solid ring keeps the rounded corners,
    /// the dashes run along the straight edges.
    pub fn dashed_ring(&mut self, r: Rect, rad: i32, c: Rgba, phase: i32) {
        const T: i32 = 2;
        self.ring(r, rad, c.with_alpha(c.3 / 4));
        let (len_x, len_y) = ((r.w - 2 * rad).max(0), (r.h - 2 * rad).max(0));
        for (a, b) in dash_spans(len_x, ds::S3, ds::S2 / 2, phase) {
            self.fill(Rect::new(r.x + rad + a, r.y, b - a, T), c);
            self.fill(Rect::new(r.x + rad + len_x - b, r.bottom() - T, b - a, T), c);
        }
        for (a, b) in dash_spans(len_y, ds::S3, ds::S2 / 2, phase) {
            self.fill(Rect::new(r.right() - T, r.y + rad + a, T, b - a), c);
            self.fill(Rect::new(r.x, r.y + rad + len_y - b, T, b - a), c);
        }
    }

    /// Segmented control (a pill with one lit segment), right-aligned at `right`, top at `y`.
    /// Returns the rect of every segment, in order, for hit testing.
    pub fn seg_control(
        &mut self,
        tx: &mut TextCtx,
        c: &Chrome,
        (right, y): (i32, i32),
        labels: &[&str],
        active: usize,
        hover: Option<usize>,
    ) -> Vec<Rect> {
        let h = ds::HIT_MIN;
        let widths: Vec<i32> = labels.iter().map(|l| tx.ui_width(l, false) + 2 * ds::S3).collect();
        let total: i32 = widths.iter().sum();
        let mut x = right - total;
        let frame = Rect::new(x - 3, y - 3, total + 6, h + 6);
        self.card(frame, ds::R_CTRL + 3, c.widget_bg, c.border);
        let mut out = Vec::with_capacity(labels.len());
        for (i, (label, w)) in labels.iter().zip(widths).enumerate() {
            let r = Rect::new(x, y, w, h);
            if i == active {
                self.fill_round(r, ds::R_CTRL, c.selection_accent);
            } else if hover == Some(i) {
                self.fill_round(r, ds::R_CTRL, c.hover_overlay);
            }
            let fg = if i == active { c.accent_text } else { c.tab_inactive_fg };
            tx.draw_ui(self, x + ds::S3, y + (h - 18) / 2, label, fg, r, i == active);
            out.push(r);
            x += w;
        }
        out
    }
}

/// How well covered a file is: green from 80 %, yellow from 50 %, red below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Good,
    Fair,
    Poor,
}

impl Level {
    pub fn of(pct: f32) -> Level {
        if pct >= 80.0 {
            Level::Good
        } else if pct >= 50.0 {
            Level::Fair
        } else {
            Level::Poor
        }
    }
}

/// "3 results" / "1 result".
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashes_tile_the_line_and_march() {
        assert_eq!(dash_spans(20, 6, 4, 0), vec![(0, 6), (10, 16)]);
        // Shifted by 3: the first dash is half gone, a new one starts later.
        assert_eq!(dash_spans(20, 6, 4, 3), vec![(0, 3), (7, 13), (17, 20)]);
        // The phase wraps around the period.
        assert_eq!(dash_spans(20, 6, 4, 13), dash_spans(20, 6, 4, 3));
        assert!(dash_spans(0, 6, 4, 0).is_empty());
    }

    #[test]
    fn coverage_levels_follow_the_thresholds() {
        assert_eq!(Level::of(100.0), Level::Good);
        assert_eq!(Level::of(80.0), Level::Good);
        assert_eq!(Level::of(79.9), Level::Fair);
        assert_eq!(Level::of(50.0), Level::Fair);
        assert_eq!(Level::of(49.9), Level::Poor);
        assert_eq!(Level::of(0.0), Level::Poor);
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "result", "results"), "1 result");
        assert_eq!(plural(0, "result", "results"), "0 results");
        assert_eq!(plural(12, "conflict", "conflicts"), "12 conflicts");
    }
}
