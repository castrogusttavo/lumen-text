//! Wheel scrolling of a document, vertical and horizontal, on top of `scroller::Scroller`.
//! `Doc::top/target/left/left_target` stay the source of truth for the rest of the code; the
//! scrollers mirror them (see the notes in scroller.rs).

use crate::{
    doc::Doc,
    editor::visual_col,
    scroller::{WHEEL_ROWS, Scroller},
    ui::line_h,
};

/// Pixels per horizontal wheel notch.
pub const H_STEP: f64 = 64.0;

/// A wheel / touchpad event reduced to what scrolling needs. Positive = scroll down / right.
/// `pixels`: exact touchpad deltas (logical px); otherwise they are notches.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelIn {
    pub dx: f64,
    pub dy: f64,
    pub pixels: bool,
}

/// Lines around the viewport that count when looking for the widest line.
const WIDE_WINDOW: usize = 800;

/// Scroll state kept per document (cheap: two small structs).
#[derive(Default)]
pub struct DocScroll {
    pub v: Scroller,
    pub h: Scroller,
    /// (version, first line, end line, widest visual column, tab width) of the last scan.
    pub wide: std::cell::Cell<(i32, usize, usize, usize, usize)>,
    /// The next reveal that moves the view glides (80 ms) instead of jumping (Ctrl+Home/End).
    pub glide_next: bool,
}

impl Doc {
    /// Width of the text area in px (what `apply_reveal` uses too).
    pub fn text_view_w(&self) -> f64 {
        let cw = self.cell_w as f64;
        (self.rect.w as f64 - self.gutter_w() as f64 - 12.0).max(cw * 4.0)
    }

    /// Widest line (visual columns, tabs expanded) near the viewport, or under the caret. Cached by
    /// document version and scanned window, so scrolling and typing do not rescan the file.
    pub fn widest_cols(&self) -> usize {
        let n = self.buf.line_count();
        let (first, _) = self.top_pos();
        let vis = self.visible_lines() as usize + 2;
        let (ver, lo, hi, cols, tab_seen) = self.scr.wide.get();
        let cached = ver == self.version && tab_seen == self.ed.tab_w && lo <= first && (first + vis <= hi || hi >= n);
        let cols = if cached {
            cols
        } else {
            let lo = first.saturating_sub(WIDE_WINDOW);
            let hi = (first + vis + WIDE_WINDOW).min(n);
            let tab = self.ed.tab_w;
            // Giant (minified) lines: the char count stands in for the visual width (no tab scan).
            let width = |l: usize| {
                let n = self.buf.line_len(l);
                if n > 4000 { n } else { visual_col(&self.buf.line(l), usize::MAX, tab) + self.inlay.hints.extra_before(l, usize::MAX, true) }
            };
            let cols = (lo..hi).map(width).max().unwrap_or(0);
            self.scr.wide.set((self.version, lo, hi, cols, tab));
            cols
        };
        cols.max(self.caret_geom(self.ed.head()).1)
    }

    /// Largest horizontal scroll in px; 0 with word wrap or when every line fits.
    pub fn max_left(&self) -> f64 {
        if self.view.cols > 0 {
            return 0.0;
        }
        let cw = self.cell_w as f64;
        ((self.widest_cols() as f64 + 1.0) * cw + cw * 4.0 - self.text_view_w()).max(0.0)
    }

    fn sync_scroll_flags(&mut self) {
        self.animating = !self.scr.v.settled() || !self.scr.h.settled();
    }

    fn adopt_v(&mut self) -> (f64, f64) {
        let lh = line_h() as f64;
        self.scr.v.eps = 0.5;
        self.scr.v.adopt_both(self.top * lh, self.target * lh);
        (lh, self.max_top() * lh)
    }

    fn adopt_h(&mut self) -> f64 {
        self.scr.h.eps = 0.3;
        self.scr.h.adopt_both(self.left, self.left_target);
        self.max_left()
    }

    fn pull_v(&mut self, lh: f64) {
        self.top = self.scr.v.pos / lh;
        self.target = self.scr.v.target / lh;
    }

    fn pull_h(&mut self) {
        self.left = self.scr.h.pos;
        self.left_target = self.scr.h.target;
    }

    /// Apply a wheel event. Vertical: 3 rows per notch (accelerating on fast spins) or exact pixels;
    /// horizontal only without word wrap.
    pub fn wheel(&mut self, w: WheelIn, now: f64) {
        if w.dy != 0.0 {
            let (lh, max) = self.adopt_v();
            if w.pixels {
                self.scr.v.pixels(w.dy, max, now);
            } else {
                self.scr.v.wheel(w.dy, WHEEL_ROWS * lh, max, now);
            }
            self.pull_v(lh);
        }
        if w.dx != 0.0 && self.view.cols == 0 {
            let max = self.adopt_h();
            if w.pixels {
                self.scr.h.pixels(w.dx, max, now);
            } else {
                self.scr.h.wheel(w.dx, H_STEP, max, now);
            }
            self.pull_h();
        }
        self.sync_scroll_flags();
    }

    /// Fingers left the touchpad: coast with the last velocity.
    pub fn wheel_end(&mut self, now: f64) {
        let (lh, _) = self.adopt_v();
        self.scr.v.fling(now);
        self.pull_v(lh);
        self.adopt_h();
        self.scr.h.fling(now);
        self.pull_h();
        self.sync_scroll_flags();
    }

    /// Advance the scroll animations by `dt` s (called by `draw`). True while moving.
    pub fn step_scroll(&mut self, dt: f64) -> bool {
        let (lh, max_v) = self.adopt_v();
        let mv = self.scr.v.step(dt, max_v);
        self.pull_v(lh);
        let max_h = self.adopt_h();
        if self.left > max_h + 0.5 {
            // The content got narrower (edit, other part of the file): stay inside it.
            self.scr.h.jump(max_h, max_h);
            self.pull_h();
        }
        let mh = self.scr.h.step(dt, max_h);
        self.pull_h();
        self.animating = mv || mh;
        self.animating
    }

    /// Scroll by `rows` with the short glide (PageUp / PageDown).
    pub fn glide_by(&mut self, rows: f64) {
        let (lh, max) = self.adopt_v();
        self.scr.v.glide_to(self.target * lh + rows * lh, max);
        self.pull_v(lh);
        self.sync_scroll_flags();
    }

    /// Called by `apply_reveal` when it moved `target` and a glide was requested: keep `top` where
    /// it is and glide to the new target.
    pub fn start_reveal_glide(&mut self) {
        let (lh, max) = self.adopt_v();
        self.scr.v.glide_to(self.target * lh, max);
        self.pull_v(lh);
        self.sync_scroll_flags();
    }

    /// Instant horizontal move (scrollbar drag).
    pub fn scroll_left_to(&mut self, x: f64) {
        let x = x.clamp(0.0, self.max_left());
        self.left = x;
        self.left_target = x;
        self.sync_scroll_flags();
    }

    /// Drag the horizontal bar: pointer at `x`, grabbed `grab` px right of the thumb's left edge.
    pub fn hbar_drag(&mut self, x: f64, grab: f64) {
        if let Some(b) = self.hb {
            let vw = self.text_view_w();
            let pos = b.pos_for(x, grab, vw + self.max_left(), vw);
            self.scroll_left_to(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::ui::Rect;

    fn doc(text: &str) -> Doc {
        let mut d = Doc::new(Buffer::from_str(text), None, None, "//", 8.0);
        d.rect = Rect::new(0, 0, 400, 200);
        d
    }

    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    fn settle(d: &mut Doc) -> usize {
        let mut n = 0;
        while d.step_scroll(1.0 / 60.0) && n < 1000 {
            n += 1;
        }
        n
    }

    #[test]
    fn a_notch_scrolls_three_rows_with_a_short_glide() {
        let mut d = doc(&lines(500));
        d.wheel(WheelIn { dx: 0.0, dy: 1.0, pixels: false }, 0.0);
        assert!((d.target - 3.0).abs() < 1e-9);
        assert!(d.animating);
        let frames = settle(&mut d);
        assert!(frames <= 14, "frames = {frames}");
        assert_eq!(d.top, 3.0);
        assert!(!d.animating);
        d.wheel(WheelIn { dx: 0.0, dy: -1.0, pixels: false }, 1.0);
        settle(&mut d);
        assert_eq!(d.top, 0.0);
    }

    #[test]
    fn fast_spin_covers_more_than_slow_notches() {
        let mut slow = doc(&lines(2000));
        let mut fast = doc(&lines(2000));
        for i in 0..8 {
            slow.wheel(WheelIn { dx: 0.0, dy: 1.0, pixels: false }, i as f64);
            fast.wheel(WheelIn { dx: 0.0, dy: 1.0, pixels: false }, i as f64 * 0.04);
        }
        assert_eq!(slow.target, 24.0);
        assert!(fast.target > 40.0, "{}", fast.target);
        settle(&mut fast);
        assert_eq!(fast.top, fast.target);
    }

    #[test]
    fn touchpad_pixels_move_exactly_without_animation() {
        let mut d = doc(&lines(500));
        let lh = line_h() as f64;
        d.wheel(WheelIn { dx: 0.0, dy: lh * 2.5, pixels: true }, 0.0);
        assert!((d.top - 2.5).abs() < 1e-9);
        assert!(!d.animating);
    }

    #[test]
    fn vertical_clamps_to_the_document() {
        let mut d = doc(&lines(40));
        d.wheel(WheelIn { dx: 0.0, dy: 100.0, pixels: false }, 0.0);
        assert_eq!(d.target, d.max_top());
        settle(&mut d);
        assert_eq!(d.top, d.max_top());
        d.wheel(WheelIn { dx: 0.0, dy: -1000.0, pixels: false }, 5.0);
        settle(&mut d);
        assert_eq!(d.top, 0.0);
    }

    fn wide_doc() -> Doc {
        let long = "x".repeat(300);
        let mut d = doc(&format!("short\n{long}\nshort\n"));
        d.cell_w = 8.0;
        d
    }

    #[test]
    fn horizontal_scroll_clamps_to_the_longest_line() {
        let mut d = wide_doc();
        let max = d.max_left();
        // 300 cols + caret + margin - view width
        let want = 301.0 * 8.0 + 32.0 - d.text_view_w();
        assert!((max - want).abs() < 1e-6, "{max} vs {want}");
        d.wheel(WheelIn { dx: 1000.0, dy: 0.0, pixels: false }, 0.0);
        assert_eq!(d.left_target, max);
        settle(&mut d);
        assert_eq!(d.left, max);
        d.wheel(WheelIn { dx: -1000.0, dy: 0.0, pixels: false }, 3.0);
        settle(&mut d);
        assert_eq!(d.left, 0.0);
        d.wheel(WheelIn { dx: 123.0, dy: 0.0, pixels: true }, 4.0);
        assert_eq!(d.left, 123.0);
        assert!(!d.animating);
    }

    #[test]
    fn short_documents_and_word_wrap_do_not_scroll_sideways() {
        let mut d = doc(&lines(10));
        assert_eq!(d.max_left(), 0.0);
        d.wheel(WheelIn { dx: 5.0, dy: 0.0, pixels: false }, 0.0);
        assert_eq!(d.left, 0.0);
        let mut w = wide_doc();
        w.view.cols = 40;
        assert_eq!(w.max_left(), 0.0);
        w.wheel(WheelIn { dx: 5.0, dy: 0.0, pixels: false }, 0.0);
        assert_eq!(w.left, 0.0);
    }

    #[test]
    fn typing_keeps_the_caret_visible_and_inside_the_range() {
        let mut d = wide_doc();
        let p = d.buf.pos(1, 300);
        d.ed.click(&mut d.buf, p, false);
        d.apply_reveal();
        assert!(d.left > 0.0, "the caret at col 300 forces a horizontal scroll");
        let caret_x = 300.0 * 8.0;
        assert!(caret_x >= d.left && caret_x + 8.0 <= d.left + d.text_view_w());
        // the range allows exactly that position (no clamping fight)
        assert!(d.left <= d.max_left() + 1e-6);
        let before = d.left;
        settle(&mut d);
        assert_eq!(d.left, before);
        // the widest-line cache follows edits
        d.ed.insert_text(&mut d.buf, &"y".repeat(100));
        d.sync_edits();
        d.apply_reveal();
        assert!(d.max_left() >= 400.0 * 8.0 - d.text_view_w());
    }

    #[test]
    fn changing_the_tab_width_re_measures_the_widest_line() {
        // Ten tabs: 40 columns at width 4, 80 at width 8 (no edit in between).
        let mut d = doc("short\n\t\t\t\t\t\t\t\t\t\tx\nshort\n");
        d.ed.tab_w = 4;
        assert_eq!(d.widest_cols(), 41);
        d.ed.tab_w = 8;
        assert_eq!(d.widest_cols(), 81, "the cached width of the old tab width is not reused");
        d.ed.tab_w = 2;
        assert_eq!(d.widest_cols(), 21);
    }

    #[test]
    fn reveal_moving_the_view_cancels_a_running_glide() {
        let mut d = doc(&lines(500));
        d.wheel(WheelIn { dx: 0.0, dy: 10.0, pixels: false }, 0.0);
        assert!(d.animating);
        let p = d.buf.pos(100, 0);
        d.ed.click(&mut d.buf, p, false);
        d.apply_reveal();
        let t = d.target;
        settle(&mut d);
        assert_eq!(d.top, t);
        assert!(d.top >= 100.0 - d.visible_lines());
    }

    #[test]
    fn page_and_jump_glides_are_short() {
        let mut d = doc(&lines(2000));
        let page = d.visible_lines();
        d.glide_by(page);
        assert_eq!(d.target, page);
        assert_eq!(d.top, 0.0, "the glide has not moved yet");
        let frames = settle(&mut d);
        assert!(frames <= 6, "page glide = {frames} frames");
        assert_eq!(d.top, page);
        // Ctrl+End style: caret jumps far away, the view glides there in ~80 ms
        d.scr.glide_next = true;
        let p = d.buf.pos(1900, 0);
        d.ed.click(&mut d.buf, p, false);
        d.apply_reveal();
        assert!(d.animating);
        assert_eq!(d.top, page, "still at the old position");
        let frames = settle(&mut d);
        assert!(frames <= 6, "jump glide = {frames} frames");
        assert_eq!(d.top, d.target);
        assert!(d.top > 1800.0);
        // without the flag the reveal snaps, as before
        let p = d.buf.pos(10, 0);
        d.ed.click(&mut d.buf, p, false);
        d.apply_reveal();
        assert_eq!(d.top, d.target);
        assert!(!d.animating);
    }

    #[test]
    fn hbar_drag_maps_to_left() {
        let mut d = wide_doc();
        let vw = d.text_view_w();
        let ml = d.max_left();
        let track = Rect::new(40, 188, 340, 12);
        d.hb = crate::ui::HBar::new(track, vw + ml, vw, 0.0);
        let b = d.hb.unwrap();
        d.hbar_drag(track.right() as f64, (b.thumb.w / 2) as f64);
        assert!((d.left - ml).abs() < 1e-6);
        d.hbar_drag(0.0, 0.0);
        assert_eq!(d.left, 0.0);
    }
}
