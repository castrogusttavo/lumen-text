//! Shared chrome of the viewers (image, diff, merge, coverage...): a segmented control, small
//! chips, a friendly empty state and the marching dashed outline of a drop target. Geometry
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
                self.fill_round(r, ds::R_CTRL, c.accent.with_alpha(0x40));
            } else if hover == Some(i) {
                self.fill_round(r, ds::R_CTRL, c.list_hover_bg);
            }
            let fg = if i == active { c.accent_text } else { c.tab_inactive_fg };
            tx.draw_ui(self, x + ds::S3, y + (h - 18) / 2, label, fg, r, i == active);
            out.push(r);
            x += w;
        }
        out
    }

    /// Small pill with `text` at (x, y); returns its width. For counters like `+12` / `-3`.
    pub fn chip(&mut self, tx: &mut TextCtx, (x, y): (i32, i32), text: &str, fg: Rgba, bg: Rgba) -> i32 {
        const SIZE: f32 = 11.0;
        let (w, h) = (tx.ui_width_sized(text, true, SIZE) + 2 * ds::S2, 18);
        let r = Rect::new(x, y, w, h);
        self.fill_round(r, ds::R_CTRL, bg);
        tx.draw_ui_sized(self, x + ds::S2, y + (h - 14) / 2, text, fg, r, true, SIZE);
        w
    }

    /// Centered empty state of a viewer: icon in a soft disc, one-line title, one-line hint.
    pub fn empty_state(&mut self, tx: &mut TextCtx, c: &Chrome, area: Rect, icon: &str, title: &str, hint: &str) {
        let disc = 2 * ds::ICON_L + ds::S2;
        let total = disc + ds::S3 + 20 + if hint.is_empty() { 0 } else { 20 };
        let top = area.y + (area.h - total).max(0) / 2;
        let d = Rect::new(area.x + (area.w - disc) / 2, top, disc, disc);
        self.fill_round(d, disc / 2, c.accent.with_alpha(0x1c));
        tx.icon(
            self,
            icon,
            d.x + (disc - ds::ICON_L) / 2,
            d.y + (disc - ds::ICON_L) / 2,
            ds::ICON_L,
            c.accent_text,
            area,
        );
        let ty = d.bottom() + ds::S3;
        let w = tx.ui_width(title, true);
        tx.draw_ui(self, area.x + (area.w - w) / 2, ty, title, c.tab_active_fg, area, true);
        if !hint.is_empty() {
            let w = tx.ui_width(hint, false);
            tx.draw_ui(self, area.x + (area.w - w) / 2, ty + 20, hint, c.tab_inactive_fg, area, false);
        }
    }
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
}
