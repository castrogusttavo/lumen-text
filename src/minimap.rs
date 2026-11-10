#![allow(dead_code)]
//! Code minimap: scaled-down overview of the file plus pointer math.

use crate::theme::Rgba;
use crate::ui::{Canvas, Rect};
use std::sync::atomic::{AtomicBool, Ordering};

/// How loud the minimap is (`minimap_style` in config.json).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// The original look: 92 px wide, 25 % bars, solid 2 px rows.
    Normal,
    /// Default: narrower (64 px), 14 % bars as 1 px lines and a fainter viewport band.
    Subtle,
}

impl Style {
    pub fn from_name(s: &str) -> Style {
        if s == "normal" { Style::Normal } else { Style::Subtle }
    }

    pub fn name(self) -> &'static str {
        match self {
            Style::Normal => "normal",
            Style::Subtle => "subtle",
        }
    }

    /// Strip width in px.
    pub fn width(self) -> i32 {
        match self {
            Style::Normal => 92,
            Style::Subtle => 64,
        }
    }

    /// Alpha of the code bars.
    fn bar_alpha(self) -> u8 {
        match self {
            Style::Normal => 64,
            Style::Subtle => 36,
        }
    }

    /// Alpha of the band that marks the visible lines.
    fn viewport_alpha(self) -> u8 {
        match self {
            Style::Normal => 22,
            Style::Subtle => 14,
        }
    }
}

static SUBTLE: AtomicBool = AtomicBool::new(true);

/// The style every editor draws with (set from the config at start-up and by the settings modal).
pub fn style() -> Style {
    if SUBTLE.load(Ordering::Relaxed) { Style::Subtle } else { Style::Normal }
}

pub fn set_style(s: Style) {
    SUBTLE.store(s == Style::Subtle, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineInfo {
    pub indent_cols: u16,
    pub len_cols: u16,
    pub color: Rgba,
    pub blank: bool,
}

/// Spans of a whole line (byte ranges) restricted to the row that covers bytes `b0..b1` of it,
/// re-based to the row's own text (continuation rows of a wrapped line).
pub fn row_spans(spans: impl Iterator<Item = (usize, usize, Rgba)>, b0: usize, b1: usize) -> Vec<(usize, usize, Rgba)> {
    spans.filter(|&(s, e, _)| e > b0 && s < b1).map(|(s, e, c)| (s.max(b0) - b0, e.min(b1) - b0, c)).collect()
}

pub fn line_info(text: &str, spans: Option<&[(usize, usize, Rgba)]>, fg: Rgba, tab_w: usize) -> LineInfo {
    let tab_w = tab_w.max(1);
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return LineInfo { indent_cols: 0, len_cols: 0, color: fg, blank: true };
    }
    let mut col = 0usize;
    let mut indent = 0usize;
    let mut in_indent = true;
    for ch in trimmed.chars() {
        if in_indent && (ch == ' ' || ch == '\t') {
            col = if ch == '\t' { (col / tab_w + 1) * tab_w } else { col + 1 };
            indent = col;
            continue;
        }
        in_indent = false;
        col = if ch == '\t' { (col / tab_w + 1) * tab_w } else { col + 1 };
    }
    let bytes = trimmed.as_bytes();
    let mut total = 0usize;
    for &b in bytes {
        if !b.is_ascii_whitespace() {
            total += 1;
        }
    }
    let mut tally: Vec<(Rgba, usize)> = Vec::new();
    let mut spanned = 0usize;
    if let Some(spans) = spans {
        for &(s, e, c) in spans {
            let e = e.min(bytes.len());
            if s >= e {
                continue;
            }
            let n = bytes[s..e].iter().filter(|b| !b.is_ascii_whitespace()).count();
            if n == 0 {
                continue;
            }
            spanned += n;
            match tally.iter_mut().find(|(k, _)| *k == c) {
                Some(t) => t.1 += n,
                None => tally.push((c, n)),
            }
        }
    }
    let rest = total.saturating_sub(spanned);
    if rest > 0 {
        match tally.iter_mut().find(|(k, _)| *k == fg) {
            Some(t) => t.1 += rest,
            None => tally.push((fg, rest)),
        }
    }
    let color = tally.iter().fold((fg, 0usize), |b, &(c, n)| if n > b.1 { (c, n) } else { b }).0;
    LineInfo {
        indent_cols: indent.min(u16::MAX as usize) as u16,
        len_cols: col.min(u16::MAX as usize) as u16,
        color,
        blank: false,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub rect: Rect,
    pub line_px: f32,
    pub char_px: f32,
    pub max_cols: usize,
}

/// Minimap on the right edge of `editor`. `line_px` is 2.0 if the file fits,
/// else exactly rect.h / line_count (so the whole file always fits; for very
/// long files several lines share one pixel row and `draw` samples them).
pub fn layout_for(editor: Rect, width: i32, line_count: usize) -> Layout {
    let w = width.clamp(0, editor.w.max(0));
    let rect = Rect::new(editor.right() - w, editor.y, w, editor.h.max(0));
    let n = line_count.max(1) as f32;
    let line_px = if n * 2.0 <= rect.h as f32 { 2.0 } else { (rect.h as f32 / n).max(1e-6) };
    let char_px = 1.0;
    Layout { rect, line_px, char_px, max_cols: (w as f32 / char_px) as usize }
}

pub fn y_to_line(l: &Layout, y: f64, line_count: usize) -> usize {
    if line_count == 0 {
        return 0;
    }
    let v = ((y - l.rect.y as f64) / l.line_px as f64).floor();
    if v <= 0.0 {
        0
    } else {
        (v as usize).min(line_count - 1)
    }
}

pub fn line_to_y(l: &Layout, line: usize) -> f64 {
    l.rect.y as f64 + line as f64 * l.line_px as f64
}

pub fn drag_to_top_line(l: &Layout, y: f64, line_count: usize, visible_lines: usize) -> usize {
    let center = y_to_line(l, y, line_count);
    let top = center.saturating_sub(visible_lines / 2);
    top.min(line_count.saturating_sub(visible_lines))
}

pub fn draw(
    cv: &mut Canvas,
    l: &Layout,
    infos: &mut dyn FnMut(usize) -> LineInfo,
    line_count: usize,
    first_visible: usize,
    last_visible: usize,
    bg: Rgba,
    viewport_tint: Rgba,
) {
    draw_styled(cv, l, infos, line_count, first_visible, last_visible, bg, viewport_tint, Style::Normal)
}

pub fn draw_styled(
    cv: &mut Canvas,
    l: &Layout,
    infos: &mut dyn FnMut(usize) -> LineInfo,
    line_count: usize,
    first_visible: usize,
    last_visible: usize,
    bg: Rgba,
    viewport_tint: Rgba,
    style: Style,
) {
    let first = first_visible.min(line_count.saturating_sub(1));
    let last = last_visible.max(first).min(line_count.saturating_sub(1));
    draw_rows(cv, l, infos, line_count, first as f64, (last + 1 - first) as f64, bg, viewport_tint, style)
}

/// Where the viewport marker goes: the `rows` visual rows that start at fractional row `top`, out
/// of `total` rows (the unit of the minimap is the visual row, so with word wrap and folds the
/// marker still covers exactly what the editor shows). At least 2 px tall, never outside the strip.
pub fn viewport_rect(l: &Layout, total: usize, top: f64, rows: f64) -> Rect {
    let r = l.rect;
    let t = top.clamp(0.0, total as f64);
    let b = (top + rows).clamp(t, total as f64);
    let y0 = (r.y as f64 + t * l.line_px as f64).round() as i32;
    let y1 = ((r.y as f64 + b * l.line_px as f64).round() as i32).max(y0 + 2);
    Rect::new(r.x, y0, r.w, y1 - y0).intersect(&r)
}

/// Like [`draw_styled`] with a unit of one visual row: `line_count` rows, `infos(i)` describes row
/// `i`, and the viewport starts at fractional row `vp_top` and spans `vp_rows` rows.
pub fn draw_rows(
    cv: &mut Canvas,
    l: &Layout,
    infos: &mut dyn FnMut(usize) -> LineInfo,
    line_count: usize,
    vp_top: f64,
    vp_rows: f64,
    bg: Rgba,
    viewport_tint: Rgba,
    style: Style,
) {
    let r = l.rect;
    cv.fill_exact(r, bg.with_alpha(255), r);
    if line_count == 0 || r.w <= 0 || r.h <= 0 {
        return;
    }
    let mut bar_h = (l.line_px.round() as i32).max(1);
    if style == Style::Subtle && l.line_px >= 2.0 {
        bar_h = (bar_h / 2).max(1); // hairlines with a gap between rows
    }
    let max_cols = l.max_cols.min((r.w as f32 / l.char_px) as usize);
    let mut bar = |cv: &mut Canvas, y: i32, info: &LineInfo| {
        if info.blank || info.len_cols == 0 {
            return;
        }
        let x0 = (info.indent_cols as usize).min(max_cols);
        let x1 = (info.len_cols as usize).min(max_cols);
        if x1 <= x0 {
            return;
        }
        let px0 = r.x + (x0 as f32 * l.char_px) as i32;
        let px1 = r.x + (x1 as f32 * l.char_px).ceil() as i32;
        cv.fill_exact(Rect::new(px0, y, px1 - px0, bar_h), info.color.with_alpha(style.bar_alpha()), r);
    };
    if l.line_px >= 1.0 {
        let n_fit = ((r.h as f32 / l.line_px).ceil() as usize).min(line_count);
        for i in 0..n_fit {
            let info = infos(i);
            bar(cv, r.y + (i as f32 * l.line_px) as i32, &info);
        }
    } else {
        let rows = ((line_count as f32 * l.line_px).ceil() as i32).clamp(1, r.h);
        for row in 0..rows {
            let a = y_to_line(l, (r.y + row) as f64, line_count);
            let b = y_to_line(l, (r.y + row + 1) as f64, line_count).max(a);
            let b = if row == rows - 1 { line_count - 1 } else { b };
            // Group is a..b (b inclusive of next row's first line is excluded unless last).
            let end = if row == rows - 1 { b + 1 } else { b.max(a + 1) };
            let count = end - a;
            let samples = count.min(2);
            let mut best: Option<LineInfo> = None;
            for s in 0..samples {
                let idx = a + s * count / samples;
                let info = infos(idx);
                if !info.blank && best.map_or(true, |bi| info.len_cols > bi.len_cols) {
                    best = Some(info);
                }
            }
            if let Some(info) = best {
                bar(cv, r.y + row, &info);
            }
        }
    }
    // Viewport
    let vp = viewport_rect(l, line_count, vp_top, vp_rows);
    if vp.h <= 0 {
        return;
    }
    cv.fill_exact(vp, viewport_tint.with_alpha(style.viewport_alpha()), r);
    let edge = viewport_tint.mix(Rgba(255, 255, 255, 255), 0.5).with_alpha(0);
    cv.fill_clip(Rect::new(vp.x, vp.y, vp.w, 1), edge, r);
    cv.fill_clip(Rect::new(vp.x, vp.bottom() - 1, vp.w, 1), edge, r);
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: Rgba = Rgba(200, 200, 200, 255);
    const RED: Rgba = Rgba(255, 0, 0, 255);
    const BLUE: Rgba = Rgba(0, 0, 255, 255);
    const BG: Rgba = Rgba(0, 0, 0, 255);

    fn info(indent: u16, len: u16) -> LineInfo {
        LineInfo { indent_cols: indent, len_cols: len, color: Rgba(255, 255, 255, 255), blank: false }
    }

    #[test]
    fn dominant_color() {
        let t = "let abcdefgh = 1";
        let spans = [(0, 3, BLUE), (4, 12, RED)];
        assert_eq!(line_info(t, Some(&spans), FG, 4).color, RED);
    }
    #[test]
    fn continuation_rows_keep_the_token_colors() {
        // "let abcdefgh = 1" wrapped after 9 bytes: the second row "efgh = 1" is mostly the RED span
        let spans = [(0usize, 3usize, BLUE), (4, 12, RED)];
        let row = row_spans(spans.iter().copied(), 9, 16);
        assert_eq!(row, vec![(0, 3, RED)]);
        assert_eq!(line_info("fgh = 1", Some(&row), FG, 4).color, RED, "not the plain foreground");
        // a span that starts before the row is clipped to its start
        assert_eq!(row_spans(spans.iter().copied(), 6, 16), vec![(0, 6, RED)]);
        // nothing overlaps: no spans at all
        assert!(row_spans(spans.iter().copied(), 12, 16).is_empty());
    }
    #[test]
    fn unknown_uses_fg() {
        assert_eq!(line_info("hello", None, FG, 4).color, FG);
        assert_eq!(line_info("hello world", Some(&[(0, 1, RED)]), FG, 4).color, FG);
    }
    #[test]
    fn tabs_indent() {
        let i = line_info("\t\tfoo", None, FG, 4);
        assert_eq!(i.indent_cols, 8);
        assert_eq!(i.len_cols, 11);
        let i = line_info("  \tx", None, FG, 4);
        assert_eq!(i.indent_cols, 4);
    }
    #[test]
    fn blank_lines() {
        assert!(line_info("", None, FG, 4).blank);
        assert!(line_info("   \t ", None, FG, 4).blank);
        assert!(!line_info(" x", None, FG, 4).blank);
    }
    #[test]
    fn trailing_space_ignored() {
        assert_eq!(line_info("ab   ", None, FG, 4).len_cols, 2);
    }
    #[test]
    fn layout_small() {
        let l = layout_for(Rect::new(100, 10, 800, 600), 90, 50);
        assert_eq!(l.line_px, 2.0);
        assert_eq!(l.rect, Rect::new(810, 10, 90, 600));
        assert_eq!(l.max_cols, 90);
    }
    #[test]
    fn layout_large_fits() {
        let l = layout_for(Rect::new(0, 0, 800, 600), 90, 3000);
        assert!((l.line_px - 0.2).abs() < 1e-6);
        let l = layout_for(Rect::new(0, 0, 800, 600), 90, 1_000_000);
        assert!(l.line_px > 0.0 && l.line_px * 1_000_000.0 <= 600.5);
    }
    #[test]
    fn layout_exact_boundary() {
        let l = layout_for(Rect::new(0, 0, 800, 600), 90, 300);
        assert_eq!(l.line_px, 2.0);
    }
    #[test]
    fn round_trip_small() {
        let l = layout_for(Rect::new(0, 5, 800, 600), 90, 100);
        for line in [0usize, 1, 50, 99] {
            let y = line_to_y(&l, line) + 0.5;
            assert_eq!(y_to_line(&l, y, 100), line);
        }
    }
    #[test]
    fn round_trip_huge() {
        let n = 100_000;
        let l = layout_for(Rect::new(0, 0, 800, 600), 90, n);
        for line in [0usize, 1234, 50_000, 99_999] {
            let back = y_to_line(&l, line_to_y(&l, line) + 1e-9, n);
            assert!((back as i64 - line as i64).abs() <= 1, "{line} {back}");
        }
    }
    #[test]
    fn y_clamped() {
        let l = layout_for(Rect::new(0, 10, 800, 600), 90, 100);
        assert_eq!(y_to_line(&l, -50.0, 100), 0);
        assert_eq!(y_to_line(&l, 9999.0, 100), 99);
        assert_eq!(y_to_line(&l, 5.0, 0), 0);
    }

    fn run(w: i32, h: i32, editor: Rect, n: usize, f: usize, la: usize, calls: &mut usize) -> (Vec<u32>, Layout) {
        let mut px = vec![0x010203u32; (w * h) as usize];
        let l = layout_for(editor, 90, n);
        {
            let mut cv = Canvas { px: &mut px, w, h };
            let mut cb = |_i: usize| {
                *calls += 1;
                info(2, 40)
            };
            draw(&mut cv, &l, &mut cb, n, f, la, BG, Rgba(120, 120, 255, 255));
        }
        (px, l)
    }

    #[test]
    fn draw_stays_inside_rect() {
        let (w, h) = (300, 200);
        let mut calls = 0;
        let (px, l) = run(w, h, Rect::new(10, 10, 280, 180), 40, 0, 9, &mut calls);
        for y in 0..h {
            for x in 0..w {
                if !l.rect.contains(x as f64, y as f64) {
                    assert_eq!(px[(y * w + x) as usize], 0x010203, "({x},{y})");
                }
            }
        }
        assert_ne!(px[(20 * w + 250) as usize], 0x010203);
    }
    #[test]
    fn draw_bar_geometry() {
        let (w, h) = (200, 100);
        let mut px = vec![0u32; (w * h) as usize];
        let l = layout_for(Rect::new(0, 0, 200, 100), 90, 10);
        let mut cv = Canvas { px: &mut px, w, h };
        let mut cb = |i: usize| if i == 3 { info(10, 30) } else { LineInfo { blank: true, ..info(0, 0) } };
        draw(&mut cv, &l, &mut cb, 10, 8, 9, BG, Rgba(0, 0, 255, 255));
        let bgpx = px[0];
        let at = |x: i32, y: i32| px[(y * w + x) as usize];
        assert_ne!(at(l.rect.x + 15, 6), bgpx);
        assert_ne!(at(l.rect.x + 15, 7), bgpx);
        assert_eq!(at(l.rect.x + 5, 6), bgpx); // before indent
        assert_eq!(at(l.rect.x + 35, 6), bgpx); // past end
        assert_eq!(at(l.rect.x + 15, 4), bgpx); // other lines blank
    }
    #[test]
    fn blank_lines_no_bars() {
        let (w, h) = (120, 60);
        let mut px = vec![0u32; (w * h) as usize];
        let l = layout_for(Rect::new(0, 0, 120, 60), 90, 20);
        let mut cv = Canvas { px: &mut px, w, h };
        let mut cb = |_| LineInfo { blank: true, ..info(0, 0) };
        // viewport outside drawing: lines 100.. clamped to last, so check only top rows
        draw(&mut cv, &l, &mut cb, 20, 18, 19, BG, Rgba(0, 0, 255, 255));
        for y in 0..30 {
            for x in 0..w {
                assert_eq!(px[(y * w + x) as usize], 0);
            }
        }
    }
    #[test]
    fn huge_file_call_counts() {
        for n in [100_000usize, 1_000_000] {
            let mut calls = 0;
            let (_, l) = run(300, 400, Rect::new(0, 0, 300, 400), n, 1000, 1050, &mut calls);
            assert!(calls <= (l.rect.h * 2) as usize, "n={n} calls={calls}");
            assert!(calls > 0);
        }
    }
    #[test]
    fn small_file_call_counts() {
        let mut calls = 0;
        run(300, 400, Rect::new(0, 0, 300, 400), 50, 0, 10, &mut calls);
        assert_eq!(calls, 50);
    }
    #[test]
    fn sampling_takes_longest() {
        // every 2nd line long: group of 2 samples both, picks long one
        let (w, h) = (100, 10);
        let mut px = vec![0u32; (w * h) as usize];
        let l = layout_for(Rect::new(0, 0, 100, 10), 90, 20);
        let mut cv = Canvas { px: &mut px, w, h };
        let mut cb = |i: usize| if i % 2 == 1 { info(0, 50) } else { info(0, 5) };
        draw(&mut cv, &l, &mut cb, 20, 0, 0, BG, Rgba(0, 0, 255, 255));
        assert_ne!(px[(5 * w + (l.rect.x + 40)) as usize], px[(5 * w + (l.rect.x + 70)) as usize]);
    }
    #[test]
    fn viewport_rect_position() {
        let (w, h) = (200, 100);
        let mut px = vec![0u32; (w * h) as usize];
        let l = layout_for(Rect::new(0, 0, 200, 100), 90, 40);
        let mut cv = Canvas { px: &mut px, w, h };
        let mut cb = |_| LineInfo { blank: true, ..info(0, 0) };
        draw(&mut cv, &l, &mut cb, 40, 10, 14, BG, Rgba(0, 0, 255, 255));
        let x = l.rect.x + 80;
        let at = |y: i32| px[(y * w + x) as usize];
        let outside = at(5);
        assert_eq!(at(19), outside);
        assert_ne!(at(20), outside); // top edge at line 10 -> y 20
        assert_ne!(at(25), outside);
        assert_ne!(at(29), outside); // bottom edge, line 15 -> y 30
        assert_eq!(at(30), outside);
    }
    #[test]
    fn style_names_and_widths() {
        assert_eq!(Style::from_name("normal"), Style::Normal);
        assert_eq!(Style::from_name("subtle"), Style::Subtle);
        assert_eq!(Style::from_name("whatever"), Style::Subtle, "unknown values fall back to the default");
        assert_eq!(Style::from_name(Style::Normal.name()), Style::Normal);
        assert!(Style::Subtle.width() < Style::Normal.width());
        assert!(Style::Subtle.bar_alpha() < Style::Normal.bar_alpha());
        assert!(Style::Subtle.viewport_alpha() < Style::Normal.viewport_alpha());
    }

    /// Sum over the strip of how far each pixel is from the background (a rough "ink" measure).
    fn ink(style: Style) -> u64 {
        let (w, h) = (200, 100);
        let mut px = vec![0u32; (w * h) as usize];
        let l = layout_for(Rect::new(0, 0, 200, 100), style.width(), 40);
        let mut cv = Canvas { px: &mut px, w, h };
        let mut cb = |_| info(2, 60);
        draw_styled(&mut cv, &l, &mut cb, 40, 0, 4, BG, Rgba(120, 120, 255, 255), style);
        px.iter().map(|&p| ((p >> 16) & 255) as u64 + ((p >> 8) & 255) as u64 + (p & 255) as u64).sum()
    }

    #[test]
    fn subtle_style_draws_less_than_normal() {
        let (n, s) = (ink(Style::Normal), ink(Style::Subtle));
        assert!(n > 0 && s > 0);
        assert!(s * 2 < n, "subtle {s} should be well under half of normal {n}");
    }

    #[test]
    fn subtle_bars_are_one_pixel_lines() {
        let (w, h) = (100, 40);
        let mut px = vec![0u32; (w * h) as usize];
        let l = layout_for(Rect::new(0, 0, 100, 40), Style::Subtle.width(), 10);
        let mut cv = Canvas { px: &mut px, w, h };
        let mut cb = |_| info(0, 30);
        draw_styled(&mut cv, &l, &mut cb, 10, 9, 9, BG, Rgba(0, 0, 255, 255), Style::Subtle);
        let at = |y: i32| px[(y * w + l.rect.x + 10) as usize];
        assert_ne!(at(0), at(1), "row 0 is a 1 px line followed by a gap");
        assert_eq!(at(1), at(3));
    }

    #[test]
    fn drag_centers_and_clamps() {
        let l = layout_for(Rect::new(0, 0, 200, 400), 90, 100);
        assert_eq!(drag_to_top_line(&l, 100.0, 100, 20), 40); // line 50 - 10
        assert_eq!(drag_to_top_line(&l, 0.0, 100, 20), 0);
        assert_eq!(drag_to_top_line(&l, 399.0, 100, 20), 80);
        assert_eq!(drag_to_top_line(&l, 100.0, 10, 20), 0);
    }
}
