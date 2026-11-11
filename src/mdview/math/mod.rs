//! TeX math for the Markdown preview: `$..$` / `$$..$$` typeset with the embedded STIX Two Math
//! font instead of shown as source.
//!
//! Pipeline: [`parse`] turns the TeX source into a node tree (unknown commands and syntax errors
//! become a [`MathErr`] with the byte range to blame), [`typeset`] lays it out into positioned
//! glyphs / rules / text runs ([`MathLay`], logical px, y down from the baseline), and [`paint`]
//! rasterizes the glyphs once per UI scale into an alpha mask that is then just blended in the
//! text color. [`MathCache`] keeps one layout per (source, size, style).
//!
//! `\text{..}` runs are measured and drawn with the preview's body font through [`TextMeasure`]
//! and `TextCtx`, so they match the surrounding prose.

mod font;
mod parse;
mod syms;
#[cfg(test)]
mod tests;
mod typeset;

pub use parse::MathErr;

use crate::theme::Rgba;
use crate::ui::{Canvas, Rect, TextCtx};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// A positioned piece of a formula. `x` is from the formula's left edge, `y` from its baseline
/// (down is positive).
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A math-font glyph with its origin at (`x`, `y`); `sx` / `sy` are px per font unit (they
    /// differ when a delimiter is stretched by scaling).
    Glyph { x: f32, y: f32, gid: u16, sx: f32, sy: f32 },
    /// A filled rectangle (fraction bars, radicals, overlines); `y` is its top.
    Rule { x: f32, y: f32, w: f32, h: f32 },
    /// Body-font text (`\text{..}`) with its baseline at `y`; `bl` is the baseline's offset below
    /// the top of the text box `TextCtx` draws.
    Text { x: f32, y: f32, s: String, size: f32, bold: bool, bl: f32 },
}

impl Item {
    fn moved(mut self, dx: f32, dy: f32) -> Item {
        match &mut self {
            Item::Glyph { x, y, .. } | Item::Rule { x, y, .. } | Item::Text { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
        }
        self
    }
}

/// A box under construction: width, height above and depth below the baseline, its items, and
/// the italic correction of its last glyph.
#[derive(Clone, Debug, Default)]
pub struct HBox {
    pub w: f32,
    pub h: f32,
    pub d: f32,
    pub items: Vec<Item>,
    pub ic: f32,
}

impl HBox {
    /// Merge `b` placed at `dx`, shifted down by `dy` (the width is left to the caller).
    pub fn add(&mut self, b: HBox, dx: f32, dy: f32) {
        self.h = self.h.max(b.h - dy);
        self.d = self.d.max(b.d + dy);
        self.items.extend(b.items.into_iter().map(|it| it.moved(dx, dy)));
    }

    /// Append `b` to the right.
    pub fn append(&mut self, b: HBox) {
        let x = self.w;
        let (w, ic) = (b.w, b.ic);
        self.add(b, x, 0.0);
        self.w = x + w;
        self.ic = ic;
    }
}

/// Size of a `\text{..}` run in the body font.
pub struct TextMetrics {
    pub w: f32,
    /// Height above / depth below the baseline used for the box.
    pub asc: f32,
    pub desc: f32,
    /// Baseline offset below the top of the box `TextCtx` draws text in.
    pub bl: f32,
}

/// Measures body-font text (the preview passes one backed by `TextCtx`, tests a fake).
pub trait TextMeasure {
    fn text(&mut self, s: &str, size: f32, bold: bool) -> TextMetrics;
}

/// A typeset formula: width, height above / depth below the baseline (logical px) and its items.
#[derive(Debug)]
pub struct MathLay {
    pub w: f32,
    pub h: f32,
    pub d: f32,
    pub items: Vec<Item>,
    /// Glyph coverage rasterized for one UI scale (its bits), made on first paint.
    raster: RefCell<Option<(u32, Option<Rc<Raster>>)>>,
}

/// Alpha coverage of a formula's glyphs and rules in device pixels; `x0` / `y0` place its top-left
/// relative to the formula origin (left edge, baseline).
#[derive(Debug)]
struct Raster {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    a: Vec<u8>,
}

/// Parse and typeset `src` at `size` px (display style when `display`).
pub fn layout(src: &str, size: f32, display: bool, m: &mut dyn TextMeasure) -> Result<MathLay, MathErr> {
    let nodes = parse::parse(src)?;
    let st = typeset::St { s: if display { parse::Style::D } else { parse::Style::T }, cramped: false };
    let len = src.len();
    let b = font::with_font(|f| {
        let mut ts = typeset::Ts { f, base: size, m, len, middle: None };
        ts.list(&nodes, st)
    })
    .unwrap_or_else(|| Err(MathErr { at: (0, len.max(1)), msg: "the math font is unavailable".into() }))?;
    Ok(MathLay { w: b.w, h: b.h, d: b.d, items: b.items, raster: RefCell::new(None) })
}

/// Layouts by (source, size, display) and failures by source (they do not depend on the size).
#[derive(Default)]
pub struct MathCache {
    map: HashMap<(String, u32, bool), Rc<MathLay>>,
    errs: HashMap<String, MathErr>,
    /// Font generation the layouts were made with (`\text` uses the UI font).
    font_gen: u32,
}

impl MathCache {
    /// The layout of `src` (cached), or why it cannot be typeset.
    pub fn get(
        &mut self,
        src: &str,
        size: f32,
        display: bool,
        m: &mut dyn TextMeasure,
    ) -> Result<Rc<MathLay>, MathErr> {
        let g = crate::fontcfg::generation();
        if g != self.font_gen {
            self.map.clear();
            self.font_gen = g;
        }
        if let Some(e) = self.errs.get(src) {
            return Err(e.clone());
        }
        let key = (src.to_string(), size.to_bits(), display);
        if let Some(l) = self.map.get(&key) {
            return Ok(l.clone());
        }
        match layout(src, size, display, m) {
            Ok(l) => {
                if self.map.len() >= 4096 {
                    self.map.clear();
                }
                let l = Rc::new(l);
                self.map.insert(key, l.clone());
                Ok(l)
            }
            Err(e) => {
                if self.errs.len() >= 1024 {
                    self.errs.clear();
                }
                self.errs.insert(src.to_string(), e.clone());
                Err(e)
            }
        }
    }

    /// The failure recorded for `src`, if it failed.
    pub fn err(&self, src: &str) -> Option<&MathErr> {
        self.errs.get(src)
    }
}

/// `text` cut into colored pieces with bytes `at` in `err` (a range past the end, as left by a
/// missing `}`, blames the last character).
pub fn err_pieces(text: &str, at: (usize, usize), base: Rgba, err: Rgba) -> Vec<(String, Rgba)> {
    let n = text.len();
    if n == 0 {
        return vec![];
    }
    let fix = |i: usize| {
        let mut i = i.min(n);
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let mut a = fix(at.0);
    let mut b = fix(at.1.max(at.0));
    if a >= n {
        a = fix(n - 1);
        b = n;
    }
    if b <= a {
        b = text[a..].chars().next().map_or(n, |c| a + c.len_utf8());
    }
    let mut v = Vec::with_capacity(3);
    if a > 0 {
        v.push((text[..a].to_string(), base));
    }
    v.push((text[a..b].to_string(), err));
    if b < n {
        v.push((text[b..].to_string(), base));
    }
    v
}

fn rasterize(f: &font::MFont, items: &[Item], s: f32) -> Option<Raster> {
    let (mut l, mut t, mut r, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for it in items {
        let (x0, y0, x1, y1) = match *it {
            Item::Glyph { x, y, gid, sx, sy } => {
                let (gx0, gy0, gx1, gy1) = f.bbox(gid);
                if gx1 <= gx0 {
                    continue;
                }
                (x + gx0 * sx, y - gy1 * sy, x + gx1 * sx, y - gy0 * sy)
            }
            Item::Rule { x, y, w, h } => (x, y, x + w, y + h + 1.0 / s),
            Item::Text { .. } => continue,
        };
        l = l.min(x0 * s);
        t = t.min(y0 * s);
        r = r.max(x1 * s);
        b = b.max(y1 * s);
    }
    if l > r || t > b {
        return None;
    }
    let x0 = l.floor() as i32 - 1;
    let y0 = t.floor() as i32 - 1;
    let w = (r.ceil() as i32 + 1 - x0).clamp(1, 8192) as u32;
    let h = (b.ceil() as i32 + 1 - y0).clamp(1, 8192) as u32;
    let mut mask = tiny_skia::Mask::new(w, h)?;
    let (fx, fy) = (x0 as f32, y0 as f32);
    for it in items {
        match *it {
            Item::Glyph { x, y, gid, sx, sy } => {
                let Some(p) = f.outline(gid) else { continue };
                let tf = tiny_skia::Transform::from_row(sx * s, 0.0, 0.0, -sy * s, x * s - fx, y * s - fy);
                mask.fill_path(&p, tiny_skia::FillRule::Winding, true, tf);
            }
            Item::Rule { x, y, w: rw, h: rh } => {
                // rules snap to whole device rows: a blurred hairline reads as grey
                let top = (y * s).round();
                let hh = (rh * s).round().max(1.0);
                if let Some(rect) = tiny_skia::Rect::from_xywh(x * s - fx, top - fy, (rw * s).max(0.5), hh) {
                    let p = tiny_skia::PathBuilder::from_rect(rect);
                    mask.fill_path(&p, tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
                }
            }
            Item::Text { .. } => {}
        }
    }
    Some(Raster { x0, y0, w: w as usize, h: h as usize, a: mask.take() })
}

/// Blend the coverage `r` in `col` with its origin at device pixel (`ox`, `oy`), inside `clip`
/// (device pixels).
fn blit(cv: &mut Canvas, r: &Raster, ox: i32, oy: i32, col: Rgba, clip: Rect) {
    let dst = Rect::new(ox + r.x0, oy + r.y0, r.w as i32, r.h as i32)
        .intersect(&clip)
        .intersect(&Rect::new(0, 0, cv.w, cv.h));
    if dst.w <= 0 || dst.h <= 0 {
        return;
    }
    let ca = col.3 as u32;
    for py in dst.y..dst.bottom() {
        let srow = (py - oy - r.y0) as usize * r.w;
        let drow = (py * cv.w) as usize;
        for px in dst.x..dst.right() {
            let m = r.a[srow + (px - ox - r.x0) as usize] as u32;
            if m == 0 {
                continue;
            }
            let a = m * ca / 255;
            let d = &mut cv.px[drow + px as usize];
            let mix = |dc: u32, sc: u8| (dc * (255 - a) + sc as u32 * a) / 255;
            *d = (mix((*d >> 16) & 0xff, col.0) << 16) | (mix((*d >> 8) & 0xff, col.1) << 8) | mix(*d & 0xff, col.2);
        }
    }
}

/// Paint a formula with its left edge at `x` and baseline at `base` (logical px), clipped.
pub fn paint(cv: &mut Canvas, tx: &mut TextCtx, lay: &MathLay, x: f32, base: f32, col: Rgba, clip: Rect) {
    let s = crate::ui::scale();
    let r = {
        let mut slot = lay.raster.borrow_mut();
        if slot.as_ref().is_none_or(|(k, _)| *k != s.to_bits()) {
            let r = font::with_font(|f| rasterize(f, &lay.items, s)).flatten().map(Rc::new);
            *slot = Some((s.to_bits(), r));
        }
        slot.as_ref().and_then(|(_, r)| r.clone())
    };
    if let Some(r) = r {
        let dclip = cv.to_dev(clip.intersect(&cv.bounds()));
        blit(cv, &r, (x * s).round() as i32, (base * s).round() as i32, col, dclip);
    }
    for it in &lay.items {
        if let Item::Text { x: ix, y, s: text, size, bold, bl } = it {
            let (tx0, ty0) = ((x + ix).round() as i32, (base + y - bl).round() as i32);
            tx.draw_ui_sized(cv, tx0, ty0, text, col, clip, *bold, *size);
        }
    }
}
