//! Image tabs: a PNG, JPEG, GIF, WebP or SVG file opens as a picture instead of its bytes.
//!
//! * Decoding reuses the Markdown preview's decoders ([`mdimg`]: `png`, `zune-jpeg`, `gif`,
//!   `image-webp` and the small SVG rasterizer on `tiny-skia`), on a worker thread.
//! * A raster image is decoded once and kept at its natural size, or, past [`BASE_PX`] pixels,
//!   downscaled right after that one decode (the full-size buffer is dropped at once). An SVG is
//!   rasterized at the size it is shown (at most [`SVG_PX`]), from the text of the tab, so edits
//!   made in "Source" show in "Preview" without saving. Everything goes with the tab.
//! * Drawing composites over a checkerboard (transparency), fitted to the pane by default; zoom
//!   (`Ctrl+wheel` around the pointer, `Ctrl+=`, `Ctrl+-`, `Ctrl+0`) and pan (drag, wheel, arrows)
//!   are per tab. A zoomed-out still is drawn from a copy scaled once per zoom level (cheap
//!   repaints); zoomed in, the visible part is sampled pixel by pixel (sharp pixels, no big copy).
//! * GIF / WebP animations play when all their frames fit the decoders' budget
//!   ([`mdimg::ANIM_BYTES`]); otherwise the first frame shows and the status bar says so.
//!
//! The document behind the tab keeps its path (session, tab, explorer) but holds no text for a
//! raster image: its buffer is read-only and never written (`Buffer::no_write`).

use crate::{
    buffer::Buffer,
    doc::{Doc, DrawOpts},
    mdimg::{self, Decoded, Pix},
    theme::{Rgba, Theme},
    ui::{Canvas, Rect, TextCtx},
};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{Receiver, TryRecvError, channel},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// PNG, JPEG, GIF, WebP: binary, shown only as a picture.
    Raster,
    /// SVG: text, with a "Preview | Source" toggle.
    Svg,
}

/// Raster pictures with more pixels than this are kept downscaled (about 32 MB of pixels).
pub const BASE_PX: u64 = 8_000_000;
/// Largest SVG rasterization (beyond it the picture is scaled up from this size).
pub const SVG_PX: u64 = 6_000_000;
/// A zoomed-out still is drawn from a scaled copy up to this many pixels.
const CACHE_PX: u64 = 4_000_000;
pub const MIN_ZOOM: f32 = 0.01;
pub const MAX_ZOOM: f32 = 64.0;
/// Margin kept around a fitted picture (logical px).
const PAD: f64 = 16.0;
/// Checkerboard square (logical px).
const CELL: f64 = 8.0;
/// `Ctrl+=` / `Ctrl+-` stops.
const LADDER: &[f32] = &[
    0.01, 0.02, 0.05, 0.1, 0.125, 0.25, 0.333, 0.5, 0.667, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0,
    24.0, 32.0, 48.0, 64.0,
];

// ── pure helpers ──

/// What an image file is, from its name and first bytes: any file whose bytes are a PNG / JPEG /
/// GIF / WebP is a raster image (whatever its extension); a `.svg` is an SVG when its text looks
/// like one (an empty or broken `.svg` opens as text, so a new one can be written).
pub fn detect(name: &str, head: &[u8]) -> Option<Kind> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match mdimg::image_format(head) {
        Some("png" | "jpeg" | "gif" | "webp") => Some(Kind::Raster),
        Some("svg") if ext == "svg" => Some(Kind::Svg),
        _ => None,
    }
}

/// [`detect`] for a file on disk (reads its first 4 KiB).
pub fn kind_for(path: &Path) -> Option<Kind> {
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(path).ok()?.take(4096).read_to_end(&mut head).ok()?;
    detect(&path.file_name()?.to_string_lossy(), &head)
}

/// The text buffer behind a raster image tab: empty, read-only, never saved.
pub fn placeholder_buffer(path: &Path) -> Buffer {
    let mut b = Buffer::from_rope(ropey::Rope::new(), Some(path.to_path_buf()));
    b.read_only = true;
    b.no_write = true;
    b
}

/// Zoom that fits a `nat` picture in a `view` (logical px, minus a margin); never above 100 %.
pub fn fit_zoom(nat: (u32, u32), view: (f64, f64)) -> f32 {
    let (w, h) = (nat.0.max(1) as f64, nat.1.max(1) as f64);
    let k = ((view.0 - 2.0 * PAD).max(1.0) / w).min((view.1 - 2.0 * PAD).max(1.0) / h);
    (k.min(1.0) as f32).clamp(MIN_ZOOM, 1.0)
}

/// The next zoom stop above (`dir > 0`) or below (`dir < 0`) `cur`.
pub fn step_zoom(cur: f32, dir: i32) -> f32 {
    let eps = 1e-3;
    let next = if dir > 0 {
        LADDER.iter().copied().find(|z| *z > cur * (1.0 + eps))
    } else {
        LADDER.iter().rev().copied().find(|z| *z < cur * (1.0 - eps))
    };
    next.unwrap_or(cur).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// New pan after zooming from `z0` to `z1` so that the picture point under `p` stays put. `pan`
/// and `p` are offsets from the centre of the view (logical px); `pan` is where the picture's
/// centre is.
pub fn zoom_about(z0: f32, z1: f32, pan: (f64, f64), p: (f64, f64)) -> (f64, f64) {
    let k = z1 as f64 / z0.max(MIN_ZOOM) as f64;
    (p.0 - (p.0 - pan.0) * k, p.1 - (p.1 - pan.1) * k)
}

/// Keep the picture (`img` size) on screen: centred along an axis where it fits, otherwise its
/// edges may not come inside the view.
pub fn clamp_pan(pan: (f64, f64), img: (f64, f64), view: (f64, f64)) -> (f64, f64) {
    let axis = |p: f64, i: f64, v: f64| {
        let room = ((i - v) / 2.0).max(0.0);
        p.clamp(-room, room)
    };
    (axis(pan.0, img.0, view.0), axis(pan.1, img.1, view.1))
}

/// Size a `nat` picture is kept at: as is, or scaled down (same aspect) to at most `max_px` pixels.
pub fn base_size(nat: (u32, u32), max_px: u64) -> (u32, u32) {
    let (w, h) = (nat.0.max(1), nat.1.max(1));
    let n = w as u64 * h as u64;
    if n <= max_px {
        return (w, h);
    }
    let k = (max_px as f64 / n as f64).sqrt();
    let (mut w2, mut h2) = (((w as f64 * k).floor() as u32).max(1), ((h as f64 * k).floor() as u32).max(1));
    // A sliver (1 px tall or wide) can't shrink along that side: the other side gets the budget.
    if w2 as u64 * h2 as u64 > max_px {
        if h2 == 1 {
            w2 = max_px.min(u32::MAX as u64) as u32;
        } else {
            h2 = max_px.min(u32::MAX as u64) as u32;
        }
    }
    (w2, h2)
}

/// "812 B", "14.2 KB", "3.4 MB".
pub fn human_bytes(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// Checkerboard squares for the theme: light greys on a light theme, dark greys on a dark one.
fn checker(theme: &Theme) -> (u32, u32) {
    let px = |c: Rgba| ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32;
    if theme.background.luminance() < 0.5 {
        (px(Rgba(0x3a, 0x3a, 0x3a, 255)), px(Rgba(0x2c, 0x2c, 0x2c, 255)))
    } else {
        (px(Rgba(0xff, 0xff, 0xff, 255)), px(Rgba(0xe4, 0xe4, 0xe4, 255)))
    }
}

/// Straight-alpha `p` (0xAARRGGBB) over the opaque `bg` (0x00RRGGBB).
fn over(bg: u32, p: u32) -> u32 {
    let a = p >> 24;
    let mix = |sh: u32| ((((bg >> sh) & 0xff) * (255 - a) + ((p >> sh) & 0xff) * a) / 255) << sh;
    mix(16) | mix(8) | mix(0)
}

// ── per-tab state ──

/// What the worker hands back: (width, height, text version) asked for, and the pixels.
type Done = (u32, u32, i32, Option<Decoded>);

pub struct ImgView {
    pub kind: Kind,
    /// Picture (true) or source text (false, SVG only).
    pub preview: bool,
    path: PathBuf,
    /// Natural size in px (`None`: not an image we can read).
    pub natural: Option<(u32, u32)>,
    /// Size of the file on disk.
    pub file_len: u64,
    /// `None` = fit to the pane.
    zoom: Option<f32>,
    /// Offset of the picture's centre from the pane's centre (logical px).
    pan: (f64, f64),
    /// Pan drag: pointer and pan at the press.
    pub drag: Option<((f64, f64), (f64, f64))>,
    /// Pane of the last frame (logical) and the picture's device rect in it.
    pub rect: Rect,
    pub dev_img: Rect,
    /// "Preview | Source" buttons of the last frame (SVG): (rect, is the preview button).
    pub toggle: Vec<(Rect, bool)>,
    /// Zoom of the last frame (status bar, steps).
    pub shown_zoom: f32,
    frames: Vec<Pix>,
    /// End of each frame of an animation, in ms from its start (empty: a still).
    ends: Vec<u64>,
    /// An animation too big to play: its first frame.
    pub badge: bool,
    /// (w, h, text version) of `frames`, and of the job running.
    have: Option<(u32, u32, i32)>,
    job: Option<(u32, u32, i32)>,
    rx: Option<Receiver<Done>>,
    failed: bool,
    t0: Instant,
    /// Zoomed-out still: ((w, h), pixels scaled from `frames[0]`).
    cache: Option<((u32, u32), Pix)>,
    /// SVG: the text the picture comes from (tab version, bytes).
    svg_src: Option<(i32, Arc<Vec<u8>>)>,
    wake: crate::lsp::Wake,
}

impl ImgView {
    pub fn new(kind: Kind, path: &Path, wake: crate::lsp::Wake) -> Self {
        let mut v = ImgView {
            kind,
            preview: true,
            path: path.to_path_buf(),
            natural: None,
            file_len: 0,
            zoom: None,
            pan: (0.0, 0.0),
            drag: None,
            rect: Rect::default(),
            dev_img: Rect::default(),
            toggle: Vec::new(),
            shown_zoom: 1.0,
            frames: Vec::new(),
            ends: Vec::new(),
            badge: false,
            have: None,
            job: None,
            rx: None,
            failed: false,
            t0: Instant::now(),
            cache: None,
            svg_src: None,
            wake,
        };
        v.reload();
        v
    }

    /// The file changed on disk (or was just opened): forget the pixels, probe it again.
    pub fn reload(&mut self) {
        self.file_len = std::fs::metadata(&self.path).map_or(0, |m| m.len());
        if self.kind == Kind::Raster {
            self.natural = mdimg::probe(&self.path);
        }
        self.frames.clear();
        self.ends.clear();
        self.cache = None;
        self.have = None;
        self.svg_src = None;
        self.failed = false;
        self.badge = false;
    }

    /// Pixels held (tests: the memory goes with the tab).
    #[cfg(test)]
    pub fn pixel_bytes(&self) -> usize {
        self.frames.iter().map(|p| p.px.len() * 4).sum::<usize>()
            + self.cache.as_ref().map_or(0, |(_, p)| p.px.len() * 4)
    }

    /// A decode / rasterization is running.
    #[cfg(test)]
    pub fn pending(&self) -> bool {
        self.job.is_some()
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let (w, h, ver, dec) = match rx.try_recv() {
            Ok(d) => d,
            Err(TryRecvError::Empty) => return,
            // The worker died (a decoder panicked): as if it had failed, so it is not asked again.
            Err(TryRecvError::Disconnected) => {
                let (w, h, v) = self.job.unwrap_or((0, 0, -1));
                (w, h, v, None)
            }
        };
        self.rx = None;
        self.job = None;
        self.have = Some((w, h, ver));
        self.cache = None;
        self.failed = dec.is_none();
        match dec {
            Some(Decoded::Still(p)) => {
                (self.frames, self.ends, self.badge) = (vec![p], Vec::new(), false);
            }
            Some(Decoded::Badge(p)) => {
                (self.frames, self.ends, self.badge) = (vec![p], Vec::new(), true);
            }
            Some(Decoded::Anim(f)) => {
                let mut t = 0u64;
                self.ends = f
                    .iter()
                    .map(|(_, ms)| {
                        t += *ms as u64;
                        t
                    })
                    .collect();
                self.frames = f.into_iter().map(|(p, _)| p).collect();
                self.badge = false;
                self.t0 = Instant::now();
            }
            // An SVG being edited can be broken for a while: the last good picture stays.
            None => {}
        }
    }

    fn request(&mut self, w: u32, h: u32, ver: i32) {
        let (tx, rx) = channel();
        let path = self.path.clone();
        let svg = self.svg_src.as_ref().map(|(_, b)| b.clone()).filter(|_| self.kind == Kind::Svg);
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let dec = match svg {
                Some(b) => mdimg::svg::render(&b, w, h).map(Decoded::Still),
                None => mdimg::decode_any(&path, w, h),
            };
            let _ = tx.send((w, h, ver, dec));
            wake();
        });
        self.job = Some((w, h, ver));
        self.rx = Some(rx);
    }

    fn frame_index(&self, now: Instant) -> usize {
        let Some(&total) = self.ends.last().filter(|t| **t > 0) else { return 0 };
        let t = now.duration_since(self.t0).as_millis() as u64 % total;
        self.ends.partition_point(|e| *e <= t).min(self.frames.len().saturating_sub(1))
    }

    /// When the next frame of a playing animation is due.
    pub fn anim_due(&self, now: Instant) -> Option<Instant> {
        let &total = self.ends.last().filter(|t| **t > 0 && self.frames.len() > 1)?;
        let t = now.duration_since(self.t0).as_millis() as u64 % total;
        let end = self.ends[self.frame_index(now)];
        Some(now + Duration::from_millis(end.saturating_sub(t).max(1)))
    }

    // ── zoom and pan ──

    /// `Ctrl+=` (1), `Ctrl+-` (-1), `Ctrl+0` (0: back to fit, or to 100 % when already fitted).
    pub fn zoom_step(&mut self, dir: i32) {
        if dir == 0 {
            self.zoom = if self.zoom.is_none() { Some(1.0) } else { None };
            self.pan = (0.0, 0.0);
            return;
        }
        let z0 = self.shown_zoom;
        let z1 = step_zoom(z0, dir);
        self.pan = zoom_about(z0, z1, self.pan, (0.0, 0.0));
        self.zoom = Some(z1);
        self.shown_zoom = z1;
    }

    /// Zoom by `factor` keeping the point under the pointer (`x`, `y`, logical window px) still.
    pub fn zoom_at(&mut self, factor: f32, x: f64, y: f64) {
        let z0 = self.shown_zoom;
        let z1 = (z0 * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let p =
            (x - (self.rect.x as f64 + self.rect.w as f64 / 2.0), y - (self.rect.y as f64 + self.rect.h as f64 / 2.0));
        self.pan = zoom_about(z0, z1, self.pan, p);
        self.zoom = Some(z1);
        self.shown_zoom = z1;
    }

    pub fn pan_by(&mut self, dx: f64, dy: f64) {
        self.pan = (self.pan.0 + dx, self.pan.1 + dy);
    }

    pub fn begin_drag(&mut self, x: f64, y: f64) {
        self.drag = Some(((x, y), self.pan));
    }

    pub fn drag_to(&mut self, x: f64, y: f64) {
        if let Some(((x0, y0), (p0, p1))) = self.drag {
            self.pan = (p0 + x - x0, p1 + y - y0);
        }
    }

    pub fn is_fit(&self) -> bool {
        self.zoom.is_none()
    }

    /// Bottom bar text: "1920 × 1080 px · 245.3 KB · 37%".
    pub fn status_text(&self) -> String {
        let size = human_bytes(self.file_len);
        let pct = (self.shown_zoom * 100.0).round();
        let mut s = match self.natural {
            Some((w, h)) => format!("{w} × {h} px · {size} · {pct}%"),
            None => format!("{size} · not readable"),
        };
        if self.badge {
            s.push_str(" · animated (first frame)");
        } else if self.frames.len() > 1 {
            s.push_str(" · animated");
        }
        s
    }

    // ── drawing ──

    /// Draw the picture into `rect`; `ver` / `buf` are the tab's text (the SVG source). True while
    /// a decode is running.
    pub fn draw(
        &mut self,
        cv: &mut Canvas,
        tx: &mut TextCtx,
        theme: &Theme,
        rect: Rect,
        ver: i32,
        buf: &Buffer,
    ) -> bool {
        self.poll();
        self.rect = rect;
        cv.fill(rect, theme.background);
        if self.kind == Kind::Svg && self.svg_src.as_ref().is_none_or(|(v, _)| *v != ver) {
            let bytes = buf.to_string().into_bytes();
            self.natural = mdimg::svg::size(&bytes);
            self.svg_src = Some((ver, Arc::new(bytes)));
        }
        let Some((nw, nh)) = self.natural else {
            self.message(cv, tx, theme, rect, "This image can't be read");
            return false;
        };
        let view = (rect.w as f64, rect.h as f64);
        let z = self.zoom.unwrap_or_else(|| fit_zoom((nw, nh), view)).clamp(MIN_ZOOM, MAX_ZOOM);
        self.shown_zoom = z;
        let (iw, ih) = (nw as f64 * z as f64, nh as f64 * z as f64);
        self.pan = clamp_pan(self.pan, (iw, ih), view);
        let s = crate::ui::scale() as f64;
        let x0 = rect.x as f64 + (view.0 - iw) / 2.0 + self.pan.0;
        let y0 = rect.y as f64 + (view.1 - ih) / 2.0 + self.pan.1;
        let dx = (x0 * s).round() as i32;
        let dy = (y0 * s).round() as i32;
        let dev = Rect::new(
            dx,
            dy,
            (((x0 + iw) * s).round() as i32 - dx).max(1),
            (((y0 + ih) * s).round() as i32 - dy).max(1),
        );
        self.dev_img = dev;

        // Pixels: the raster picture once, an SVG at the size it is shown.
        let (want, key) = match self.kind {
            Kind::Raster => (base_size((nw, nh), BASE_PX), 0),
            Kind::Svg => (base_size((dev.w as u32, dev.h as u32), SVG_PX), ver),
        };
        // A raster file that failed is not tried again (until it changes on disk: `reload`).
        let may_try = !self.failed || self.kind == Kind::Svg;
        if self.have != Some((want.0, want.1, key)) && self.job.is_none() && may_try {
            self.request(want.0, want.1, key);
        }
        if self.frames.is_empty() {
            let msg = if self.failed { "This image can't be read" } else { "Loading…" };
            self.message(cv, tx, theme, rect, msg);
            return self.job.is_some();
        }

        let clip = cv.to_dev(rect).intersect(&dev).intersect(&Rect::new(0, 0, cv.w, cv.h));
        if clip.w > 0 && clip.h > 0 {
            let fi = self.frame_index(Instant::now());
            let (dw, dh) = (dev.w as u32, dev.h as u32);
            let still = self.frames.len() == 1;
            let f = &self.frames[fi];
            let exact = f.w == dw && f.h == dh;
            if !exact && still && dw <= f.w && dw as u64 * dh as u64 <= CACHE_PX {
                if self.cache.as_ref().is_none_or(|(k, _)| *k != (dw, dh)) {
                    self.cache = Some(((dw, dh), mdimg::scaled(f, dw, dh)));
                }
            } else {
                self.cache = None;
            }
            let src = match &self.cache {
                Some((_, p)) => p,
                None => &self.frames[fi],
            };
            composite(cv, src, dev, clip, checker(theme), ((CELL * s).round() as i32).max(2));
        }
        self.job.is_some()
    }

    fn message(&self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect, msg: &str) {
        let w = tx.ui_width(msg, false);
        tx.draw_ui(cv, rect.x + (rect.w - w) / 2, rect.y + rect.h / 2 - 9, msg, theme.line_number, rect, false);
    }

    /// The SVG "Preview | Source" switch, top right of the pane (drawn in both modes).
    pub fn draw_toggle(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect) {
        self.toggle.clear();
        if self.kind != Kind::Svg || rect.w < 240 {
            return;
        }
        let c = theme.chrome;
        let (h, pad) = (24, 10);
        let parts = [("Preview", true), ("Source", false)];
        let widths: Vec<i32> = parts.iter().map(|(l, _)| tx.ui_width(l, false) + 2 * pad).collect();
        let total: i32 = widths.iter().sum();
        let mut x = rect.right() - total - 16;
        let y = rect.y + 10;
        let bg = Rect::new(x - 3, y - 3, total + 6, h + 6);
        cv.fill_round(bg, 9, c.border);
        cv.fill_round(bg.inset(1), 8, c.widget_bg);
        for ((label, is_preview), w) in parts.into_iter().zip(widths) {
            let r = Rect::new(x, y, w, h);
            let on = is_preview == self.preview;
            if on {
                cv.fill_round(r, 6, c.accent.with_alpha(0x40));
            }
            let fg = if on { c.accent_text } else { c.tab_inactive_fg };
            tx.draw_ui(cv, x + pad, y + (h - 18) / 2, label, fg, r, on);
            self.toggle.push((r, is_preview));
            x += w;
        }
    }
}

/// Paint `src` stretched over the device rect `dev` (nearest pixel unless the sizes match),
/// over a checkerboard, inside `clip` (device px, inside the canvas).
fn composite(cv: &mut Canvas, src: &Pix, dev: Rect, clip: Rect, (c1, c2): (u32, u32), cell: i32) {
    let (fw, fh) = (src.w as i64, src.h as i64);
    let (dw, dh) = (dev.w as i64, dev.h as i64);
    let exact = fw == dw && fh == dh;
    for y in clip.y..clip.bottom() {
        let ry = (y - dev.y) as i64;
        let sy = if exact { ry } else { (ry * fh / dh).clamp(0, fh - 1) };
        let srow = (sy * fw) as usize;
        let drow = (y * cv.w) as usize;
        let band = ((y - dev.y) / cell) & 1;
        for x in clip.x..clip.right() {
            let rx = (x - dev.x) as i64;
            let sx = if exact { rx } else { (rx * fw / dw).clamp(0, fw - 1) };
            let p = src.px[srow + sx as usize];
            let a = p >> 24;
            cv.px[drow + x as usize] = if a == 255 {
                p & 0x00ff_ffff
            } else {
                let bg = if (((x - dev.x) / cell) & 1) == band { c1 } else { c2 };
                if a == 0 { bg } else { over(bg, p) }
            };
        }
    }
}

// ── the document side ──

impl Doc {
    /// The tab shows a picture (an image, or an SVG in "Preview").
    pub fn showing_image(&self) -> bool {
        self.img.as_ref().is_some_and(|v| v.preview)
    }

    /// SVG: switch between the picture and the source text (editable only as source). False for
    /// any other document.
    pub fn img_toggle(&mut self) -> bool {
        let Some(v) = self.img.as_mut().filter(|v| v.kind == Kind::Svg) else { return false };
        v.preview = !v.preview;
        v.drag = None;
        self.buf.read_only = v.preview;
        self.reveal = true;
        true
    }

    /// SVG: show the source (session restore).
    pub fn img_show_source(&mut self) {
        if self.showing_image() {
            self.img_toggle();
        }
    }

    /// Draws into `rect`: the picture of an image tab, the text otherwise (see `draw_text`).
    /// Returns true while more frames are needed.
    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect, o: &DrawOpts) -> bool {
        let Some(mut v) = self.img.take() else { return self.draw_text(cv, tx, theme, rect, o) };
        let busy = if v.preview {
            self.rect = rect;
            v.draw(cv, tx, theme, rect, self.version, &self.buf)
        } else {
            self.draw_text(cv, tx, theme, rect, o)
        };
        v.draw_toggle(cv, tx, theme, rect);
        self.img = Some(v);
        busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn formats_are_detected_from_the_bytes_and_svg_from_name_and_text() {
        assert_eq!(detect("a.png", PNG_MAGIC), Some(Kind::Raster));
        assert_eq!(detect("photo.JPG", &[0xff, 0xd8, 0xff, 0xe0]), Some(Kind::Raster));
        assert_eq!(detect("anim.gif", b"GIF89a\x01\0\x01\0"), Some(Kind::Raster));
        assert_eq!(detect("x.webp", b"RIFF\0\0\0\0WEBPVP8 "), Some(Kind::Raster));
        // The bytes decide for rasters: a PNG without its extension still opens as a picture...
        assert_eq!(detect("icon", PNG_MAGIC), Some(Kind::Raster));
        // ...and a text file named .png is text.
        assert_eq!(detect("notes.png", b"just text"), None);
        assert_eq!(detect("empty.png", b""), None);
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;
        assert_eq!(detect("logo.svg", svg), Some(Kind::Svg));
        assert_eq!(detect("LOGO.SVG", b"<?xml version=\"1.0\"?>\n<svg/>"), Some(Kind::Svg));
        assert_eq!(detect("new.svg", b""), None, "an empty .svg is written as text");
        assert_eq!(detect("page.html", svg), None, "inline SVG in another file stays text");
        assert_eq!(detect("main.rs", b"fn main() {}"), None);
    }

    #[test]
    fn fit_never_upscales_and_keeps_a_margin() {
        assert_eq!(fit_zoom((100, 50), (800.0, 600.0)), 1.0, "small pictures show at 100 %");
        let z = fit_zoom((4000, 1000), (832.0, 600.0));
        assert!((z - 0.2).abs() < 1e-6, "{z}"); // (832 - 32) / 4000
        let z = fit_zoom((1000, 3000), (800.0, 632.0));
        assert!((z - 0.2).abs() < 1e-6, "{z}"); // (632 - 32) / 3000
        assert_eq!(fit_zoom((1, 1), (0.0, 0.0)), 1.0);
        assert_eq!(fit_zoom((1_000_000, 10), (100.0, 100.0)), MIN_ZOOM);
    }

    #[test]
    fn zoom_steps_walk_the_ladder_and_stop_at_the_ends() {
        assert_eq!(step_zoom(1.0, 1), 1.25);
        assert_eq!(step_zoom(1.0, -1), 0.75);
        assert_eq!(step_zoom(0.37, 1), 0.5, "from a fitted zoom to the next stop");
        assert_eq!(step_zoom(0.37, -1), 0.333);
        assert_eq!(step_zoom(MAX_ZOOM, 1), MAX_ZOOM);
        assert_eq!(step_zoom(MIN_ZOOM, -1), MIN_ZOOM);
        let mut z = MIN_ZOOM;
        for _ in 0..LADDER.len() {
            z = step_zoom(z, 1);
        }
        assert_eq!(z, MAX_ZOOM);
    }

    #[test]
    fn zooming_about_a_point_keeps_it_under_the_pointer() {
        // Picture centre at (10, -20) from the view centre, pointer at (110, 30), 1x -> 2x.
        let (z0, z1, pan, p) = (1.0, 2.0, (10.0, -20.0), (110.0, 30.0));
        let pan1 = zoom_about(z0, z1, pan, p);
        // The picture point under the pointer, in picture px from its centre, is the same before and after.
        let before = ((p.0 - pan.0) / z0 as f64, (p.1 - pan.1) / z0 as f64);
        let after = ((p.0 - pan1.0) / z1 as f64, (p.1 - pan1.1) / z1 as f64);
        assert!((before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9);
        assert_eq!(zoom_about(1.0, 3.0, (5.0, 5.0), (0.0, 0.0)), (15.0, 15.0), "about the centre: pan scales");
    }

    #[test]
    fn panning_is_kept_inside_the_picture() {
        // Fits horizontally (centred), larger vertically (edges stay outside the view).
        assert_eq!(clamp_pan((50.0, 500.0), (300.0, 1000.0), (800.0, 600.0)), (0.0, 200.0));
        assert_eq!(clamp_pan((-900.0, -10.0), (2000.0, 1000.0), (800.0, 600.0)), (-600.0, -10.0));
    }

    #[test]
    fn big_pictures_are_kept_downscaled_and_sizes_read_well() {
        assert_eq!(base_size((1920, 1080), BASE_PX), (1920, 1080));
        let (w, h) = base_size((8000, 6000), BASE_PX);
        assert!(w as u64 * h as u64 <= BASE_PX && (w as f64 / h as f64 - 4.0 / 3.0).abs() < 0.01, "{w}x{h}");
        assert_eq!(base_size((100_000, 1), 1000), (1000, 1));
        assert_eq!(human_bytes(812), "812 B");
        assert_eq!(human_bytes(14_540), "14.2 KB");
        assert_eq!(human_bytes(3_565_158), "3.4 MB");
    }

    #[test]
    fn compositing_shows_the_checkerboard_through_transparent_pixels() {
        assert_eq!(over(0x000000, 0xff12_3456), 0x123456);
        assert_eq!(over(0xffffff, 0x0000_0000), 0xffffff);
        assert_eq!(over(0x000000, 0x80ff_ffff), 0x808080);
        // 2x1 source: opaque red, fully transparent; stretched 2x onto a 4x2 canvas, cell 1.
        let src = Pix { w: 2, h: 1, px: vec![0xffff_0000, 0x0000_0000] };
        let mut px = vec![0u32; 8];
        {
            let mut cv = Canvas { px: &mut px, w: 4, h: 2 };
            composite(&mut cv, &src, Rect::new(0, 0, 4, 2), Rect::new(0, 0, 4, 2), (0xaaaaaa, 0x555555), 1);
        }
        assert_eq!(px, vec![0xff0000, 0xff0000, 0xaaaaaa, 0x555555, 0xff0000, 0xff0000, 0x555555, 0xaaaaaa]);
    }
}
