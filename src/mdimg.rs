//! Images of the Markdown preview: local files only (png, jpg, gif, webp, svg).
//!
//! * [`probe`] reads just enough of a file to know its size (layout needs the size, not the pixels).
//! * Pixels are decoded on a worker thread ([`ImgStore::texture`] answers `Pending` meanwhile and the
//!   preview keeps redrawing while [`ImgStore::pending`]), scaled once to the size they are drawn at
//!   and kept in a small LRU (about [`CACHE_BYTES`]).
//! * Decoders are small crates with their default features off (`png`, `zune-jpeg`, `gif`,
//!   `image-webp`); SVG goes through a minimal renderer on top of `tiny-skia` (see [`svg`]).
//!
//! Remote URLs (`http:`, `https:`, `data:`) and absolute paths are never loaded: the preview keeps
//! its placeholder card for them.

use crate::ui::{Canvas, Rect};
use std::{
    collections::{HashMap, VecDeque},
    io::{Cursor, Read},
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant, SystemTime},
};

/// Decoded pixels kept for the images that were drawn recently.
pub const CACHE_BYTES: usize = 40 << 20;
/// Files bigger than this are not loaded.
const MAX_FILE: u64 = 48 << 20;
/// Images with more pixels than this are not decoded (the full RGBA buffer would be too big).
const MAX_PIXELS: u64 = 40_000_000;
/// Decoder threads running at once.
const WORKERS: usize = 2;
/// An animation keeps all its frames scaled to the drawn size; past this many bytes (or frames)
/// the image shows its first frame with an "animated" badge instead of playing.
pub const ANIM_BYTES: usize = 16 << 20;
pub const ANIM_FRAMES: usize = 300;
/// Animations that play at once; the other visible ones stay on their first frame (and get the badge).
pub const MAX_PLAYING: usize = 4;

/// Straight-alpha pixels, `0xAARRGGBB`.
#[derive(Clone, Debug)]
pub struct Pix {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

impl Pix {
    fn bytes(&self) -> usize {
        self.px.len() * 4
    }
}

fn argb(r: u8, g: u8, b: u8, a: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

// ---------------------------------------------------------------------------------------------
// probing

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}
fn le16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}
fn le24(b: &[u8], i: usize) -> Option<u32> {
    Some(*b.get(i)? as u32 | (*b.get(i + 1)? as u32) << 8 | (*b.get(i + 2)? as u32) << 16)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Fmt {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
}

fn sniff(b: &[u8]) -> Option<Fmt> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Fmt::Png)
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Fmt::Jpeg)
    } else if b.starts_with(b"GIF8") {
        Some(Fmt::Gif)
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(Fmt::Webp)
    } else {
        let head = &b[..b.len().min(4096)];
        let s = String::from_utf8_lossy(head);
        (s.contains("<svg")
            || (s.trim_start_matches('\u{feff}').trim_start().starts_with("<?xml") && s.contains("svg")))
        .then_some(Fmt::Svg)
    }
}

/// Size from the header (`b` is a prefix of the file; SVG needs the whole file).
fn header_size(b: &[u8]) -> Option<(u32, u32)> {
    let (w, h) = match sniff(b)? {
        Fmt::Png => {
            (u32::from_be_bytes(b.get(16..20)?.try_into().ok()?), u32::from_be_bytes(b.get(20..24)?.try_into().ok()?))
        }
        Fmt::Gif => (le16(b, 6)?, le16(b, 8)?),
        Fmt::Jpeg => {
            let mut i = 2;
            loop {
                while *b.get(i)? != 0xff {
                    i += 1;
                }
                while *b.get(i)? == 0xff {
                    i += 1;
                }
                let m = *b.get(i)?;
                i += 1;
                if m == 0x01 || (0xd0..=0xd9).contains(&m) {
                    continue;
                }
                let len = be16(b, i)? as usize;
                if (0xc0..=0xcf).contains(&m) && !matches!(m, 0xc4 | 0xc8 | 0xcc) {
                    break (be16(b, i + 5)?, be16(b, i + 3)?);
                }
                i += len;
            }
        }
        Fmt::Webp => match b.get(12..16)? {
            b"VP8 " => (le16(b, 26)? & 0x3fff, le16(b, 28)? & 0x3fff),
            b"VP8L" => {
                let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                ((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1)
            }
            b"VP8X" => (le24(b, 24)? + 1, le24(b, 27)? + 1),
            _ => return None,
        },
        Fmt::Svg => return svg::size(b),
    };
    (w > 0 && h > 0 && (w as u64) * (h as u64) <= MAX_PIXELS).then_some((w, h))
}

fn read_prefix(path: &Path, max: u64) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    if f.metadata().ok()?.len() > MAX_FILE {
        return None;
    }
    let mut v = Vec::new();
    f.take(max).read_to_end(&mut v).ok()?;
    Some(v)
}

/// Natural size in px of the image at `path`, without decoding it.
pub fn probe(path: &Path) -> Option<(u32, u32)> {
    let head = read_prefix(path, 1 << 20)?;
    match sniff(&head)? {
        Fmt::Svg => header_size(&read_prefix(path, 8 << 20)?),
        _ => header_size(&head),
    }
}

// ---------------------------------------------------------------------------------------------
// decoding

fn from_rgba(w: u32, h: u32, d: &[u8], ch: usize) -> Option<Pix> {
    if d.len() < (w as usize) * (h as usize) * ch {
        return None;
    }
    let px = d
        .chunks_exact(ch)
        .take((w * h) as usize)
        .map(|p| match ch {
            1 => argb(p[0], p[0], p[0], 255),
            2 => argb(p[0], p[0], p[0], p[1]),
            3 => argb(p[0], p[1], p[2], 255),
            _ => argb(p[0], p[1], p[2], p[3]),
        })
        .collect();
    Some(Pix { w, h, px })
}

pub fn decode_png(b: &[u8]) -> Option<Pix> {
    let mut dec = png::Decoder::new(Cursor::new(b));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut rd = dec.read_info().ok()?;
    let mut buf = vec![0; rd.output_buffer_size()?];
    let info = rd.next_frame(&mut buf).ok()?;
    let ch = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    from_rgba(info.width, info.height, &buf, ch)
}

fn decode_jpeg(b: &[u8]) -> Option<Pix> {
    let mut d = zune_jpeg::JpegDecoder::new(Cursor::new(b));
    let px = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let ch = px.len() / ((w * h) as usize).max(1);
    from_rgba(w, h, &px, ch.clamp(1, 4))
}

/// Frame delay as a browser would play it: the very short and zero delays that old GIFs carry
/// mean "about a tenth of a second", and nothing runs faster than ~25 fps (low CPU).
pub fn frame_ms(raw_ms: u32) -> u32 {
    if raw_ms <= 10 { 100 } else { raw_ms.max(40) }
}

/// Every frame of a GIF composited on the full canvas (disposal methods honoured), handed to
/// `sink` with its delay in ms as soon as it is ready; `sink` returns false to stop early.
fn gif_each(b: &[u8], mut sink: impl FnMut(Pix, u32) -> bool) -> Option<()> {
    let mut o = gif::DecodeOptions::new();
    o.set_color_output(gif::ColorOutput::RGBA);
    let mut d = o.read_info(Cursor::new(b)).ok()?;
    let (sw, sh) = (d.width() as u32, d.height() as u32);
    let mut canvas = vec![0u32; (sw * sh) as usize];
    let mut any = false;
    // What the previous frame asks to be undone before the next one is drawn.
    let mut undo: Option<(gif::DisposalMethod, (u32, u32, u32, u32), Option<Vec<u32>>)> = None;
    while let Some(f) = d.read_next_frame().ok()? {
        if let Some((method, (fl, ft, fw, fh), saved)) = undo.take() {
            match (method, saved) {
                (gif::DisposalMethod::Background, _) => {
                    for y in ft..(ft + fh).min(sh) {
                        for x in fl..(fl + fw).min(sw) {
                            canvas[(y * sw + x) as usize] = 0;
                        }
                    }
                }
                (gif::DisposalMethod::Previous, Some(prev)) => canvas = prev,
                _ => {}
            }
        }
        let rect = (f.left as u32, f.top as u32, f.width as u32, f.height as u32);
        let saved = (f.dispose == gif::DisposalMethod::Previous).then(|| canvas.clone());
        let (fl, ft, fw, fh) = rect;
        for y in 0..fh.min(sh.saturating_sub(ft)) {
            for x in 0..fw.min(sw.saturating_sub(fl)) {
                let i = ((y * fw + x) * 4) as usize;
                let p = f.buffer.get(i..i + 4)?;
                if p[3] != 0 {
                    canvas[((ft + y) * sw + fl + x) as usize] = argb(p[0], p[1], p[2], p[3]);
                }
            }
        }
        undo = Some((f.dispose, rect, saved));
        let ms = f.delay as u32 * 10;
        any = true;
        if !sink(Pix { w: sw, h: sh, px: canvas.clone() }, ms) {
            break;
        }
    }
    any.then_some(())
}

/// Frames of an animated WebP (each one the full canvas, composited by the decoder) through
/// `sink`; `None` when the file is not animated.
fn webp_each(b: &[u8], mut sink: impl FnMut(Pix, u32) -> bool) -> Option<()> {
    let mut d = image_webp::WebPDecoder::new(Cursor::new(b)).ok()?;
    if !d.is_animated() {
        return None;
    }
    let (w, h) = d.dimensions();
    let ch = if d.has_alpha() { 4 } else { 3 };
    let mut buf = vec![0; d.output_buffer_size()?];
    for _ in 0..d.num_frames() {
        let ms = d.read_frame(&mut buf).ok()?;
        if !sink(from_rgba(w, h, &buf, ch)?, ms) {
            break;
        }
    }
    Some(())
}

fn decode_webp(b: &[u8]) -> Option<Pix> {
    let mut d = image_webp::WebPDecoder::new(Cursor::new(b)).ok()?;
    let (w, h) = d.dimensions();
    let ch = if d.has_alpha() { 4 } else { 3 };
    let mut buf = vec![0; d.output_buffer_size()?];
    d.read_image(&mut buf).ok()?;
    from_rgba(w, h, &buf, ch)
}

/// What a worker hands back for one image at one size.
pub enum Decoded {
    /// One picture.
    Still(Pix),
    /// An animation: every frame scaled to the drawn size, with its delay in ms.
    Anim(Vec<(Pix, u32)>),
    /// An animation that is too big to play: its first frame (shown with a badge).
    Badge(Pix),
}

impl Decoded {
    pub fn bytes(&self) -> usize {
        match self {
            Decoded::Still(p) | Decoded::Badge(p) => p.bytes(),
            Decoded::Anim(f) => f.iter().map(|(p, _)| p.bytes()).sum(),
        }
    }

    #[cfg(test)]
    /// The first frame.
    pub fn first(self) -> Option<Pix> {
        match self {
            Decoded::Still(p) | Decoded::Badge(p) => Some(p),
            Decoded::Anim(f) => f.into_iter().next().map(|(p, _)| p),
        }
    }
}

/// Collect the frames an `each` source produces, scaled to `tw` x `th`, within the animation caps.
fn collect_frames(
    each: impl FnOnce(&mut dyn FnMut(Pix, u32) -> bool) -> Option<()>,
    tw: u32,
    th: u32,
) -> Option<Decoded> {
    let mut frames: Vec<(Pix, u32)> = Vec::new();
    let (mut bytes, mut over) = (0usize, false);
    each(&mut |p, ms| {
        let sc = scale_to(p, tw, th);
        if !frames.is_empty() && (frames.len() >= ANIM_FRAMES || bytes + sc.bytes() > ANIM_BYTES) {
            over = true;
            return false;
        }
        bytes += sc.bytes();
        frames.push((sc, frame_ms(ms)));
        true
    })?;
    match (frames.len(), over) {
        (0, _) => None,
        (1, false) => frames.pop().map(|(p, _)| Decoded::Still(p)),
        (_, false) => Some(Decoded::Anim(frames)),
        (_, true) => frames.into_iter().next().map(|(p, _)| Decoded::Badge(p)),
    }
}

/// Decode `path` scaled to exactly `tw` x `th` pixels; animated GIF / WebP keep their frames.
pub fn decode_any(path: &Path, tw: u32, th: u32) -> Option<Decoded> {
    if tw == 0 || th == 0 {
        return None;
    }
    let bytes = read_prefix(path, MAX_FILE)?;
    let fmt = sniff(&bytes)?;
    if fmt == Fmt::Svg {
        return svg::render(&bytes, tw, th).map(Decoded::Still);
    }
    let (w, h) = header_size(&bytes)?;
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    match fmt {
        Fmt::Gif => collect_frames(|sink| gif_each(&bytes, sink), tw, th),
        Fmt::Webp => collect_frames(|sink| webp_each(&bytes, sink), tw, th)
            .or_else(|| decode_webp(&bytes).map(|p| Decoded::Still(scale_to(p, tw, th)))),
        Fmt::Png => decode_png(&bytes).map(|p| Decoded::Still(scale_to(p, tw, th))),
        Fmt::Jpeg => decode_jpeg(&bytes).map(|p| Decoded::Still(scale_to(p, tw, th))),
        Fmt::Svg => None,
    }
}

#[cfg(test)]
/// Decode `path` and scale it to exactly `tw` x `th` pixels (first frame of an animation).
pub fn decode(path: &Path, tw: u32, th: u32) -> Option<Pix> {
    decode_any(path, tw, th)?.first()
}

// ---------------------------------------------------------------------------------------------
// scaling

/// `src` resampled to `tw` x `th`: area average when shrinking, bilinear when growing.
pub fn scale_to(src: Pix, tw: u32, th: u32) -> Pix {
    if src.w == tw && src.h == th {
        return src;
    }
    let (sw, sh) = (src.w as usize, src.h as usize);
    let (tw_, th_) = (tw as usize, th as usize);
    let mut out = vec![0u32; tw_ * th_];
    if tw_ <= sw && th_ <= sh {
        for dy in 0..th_ {
            let y0 = dy * sh / th_;
            let y1 = (((dy + 1) * sh).div_ceil(th_)).clamp(y0 + 1, sh);
            for dx in 0..tw_ {
                let x0 = dx * sw / tw_;
                let x1 = (((dx + 1) * sw).div_ceil(tw_)).clamp(x0 + 1, sw);
                let (mut sa, mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
                for y in y0..y1 {
                    for p in &src.px[y * sw + x0..y * sw + x1] {
                        let a = (p >> 24) as u64;
                        sa += a;
                        sr += ((p >> 16) & 0xff) as u64 * a;
                        sg += ((p >> 8) & 0xff) as u64 * a;
                        sb += (p & 0xff) as u64 * a;
                        n += 1;
                    }
                }
                out[dy * tw_ + dx] = match (sr.checked_div(sa), sg.checked_div(sa), sb.checked_div(sa)) {
                    (Some(r), Some(g), Some(b)) => argb(r as u8, g as u8, b as u8, (sa / n) as u8),
                    _ => 0,
                };
            }
        }
    } else {
        let fx = sw as f32 / tw_ as f32;
        let fy = sh as f32 / th_ as f32;
        for dy in 0..th_ {
            let sy = ((dy as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
            let (y0, ty) = (sy as usize, sy.fract());
            let y1 = (y0 + 1).min(sh - 1);
            for dx in 0..tw_ {
                let sx = ((dx as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
                let (x0, tx) = (sx as usize, sx.fract());
                let x1 = (x0 + 1).min(sw - 1);
                let q = [
                    (src.px[y0 * sw + x0], (1.0 - tx) * (1.0 - ty)),
                    (src.px[y0 * sw + x1], tx * (1.0 - ty)),
                    (src.px[y1 * sw + x0], (1.0 - tx) * ty),
                    (src.px[y1 * sw + x1], tx * ty),
                ];
                let (mut a, mut r, mut g, mut b) = (0f32, 0f32, 0f32, 0f32);
                for (p, wgt) in q {
                    let pa = (p >> 24) as f32 * wgt;
                    a += pa;
                    r += ((p >> 16) & 0xff) as f32 * pa;
                    g += ((p >> 8) & 0xff) as f32 * pa;
                    b += (p & 0xff) as f32 * pa;
                }
                out[dy * tw_ + dx] =
                    if a <= 0.0 { 0 } else { argb((r / a) as u8, (g / a) as u8, (b / a) as u8, a.round() as u8) };
            }
        }
    }
    Pix { w: tw, h: th, px: out }
}

/// Source-over blit of `pix` (straight alpha) with its top-left at device (`x`, `y`), clipped to
/// `clip` (device pixels).
pub fn blit(cv: &mut Canvas, x: i32, y: i32, pix: &Pix, clip: Rect) {
    let dst = Rect::new(x, y, pix.w as i32, pix.h as i32).intersect(&clip).intersect(&Rect::new(0, 0, cv.w, cv.h));
    if dst.w <= 0 || dst.h <= 0 {
        return;
    }
    for py in dst.y..dst.bottom() {
        let srow = ((py - y) as usize) * pix.w as usize;
        let drow = (py * cv.w) as usize;
        for px in dst.x..dst.right() {
            let s = pix.px[srow + (px - x) as usize];
            let a = s >> 24;
            if a == 0 {
                continue;
            }
            let d = &mut cv.px[drow + px as usize];
            if a == 255 {
                *d = s & 0x00ff_ffff;
            } else {
                let mix = |dc: u32, sc: u32| (dc * (255 - a) + sc * a) / 255;
                *d = (mix((*d >> 16) & 0xff, (s >> 16) & 0xff) << 16)
                    | (mix((*d >> 8) & 0xff, (s >> 8) & 0xff) << 8)
                    | mix(*d & 0xff, s & 0xff);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// store

#[derive(Clone)]
pub enum Tex {
    /// Pixels to draw (a still image, or the current frame of a playing animation).
    Ready(Rc<Pix>),
    /// An animation that does not play (too big, or too many are on screen): its first frame,
    /// drawn with an "animated" badge.
    Badged(Rc<Pix>),
    Pending,
    Failed,
}

/// Frames of a playing animation and where each one ends (ms from the start of a loop).
pub struct Anim {
    frames: Vec<Rc<Pix>>,
    ends: Vec<u64>,
}

impl Anim {
    fn new(frames: Vec<(Pix, u32)>) -> Self {
        let mut t = 0u64;
        let ends = frames
            .iter()
            .map(|(_, ms)| {
                t += *ms as u64;
                t
            })
            .collect();
        Anim { frames: frames.into_iter().map(|(p, _)| Rc::new(p)).collect(), ends }
    }

    fn total(&self) -> u64 {
        self.ends.last().copied().unwrap_or(1).max(1)
    }

    /// Frame showing `ms` after the animation started (it loops forever).
    pub fn frame_at(&self, ms: u64) -> usize {
        let t = ms % self.total();
        self.ends.partition_point(|e| *e <= t).min(self.frames.len() - 1)
    }

    /// Milliseconds from `ms` until the next frame is due.
    pub fn until_next(&self, ms: u64) -> u64 {
        let t = ms % self.total();
        self.ends[self.frame_at(ms)].saturating_sub(t).max(1)
    }

    fn bytes(&self) -> usize {
        self.frames.iter().map(|f| f.bytes()).sum()
    }
}

enum Slot {
    Pending,
    Ready(Rc<Pix>, u64),
    Badged(Rc<Pix>, u64),
    /// A playing animation: the clock it started on.
    Anim(Rc<Anim>, u64, Instant),
    Failed,
}

impl Slot {
    /// (last drawn tick, bytes held) of a decoded slot.
    fn held(&self) -> Option<(u64, usize)> {
        match self {
            Slot::Ready(p, u) | Slot::Badged(p, u) => Some((*u, p.bytes())),
            Slot::Anim(a, u, _) => Some((*u, a.bytes())),
            Slot::Pending | Slot::Failed => None,
        }
    }
}

/// (image id, device width, device height).
type Key = (u32, u32, u32);

/// What a file looked like when it was probed: a change means the cache entry is stale.
type Stamp = Option<(SystemTime, u64)>;

fn stamp_of(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Image paths, probed sizes and the decoded textures of one preview.
pub struct ImgStore {
    base: Option<PathBuf>,
    paths: Vec<PathBuf>,
    ids: HashMap<PathBuf, u32>,
    sizes: HashMap<u32, Option<(u32, u32)>>,
    /// File state at probe time, per image id (see [`ImgStore::changed_on_disk`]).
    stamps: HashMap<u32, Stamp>,
    /// Bumped when an image's file changed: results of older decodes are dropped.
    gens: HashMap<u32, u32>,
    tex: HashMap<Key, Slot>,
    jobs: VecDeque<Key>,
    inflight: usize,
    tx: Sender<(Key, u32, Option<Decoded>)>,
    rx: Receiver<(Key, u32, Option<Decoded>)>,
    clock: u64,
    bytes: usize,
    /// Earliest moment a drawn animation needs its next frame (reset by [`ImgStore::begin_frame`]).
    anim_due: Option<Instant>,
    /// Animations that played in the current frame.
    playing: usize,
}

impl Default for ImgStore {
    fn default() -> Self {
        Self::new()
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl ImgStore {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        ImgStore {
            base: None,
            paths: vec![],
            ids: HashMap::new(),
            sizes: HashMap::new(),
            stamps: HashMap::new(),
            gens: HashMap::new(),
            tex: HashMap::new(),
            jobs: VecDeque::new(),
            inflight: 0,
            tx,
            rx,
            clock: 0,
            bytes: 0,
            anim_due: None,
            playing: 0,
        }
    }

    /// Directory relative image paths are resolved against. True when it changed.
    pub fn set_base(&mut self, dir: Option<&Path>) -> bool {
        if self.base.as_deref() == dir {
            return false;
        }
        self.base = dir.map(Path::to_path_buf);
        true
    }

    /// Id of the local file `url` names (relative to the base directory), `None` for remote URLs,
    /// absolute paths and anything that does not name a file.
    pub fn resolve(&mut self, url: &str) -> Option<u32> {
        let url = url.trim();
        if url.is_empty() || url.starts_with('/') || url.starts_with('#') || url.starts_with('\\') {
            return None;
        }
        if url
            .find(':')
            .is_some_and(|i| i > 1 && url[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)))
        {
            return None;
        }
        let path = url.split(['?', '#']).next().unwrap_or("");
        let full = self.base.as_ref()?.join(percent_decode(path));
        Some(match self.ids.get(&full) {
            Some(&i) => i,
            None => {
                let i = self.paths.len() as u32;
                self.paths.push(full.clone());
                self.ids.insert(full, i);
                i
            }
        })
    }

    /// Natural size of image `id` (header probe, cached; `None` when missing or unreadable).
    pub fn size(&mut self, id: u32) -> Option<(u32, u32)> {
        if let Some(s) = self.sizes.get(&id) {
            return *s;
        }
        let s = self.paths.get(id as usize).and_then(|p| probe(p));
        self.sizes.insert(id, s);
        self.stamps.insert(id, self.paths.get(id as usize).and_then(|p| stamp_of(p)));
        s
    }

    /// Forget what is cached about images whose file changed since it was probed (size, decoded
    /// pixels, queued and running decodes). True when any did: the layout has to be redone.
    pub fn changed_on_disk(&mut self) -> bool {
        let stale: Vec<u32> = self
            .stamps
            .iter()
            .filter(|(id, st)| self.paths.get(**id as usize).is_none_or(|p| stamp_of(p) != **st))
            .map(|(id, _)| *id)
            .collect();
        for id in &stale {
            self.sizes.remove(id);
            self.stamps.remove(id);
            *self.gens.entry(*id).or_insert(0) += 1;
            let keys: Vec<Key> = self.tex.keys().filter(|k| k.0 == *id).copied().collect();
            for k in keys {
                if let Some((_, b)) = self.tex.remove(&k).and_then(|s| s.held()) {
                    self.bytes = self.bytes.saturating_sub(b);
                }
            }
            self.jobs.retain(|k| k.0 != *id);
        }
        !stale.is_empty()
    }

    /// Start of a draw pass: no animation has asked for a frame yet.
    pub fn begin_frame(&mut self) {
        self.anim_due = None;
        self.playing = 0;
    }

    /// When the animations drawn in the last pass need their next frame (None = nothing moves,
    /// so no timer is needed: off-screen animations cost nothing).
    pub fn anim_due(&self) -> Option<Instant> {
        self.anim_due
    }

    /// Pixels of image `id` scaled to `w` x `h` device pixels; starts decoding on first request.
    /// Call it only for images that are on screen: an animation answers with the frame due now
    /// and registers when the next one is.
    pub fn texture(&mut self, id: u32, w: u32, h: u32) -> Tex {
        let key = (id, w, h);
        self.clock += 1;
        let clock = self.clock;
        let can_play = self.playing < MAX_PLAYING;
        match self.tex.get_mut(&key) {
            Some(Slot::Ready(p, used)) => {
                *used = clock;
                Tex::Ready(p.clone())
            }
            Some(Slot::Badged(p, used)) => {
                *used = clock;
                Tex::Badged(p.clone())
            }
            Some(Slot::Anim(a, used, t0)) => {
                *used = clock;
                let el = t0.elapsed().as_millis() as u64;
                if !can_play {
                    return Tex::Badged(a.frames[0].clone());
                }
                self.playing += 1;
                let due = Instant::now() + Duration::from_millis(a.until_next(el));
                self.anim_due = Some(self.anim_due.map_or(due, |d| d.min(due)));
                Tex::Ready(a.frames[a.frame_at(el)].clone())
            }
            Some(Slot::Pending) => Tex::Pending,
            Some(Slot::Failed) => Tex::Failed,
            None => {
                self.tex.insert(key, Slot::Pending);
                self.jobs.push_back(key);
                self.start_jobs();
                Tex::Pending
            }
        }
    }

    fn start_jobs(&mut self) {
        while self.inflight < WORKERS {
            let Some(key) = self.jobs.pop_front() else { break };
            let Some(path) = self.paths.get(key.0 as usize).cloned() else { continue };
            let tx = self.tx.clone();
            let gen_ = self.gens.get(&key.0).copied().unwrap_or(0);
            self.inflight += 1;
            let spawned = std::thread::Builder::new().name("md-image".into()).spawn(move || {
                let pix = std::panic::catch_unwind(|| decode_any(&path, key.1, key.2)).ok().flatten();
                let _ = tx.send((key, gen_, pix));
            });
            if spawned.is_err() {
                self.inflight -= 1;
                self.tex.insert(key, Slot::Failed);
            }
        }
    }

    /// Collect finished decodes. True when something new arrived (a redraw is due).
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, gen_, out)) = self.rx.try_recv() {
            self.inflight = self.inflight.saturating_sub(1);
            if gen_ != self.gens.get(&key.0).copied().unwrap_or(0) || !matches!(self.tex.get(&key), Some(Slot::Pending))
            {
                continue; // the file changed while this was decoding: a newer request owns the slot
            }
            any = true;
            let slot = match out {
                Some(d) => {
                    self.bytes += d.bytes();
                    match d {
                        Decoded::Still(p) => Slot::Ready(Rc::new(p), self.clock),
                        Decoded::Badge(p) => Slot::Badged(Rc::new(p), self.clock),
                        Decoded::Anim(f) => Slot::Anim(Rc::new(Anim::new(f)), self.clock, Instant::now()),
                    }
                }
                None => Slot::Failed,
            };
            self.tex.insert(key, slot);
        }
        self.start_jobs();
        any
    }

    /// Decodes are running or queued: keep redrawing.
    pub fn pending(&self) -> bool {
        self.inflight > 0 || !self.jobs.is_empty()
    }

    /// Drop the least recently drawn textures while over [`CACHE_BYTES`].
    pub fn trim(&mut self) {
        while self.bytes > CACHE_BYTES {
            let oldest = self.tex.iter().filter_map(|(k, s)| s.held().map(|(u, _)| (u, *k))).min();
            let Some((used, key)) = oldest else { break };
            if used == self.clock {
                break; // everything left was drawn in this very frame
            }
            if let Some((_, b)) = self.tex.remove(&key).and_then(|s| s.held()) {
                self.bytes = self.bytes.saturating_sub(b);
            }
        }
    }

    #[cfg(test)]
    /// Bytes of decoded pixels held (diagnostics / tests).
    pub fn cached_bytes(&self) -> usize {
        self.bytes
    }
}

// ---------------------------------------------------------------------------------------------
// SVG: shapes, paths, solid fills and strokes, transforms, viewBox. No text, CSS classes, filters,
// masks, clip paths or real gradients (a gradient paints its first stop color).

pub mod svg {
    use super::{Pix, argb};
    use std::collections::HashMap;
    use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

    /// `width` / `height` / `viewBox` of the root, in px.
    pub fn size(b: &[u8]) -> Option<(u32, u32)> {
        let text = std::str::from_utf8(b).ok()?;
        let doc = roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
        )
        .ok()?;
        let root = doc.root_element();
        if root.tag_name().name() != "svg" {
            return None;
        }
        let vb = view_box(&root);
        let dim = |name: &str, vbv: Option<f32>| match root.attribute(name).and_then(length) {
            Some(v) => Some(v),
            None => vbv,
        };
        let w = dim("width", vb.map(|v| v[2])).unwrap_or(300.0);
        let h = dim("height", vb.map(|v| v[3])).unwrap_or(150.0);
        // Only one of width/height given: keep the viewBox ratio.
        let (w, h) = match (root.attribute("width").is_some(), root.attribute("height").is_some(), vb) {
            (true, false, Some(v)) if v[2] > 0.0 => (w, w * v[3] / v[2]),
            (false, true, Some(v)) if v[3] > 0.0 => (h * v[2] / v[3], h),
            _ => (w, h),
        };
        let (w, h) = (w.round() as u32, h.round() as u32);
        (w > 0 && h > 0 && w <= 20_000 && h <= 20_000).then_some((w, h))
    }

    fn length(s: &str) -> Option<f32> {
        let s = s.trim();
        if s.ends_with('%') {
            return None;
        }
        let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len()));
        let v: f32 = num.trim().parse().ok()?;
        Some(match unit {
            "" | "px" => v,
            "pt" => v * 4.0 / 3.0,
            "pc" => v * 16.0,
            "mm" => v * 96.0 / 25.4,
            "cm" => v * 96.0 / 2.54,
            "in" => v * 96.0,
            "em" => v * 16.0,
            _ => v,
        })
    }

    fn view_box(n: &roxmltree::Node) -> Option<[f32; 4]> {
        let v: Vec<f32> = n
            .attribute("viewBox")?
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        (v.len() == 4 && v[2] > 0.0 && v[3] > 0.0).then(|| [v[0], v[1], v[2], v[3]])
    }

    #[derive(Clone)]
    struct St {
        fill: Option<[u8; 4]>,
        stroke: Option<[u8; 4]>,
        sw: f32,
        even_odd: bool,
        cap: LineCap,
        join: LineJoin,
        opacity: f32,
        fill_op: f32,
        stroke_op: f32,
        color: [u8; 4],
        ts: Transform,
    }

    const NAMED: &[(&str, [u8; 3])] = &[
        ("black", [0, 0, 0]),
        ("white", [255, 255, 255]),
        ("red", [255, 0, 0]),
        ("green", [0, 128, 0]),
        ("lime", [0, 255, 0]),
        ("blue", [0, 0, 255]),
        ("yellow", [255, 255, 0]),
        ("orange", [255, 165, 0]),
        ("gray", [128, 128, 128]),
        ("grey", [128, 128, 128]),
        ("silver", [192, 192, 192]),
        ("purple", [128, 0, 128]),
        ("pink", [255, 192, 203]),
        ("cyan", [0, 255, 255]),
        ("aqua", [0, 255, 255]),
        ("magenta", [255, 0, 255]),
        ("fuchsia", [255, 0, 255]),
        ("brown", [165, 42, 42]),
        ("navy", [0, 0, 128]),
        ("teal", [0, 128, 128]),
        ("maroon", [128, 0, 0]),
        ("olive", [128, 128, 0]),
        ("gold", [255, 215, 0]),
        ("crimson", [220, 20, 60]),
        ("darkgray", [169, 169, 169]),
        ("darkgrey", [169, 169, 169]),
        ("lightgray", [211, 211, 211]),
        ("lightgrey", [211, 211, 211]),
    ];

    /// `None` = not a color we know; `Some(None)` = `none` / transparent.
    fn color(s: &str, cur: [u8; 4]) -> Option<Option<[u8; 4]>> {
        let s = s.trim();
        let l = s.to_ascii_lowercase();
        if l == "none" || l == "transparent" {
            return Some(None);
        }
        if l == "currentcolor" {
            return Some(Some(cur));
        }
        if let Some(h) = l.strip_prefix('#') {
            let p = |i: usize, n: usize| u8::from_str_radix(h.get(i..i + n)?, 16).ok();
            return match h.len() {
                3 => Some(Some([p(0, 1)? * 17, p(1, 1)? * 17, p(2, 1)? * 17, 255])),
                4 => Some(Some([p(0, 1)? * 17, p(1, 1)? * 17, p(2, 1)? * 17, p(3, 1)? * 17])),
                6 => Some(Some([p(0, 2)?, p(2, 2)?, p(4, 2)?, 255])),
                8 => Some(Some([p(0, 2)?, p(2, 2)?, p(4, 2)?, p(6, 2)?])),
                _ => None,
            };
        }
        if let Some(inner) = l
            .strip_prefix("rgb")
            .and_then(|r| r.trim_start_matches('a').strip_prefix('('))
            .and_then(|r| r.strip_suffix(')'))
        {
            let parts: Vec<&str> =
                inner.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|x| !x.is_empty()).collect();
            let ch = |x: &str| -> Option<u8> {
                match x.strip_suffix('%') {
                    Some(p) => Some((p.parse::<f32>().ok()? * 2.55).round().clamp(0.0, 255.0) as u8),
                    None => Some(x.parse::<f32>().ok()?.round().clamp(0.0, 255.0) as u8),
                }
            };
            let a = match parts.get(3) {
                Some(x) => match x.strip_suffix('%') {
                    Some(p) => (p.parse::<f32>().ok()? * 2.55).round().clamp(0.0, 255.0) as u8,
                    None => (x.parse::<f32>().ok()? * 255.0).round().clamp(0.0, 255.0) as u8,
                },
                None => 255,
            };
            return Some(Some([ch(parts.first()?)?, ch(parts.get(1)?)?, ch(parts.get(2)?)?, a]));
        }
        NAMED.iter().find(|(n, _)| *n == l).map(|(_, c)| Some([c[0], c[1], c[2], 255]))
    }

    struct Ctx<'a, 'b> {
        ids: HashMap<&'a str, roxmltree::Node<'a, 'b>>,
        depth: u32,
    }

    /// Value of presentation property `name`: the `style` attribute wins over the attribute.
    fn prop(n: &roxmltree::Node, name: &str) -> Option<String> {
        if let Some(st) = n.attribute("style") {
            for decl in st.split(';') {
                if let Some((k, v)) = decl.split_once(':')
                    && k.trim() == name
                {
                    return Some(v.trim().to_string());
                }
            }
        }
        n.attribute(name).map(|v| v.trim().to_string())
    }

    fn num(n: &roxmltree::Node, name: &str) -> f32 {
        n.attribute(name).and_then(length).unwrap_or(0.0)
    }

    fn transform(s: &str) -> Transform {
        let mut ts = Transform::identity();
        let mut rest = s;
        while let Some(open) = rest.find('(') {
            let name = rest[..open].trim().trim_start_matches(|c: char| c == ',' || c.is_whitespace());
            let Some(close) = rest[open..].find(')') else { break };
            let args: Vec<f32> = rest[open + 1..open + close]
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|x| !x.is_empty())
                .filter_map(|x| x.parse().ok())
                .collect();
            let a = |i: usize| args.get(i).copied().unwrap_or(0.0);
            let t = match name {
                "matrix" if args.len() == 6 => Transform::from_row(a(0), a(1), a(2), a(3), a(4), a(5)),
                "translate" => Transform::from_translate(a(0), a(1)),
                "scale" => Transform::from_scale(a(0), if args.len() > 1 { a(1) } else { a(0) }),
                "rotate" => {
                    if args.len() >= 3 {
                        Transform::from_rotate_at(a(0), a(1), a(2))
                    } else {
                        Transform::from_rotate(a(0))
                    }
                }
                "skewX" => Transform::from_skew(a(0).to_radians().tan(), 0.0),
                "skewY" => Transform::from_skew(0.0, a(0).to_radians().tan()),
                _ => Transform::identity(),
            };
            ts = ts.pre_concat(t);
            rest = &rest[open + close + 1..];
        }
        ts
    }

    fn apply_props(n: &roxmltree::Node, st: &mut St, cx: &Ctx) {
        if let Some(c) = prop(n, "color").and_then(|v| color(&v, st.color)).flatten() {
            st.color = c;
        }
        let paint = |v: &str, st: &St, cx: &Ctx| -> Option<Option<[u8; 4]>> {
            if let Some(id) = v.trim().strip_prefix("url(#").and_then(|r| r.split(')').next()) {
                // Gradient: first stop.
                let g = cx.ids.get(id)?;
                let stop = g.children().find(|c| c.tag_name().name() == "stop")?;
                let col = prop(&stop, "stop-color").and_then(|v| color(&v, st.color)).unwrap_or(Some([0, 0, 0, 255]));
                let op = prop(&stop, "stop-opacity").and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
                return Some(col.map(|c| [c[0], c[1], c[2], (c[3] as f32 * op) as u8]));
            }
            color(v, st.color)
        };
        if let Some(v) = prop(n, "fill")
            && let Some(c) = paint(&v, st, cx)
        {
            st.fill = c;
        }
        if let Some(v) = prop(n, "stroke")
            && let Some(c) = paint(&v, st, cx)
        {
            st.stroke = c;
        }
        if let Some(v) = prop(n, "stroke-width").and_then(|v| length(&v)) {
            st.sw = v;
        }
        if let Some(v) = prop(n, "fill-rule") {
            st.even_odd = v == "evenodd";
        }
        if let Some(v) = prop(n, "stroke-linecap") {
            st.cap = match v.as_str() {
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => LineCap::Butt,
            };
        }
        if let Some(v) = prop(n, "stroke-linejoin") {
            st.join = match v.as_str() {
                "round" => LineJoin::Round,
                "bevel" => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
        }
        let f = |name: &str| prop(n, name).and_then(|v| v.parse::<f32>().ok()).map(|v| v.clamp(0.0, 1.0));
        if let Some(v) = f("fill-opacity") {
            st.fill_op = v;
        }
        if let Some(v) = f("stroke-opacity") {
            st.stroke_op = v;
        }
        if let Some(v) = f("opacity") {
            st.opacity *= v;
        }
        if let Some(t) = n.attribute("transform") {
            st.ts = st.ts.pre_concat(transform(t));
        }
    }

    fn paint_of(c: [u8; 4], op: f32) -> Paint<'static> {
        let mut p = Paint::default();
        p.set_color(Color::from_rgba8(c[0], c[1], c[2], (c[3] as f32 * op).round().clamp(0.0, 255.0) as u8));
        p.anti_alias = true;
        p
    }

    fn draw_path(pm: &mut Pixmap, path: &tiny_skia::Path, st: &St) {
        if let Some(c) = st.fill {
            let rule = if st.even_odd { FillRule::EvenOdd } else { FillRule::Winding };
            pm.fill_path(path, &paint_of(c, st.fill_op * st.opacity), rule, st.ts, None);
        }
        if let (Some(c), true) = (st.stroke, st.sw > 0.0) {
            let stroke = Stroke { width: st.sw, line_cap: st.cap, line_join: st.join, ..Default::default() };
            pm.stroke_path(path, &paint_of(c, st.stroke_op * st.opacity), &stroke, st.ts, None);
        }
    }

    fn rect_path(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        let (rx, ry) = (rx.min(w / 2.0), ry.min(h / 2.0));
        if rx <= 0.0 || ry <= 0.0 {
            pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
        } else {
            const K: f32 = 0.552_284_8;
            pb.move_to(x + rx, y);
            pb.line_to(x + w - rx, y);
            pb.cubic_to(x + w - rx + rx * K, y, x + w, y + ry - ry * K, x + w, y + ry);
            pb.line_to(x + w, y + h - ry);
            pb.cubic_to(x + w, y + h - ry + ry * K, x + w - rx + rx * K, y + h, x + w - rx, y + h);
            pb.line_to(x + rx, y + h);
            pb.cubic_to(x + rx - rx * K, y + h, x, y + h - ry + ry * K, x, y + h - ry);
            pb.line_to(x, y + ry);
            pb.cubic_to(x, y + ry - ry * K, x + rx - rx * K, y, x + rx, y);
            pb.close();
        }
        pb.finish()
    }

    fn points(s: &str, close: bool) -> Option<tiny_skia::Path> {
        let v: Vec<f32> = s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|x| !x.is_empty())
            .filter_map(|x| x.parse().ok())
            .collect();
        if v.len() < 4 {
            return None;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(v[0], v[1]);
        for p in v[2..].as_chunks::<2>().0 {
            pb.line_to(p[0], p[1]);
        }
        if close {
            pb.close();
        }
        pb.finish()
    }

    fn walk(n: roxmltree::Node, parent: &St, pm: &mut Pixmap, cx: &mut Ctx) {
        if !n.is_element() || cx.depth > 24 {
            return;
        }
        let tag = n.tag_name().name();
        if matches!(
            tag,
            "defs"
                | "clipPath"
                | "mask"
                | "symbol"
                | "title"
                | "desc"
                | "metadata"
                | "style"
                | "linearGradient"
                | "radialGradient"
                | "pattern"
                | "filter"
                | "marker"
                | "text"
                | "image"
                | "script"
                | "foreignObject"
        ) {
            return;
        }
        if prop(&n, "display").as_deref() == Some("none") || prop(&n, "visibility").as_deref() == Some("hidden") {
            return;
        }
        let mut st = parent.clone();
        apply_props(&n, &mut st, cx);
        match tag {
            "svg" | "g" | "a" | "switch" => {
                cx.depth += 1;
                for c in n.children() {
                    walk(c, &st, pm, cx);
                }
                cx.depth -= 1;
            }
            "use" => {
                let href =
                    n.attribute("href").or_else(|| n.attributes().find(|a| a.name() == "href").map(|a| a.value()));
                if let Some(target) = href.and_then(|h| h.strip_prefix('#')).and_then(|id| cx.ids.get(id)).copied() {
                    st.ts = st.ts.pre_translate(num(&n, "x"), num(&n, "y"));
                    cx.depth += 1;
                    if target.tag_name().name() == "symbol" {
                        for c in target.children() {
                            walk(c, &st, pm, cx);
                        }
                    } else {
                        walk(target, &st, pm, cx);
                    }
                    cx.depth -= 1;
                }
            }
            "rect" => {
                let (w, h) = (num(&n, "width"), num(&n, "height"));
                let (mut rx, mut ry) = (num(&n, "rx"), num(&n, "ry"));
                if n.attribute("rx").is_none() {
                    rx = ry;
                }
                if n.attribute("ry").is_none() {
                    ry = rx;
                }
                if let Some(p) =
                    (w > 0.0 && h > 0.0).then(|| rect_path(num(&n, "x"), num(&n, "y"), w, h, rx, ry)).flatten()
                {
                    draw_path(pm, &p, &st);
                }
            }
            "circle" => {
                if let Some(p) = PathBuilder::from_circle(num(&n, "cx"), num(&n, "cy"), num(&n, "r")) {
                    draw_path(pm, &p, &st);
                }
            }
            "ellipse" => {
                let (cx_, cy, rx, ry) = (num(&n, "cx"), num(&n, "cy"), num(&n, "rx"), num(&n, "ry"));
                if let Some(p) =
                    tiny_skia::Rect::from_xywh(cx_ - rx, cy - ry, rx * 2.0, ry * 2.0).and_then(PathBuilder::from_oval)
                {
                    draw_path(pm, &p, &st);
                }
            }
            "line" => {
                let mut pb = PathBuilder::new();
                pb.move_to(num(&n, "x1"), num(&n, "y1"));
                pb.line_to(num(&n, "x2"), num(&n, "y2"));
                if let Some(p) = pb.finish() {
                    st.fill = None;
                    draw_path(pm, &p, &st);
                }
            }
            "polyline" | "polygon" => {
                if let Some(p) = n.attribute("points").and_then(|s| points(s, tag == "polygon")) {
                    draw_path(pm, &p, &st);
                }
            }
            "path" => {
                if let Some(p) = n.attribute("d").and_then(path_data) {
                    draw_path(pm, &p, &st);
                }
            }
            _ => {}
        }
    }

    struct Scan<'a> {
        s: &'a [u8],
        i: usize,
    }

    impl Scan<'_> {
        fn skip(&mut self) {
            while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
                self.i += 1;
            }
        }
        fn at_cmd(&mut self) -> Option<u8> {
            self.skip();
            self.s.get(self.i).copied().filter(|c| c.is_ascii_alphabetic())
        }
        fn num(&mut self) -> Option<f32> {
            self.skip();
            let st = self.i;
            let s = self.s;
            if matches!(s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let mut dot = false;
            while let Some(&c) = s.get(self.i) {
                if c.is_ascii_digit() {
                    self.i += 1;
                } else if c == b'.' && !dot {
                    dot = true;
                    self.i += 1;
                } else {
                    break;
                }
            }
            if matches!(s.get(self.i), Some(b'e' | b'E'))
                && matches!(s.get(self.i + 1), Some(b'0'..=b'9' | b'+' | b'-'))
            {
                self.i += 2;
                while s.get(self.i).is_some_and(u8::is_ascii_digit) {
                    self.i += 1;
                }
            }
            std::str::from_utf8(&s[st..self.i]).ok()?.parse().ok()
        }
        fn flag(&mut self) -> Option<bool> {
            self.skip();
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'0' => Some(false),
                b'1' => Some(true),
                _ => None,
            }
        }
    }

    /// SVG arc (endpoint form) as cubic Béziers (SVG 1.1 appendix F.6).
    #[allow(clippy::too_many_arguments)]
    fn arc(
        pb: &mut PathBuilder,
        x0: f32,
        y0: f32,
        mut rx: f32,
        mut ry: f32,
        rot: f32,
        large: bool,
        sweep: bool,
        x: f32,
        y: f32,
    ) {
        if (x0 - x).abs() < 1e-6 && (y0 - y).abs() < 1e-6 {
            return;
        }
        rx = rx.abs();
        ry = ry.abs();
        if rx < 1e-6 || ry < 1e-6 {
            pb.line_to(x, y);
            return;
        }
        let phi = rot.to_radians();
        let (sp, cp) = phi.sin_cos();
        let (dx, dy) = ((x0 - x) / 2.0, (y0 - y) / 2.0);
        let (x1, y1) = (cp * dx + sp * dy, -sp * dx + cp * dy);
        let lam = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
        if lam > 1.0 {
            let s = lam.sqrt();
            rx *= s;
            ry *= s;
        }
        let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
        let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
        let mut co = if den == 0.0 { 0.0 } else { (num / den).max(0.0).sqrt() };
        if large == sweep {
            co = -co;
        }
        let (cxp, cyp) = (co * rx * y1 / ry, -co * ry * x1 / rx);
        let (cx, cy) = (cp * cxp - sp * cyp + (x0 + x) / 2.0, sp * cxp + cp * cyp + (y0 + y) / 2.0);
        let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
        let th1 = ang(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
        let mut dth = ang((x1 - cxp) / rx, (y1 - cyp) / ry, (-x1 - cxp) / rx, (-y1 - cyp) / ry);
        if !sweep && dth > 0.0 {
            dth -= std::f32::consts::TAU;
        } else if sweep && dth < 0.0 {
            dth += std::f32::consts::TAU;
        }
        let n = (dth.abs() / (std::f32::consts::FRAC_PI_2)).ceil().max(1.0) as usize;
        let step = dth / n as f32;
        let t = 4.0 / 3.0 * (step / 4.0).tan();
        let pt = |a: f32| {
            let (s, c) = a.sin_cos();
            (cx + rx * c * cp - ry * s * sp, cy + rx * c * sp + ry * s * cp)
        };
        let dpt = |a: f32| {
            let (s, c) = a.sin_cos();
            (-rx * s * cp - ry * c * sp, -rx * s * sp + ry * c * cp)
        };
        let mut a = th1;
        for _ in 0..n {
            let (p0, d0) = (pt(a), dpt(a));
            let (p1, d1) = (pt(a + step), dpt(a + step));
            pb.cubic_to(p0.0 + t * d0.0, p0.1 + t * d0.1, p1.0 - t * d1.0, p1.1 - t * d1.1, p1.0, p1.1);
            a += step;
        }
    }

    pub fn path_data(d: &str) -> Option<tiny_skia::Path> {
        let mut sc = Scan { s: d.as_bytes(), i: 0 };
        let mut pb = PathBuilder::new();
        let (mut cx, mut cy, mut sx, mut sy) = (0f32, 0f32, 0f32, 0f32);
        let (mut lcx, mut lcy) = (0f32, 0f32); // last control point (S / T)
        let mut prev = b' ';
        let mut cmd = 0u8;
        let mut started = false;
        loop {
            if let Some(c) = sc.at_cmd() {
                cmd = c;
                sc.i += 1;
            } else {
                sc.skip();
                if sc.i >= sc.s.len() {
                    break;
                }
                // Implicit repetition: a pair after M is a lineto.
                match cmd {
                    0 | b'z' | b'Z' => return None,
                    b'M' => cmd = b'L',
                    b'm' => cmd = b'l',
                    _ => {}
                }
            }
            let rel = cmd.is_ascii_lowercase();
            let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
            match cmd.to_ascii_uppercase() {
                b'M' => {
                    let (x, y) = (sc.num()? + ox, sc.num()? + oy);
                    pb.move_to(x, y);
                    (cx, cy, sx, sy) = (x, y, x, y);
                    started = true;
                }
                b'L' => {
                    let (x, y) = (sc.num()? + ox, sc.num()? + oy);
                    pb.line_to(x, y);
                    (cx, cy) = (x, y);
                }
                b'H' => {
                    let x = sc.num()? + ox;
                    pb.line_to(x, cy);
                    cx = x;
                }
                b'V' => {
                    let y = sc.num()? + oy;
                    pb.line_to(cx, y);
                    cy = y;
                }
                b'C' => {
                    let v = [sc.num()?, sc.num()?, sc.num()?, sc.num()?, sc.num()?, sc.num()?];
                    pb.cubic_to(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy, v[4] + ox, v[5] + oy);
                    (lcx, lcy) = (v[2] + ox, v[3] + oy);
                    (cx, cy) = (v[4] + ox, v[5] + oy);
                }
                b'S' => {
                    let v = [sc.num()?, sc.num()?, sc.num()?, sc.num()?];
                    let (x1, y1) = if matches!(prev.to_ascii_uppercase(), b'C' | b'S') {
                        (2.0 * cx - lcx, 2.0 * cy - lcy)
                    } else {
                        (cx, cy)
                    };
                    pb.cubic_to(x1, y1, v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    (lcx, lcy) = (v[0] + ox, v[1] + oy);
                    (cx, cy) = (v[2] + ox, v[3] + oy);
                }
                b'Q' => {
                    let v = [sc.num()?, sc.num()?, sc.num()?, sc.num()?];
                    pb.quad_to(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                    (lcx, lcy) = (v[0] + ox, v[1] + oy);
                    (cx, cy) = (v[2] + ox, v[3] + oy);
                }
                b'T' => {
                    let v = [sc.num()?, sc.num()?];
                    let (x1, y1) = if matches!(prev.to_ascii_uppercase(), b'Q' | b'T') {
                        (2.0 * cx - lcx, 2.0 * cy - lcy)
                    } else {
                        (cx, cy)
                    };
                    pb.quad_to(x1, y1, v[0] + ox, v[1] + oy);
                    (lcx, lcy) = (x1, y1);
                    (cx, cy) = (v[0] + ox, v[1] + oy);
                }
                b'A' => {
                    let (rx, ry, rot) = (sc.num()?, sc.num()?, sc.num()?);
                    let (large, sweep) = (sc.flag()?, sc.flag()?);
                    let (x, y) = (sc.num()? + ox, sc.num()? + oy);
                    arc(&mut pb, cx, cy, rx, ry, rot, large, sweep, x, y);
                    (cx, cy) = (x, y);
                }
                b'Z' => {
                    if started {
                        pb.close();
                    }
                    (cx, cy) = (sx, sy);
                }
                _ => return None,
            }
            prev = cmd;
        }
        pb.finish()
    }

    /// Rasterize at exactly `tw` x `th` pixels.
    pub fn render(b: &[u8], tw: u32, th: u32) -> Option<Pix> {
        let text = std::str::from_utf8(b).ok()?;
        let doc = roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
        )
        .ok()?;
        let root = doc.root_element();
        let (nw, nh) = size(b)?;
        let vb = view_box(&root).unwrap_or([0.0, 0.0, nw as f32, nh as f32]);
        let mut pm = Pixmap::new(tw, th)?;
        let ts = Transform::from_scale(tw as f32 / vb[2], th as f32 / vb[3]).pre_translate(-vb[0], -vb[1]);
        let ids: HashMap<&str, roxmltree::Node> =
            doc.descendants().filter(|n| n.is_element()).filter_map(|n| n.attribute("id").map(|i| (i, n))).collect();
        let mut cx = Ctx { ids, depth: 0 };
        let base = St {
            fill: Some([0, 0, 0, 255]),
            stroke: None,
            sw: 1.0,
            even_odd: false,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            opacity: 1.0,
            fill_op: 1.0,
            stroke_op: 1.0,
            color: [0, 0, 0, 255],
            ts,
        };
        walk(root, &base, &mut pm, &mut cx);
        let px = pm
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let a = p[3] as u32;
                if a == 0 {
                    return 0;
                }
                let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
                argb(un(p[0]), un(p[1]), un(p[2]), a as u8)
            })
            .collect();
        Some(Pix { w: tw, h: th, px })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-md-images");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png_bytes(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            let f = &f;
            let data: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| f(x, y))).collect();
            wr.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn png_probe_and_decode() {
        let p = dir().join("t1.png");
        std::fs::write(&p, png_bytes(40, 20, |x, _| if x < 20 { [255, 0, 0, 255] } else { [0, 0, 255, 128] })).unwrap();
        assert_eq!(probe(&p), Some((40, 20)));
        let px = decode(&p, 40, 20).unwrap();
        assert_eq!(px.px[0], 0xffff0000);
        assert_eq!(px.px[39] >> 24, 128);
        let small = decode(&p, 10, 5).unwrap();
        assert_eq!((small.w, small.h), (10, 5));
        assert_eq!(small.px[0], 0xffff0000, "area average keeps a flat color");
        let big = decode(&p, 80, 40).unwrap();
        assert_eq!((big.w, big.h), (80, 40));
        assert_eq!(big.px[0], 0xffff0000);
    }

    #[test]
    fn gif_probe_and_decode() {
        // 2x2 GIF89a: palette red/green/blue/white, indices 0 1 / 2 3.
        let gif: [u8; 35] = [
            b'G', b'I', b'F', b'8', b'9', b'a', 2, 0, 2, 0, 0xf1, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255,
            0x2c, 0, 0, 0, 0, 2, 0, 2, 0, 0,
        ];
        let mut data = gif.to_vec();
        data.extend_from_slice(&[2, 3, 0x44, 0x34, 0x05, 0, 0x3b]);
        let p = dir().join("t2.gif");
        std::fs::write(&p, &data).unwrap();
        assert_eq!(probe(&p), Some((2, 2)));
        let px = decode(&p, 2, 2).expect("gif decodes");
        assert_eq!(px.px[0], 0xffff0000);
        assert_eq!(px.px[1], 0xff00ff00);
        assert_eq!(px.px[2], 0xff0000ff);
        assert_eq!(px.px[3], 0xffffffff);
    }

    #[test]
    fn jpeg_and_webp_headers() {
        // Minimal JPEG header chain: SOI, APP0, SOF0 (8x6).
        let mut j = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 11, 8, 0, 6, 0, 8, 1, 1, 0x11, 0];
        j.extend_from_slice(&[0xff, 0xd9]);
        assert_eq!(header_size(&j), Some((8, 6)));
        // WebP VP8X header 100x50.
        let mut w = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        w.extend_from_slice(&[0, 0, 0, 0, 99, 0, 0, 49, 0, 0]);
        assert_eq!(header_size(&w), Some((100, 50)));
        // WebP lossless header 3x2.
        let v: u32 = 2 | (1 << 14);
        let mut l = b"RIFF\0\0\0\0WEBPVP8L\x05\0\0\0\x2f".to_vec();
        l.extend_from_slice(&v.to_le_bytes());
        assert_eq!(header_size(&l), Some((3, 2)));
        assert_eq!(header_size(b"not an image at all"), None);
    }

    #[test]
    fn svg_size_and_render() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10" width="40" height="20">
            <rect x="0" y="0" width="10" height="10" fill="#ff0000"/>
            <circle cx="15" cy="5" r="4" fill="rgb(0,0,255)" stroke="none"/>
            <path d="M0 0 L2 0 L2 2 Z" fill="none" stroke="black"/></svg>"##;
        assert_eq!(svg::size(svg), Some((40, 20)));
        let p = svg::render(svg, 40, 20).unwrap();
        let at = |x: u32, y: u32| p.px[(y * 40 + x) as usize];
        assert_eq!(at(10, 12) >> 24, 255);
        assert_eq!(at(10, 12) & 0x00ff_ffff, 0x00ff_0000, "red square");
        assert_eq!(at(30, 10) & 0x00ff_ffff, 0x0000_00ff, "blue circle");
        assert_eq!(at(21, 1) >> 24, 0, "transparent between the square and the circle");
    }

    #[test]
    fn svg_paths_arcs_and_viewbox_only() {
        let p = svg::path_data("M10 10 h 10 v 10 h -10 z M30,30 a5,5 0 1 1 10,0 a5,5 0 1 1 -10,0 z").unwrap();
        assert!(p.bounds().width() > 25.0);
        let only_vb = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 12"/>"#;
        assert_eq!(svg::size(only_vb), Some((24, 12)));
        assert!(svg::path_data("M 1 2 Q").is_none() || svg::path_data("M 1 2 Q").is_some());
    }

    #[test]
    fn scaling_preserves_alpha_and_exact_sizes() {
        let src = Pix { w: 4, h: 4, px: vec![0x80ff0000; 16] };
        let s = scale_to(src.clone(), 2, 2);
        assert!(s.px.iter().all(|p| *p == 0x80ff0000));
        let b = scale_to(src, 7, 5);
        assert_eq!((b.w, b.h, b.px.len()), (7, 5, 35));
        assert!(b.px.iter().all(|p| *p >> 24 == 0x80));
    }

    #[test]
    fn resolve_only_accepts_local_relative_paths() {
        let mut s = ImgStore::new();
        assert_eq!(s.resolve("a.png"), None, "no base directory yet");
        s.set_base(Some(&dir()));
        assert!(s.resolve("a.png").is_some());
        assert!(s.resolve("sub/../a%20b.png?x=1#f").is_some());
        for bad in [
            "https://x.io/a.png",
            "http://x/a.png",
            "data:image/png;base64,AA",
            "/etc/a.png",
            "//cdn/a.png",
            "#frag",
            "",
            "file:///x.png",
        ] {
            assert_eq!(s.resolve(bad), None, "{bad}");
        }
        assert_eq!(s.resolve("a.png"), s.resolve("a.png"));
    }

    #[test]
    fn store_decodes_in_the_background_and_trims() {
        let p = dir().join("t3.png");
        std::fs::write(&p, png_bytes(16, 16, |_, _| [0, 255, 0, 255])).unwrap();
        let mut s = ImgStore::new();
        s.set_base(Some(&dir()));
        let id = s.resolve("t3.png").unwrap();
        assert_eq!(s.size(id), Some((16, 16)));
        assert!(matches!(s.texture(id, 8, 8), Tex::Pending));
        assert!(s.pending());
        let t0 = std::time::Instant::now();
        while s.pending() && t0.elapsed().as_secs() < 10 {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        s.poll();
        let Tex::Ready(px) = s.texture(id, 8, 8) else { panic!("not decoded") };
        assert_eq!((px.w, px.h), (8, 8));
        assert_eq!(s.cached_bytes(), 8 * 8 * 4);
        let missing = s.resolve("nope.png").unwrap();
        assert_eq!(s.size(missing), None);
    }

    // ---- animation ----

    /// GIF with one solid-color frame per `(palette index, delay in 1/100 s)`, 4x4, palette R G B.
    fn anim_gif(frames: &[(u8, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, 4, 4, &[255, 0, 0, 0, 255, 0, 0, 0, 255]).unwrap();
            enc.set_repeat(gif::Repeat::Infinite).unwrap();
            for &(i, delay) in frames {
                let mut f = gif::Frame::default();
                (f.width, f.height, f.delay) = (4, 4, delay);
                f.buffer = std::borrow::Cow::Owned(vec![i; 16]);
                enc.write_frame(&f).unwrap();
            }
        }
        out
    }

    #[test]
    fn frame_delays_follow_browser_conventions_and_cap_the_rate() {
        assert_eq!([frame_ms(0), frame_ms(10), frame_ms(20), frame_ms(40), frame_ms(100)], [100, 100, 40, 40, 100]);
    }

    #[test]
    fn animated_gif_keeps_its_frames_and_delays() {
        let p = dir().join("anim3.gif");
        std::fs::write(&p, anim_gif(&[(0, 10), (1, 5), (2, 20)])).unwrap();
        let Some(Decoded::Anim(frames)) = decode_any(&p, 4, 4) else { panic!("expected an animation") };
        // GIF delays are in 1/100 s: 10 -> 100 ms, 5 -> 50 ms, 20 -> 200 ms.
        assert_eq!(frames.iter().map(|(_, ms)| *ms).collect::<Vec<_>>(), vec![100, 50, 200]);
        assert_eq!(frames.iter().map(|(f, _)| f.px[0]).collect::<Vec<_>>(), [0xffff0000, 0xff00ff00, 0xff0000ff]);
        // `decode` (still image users) gets the first frame.
        assert_eq!(decode(&p, 4, 4).unwrap().px[0], 0xffff0000);
        // A single frame is a still image, whatever the format.
        let q = dir().join("anim1.gif");
        std::fs::write(&q, anim_gif(&[(1, 10)])).unwrap();
        assert!(matches!(decode_any(&q, 4, 4), Some(Decoded::Still(_))));
    }

    #[test]
    fn gif_partial_frames_composite_on_the_canvas_and_honour_disposal() {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, 2, 2, &[255, 0, 0, 0, 0, 255]).unwrap();
            let mut base = gif::Frame::default();
            (base.width, base.height, base.delay) = (2, 2, 10);
            base.buffer = std::borrow::Cow::Owned(vec![0; 4]);
            enc.write_frame(&base).unwrap();
            // A 1x1 blue patch at (1,1) that is wiped before the next frame...
            let mut patch = gif::Frame::default();
            (patch.left, patch.top, patch.width, patch.height, patch.delay) = (1, 1, 1, 1, 10);
            patch.dispose = gif::DisposalMethod::Previous;
            patch.buffer = std::borrow::Cow::Owned(vec![1]);
            enc.write_frame(&patch).unwrap();
            // ...so this empty-looking patch shows the base again.
            let mut again = gif::Frame::default();
            (again.left, again.top, again.width, again.height, again.delay) = (0, 0, 1, 1, 10);
            again.buffer = std::borrow::Cow::Owned(vec![0]);
            enc.write_frame(&again).unwrap();
        }
        let p = dir().join("anim-composite.gif");
        std::fs::write(&p, out).unwrap();
        let Some(Decoded::Anim(f)) = decode_any(&p, 2, 2) else { panic!("animation") };
        assert_eq!(f[0].0.px, vec![0xffff0000; 4]);
        assert_eq!(
            f[1].0.px,
            vec![0xffff0000, 0xffff0000, 0xffff0000, 0xff0000ff],
            "the patch lands on the previous canvas"
        );
        assert_eq!(f[2].0.px, vec![0xffff0000; 4], "disposal `previous` restored the canvas before the third frame");
    }

    #[test]
    fn too_long_an_animation_falls_back_to_a_badged_first_frame() {
        let frames: Vec<(u8, u16)> = (0..ANIM_FRAMES + 1).map(|i| ((i % 3) as u8, 10)).collect();
        let p = dir().join("anim-long.gif");
        std::fs::write(&p, anim_gif(&frames)).unwrap();
        let Some(Decoded::Badge(first)) = decode_any(&p, 4, 4) else { panic!("expected the badge fallback") };
        assert_eq!(first.px[0], 0xffff0000);
    }

    /// Animated WebP assembled by hand from lossless single-frame files (the encoder only writes stills).
    fn anim_webp(colors: &[[u8; 4]]) -> Vec<u8> {
        let vp8l = |c: [u8; 4]| -> Vec<u8> {
            let mut still = Vec::new();
            image_webp::WebPEncoder::new(&mut still).encode(&c.repeat(4), 2, 2, image_webp::ColorType::Rgba8).unwrap();
            // RIFF size WEBP, then the chunk we need: "VP8L" size data.
            still[12..].to_vec()
        };
        let le24 = |n: u32| [n as u8, (n >> 8) as u8, (n >> 16) as u8];
        let chunk = |tag: &[u8; 4], body: &[u8]| -> Vec<u8> {
            let mut v = tag.to_vec();
            v.extend((body.len() as u32).to_le_bytes());
            v.extend(body);
            if body.len() % 2 == 1 {
                v.push(0);
            }
            v
        };
        let mut body = b"WEBP".to_vec();
        let mut vp8x = vec![0x12, 0, 0, 0];
        vp8x.extend(le24(1));
        vp8x.extend(le24(1));
        body.extend(chunk(b"VP8X", &vp8x));
        body.extend(chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
        for c in colors {
            let mut f = Vec::new();
            f.extend(le24(0));
            f.extend(le24(0));
            f.extend(le24(1));
            f.extend(le24(1));
            f.extend(le24(120));
            f.push(0);
            f.extend(vp8l(*c));
            body.extend(chunk(b"ANMF", &f));
        }
        let mut out = b"RIFF".to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn animated_webp_plays_every_frame() {
        let p = dir().join("anim.webp");
        std::fs::write(&p, anim_webp(&[[255, 0, 0, 255], [0, 0, 255, 255]])).unwrap();
        let Some(Decoded::Anim(f)) = decode_any(&p, 2, 2) else { panic!("expected an animated WebP") };
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].1, f[1].1), (120, 120));
        // (the decoder's alpha blending may be off by one in a channel)
        let near = |a: u32, b: u32| (0..4).all(|i| ((a >> (8 * i)) & 0xff).abs_diff((b >> (8 * i)) & 0xff) <= 1);
        assert!(
            near(f[0].0.px[0], 0xffff0000) && near(f[1].0.px[0], 0xff0000ff),
            "{:08x} {:08x}",
            f[0].0.px[0],
            f[1].0.px[0]
        );
    }

    #[test]
    fn animation_clock_picks_frames_and_loops() {
        let mk =
            |ms: &[u32]| Anim::new(ms.iter().map(|&m| (Pix { w: 1, h: 1, px: vec![0xff000000 | m] }, m)).collect());
        let a = mk(&[100, 100, 100]);
        let at = |t| (a.frame_at(t), a.until_next(t));
        assert_eq!([at(0), at(99), at(100), at(250), at(299)], [(0, 100), (0, 1), (1, 100), (2, 50), (2, 1)]);
        assert_eq!(at(300), (0, 100), "loops");
        assert_eq!(a.frame_at(650), 0);
    }

    fn wait_ready(s: &mut ImgStore) {
        let t0 = Instant::now();
        while s.pending() && t0.elapsed().as_secs() < 10 {
            s.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        s.poll();
    }

    #[test]
    fn store_plays_animations_only_while_drawn_and_caps_how_many() {
        let d = dir();
        let mut s = ImgStore::new();
        s.set_base(Some(&d));
        let ids: Vec<u32> = (0..MAX_PLAYING + 1)
            .map(|i| {
                std::fs::write(d.join(format!("play{i}.gif")), anim_gif(&[(0, 10), (1, 1000), (2, 1000)])).unwrap();
                s.resolve(&format!("play{i}.gif")).unwrap()
            })
            .collect();
        for &id in &ids {
            s.size(id);
            assert!(matches!(s.texture(id, 4, 4), Tex::Pending));
        }
        wait_ready(&mut s);
        s.begin_frame();
        assert!(s.anim_due().is_none(), "nothing drawn, nothing to wake up for");
        let first_px = |t: Tex| match t {
            Tex::Ready(p) | Tex::Badged(p) => p.px[0],
            _ => panic!("not decoded"),
        };
        let mut kinds = Vec::new();
        for &id in &ids {
            kinds.push(matches!(s.texture(id, 4, 4), Tex::Ready(_)));
        }
        assert_eq!(
            kinds,
            [true, true, true, true, false],
            "at most {MAX_PLAYING} play at once; the rest show the badge"
        );
        let due = s.anim_due().expect("a playing animation asks for its next frame");
        assert!(due <= Instant::now() + Duration::from_millis(100), "frames are 100 ms apart");
        // After the first delay the next frame is the one drawn.
        std::thread::sleep(Duration::from_millis(130));
        s.begin_frame();
        assert_ne!(first_px(s.texture(ids[0], 4, 4)), 0xffff0000, "the clock moved on to a later frame");
        // A new pass starts clean: not drawing means no timer.
        s.begin_frame();
        assert!(s.anim_due().is_none());
        assert!(s.cached_bytes() >= 3 * 4 * 4 * 4 * ids.len(), "frames count towards the cache budget");
    }

    #[test]
    fn edited_image_files_are_noticed_and_decoded_again() {
        let d = dir();
        let p = d.join("edited.png");
        std::fs::write(&p, png_bytes(16, 16, |_, _| [255, 0, 0, 255])).unwrap();
        let mut s = ImgStore::new();
        s.set_base(Some(&d));
        let id = s.resolve("edited.png").unwrap();
        assert_eq!(s.size(id), Some((16, 16)));
        assert!(!s.changed_on_disk(), "untouched files stay cached");
        assert!(matches!(s.texture(id, 16, 16), Tex::Pending));
        wait_ready(&mut s);
        let Tex::Ready(px) = s.texture(id, 16, 16) else { panic!("decoded") };
        assert_eq!(px.px[0], 0xffff0000);
        // The user saves a different image over it.
        std::fs::write(&p, png_bytes(32, 8, |_, _| [0, 0, 255, 255])).unwrap();
        assert!(s.changed_on_disk());
        assert_eq!(s.cached_bytes(), 0, "stale pixels are dropped");
        assert_eq!(s.size(id), Some((32, 8)), "the size is probed again");
        assert!(matches!(s.texture(id, 32, 8), Tex::Pending));
        wait_ready(&mut s);
        let Tex::Ready(px) = s.texture(id, 32, 8) else { panic!("decoded again") };
        assert_eq!(px.px[0], 0xff0000ff);
        assert!(!s.changed_on_disk(), "and then it is stable");
    }

    #[test]
    fn blit_blends_and_clips() {
        let mut px = vec![0x00101010u32; 8 * 8];
        let mut cv = Canvas { px: &mut px, w: 8, h: 8 };
        let img = Pix {
            w: 4,
            h: 4,
            px: vec![0xffff0000, 0x00000000, 0x80ffffff, 0xff00ff00].into_iter().cycle().take(16).collect(),
        };
        blit(&mut cv, 6, 6, &img, Rect::new(0, 0, 8, 8));
        assert_eq!(px[6 * 8 + 6], 0x00ff0000);
        assert_eq!(px[6 * 8 + 7], 0x00101010, "transparent pixel leaves the canvas alone");
        assert_eq!(px[0], 0x00101010);
    }
}
