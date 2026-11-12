//! Drawing primitives shared by every part of the UI: rectangles, a pixel canvas
//! with alpha blending, and a text context that renders colored runs.

use crate::theme::Rgba;
use cosmic_text::{
    Align, Attrs, Buffer as TextBuf, Color, Family, FeatureTag, FontFeatures, FontSystem, Metrics, Shaping, SwashCache,
    UnderlineStyle, Wrap, fontdb,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Style flags for [`TextCtx::draw_styled`] pieces.
pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINE: u8 = 4;

/// Max number of fallback faces (emoji / CJK / symbols) loaded on top of the main fonts.
const MAX_FALLBACKS: usize = 4;
/// Fallback font files bigger than this are not loaded.
const MAX_FALLBACK_BYTES: u64 = 25 * 1024 * 1024;

use std::sync::atomic::{AtomicU32, Ordering};

const DEFAULT_FONT_SIZE: f32 = crate::fontcfg::DEFAULT_BUFFER_SIZE;

static SCALE_BITS: AtomicU32 = AtomicU32::new(0x3f800000); // 1.0

/// Global UI scale (device pixels per logical pixel): 1.0 = classic, 1.5/2.0 = hi-DPI.
/// All layout code works in *logical* pixels; `Canvas` and `TextCtx` convert to device pixels
/// when they draw, so at 1.0 every coordinate is passed through unchanged.
pub fn scale() -> f32 {
    f32::from_bits(SCALE_BITS.load(Ordering::Relaxed))
}

/// Set the scale (clamped to 0.5..=4). Callers go through `TextCtx::set_scale` so that the
/// font metrics are re-measured too.
fn store_scale(s: f32) -> f32 {
    let s = if s.is_finite() { s.clamp(0.5, 4.0) } else { 1.0 };
    SCALE_BITS.store(s.to_bits(), Ordering::Relaxed);
    s
}

/// Logical -> device pixels (rounded). Identity at scale 1.0.
pub fn sc(v: i32) -> i32 {
    let s = scale();
    if s == 1.0 { v } else { (v as f32 * s).round() as i32 }
}

/// Device pixels -> logical (rounded up, so the logical area always covers the device area).
pub fn unsc_ceil(v: u32) -> u32 {
    let s = scale();
    if s == 1.0 { v } else { (v as f32 / s - 1e-3).ceil().max(1.0) as u32 }
}

/// Device thickness of a 1-logical-pixel line: whole device pixels, never thinner than 1.
pub fn hairline() -> i32 {
    (scale().round() as i32).max(1)
}

/// Window size in device pixels -> logical size.
pub fn logical_size(w: u32, h: u32) -> (u32, u32) {
    (unsc_ceil(w), unsc_ceil(h))
}

/// Where the UI scale comes from: the window's device scale factor, unless overridden.
#[derive(Clone, Copy, Debug)]
pub struct Dpi {
    /// `window.scale_factor()` (fractional on Wayland with wp-fractional-scale: 1.25, 1.5 ...).
    pub device: f64,
    /// `--scale` / `LUMEN_SCALE` (tests, screenshots); beats the config.
    pub forced: Option<f32>,
}

impl Dpi {
    pub fn from_env() -> Self {
        Self { device: 1.0, forced: std::env::var("LUMEN_SCALE").ok().and_then(|v| parse_scale(&v)) }
    }

    /// `cfg` is the `ui_scale` setting (0 = automatic).
    pub fn effective(&self, cfg: f32) -> f32 {
        self.forced.or((cfg > 0.0).then_some(cfg)).unwrap_or(self.device as f32).clamp(0.5, 4.0)
    }
}

pub fn parse_scale(v: &str) -> Option<f32> {
    v.trim().parse::<f32>().ok().filter(|s| s.is_finite() && *s >= 0.5 && *s <= 4.0)
}

/// Current font size in px (changed by zoom).
pub fn font_size() -> f32 {
    crate::fontcfg::buffer_size()
}

/// Buffer row height in logical pixels: the buffer font size times the line-height setting
/// (default 22 px at 14 px), or a fixed pixel height.
pub fn line_h() -> f32 {
    crate::fontcfg::line_px(font_size(), crate::fontcfg::buffer_line_height())
}

/// Interface text size and text box height (see `fontcfg`).
pub fn ui_size() -> f32 {
    crate::fontcfg::ui_size()
}

pub fn ui_line() -> f32 {
    crate::fontcfg::ui_line()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, (self.w - 2 * d).max(0), (self.h - 2 * d).max(0))
    }
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x as f64 && px < self.right() as f64 && py >= self.y as f64 && py < self.bottom() as f64
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }
}

pub fn to_color(c: Rgba) -> Color {
    Color::rgba(c.0, c.1, c.2, c.3)
}

fn blend_px(dst: u32, c: Rgba) -> u32 {
    let a = c.3 as u32;
    if a == 255 {
        return ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32;
    }
    let mix = |d: u32, s: u8| (d * (255 - a) + s as u32 * a) / 255;
    (mix((dst >> 16) & 0xff, c.0) << 16) | (mix((dst >> 8) & 0xff, c.1) << 8) | mix(dst & 0xff, c.2)
}

pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub w: i32,
    pub h: i32,
}

impl Canvas<'_> {
    /// Logical bounds (`w`/`h` are device pixels).
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, unsc_ceil(self.w as u32) as i32, unsc_ceil(self.h as u32) as i32)
    }

    fn dev_bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    /// Logical rect -> device rect. Edges are rounded independently so neighbours tile exactly.
    pub fn to_dev(&self, r: Rect) -> Rect {
        if scale() == 1.0 {
            return r;
        }
        let (x0, y0, x1, y1) = (sc(r.x), sc(r.y), sc(r.right()), sc(r.bottom()));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Like [`to_dev`](Self::to_dev), but a 1-logical-pixel extent becomes a [`hairline`] so
    /// borders and separators have the same thickness wherever they fall.
    fn to_dev_line(&self, r: Rect) -> Rect {
        let mut d = self.to_dev(r);
        let hl = hairline();
        if r.w == 1 {
            d.w = hl;
        }
        if r.h == 1 {
            d.h = hl;
        }
        d
    }

    pub fn clear(&mut self, c: Rgba) {
        self.px.fill(blend_px(0, c.with_alpha(255)));
    }

    pub fn fill(&mut self, r: Rect, c: Rgba) {
        self.fill_clip(r, c, self.bounds());
    }

    pub fn fill_clip(&mut self, r: Rect, c: Rgba, clip: Rect) {
        let (d, dc) = (self.to_dev_line(r), self.to_dev(clip));
        self.fill_dev(d, c, dc);
    }

    /// `fill_clip` without the hairline rule: 1-pixel rects scale like any other rect (minimap
    /// rows, where neighbours must tile).
    pub fn fill_exact(&mut self, r: Rect, c: Rgba, clip: Rect) {
        let (d, dc) = (self.to_dev(r), self.to_dev(clip));
        self.fill_dev(d, c, dc);
    }

    /// Fill a rect given in device pixels.
    fn fill_dev(&mut self, r: Rect, c: Rgba, clip: Rect) {
        let r = r.intersect(&clip).intersect(&self.dev_bounds());
        for y in r.y..r.bottom() {
            let row = (y * self.w) as usize;
            for x in r.x..r.right() {
                let i = row + x as usize;
                self.px[i] = blend_px(self.px[i], c);
            }
        }
    }

    /// One logical pixel (a `scale` x `scale` block of device pixels).
    pub fn dot(&mut self, x: i32, y: i32, c: Rgba) {
        if scale() == 1.0 {
            return self.dot_dev(x, y, c);
        }
        let d = self.to_dev(Rect::new(x, y, 1, 1));
        self.fill_dev(d, c, self.dev_bounds());
    }

    /// One device pixel.
    pub fn dot_dev(&mut self, x: i32, y: i32, c: Rgba) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            let i = (y * self.w + x) as usize;
            self.px[i] = blend_px(self.px[i], c);
        }
    }

    /// Filled rounded rectangle with antialiased corners.
    pub fn fill_round(&mut self, r: Rect, rad: i32, c: Rgba) {
        self.fill_round_clip(r, rad, c, self.bounds());
    }

    /// [`fill_round`](Self::fill_round) clipped to `clip` (a pane smaller than the canvas).
    pub fn fill_round_clip(&mut self, r: Rect, rad: i32, c: Rgba, clip: Rect) {
        let (d, dc) = (self.to_dev(r), self.to_dev(clip));
        self.round_dev(d, sc(rad), c, dc);
    }

    fn round_dev(&mut self, r: Rect, rad: i32, c: Rgba, clip: Rect) {
        let rad = rad.min(r.w / 2).min(r.h / 2).max(0);
        if rad == 0 {
            return self.fill_dev(r, c, clip);
        }
        self.fill_dev(Rect::new(r.x + rad, r.y, r.w - 2 * rad, r.h), c, clip);
        self.fill_dev(Rect::new(r.x, r.y + rad, rad, r.h - 2 * rad), c, clip);
        self.fill_dev(Rect::new(r.right() - rad, r.y + rad, rad, r.h - 2 * rad), c, clip);
        let rf = rad as f32;
        for (cx, cy, x0, y0) in [
            (r.x + rad, r.y + rad, r.x, r.y),
            (r.right() - rad, r.y + rad, r.right() - rad, r.y),
            (r.x + rad, r.bottom() - rad, r.x, r.bottom() - rad),
            (r.right() - rad, r.bottom() - rad, r.right() - rad, r.bottom() - rad),
        ] {
            for y in y0..y0 + rad {
                for x in x0..x0 + rad {
                    if !clip.contains(x as f64, y as f64) {
                        continue;
                    }
                    let (dx, dy) = (x as f32 + 0.5 - cx as f32, y as f32 + 0.5 - cy as f32);
                    let cov = (rf - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        self.dot_dev(x, y, Rgba(c.0, c.1, c.2, (c.3 as f32 * cov) as u8));
                    }
                }
            }
        }
    }

    /// Rounded card: 1px `border` ring around a `fill` interior.
    pub fn card(&mut self, r: Rect, rad: i32, fill: Rgba, border: Rgba) {
        let (d, hl) = (self.to_dev(r), hairline());
        let rad = sc(rad);
        let all = self.dev_bounds();
        self.round_dev(d, rad, border, all);
        self.round_dev(Rect::new(d.x + hl, d.y + hl, d.w - 2 * hl, d.h - 2 * hl), (rad - hl).max(0), fill, all);
    }

    /// Antialiased 1px outline of a rounded rectangle, drawn over whatever is there.
    pub fn ring(&mut self, r: Rect, rad: i32, c: Rgba) {
        let d = self.to_dev(r);
        let hl = hairline();
        let rad = sc(rad).min(d.w / 2).min(d.h / 2).max(1);
        let all = self.dev_bounds();
        self.fill_dev(Rect::new(d.x + rad, d.y, d.w - 2 * rad, hl), c, all);
        self.fill_dev(Rect::new(d.x + rad, d.bottom() - hl, d.w - 2 * rad, hl), c, all);
        self.fill_dev(Rect::new(d.x, d.y + rad, hl, d.h - 2 * rad), c, all);
        self.fill_dev(Rect::new(d.right() - hl, d.y + rad, hl, d.h - 2 * rad), c, all);
        let rf = rad as f32;
        for (cx, cy, x0, y0) in [
            (d.x + rad, d.y + rad, d.x, d.y),
            (d.right() - rad, d.y + rad, d.right() - rad, d.y),
            (d.x + rad, d.bottom() - rad, d.x, d.bottom() - rad),
            (d.right() - rad, d.bottom() - rad, d.right() - rad, d.bottom() - rad),
        ] {
            for y in y0..y0 + rad {
                for x in x0..x0 + rad {
                    let (dx, dy) = (x as f32 + 0.5 - cx as f32, y as f32 + 0.5 - cy as f32);
                    let dist = (dx * dx + dy * dy).sqrt();
                    let outer = (rf - dist + 0.5).clamp(0.0, 1.0);
                    let inner = (rf - hl as f32 - dist + 0.5).clamp(0.0, 1.0);
                    let cov = (outer - inner).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        self.dot_dev(x, y, Rgba(c.0, c.1, c.2, (c.3 as f32 * cov) as u8));
                    }
                }
            }
        }
    }

    /// Soft drop shadow under a rounded rectangle (call before drawing the card).
    pub fn shadow(&mut self, r: Rect, rad: i32) {
        // Soft, wide, low-contrast shadow (macOS style): many faint layers, shifted down.
        for i in 1..=16 {
            let g = Rect::new(r.x - i, r.y - i + 8, r.w + 2 * i, r.h + 2 * i);
            self.fill_round(g, rad + i, Rgba(0, 0, 0, 3));
        }
    }
}

/// A thin overlay scrollbar: `track` is the (wider) hit area at the right edge, `thumb` the handle.
#[derive(Clone, Copy, Debug)]
pub struct VBar {
    pub track: Rect,
    pub thumb: Rect,
}

impl VBar {
    /// `None` when everything fits. `total`/`visible`/`pos` are in the same unit (rows or px).
    pub fn new(track: Rect, total: f64, visible: f64, pos: f64) -> Option<VBar> {
        if total <= visible + 0.5 || track.h < 24 || crate::display::scrollbar() == crate::display::Scrollbar::Never {
            return None;
        }
        let th = (((track.h as f64) * (visible / total).clamp(0.0, 1.0)) as i32).clamp(32, track.h);
        let max = (total - visible).max(1e-9);
        let ty = track.y + (((track.h - th) as f64) * (pos / max).clamp(0.0, 1.0)) as i32;
        Some(VBar { track, thumb: Rect::new(track.x, ty, track.w, th) })
    }

    /// Scroll position for dragging the thumb with the pointer at `y`, grabbed `grab` px below its top.
    pub fn pos_for(&self, y: f64, grab: f64, total: f64, visible: f64) -> f64 {
        let span = (self.track.h - self.thumb.h).max(1) as f64;
        ((y - grab - self.track.y as f64) / span).clamp(0.0, 1.0) * (total - visible).max(0.0)
    }

    /// Draw the handle: faint at rest, a bit wider and stronger when the pointer is near or dragging.
    pub fn draw(&self, cv: &mut Canvas, fg: Rgba, hot: bool, active: bool) {
        let w = if hot || active { 8 } else { 5 };
        let a = if active {
            0x78
        } else if hot {
            0x55
        } else {
            0x2c
        };
        let k = crate::display::bar_strength(hot || active);
        if k <= 0.0 {
            return;
        }
        let a = (a as f32 * k) as u8;
        let r = Rect::new(self.track.right() - w - 3, self.thumb.y + 2, w, self.thumb.h - 4);
        cv.fill_round_clip(r, w / 2, fg.with_alpha(a), self.track);
    }
}

/// The horizontal twin of `VBar`: a thin overlay bar along the bottom edge. `track` is the hit area,
/// `thumb` the handle; `total`/`visible`/`pos` are in the same unit (px or columns).
#[derive(Clone, Copy, Debug)]
pub struct HBar {
    pub track: Rect,
    pub thumb: Rect,
}

impl HBar {
    /// `None` when everything fits.
    pub fn new(track: Rect, total: f64, visible: f64, pos: f64) -> Option<HBar> {
        if total <= visible + 0.5 || track.w < 24 || crate::display::scrollbar() == crate::display::Scrollbar::Never {
            return None;
        }
        let tw = (((track.w as f64) * (visible / total).clamp(0.0, 1.0)) as i32).clamp(32, track.w);
        let max = (total - visible).max(1e-9);
        let tx = track.x + (((track.w - tw) as f64) * (pos / max).clamp(0.0, 1.0)) as i32;
        Some(HBar { track, thumb: Rect::new(tx, track.y, tw, track.h) })
    }

    /// Scroll position for dragging the thumb with the pointer at `x`, grabbed `grab` px right of its left edge.
    pub fn pos_for(&self, x: f64, grab: f64, total: f64, visible: f64) -> f64 {
        let span = (self.track.w - self.thumb.w).max(1) as f64;
        ((x - grab - self.track.x as f64) / span).clamp(0.0, 1.0) * (total - visible).max(0.0)
    }

    /// Draw the handle: faint at rest, a bit thicker and stronger when the pointer is near or dragging.
    pub fn draw(&self, cv: &mut Canvas, fg: Rgba, hot: bool, active: bool) {
        let h = if hot || active { 8 } else { 5 };
        let a = if active {
            0x78
        } else if hot {
            0x55
        } else {
            0x2c
        };
        let k = crate::display::bar_strength(hot || active);
        if k <= 0.0 {
            return;
        }
        let a = (a as f32 * k) as u8;
        let r = Rect::new(self.thumb.x + 2, self.track.bottom() - h - 3, self.thumb.w - 4, h);
        cv.fill_round_clip(r, h / 2, fg.with_alpha(a), self.track);
    }
}

/// `s` shortened with a trailing "…" so it fits `max_w` px in the UI font (`s` itself when it fits,
/// empty when not even the ellipsis does). Binary search over char boundaries, so it is safe on any
/// UTF-8 and costs O(log n) measurements (each cached by `TextCtx::ui_width`).
pub fn fit_ellipsis(tx: &mut TextCtx, s: &str, max_w: i32, bold: bool) -> String {
    if max_w <= 0 {
        return String::new();
    }
    if tx.ui_width(s, bold) <= max_w {
        return s.to_string();
    }
    // Byte offsets of every char boundary after the first char: candidate cut points.
    let cuts: Vec<usize> = s.char_indices().skip(1).map(|(i, _)| i).collect();
    let (mut lo, mut hi) = (0usize, cuts.len()); // number of cut points usable (0 = no prefix)
    let prefix = |k: usize| if k == 0 { "" } else { &s[..cuts[k - 1]] };
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand = format!("{}…", prefix(mid).trim_end());
        if tx.ui_width(&cand, bold) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 && tx.ui_width("…", bold) > max_w {
        return String::new();
    }
    format!("{}…", prefix(lo).trim_end())
}

#[cfg(test)]
mod hbar_tests {
    use super::*;

    #[test]
    fn hidden_when_everything_fits() {
        let t = Rect::new(10, 100, 300, 12);
        assert!(HBar::new(t, 300.0, 300.0, 0.0).is_none());
        assert!(HBar::new(t, 200.0, 300.0, 0.0).is_none());
        assert!(HBar::new(Rect::new(0, 0, 10, 12), 900.0, 300.0, 0.0).is_none(), "track too short");
    }

    #[test]
    fn thumb_is_proportional_and_follows_the_position() {
        let t = Rect::new(10, 100, 300, 12);
        let b = HBar::new(t, 900.0, 300.0, 0.0).unwrap();
        assert_eq!((b.thumb.x, b.thumb.w, b.thumb.y, b.thumb.h), (10, 100, 100, 12));
        let b = HBar::new(t, 900.0, 300.0, 600.0).unwrap();
        assert_eq!(b.thumb.right(), t.right(), "at the end the thumb touches the end of the track");
        let b = HBar::new(t, 900.0, 300.0, 300.0).unwrap();
        assert_eq!(b.thumb.x, 10 + 100);
        // tiny visible fraction: the thumb keeps a grabbable minimum
        let b = HBar::new(t, 90000.0, 300.0, 0.0).unwrap();
        assert_eq!(b.thumb.w, 32);
    }

    #[test]
    fn dragging_maps_the_pointer_back_to_a_position() {
        let t = Rect::new(10, 100, 300, 12);
        let b = HBar::new(t, 900.0, 300.0, 0.0).unwrap();
        // grabbing the middle of the thumb and moving to the far right gives the maximum
        assert_eq!(b.pos_for(10.0 + 300.0, 50.0, 900.0, 300.0), 600.0);
        assert_eq!(b.pos_for(0.0, 50.0, 900.0, 300.0), 0.0);
        let mid = b.pos_for(10.0 + 50.0 + 100.0, 50.0, 900.0, 300.0);
        assert!((mid - 300.0).abs() < 1.0, "mid = {mid}");
        // round trip: the pos placed by the thumb is the pos asked for
        let moved = HBar::new(t, 900.0, 300.0, mid).unwrap();
        assert!((moved.thumb.x - (10 + 100)).abs() <= 1);
    }
}

pub struct TextCtx {
    pub fonts: FontSystem,
    pub swash: SwashCache,
    buf: TextBuf,
    ui_buf: TextBuf,
    family: Option<String>,
    ui_family: Option<String>,
    pub cell_w: f32,
    pub icons: crate::icons::Icons,
    widths: std::collections::HashMap<(String, bool), f32>,
    /// Every font file already in the database (main, variants, UI, fallbacks), canonicalized.
    loaded: Vec<PathBuf>,
    fallbacks_loaded: bool,
    /// Arabic / Hebrew faces looked up (see [`TextCtx::ensure_rtl_fonts`]).
    rtl_loaded: bool,
    /// Cached result of [`TextCtx::italic_works`].
    italic_ok: Option<bool>,
    /// Buffer font weight / ligatures and UI font weight / ligatures (see `fontcfg`).
    buf_weight: u16,
    buf_ligs: bool,
    /// Buffer OpenType features (`buffer_font_features`) and letter spacing in logical px.
    buf_feats: Vec<([u8; 4], u32)>,
    buf_spacing: f32,
    ui_weight: u16,
    ui_ligs: bool,
    /// The families found at startup (system monospace / sans-serif), used when a chosen one is missing.
    default_family: Option<String>,
    default_ui_family: Option<String>,
    /// Faces loaded for the user's buffer / UI family (removed again when the choice changes).
    user_faces: [Vec<fontdb::ID>; 2],
    /// The font system holds every installed font (no single-font database): families just resolve.
    sys_all: bool,
    /// Faces loaded to preview families in the font picker (dropped when it closes).
    previews: Vec<(String, Vec<fontdb::ID>)>,
}

#[cfg(all(unix, not(target_os = "macos")))]
fn find_font(query: &str) -> Option<PathBuf> {
    let out = Command::new("fc-match").args(["-f", "%{file}", query]).output().ok()?;
    let p = PathBuf::from(String::from_utf8(out.stdout).ok()?);
    p.exists().then_some(p)
}

/// Windows and macOS have no fontconfig: the same queries are answered from the fonts the OS ships.
/// `query` is `<family or generic>[:bold][:italic][:semibold]`; the first existing file wins.
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn find_font(query: &str) -> Option<PathBuf> {
    let mut parts = query.split(':');
    let base = parts.next().unwrap_or("").to_ascii_lowercase();
    let flags: Vec<&str> = parts.collect();
    let (bold, italic) = (flags.contains(&"bold"), flags.contains(&"italic"));
    let semibold = flags.contains(&"semibold");
    let variant = |r: &'static str, b: &'static str, i: &'static str, bi: &'static str| match (bold, italic) {
        (false, false) => r,
        (true, false) => b,
        (false, true) => i,
        (true, true) => bi,
    };
    let names: Vec<&'static str> = if cfg!(windows) {
        match base.as_str() {
            "monospace" | "consolas" => vec![variant("consola.ttf", "consolab.ttf", "consolai.ttf", "consolaz.ttf")],
            "sans-serif" if semibold => vec!["seguisb.ttf", "segoeui.ttf"],
            "sans-serif" | "segoe ui" => vec![variant("segoeui.ttf", "segoeuib.ttf", "segoeuii.ttf", "segoeuiz.ttf")],
            "emoji" => vec!["seguiemj.ttf"],
            "noto sans symbols 2" => vec!["seguisym.ttf"],
            _ => vec![],
        }
    } else {
        match base.as_str() {
            "monospace" | "menlo" => vec!["Menlo.ttc"],
            "sans-serif" => vec!["SFNS.ttf", "Helvetica.ttc"],
            "emoji" => vec!["Apple Color Emoji.ttc"],
            "noto sans symbols 2" => vec!["Apple Symbols.ttf"],
            "noto sans arabic" => vec!["GeezaPro.ttc"],
            "noto sans hebrew" => vec!["ArialHB.ttc"],
            _ => vec![],
        }
    };
    let dirs: Vec<PathBuf> = if cfg!(windows) {
        let win = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        vec![win.join("Fonts")]
    } else {
        vec![
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/System/Library/Fonts/Supplemental"),
            PathBuf::from("/Library/Fonts"),
        ]
    };
    // CJK: the lang filter of the fontconfig query selects one of the system's CJK faces.
    let cjk: &[&str] = if !query.contains("charset=65e5") {
        &[]
    } else if cfg!(windows) {
        &["msyh.ttc", "YuGothR.ttc", "simsun.ttc", "malgun.ttf"]
    } else {
        &["Hiragino Sans GB.ttc", "PingFang.ttc", "STHeiti Light.ttc"]
    };
    let names: Vec<&'static str> = if cjk.is_empty() { names } else { Vec::new() };
    names.iter().chain(cjk.iter()).flat_map(|n| dirs.iter().map(move |d| d.join(n))).find(|p| p.exists())
}

fn find_mono_font() -> Option<PathBuf> {
    find_font("monospace")
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Files of every installed font that has a glyph for `cp` (fontconfig charset query).
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn files_covering(_cp: u32) -> Vec<PathBuf> {
    // No coverage query without fontconfig: the fallback tables above already pick a font per class.
    let mut out = Vec::new();
    for q in ["emoji", "sans-serif:charset=65e5", "sans-serif", "monospace", "Noto Sans Symbols 2"] {
        out.extend(find_font(q).map(|p| canon(&p)));
    }
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn files_covering(cp: u32) -> Vec<PathBuf> {
    let Ok(out) = Command::new("fc-list").args(["-f", "%{file}\n", &format!(":charset={cp:x}")]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).map(|l| canon(Path::new(l))).collect()
}

fn face_ids(db: &fontdb::Database) -> Vec<fontdb::ID> {
    db.faces().map(|f| f.id).collect()
}

/// Load `path` into `db`. Returns the ids of the faces it added (empty on failure).
fn load_into(db: &mut fontdb::Database, path: &Path) -> Vec<fontdb::ID> {
    let before = face_ids(db);
    if db.load_font_file(path).is_err() {
        return Vec::new();
    }
    face_ids(db).into_iter().filter(|id| !before.contains(id)).collect()
}

/// Bold / italic / bold-italic files of `family` that differ from what is already loaded.
/// A candidate is kept only if the face it adds really is that variant of that family
/// (fc-match happily answers with some other font when the variant doesn't exist).
fn load_variants(db: &mut fontdb::Database, family: &str, loaded: &mut Vec<PathBuf>) {
    for (suffix, bold, italic) in [(":bold", true, false), (":italic", false, true), (":bold:italic", true, true)] {
        let Some(p) = find_font(&format!("{family}{suffix}")) else { continue };
        let p = canon(&p);
        if loaded.contains(&p) {
            continue;
        }
        let added = load_into(db, &p);
        let ok = added.iter().any(|id| {
            db.face(*id).is_some_and(|f| {
                f.families.iter().any(|(n, _)| n == family)
                    && (f.weight.0 >= 600) == bold
                    && (f.style != fontdb::Style::Normal) == italic
            })
        });
        if ok {
            loaded.push(p);
        } else {
            for id in added {
                db.remove_face(id);
            }
        }
    }
}

/// A font database with the code font + one proportional UI family: `FontSystem::new_with_fonts`
/// would also load every system font (~25 MB of mapped files), which we can't afford.
fn single_font_system(path: Option<&Path>) -> (FontSystem, Option<String>, Option<String>, Vec<PathBuf>) {
    if let Some(p) = path {
        let mut db = fontdb::Database::new();
        if db.load_font_file(p).is_ok() {
            let fam = db.faces().next().and_then(|f| f.families.first().map(|x| x.0.clone()));
            if let Some(fam) = fam {
                db.set_monospace_family(&fam);
                let mut loaded = vec![canon(p)];
                // One proportional family for the UI chrome (regular + semibold).
                let mut ui_family = None;
                for q in ["sans-serif", "sans-serif:semibold"] {
                    if let Some(fp) = find_font(q) {
                        let before = db.len();
                        if db.load_font_file(&fp).is_ok() && db.len() > before {
                            loaded.push(canon(&fp));
                            if ui_family.is_none() {
                                ui_family = db.faces().last().and_then(|f| f.families.first().map(|x| x.0.clone()));
                            }
                        }
                    }
                }
                return (FontSystem::new_with_locale_and_db("en-US".to_string(), db), Some(fam), ui_family, loaded);
            }
        }
    }
    (FontSystem::new(), None, None, Vec::new())
}

/// Fallback font files for glyphs the code font lacks, best first: color emoji, CJK, then up to
/// two symbol fonts (arrows / math / dingbats, box drawing). Each query is verified against
/// fontconfig's coverage list (fc-match always answers with *something*).
fn fallback_candidates(loaded: &[PathBuf]) -> Vec<PathBuf> {
    let queries: [(&[&str], u32); 5] = [
        (&["emoji"], 0x1F600),
        (&["sans-serif:lang=zh-cn:charset=65e5", "sans-serif:lang=ja:charset=65e5"], 0x65E5),
        (&["sans-serif:charset=2190-21ff 2200-22ff 2713"], 0x2192),
        (&["monospace:charset=2500-257f"], 0x2500),
        (&["Noto Sans Symbols 2"], 0x2713),
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for (qs, probe) in queries {
        if out.len() >= MAX_FALLBACKS {
            break;
        }
        let covering = files_covering(probe);
        for q in qs {
            let Some(p) = find_font(q) else { continue };
            let p = canon(&p);
            if !covering.contains(&p) {
                continue;
            }
            if !loaded.contains(&p)
                && !out.contains(&p)
                && std::fs::metadata(&p).is_ok_and(|m| m.len() <= MAX_FALLBACK_BYTES)
            {
                out.push(p);
            }
            break; // this class is satisfied (possibly by an already loaded file)
        }
    }
    out
}

/// Faces for right-to-left code and prose when the loaded fonts lack them: Arabic, then Hebrew.
/// A monospace face with both (DejaVu Sans Mono) is preferred, then Noto, then whatever
/// fontconfig picks for the language. Windows needs none (Segoe UI, the interface font, has both).
fn rtl_candidates(loaded: &[PathBuf]) -> Vec<PathBuf> {
    let fc = cfg!(all(unix, not(target_os = "macos")));
    let queries: [(&[&str], u32); 2] = [
        (&["DejaVu Sans Mono", "Noto Sans Arabic", "sans-serif:lang=ar"], 0x0627),
        (&["DejaVu Sans Mono", "Noto Sans Hebrew", "sans-serif:lang=he"], 0x05D0),
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for (qs, probe) in queries {
        let covering = if fc { files_covering(probe) } else { Vec::new() };
        if fc && loaded.iter().chain(out.iter()).any(|p| covering.contains(p)) {
            continue; // already covered
        }
        for q in qs {
            let Some(p) = find_font(q) else { continue };
            let p = canon(&p);
            if fc && !covering.contains(&p) {
                continue;
            }
            if !loaded.contains(&p)
                && !out.contains(&p)
                && std::fs::metadata(&p).is_ok_and(|m| m.len() <= MAX_FALLBACK_BYTES)
            {
                out.push(p);
            }
            break;
        }
    }
    out
}

/// True if `s` has a char >= U+2000 (UTF-8 lead byte >= 0xE2): the cheap trigger for loading
/// fallback fonts. Latin, Latin-1 and Greek/Cyrillic never need them.
fn needs_fallback(s: &str) -> bool {
    s.bytes().any(|b| b >= 0xE2)
}

/// `Shaping::Basic` (what the editor uses: cheap, no ligatures) never consults more than one
/// fallback font, so text with exotic chars goes through `Shaping::Advanced`, which walks every
/// face in the database. Ligature features are switched off there so the cell grid and the look
/// stay the same as in Basic mode.
fn plain_features() -> FontFeatures {
    let mut f = FontFeatures::new();
    for t in [FeatureTag::STANDARD_LIGATURES, FeatureTag::CONTEXTUAL_LIGATURES, FeatureTag::CONTEXTUAL_ALTERNATES] {
        f.disable(t);
    }
    f
}

/// Features for buffer text shaped by `Shaping::Advanced`: `liga` / `clig` / `calt` off unless
/// ligatures are on (an explicit user entry for one of them wins), then the user's own features
/// (`zero`, `ss01`, `cv02`, ...).
fn buffer_features(ligs: bool, user: &[([u8; 4], u32)]) -> FontFeatures {
    let mut f = FontFeatures::new();
    if !ligs {
        for t in [FeatureTag::STANDARD_LIGATURES, FeatureTag::CONTEXTUAL_LIGATURES, FeatureTag::CONTEXTUAL_ALTERNATES] {
            if !user.iter().any(|(u, _)| u == t.as_bytes()) {
                f.disable(t);
            }
        }
    }
    for (tag, v) in user {
        f.set(FeatureTag::new(tag), *v);
    }
    f
}

/// Cell width in *logical* pixels: measured at the device size (`px * scale`) so that glyph
/// advances match what is drawn, then divided back (fractional, e.g. 8.4 / 12.6 / 14.7 ...).
fn measure_cell(fonts: &mut FontSystem, attrs: &Attrs, px: f32) -> f32 {
    let s = scale();
    let dev = px * s;
    let mut b = TextBuf::new(fonts, Metrics::new(dev, (px * 20.0 / 14.0).round() * s));
    b.set_size(Some(2000.0), Some(200.0));
    b.set_text("MMMMMMMMMMMMMMMMMMMM", attrs, Shaping::Basic, None);
    b.shape_until_scroll(fonts, false);
    b.layout_runs().next().map_or(px * 0.6, |r| r.line_w / 20.0 / s)
}

impl TextCtx {
    pub fn new(font: Option<&Path>) -> Self {
        let mut ctx = Self::new_bare(font);
        ctx.load_variants();
        ctx
    }

    /// Main code font + UI family only: no bold/italic files, no fallbacks.
    fn new_bare(font: Option<&Path>) -> Self {
        let found = font.map(Path::to_path_buf).or_else(find_mono_font);
        let (mut fonts, family, ui_family, loaded) = single_font_system(found.as_deref());
        if fonts.db().faces().next().is_none() {
            // cosmic-text would panic ("no default font found") on the first shaped glyph.
            eprintln!(
                "lumen-text: no fonts found. Install fontconfig and a font (e.g. DejaVu Sans Mono: `apt install fonts-dejavu-core`, `dnf install dejavu-sans-mono-fonts`, `pacman -S ttf-dejavu`)."
            );
            std::process::exit(1);
        }
        let attrs = family_attrs(&family);
        let cell_w = measure_cell(&mut fonts, &attrs, font_size());
        let sys_all = family.is_none();
        let buf = TextBuf::new(&mut fonts, Metrics::new(font_size() * scale(), line_h() * scale()));
        let ui_buf = TextBuf::new(&mut fonts, Metrics::new(ui_size() * scale(), ui_line() * scale()));
        Self {
            fonts,
            swash: SwashCache::new(),
            buf,
            ui_buf,
            default_family: family.clone(),
            default_ui_family: ui_family.clone(),
            family,
            ui_family,
            cell_w,
            widths: Default::default(),
            icons: crate::icons::Icons::new(),
            loaded,
            fallbacks_loaded: false,
            rtl_loaded: false,
            italic_ok: None,
            buf_weight: 400,
            buf_ligs: false,
            buf_feats: Vec::new(),
            buf_spacing: 0.0,
            ui_weight: 400,
            ui_ligs: true,
            user_faces: [Vec::new(), Vec::new()],
            sys_all,
            previews: Vec::new(),
        }
    }

    /// Load the bold / italic / bold-italic files of the code family (when the system has them).
    fn load_variants(&mut self) {
        if let Some(fam) = self.family.clone() {
            load_variants(self.fonts.db_mut(), &fam, &mut self.loaded);
        }
    }

    /// Load the curated fallback faces (color emoji, CJK, symbols) used for glyphs the code font
    /// lacks. Cheap no-op after the first call; called by every `draw*` when it first meets a
    /// char >= U+2000, so ASCII/Latin-only sessions never pay for it. Does nothing when the
    /// FontSystem already holds every system font.
    pub fn ensure_fallbacks(&mut self) {
        if self.fallbacks_loaded {
            return;
        }
        self.fallbacks_loaded = true;
        if self.sys_all {
            return;
        }
        for p in fallback_candidates(&self.loaded) {
            if !load_into(self.fonts.db_mut(), &p).is_empty() {
                self.loaded.push(p);
            }
        }
    }

    /// Load Arabic and Hebrew faces if the loaded fonts lack them. Cheap no-op after the first
    /// call; called when right-to-left text is first drawn, so other sessions never pay for it.
    pub fn ensure_rtl_fonts(&mut self) {
        if self.rtl_loaded {
            return;
        }
        self.rtl_loaded = true;
        if self.sys_all {
            return;
        }
        for p in rtl_candidates(&self.loaded) {
            if !load_into(self.fonts.db_mut(), &p).is_empty() {
                self.loaded.push(p);
            }
        }
    }

    /// Change the font size (clamped) and re-measure the cell width. Returns the size in use.
    pub fn set_zoom(&mut self, px: f32) -> f32 {
        let px = px.clamp(8.0, 40.0);
        crate::fontcfg::store_buffer_size(px);
        self.remeasure();
        px
    }

    /// Re-measure the buffer cell width (family, weight, size, letter spacing or scale changed).
    /// The letter spacing is part of every glyph's advance, so it is part of the cell too: cursor
    /// x positions, columns and box layout all follow.
    fn remeasure(&mut self) {
        let family = self.family.clone();
        let attrs = self.buffer_attrs(&family);
        self.cell_w = measure_cell(&mut self.fonts, &attrs, font_size()).max(1.0);
    }

    /// Buffer font attributes: family, weight and letter spacing (in em of the current size).
    /// The OpenType features are added where `Shaping::Advanced` runs (see [`buffer_features`]).
    fn buffer_attrs<'a>(&self, family: &'a Option<String>) -> Attrs<'a> {
        let a = buf_attrs(family, self.buf_weight);
        if self.buf_spacing == 0.0 { a } else { a.letter_spacing(self.buf_spacing / font_size()) }
    }

    /// Does buffer text need `Shaping::Advanced` even when it is plain ASCII (ligatures or
    /// OpenType features are on)?
    fn buffer_fancy(&self) -> bool {
        self.buf_ligs || !self.buf_feats.is_empty()
    }

    pub fn reset_zoom(&mut self) -> f32 {
        self.set_zoom(DEFAULT_FONT_SIZE)
    }

    /// Fresh font system on the same database: every shaping / font-match / glyph cache starts empty.
    fn rebuild_fonts(&mut self) {
        let db = std::mem::take(self.fonts.db_mut());
        self.fonts = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
        self.swash = SwashCache::new();
        self.widths.clear();
        self.italic_ok = None;
        crate::fontcfg::bump();
    }

    /// Load the files of `name` for font group `g` (0 = buffer, 1 = UI), dropping what the group
    /// loaded before. Returns the family name as the database spells it, `None` when it is missing.
    fn install_family(&mut self, name: &str, g: usize) -> Option<String> {
        let old = std::mem::take(&mut self.user_faces[g]);
        if !old.is_empty() {
            let db = self.fonts.db_mut();
            for id in old {
                db.remove_face(id);
            }
        }
        if name.is_empty() {
            return None;
        }
        if !self.sys_all {
            let files = crate::fontcfg::family_files(name, 16, 40 * 1024 * 1024);
            let mut added = Vec::new();
            for f in files {
                added.extend(load_into(self.fonts.db_mut(), &f));
            }
            self.user_faces[g] = added;
        }
        self.fonts
            .db()
            .faces()
            .flat_map(|f| f.families.iter().map(|(n, _)| n))
            .find(|n| n.eq_ignore_ascii_case(name))
            .cloned()
    }

    /// Apply the buffer font group (family, size, weight, line height, ligatures) live. Returns
    /// false when the family is not installed: the system monospace is used instead.
    pub fn set_buffer_font(&mut self, spec: &crate::fontcfg::FontSpec) -> bool {
        let found = self.install_family(&spec.family, 0);
        let ok = spec.family.is_empty() || found.is_some();
        self.family = found.or_else(|| self.default_family.clone());
        self.buf_weight = spec.weight;
        self.buf_ligs = spec.ligatures;
        self.buf_feats =
            spec.features.iter().filter_map(|(k, v)| <[u8; 4]>::try_from(k.as_bytes()).ok().map(|t| (t, *v))).collect();
        self.buf_spacing =
            spec.letter_spacing.clamp(crate::fontcfg::LETTER_SPACING_RANGE.0, crate::fontcfg::LETTER_SPACING_RANGE.1);
        crate::fontcfg::store_buffer_size(spec.size.clamp(8.0, 40.0));
        crate::fontcfg::store_buffer_line_height(spec.line_height);
        self.rebuild_fonts();
        self.remeasure();
        ok
    }

    /// Apply the UI font group live (family, size, weight, ligatures; the line height only scales
    /// multi-line prose, see `fontcfg::prose_scale`). False when the family is not installed.
    pub fn set_ui_font(&mut self, spec: &crate::fontcfg::FontSpec) -> bool {
        let found = self.install_family(&spec.family, 1);
        let ok = spec.family.is_empty() || found.is_some();
        self.ui_family = found.or_else(|| self.default_ui_family.clone());
        self.ui_weight = spec.weight;
        self.ui_ligs = spec.ligatures;
        crate::fontcfg::store_ui_size(spec.size);
        crate::fontcfg::store_ui_line_height(spec.line_height);
        self.rebuild_fonts();
        ok
    }

    /// Make `family` drawable by [`draw_family`](Self::draw_family): loads its preview face once.
    /// False for fonts that are missing or too big to load just for a preview.
    pub fn preview_ready(&mut self, family: &str, file: &Path) -> bool {
        if self.sys_all || self.previews.iter().any(|(n, _)| n == family) {
            return true;
        }
        if std::fs::metadata(file).map_or(true, |m| m.len() > 12 * 1024 * 1024) {
            return false;
        }
        if self.previews.len() >= 48 {
            self.clear_previews();
        }
        let ids = load_into(self.fonts.db_mut(), file);
        let ok = !ids.is_empty();
        self.previews.push((family.to_string(), ids));
        ok
    }

    /// Drop every preview face (picker closed): memory goes back to what the editor needs.
    pub fn clear_previews(&mut self) {
        if self.previews.is_empty() {
            return;
        }
        let db = self.fonts.db_mut();
        for (_, ids) in self.previews.drain(..) {
            for id in ids {
                db.remove_face(id);
            }
        }
        self.rebuild_fonts();
        self.remeasure();
    }

    /// Draw `text` in the named family at `size` px (a font preview); the UI font stands in for
    /// families that could not be loaded.
    pub fn draw_family(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        family: &str,
        text: &str,
        color: Rgba,
        clip: Rect,
        size: f32,
    ) {
        let a = Attrs::new().family(Family::Name(family)).color(to_color(color));
        self.draw_family_attrs(cv, x, y, text, &a, clip, size);
    }

    /// Draw `text` in `family` at `size` px with the buffer font's OpenType features and letter
    /// spacing applied: the buffer font picker's preview of what the code will look like.
    pub fn draw_family_buffer(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        family: &str,
        text: &str,
        color: Rgba,
        clip: Rect,
        size: f32,
    ) {
        let mut a = Attrs::new()
            .family(Family::Name(family))
            .color(to_color(color))
            .font_features(buffer_features(self.buf_ligs, &self.buf_feats));
        if self.buf_spacing != 0.0 {
            a = a.letter_spacing(self.buf_spacing / size.max(1.0));
        }
        self.draw_family_attrs(cv, x, y, text, &a, clip, size);
    }

    fn draw_family_attrs(&mut self, cv: &mut Canvas, x: i32, y: i32, text: &str, a: &Attrs, clip: Rect, size: f32) {
        if text.is_empty() || clip.w <= 0 || clip.h <= 0 {
            return;
        }
        let lh = (size * 1.4).round();
        let s = scale();
        let mut b = TextBuf::new(&mut self.fonts, Metrics::new(size * s, lh * s));
        b.set_size(Some(4000.0 * s), Some(lh * 2.0 * s));
        b.set_wrap(Wrap::None);
        b.set_text(text, a, Shaping::Advanced, Some(Align::Left));
        b.shape_until_scroll(&mut self.fonts, false);
        let clip = cv.to_dev(clip.intersect(&cv.bounds())).intersect(&cv.dev_bounds());
        let (x, y) = (sc(x), sc(y));
        let (w, px) = (cv.w, &mut *cv.px);
        b.draw(&mut self.fonts, &mut self.swash, Color::rgba(255, 255, 255, 255), |gx, gy, gw, gh, c| {
            for dy in 0..gh as i32 {
                let py = gy + dy + y;
                if py < clip.y || py >= clip.bottom() {
                    continue;
                }
                for dx in 0..gw as i32 {
                    let pxx = gx + dx + x;
                    if pxx < clip.x || pxx >= clip.right() {
                        continue;
                    }
                    let i = (py * w + pxx) as usize;
                    px[i] = blend_px(px[i], Rgba(c.r(), c.g(), c.b(), c.a()));
                }
            }
        });
    }

    /// Width of `n` buffer cells for ligature tests: the shaped width of `text` in the buffer font.
    #[cfg(test)]
    pub fn buffer_text_width(&mut self, text: &str) -> f32 {
        let family = self.family.clone();
        let mut attrs = self.buffer_attrs(&family);
        let fancy = self.buffer_fancy();
        if fancy || !self.buf_ligs {
            attrs = attrs.font_features(buffer_features(self.buf_ligs, &self.buf_feats));
        }
        let s = scale();
        let mut b = TextBuf::new(&mut self.fonts, Metrics::new(font_size() * s, line_h() * s));
        b.set_size(Some(4000.0), Some(200.0));
        b.set_text(text, &attrs, if fancy { Shaping::Advanced } else { Shaping::Basic }, None);
        b.shape_until_scroll(&mut self.fonts, false);
        b.layout_runs().next().map_or(0.0, |r| r.line_w) / s
    }

    /// Change the UI scale (hi-DPI). Re-measures the code cell width and drops the width cache,
    /// which is in logical pixels but was measured at the old device size. Returns the scale in use.
    pub fn set_scale(&mut self, s: f32) -> f32 {
        let s = store_scale(s);
        self.widths.clear();
        self.remeasure();
        crate::fontcfg::bump();
        s
    }

    /// Draw colored runs with their top-left at (x, y); '\n' inside a run starts a new line.
    /// Everything is clipped to `clip`.
    pub fn draw(&mut self, cv: &mut Canvas, x: i32, y: i32, pieces: &[(String, Rgba)], clip: Rect) {
        let spans: Vec<(&str, Rgba, u8)> = pieces.iter().map(|(s, c)| (s.as_str(), *c, 0)).collect();
        self.draw_spans(cv, x, y, &spans, clip);
    }

    /// Like [`draw`](Self::draw) with a style per piece: [`BOLD`], [`ITALIC`], [`UNDERLINE`] flags.
    /// Bold/italic use the real font files when the system has them; without an italic file
    /// cosmic-text slants the upright glyphs synthetically (`CacheKeyFlags::FAKE_ITALIC`).
    pub fn draw_styled(&mut self, cv: &mut Canvas, x: i32, y: i32, pieces: &[(String, Rgba, u8)], clip: Rect) {
        let spans: Vec<(&str, Rgba, u8)> = pieces.iter().map(|(s, c, f)| (s.as_str(), *c, *f)).collect();
        self.draw_spans(cv, x, y, &spans, clip);
    }

    /// Draw an inlay hint label: the code font at `inlay::HINT_SCALE` of its size, italic. (x, y)
    /// is the top-left of the *row*, so the smaller text keeps the baseline of the full-size text.
    pub fn draw_hint(&mut self, cv: &mut Canvas, x: i32, y: i32, text: &str, color: Rgba, clip: Rect) {
        let dy = (font_size() * (1.0 - crate::inlay::HINT_SCALE) * 0.35).round() as i32;
        self.draw_spans_scaled(cv, x, y + dy, &[(text, color, ITALIC)], clip, crate::inlay::HINT_SCALE);
    }

    fn draw_spans(&mut self, cv: &mut Canvas, x: i32, y: i32, pieces: &[(&str, Rgba, u8)], clip: Rect) {
        self.draw_spans_scaled(cv, x, y, pieces, clip, 1.0);
    }

    fn draw_spans_scaled(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        pieces: &[(&str, Rgba, u8)],
        clip: Rect,
        mul: f32,
    ) {
        if pieces.is_empty() || clip.w <= 0 || clip.h <= 0 {
            return;
        }
        if !self.fallbacks_loaded && pieces.iter().any(|(s, ..)| needs_fallback(s)) {
            self.ensure_fallbacks();
        }
        let wide = pieces.iter().any(|(s, ..)| needs_fallback(s));
        let fancy = self.buffer_fancy();
        let family = self.family.clone();
        let mut attrs = self.buffer_attrs(&family);
        if wide || fancy {
            attrs = attrs.font_features(buffer_features(self.buf_ligs, &self.buf_feats));
        }
        let s = scale();
        self.buf.set_metrics_and_size(
            Metrics::new(font_size() * s * mul, line_h() * s),
            Some(4000.0 * s),
            Some((clip.h as f32 + 2.0 * line_h()) * s),
        );
        self.buf.set_wrap(Wrap::None);
        let spans: Vec<(&str, Attrs)> = pieces.iter().map(|(s, c, f)| (*s, styled(&attrs, *c, *f))).collect();
        self.buf.set_rich_text(
            spans,
            &attrs,
            if wide || fancy { Shaping::Advanced } else { Shaping::Basic },
            Some(Align::Left),
        );
        let clip = cv.to_dev(clip.intersect(&cv.bounds())).intersect(&cv.dev_bounds());
        let (x, y) = (sc(x), sc(y));
        let (w, px) = (cv.w, &mut *cv.px);
        // Glyph pixels arrive as final RGBA: tinted with the piece color for mask glyphs, real
        // colors for color glyphs (emoji bitmaps); decoration rectangles come through here too.
        self.buf.draw(&mut self.fonts, &mut self.swash, Color::rgba(255, 255, 255, 255), |gx, gy, gw, gh, c| {
            for dy in 0..gh as i32 {
                let py = gy + dy + y;
                if py < clip.y || py >= clip.bottom() {
                    continue;
                }
                for dx in 0..gw as i32 {
                    let pxx = gx + dx + x;
                    if pxx < clip.x || pxx >= clip.right() {
                        continue;
                    }
                    let i = (py * w + pxx) as usize;
                    px[i] = blend_px(px[i], Rgba(c.r(), c.g(), c.b(), c.a()));
                }
            }
        });
    }

    /// Draw one row of bidirectional code on the cell grid (see `bidi`): `pieces` hold the row's
    /// text in logical order with its colors and styles (no tabs or newlines), and `cells[i]` is
    /// the (first cell, cell count) of its char `i`, cell 0 starting at `x`; `rtl` is the
    /// paragraph direction. The text is shaped as a whole (Arabic letters join, brackets mirror in
    /// right-to-left runs), then every cluster is centred on its own cells, so glyphs line up with
    /// the caret and the selection exactly as left-to-right code does. `y` is the row's top.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_cells(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        pieces: &[(String, Rgba, u8)],
        cells: &[(usize, usize)],
        rtl: bool,
        clip: Rect,
    ) {
        if pieces.is_empty() || cells.is_empty() || clip.w <= 0 || clip.h <= 0 {
            return;
        }
        self.ensure_fallbacks();
        self.ensure_rtl_fonts();
        let family = self.family.clone();
        let attrs = buf_attrs(&family, self.buf_weight).font_features(plain_features());
        // A direction mark first, so cosmic-text resolves the paragraph direction the caller laid
        // the row out with (it decides the direction of neutrals such as brackets).
        let mark = if rtl { "\u{200F}" } else { "\u{200E}" };
        let mut spans: Vec<(&str, Attrs)> = Vec::with_capacity(pieces.len() + 1);
        spans.push((mark, attrs.clone()));
        spans.extend(pieces.iter().map(|(t, c, f)| (t.as_str(), styled(&attrs, *c, *f))));
        // Byte offset in the text after the mark -> char index.
        let mut char_of: Vec<usize> = Vec::new();
        for (ci, ch) in pieces.iter().flat_map(|(t, ..)| t.chars()).enumerate() {
            char_of.extend(std::iter::repeat_n(ci, ch.len_utf8()));
        }
        if char_of.is_empty() {
            return;
        }
        let s = scale();
        self.buf.set_metrics_and_size(
            Metrics::new(font_size() * s, line_h() * s),
            Some(1.0e7),
            Some(line_h() * 2.0 * s),
        );
        self.buf.set_wrap(Wrap::None);
        self.buf.set_rich_text(spans, &attrs, Shaping::Advanced, Some(Align::Left));
        self.buf.shape_until_scroll(&mut self.fonts, false);
        let clip = cv.to_dev(clip.intersect(&cv.bounds())).intersect(&cv.dev_bounds());
        let cw = self.cell_w * s;
        let (ox, oy) = (sc(x) as f32, sc(y) as f32);
        let (w, px) = (cv.w, &mut *cv.px);
        let white = Color::rgba(255, 255, 255, 255);
        let m = mark.len();
        let last = char_of.len() - 1;
        for run in self.buf.layout_runs() {
            let gs = run.glyphs;
            let mut i = 0;
            while i < gs.len() {
                let (st, en) = (gs[i].start, gs[i].end);
                let mut j = i + 1;
                while j < gs.len() && gs[j].start == st {
                    j += 1;
                }
                let group = &gs[i..j];
                i = j;
                if st < m {
                    continue; // the direction mark
                }
                let a = char_of[(st - m).min(last)];
                let b = char_of[(en.max(st + 1) - 1 - m).min(last)];
                let (mut c0, mut c1) = (usize::MAX, 0usize);
                for c in a..=b {
                    if let Some(&(cx, cn)) = cells.get(c) {
                        c0 = c0.min(cx);
                        c1 = c1.max(cx + cn);
                    }
                }
                if c0 >= c1 {
                    continue;
                }
                let (mut g0, mut g1) = (f32::MAX, f32::MIN);
                for g in group {
                    g0 = g0.min(g.x);
                    g1 = g1.max(g.x + g.w);
                }
                let dx = ox + c0 as f32 * cw + ((c1 - c0) as f32 * cw - (g1 - g0)) / 2.0 - g0;
                for g in group {
                    let pg = g.physical((dx, oy + run.line_y), 1.0);
                    let (bx, by) = (pg.x, pg.y);
                    let color = g.color_opt.unwrap_or(white);
                    self.swash.with_pixels(&mut self.fonts, pg.cache_key, color, |gx, gy, c| {
                        let (pxx, py) = (bx + gx, by + gy);
                        if py < clip.y || py >= clip.bottom() || pxx < clip.x || pxx >= clip.right() {
                            return;
                        }
                        let k = (py * w + pxx) as usize;
                        px[k] = blend_px(px[k], Rgba(c.r(), c.g(), c.b(), c.a()));
                    });
                }
            }
        }
    }

    /// Width in px of `text` in the UI font.
    pub fn ui_width(&mut self, text: &str, bold: bool) -> i32 {
        if let Some(w) = self.widths.get(&(text.to_string(), bold)) {
            return w.round() as i32;
        }
        if !self.fallbacks_loaded && needs_fallback(text) {
            self.ensure_fallbacks();
        }
        let fam = self.ui_family.clone();
        let a = ui_attrs(&fam, self.ui_weight, self.ui_ligs, bold);
        let s = scale();
        let mut b = TextBuf::new(&mut self.fonts, Metrics::new(ui_size() * s, ui_line() * s));
        b.set_size(Some(8000.0 * s), Some(ui_line() * 2.0 * s));
        b.set_wrap(Wrap::None);
        b.set_text(text, &a, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fonts, false);
        let w = b.layout_runs().next().map_or(0.0, |r| r.line_w) / s;
        if self.widths.len() > 4000 {
            self.widths.clear();
        }
        self.widths.insert((text.to_string(), bold), w);
        w.round() as i32
    }

    /// X offset (px, unscaled) of every char boundary of `text` in the UI font: `n + 1` values, the
    /// last being the full width. `size` None = the standard UI size. Used to place a text
    /// selection per character.
    pub fn ui_char_xs(&mut self, text: &str, bold: bool, size: Option<f32>) -> Vec<f32> {
        let n = text.chars().count();
        if n == 0 {
            return vec![0.0];
        }
        if !self.fallbacks_loaded && needs_fallback(text) {
            self.ensure_fallbacks();
        }
        let (sz, lh) = match size {
            Some(sz) => (sz, (sz * 1.4).round()),
            None => (ui_size(), ui_line()),
        };
        let fam = self.ui_family.clone();
        let a = ui_attrs(&fam, self.ui_weight, self.ui_ligs, bold);
        let s = scale();
        let mut b = TextBuf::new(&mut self.fonts, Metrics::new(sz * s, lh * s));
        b.set_size(Some(8000.0 * s), Some(lh * 2.0 * s));
        b.set_wrap(Wrap::None);
        b.set_text(text, &a, Shaping::Advanced, Some(Align::Left));
        b.shape_until_scroll(&mut self.fonts, false);
        let mut idx = vec![0usize; text.len() + 1];
        for (ci, (bi, _)) in text.char_indices().enumerate() {
            idx[bi] = ci;
        }
        idx[text.len()] = n;
        let mut xs = vec![f32::NAN; n + 1];
        let mut total = 0.0f32;
        if let Some(run) = b.layout_runs().next() {
            total = run.line_w;
            for g in run.glyphs {
                let (s0, e0) = (idx[g.start.min(text.len())], idx[g.end.min(text.len())]);
                let k = e0.saturating_sub(s0).max(1);
                for j in 0..k {
                    if s0 + j < n {
                        xs[s0 + j] = g.x + g.w * j as f32 / k as f32;
                    }
                }
            }
        }
        xs[n] = total;
        let mut last = 0.0f32;
        for x in xs.iter_mut() {
            if x.is_nan() {
                *x = last;
            }
            last = *x;
        }
        xs.iter().map(|x| x / s).collect()
    }

    /// Draw UI-font runs on one line at (x, y) (top-left), clipped.
    pub fn draw_ui_pieces(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        pieces: &[(String, Rgba)],
        clip: Rect,
        bold: bool,
    ) {
        self.draw_ui_pieces_i(cv, x, y, pieces, clip, bold, false);
    }

    /// [`draw_ui_pieces`](Self::draw_ui_pieces) with an `italic` switch. The UI family has no italic
    /// face, so cosmic-text slants the upright glyphs (`CacheKeyFlags::FAKE_ITALIC`); advances are
    /// unchanged, which keeps `ui_width` valid for italic text.
    pub fn draw_ui_pieces_i(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        pieces: &[(String, Rgba)],
        clip: Rect,
        bold: bool,
        italic: bool,
    ) {
        if pieces.is_empty() || clip.w <= 0 || clip.h <= 0 {
            return;
        }
        if !self.fallbacks_loaded && pieces.iter().any(|(s, _)| needs_fallback(s)) {
            self.ensure_fallbacks();
        }
        let fam = self.ui_family.clone();
        let mut base = ui_attrs(&fam, self.ui_weight, self.ui_ligs, bold);
        if italic {
            base = base.style(cosmic_text::Style::Italic);
        }
        let s = scale();
        self.ui_buf.set_metrics_and_size(
            Metrics::new(ui_size() * s, ui_line() * s),
            Some(4000.0 * s),
            Some(ui_line() * 2.0 * s),
        );
        self.ui_buf.set_wrap(Wrap::None);
        let spans: Vec<(&str, Attrs)> =
            pieces.iter().map(|(s, c)| (s.as_str(), base.clone().color(to_color(*c)))).collect();
        self.ui_buf.set_rich_text(spans, &base, Shaping::Advanced, Some(Align::Left));
        let clip = cv.to_dev(clip.intersect(&cv.bounds())).intersect(&cv.dev_bounds());
        let (x, y) = (sc(x), sc(y));
        let (w, px) = (cv.w, &mut *cv.px);
        self.ui_buf.draw(&mut self.fonts, &mut self.swash, Color::rgba(255, 255, 255, 255), |gx, gy, gw, gh, c| {
            for dy in 0..gh as i32 {
                let py = gy + dy + y;
                if py < clip.y || py >= clip.bottom() {
                    continue;
                }
                for dx in 0..gw as i32 {
                    let pxx = gx + dx + x;
                    if pxx < clip.x || pxx >= clip.right() {
                        continue;
                    }
                    let i = (py * w + pxx) as usize;
                    px[i] = blend_px(px[i], Rgba(c.r(), c.g(), c.b(), c.a()));
                }
            }
        });
    }

    pub fn draw_ui(&mut self, cv: &mut Canvas, x: i32, y: i32, text: &str, color: Rgba, clip: Rect, bold: bool) {
        self.draw_ui_pieces(cv, x, y, &[(text.to_string(), color)], clip, bold);
    }

    /// [`draw_ui`](Self::draw_ui) in (synthetic) italic.
    pub fn draw_ui_italic(&mut self, cv: &mut Canvas, x: i32, y: i32, text: &str, color: Rgba, clip: Rect, bold: bool) {
        self.draw_ui_pieces_i(cv, x, y, &[(text.to_string(), color)], clip, bold, true);
    }

    /// True when italic UI text really renders slanted (the synthetic slant works). Measured once
    /// by drawing the same word upright and italic; callers keep a lighter-color fallback for the
    /// case where it does not (a font stack that refuses the transform).
    pub fn italic_works(&mut self) -> bool {
        if let Some(v) = self.italic_ok {
            return v;
        }
        let white = Rgba(255, 255, 255, 255);
        let clip = Rect::new(0, 0, 80, 30);
        let render = |ctx: &mut TextCtx, italic: bool| {
            let mut px = vec![0u32; 80 * 30];
            let mut cv = Canvas { px: &mut px, w: 80, h: 30 };
            ctx.draw_ui_pieces_i(&mut cv, 2, 2, &[("Hlfi".to_string(), white)], clip, false, italic);
            px
        };
        let (a, b) = (render(self, false), render(self, true));
        let v = a != b;
        self.italic_ok = Some(v);
        v
    }

    /// Draw an icon (see `icons.rs`) tinted with `color`. Size is in pixels and independent of the
    /// code font size, so zooming the code never changes the interface.
    pub fn icon(&mut self, cv: &mut Canvas, name: &str, x: i32, y: i32, size: i32, color: Rgba, clip: Rect) {
        self.icons.draw(cv, name, x, y, size, color, clip);
    }

    pub fn draw_line(&mut self, cv: &mut Canvas, x: i32, y: i32, text: &str, color: Rgba, clip: Rect) {
        self.draw(cv, x, y, &[(text.to_string(), color)], clip);
    }

    /// Width in pixels of `n` monospace cells.
    pub fn cells(&self, n: usize) -> i32 {
        (n as f32 * self.cell_w).round() as i32
    }

    /// Draw `text` in the UI font at an arbitrary `size` (line height `round(size * 1.4)`) with its
    /// top-left at (x, y), clipped. Same family / weight rules as [`draw_ui`](Self::draw_ui).
    pub fn draw_ui_sized(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        clip: Rect,
        bold: bool,
        size: f32,
    ) {
        self.draw_ui_sized_i(cv, x, y, text, color, clip, bold, false, size);
    }

    /// [`draw_ui_sized`](Self::draw_ui_sized) with an `italic` switch (synthetic slant, see
    /// [`draw_ui_pieces_i`](Self::draw_ui_pieces_i)).
    pub fn draw_ui_sized_i(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        clip: Rect,
        bold: bool,
        italic: bool,
        size: f32,
    ) {
        if text.is_empty() || clip.w <= 0 || clip.h <= 0 {
            return;
        }
        if !self.fallbacks_loaded && needs_fallback(text) {
            self.ensure_fallbacks();
        }
        let lh = (size * 1.4).round();
        let fam = self.ui_family.clone();
        let a = ui_attrs(&fam, self.ui_weight, self.ui_ligs, bold);
        let mut a = a;
        if italic {
            a = a.style(cosmic_text::Style::Italic);
        }
        let a = a.color(to_color(color));
        let s = scale();
        let mut b = TextBuf::new(&mut self.fonts, Metrics::new(size * s, lh * s));
        b.set_size(Some(4000.0 * s), Some(lh * 2.0 * s));
        b.set_wrap(Wrap::None);
        b.set_text(text, &a, Shaping::Advanced, Some(Align::Left));
        b.shape_until_scroll(&mut self.fonts, false);
        let clip = cv.to_dev(clip.intersect(&cv.bounds())).intersect(&cv.dev_bounds());
        let (x, y) = (sc(x), sc(y));
        let (w, px) = (cv.w, &mut *cv.px);
        b.draw(&mut self.fonts, &mut self.swash, Color::rgba(255, 255, 255, 255), |gx, gy, gw, gh, c| {
            for dy in 0..gh as i32 {
                let py = gy + dy + y;
                if py < clip.y || py >= clip.bottom() {
                    continue;
                }
                for dx in 0..gw as i32 {
                    let pxx = gx + dx + x;
                    if pxx < clip.x || pxx >= clip.right() {
                        continue;
                    }
                    let i = (py * w + pxx) as usize;
                    px[i] = blend_px(px[i], Rgba(c.r(), c.g(), c.b(), c.a()));
                }
            }
        });
    }

    /// Width in px of `text` in the UI font at `size` (uncached; callers cache).
    pub fn ui_width_sized(&mut self, text: &str, bold: bool, size: f32) -> i32 {
        if text.is_empty() {
            return 0;
        }
        if !self.fallbacks_loaded && needs_fallback(text) {
            self.ensure_fallbacks();
        }
        let lh = (size * 1.4).round();
        let fam = self.ui_family.clone();
        let a = ui_attrs(&fam, self.ui_weight, self.ui_ligs, bold);
        let s = scale();
        let mut b = TextBuf::new(&mut self.fonts, Metrics::new(size * s, lh * s));
        b.set_size(Some(8000.0 * s), Some(lh * 2.0 * s));
        b.set_wrap(Wrap::None);
        b.set_text(text, &a, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fonts, false);
        (b.layout_runs().next().map_or(0.0, |r| r.line_w) / s).round() as i32
    }

    /// Baseline of UI text drawn by [`draw_ui_sized`](Self::draw_ui_sized) at `size`: logical px
    /// below the `y` passed to it (inline math sits on it).
    pub fn ui_baseline_sized(&mut self, size: f32) -> f32 {
        let lh = (size * 1.4).round();
        let fam = self.ui_family.clone();
        let a = ui_attrs(&fam, self.ui_weight, self.ui_ligs, false);
        let s = scale();
        let mut b = TextBuf::new(&mut self.fonts, Metrics::new(size * s, lh * s));
        b.set_size(Some(4000.0 * s), Some(lh * 2.0 * s));
        b.set_wrap(Wrap::None);
        b.set_text("x", &a, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fonts, false);
        b.layout_runs().next().map_or(size * 1.1, |r| r.line_y / s)
    }
}

/// `base` with the piece's color and BOLD / ITALIC / UNDERLINE flags applied.
fn styled<'a>(base: &Attrs<'a>, c: Rgba, flags: u8) -> Attrs<'a> {
    let mut a = base.clone().color(to_color(c));
    if flags & BOLD != 0 {
        a = a.weight(cosmic_text::Weight(crate::fontcfg::bold_weight(base.weight.0, 300, 700)));
    }
    if flags & ITALIC != 0 {
        a = a.style(cosmic_text::Style::Italic);
    }
    if flags & UNDERLINE != 0 {
        a = a.underline(UnderlineStyle::Single);
    }
    a
}

/// Buffer font attributes: the family (or the system monospace) at the configured weight.
fn buf_attrs(family: &Option<String>, weight: u16) -> Attrs<'_> {
    family_attrs(family).weight(cosmic_text::Weight(weight))
}

/// Interface font attributes; `bold` is the semibold run weight, `ligs` switches `liga`/`calt` off when false.
fn ui_attrs(family: &Option<String>, weight: u16, ligs: bool, bold: bool) -> Attrs<'_> {
    let mut a = match family {
        Some(f) => Attrs::new().family(Family::Name(f)),
        None => Attrs::new().family(Family::SansSerif),
    };
    let w = if bold { crate::fontcfg::bold_weight(weight, 200, 600) } else { weight };
    a = a.weight(cosmic_text::Weight(w));
    if !ligs {
        a = a.font_features(plain_features());
    }
    a
}

fn family_attrs(family: &Option<String>) -> Attrs<'_> {
    match family {
        Some(f) => Attrs::new().family(Family::Name(f)),
        None => Attrs::new().family(Family::Monospace),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_priority_and_parsing() {
        let mut d = Dpi { device: 1.5, forced: None };
        assert_eq!(d.effective(0.0), 1.5, "automatic follows the window");
        assert_eq!(d.effective(2.0), 2.0, "config overrides the window");
        d.forced = Some(1.25);
        assert_eq!(d.effective(2.0), 1.25, "--scale / LUMEN_SCALE beats the config");
        assert_eq!(parse_scale("1.75"), Some(1.75));
        assert_eq!(parse_scale("0"), None);
        assert_eq!(parse_scale("abc"), None);
    }

    #[test]
    fn scale_one_is_identity_for_rects() {
        // (the global scale is 1.0 unless a test changes it; none does)
        let mut px = vec![0u32; 20 * 20];
        let cv = Canvas { px: &mut px, w: 20, h: 20 };
        assert_eq!(cv.to_dev(Rect::new(3, 4, 5, 6)), Rect::new(3, 4, 5, 6));
        assert_eq!(cv.bounds(), Rect::new(0, 0, 20, 20));
        assert_eq!(logical_size(1000, 700), (1000, 700));
    }

    fn has(ctx: &TextCtx, bold: bool, italic: bool) -> bool {
        let fam = ctx.family.as_deref().unwrap_or("");
        ctx.fonts.db().faces().any(|f| {
            f.families.iter().any(|(n, _)| n == fam)
                && (f.weight.0 >= 600) == bold
                && (f.style != fontdb::Style::Normal) == italic
        })
    }

    /// Shape `text` with the code family under `attrs`; returns (glyph_id, font_id) per glyph.
    fn shape(ctx: &mut TextCtx, text: &str, flags: u8) -> Vec<(u16, fontdb::ID)> {
        let fam = ctx.family.clone();
        let wide = needs_fallback(text);
        let mut base = family_attrs(&fam);
        if wide {
            base = base.font_features(plain_features());
        }
        let attrs = styled(&base, Rgba(255, 255, 255, 255), flags);
        let mut b = TextBuf::new(&mut ctx.fonts, Metrics::new(14.0, 20.0));
        b.set_size(Some(2000.0), Some(200.0));
        b.set_wrap(Wrap::None);
        b.set_rich_text([(text, attrs.clone())], &attrs, if wide { Shaping::Advanced } else { Shaping::Basic }, None);
        b.shape_until_scroll(&mut ctx.fonts, false);
        b.layout_runs().flat_map(|r| r.glyphs.iter().map(|g| (g.glyph_id, g.font_id)).collect::<Vec<_>>()).collect()
    }

    fn canvas_ink(ctx: &mut TextCtx, pieces: &[(String, Rgba, u8)]) -> (Vec<u32>, usize) {
        let (w, h) = (240, 40);
        let mut px = vec![0u32; (w * h) as usize];
        let mut cv = Canvas { px: &mut px, w, h };
        ctx.draw_styled(&mut cv, 2, 2, pieces, Rect::new(0, 0, w, h));
        let ink = px.iter().filter(|p| **p != 0).count();
        (px, ink)
    }

    #[test]
    fn variants_have_identical_cell_width() {
        let mut ctx = TextCtx::new(None);
        let Some(fam) = ctx.family.clone() else { return };
        let regular = ctx.cell_w;
        let mut seen = 0;
        for (bold, italic, flags) in [(true, false, BOLD), (false, true, ITALIC), (true, true, BOLD | ITALIC)] {
            if !has(&ctx, bold, italic) {
                eprintln!("[ui test] no real face for {fam} bold={bold} italic={italic}; skipped");
                continue;
            }
            seen += 1;
            let fam_opt = Some(fam.clone());
            let attrs = styled(&family_attrs(&fam_opt), Rgba(0, 0, 0, 255), flags);
            let w = measure_cell(&mut ctx.fonts, &attrs, font_size());
            assert!((w - regular).abs() < 0.01, "{fam} flags={flags}: cell {w} != regular {regular}");
            // And the layout really uses a different face than the regular one.
            let reg = shape(&mut ctx, "A", 0)[0].1;
            let var = shape(&mut ctx, "A", flags)[0].1;
            assert_ne!(reg, var, "{fam} flags={flags} still rendered with the regular face");
        }
        eprintln!("[ui test] {fam}: {seen} style variants loaded, cell_w {regular}");
    }

    #[test]
    fn styled_drawing_changes_pixels() {
        let mut ctx = TextCtx::new(None);
        let white = Rgba(255, 255, 255, 255);
        let (plain, _) = canvas_ink(&mut ctx, &[("iiHH".into(), white, 0)]);
        let (bold, _) = canvas_ink(&mut ctx, &[("iiHH".into(), white, BOLD)]);
        let (ital, _) = canvas_ink(&mut ctx, &[("iiHH".into(), white, ITALIC)]);
        let (under, ink_u) = canvas_ink(&mut ctx, &[("iiHH".into(), white, UNDERLINE)]);
        let (_, ink_p) = canvas_ink(&mut ctx, &[("iiHH".into(), white, 0)]);
        assert!(ink_u > ink_p, "underline must add pixels");
        assert_ne!(plain, under);
        // Italic is real or synthetic slant either way: pixels must differ. Bold only if a face exists.
        assert_ne!(plain, ital, "italic (real or synthetic) must change the rendering");
        if has(&ctx, true, false) {
            assert_ne!(plain, bold, "bold face must change the rendering");
        }
        // The plain path stays identical to draw().
        let mut px = vec![0u32; 240 * 40];
        let mut cv = Canvas { px: &mut px, w: 240, h: 40 };
        ctx.draw(&mut cv, 2, 2, &[("iiHH".into(), white)], Rect::new(0, 0, 240, 40));
        assert_eq!(px, plain);
    }

    #[test]
    fn ui_italic_is_slanted_not_just_recolored() {
        let mut ctx = TextCtx::new(None);
        let white = Rgba(255, 255, 255, 255);
        let clip = Rect::new(0, 0, 160, 30);
        let mut render = |italic: bool, bold: bool| {
            let mut px = vec![0u32; 160 * 30];
            let mut cv = Canvas { px: &mut px, w: 160, h: 30 };
            ctx.draw_ui_pieces_i(&mut cv, 2, 4, &[("Illl lIIl iil".to_string(), white)], clip, bold, italic);
            px
        };
        let (up, it, itb) = (render(false, false), render(true, false), render(true, true));
        assert_ne!(up, it, "the synthetic slant must change the pixels");
        assert_ne!(it, itb);
        // Same ink amount (+-30%), same colour: it is a shear, not a dimmed copy.
        let ink = |p: &Vec<u32>| p.iter().filter(|v| **v != 0).count() as f64;
        let ratio = ink(&it) / ink(&up);
        assert!((0.7..1.3).contains(&ratio), "ink ratio {ratio}");
        // Vertical stems lean right at the top: the ink centroid of the top third moves right of the
        // bottom third's by more than in the upright text.
        let third = |p: &Vec<u32>, top: bool| -> f64 {
            let rows: Vec<usize> = (0..30).filter(|r| (0..160).any(|x| p[r * 160 + x] != 0)).collect();
            let n = (rows.len() / 3).max(1);
            let sel: Vec<usize> = if top { rows[..n].to_vec() } else { rows[rows.len() - n..].to_vec() };
            let (mut w, mut sx) = (0.0, 0.0);
            for r in sel {
                for x in 0..160 {
                    let a = (p[r * 160 + x] & 0xff) as f64;
                    w += a;
                    sx += a * x as f64;
                }
            }
            sx / w.max(1.0)
        };
        let lean = |p: &Vec<u32>| third(p, true) - third(p, false);
        assert!(lean(&it) > lean(&up) + 0.8, "italic leans right: {} vs {}", lean(&it), lean(&up));
        assert!(ctx.italic_works());
    }

    fn covered_by_loaded(ctx: &TextCtx, ch: char) -> bool {
        files_covering(ch as u32).iter().any(|f| ctx.loaded.contains(f))
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn fallbacks_leave_no_tofu() {
        let mut ctx = TextCtx::new(None);
        if ctx.family.is_none() {
            return;
        }
        ctx.ensure_fallbacks();
        eprintln!("[ui test] loaded font files: {:?}", ctx.loaded);
        let mut checked = 0;
        for ch in ['✓', '→', '日', '本', '😀', '─', '∀'] {
            if !covered_by_loaded(&ctx, ch) {
                eprintln!("[ui test] no loaded font covers {ch:?}; skipped");
                continue;
            }
            let s = format!("a{ch}b");
            let glyphs = shape(&mut ctx, &s, 0);
            assert!(glyphs.iter().all(|(id, _)| *id != 0), "tofu for {ch:?}: {glyphs:?}");
            checked += 1;
        }
        eprintln!("[ui test] tofu check passed for {checked} chars");
        assert!(ctx.loaded.len() <= 1 + 2 + 3 + MAX_FALLBACKS + 2);
    }

    #[test]
    fn emoji_keep_their_own_colors() {
        let mut ctx = TextCtx::new(None);
        // draw() must lazily load the fallbacks by itself.
        let (px, _) = canvas_ink(&mut ctx, &[("😀".into(), Rgba(255, 255, 255, 255), 0)]);
        // Only meaningful with a *color* emoji font (e.g. Noto Color Emoji); CI runners often have
        // a monochrome font that merely covers the code point.
        let color_font =
            ctx.fonts.db().faces().any(|f| f.families.iter().any(|(n, _)| n.to_lowercase().contains("emoji")));
        // CI runners ship emoji fonts in formats this renderer may not rasterize in color, so the
        // check is only run on developer machines.
        if std::env::var_os("CI").is_some() || !covered_by_loaded(&ctx, '😀') || !color_font {
            eprintln!("[ui test] no color emoji font loaded; skipped");
            return;
        }
        let colorful = px.iter().any(|p| {
            let (r, g, b) = ((p >> 16) & 255, (p >> 8) & 255, p & 255);
            r.max(g).max(b) - r.min(g).min(b) > 60
        });
        assert!(colorful, "emoji drawn as a tinted mask instead of color bitmap");
    }

    #[test]
    fn lazy_trigger_is_cheap_and_precise() {
        assert!(!needs_fallback("fn main() { é ü ñ ß }"));
        assert!(!needs_fallback("привет αβγ"));
        assert!(needs_fallback("a → b"));
        assert!(needs_fallback("😀"));
        assert!(needs_fallback("日本"));
    }

    /// Prints PSS at each loading stage (run with `--nocapture --test-threads=1` for clean
    /// numbers: parallel tests allocate too). Asserts the +6 MB budget for variants + fallbacks.
    /// Process-wide PSS is meaningless while ~900 tests run in parallel, so this only runs in the
    /// single-threaded performance job (`cargo test --release memory_budget -- --ignored --test-threads=1`).
    #[test]
    #[ignore = "process-wide PSS: run single-threaded (performance job)"]
    fn memory_budget() {
        let Some(m0) = crate::mem::snapshot() else { return };
        let mut ctx = TextCtx::new_bare(None);
        if ctx.family.is_none() {
            return;
        }
        let white = Rgba(255, 255, 255, 255);
        canvas_ink(&mut ctx, &[("fn main() { println!(\"hi\"); }".into(), white, 0)]);
        let m1 = crate::mem::snapshot().unwrap();
        ctx.load_variants();
        canvas_ink(
            &mut ctx,
            &[("fn".into(), white, BOLD), (" main".into(), white, ITALIC), ("()".into(), white, BOLD | ITALIC)],
        );
        let m2 = crate::mem::snapshot().unwrap();
        ctx.ensure_fallbacks();
        canvas_ink(&mut ctx, &[("a ✓ → 日本 😀 ─".into(), white, 0)]);
        let m3 = crate::mem::snapshot().unwrap();
        let mb = |a: u64, b: u64| (b as f64 - a as f64) / 1024.0;
        eprintln!(
            "[ui mem] PSS start {:.1} MB | single font +{:.1} | + variants +{:.1} | + fallbacks (drawn once) +{:.1} | total vs single-font setup +{:.1} MB",
            m0.pss_kb as f64 / 1024.0,
            mb(m0.pss_kb, m1.pss_kb),
            mb(m1.pss_kb, m2.pss_kb),
            mb(m2.pss_kb, m3.pss_kb),
            mb(m1.pss_kb, m3.pss_kb),
        );
        assert!(mb(m1.pss_kb, m3.pss_kb) < 6.0, "variants + fallbacks exceed the +6 MB budget");
    }

    #[test]
    fn vbar_geometry_and_drag_roundtrip() {
        let track = Rect::new(100, 10, 14, 400);
        assert!(VBar::new(track, 30.0, 40.0, 0.0).is_none(), "fits: no bar");
        let b = VBar::new(track, 1000.0, 50.0, 0.0).unwrap();
        assert_eq!(b.thumb.y, track.y);
        assert!(b.thumb.h >= 32 && b.thumb.h < track.h);
        let end = VBar::new(track, 1000.0, 50.0, 950.0).unwrap();
        assert_eq!(end.thumb.bottom(), track.bottom());
        // Dragging the thumb to the middle of the track maps to the middle of the scroll range.
        let mid = b.pos_for(track.y as f64 + track.h as f64 / 2.0, b.thumb.h as f64 / 2.0, 1000.0, 50.0);
        assert!((mid - 475.0).abs() < 8.0, "{mid}");
        assert_eq!(b.pos_for(-500.0, 0.0, 1000.0, 50.0), 0.0);
        assert_eq!(b.pos_for(9999.0, 0.0, 1000.0, 50.0), 950.0);
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn fit_ellipsis_cuts_on_char_boundaries_and_fits() {
        let mut tx = TextCtx::new(None);
        let s = "ção-über-日本語-component.tsx";
        let full = tx.ui_width(s, false);
        if full == 0 {
            return; // no UI font on this machine
        }
        assert_eq!(fit_ellipsis(&mut tx, s, full, false), s, "fits: unchanged");
        assert_eq!(fit_ellipsis(&mut tx, s, 0, false), "");
        for w in (1..full).step_by(3) {
            let f = fit_ellipsis(&mut tx, s, w, false);
            if f.is_empty() {
                assert!(tx.ui_width("…", false) > w, "empty only when even the ellipsis is too wide");
                continue;
            }
            assert!(f.ends_with('…'), "{f:?}");
            assert!(tx.ui_width(&f, false) <= w, "{f:?} wider than {w}");
            assert!(s.starts_with(f.trim_end_matches('…')), "{f:?} is a prefix");
        }
        // More room never shows less of the name.
        let a = fit_ellipsis(&mut tx, s, full / 3, false).chars().count();
        let b = fit_ellipsis(&mut tx, s, full * 2 / 3, false).chars().count();
        assert!(b >= a);
    }
}
