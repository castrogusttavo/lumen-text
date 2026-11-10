//! Markdown preview renderer (CPU canvas, no webview).
//!
//! Usage from the app, once per frame for the preview pane:
//!
//! ```ignore
//! view.set_text(&doc_text);                       // only when the source changed (cheap no-op otherwise)
//! view.draw(&mut cv, &mut tx, &theme, rect, scroll_px);
//! let h = view.content_height();                  // valid after draw (or `ensure_layout`)
//! ```
//!
//! Scroll sync uses 0-based source line numbers: [`MdView::y_for_source_line`] maps an editor line
//! to a content y (px from the top of the document, before scrolling) and
//! [`MdView::source_line_at_y`] is its inverse. Both interpolate inside long blocks (paragraphs,
//! code blocks, lists, tables) and across the blank lines between blocks, and are monotonic.
//!
//! The document is cut at blank lines into *segments* (see [`split`]); each segment is parsed and
//! laid out on its own and cached under the hash of its text, so a keystroke only re-parses and
//! re-lays out the segment it landed in (the other segments are reused as they are and placed by
//! y offset). Layouts are cached per width / font size.
//!
//! Supported: ATX/setext headings (H1-H6, hairline under H1/H2), paragraphs with word wrap,
//! **bold** (semibold face), *emphasis* (the UI family has no italic face: emphasis is slanted
//! synthetically by cosmic-text; if that ever fails it falls back to the text colour mixed toward
//! the accent), ~~strikethrough~~ (line through), `inline code` (monospace
//! chip), links (accent + underline, hit-testable with [`MdView::link_at`]), images (local
//! files are decoded: png, jpg, gif, webp, svg, see `mdimg`; remote URLs keep a bordered placeholder
//! with alt text and URL), bullet / ordered / nested / task lists (a click on a checkbox toggles the
//! source, see [`MdView::check_at`]), footnotes (numbered by first reference, collected at the end),
//! math (`$..$` as a chip, `$$..$$` as a block, monospace source text, no LaTeX engine), block quotes, thematic breaks, fenced and indented code blocks (monospace, never wrapped, clipped
//! horizontally, language label, TextMate syntax colors when a highlighter is given to
//! [`MdView::draw_with`]), GFM tables (content-sized columns, ellipsized cells, alignment),
//! hard and soft breaks, and `<img>` / `<br>` / `<a href>` found in raw HTML (other tags are
//! stripped; HTML comments are dropped).
//! Not supported: definition lists, metadata blocks, admonition quotes, true italic faces, inline images
//! in the middle of a text line (an image ends the paragraph text before it; consecutive images
//! share a row), animated GIF/WebP (first frame only), SVG text / CSS classes / filters / gradients
//! (a gradient paints its first stop color), LaTeX typesetting.
#![allow(dead_code)]

use crate::mdimg::{ImgStore, Tex};
use crate::theme::{Rgba, Theme};
use crate::ui::{Canvas, Rect, TextCtx, font_size, line_h};
use pulldown_cmark::{Alignment, BrokenLink, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

mod layout;
mod paint;
mod parse;
#[cfg(test)]
mod tests;

use layout::*;
use paint::*;
pub use parse::*;

// ---------------------------------------------------------------------------------------------
// constants

const PAD: i32 = 24;
const MAX_COL: i32 = 760;
/// UI font sizes in use: H1..H6 (0..5 -- index 4 is also the body, 5 also table cells), then small.
const SIZES: [f32; 7] = [28.0, 22.0, 18.0, 16.0, 14.0, 13.0, 11.0];
/// Line box height (px) per size index.
const LH: [i32; 7] = [38, 30, 25, 22, 22, 20, 16];
const BODY: usize = 4;
const SMALL: usize = 5;
const TINY: usize = 6;
const ROW_H: i32 = 28;
const CELL_PAD: i32 = 10;
const CHIP_PAD: i32 = 4;
const LIST_BULLET_INDENT: i32 = 24;
const QUOTE_INDENT: i32 = 16;

const F_B: u8 = 1;
const F_I: u8 = 2;
const F_S: u8 = 4;
const F_CODE: u8 = 8;
const F_LINK: u8 = 16;
const F_DIM: u8 = 32;
/// Inline math: drawn as a code chip, tinted with the accent.
const F_MATH: u8 = 64;

// ---------------------------------------------------------------------------------------------
// public view

/// Colored runs of every line of a code block: `(byte start, byte end, color, style flags)`.
pub type HlLines = Vec<Vec<(usize, usize, Rgba, u8)>>;
/// Syntax highlighter for fenced code: `(language, text)` -> runs per line (`None` = no grammar).
pub type HlFn<'a> = dyn FnMut(&str, &str) -> Option<HlLines> + 'a;

/// Code blocks longer than this stay plain (tokenizing them would stall a frame).
const HL_MAX_LINES: usize = 800;

/// Markdown preview state: the segments of the document with their parsed blocks and cached
/// layouts, and the flattened layout for the last width.
pub struct MdView {
    text: String,
    ctx: Ctx,
    order: Vec<Placed>,
    cache: KeyMap<SegData>,
    urls: Urls,
    imgs: ImgStore,
    /// Bumped when the directory images are resolved against changes.
    base_gen: u32,
    line_count: usize,
    version: u64,
    measure: Measure,
    lay: Option<Layout>,
    layouts: u32,
    seg_layouts: u32,
    parses: u32,
    last_layout_ms: f64,
    /// The footnote section (laid out again with every flatten; it is short).
    foot: Option<(SegLay, Placed)>,
    hl: HashMap<u64, Option<Rc<HlLines>>>,
    hl_theme: u64,
}

impl Default for MdView {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse one segment: its blocks (lines relative to the segment) and the footnote labels it refers to.
fn parse_seg(text: &str, ctx: &Ctx, urls: &mut Urls) -> (Vec<Block>, Vec<String>) {
    let idx = LineIndex::new(text);
    let mut opts =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_MATH;
    if !ctx.foot.is_empty() {
        opts |= Options::ENABLE_OLD_FOOTNOTES;
    }
    let mut b = Builder {
        idx: &idx,
        urls,
        known: &ctx.foot,
        refs: vec![],
        stack: vec![Cont::Root(vec![])],
        inl: None,
        strong: 0,
        em: 0,
        strike: 0,
        links: vec![],
        code: None,
        table: None,
        html: None,
        image: None,
    };
    let refs = &ctx.refs;
    let cb = move |bl: BrokenLink| {
        refs.get(&norm_label(&bl.reference)).map(|u| (CowStr::from(u.clone()), CowStr::from(String::new())))
    };
    for (ev, range) in Parser::new_with_broken_link_callback(text, opts, Some(cb)).into_offset_iter() {
        b.event(ev, range);
    }
    b.close_implicit();
    while b.stack.len() > 1 {
        // unbalanced input cannot happen with pulldown-cmark, but never lose content
        match b.stack.pop() {
            Some(Cont::Quote { blocks, l }) => b.push_block(Block { kind: Kind::Quote(blocks), line: l.0, end: l.1 }),
            Some(Cont::Foot { blocks, label, l }) => {
                b.push_block(Block { kind: Kind::FootDef { label, blocks }, line: l.0, end: l.1 })
            }
            _ => {}
        }
    }
    let Some(Cont::Root(blocks)) = b.stack.pop() else { unreachable!() };
    (blocks, b.refs)
}

/// Cut `lay` out of what a [`Lay`] collected.
fn seg_lay(key: SegKey, lay: Lay, y_end: i32, blocks: &[Block]) -> SegLay {
    let mut ops = lay.ops;
    ops.sort_by_key(|o| o.y0);
    let mut pmax = Vec::with_capacity(ops.len());
    let mut m = i32::MIN;
    for o in &ops {
        m = m.max(o.y1);
        pmax.push(m);
    }
    let shown: Vec<&Block> = blocks.iter().filter(|b| !matches!(b.kind, Kind::FootDef { .. })).collect();
    let mut links = lay.links;
    links.sort_by_key(|l| l.y);
    let max_link_h = links.iter().map(|l| l.h).max().unwrap_or(0);
    let mut checks = lay.checks;
    checks.sort_by_key(|c| c.y);
    SegLay {
        key,
        pmax,
        height: y_end,
        anchors: lay.anchors,
        links,
        checks,
        heads: lay.heads,
        max_link_h,
        first_before: shown.first().map_or(0, |b| before(b)),
        last_after: shown.last().map_or(0, |b| after(b)),
        empty: ops.is_empty() && y_end <= 0 && shown.is_empty(),
        ops,
    }
}

/// y inside a segment (relative) of its relative source line `ll`, from its anchors; `nl` / `h`
/// are the segment's line count and height (the edges of the blank stretches at either end).
fn y_in_seg(a: &[Anchor], nl: i64, h: i32, ll: i64) -> i32 {
    let i = a.partition_point(|p| p.l1 < ll);
    if let Some(p) = a.get(i).filter(|p| p.l0 <= ll) {
        let n = (p.l1 - p.l0 + 1) as f64;
        return p.y0 + (((ll - p.l0) as f64 / n) * (p.y1 - p.y0) as f64).round() as i32;
    }
    let (ql, qy) = i.checked_sub(1).map_or((-1, 0), |k| (a[k].l1, a[k].y1));
    let (pl, py) = a.get(i).map_or((nl, h), |p| (p.l0, p.y0));
    let m = (pl - ql - 1).max(1) as f64;
    qy + ((((ll - ql - 1) as f64) / m) * (py - qy) as f64).round() as i32
}

/// Source line (relative to the segment) at relative y `yl`: inverse of [`y_in_seg`].
fn line_in_seg(a: &[Anchor], nl: i64, h: i32, yl: i32) -> i64 {
    let i = a.partition_point(|p| p.y1 <= yl);
    if let Some(p) = a.get(i).filter(|p| p.y0 <= yl) {
        let n = (p.l1 - p.l0 + 1) as f64;
        let hh = (p.y1 - p.y0).max(1) as f64;
        return (p.l0 + (((yl - p.y0) as f64 + 0.5) / hh * n).floor() as i64).min(p.l1);
    }
    let (ql, qy) = i.checked_sub(1).map_or((-1, 0), |k| (a[k].l1, a[k].y1));
    let (pl, py) = a.get(i).map_or((nl, h), |p| (p.l0, p.y0));
    gap_line(ql, qy, pl, py, yl)
}

/// Line at `y` inside the blank stretch between line `ql` (ending at `qy`) and line `pl` (starting at `py`).
fn gap_line(ql: i64, qy: i32, pl: i64, py: i32, y: i32) -> i64 {
    let n = pl - ql - 1;
    if n <= 0 {
        return pl;
    }
    let span = (py - qy).max(1) as f64;
    let frac = (y - qy) as f64 / span;
    (ql + 1 + (frac * n as f64 + 0.5 / span * n as f64).floor() as i64).min(pl - 1)
}

impl MdView {
    pub fn new() -> Self {
        MdView {
            text: String::new(),
            ctx: Ctx::default(),
            order: vec![],
            cache: KeyMap::default(),
            urls: Urls::default(),
            imgs: ImgStore::new(),
            base_gen: 0,
            line_count: 1,
            version: 0,
            measure: Measure::default(),
            lay: None,
            layouts: 0,
            seg_layouts: 0,
            parses: 0,
            last_layout_ms: 0.0,
            foot: None,
            hl: HashMap::new(),
            hl_theme: 0,
        }
    }

    /// Replace the document. A no-op when the text is unchanged. Otherwise the text is cut into
    /// segments and only the ones never seen before are parsed (the layout is rebuilt lazily on
    /// the next `draw` / `ensure_layout`, and only for those segments).
    pub fn set_text(&mut self, text: &str) {
        if self.version > 0 && self.text == text {
            return;
        }
        self.text.clear();
        self.text.push_str(text);
        self.version += 1;
        let sp = split(&self.text);
        self.line_count = sp.lines;
        self.ctx = sp.ctx;
        self.order.clear();
        self.order.reserve(sp.segs.len());
        for s in &sp.segs {
            let key = fxhash(&self.text.as_bytes()[s.b0..s.b1], self.ctx.hash);
            if !self.cache.contains_key(&key) {
                let (blocks, refs) = parse_seg(&self.text[s.b0..s.b1], &self.ctx, &mut self.urls);
                self.cache.insert(key, SegData { blocks, refs, lay: None, used: 0 });
                self.parses += 1;
            }
            self.order.push(Placed { key, l0: s.l0, nl: s.nl, b0: s.b0, y0: 0, y1: 0 });
        }
        // Segments that left the document (one per keystroke) are dropped in batches.
        if self.cache.len() > self.order.len() + 48 {
            let live: KeySet = self.order.iter().map(|p| p.key).collect();
            self.cache.retain(|k, _| live.contains(k));
        }
        self.lay = None;
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of full layouts done so far (diagnostics / tests).
    pub fn layout_count(&self) -> u32 {
        self.layouts
    }

    /// Segments parsed so far / segments laid out so far (an edit should add one of each).
    pub fn work_counts(&self) -> (u32, u32) {
        (self.parses, self.seg_layouts)
    }

    /// Wall time of the last layout in milliseconds.
    pub fn last_layout_ms(&self) -> f64 {
        self.last_layout_ms
    }

    /// Directory relative image paths are resolved against (the document's folder).
    pub fn set_base_dir(&mut self, dir: Option<&Path>) {
        if self.imgs.set_base(dir) {
            self.base_gen += 1;
            self.lay = None;
        }
    }

    /// Images are still being decoded: keep drawing so they show up.
    pub fn pending(&self) -> bool {
        self.imgs.pending()
    }

    /// When an animated image drawn in the last pass needs its next frame (None = nothing moves).
    pub fn anim_due(&self) -> Option<std::time::Instant> {
        self.imgs.anim_due()
    }

    /// Image files edited on disk since they were laid out: forget them (size and pixels) and lay
    /// the document out again. True when anything changed (a redraw is due).
    pub fn refresh_images(&mut self) -> bool {
        if !self.imgs.changed_on_disk() {
            return false;
        }
        self.base_gen += 1;
        self.lay = None;
        true
    }

    /// Footnote numbers (by first reference, then the definitions nobody references) and the
    /// definitions in that order.
    fn footnotes(&self) -> (HashMap<String, usize>, Vec<(String, Vec<Block>)>) {
        let mut defs: Vec<(String, &Vec<Block>)> = vec![];
        let mut num: HashMap<String, usize> = HashMap::new();
        if !self.ctx.foot.is_empty() {
            for p in &self.order {
                let Some(sd) = self.cache.get(&p.key) else { continue };
                for l in &sd.refs {
                    let n = num.len() + 1;
                    num.entry(l.clone()).or_insert(n);
                }
                for b in &sd.blocks {
                    if let Kind::FootDef { label, blocks } = &b.kind
                        && !defs.iter().any(|(l, _)| l == label)
                    {
                        defs.push((label.clone(), blocks));
                    }
                }
            }
            for (l, _) in &defs {
                let n = num.len() + 1;
                num.entry(l.clone()).or_insert(n);
            }
        }
        let mut out: Vec<(String, Vec<Block>)> = defs.into_iter().map(|(l, b)| (l, b.clone())).collect();
        out.sort_by_key(|(l, _)| num.get(l).copied().unwrap_or(usize::MAX));
        (num, out)
    }

    /// Lay the document out for a pane `width` px wide (no-op when the layout for the current
    /// text / width / code font size is already there). `draw` calls this itself. Only segments
    /// whose cached layout does not match are laid out again.
    pub fn ensure_layout(&mut self, tx: &mut TextCtx, width: i32) {
        let key = (width, font_size().to_bits(), tx.cell_w.to_bits(), self.version, self.base_gen);
        if self.lay.as_ref().is_some_and(|l| l.key == key) {
            return;
        }
        let t0 = std::time::Instant::now();
        let col_w = (width - 2 * PAD).clamp(40, MAX_COL);
        let col_x = ((width - col_w) / 2).max(PAD.min(width / 2));
        let (fnum, defs) = self.footnotes();
        let mut segs: Vec<SegRef> = Vec::with_capacity(self.order.len());
        let mut y = PAD;
        let mut prev_after: Option<i32> = None;
        let base_gen = self.base_gen;
        for (ix, p) in self.order.iter_mut().enumerate() {
            let Some(sd) = self.cache.get_mut(&p.key) else { continue };
            let mut fh = 0u64;
            for l in &sd.refs {
                fh = fh.wrapping_mul(31).wrapping_add(fnum.get(l).copied().unwrap_or(0) as u64 + 1);
            }
            let skey: SegKey = (width, key.1, key.2, fh, base_gen);
            if sd.lay.as_ref().is_none_or(|l| l.key != skey) {
                let mut lay = Lay {
                    tx: &mut *tx,
                    m: &mut self.measure,
                    urls: &mut self.urls,
                    imgs: &mut self.imgs,
                    fnum: &fnum,
                    ops: vec![],
                    anchors: vec![],
                    links: vec![],
                    checks: vec![],
                    heads: vec![],
                };
                let y_end = lay.blocks(&sd.blocks, col_x, col_w, 0, St { dim: false, depth: 0, tight: None });
                sd.lay = Some(seg_lay(skey, lay, y_end, &sd.blocks));
                self.seg_layouts += 1;
            }
            let Some(l) = sd.lay.as_ref() else { continue };
            if l.empty {
                p.y0 = y;
                p.y1 = y;
                continue;
            }
            if let Some(a) = prev_after {
                y += a.max(l.first_before);
            }
            p.y0 = y;
            p.y1 = y + l.height;
            y = p.y1;
            prev_after = Some(l.last_after);
            segs.push(SegRef { ix, l0: p.l0 as i64, nl: p.nl as i64, y0: p.y0, y1: p.y1 });
        }
        // Footnotes go after the last segment.
        self.foot = None;
        if !defs.is_empty() {
            let gap = prev_after.unwrap_or(0).max(24);
            let top = y + gap;
            let list: Vec<(usize, &str, &[Block])> =
                defs.iter().map(|(l, b)| (fnum.get(l).copied().unwrap_or(0), l.as_str(), b.as_slice())).collect();
            let mut lay = Lay {
                tx: &mut *tx,
                m: &mut self.measure,
                urls: &mut self.urls,
                imgs: &mut self.imgs,
                fnum: &fnum,
                ops: vec![],
                anchors: vec![],
                links: vec![],
                checks: vec![],
                heads: vec![],
            };
            let y_end = lay.foot_section(&list, col_x, col_w, 0);
            let sl = seg_lay((width, key.1, key.2, 0, base_gen), lay, y_end, &[]);
            y = top + sl.height;
            self.foot = Some((sl, Placed { key: 0, l0: 0, nl: 0, b0: 0, y0: top, y1: y }));
        }
        let height = y.max(PAD) + PAD;
        self.lay = Some(Layout { key, height, segs, col_x, col_w });
        self.layouts += 1;
        self.last_layout_ms = t0.elapsed().as_secs_f64() * 1000.0;
    }

    /// Paint the preview into `rect` (clipped to it): background, then the part of the document
    /// starting at content y = `scroll_px`. Code blocks are plain monospace; see [`draw_with`](Self::draw_with).
    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect, scroll_px: f64) {
        self.draw_with(cv, tx, theme, rect, scroll_px, None);
    }

    /// [`draw`](Self::draw) with a syntax highlighter for fenced code blocks. It is asked lazily,
    /// once per block that comes into view, and the result is cached per theme.
    pub fn draw_with(
        &mut self,
        cv: &mut Canvas,
        tx: &mut TextCtx,
        theme: &Theme,
        rect: Rect,
        scroll_px: f64,
        mut hl: Option<&mut HlFn>,
    ) {
        let pal = Pal::new(theme);
        let rect = rect.intersect(&cv.bounds());
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        cv.fill_clip(rect, pal.bg, rect);
        self.imgs.poll();
        self.imgs.begin_frame();
        self.ensure_layout(tx, rect.w);
        let theme_key = fxhash(
            theme.name.as_bytes(),
            ((theme.background.0 as u64) << 40)
                | ((theme.background.1 as u64) << 32)
                | ((theme.foreground.0 as u64) << 24)
                | ((theme.foreground.1 as u64) << 16)
                | theme.foreground.2 as u64,
        );
        if self.hl_theme != theme_key {
            self.hl.clear();
            self.hl_theme = theme_key;
        }
        let MdView { order, cache, urls: _, imgs, lay, hl: hl_cache, foot, .. } = self;
        let Some(lay) = lay.as_ref() else { return };
        let sc = scroll_px.floor() as i32;
        let (top, bot) = (sc, sc + rect.h);
        let clip = rect;
        let ox = rect.x;
        let first = order.partition_point(|p| p.y1 <= top);
        let mut segs: Vec<(&SegLay, i32)> = vec![];
        for p in &order[first..] {
            if p.y0 >= bot {
                break;
            }
            if let Some(l) = cache.get(&p.key).and_then(|d| d.lay.as_ref()) {
                segs.push((l, p.y0));
            }
        }
        if let Some((l, p)) = foot.as_ref()
            && p.y0 < bot
            && p.y1 > top
        {
            segs.push((l, p.y0));
        }
        let _ = lay;
        for (sl, base) in segs {
            let (ltop, lbot) = (top - base, bot - base);
            let start = sl.pmax.partition_point(|&m| m <= ltop);
            let oy = rect.y - sc + base;
            let tr = |r: Rect| Rect::new(r.x + ox, r.y + oy, r.w, r.h);
            for d in &sl.ops[start..] {
                if d.y0 >= lbot {
                    break;
                }
                if d.y1 <= ltop {
                    continue;
                }
                match &d.op {
                    Op::Text { x, w, text, flags, si, lh } => {
                        let size = SIZES[*si as usize];
                        let (sx, sy) = (x + ox, d.y0 + oy);
                        let base_col = if flags & F_DIM != 0 { pal.dim } else { pal.fg };
                        let mut color = base_col;
                        // Emphasis is slanted synthetically; the accent tint stays only as a fallback
                        // when the slant does not render.
                        let slant = flags & F_I != 0 && tx.italic_works();
                        if flags & F_I != 0 && !slant {
                            color = color.mix(pal.accent, 0.45);
                        }
                        if flags & F_LINK != 0 {
                            color = pal.accent;
                        }
                        if flags & F_CODE != 0 {
                            let ch = (*lh - 2).min(line_h() as i32 + 2);
                            let chip = Rect::new(sx, sy + (*lh - ch) / 2, *w, ch);
                            let fill = if flags & F_MATH != 0 { pal.math_chip(color) } else { pal.input };
                            round_clip(cv, chip, 4, fill, clip);
                            let cl = line_h() as i32;
                            tx.draw_line(cv, sx + CHIP_PAD, sy + (*lh - cl) / 2, text, color, clip);
                        } else {
                            let off = (*lh - (size * 1.4).round() as i32) / 2;
                            tx.draw_ui_sized_i(cv, sx, sy + off, text, color, clip, flags & F_B != 0, slant, size);
                        }
                        if flags & F_S != 0 {
                            cv.fill_clip(Rect::new(sx, sy + *lh / 2 + 1, *w, 1), color, clip);
                        }
                        if flags & F_LINK != 0 && flags & F_CODE == 0 {
                            let uy = sy + *lh / 2 + (size * 0.4).round() as i32 + 2;
                            cv.fill_clip(Rect::new(sx, uy, *w, 1), color.with_alpha(170), clip);
                        }
                    }
                    Op::Fill { r, col, rad } => round_clip(cv, tr(*r), *rad, pal.get(*col), clip),
                    Op::Card { r, rad, fill, border } => {
                        card_clip(cv, tr(*r), *rad, pal.get(*fill), pal.get(*border), clip)
                    }
                    Op::Stroke { r, col } => stroke_clip(cv, tr(*r), pal.get(*col), clip),
                    Op::Bullet { cx, cy, level } => {
                        let (cx, cy) = (cx + ox, cy + oy);
                        let c = pal.fg.mix(pal.bg, 0.15);
                        match level {
                            0 => round_clip(cv, Rect::new(cx - 3, cy - 3, 6, 6), 3, c, clip),
                            1 => {
                                round_clip(cv, Rect::new(cx - 3, cy - 3, 6, 6), 3, c, clip);
                                round_clip(cv, Rect::new(cx - 2, cy - 2, 4, 4), 2, pal.bg, clip);
                            }
                            _ => cv.fill_clip(Rect::new(cx - 2, cy - 2, 5, 5), c, clip),
                        }
                    }
                    Op::Check { x, y, done } => {
                        let r = Rect::new(x + ox, y + oy, 14, 14);
                        if *done {
                            round_clip(cv, r, 3, pal.accent, clip);
                            check_mark(cv, r.x, r.y, pal.bg, clip);
                        } else {
                            card_clip(cv, r, 3, pal.bg, pal.dim, clip);
                        }
                    }
                    Op::Image { r, id } => {
                        let sr = tr(*r);
                        let dev = cv.to_dev(sr);
                        match imgs.texture(*id, dev.w.max(1) as u32, dev.h.max(1) as u32) {
                            Tex::Ready(p) => {
                                let dclip = cv.to_dev(clip);
                                crate::mdimg::blit(cv, dev.x, dev.y, &p, dclip);
                            }
                            Tex::Badged(p) => {
                                let dclip = cv.to_dev(clip);
                                crate::mdimg::blit(cv, dev.x, dev.y, &p, dclip);
                                // First frame only: say so instead of looking like a broken animation.
                                let label = "animated";
                                let lw = tx.ui_width_sized(label, false, SIZES[TINY]);
                                let pill = Rect::new(sr.x + 6, sr.y + 6, lw + 12, 18).intersect(&sr);
                                round_clip(cv, pill, 9, pal.bg.with_alpha(0xd0), clip);
                                tx.draw_ui_sized(
                                    cv,
                                    pill.x + 6,
                                    pill.y + 2,
                                    label,
                                    pal.fg,
                                    clip.intersect(&pill),
                                    false,
                                    SIZES[TINY],
                                );
                            }
                            Tex::Pending => card_clip(cv, sr, 4, pal.widget, pal.border, clip),
                            Tex::Failed => {
                                card_clip(cv, sr, 4, pal.widget, pal.border, clip);
                                tx.draw_ui_sized(
                                    cv,
                                    sr.x + 8,
                                    sr.y + 6,
                                    "image unavailable",
                                    pal.dim,
                                    clip.intersect(&sr),
                                    false,
                                    SIZES[TINY],
                                );
                            }
                        }
                    }
                    Op::Code { r, lang, lines, lh, top: ctop, key, math } => {
                        let sr = tr(*r);
                        let fill = if *math { pal.math_block() } else { pal.widget };
                        card_clip(cv, sr, 6, fill, pal.border, clip);
                        if *math {
                            round_clip(cv, Rect::new(sr.x + 1, sr.y + 6, 3, sr.h - 12), 1, pal.accent, clip);
                        }
                        if !lang.is_empty() {
                            let lw = tx.ui_width_sized(lang, false, SIZES[TINY]);
                            tx.draw_ui_sized(
                                cv,
                                sr.right() - lw - 10,
                                sr.y + 3,
                                lang,
                                pal.dim,
                                clip.intersect(&sr),
                                false,
                                SIZES[TINY],
                            );
                        }
                        let inner = Rect::new(sr.x + 1, sr.y + 1, sr.w - 2, sr.h - 2).intersect(&clip);
                        let first = ((clip.y - sr.y - ctop) / lh).max(0) as usize;
                        let max_chars = ((sr.w as f32) / tx.cell_w.max(1.0)) as usize + 2;
                        let colored = if !*math && !lang.is_empty() && lines.len() <= HL_MAX_LINES {
                            hl_lines(hl_cache, *key, lang, lines, &mut hl, fill, pal.bg)
                        } else {
                            None
                        };
                        for (i, line) in lines.iter().enumerate().skip(first) {
                            let ly = sr.y + ctop + i as i32 * lh;
                            if ly >= clip.bottom() {
                                break;
                            }
                            if line.is_empty() {
                                continue;
                            }
                            let shown: String = if line.len() > max_chars {
                                line.chars().take(max_chars).collect()
                            } else {
                                line.clone()
                            };
                            let sw = code_w(tx, &shown);
                            let lx = if *math { sr.x + ((sr.w as f32 - sw) / 2.0).max(14.0) as i32 } else { sr.x + 14 };
                            match colored.as_ref().and_then(|c| c.get(i)) {
                                Some(runs) if !runs.is_empty() => {
                                    let pieces = pieces_of(&shown, runs, pal.fg);
                                    tx.draw_styled(cv, lx, ly, &pieces, inner);
                                }
                                _ => tx.draw_line(cv, lx, ly, &shown, pal.fg, inner),
                            }
                        }
                    }
                }
            }
        }
        imgs.trim();
    }

    /// Total height of the laid-out document in px (0 before the first layout).
    pub fn content_height(&self) -> i32 {
        self.lay.as_ref().map_or(0, |l| l.height)
    }

    /// Content y (px from the document top) of 0-based source `line`. Interpolated inside blocks and
    /// across the blank lines between them; lines past the end give the content height.
    /// `None` until the first layout.
    pub fn y_for_source_line(&self, line: usize) -> Option<i32> {
        let lay = self.lay.as_ref()?;
        let n = self.line_count as i64;
        let line = line as i64;
        if line >= n {
            return Some(lay.height);
        }
        let i = lay.segs.partition_point(|s| s.l0 <= line);
        // The edges of the blank stretch `line` falls in (a segment's last line and the next one's first).
        let (ql, qy) = match i.checked_sub(1).map(|k| lay.segs[k]) {
            Some(s) if line < s.l0 + s.nl => {
                let a = &self.seg_lay_of(&s)?.anchors;
                return Some(s.y0 + y_in_seg(a, s.nl, s.y1 - s.y0, line - s.l0));
            }
            Some(s) => (s.l0 + s.nl - 1, s.y1),
            None => (-1, 0),
        };
        let (pl, py) = lay.segs.get(i).map_or((n, lay.height), |t| (t.l0, t.y0));
        let m = (pl - ql - 1).max(1) as f64;
        let frac = (line - ql - 1) as f64 / m;
        Some(qy + (frac * (py - qy) as f64).round() as i32)
    }

    fn seg_lay_of(&self, s: &SegRef) -> Option<&SegLay> {
        self.cache.get(&self.order.get(s.ix)?.key)?.lay.as_ref()
    }

    /// 0-based source line shown at content y (inverse of [`y_for_source_line`](Self::y_for_source_line)).
    pub fn source_line_at_y(&self, y: i32) -> usize {
        let Some(lay) = self.lay.as_ref() else { return 0 };
        let n = self.line_count as i64;
        let last = (n - 1).max(0);
        if y >= lay.height {
            return last as usize;
        }
        let y = y.max(0);
        let i = lay.segs.partition_point(|s| s.y1 <= y);
        let (ql, qy) = match i.checked_sub(1).map(|k| lay.segs[k]) {
            Some(s) => (s.l0 + s.nl - 1, s.y1),
            None => (-1, 0),
        };
        let line = match lay.segs.get(i) {
            Some(s) if s.y0 <= y => {
                let Some(l) = self.seg_lay_of(s) else { return last as usize };
                s.l0 + line_in_seg(&l.anchors, s.nl, s.y1 - s.y0, y - s.y0)
            }
            next => {
                let (pl, py) = next.map_or((n, lay.height), |t| (t.l0, t.y0));
                gap_line(ql, qy, pl, py, y)
            }
        };
        line.clamp(0, last) as usize
    }

    /// URL of the link under the pointer (`x`, `y` in canvas coordinates; `rect` and `scroll_px` as
    /// passed to the last `draw`). Images wrapped in a link (badges) are links too.
    pub fn link_at(&self, x: f64, y: f64, rect: Rect, scroll_px: f64) -> Option<String> {
        if !rect.contains(x, y) {
            return None;
        }
        let lay = self.lay.as_ref()?;
        let cx = x - rect.x as f64;
        let cy = y - rect.y as f64 + scroll_px.floor();
        let (sl, base) = self.seg_at_y(lay, cy as i32)?;
        let cy = cy - base as f64;
        let from = sl.links.partition_point(|l| ((l.y + sl.max_link_h) as f64) <= cy);
        for l in &sl.links[from..] {
            if l.y as f64 > cy {
                break;
            }
            if cx >= l.x as f64 && cx < (l.x + l.w) as f64 && cy < (l.y + l.h) as f64 {
                return self.urls.get(l.link).cloned();
            }
        }
        None
    }

    /// The laid-out segment (or the footnote section) covering content y, and its top.
    fn seg_at_y(&self, lay: &Layout, y: i32) -> Option<(&SegLay, i32)> {
        if let Some((l, p)) = self.foot.as_ref()
            && y >= p.y0
            && y < p.y1
        {
            return Some((l, p.y0));
        }
        let s = lay.segs.get(lay.segs.partition_point(|s| s.y1 <= y)).filter(|s| s.y0 <= y)?;
        Some((self.seg_lay_of(s)?, s.y0))
    }

    /// The task checkbox under the pointer: the byte offset in the document text of the `[` of its
    /// `[ ]` / `[x]` marker (so the caller can flip the character at `offset + 1`).
    pub fn check_at(&self, x: f64, y: f64, rect: Rect, scroll_px: f64) -> Option<usize> {
        if !rect.contains(x, y) {
            return None;
        }
        let lay = self.lay.as_ref()?;
        let cx = x - rect.x as f64;
        let cy = y - rect.y as f64 + scroll_px.floor();
        let s = *lay.segs.get(lay.segs.partition_point(|s| s.y1 <= cy as i32)).filter(|s| s.y0 <= cy as i32)?;
        let sl = self.seg_lay_of(&s)?;
        let cy = cy - s.y0 as f64;
        let b0 = self.order.get(s.ix)?.b0;
        sl.checks
            .iter()
            .find(|c| cx >= c.x as f64 && cx < (c.x + c.w) as f64 && cy >= c.y as f64 && cy < (c.y + c.h) as f64)
            .map(|c| c.off + b0)
    }

    /// `(github-style slug, content y)` of every heading in document order (duplicates get
    /// `-1`, `-2`, ... suffixes), for `#section` links. Footnote definitions are `fn-<label>`.
    /// Built on demand (a link click), not on every layout.
    pub fn heading_anchors(&self) -> Vec<(String, i32)> {
        let Some(lay) = self.lay.as_ref() else { return vec![] };
        let mut seen = HashMap::new();
        let mut out = vec![];
        let mut add = |l: &SegLay, base: i32| {
            for (n, hy, exact) in &l.heads {
                out.push((if *exact { n.clone() } else { dedupe_slug(n.clone(), &mut seen) }, hy + base));
            }
        };
        for s in &lay.segs {
            if let Some(l) = self.seg_lay_of(s)
                && !l.heads.is_empty()
            {
                add(l, s.y0);
            }
        }
        if let Some((l, p)) = self.foot.as_ref() {
            add(l, p.y0);
        }
        out
    }

    /// Blocks of the whole document with absolute source lines (tests).
    #[cfg(test)]
    fn blocks(&self) -> Vec<Block> {
        fn shift(b: &Block, d: usize) -> Block {
            let kind = match &b.kind {
                Kind::Quote(v) => Kind::Quote(v.iter().map(|x| shift(x, d)).collect()),
                Kind::List { start, items, loose } => Kind::List {
                    start: *start,
                    loose: *loose,
                    items: items
                        .iter()
                        .map(|i| LItem {
                            task: i.task,
                            task_off: i.task_off,
                            blocks: i.blocks.iter().map(|x| shift(x, d)).collect(),
                        })
                        .collect(),
                },
                Kind::FootDef { label, blocks } => {
                    Kind::FootDef { label: label.clone(), blocks: blocks.iter().map(|x| shift(x, d)).collect() }
                }
                k => k.clone(),
            };
            Block { kind, line: b.line + d, end: b.end + d }
        }
        self.order
            .iter()
            .filter_map(|p| self.cache.get(&p.key).map(|d| (p, d)))
            .flat_map(|(p, d)| d.blocks.iter().map(move |b| shift(b, p.l0)))
            .collect()
    }

    /// Link boxes with absolute y, in document order (tests).
    #[cfg(test)]
    fn all_links(&self) -> Vec<LinkHit> {
        let Some(lay) = self.lay.as_ref() else { return vec![] };
        let mut v: Vec<LinkHit> = lay
            .segs
            .iter()
            .filter_map(|s| self.seg_lay_of(s).map(|l| (s, l)))
            .flat_map(|(s, l)| l.links.iter().map(move |k| LinkHit { y: k.y + s.y0, ..*k }))
            .collect();
        if let Some((l, p)) = self.foot.as_ref() {
            v.extend(l.links.iter().map(|k| LinkHit { y: k.y + p.y0, ..*k }));
        }
        v
    }

    /// Checkboxes with absolute y and source offsets (tests).
    #[cfg(test)]
    fn all_checks(&self) -> Vec<CheckHit> {
        let Some(lay) = self.lay.as_ref() else { return vec![] };
        lay.segs
            .iter()
            .filter_map(|s| self.seg_lay_of(s).map(|l| (s, l)))
            .flat_map(|(s, l)| {
                l.checks.iter().map(move |c| CheckHit { y: c.y + s.y0, off: c.off + self.order[s.ix].b0, ..*c })
            })
            .collect()
    }

    /// Draw ops of the whole document, in layout order (tests).
    #[cfg(test)]
    fn ops(&self) -> Vec<&DrawOp> {
        self.order
            .iter()
            .filter_map(|p| self.cache.get(&p.key))
            .filter_map(|d| d.lay.as_ref())
            .chain(self.foot.iter().map(|(l, _)| l))
            .flat_map(|l| l.ops.iter())
            .collect()
    }
}
