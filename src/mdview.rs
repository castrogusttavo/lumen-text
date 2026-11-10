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
// slugs

/// GitHub-style heading anchor: lowercase, spaces become `-`, punctuation dropped, Unicode
/// letters/digits kept (`-` and `_` are kept too).
pub fn slugify(heading: &str) -> String {
    let mut out = String::with_capacity(heading.len());
    for c in heading.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.extend(c.to_lowercase());
        } else if c.is_whitespace() {
            out.push('-');
        }
    }
    out
}

/// [`slugify`] with GitHub's duplicate handling: the first `foo` stays `foo`, the next ones become
/// `foo-1`, `foo-2`, ... `seen` carries the state across the headings of one document.
pub fn slugify_unique(heading: &str, seen: &mut HashMap<String, usize>) -> String {
    dedupe_slug(slugify(heading), seen)
}

/// The duplicate handling of [`slugify_unique`] for an already slugified `base`.
fn dedupe_slug(base: String, seen: &mut HashMap<String, usize>) -> String {
    let Some(count) = seen.get(&base).copied() else {
        seen.insert(base.clone(), 1);
        return base;
    };
    let mut n = count;
    let cand = loop {
        let c = format!("{base}-{n}");
        n += 1;
        if !seen.contains_key(&c) {
            break c;
        }
    };
    seen.insert(base, n);
    seen.insert(cand.clone(), 1);
    cand
}

// ---------------------------------------------------------------------------------------------
// document model

#[derive(Clone, Debug, PartialEq)]
enum Inl {
    /// `link` is 0 for none, else index + 1 into `MdView::urls`.
    Text {
        text: String,
        flags: u8,
        link: u32,
    },
    Soft,
    Hard,
    /// `[^label]`: shown as `[n]`, `n` being the order of first reference in the document.
    FootRef(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Img {
    alt: String,
    url: String,
    link: u32,
    /// `width` / `height` attributes of a raw HTML `<img>`, in px.
    size: (Option<i32>, Option<i32>),
}

#[derive(Clone, Debug)]
struct LItem {
    task: Option<bool>,
    /// Byte offset (inside the segment) of the `[` of the task marker.
    task_off: usize,
    blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
enum Kind {
    Heading {
        level: u8,
        inl: Vec<Inl>,
        slug: String,
    },
    Para(Vec<Inl>),
    Code {
        lang: String,
        text: String,
    },
    Rule,
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<LItem>,
        loose: bool,
    },
    Table {
        aligns: Vec<Alignment>,
        head: Vec<Vec<Inl>>,
        rows: Vec<Vec<Vec<Inl>>>,
    },
    Images(Vec<Img>),
    /// `$$ ... $$`.
    Math(String),
    /// `[^label]: ...`, moved to the end of the document when laid out.
    FootDef {
        label: String,
        blocks: Vec<Block>,
    },
}

/// A block with its 0-based source line range (inclusive).
#[derive(Clone, Debug)]
struct Block {
    kind: Kind,
    line: usize,
    end: usize,
}

// ---------------------------------------------------------------------------------------------
// shared tables

/// Link targets of the whole document, interned so that the ids stored in cached segments stay
/// valid when other segments change. Ids are 1-based (0 = no link).
#[derive(Default)]
struct Urls {
    list: Vec<String>,
    map: HashMap<String, u32>,
}

impl Urls {
    fn id(&mut self, url: &str) -> u32 {
        if let Some(&i) = self.map.get(url) {
            return i;
        }
        self.list.push(url.to_string());
        let i = self.list.len() as u32;
        self.map.insert(url.to_string(), i);
        i
    }
    fn get(&self, id: u32) -> Option<&String> {
        self.list.get((id as usize).checked_sub(1)?)
    }
}

/// 64-bit hash of `bytes` (8 bytes per step), mixed with `seed`.
fn fxhash(bytes: &[u8], seed: u64) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = (seed ^ bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let (chunks, rest) = bytes.as_chunks::<8>();
    for c in chunks {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(*c)).wrapping_mul(K);
    }
    for &b in rest {
        h = (h.rotate_left(5) ^ b as u64).wrapping_mul(K);
    }
    (h ^ (h >> 32)).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Document-wide parse context: reference-style link targets and footnote labels, found by [`split`].
/// A segment is parsed with these, so its cache key includes their hash.
#[derive(Default)]
struct Ctx {
    refs: HashMap<String, String>,
    foot: HashSet<String>,
    hash: u64,
}

fn norm_label(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

// ---------------------------------------------------------------------------------------------
// segmenting

/// A run of source lines that parses on its own: `[b0, b1)` bytes, `nl` lines from line `l0`.
#[derive(Clone, Copy, Debug)]
struct SegSpan {
    b0: usize,
    b1: usize,
    l0: usize,
    nl: usize,
}

struct Split {
    segs: Vec<SegSpan>,
    ctx: Ctx,
    /// Number of lines (`\n` count + 1, like [`LineIndex`]).
    lines: usize,
}

/// `(columns, bytes)` of leading blanks (a tab goes to the next multiple of 4).
fn indent_of(line: &str) -> (usize, usize) {
    let (mut cols, mut n) = (0, 0);
    for b in line.bytes() {
        match b {
            b' ' => cols += 1,
            b'\t' => cols = (cols / 4 + 1) * 4,
            _ => break,
        }
        n += 1;
    }
    (cols, n)
}

fn is_list_marker(rest: &str) -> bool {
    let b = rest.as_bytes();
    match b.first() {
        Some(b'-' | b'+' | b'*') => b.get(1).is_none_or(|c| *c == b' ' || *c == b'\t'),
        Some(c) if c.is_ascii_digit() => {
            let d = b.iter().take_while(|c| c.is_ascii_digit()).count();
            d <= 9 && matches!(b.get(d), Some(b'.' | b')')) && b.get(d + 1).is_none_or(|c| *c == b' ' || *c == b'\t')
        }
        _ => false,
    }
}

/// Opening code fence: (char, run length) of ```` ``` ```` / `~~~` (info string of a backtick fence
/// cannot hold a backtick).
fn fence_open(rest: &str) -> Option<(u8, usize)> {
    let c = *rest.as_bytes().first().filter(|c| **c == b'`' || **c == b'~')?;
    let n = rest.bytes().take_while(|b| *b == c).count();
    (n >= 3 && !(c == b'`' && rest[n..].contains('`'))).then_some((c, n))
}

fn fence_close(line: &str, c: u8, n: usize) -> bool {
    let t = line.trim_start();
    let k = t.bytes().take_while(|b| *b == c).count();
    k >= n && t[k..].trim().is_empty()
}

/// Cut `text` into segments at blank lines outside code fences and raw HTML blocks that must stay
/// together. Merging is always safe (a merged segment parses exactly as the document would), so
/// the rules lean towards it: a line indented two columns or more continues the segment, and a
/// list marker continues a segment that starts with a list.
fn split(text: &str) -> Split {
    struct Cur {
        b0: usize,
        l0: usize,
        b1: usize,
        l1: usize,
        list: bool,
    }
    let mut segs: Vec<SegSpan> = vec![];
    let mut ctx = Ctx::default();
    let mut cur: Option<Cur> = None;
    let mut blank_pending = false;
    let mut fence: Option<(u8, usize)> = None;
    let mut raw: Option<&'static str> = None;
    let (mut off, mut ln) = (0usize, 0usize);
    let close =
        |c: Cur, segs: &mut Vec<SegSpan>| segs.push(SegSpan { b0: c.b0, b1: c.b1, l0: c.l0, nl: c.l1 - c.l0 + 1 });
    for line in text.split_inclusive('\n') {
        let len = line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        let blank = content.bytes().all(|b| b == b' ' || b == b'\t');
        if let Some((c, n)) = fence {
            if fence_close(content, c, n) {
                fence = None;
            }
            if let Some(c) = cur.as_mut() {
                (c.b1, c.l1) = (off + len, ln);
            }
        } else if let Some(end) = raw {
            if content.to_ascii_lowercase().contains(end) {
                raw = None;
            }
            if let Some(c) = cur.as_mut() {
                (c.b1, c.l1) = (off + len, ln);
            }
        } else if blank {
            blank_pending = cur.is_some();
        } else {
            // Fast path: a plain line that continues the current segment (most lines).
            if !blank_pending && let Some(c) = cur.as_mut() {
                let first = content.bytes().find(|b| *b != b' ' && *b != b'\t');
                if !matches!(first, Some(b'`' | b'~' | b'<' | b'[')) {
                    (c.b1, c.l1) = (off + len, ln);
                    off += len;
                    ln += 1;
                    continue;
                }
            }
            let (cols, nb) = indent_of(content);
            let rest = &content[nb..];
            let merge = cols >= 2 || cur.as_ref().is_some_and(|c| c.list && is_list_marker(rest));
            // `[^a]: ..` directly after another definition is a new one (the old footnote syntax
            // would take it for a continuation line).
            let foot_def = cols <= 3 && rest.starts_with("[^") && rest.contains("]:");
            if cur.is_none() || (blank_pending && !merge) || foot_def {
                if let Some(c) = cur.take() {
                    close(c, &mut segs);
                }
                cur = Some(Cur { b0: off, l0: ln, b1: off, l1: ln, list: cols <= 3 && is_list_marker(rest) });
            }
            blank_pending = false;
            if let Some(c) = cur.as_mut() {
                (c.b1, c.l1) = (off + len, ln);
            }
            if let Some(f) = fence_open(rest) {
                fence = Some(f);
            } else if cols <= 3 && rest.starts_with('<') {
                let l = rest.to_ascii_lowercase();
                raw = if l.starts_with("<!--") && !l.contains("-->") {
                    Some("-->")
                } else {
                    [("<pre", "</pre>"), ("<script", "</script>"), ("<style", "</style>"), ("<textarea", "</textarea>")]
                        .iter()
                        .find(|(o, e)| l.starts_with(o) && !l.contains(e))
                        .map(|(_, e)| *e)
                };
            } else if cols <= 3
                && rest.starts_with('[')
                && let Some(e) = rest.find("]:")
            {
                let label = &rest[1..e];
                if let Some(f) = label.strip_prefix('^') {
                    ctx.foot.insert(f.trim().to_lowercase());
                } else if !label.is_empty() {
                    let url = rest[e + 2..].split_whitespace().next().unwrap_or("").trim_matches(['<', '>']);
                    if !url.is_empty() {
                        ctx.refs.entry(norm_label(label)).or_insert_with(|| url.to_string());
                    }
                }
            }
        }
        off += len;
        ln += 1;
    }
    if let Some(c) = cur.take() {
        close(c, &mut segs);
    }
    // Hash of the document-wide context: when empty, segments hash by their text alone.
    if !ctx.refs.is_empty() || !ctx.foot.is_empty() {
        let mut keys: Vec<(&String, &String)> = ctx.refs.iter().collect();
        keys.sort();
        let mut h = 1u64;
        for (k, v) in keys {
            h = fxhash(k.as_bytes(), h);
            h = fxhash(v.as_bytes(), h);
        }
        let mut f: Vec<&String> = ctx.foot.iter().collect();
        f.sort();
        for k in f {
            h = fxhash(k.as_bytes(), h ^ 0xf007);
        }
        ctx.hash = h;
    }
    let lines = text.bytes().filter(|b| *b == b'\n').count() + 1;
    Split { segs, ctx, lines }
}

// ---------------------------------------------------------------------------------------------
// parsing

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(s: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(s.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
        Self { starts }
    }
    fn line_of(&self, off: usize) -> usize {
        self.starts.partition_point(|&s| s <= off).saturating_sub(1)
    }
    fn range(&self, r: &std::ops::Range<usize>) -> (usize, usize) {
        let a = self.line_of(r.start);
        let b = self.line_of(r.end.saturating_sub(1).max(r.start));
        (a, b.max(a))
    }
}

enum Cont {
    Root(Vec<Block>),
    Quote { blocks: Vec<Block>, l: (usize, usize) },
    Item { blocks: Vec<Block>, task: Option<bool>, task_off: usize },
    Foot { blocks: Vec<Block>, label: String, l: (usize, usize) },
    List { start: Option<u64>, items: Vec<LItem>, loose: bool, l: (usize, usize) },
}

enum Seg {
    Spans(Vec<Inl>),
    Img(Img),
    Math(String),
}

#[derive(Clone, Copy, PartialEq)]
enum IK {
    Para,
    Head(u8),
    Cell,
}

struct InlCtx {
    kind: IK,
    spans: Vec<Inl>,
    segs: Vec<Seg>,
    start: usize,
    end: usize,
    implicit: bool,
}

#[derive(Default)]
struct TableB {
    aligns: Vec<Alignment>,
    head: Vec<Vec<Inl>>,
    rows: Vec<Vec<Vec<Inl>>>,
    cur: Vec<Vec<Inl>>,
    l: (usize, usize),
}

struct Builder<'a> {
    idx: &'a LineIndex,
    urls: &'a mut Urls,
    /// Footnote labels (lowercase) defined somewhere in the document.
    known: &'a HashSet<String>,
    /// Footnote references met in this segment, in order.
    refs: Vec<String>,
    stack: Vec<Cont>,
    inl: Option<InlCtx>,
    strong: u32,
    em: u32,
    strike: u32,
    links: Vec<u32>,
    code: Option<(String, String, (usize, usize))>,
    table: Option<TableB>,
    html: Option<(String, (usize, usize))>,
    image: Option<(String, String, u32)>,
}

fn blank(spans: &[Inl]) -> bool {
    spans.iter().all(|s| match s {
        Inl::Text { text, .. } => text.trim().is_empty(),
        Inl::FootRef(_) => false,
        _ => true,
    })
}

fn plain(spans: &[Inl]) -> String {
    let mut s = String::new();
    for i in spans {
        match i {
            Inl::Text { text, .. } => s.push_str(text),
            Inl::Soft | Inl::Hard => s.push(' '),
            Inl::FootRef(_) => {}
        }
    }
    s
}

impl<'a> Builder<'a> {
    fn flags(&self) -> (u8, u32) {
        let mut f = 0;
        if self.strong > 0 {
            f |= F_B;
        }
        if self.em > 0 {
            f |= F_I;
        }
        if self.strike > 0 {
            f |= F_S;
        }
        let link = self.links.last().copied().unwrap_or(0);
        if link != 0 {
            f |= F_LINK;
        }
        (f, link)
    }

    fn push_block(&mut self, b: Block) {
        match self.stack.last_mut() {
            Some(Cont::Root(v)) => v.push(b),
            Some(Cont::Quote { blocks, .. }) | Some(Cont::Item { blocks, .. }) | Some(Cont::Foot { blocks, .. }) => {
                blocks.push(b)
            }
            _ => {}
        }
    }

    fn start_inline(&mut self, kind: IK, start: usize, end: usize, implicit: bool) {
        self.inl = Some(InlCtx { kind, spans: vec![], segs: vec![], start, end, implicit });
    }

    fn push_span(&mut self, text: &str, extra: u8) {
        let (flags, link) = self.flags();
        let flags = flags | extra;
        let Some(ic) = self.inl.as_mut() else { return };
        if let Some(Inl::Text { text: t, flags: f, link: l }) = ic.spans.last_mut()
            && *f == flags
            && *l == link
            && flags & F_CODE == 0
        {
            t.push_str(text);
            return;
        }
        ic.spans.push(Inl::Text { text: text.to_string(), flags, link });
    }

    fn text(&mut self, t: &str, range: &std::ops::Range<usize>) {
        if let Some((alt, _, _)) = self.image.as_mut() {
            alt.push_str(t);
            return;
        }
        if let Some((_, code, _)) = self.code.as_mut() {
            code.push_str(t);
            return;
        }
        if let Some((h, _)) = self.html.as_mut() {
            h.push_str(t);
            return;
        }
        if self.inl.is_none() {
            self.start_inline(IK::Para, range.start, range.end, true);
        }
        if let Some(ic) = self.inl.as_mut() {
            ic.end = ic.end.max(range.end);
        }
        self.push_span(t, 0);
    }

    /// Finish an implicit (tag-less) paragraph, as found in tight list items.
    fn close_implicit(&mut self) {
        if self.inl.as_ref().is_some_and(|i| i.implicit) {
            self.finish_inline();
        }
    }

    fn finish_inline(&mut self) {
        let Some(mut ic) = self.inl.take() else { return };
        let l = self.idx.range(&(ic.start..ic.end));
        match ic.kind {
            IK::Cell => {
                if let Some(t) = self.table.as_mut() {
                    t.cur.push(std::mem::take(&mut ic.spans));
                }
            }
            IK::Head(level) => {
                let slug = slugify(&plain(&ic.spans));
                let b = Block { kind: Kind::Heading { level, inl: ic.spans, slug }, line: l.0, end: l.1 };
                self.push_block(b);
            }
            IK::Para => {
                if !ic.spans.is_empty() {
                    ic.segs.push(Seg::Spans(std::mem::take(&mut ic.spans)));
                }
                let mut imgs: Vec<Img> = vec![];
                let mut out: Vec<Block> = vec![];
                let mk = |kind| Block { kind, line: l.0, end: l.1 };
                for seg in ic.segs {
                    match seg {
                        Seg::Img(i) => imgs.push(i),
                        Seg::Math(m) => {
                            if !imgs.is_empty() {
                                out.push(mk(Kind::Images(std::mem::take(&mut imgs))));
                            }
                            out.push(mk(Kind::Math(m)));
                        }
                        Seg::Spans(s) => {
                            if blank(&s) {
                                continue;
                            }
                            if !imgs.is_empty() {
                                out.push(mk(Kind::Images(std::mem::take(&mut imgs))));
                            }
                            out.push(mk(Kind::Para(s)));
                        }
                    }
                }
                if !imgs.is_empty() {
                    out.push(mk(Kind::Images(imgs)));
                }
                for b in out {
                    self.push_block(b);
                }
            }
        }
    }

    fn url_index(&mut self, url: &str) -> u32 {
        self.urls.id(url)
    }

    fn html_block(&mut self, raw: &str, l: (usize, usize)) {
        let mut s = raw.to_string();
        while let Some(a) = s.find("<!--") {
            let e = s[a..].find("-->").map_or(s.len(), |e| a + e + 3);
            s.replace_range(a..e, "");
        }
        let lower = s.to_ascii_lowercase();
        let href = lower.find("<a ").and_then(|i| {
            let end = s[i..].find('>').map_or(s.len(), |e| i + e);
            attr(&s[i..end], "href")
        });
        let link = href.map_or(0, |h| self.url_index(&h));
        let mut imgs = vec![];
        let mut from = 0;
        while let Some(i) = lower[from..].find("<img") {
            let i = from + i;
            let end = s[i..].find('>').map_or(s.len(), |e| i + e + 1);
            let tag = &s[i..end];
            let dim = |n: &str| {
                attr(tag, n)
                    .and_then(|v| v.trim().trim_end_matches("px").trim().parse::<f32>().ok())
                    .filter(|v| *v >= 1.0)
                    .map(|v| v as i32)
            };
            imgs.push(Img {
                alt: attr(tag, "alt").unwrap_or_default(),
                url: attr(tag, "src").unwrap_or_default(),
                link,
                size: (dim("width"), dim("height")),
            });
            from = end;
        }
        let text = strip_tags(&s);
        let b = |kind| Block { kind, line: l.0, end: l.1 };
        if !imgs.is_empty() {
            self.push_block(b(Kind::Images(imgs)));
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            self.push_block(b(Kind::Para(vec![Inl::Text { text, flags: F_DIM, link: 0 }])));
        }
    }

    fn event(&mut self, ev: Event, range: std::ops::Range<usize>) {
        match ev {
            Event::Start(tag) => self.start(tag, &range),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t, &range),
            Event::Code(c) => {
                if let Some((alt, _, _)) = self.image.as_mut() {
                    alt.push_str(&c);
                } else {
                    if self.inl.is_none() {
                        self.start_inline(IK::Para, range.start, range.end, true);
                    }
                    if let Some(ic) = self.inl.as_mut() {
                        ic.end = ic.end.max(range.end);
                    }
                    self.push_span(&c, F_CODE);
                }
            }
            Event::InlineMath(m) => {
                if let Some((alt, _, _)) = self.image.as_mut() {
                    alt.push_str(&m);
                } else {
                    if self.inl.is_none() {
                        self.start_inline(IK::Para, range.start, range.end, true);
                    }
                    if let Some(ic) = self.inl.as_mut() {
                        ic.end = ic.end.max(range.end);
                    }
                    self.push_span(m.trim(), F_CODE | F_MATH);
                }
            }
            Event::DisplayMath(m) => {
                if self.inl.is_none() {
                    self.start_inline(IK::Para, range.start, range.end, true);
                }
                let para = self.inl.as_ref().is_some_and(|i| i.kind == IK::Para);
                if let Some(ic) = self.inl.as_mut() {
                    ic.end = ic.end.max(range.end);
                    if para {
                        if !ic.spans.is_empty() {
                            let s = std::mem::take(&mut ic.spans);
                            ic.segs.push(Seg::Spans(s));
                        }
                        ic.segs.push(Seg::Math(m.trim().to_string()));
                    }
                }
                if !para {
                    self.push_span(m.trim(), F_CODE | F_MATH);
                }
            }
            Event::FootnoteReference(label) => {
                let known = self.known.contains(&label.trim().to_lowercase());
                if self.inl.is_none() {
                    self.start_inline(IK::Para, range.start, range.end, true);
                }
                if let Some(ic) = self.inl.as_mut() {
                    ic.end = ic.end.max(range.end);
                }
                if known {
                    self.refs.push(label.trim().to_lowercase());
                    if let Some(ic) = self.inl.as_mut() {
                        ic.spans.push(Inl::FootRef(label.trim().to_lowercase()));
                    }
                } else {
                    self.push_span(&format!("[^{label}]"), 0);
                }
            }
            Event::SoftBreak => {
                if let Some((alt, _, _)) = self.image.as_mut() {
                    alt.push(' ');
                } else if let Some(ic) = self.inl.as_mut() {
                    ic.spans.push(Inl::Soft);
                }
            }
            Event::HardBreak => {
                if let Some(ic) = self.inl.as_mut() {
                    ic.spans.push(Inl::Hard);
                }
            }
            Event::Html(h) => {
                if let Some((s, _)) = self.html.as_mut() {
                    s.push_str(&h);
                }
            }
            Event::InlineHtml(h) => {
                let t = h.trim().to_ascii_lowercase();
                if t.starts_with("<br")
                    && let Some(ic) = self.inl.as_mut()
                {
                    ic.spans.push(Inl::Hard);
                }
            }
            Event::Rule => {
                self.close_implicit();
                let l = self.idx.range(&range);
                self.push_block(Block { kind: Kind::Rule, line: l.0, end: l.1 });
            }
            Event::TaskListMarker(done) => {
                if let Some(Cont::Item { task, task_off, .. }) = self.stack.last_mut() {
                    *task = Some(done);
                    *task_off = range.start;
                }
            }
        }
    }

    fn start(&mut self, tag: Tag, range: &std::ops::Range<usize>) {
        let l = self.idx.range(range);
        match tag {
            Tag::Paragraph => {
                self.close_implicit();
                let n = self.stack.len();
                if n >= 2
                    && matches!(self.stack[n - 1], Cont::Item { .. })
                    && let Cont::List { loose, .. } = &mut self.stack[n - 2]
                {
                    *loose = true;
                }
                self.start_inline(IK::Para, range.start, range.end, false);
            }
            Tag::Heading { level, .. } => {
                self.close_implicit();
                self.start_inline(IK::Head(level as u8), range.start, range.end, false);
            }
            Tag::BlockQuote(_) => {
                self.close_implicit();
                self.stack.push(Cont::Quote { blocks: vec![], l });
            }
            Tag::CodeBlock(kind) => {
                self.close_implicit();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").trim_matches(|c| c == '{' || c == '}').to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new(), l));
            }
            Tag::HtmlBlock => {
                self.close_implicit();
                self.html = Some((String::new(), l));
            }
            Tag::List(start) => {
                self.close_implicit();
                self.stack.push(Cont::List { start, items: vec![], loose: false, l });
            }
            Tag::Item => {
                self.close_implicit();
                self.stack.push(Cont::Item { blocks: vec![], task: None, task_off: 0 });
            }
            Tag::FootnoteDefinition(label) => {
                self.close_implicit();
                self.stack.push(Cont::Foot { blocks: vec![], label: label.trim().to_lowercase(), l });
            }
            Tag::Table(aligns) => {
                self.close_implicit();
                self.table = Some(TableB { aligns, l, ..Default::default() });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.cur.clear();
                }
            }
            Tag::TableCell => self.start_inline(IK::Cell, range.start, range.end, false),
            Tag::Emphasis => self.em += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => {
                let i = self.url_index(&dest_url);
                self.links.push(i);
            }
            Tag::Image { dest_url, .. } => {
                let link = self.links.last().copied().unwrap_or(0);
                self.image = Some((String::new(), dest_url.to_string(), link));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell => self.finish_inline(),
            TagEnd::BlockQuote(_) => {
                self.close_implicit();
                if let Some(Cont::Quote { blocks, l }) = self.stack.pop() {
                    self.push_block(Block { kind: Kind::Quote(blocks), line: l.0, end: l.1 });
                }
            }
            TagEnd::CodeBlock => {
                if let Some((lang, mut text, l)) = self.code.take() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    self.push_block(Block { kind: Kind::Code { lang, text }, line: l.0, end: l.1 });
                }
            }
            TagEnd::HtmlBlock => {
                if let Some((raw, l)) = self.html.take() {
                    self.html_block(&raw, l);
                }
            }
            TagEnd::List(_) => {
                self.close_implicit();
                if let Some(Cont::List { start, items, loose, l }) = self.stack.pop() {
                    self.push_block(Block { kind: Kind::List { start, items, loose }, line: l.0, end: l.1 });
                }
            }
            TagEnd::Item => {
                self.close_implicit();
                if let Some(Cont::Item { blocks, task, task_off }) = self.stack.pop()
                    && let Some(Cont::List { items, .. }) = self.stack.last_mut()
                {
                    items.push(LItem { task, task_off, blocks });
                }
            }
            TagEnd::FootnoteDefinition => {
                self.close_implicit();
                if let Some(Cont::Foot { blocks, label, l }) = self.stack.pop() {
                    self.push_block(Block { kind: Kind::FootDef { label, blocks }, line: l.0, end: l.1 });
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.head = std::mem::take(&mut t.cur);
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    let r = std::mem::take(&mut t.cur);
                    t.rows.push(r);
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.push_block(Block {
                        kind: Kind::Table { aligns: t.aligns, head: t.head, rows: t.rows },
                        line: t.l.0,
                        end: t.l.1,
                    });
                }
            }
            TagEnd::Emphasis => self.em = self.em.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                if let Some((alt, url, link)) = self.image.take() {
                    let in_para = self.inl.as_ref().is_some_and(|i| i.kind == IK::Para);
                    if in_para {
                        if let Some(ic) = self.inl.as_mut() {
                            if !ic.spans.is_empty() {
                                let s = std::mem::take(&mut ic.spans);
                                ic.segs.push(Seg::Spans(s));
                            }
                            ic.segs.push(Seg::Img(Img { alt, url, link, size: (None, None) }));
                        }
                    } else {
                        let label = if alt.is_empty() { "image".to_string() } else { alt };
                        self.push_span(&format!("[{label}]"), 0);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Value of `name="..."` (or single quoted) inside an HTML tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let i = from + i;
        from = i + name.len();
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = tag[i + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let v = rest[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let inner = &v[1..];
                return inner.find(q).map(|e| inner[..e].to_string());
            }
            return Some(v.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or("").to_string());
        }
    }
    None
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// layout output

#[derive(Clone, Copy, PartialEq, Debug)]
enum Col {
    Fg,
    Dim,
    Accent,
    AccentSoft,
    Border,
    Bar,
    Widget,
    Input,
    Bg,
}

#[derive(Debug)]
enum Op {
    Text {
        x: i32,
        w: i32,
        text: String,
        flags: u8,
        si: u8,
        lh: i32,
    },
    Fill {
        r: Rect,
        col: Col,
        rad: i32,
    },
    Card {
        r: Rect,
        rad: i32,
        fill: Col,
        border: Col,
    },
    Stroke {
        r: Rect,
        col: Col,
    },
    Bullet {
        cx: i32,
        cy: i32,
        level: u8,
    },
    Check {
        x: i32,
        y: i32,
        done: bool,
    },
    /// Code block (`math` = a `$$` block: centered, tinted). `key` identifies lang + text for the
    /// highlight cache.
    Code {
        r: Rect,
        lang: String,
        lines: Vec<String>,
        lh: i32,
        top: i32,
        key: u64,
        math: bool,
    },
    /// A local image drawn at `r` (logical px); `id` is its [`ImgStore`] id.
    Image {
        r: Rect,
        id: u32,
    },
}

#[derive(Debug)]
struct DrawOp {
    y0: i32,
    y1: i32,
    op: Op,
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    l0: i64,
    l1: i64,
    y0: i32,
    y1: i32,
}

#[derive(Clone, Copy, Debug)]
struct LinkHit {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    link: u32,
}

/// A task checkbox: its hit box and the byte offset of its `[` (inside the segment).
#[derive(Clone, Copy, Debug)]
struct CheckHit {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    off: usize,
}

/// Cache key of a laid-out segment: width, code font size, cell width, footnote numbering, image base.
type SegKey = (i32, u32, u32, u64, u32);

/// One segment laid out on its own: ops and hit boxes with y relative to the segment's top (x is
/// absolute: the text column is baked in).
struct SegLay {
    key: SegKey,
    ops: Vec<DrawOp>,
    /// Running max of `y1` over `ops` (sorted by `y0`), to find the first visible op by bisection.
    pmax: Vec<i32>,
    height: i32,
    /// Anchors with lines relative to the segment's first line.
    anchors: Vec<Anchor>,
    links: Vec<LinkHit>,
    checks: Vec<CheckHit>,
    /// `(name, y, exact)`: headings carry their base slug (deduplicated when flattened), footnotes
    /// an exact `fn-label`.
    heads: Vec<(String, i32, bool)>,
    max_link_h: i32,
    first_before: i32,
    last_after: i32,
    /// Nothing to show (blank, only definitions): takes no space and no gap.
    empty: bool,
}

/// The whole document: segments stacked, with everything hit-testing and line mapping need
/// flattened into absolute coordinates.
struct Layout {
    key: (i32, u32, u32, u64, u32),
    height: i32,
    /// The segments that take space, in document order (nothing is copied out of them: line
    /// mapping and hit-testing look into the segment that owns the line / point).
    segs: Vec<SegRef>,
    /// Left edge and width of the text column.
    col_x: i32,
    col_w: i32,
}

/// Where one non-empty segment sits: index into `MdView::order`, first source line, line count
/// and y range.
#[derive(Clone, Copy, Debug)]
struct SegRef {
    ix: usize,
    l0: i64,
    nl: i64,
    y0: i32,
    y1: i32,
}

/// Cheap hasher for keys that already are hashes.
#[derive(Default)]
struct IdHasher(u64);

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8)) ^ b as u64;
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

type KeyMap<V> = HashMap<u64, V, std::hash::BuildHasherDefault<IdHasher>>;
type KeySet = HashSet<u64, std::hash::BuildHasherDefault<IdHasher>>;

/// Parsed (and maybe laid out) segment, shared by every identical piece of source.
struct SegData {
    blocks: Vec<Block>,
    /// Footnote labels referenced, in order.
    refs: Vec<String>,
    lay: Option<SegLay>,
    used: u64,
}

/// A segment of the current text and where it sits.
#[derive(Clone, Copy, Debug)]
struct Placed {
    key: u64,
    l0: usize,
    nl: usize,
    b0: usize,
    y0: i32,
    y1: i32,
}

// ---------------------------------------------------------------------------------------------
// measuring and inline flow

#[derive(Default)]
struct Measure {
    maps: Vec<HashMap<String, f32>>,
}

impl Measure {
    /// Width in px with 1/8 px precision (measured at 8x size): per-word integer rounding would
    /// drift by several px over a line.
    fn w(&mut self, tx: &mut TextCtx, s: &str, bold: bool, si: usize) -> f32 {
        if self.maps.is_empty() {
            self.maps = (0..SIZES.len() * 2).map(|_| HashMap::new()).collect();
        }
        let m = &mut self.maps[si * 2 + bold as usize];
        if let Some(&w) = m.get(s) {
            return w;
        }
        let w = tx.ui_width_sized(s, bold, SIZES[si] * 8.0) as f32 / 8.0;
        if m.len() > 100_000 {
            m.clear();
        }
        m.insert(s.to_string(), w);
        w
    }
}

/// Monospace cells taken by `c`.
fn cell_count(c: char) -> usize {
    let u = c as u32;
    if u < 0x300 {
        return 1;
    }
    if (0x300..=0x36f).contains(&u) || u == 0x200b || u == 0xfe0f {
        return 0;
    }
    let wide = (0x1100..=0x115f).contains(&u)
        || (0x2e80..=0xa4cf).contains(&u)
        || (0xac00..=0xd7a3).contains(&u)
        || (0xf900..=0xfaff).contains(&u)
        || (0xfe30..=0xfe6f).contains(&u)
        || (0xff00..=0xff60).contains(&u)
        || (0xffe0..=0xffe6).contains(&u)
        || (0x1f300..=0x1f64f).contains(&u)
        || (0x1f900..=0x1f9ff).contains(&u);
    if wide { 2 } else { 1 }
}

fn code_w(tx: &TextCtx, s: &str) -> f32 {
    s.chars().map(cell_count).sum::<usize>() as f32 * tx.cell_w
}

#[derive(Clone, Debug)]
struct Run {
    x: f32,
    w: f32,
    text: String,
    flags: u8,
    link: u32,
}

struct Atom {
    text: String,
    flags: u8,
    link: u32,
    w: f32,
}

fn push_piece(line: &mut Vec<Run>, x: f32, text: &str, flags: u8, link: u32, w: f32) {
    if flags & F_CODE == 0
        && let Some(l) = line.last_mut()
        && l.flags == flags
        && l.link == link
        && (l.x + l.w - x).abs() < 0.01
    {
        l.text.push_str(text);
        l.w += w;
        return;
    }
    line.push(Run { x, w, text: text.to_string(), flags, link });
}

struct Flow {
    lines: Vec<Vec<Run>>,
    x: f32,
    chunk: Vec<Atom>,
    chunk_w: f32,
    space: Option<(u8, u32, f32)>,
    width: f32,
    wrap: bool,
    si: usize,
    bold_base: bool,
}

impl Flow {
    fn flush(&mut self, m: &mut Measure, tx: &mut TextCtx) {
        if self.chunk.is_empty() {
            return;
        }
        let mut line_empty = self.lines.last().is_none_or(|l| l.is_empty());
        let mut sp = if line_empty { None } else { self.space };
        self.space = None;
        let sp_w = sp.map_or(0.0, |s| s.2);
        if self.wrap && !line_empty && self.x + sp_w + self.chunk_w > self.width {
            self.lines.push(vec![]);
            self.x = 0.0;
            line_empty = true;
            sp = None;
        }
        if let (Some((f, l, w)), false) = (sp, line_empty) {
            push_piece(self.lines.last_mut().unwrap(), self.x, " ", f, l, w);
            self.x += w;
        }
        if self.wrap && self.x + self.chunk_w > self.width {
            self.split_chunk(m, tx);
        } else {
            for a in self.chunk.drain(..) {
                push_piece(self.lines.last_mut().unwrap(), self.x, &a.text, a.flags, a.link, a.w);
                self.x += a.w;
            }
        }
        self.chunk.clear();
        self.chunk_w = 0.0;
    }

    /// A word wider than the line: break it between characters.
    fn split_chunk(&mut self, m: &mut Measure, tx: &mut TextCtx) {
        let atoms = std::mem::take(&mut self.chunk);
        for a in atoms {
            let code = a.flags & F_CODE != 0;
            let pad = if code { CHIP_PAD as f32 } else { 0.0 };
            let bold = self.bold_base || a.flags & F_B != 0;
            let (mut buf, mut bw) = (String::new(), 0.0f32);
            for ch in a.text.chars() {
                let cw = if code {
                    cell_count(ch) as f32 * tx.cell_w
                } else {
                    m.w(tx, ch.encode_utf8(&mut [0; 4]), bold, self.si)
                };
                if self.x + 2.0 * pad + bw + cw > self.width && (bw > 0.0 || self.x > 0.0) {
                    if bw > 0.0 {
                        push_piece(self.lines.last_mut().unwrap(), self.x, &buf, a.flags, a.link, bw + 2.0 * pad);
                        buf.clear();
                        bw = 0.0;
                    }
                    self.lines.push(vec![]);
                    self.x = 0.0;
                }
                buf.push(ch);
                bw += cw;
            }
            if bw > 0.0 {
                push_piece(self.lines.last_mut().unwrap(), self.x, &buf, a.flags, a.link, bw + 2.0 * pad);
                self.x += bw + 2.0 * pad;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// layout

#[derive(Clone, Copy)]
struct St {
    dim: bool,
    depth: u8,
    tight: Option<i32>,
}

struct Lay<'a> {
    tx: &'a mut TextCtx,
    m: &'a mut Measure,
    urls: &'a mut Urls,
    imgs: &'a mut ImgStore,
    /// Footnote label -> number.
    fnum: &'a HashMap<String, usize>,
    ops: Vec<DrawOp>,
    anchors: Vec<Anchor>,
    links: Vec<LinkHit>,
    checks: Vec<CheckHit>,
    heads: Vec<(String, i32, bool)>,
}

fn before(b: &Block) -> i32 {
    match &b.kind {
        Kind::Heading { level, .. } => {
            if *level <= 2 {
                24
            } else {
                20
            }
        }
        _ => 0,
    }
}

fn after(b: &Block) -> i32 {
    match &b.kind {
        Kind::Heading { .. } => 10,
        _ => 12,
    }
}

impl Lay<'_> {
    fn op(&mut self, y0: i32, y1: i32, op: Op) {
        self.ops.push(DrawOp { y0, y1, op });
    }

    fn fill(&mut self, r: Rect, col: Col, rad: i32) {
        self.op(r.y, r.bottom(), Op::Fill { r, col, rad });
    }

    /// Register the `fnref-<label>` anchor of every footnote reference in `inl` (laid out at `y`):
    /// the back-link of the footnote returns to the block that first mentions it.
    fn ref_anchors(&mut self, inl: &[Inl], y: i32) {
        for item in inl {
            if let Inl::FootRef(label) = item {
                self.heads.push((format!("fnref-{label}"), y, true));
            }
        }
    }

    fn anchor(&mut self, b: &Block, y0: i32, y1: i32) {
        let (mut l0, mut l1) = (b.line as i64, b.end as i64);
        if let Some(p) = self.anchors.last() {
            l0 = l0.max(p.l0);
            l1 = l1.max(p.l1);
        }
        self.anchors.push(Anchor { l0, l1: l1.max(l0), y0, y1 });
    }

    /// Flow inline content into lines of runs (x relative to the column).
    fn flow(&mut self, inl: &[Inl], width: i32, si: usize, bold_base: bool, wrap: bool) -> Vec<Vec<Run>> {
        let mut f = Flow {
            lines: vec![vec![]],
            x: 0.0,
            chunk: vec![],
            chunk_w: 0.0,
            space: None,
            width: width.max(1) as f32,
            wrap,
            si,
            bold_base,
        };
        let (mut lf, mut ll) = (0u8, 0u32);
        for item in inl {
            match item {
                Inl::Text { text, flags, link } => {
                    lf = *flags & !F_CODE;
                    ll = *link;
                    if flags & F_CODE != 0 {
                        let w = code_w(self.tx, text) + 2.0 * CHIP_PAD as f32;
                        f.chunk.push(Atom { text: text.clone(), flags: *flags, link: *link, w });
                        f.chunk_w += w;
                        continue;
                    }
                    let bold = bold_base || flags & F_B != 0;
                    let mut rest = text.as_str();
                    while !rest.is_empty() {
                        let is_sp = rest.starts_with(char::is_whitespace);
                        let end = rest.find(|c: char| c.is_whitespace() != is_sp).unwrap_or(rest.len());
                        let (tok, r) = rest.split_at(end);
                        rest = r;
                        if is_sp {
                            f.flush(self.m, self.tx);
                            f.space = Some((*flags & !F_CODE, *link, self.m.w(self.tx, " ", bold, si)));
                        } else {
                            let w = self.m.w(self.tx, tok, bold, si);
                            f.chunk.push(Atom { text: tok.to_string(), flags: *flags, link: *link, w });
                            f.chunk_w += w;
                        }
                    }
                }
                Inl::FootRef(label) => {
                    let text = format!("[{}]", self.fnum.get(label).copied().unwrap_or(0));
                    let link = self.urls.id(&format!("#fn-{label}"));
                    let w = self.m.w(self.tx, &text, bold_base, si);
                    f.chunk.push(Atom { text, flags: F_LINK, link, w });
                    f.chunk_w += w;
                }
                Inl::Soft => {
                    f.flush(self.m, self.tx);
                    f.space = Some((lf, ll, self.m.w(self.tx, " ", bold_base, si)));
                }
                Inl::Hard => {
                    f.flush(self.m, self.tx);
                    f.space = None;
                    f.lines.push(vec![]);
                    f.x = 0.0;
                }
            }
        }
        f.flush(self.m, self.tx);
        let mut lines = f.lines;
        while lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines
    }

    /// Cut a single-line run list to `max_w`, ending in "..." (a real ellipsis glyph would drag in
    /// the fallback fonts).
    fn ellipsize(&mut self, line: Vec<Run>, max_w: f32, si: usize, bold_base: bool) -> Vec<Run> {
        let total = line.last().map_or(0.0, |r| r.x + r.w);
        if total <= max_w + 0.5 {
            return line;
        }
        let ell = self.m.w(self.tx, "...", bold_base, si);
        let target = (max_w - ell).max(0.0);
        let mut out: Vec<Run> = vec![];
        for run in line {
            if run.x + run.w <= target {
                out.push(run);
                continue;
            }
            let code = run.flags & F_CODE != 0;
            let pad = if code { CHIP_PAD as f32 } else { 0.0 };
            let bold = bold_base || run.flags & F_B != 0;
            let (mut buf, mut bw) = (String::new(), 0.0f32);
            for ch in run.text.chars() {
                let cw = if code {
                    cell_count(ch) as f32 * self.tx.cell_w
                } else {
                    self.m.w(self.tx, ch.encode_utf8(&mut [0; 4]), bold, si)
                };
                if run.x + pad + bw + cw > target {
                    break;
                }
                buf.push(ch);
                bw += cw;
            }
            let mut ex = run.x;
            if bw > 0.0 {
                ex = run.x + bw + 2.0 * pad;
                out.push(Run { x: run.x, w: bw + 2.0 * pad, text: buf, flags: run.flags, link: run.link });
            }
            out.push(Run { x: ex, w: ell, text: "...".into(), flags: run.flags & !F_CODE, link: run.link });
            return out;
        }
        out
    }

    /// Truncate a plain string to `max_w` (adds "..."); returns text and width in px.
    fn fit(&mut self, s: &str, si: usize, bold: bool, max_w: i32) -> (String, i32) {
        let w = self.m.w(self.tx, s, bold, si);
        if w <= max_w as f32 {
            return (s.to_string(), w.round() as i32);
        }
        let ell = self.m.w(self.tx, "...", bold, si);
        let (mut out, mut ow) = (String::new(), 0.0f32);
        for ch in s.chars() {
            let cw = self.m.w(self.tx, ch.encode_utf8(&mut [0; 4]), bold, si);
            if ow + cw + ell > max_w as f32 {
                break;
            }
            out.push(ch);
            ow += cw;
        }
        out.push_str("...");
        (out, (ow + ell).round() as i32)
    }

    fn emit(&mut self, lines: Vec<Vec<Run>>, x: i32, y: i32, si: usize, base: u8) {
        let lh = LH[si];
        for (i, line) in lines.into_iter().enumerate() {
            let ly = y + i as i32 * lh;
            for r in line {
                let rx = (x as f32 + r.x).round() as i32;
                let rw = ((x as f32 + r.x + r.w).round() as i32 - rx).max(1);
                if r.link != 0 {
                    self.links.push(LinkHit { x: rx, y: ly, w: rw, h: lh, link: r.link });
                }
                self.op(ly, ly + lh, Op::Text { x: rx, w: rw, text: r.text, flags: r.flags | base, si: si as u8, lh });
            }
        }
    }

    fn blocks(&mut self, bl: &[Block], x: i32, w: i32, mut y: i32, st: St) -> i32 {
        let mut prev: Option<&Block> = None;
        for b in bl {
            if matches!(b.kind, Kind::FootDef { .. }) {
                continue; // collected at the end of the document
            }
            if let Some(p) = prev {
                y += st.tight.unwrap_or_else(|| after(p).max(before(b)));
            }
            y = self.block(b, x, w, y, st);
            prev = Some(b);
        }
        y
    }

    fn block(&mut self, b: &Block, x: i32, w: i32, y: i32, st: St) -> i32 {
        let w = w.max(40);
        match &b.kind {
            Kind::Heading { level, inl, slug } => {
                let si = (*level - 1) as usize;
                self.ref_anchors(inl, y);
                let lines = self.flow(inl, w, si, true, true);
                let n = lines.len().max(1) as i32;
                self.heads.push((slug.clone(), y, false));
                let base = F_B | if *level == 6 || st.dim { F_DIM } else { 0 };
                self.emit(lines, x, y, si, base);
                let mut h = n * LH[si];
                if *level <= 2 {
                    self.fill(Rect::new(x, y + h + 6, w, 1), Col::Border, 0);
                    h += 7;
                }
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::Para(inl) => {
                self.ref_anchors(inl, y);
                let lines = self.flow(inl, w, BODY, false, true);
                let h = lines.len().max(1) as i32 * LH[BODY];
                self.emit(lines, x, y, BODY, if st.dim { F_DIM } else { 0 });
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::Code { lang, text } => {
                let lh = line_h() as i32;
                let lines: Vec<String> =
                    text.split('\n').map(|l| l.trim_end_matches('\r').replace('\t', "    ")).collect();
                let top = if lang.is_empty() { 12 } else { 18 };
                let h = top + lines.len() as i32 * lh + 12;
                let r = Rect::new(x, y, w, h);
                let key = fxhash(text.as_bytes(), fxhash(lang.as_bytes(), 3));
                self.op(y, y + h, Op::Code { r, lang: lang.clone(), lines, lh, top, key, math: false });
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::Math(text) => {
                let lh = line_h() as i32;
                let lines: Vec<String> =
                    text.split('\n').map(|l| l.trim_end_matches('\r').replace('\t', "    ")).collect();
                let h = 18 + lines.len() as i32 * lh + 12;
                self.op(
                    y,
                    y + h,
                    Op::Code { r: Rect::new(x, y, w, h), lang: "math".into(), lines, lh, top: 18, key: 0, math: true },
                );
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::FootDef { .. } => {
                self.anchor(b, y, y);
                y
            }
            Kind::Rule => {
                self.fill(Rect::new(x, y, w, 1), Col::Border, 0);
                self.anchor(b, y, y + 1);
                y + 1
            }
            Kind::Quote(bl) => {
                let y1 = self.blocks(bl, x + QUOTE_INDENT, w - QUOTE_INDENT, y, St { dim: true, ..st });
                if y1 > y {
                    self.fill(Rect::new(x, y, 3, y1 - y), Col::Bar, 0);
                }
                y1
            }
            Kind::List { start, items, loose } => self.list(*start, items, *loose, x, w, y, st),
            Kind::Table { aligns, head, rows } => self.table(b, aligns, head, rows, x, w, y),
            Kind::Images(imgs) => self.images(b, imgs, x, w, y),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn list(&mut self, start: Option<u64>, items: &[LItem], loose: bool, x: i32, w: i32, y: i32, st: St) -> i32 {
        let labels: Vec<String> = match start {
            Some(s) => (0..items.len()).map(|i| format!("{}.", s + i as u64)).collect(),
            None => vec![],
        };
        let lws: Vec<i32> = labels.iter().map(|l| self.m.w(self.tx, l, false, BODY).ceil() as i32).collect();
        let lw = lws.iter().copied().max().unwrap_or(0);
        let indent = if start.is_some() { (lw + 12).max(26) } else { LIST_BULLET_INDENT };
        let mut yy = y;
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                yy += if loose { 10 } else { 4 };
            }
            let iy = yy;
            let inner = St { depth: st.depth + 1, tight: if loose { None } else { Some(4) }, dim: st.dim };
            let mut ey = self.blocks(&it.blocks, x + indent, w - indent, iy, inner);
            if it.blocks.is_empty() {
                ey = iy + LH[BODY];
            }
            let mid = iy + LH[BODY] / 2;
            if let Some(done) = it.task {
                self.op(iy + 4, iy + 18, Op::Check { x: x + indent - 22, y: iy + 4, done });
                self.checks.push(CheckHit { x: x + indent - 26, y: iy, w: 22, h: LH[BODY], off: it.task_off });
            } else if start.is_some() {
                let f = F_DIM;
                self.op(
                    iy,
                    iy + LH[BODY],
                    Op::Text {
                        x: x + indent - 8 - lws[i],
                        w: lws[i],
                        text: labels[i].clone(),
                        flags: f,
                        si: BODY as u8,
                        lh: LH[BODY],
                    },
                );
            } else {
                self.op(mid - 4, mid + 4, Op::Bullet { cx: x + indent - 15, cy: mid, level: st.depth % 3 });
            }
            yy = ey;
        }
        yy
    }

    #[allow(clippy::too_many_arguments)]
    fn table(
        &mut self,
        b: &Block,
        aligns: &[Alignment],
        head: &[Vec<Inl>],
        rows: &[Vec<Vec<Inl>>],
        x: i32,
        w: i32,
        y: i32,
    ) -> i32 {
        let ncols = rows.iter().map(|r| r.len()).chain([head.len()]).max().unwrap_or(0);
        if ncols == 0 {
            return y;
        }
        let empty: Vec<Inl> = vec![];
        let all: Vec<&[Vec<Inl>]> = std::iter::once(head).chain(rows.iter().map(|r| r.as_slice())).collect();
        let mut grid: Vec<Vec<Vec<Run>>> = vec![];
        let mut nat = vec![36; ncols];
        for (ri, row) in all.iter().enumerate() {
            let mut cells = vec![];
            for (c, nat_c) in nat.iter_mut().enumerate() {
                let inl = row.get(c).unwrap_or(&empty);
                let runs = self.flow(inl, i32::MAX / 4, SMALL, ri == 0, false).into_iter().next().unwrap_or_default();
                *nat_c = (*nat_c).max(runs.last().map_or(0.0, |r| r.x + r.w).ceil() as i32 + 2 * CELL_PAD);
                cells.push(runs);
            }
            grid.push(cells);
        }
        let colw = fit_columns(&nat, w);
        let tw: i32 = colw.iter().sum();
        let h = all.len() as i32 * ROW_H;
        self.fill(Rect::new(x, y, tw, ROW_H), Col::Widget, 0);
        let mut cx = x;
        for (c, cw) in colw.iter().enumerate() {
            if c > 0 {
                self.fill(Rect::new(cx, y, 1, h), Col::Border, 0);
            }
            cx += cw;
        }
        for r in 1..all.len() as i32 {
            self.fill(Rect::new(x, y + r * ROW_H - 1, tw, 1), Col::Border, 0);
        }
        self.op(y, y + h, Op::Stroke { r: Rect::new(x, y, tw, h), col: Col::Border });
        for (ri, cells) in grid.into_iter().enumerate() {
            let ry = y + ri as i32 * ROW_H;
            let mut cx = x;
            for (c, runs) in cells.into_iter().enumerate() {
                let avail = colw[c] - 2 * CELL_PAD;
                let runs = self.ellipsize(runs, avail as f32, SMALL, ri == 0);
                let cw = runs.last().map_or(0.0, |r| r.x + r.w).round() as i32;
                let ox = match aligns.get(c) {
                    Some(Alignment::Right) => (avail - cw).max(0),
                    Some(Alignment::Center) => ((avail - cw) / 2).max(0),
                    _ => 0,
                };
                let ty = ry + (ROW_H - LH[SMALL]) / 2;
                self.emit(vec![runs], cx + CELL_PAD + ox, ty, SMALL, if ri == 0 { F_B } else { 0 });
                cx += colw[c];
            }
        }
        self.anchor(b, y, y + h);
        y + h
    }

    fn images(&mut self, b: &Block, imgs: &[Img], x: i32, w: i32, y: i32) -> i32 {
        const H: i32 = 46;
        const GAP: i32 = 8;
        let (mut cx, mut cy, mut row_h) = (0, y, 0);
        for im in imgs {
            let real = self.imgs.resolve(&im.url).and_then(|id| self.imgs.size(id).map(|s| (id, s)));
            if let Some((id, (nw, nh))) = real {
                let (nw, nh) = (nw as i32, nh as i32);
                let (mut dw, mut dh) = match im.size {
                    (Some(a), Some(b)) => (a, b),
                    (Some(a), None) => (a, (nh as f32 * a as f32 / nw as f32).round() as i32),
                    (None, Some(b)) => ((nw as f32 * b as f32 / nh as f32).round() as i32, b),
                    _ => (nw, nh),
                };
                if dw > w {
                    dh = (dh as f32 * w as f32 / dw as f32).round() as i32;
                    dw = w;
                }
                if dh > 720 {
                    dw = (dw as f32 * 720.0 / dh as f32).round() as i32;
                    dh = 720;
                }
                let (dw, dh) = (dw.max(1), dh.max(1));
                if cx > 0 && cx + dw > w {
                    cx = 0;
                    cy += row_h + GAP;
                    row_h = 0;
                }
                let r = Rect::new(x + cx, cy, dw, dh);
                self.op(cy, cy + dh, Op::Image { r, id });
                if im.link != 0 {
                    self.links.push(LinkHit { x: r.x, y: cy, w: dw, h: dh, link: im.link });
                }
                cx += dw + GAP;
                row_h = row_h.max(dh);
                continue;
            }
            let label = if !im.alt.trim().is_empty() {
                im.alt.trim().to_string()
            } else {
                im.url.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("image").to_string()
            };
            let lw = self.m.w(self.tx, &label, false, SMALL).ceil() as i32;
            let uw = self.m.w(self.tx, &im.url, false, TINY).ceil() as i32;
            let bw = (lw.max(uw) + 56).clamp(170, w);
            if cx > 0 && cx + bw > w {
                cx = 0;
                cy += row_h + GAP;
                row_h = 0;
            }
            let r = Rect::new(x + cx, cy, bw, H);
            self.op(cy, cy + H, Op::Card { r, rad: 6, fill: Col::Widget, border: Col::Border });
            self.fill(Rect::new(r.x + 10, cy + 13, 20, 20), Col::AccentSoft, 5);
            self.fill(Rect::new(r.x + 15, cy + 18, 10, 10), Col::Accent, 2);
            let tw = bw - 44 - 10;
            let (alt, aw) = self.fit(&label, SMALL, false, tw);
            let (url, uw2) = self.fit(&im.url, TINY, false, tw);
            self.op(
                cy + 5,
                cy + 5 + LH[SMALL],
                Op::Text { x: r.x + 40, w: aw, text: alt, flags: 0, si: SMALL as u8, lh: LH[SMALL] },
            );
            if !url.is_empty() {
                self.op(
                    cy + 24,
                    cy + 24 + LH[TINY],
                    Op::Text { x: r.x + 40, w: uw2, text: url, flags: F_DIM, si: TINY as u8, lh: LH[TINY] },
                );
            }
            if im.link != 0 {
                self.links.push(LinkHit { x: r.x, y: cy, w: bw, h: H, link: im.link });
            }
            cx += bw + GAP;
            row_h = row_h.max(H);
        }
        let y1 = cy + row_h;
        self.anchor(b, y, y1);
        y1
    }

    /// The footnotes collected at the end of the document: a rule, then `n.` and the definition's
    /// blocks for each label, in reference order. Registers `fn-label` anchors.
    fn foot_section(&mut self, defs: &[(usize, &str, &[Block])], x: i32, w: i32, y: i32) -> i32 {
        self.fill(Rect::new(x, y, w, 1), Col::Border, 0);
        let mut y = y + 14;
        for (n, label, blocks) in defs {
            let num = format!("{n}.");
            let lw = self.m.w(self.tx, &num, false, BODY).ceil() as i32;
            let indent = (lw + 12).max(26);
            let iy = y;
            self.heads.push((format!("fn-{label}"), iy, true));
            // The back-link (the return arrow) sits at the right edge of the definition's first line.
            let back = "\u{21a9}";
            let bw = self.m.w(self.tx, back, false, BODY).ceil() as i32;
            let link = self.urls.id(&format!("#fnref-{label}"));
            let mut ey =
                self.blocks(blocks, x + indent, w - indent - bw - 10, iy, St { depth: 1, tight: Some(4), dim: false });
            self.emit(
                vec![vec![Run { x: (w - bw) as f32, w: bw as f32, text: back.into(), flags: F_LINK, link }]],
                x,
                iy,
                BODY,
                0,
            );
            if blocks.is_empty() {
                ey = iy + LH[BODY];
            }
            self.op(
                iy,
                iy + LH[BODY],
                Op::Text { x: x + indent - 8 - lw, w: lw, text: num, flags: F_DIM, si: BODY as u8, lh: LH[BODY] },
            );
            y = ey + 6;
        }
        y
    }
}

/// Column widths for natural widths `nat` in `avail` px: natural when they fit, otherwise columns
/// below the fair share keep their size and the others split what is left.
fn fit_columns(nat: &[i32], avail: i32) -> Vec<i32> {
    let n = nat.len();
    if nat.iter().sum::<i32>() <= avail {
        return nat.to_vec();
    }
    let min = 36;
    let mut colw = nat.to_vec();
    let mut fixed = vec![false; n];
    let mut rem = avail.max(min * n as i32);
    loop {
        let free = fixed.iter().filter(|f| !**f).count() as i32;
        if free == 0 {
            break;
        }
        let share = (rem / free).max(min);
        let mut changed = false;
        for c in 0..n {
            if !fixed[c] && nat[c] <= share {
                fixed[c] = true;
                rem -= nat[c];
                changed = true;
            }
        }
        if !changed {
            for c in 0..n {
                if !fixed[c] {
                    colw[c] = share;
                }
            }
            break;
        }
    }
    colw
}

// ---------------------------------------------------------------------------------------------
// clipped drawing helpers (Canvas::fill_round & co. only clip to the canvas, not to the pane)

fn round_clip(cv: &mut Canvas, r: Rect, rad: i32, c: Rgba, clip: Rect) {
    cv.fill_round_clip(r, rad, c, clip);
}

fn card_clip(cv: &mut Canvas, r: Rect, rad: i32, fill: Rgba, border: Rgba, clip: Rect) {
    round_clip(cv, r, rad, border, clip);
    round_clip(cv, r.inset(1), (rad - 1).max(0), fill, clip);
}

fn stroke_clip(cv: &mut Canvas, r: Rect, c: Rgba, clip: Rect) {
    cv.fill_clip(Rect::new(r.x, r.y, r.w, 1), c, clip);
    cv.fill_clip(Rect::new(r.x, r.bottom() - 1, r.w, 1), c, clip);
    cv.fill_clip(Rect::new(r.x, r.y, 1, r.h), c, clip);
    cv.fill_clip(Rect::new(r.right() - 1, r.y, 1, r.h), c, clip);
}

/// Thick polyline check mark inside a 14x14 box at (x, y).
fn check_mark(cv: &mut Canvas, x: i32, y: i32, c: Rgba, clip: Rect) {
    let pts = [(3.0f32, 7.5f32), (6.0, 10.5), (11.0, 4.0)];
    for seg in pts.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        for i in 0..=16 {
            let t = i as f32 / 16.0;
            let (px, py) = (x + (a.0 + (b.0 - a.0) * t) as i32, y + (a.1 + (b.1 - a.1) * t) as i32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1)] {
                if clip.contains((px + dx) as f64, (py + dy) as f64) {
                    cv.dot(px + dx, py + dy, c);
                }
            }
        }
    }
}

struct Pal {
    fg: Rgba,
    dim: Rgba,
    accent: Rgba,
    accent_soft: Rgba,
    border: Rgba,
    bar: Rgba,
    widget: Rgba,
    input: Rgba,
    bg: Rgba,
}

impl Pal {
    fn new(t: &Theme) -> Self {
        let bg = t.background.with_alpha(255);
        let fg = t.foreground.with_alpha(255);
        let c = &t.chrome;
        Pal {
            fg,
            dim: fg.mix(bg, 0.4),
            accent: c.accent.with_alpha(255),
            accent_soft: c.accent.with_alpha(70),
            border: c.border.with_alpha(255),
            bar: c.border.with_alpha(255).mix(c.accent, 0.6),
            widget: c.widget_bg.with_alpha(255),
            input: c.input_bg.with_alpha(255),
            bg,
        }
    }
    /// Chip behind inline math: the page tinted with the accent, as long as `text` reads on it.
    fn math_chip(&self, text: Rgba) -> Rgba {
        let c = self.bg.mix(self.accent, 0.13);
        if text.contrast(c) >= 4.5 { c } else { self.input }
    }
    /// Fill of a `$$` block.
    fn math_block(&self) -> Rgba {
        let c = self.widget.mix(self.accent, 0.07);
        if self.fg.contrast(c) >= 4.5 { c } else { self.widget }
    }
    fn get(&self, c: Col) -> Rgba {
        match c {
            Col::Fg => self.fg,
            Col::Dim => self.dim,
            Col::Accent => self.accent,
            Col::AccentSoft => self.accent_soft,
            Col::Border => self.border,
            Col::Bar => self.bar,
            Col::Widget => self.widget,
            Col::Input => self.input,
            Col::Bg => self.bg,
        }
    }
}

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

/// Colored runs of a code block, asked from the host once and cached. Colors that would not read
/// on the block's own background are nudged (the theme's tokens are checked against the editor's).
fn hl_lines(
    cache: &mut HashMap<u64, Option<Rc<HlLines>>>,
    key: u64,
    lang: &str,
    lines: &[String],
    host: &mut Option<&mut HlFn>,
    fill: Rgba,
    bg: Rgba,
) -> Option<Rc<HlLines>> {
    if let Some(v) = cache.get(&key) {
        return v.clone();
    }
    let h = host.as_mut()?;
    let mut res = h(lang, &lines.join("\n"));
    if let Some(runs) = res.as_mut() {
        for line in runs.iter_mut() {
            for r in line.iter_mut() {
                if r.2.contrast(fill) < 4.5 {
                    r.2 = r.2.readable_on(&[fill, bg], 4.5);
                }
            }
        }
    }
    let res = res.map(Rc::new);
    cache.insert(key, res.clone());
    res
}

/// Pieces to draw for `line` (already cut to what is visible) from highlighter `runs`.
fn pieces_of(line: &str, runs: &[(usize, usize, Rgba, u8)], fg: Rgba) -> Vec<(String, Rgba, u8)> {
    let mut out: Vec<(String, Rgba, u8)> = vec![];
    let mut at = 0;
    for &(s, e, c, f) in runs {
        let (s, e) = (s.min(line.len()), e.min(line.len()));
        if s >= e || s < at || !line.is_char_boundary(s) || !line.is_char_boundary(e) {
            continue;
        }
        if s > at {
            out.push((line[at..s].to_string(), fg, 0));
        }
        out.push((line[s..e].to_string(), c, f));
        at = e;
    }
    if at < line.len() {
        out.push((line[at..].to_string(), fg, 0));
    }
    out
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const README: &str = r#"# Lumen Text

[![CI](https://img.shields.io/ci.svg)](https://ci.example.com) ![logo](docs/logo.png)

A *fast*, **native** code editor with ~~no~~ little memory use. See the
[docs](https://example.com/docs) or `cargo run --release`.

## Features

- Syntax highlighting
- Multi cursor editing
  - nested one
  - nested two
    - deeper
- Live markdown preview

1. First
2. Second
3. Third

- [ ] write tests
- [x] ship it

> Quoted wisdom
> continues here

### Install

```rust
fn main() {
    println!("hello");
}
```

| Name | Description | Default |
|:-----|:-----------:|--------:|
| theme | Color theme | dark-plus |
| font | Monospace font family used for the code | `monospace` |

---

## Features

Last paragraph with a hard break
after it.
"#;

    fn assets() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
    }
    fn theme() -> Theme {
        Theme::load(&assets().join("themes/dark-plus.json")).unwrap()
    }
    fn view(src: &str) -> MdView {
        let mut v = MdView::new();
        v.set_text(src);
        v
    }
    fn laid(src: &str, w: i32) -> (MdView, TextCtx) {
        let mut tx = TextCtx::new(None);
        let mut v = view(src);
        v.ensure_layout(&mut tx, w);
        (v, tx)
    }
    fn kinds(v: &MdView) -> Vec<&'static str> {
        v.blocks()
            .iter()
            .map(|b| match &b.kind {
                Kind::Heading { .. } => "h",
                Kind::Para(_) => "p",
                Kind::Code { .. } => "code",
                Kind::Rule => "rule",
                Kind::Quote(_) => "quote",
                Kind::List { .. } => "list",
                Kind::Table { .. } => "table",
                Kind::Images(_) => "img",
                Kind::Math(_) => "math",
                Kind::FootDef { .. } => "foot",
            })
            .collect()
    }
    fn big_doc(lines: usize) -> String {
        let mut s = String::new();
        let mut i = 0;
        while s.lines().count() < lines {
            s.push_str(&format!("## Section {i}\n\nParagraph {i} with **bold** text, a [link](https://x.io/{i}) and `code {i}` plus some more words to wrap around the line a few times over.\n\n- item one {i}\n- item two {i}\n\n```rust\nlet x = {i};\n```\n\n| a | b |\n|---|---|\n| {i} | y |\n\n"));
            i += 1;
        }
        s
    }

    #[test]
    fn parses_block_sequence() {
        let v = view(README);
        assert_eq!(
            kinds(&v),
            ["h", "img", "p", "h", "list", "list", "list", "quote", "h", "code", "table", "rule", "h", "p"]
        );
    }

    #[test]
    fn heading_levels_and_source_lines() {
        let v = view(README);
        let bl = v.blocks();
        let Kind::Heading { level, .. } = &bl[0].kind else { panic!() };
        assert_eq!(*level, 1);
        assert_eq!((bl[0].line, bl[0].end), (0, 0));
        let code = bl.iter().find(|b| matches!(b.kind, Kind::Code { .. })).unwrap();
        let src_line = README.lines().position(|l| l.starts_with("```rust")).unwrap();
        assert_eq!(code.line, src_line);
        assert_eq!(code.end, src_line + 4);
        let hs: Vec<u8> = bl
            .iter()
            .filter_map(|b| if let Kind::Heading { level, .. } = &b.kind { Some(*level) } else { None })
            .collect();
        assert_eq!(hs, [1, 2, 3, 2]);
    }

    #[test]
    fn paragraph_ranges_cover_wrapped_source_lines() {
        let v = view(README);
        let bl = v.blocks();
        let p = &bl[2];
        let start = README.lines().position(|l| l.starts_with("A *fast*")).unwrap();
        assert_eq!((p.line, p.end), (start, start + 1));
    }

    #[test]
    fn inline_styles_are_recorded() {
        let v = view("a *em* **st** ~~del~~ `co` [ln](http://u) z");
        let bl = v.blocks();
        let Kind::Para(inl) = &bl[0].kind else { panic!() };
        let f = |t: &str| {
            inl.iter().find_map(|i| match i {
                Inl::Text { text, flags, link } if text == t => Some((*flags, *link)),
                _ => None,
            })
        };
        assert_eq!(f("em").unwrap().0, F_I);
        assert_eq!(f("st").unwrap().0, F_B);
        assert_eq!(f("del").unwrap().0, F_S);
        assert_eq!(f("co").unwrap().0, F_CODE);
        assert_eq!(f("ln").unwrap(), (F_LINK, 1));
        assert_eq!(v.urls.list, ["http://u"]);
    }

    #[test]
    fn nested_emphasis_combines_flags() {
        let v = view("***both*** and **bold *inner* text**");
        let bl = v.blocks();
        let Kind::Para(inl) = &bl[0].kind else { panic!() };
        assert!(
            inl.iter().any(|i| matches!(i, Inl::Text { text, flags, .. } if text == "both" && *flags == F_B | F_I))
        );
        assert!(
            inl.iter().any(|i| matches!(i, Inl::Text { text, flags, .. } if text == "inner" && *flags == F_B | F_I))
        );
    }

    #[test]
    fn tight_list_items_get_implicit_paragraphs_and_nest() {
        let v = view("- one\n- two\n  - sub a\n  - sub b\n    - deep\n- three\n");
        let bl = v.blocks();
        let Kind::List { items, loose, start } = &bl[0].kind else { panic!() };
        assert!(!loose && start.is_none());
        assert_eq!(items.len(), 3);
        assert!(matches!(items[0].blocks[0].kind, Kind::Para(_)));
        let Kind::List { items: sub, .. } = &items[1].blocks[1].kind else { panic!("no nested list") };
        assert_eq!(sub.len(), 2);
        assert!(matches!(&sub[1].blocks[1].kind, Kind::List { .. }));
        // source lines of the implicit paragraph
        assert_eq!((items[2].blocks[0].line, items[2].blocks[0].end), (5, 5));
    }

    #[test]
    fn loose_and_ordered_lists() {
        let v = view("3. a\n\n4. b\n\n5. c\n");
        let bl = v.blocks();
        let Kind::List { items, loose, start } = &bl[0].kind else { panic!() };
        assert!(*loose);
        assert_eq!(*start, Some(3));
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn task_lists() {
        let v = view("- [ ] todo\n- [x] done\n- plain\n");
        let bl = v.blocks();
        let Kind::List { items, .. } = &bl[0].kind else { panic!() };
        assert_eq!(items.iter().map(|i| i.task).collect::<Vec<_>>(), [Some(false), Some(true), None]);
    }

    #[test]
    fn code_blocks_fenced_and_indented() {
        let v = view("```js {1}\nlet a = 1;\n\nlet b;\n```\n\n    indented\n    code\n");
        let bl = v.blocks();
        let Kind::Code { lang, text } = &bl[0].kind else { panic!() };
        assert_eq!(lang, "js");
        assert_eq!(text, "let a = 1;\n\nlet b;");
        let Kind::Code { lang, text } = &bl[1].kind else { panic!() };
        assert_eq!((lang.as_str(), text.as_str()), ("", "indented\ncode"));
    }

    #[test]
    fn table_is_parsed_with_alignment() {
        let v = view(README);
        let bl = v.blocks();
        let Some(Kind::Table { aligns, head, rows }) =
            bl.iter().find_map(|b| if let Kind::Table { .. } = &b.kind { Some(b.kind.clone()) } else { None })
        else {
            panic!()
        };
        assert_eq!(aligns, [Alignment::Left, Alignment::Center, Alignment::Right]);
        assert_eq!(head.len(), 3);
        assert_eq!(rows.len(), 2);
        assert!(rows[1][2].iter().any(|i| matches!(i, Inl::Text { flags, .. } if *flags == F_CODE)));
    }

    #[test]
    fn images_become_boxes_and_keep_links() {
        let v = view("[![CI](a.svg)](http://ci) ![](b.png)\n\ntext ![inline](c.png) tail\n");
        let bl = v.blocks();
        let Kind::Images(im) = &bl[0].kind else { panic!() };
        assert_eq!(im.len(), 2);
        assert_eq!(im[0].alt, "CI");
        assert_eq!(v.urls.list[im[0].link as usize - 1], "http://ci");
        assert_eq!(im[1].link, 0);
        assert_eq!(kinds(&v), ["img", "p", "img", "p"]);
    }

    #[test]
    fn raw_html_img_and_comments() {
        let v = view(
            "<!-- hidden -->\n\n<p align=\"center\"><a href=\"http://h\"><img src=\"x/logo.png\" alt=\"Logo\"></a></p>\n\nafter <br> br\n",
        );
        let bl = v.blocks();
        let Kind::Images(im) = &bl[0].kind else { panic!("{:?}", kinds(&v)) };
        assert_eq!((im[0].alt.as_str(), im[0].url.as_str()), ("Logo", "x/logo.png"));
        assert_eq!(v.urls.list[im[0].link as usize - 1], "http://h");
        assert_eq!(bl.len(), 2);
    }

    #[test]
    fn hard_and_soft_breaks() {
        let v = view("a  \nb\nc\\\nd");
        let bl = v.blocks();
        let Kind::Para(inl) = &bl[0].kind else { panic!() };
        assert_eq!(inl.iter().filter(|i| **i == Inl::Hard).count(), 2);
        assert_eq!(inl.iter().filter(|i| **i == Inl::Soft).count(), 1);
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("What's new? (v1.2)"), "whats-new-v12");
        assert_eq!(slugify("  Trim me  "), "trim-me");
        assert_eq!(slugify("snake_case-and-kebab"), "snake_case-and-kebab");
        assert_eq!(slugify("C++ & Rust"), "c--rust");
    }

    #[test]
    fn slugify_keeps_unicode_letters() {
        assert_eq!(slugify("Ação e Reação"), "ação-e-reação");
        assert_eq!(slugify("日本語 見出し"), "日本語-見出し");
        assert_eq!(slugify("Über Straße"), "über-straße");
        assert_eq!(slugify("🚀 Launch"), "-launch");
    }

    #[test]
    fn slugify_duplicates_get_suffixes() {
        let mut seen = HashMap::new();
        let s: Vec<String> =
            ["Intro", "Intro", "Intro", "Other", "intro"].iter().map(|h| slugify_unique(h, &mut seen)).collect();
        assert_eq!(s, ["intro", "intro-1", "intro-2", "other", "intro-3"]);
    }

    #[test]
    fn heading_anchors_in_order_with_dedupe() {
        let (v, _tx) = laid(README, 800);
        let a = v.heading_anchors();
        let names: Vec<&str> = a.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(names, ["lumen-text", "features", "install", "features-1"]);
        assert!(a.windows(2).all(|w| w[0].1 < w[1].1));
        assert_eq!(a[0].1, PAD);
    }

    #[test]
    fn paragraph_height_grows_as_width_shrinks() {
        let para = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. ".repeat(6);
        let mut tx = TextCtx::new(None);
        let mut v = view(&para);
        let mut last = 0;
        for w in [1400, 900, 700, 500, 360, 240] {
            v.ensure_layout(&mut tx, w);
            let h = v.content_height();
            assert!(h >= last, "height {h} at width {w} shrank below {last}");
            last = h;
        }
        v.ensure_layout(&mut tx, 1400);
        let wide = v.content_height();
        assert!(last > wide + 22 * 3, "narrow {last} vs wide {wide}");
        // 2 * PAD padding + whole lines of 22 px
        assert_eq!((wide - 2 * PAD) % 22, 0);
    }

    #[test]
    fn long_unbreakable_word_is_split() {
        let word = "x".repeat(400);
        let (v, _) = laid(&word, 300);
        assert!(v.content_height() > 2 * PAD + 22 * 2, "{}", v.content_height());
    }

    #[test]
    fn text_column_is_capped_and_centered() {
        let (v, _) = laid("hello world", 1600);
        let lay = v.lay.as_ref().unwrap();
        assert_eq!(lay.col_w, MAX_COL);
        assert_eq!(lay.col_x, (1600 - MAX_COL) / 2);
        let (v, _) = laid("hello world", 400);
        let lay = v.lay.as_ref().unwrap();
        assert_eq!((lay.col_x, lay.col_w), (PAD, 400 - 2 * PAD));
    }

    #[test]
    fn code_block_does_not_wrap() {
        let long = "let v = ".to_string() + &"1234567890, ".repeat(40);
        let src = format!("```rust\n{long}\nshort\n```\n");
        let (mut v, mut tx) = laid(&src, 1200);
        let h_wide = v.content_height();
        v.ensure_layout(&mut tx, 300);
        assert_eq!(v.content_height(), h_wide);
        // panel = top 18 + 2 lines + bottom 12, plus the document padding
        assert_eq!(h_wide, 2 * PAD + 18 + 2 * line_h() as i32 + 12);
    }

    #[test]
    fn list_nesting_indents_text() {
        let (v, _) = laid("- a\n  - b\n    - c\n", 800);
        let _lay = v.lay.as_ref().unwrap();
        let xs: Vec<i32> = v
            .ops()
            .into_iter()
            .filter_map(|o| if let Op::Text { x, text, .. } = &o.op { Some((text.as_str(), *x)) } else { None })
            .map(|(_, x)| x)
            .collect();
        assert_eq!(xs.len(), 3);
        assert_eq!(xs[1] - xs[0], LIST_BULLET_INDENT);
        assert_eq!(xs[2] - xs[1], LIST_BULLET_INDENT);
        // three bullets of increasing depth, hanging to the left of their text
        let bullets: Vec<(i32, u8)> = v
            .ops()
            .into_iter()
            .filter_map(|o| if let Op::Bullet { cx, level, .. } = o.op { Some((cx, level)) } else { None })
            .collect();
        assert_eq!(bullets.iter().map(|b| b.1).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(bullets.iter().zip(&xs).all(|(b, x)| b.0 < *x));
    }

    #[test]
    fn ordered_list_indent_grows_with_number_width() {
        let items: String = (1..=12).map(|i| format!("{i}. item\n")).collect();
        let (v, _) = laid(&items, 800);
        let (v1, _) = laid("1. item\n2. item\n", 800);
        let x = |v: &MdView| {
            v.ops()
                .into_iter()
                .filter_map(
                    |o| if let Op::Text { x, text, .. } = &o.op { (text == "item").then_some(*x) } else { None },
                )
                .next()
                .unwrap()
        };
        assert!(x(&v) > x(&v1));
    }

    #[test]
    fn table_column_widths_follow_content() {
        let src = "| a | long header here |\n|---|---|\n| short | x |\n| much longer cell text | y |\n";
        let (v, _) = laid(src, 900);
        let lay = v.lay.as_ref().unwrap();
        let verts: Vec<i32> = v
            .ops()
            .into_iter()
            .filter_map(|o| {
                if let Op::Fill { r, .. } = &o.op { (r.w == 1 && r.h == 3 * ROW_H).then_some(r.x) } else { None }
            })
            .collect();
        assert_eq!(verts.len(), 1, "one inner column rule");
        let first = verts[0] - lay.col_x;
        assert!(first > 100, "first column sized by 'much longer cell text', got {first}");
        let stroke =
            v.ops().into_iter().find_map(|o| if let Op::Stroke { r, .. } = &o.op { Some(*r) } else { None }).unwrap();
        assert!(stroke.w < 900 - 2 * PAD);
        assert_eq!(stroke.h, 3 * ROW_H);
    }

    #[test]
    fn narrow_table_shrinks_columns_and_ellipsizes() {
        let src = "| a | b |\n|---|---|\n| aaaaaaaa bbbbbbbb cccccccc dddddddd eeeeeeee | zzzzzzzz yyyyyyyy xxxxxxxx wwwwwwww |\n";
        let (v, _) = laid(src, 260);
        let _lay = v.lay.as_ref().unwrap();
        let stroke =
            v.ops().into_iter().find_map(|o| if let Op::Stroke { r, .. } = &o.op { Some(*r) } else { None }).unwrap();
        assert!(stroke.w <= 260 - 2 * PAD + 2, "table width {}", stroke.w);
        assert!(v.ops().into_iter().any(|o| matches!(&o.op, Op::Text { text, .. } if text.ends_with("..."))));
    }

    #[test]
    fn fit_columns_water_fills() {
        assert_eq!(fit_columns(&[50, 60], 200), [50, 60]);
        assert_eq!(fit_columns(&[50, 400], 200), [50, 150]);
        let c = fit_columns(&[300, 300], 200);
        assert_eq!(c, [100, 100]);
    }

    #[test]
    fn line_mapping_is_monotonic_and_roughly_inverse() {
        let (v, _) = laid(&big_doc(400), 700);
        let n = v.line_count;
        let mut last = -1;
        for l in 0..n {
            let y = v.y_for_source_line(l).unwrap();
            assert!(y >= last, "y_for_source_line not monotonic at {l}: {y} < {last}");
            last = y;
        }
        assert!(last <= v.content_height());
        let mut prev_line = 0;
        for y in (0..v.content_height()).step_by(3) {
            let l = v.source_line_at_y(y);
            assert!(l >= prev_line, "source_line_at_y not monotonic at y={y}: {l} < {prev_line}");
            assert!(l < n);
            prev_line = l;
        }
    }

    #[test]
    fn line_mapping_round_trips_within_tolerance() {
        let (v, _) = laid(&big_doc(300), 640);
        let mut worst = 0usize;
        for l in 0..v.line_count - 1 {
            let y = v.y_for_source_line(l).unwrap();
            let back = v.source_line_at_y(y);
            worst = worst.max(back.abs_diff(l));
        }
        assert!(worst <= 2, "worst round-trip error {worst} lines");
    }

    #[test]
    fn code_block_lines_interpolate_linearly() {
        let body: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let (v, _) = laid(&format!("```\n{body}```\n"), 700);
        let ys: Vec<i32> = (1..=20).map(|l| v.y_for_source_line(l).unwrap()).collect();
        let lh = line_h() as i32;
        for w in ys.windows(2) {
            assert!((w[1] - w[0] - lh).abs() <= 2, "step {} vs line height {lh}", w[1] - w[0]);
        }
    }

    #[test]
    fn mapping_edges() {
        let (v, _) = laid("# T\n\ntext\n", 600);
        assert_eq!(v.y_for_source_line(0), Some(PAD));
        assert_eq!(v.y_for_source_line(10_000), Some(v.content_height()));
        assert_eq!(v.source_line_at_y(-50), 0);
        assert_eq!(v.source_line_at_y(1_000_000), v.line_count - 1);
        assert_eq!(MdView::new().y_for_source_line(3), None);
    }

    #[test]
    fn link_at_hits_and_misses() {
        let mut tx = TextCtx::new(None);
        let mut v = view("[click me](https://example.com/a) and plain text here\n");
        v.ensure_layout(&mut tx, 800);
        let rect = Rect::new(100, 50, 800, 400);
        let _lay = v.lay.as_ref().unwrap();
        let hit = v.all_links()[0];
        let (px, py) = (rect.x as f64 + (hit.x + hit.w / 2) as f64, rect.y as f64 + (hit.y + hit.h / 2) as f64);
        assert_eq!(v.link_at(px, py, rect, 0.0).as_deref(), Some("https://example.com/a"));
        // scrolled down by 7px the same content point sits 7px higher
        assert_eq!(v.link_at(px, py - 7.0, rect, 7.0).as_deref(), Some("https://example.com/a"));
        // plain text to the right of the link, above, below, and outside the pane
        let right = rect.x as f64 + (hit.x + hit.w + 40) as f64;
        assert_eq!(v.link_at(right, py, rect, 0.0), None);
        assert_eq!(v.link_at(px, rect.y as f64 + 2.0, rect, 0.0), None);
        assert_eq!(v.link_at(px, rect.y as f64 + 300.0, rect, 0.0), None);
        assert_eq!(v.link_at(5.0, py, rect, 0.0), None);
    }

    #[test]
    fn badge_image_links_are_clickable() {
        let mut tx = TextCtx::new(None);
        let mut v = view("[![CI](a.svg)](http://ci)\n");
        v.ensure_layout(&mut tx, 800);
        let rect = Rect::new(0, 0, 800, 300);
        let h = v.all_links()[0];
        assert_eq!(v.link_at((h.x + 5) as f64, (h.y + 5) as f64, rect, 0.0).as_deref(), Some("http://ci"));
    }

    #[test]
    fn content_height_is_stable_and_relayout_is_lazy() {
        let mut tx = TextCtx::new(None);
        let t = theme();
        let mut v = view(README);
        let rect = Rect::new(10, 10, 700, 300);
        let mut px = vec![0u32; 720 * 320];
        let mut cv = Canvas { px: &mut px, w: 720, h: 320 };
        v.draw(&mut cv, &mut tx, &t, rect, 0.0);
        let h1 = v.content_height();
        v.draw(&mut cv, &mut tx, &t, rect, 120.0);
        v.draw(&mut cv, &mut tx, &t, rect, 0.0);
        assert_eq!(v.content_height(), h1);
        assert_eq!(v.layout_count(), 1, "scrolling must not re-layout");
        v.set_text(README);
        v.draw(&mut cv, &mut tx, &t, rect, 0.0);
        assert_eq!(v.layout_count(), 1, "same text is a no-op");
        v.set_text(&format!("{README}\nmore\n"));
        v.draw(&mut cv, &mut tx, &t, rect, 0.0);
        assert_eq!(v.layout_count(), 2);
        v.draw(
            &mut cv,
            &mut tx,
            &Theme::load(&assets().join("themes/dark-plus.json")).unwrap(),
            Rect::new(10, 10, 500, 300),
            0.0,
        );
        assert_eq!(v.layout_count(), 3, "width change re-lays out");
    }

    #[test]
    fn empty_document() {
        let mut tx = TextCtx::new(None);
        let mut v = view("");
        v.ensure_layout(&mut tx, 500);
        assert_eq!(v.content_height(), 2 * PAD);
        assert!(v.heading_anchors().is_empty());
        assert_eq!(v.source_line_at_y(10), 0);
    }

    #[test]
    fn ui_width_sized_scales_with_size() {
        let mut tx = TextCtx::new(None);
        let a = tx.ui_width_sized("Hello world", false, 14.0);
        let b = tx.ui_width_sized("Hello world", false, 28.0);
        assert!(a > 20);
        assert!((b as f32 / a as f32 - 2.0).abs() < 0.15, "{a} vs {b}");
        assert!(tx.ui_width_sized(" ", false, 14.0) > 0);
        assert!(tx.ui_width_sized("Hello world", true, 14.0) >= a);
    }

    #[test]
    fn draw_paints_inside_rect_only() {
        let mut tx = TextCtx::new(None);
        let t = theme();
        let (w, h) = (640, 480);
        let sentinel = 0x00ab_cdefu32;
        let mut px = vec![sentinel; (w * h) as usize];
        let rect = Rect::new(40, 30, 500, 380);
        let mut v = view(README);
        {
            let mut cv = Canvas { px: &mut px, w, h };
            v.draw(&mut cv, &mut tx, &t, rect, 0.0);
        }
        let mut inside_changed = 0;
        for y in 0..h {
            for x in 0..w {
                let p = px[(y * w + x) as usize];
                if rect.contains(x as f64, y as f64) {
                    inside_changed += (p != sentinel) as i32;
                } else {
                    assert_eq!(p, sentinel, "pixel outside the rect was touched at ({x},{y})");
                }
            }
        }
        assert_eq!(inside_changed, rect.w * rect.h, "background must fill the whole rect");
        let bg = t.background.with_alpha(255);
        let bgpx = ((bg.0 as u32) << 16) | ((bg.1 as u32) << 8) | bg.2 as u32;
        let ink = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| rect.contains(x as f64, y as f64) && px[(y * w + x) as usize] != bgpx)
            .count();
        assert!(ink > 1500, "only {ink} pixels differ from the background");
        // scrolled far down shows a different picture
        let mut px2 = vec![sentinel; (w * h) as usize];
        let mut cv = Canvas { px: &mut px2, w, h };
        let max_scroll = (v.content_height() - rect.h).max(1) as f64;
        v.draw(&mut cv, &mut tx, &t, rect, max_scroll);
        assert_ne!(px, px2);
    }

    #[test]
    fn draw_clips_to_a_rect_inside_the_canvas_for_every_block_kind() {
        // pane in the middle of the canvas, scrolled so blocks straddle both edges
        let mut tx = TextCtx::new(None);
        let t = theme();
        let (w, h) = (500, 400);
        let sentinel = 0x0011_2233u32;
        let rect = Rect::new(60, 50, 300, 200);
        let mut v = view(README);
        for scroll in [0.0, 37.5, 90.0, 210.0, 333.0] {
            let mut px = vec![sentinel; (w * h) as usize];
            let mut cv = Canvas { px: &mut px, w, h };
            v.draw(&mut cv, &mut tx, &t, rect, scroll);
            for y in 0..h {
                for x in 0..w {
                    if !rect.contains(x as f64, y as f64) {
                        assert_eq!(px[(y * w + x) as usize], sentinel, "leak at ({x},{y}) scroll {scroll}");
                    }
                }
            }
        }
    }

    #[test]
    fn layout_time_for_5000_lines() {
        let src = big_doc(5000);
        assert!(src.lines().count() >= 5000);
        let mut tx = TextCtx::new(None);
        let mut v = MdView::new();
        let t0 = std::time::Instant::now();
        v.set_text(&src);
        let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
        v.ensure_layout(&mut tx, 900);
        let cold = v.last_layout_ms();
        v.ensure_layout(&mut tx, 700);
        let warm = v.last_layout_ms();
        eprintln!(
            "[mdview] 5000 lines ({} KB): parse {parse_ms:.1} ms, layout cold {cold:.1} ms, relayout warm {warm:.1} ms, height {} px",
            src.len() / 1024,
            v.content_height()
        );
        assert!(warm < 1500.0 && cold < 15_000.0);
    }
    #[test]
    fn layout_time_for_5000_lines_of_varied_prose() {
        // ~5000 source lines, thousands of distinct words (worst case for the width cache)
        let mut seed = 12345u64;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let word = |r: usize| {
            let n = 3 + r % 8;
            (0..n).map(|k| (b'a' + ((r / (k + 3) + k * 7) % 26) as u8) as char).collect::<String>()
        };
        let mut src = String::new();
        for i in 0..5000 {
            if i % 40 == 0 {
                src.push_str(&format!("\n## Heading {}\n\n", word(rnd())));
            }
            let n = 8 + rnd() % 14;
            let line: Vec<String> = (0..n).map(|_| word(rnd() % 30_000)).collect();
            src.push_str(&line.join(" "));
            src.push('\n');
        }
        let mut tx = TextCtx::new(None);
        let mut v = MdView::new();
        v.set_text(&src);
        v.ensure_layout(&mut tx, 900);
        let cold = v.last_layout_ms();
        v.ensure_layout(&mut tx, 700);
        let warm = v.last_layout_ms();
        eprintln!(
            "[mdview] varied prose {} KB: layout cold {cold:.0} ms, relayout warm {warm:.0} ms, height {} px",
            src.len() / 1024,
            v.content_height()
        );
        assert!(warm < 1000.0);
    }

    // ---- incremental layout, footnotes, math, tasks, images, highlighting ----

    fn unit(i: usize) -> String {
        format!(
            "## Section {i}\n\nParagraph {i} with **bold** text, a [link](https://x.io/{i}) and `code {i}` plus some more words to wrap around the line a few times over.\n\n- item one {i}\n- item two {i}\n\n```rust\nlet x = {i};\n```\n\n| a | b |\n|---|---|\n| {i} | y |\n\n"
        )
    }

    fn doc_of(units: usize) -> String {
        (0..units).map(unit).collect()
    }

    #[test]
    fn split_cuts_at_blank_lines_but_keeps_fences_lists_and_indents_together() {
        let t = "# T\n\npara one\nstill\n\n```rust\nlet a;\n\nlet b;\n```\n\n- a\n\n- b\n\n  cont\n\nnext para\n\n    code\n\n<!-- a\n\nb -->\n\nlast\n";
        let sp = split(t);
        let segs: Vec<&str> = sp.segs.iter().map(|s| &t[s.b0..s.b1]).collect();
        assert_eq!(
            segs,
            [
                "# T\n",
                "para one\nstill\n",
                "```rust\nlet a;\n\nlet b;\n```\n",
                "- a\n\n- b\n\n  cont\n",
                "next para\n\n    code\n",
                "<!-- a\n\nb -->\n",
                "last\n"
            ]
        );
        for s in &sp.segs {
            assert_eq!(t[..s.b0].matches('\n').count(), s.l0, "first line of {:?}", &t[s.b0..s.b1]);
            assert_eq!(t[s.b0..s.b1].trim_end_matches('\n').matches('\n').count() + 1, s.nl);
        }
        assert_eq!(sp.lines, t.matches('\n').count() + 1);
    }

    #[test]
    fn split_finds_reference_and_footnote_definitions() {
        let sp = split("[ref]: https://a.dev/x \"t\"\n\n[^n]: note\n\n```\n[hidden]: nope\n```\n");
        assert_eq!(sp.ctx.refs.get("ref").map(String::as_str), Some("https://a.dev/x"));
        assert!(sp.ctx.foot.contains("n"));
        assert!(!sp.ctx.refs.contains_key("hidden"), "definitions inside a fence do not count");
    }

    /// Everything hit-testing and scroll sync can see must be the same for an incrementally edited
    /// view and one built from scratch.
    fn assert_same_layout(a: &MdView, b: &MdView, what: &str) {
        assert_eq!(a.content_height(), b.content_height(), "{what}: height");
        assert_eq!(a.heading_anchors(), b.heading_anchors(), "{what}: headings");
        for l in (0..a.line_count).step_by(3) {
            assert_eq!(a.y_for_source_line(l), b.y_for_source_line(l), "{what}: line {l}");
        }
        assert_eq!(a.all_links().len(), b.all_links().len(), "{what}: links");
        for y in (0..a.content_height()).step_by(7) {
            assert_eq!(a.source_line_at_y(y), b.source_line_at_y(y), "{what}: y {y}");
        }
    }

    #[test]
    fn an_edit_reparses_and_relays_out_one_segment_and_matches_a_fresh_layout() {
        let mut tx = TextCtx::new(None);
        let src = doc_of(300);
        let mut v = MdView::new();
        v.set_text(&src);
        v.ensure_layout(&mut tx, 800);
        let (p0, l0) = v.work_counts();
        assert!(p0 >= 1000 && l0 >= 1000, "cold: {p0} parsed, {l0} laid out");
        let edits: Vec<(&str, Box<dyn Fn(&str) -> String>)> = vec![
            ("typing in a paragraph", Box::new(|s| s.replacen("Paragraph 150 with", "Paragraph 150 X with", 1))),
            (
                "making a paragraph longer",
                Box::new(|s| s.replacen("Paragraph 77 with", &format!("Paragraph 77 {} with", "wrap ".repeat(60)), 1)),
            ),
            ("editing a heading", Box::new(|s| s.replacen("## Section 200", "## Section two hundred", 1))),
            (
                "splitting a segment with a blank line",
                Box::new(|s| {
                    s.replacen("- item one 40\n- item two 40", "- item one 40\n\nmiddle text\n\n- item two 40", 1)
                }),
            ),
            (
                "joining two segments",
                Box::new(|s| s.replacen("## Section 9\n\nParagraph 9", "## Section 9\nParagraph 9", 1)),
            ),
        ];
        for (what, f) in edits {
            let new = f(&v.text);
            assert_ne!(new, v.text, "{what}: the edit applies");
            let (p, l) = v.work_counts();
            v.set_text(&new);
            v.ensure_layout(&mut tx, 800);
            let (p2, l2) = v.work_counts();
            assert!(p2 - p <= 3 && l2 - l <= 3, "{what}: {} parsed, {} laid out", p2 - p, l2 - l);
            let mut fresh = MdView::new();
            fresh.set_text(&new);
            fresh.ensure_layout(&mut tx, 800);
            assert_same_layout(&v, &fresh, what);
        }
    }

    #[test]
    fn a_width_change_lays_out_again_but_does_not_reparse() {
        let mut tx = TextCtx::new(None);
        let mut v = MdView::new();
        v.set_text(&doc_of(20));
        v.ensure_layout(&mut tx, 800);
        let (p, l) = v.work_counts();
        v.ensure_layout(&mut tx, 500);
        let (p2, l2) = v.work_counts();
        assert_eq!(p2, p);
        assert!(l2 > l);
        v.ensure_layout(&mut tx, 800);
        assert_eq!(v.work_counts().1, l2 + (l2 - l), "going back re-lays out (one cache slot per segment)");
    }

    /// Median and worst time of one keystroke (rope -> string, `set_text`, draw) in a document of
    /// `units` sections, edited in the middle.
    fn typing_bench(units: usize, label: &str) -> (f64, f64) {
        let mut tx = TextCtx::new(None);
        let t = theme();
        let src = doc_of(units);
        let mut v = MdView::new();
        let t0 = std::time::Instant::now();
        v.set_text(&src);
        v.ensure_layout(&mut tx, 900);
        let cold = t0.elapsed().as_secs_f64() * 1000.0;
        let mut rope = ropey::Rope::from_str(&src);
        let mid = units / 2;
        let at = rope.byte_to_char(src.find(&format!("Paragraph {mid} with")).unwrap() + 14);
        let (w, h) = (900, 700);
        let mut px = vec![0u32; (w * h) as usize];
        let rect = Rect::new(0, 0, w, h);
        let scroll = v.y_for_source_line(mid * 17).unwrap() as f64;
        let mut times = vec![];
        for i in 0..40 {
            let t0 = std::time::Instant::now();
            rope.insert(at + i, "x");
            let text = rope.to_string(); // what the app does with the buffer on every keystroke
            v.set_text(&text);
            let mut cv = Canvas { px: &mut px, w, h };
            v.draw(&mut cv, &mut tx, &t, rect, scroll);
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (median, worst) = (times[times.len() / 2], times[times.len() - 1]);
        eprintln!(
            "[mdview] {label} ({} KB, {} lines): cold {cold:.0} ms; per keystroke median {median:.1} ms, worst {worst:.1} ms",
            src.len() / 1024,
            v.line_count
        );
        drop((v, rope, src, px));
        // Give the memory back: `perf::memory_100k_buffer_and_tokenizer` measures the whole test process.
        unsafe { libc::malloc_trim(0) };
        (median, worst)
    }

    #[test]
    fn typing_in_a_large_document_stays_under_16ms() {
        // 200 KB here (the full 2 MB run below is ignored by default: it inflates the process
        // memory that `perf::memory_100k_buffer_and_tokenizer` measures when tests run in parallel).
        let (median, _) = typing_bench(850, "200 KB");
        if !cfg!(debug_assertions) {
            assert!(median < 16.0, "median keystroke {median:.1} ms");
        }
    }

    /// `cargo test --release typing_in_a_2mb -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn typing_in_a_2mb_document_stays_under_16ms() {
        let (median, _) = typing_bench(9500, "2 MB");
        assert!(median < 16.0, "median keystroke {median:.1} ms");
    }

    #[test]
    fn footnotes_are_numbered_by_first_reference_and_collected_at_the_end() {
        let src = "A[^b] and B[^a] again[^b], unknown[^zz].\n\n[^a]: Alpha note.\n[^b]: Beta note.\n\nTail.\n";
        let (v, _) = laid(src, 800);
        let texts: Vec<String> = v
            .ops()
            .iter()
            .filter_map(|o| if let Op::Text { text, .. } = &o.op { Some(text.clone()) } else { None })
            .collect();
        let all = texts.join("|");
        assert!(all.contains("[1]") && all.contains("[2]"), "{all}");
        assert!(all.contains("[^zz]"), "an undefined reference stays literal: {all}");
        let pos = |s: &str| texts.iter().position(|t| t.contains(s)).unwrap_or_else(|| panic!("{s} not in {all}"));
        assert!(pos("Beta note") < pos("Alpha note"), "definitions follow the numbering: {all}");
        assert!(pos("Tail") < pos("Beta note"), "footnotes come after the body: {all}");
        let names: Vec<String> = v.heading_anchors().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            ["fnref-b", "fnref-a", "fnref-b", "fn-b", "fn-a"],
            "every reference is an anchor for the back-link; the first one wins"
        );
        // the reference links to its definition
        assert!(v.all_links().iter().any(|l| v.urls.get(l.link).is_some_and(|u| u == "#fn-b")));
        assert_eq!(v.blocks().iter().filter(|b| matches!(b.kind, Kind::FootDef { .. })).count(), 2);
    }

    #[test]
    fn footnote_definitions_get_a_back_link_to_the_first_reference() {
        let src = "Intro paragraph.\n\nSecond paragraph cites it[^a] and again[^a].\n\n[^a]: The note.\n";
        let (v, _) = laid(src, 800);
        let arrows: Vec<&Op> = v
            .ops()
            .iter()
            .map(|o| &o.op)
            .filter(|o| matches!(o, Op::Text { text, .. } if text == "\u{21a9}"))
            .collect();
        assert_eq!(arrows.len(), 1, "one return arrow per footnote definition");
        let back = v.all_links().iter().filter(|l| v.urls.get(l.link).is_some_and(|u| u == "#fnref-a")).count();
        assert_eq!(back, 1, "and it is a link");
        let anchors = v.heading_anchors();
        let y = |n: &str| {
            anchors
                .iter()
                .find(|(a, _)| a == n)
                .map(|(_, y)| *y)
                .unwrap_or_else(|| panic!("{n} missing in {anchors:?}"))
        };
        // The link target is the paragraph that mentions the footnote (not the first one), above the definition.
        assert!(y("fnref-a") > PAD && y("fnref-a") < y("fn-a"));
        // A definition nobody references still gets its arrow, but no anchor to land on.
        let (v, _) = laid("Text.\n\n[^z]: Orphan.\n", 800);
        assert!(!v.heading_anchors().iter().any(|(a, _)| a == "fnref-z"));
    }

    #[test]
    fn edited_image_files_relayout_the_preview() {
        let name = "md-edit-me.png";
        let dir = png_file(name, 64, 32, [255, 0, 0, 255]);
        let mut tx = TextCtx::new(None);
        let mut v = view("![x](md-edit-me.png)\n");
        v.set_base_dir(Some(&dir));
        v.ensure_layout(&mut tx, 800);
        let size_of = |v: &MdView| {
            v.ops().iter().find_map(|o| if let Op::Image { r, .. } = &o.op { Some((r.w, r.h)) } else { None })
        };
        assert_eq!(size_of(&v), Some((64, 32)));
        assert!(!v.refresh_images(), "nothing changed on disk");
        png_file(name, 40, 90, [0, 255, 0, 255]);
        assert!(v.refresh_images());
        v.ensure_layout(&mut tx, 800);
        assert_eq!(size_of(&v), Some((40, 90)), "the layout uses the new file's size");
        assert!(!v.refresh_images());
    }

    #[test]
    fn math_inline_and_display() {
        let src = "Euler $e^{i\\pi}+1=0$ costs $5 and $6.\n\n$$\n\\int_0^1 x\\,dx\n$$\n\ntail\n";
        let v = view(src);
        let bl = v.blocks();
        assert_eq!(kinds(&v), ["p", "math", "p"]);
        let Kind::Para(inl) = &bl[0].kind else { panic!() };
        let math: Vec<&str> = inl
            .iter()
            .filter_map(|i| {
                if let Inl::Text { text, flags, .. } = i {
                    (flags & F_MATH != 0).then_some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(math, ["e^{i\\pi}+1=0"], "only the formula is math, the dollar amounts are text");
        let Kind::Math(m) = &bl[1].kind else { panic!() };
        assert_eq!(m, "\\int_0^1 x\\,dx");
        let (v, _) = laid(src, 800);
        assert!(v.ops().iter().any(|o| matches!(&o.op, Op::Code { math: true, .. })));
    }

    #[test]
    fn task_checkboxes_report_the_source_offset_of_their_bracket() {
        let src = "para\n\n- [ ] one\n- [x] two\n\n  - [ ] nested\n";
        let mut tx = TextCtx::new(None);
        let mut v = view(src);
        v.ensure_layout(&mut tx, 800);
        let rect = Rect::new(20, 30, 800, 400);
        let checks = v.all_checks();
        assert_eq!(checks.len(), 3);
        let offs: Vec<usize> = checks.iter().map(|c| c.off).collect();
        let at = |s: &str| src.find(s).unwrap();
        assert_eq!(offs, [at("[ ] one"), at("[x] two"), at("[ ] nested")]);
        for c in checks.clone() {
            let (px, py) = (rect.x as f64 + c.x as f64 + 6.0, rect.y as f64 + c.y as f64 + 8.0);
            assert_eq!(v.check_at(px, py, rect, 0.0), Some(c.off));
            assert_eq!(v.check_at(px + 60.0, py, rect, 0.0), None, "text next to the box is not a hit");
            assert_eq!(v.check_at(px, py - 200.0 + 1.0, rect, 0.0).filter(|o| *o == c.off), None);
        }
        // a click through a scrolled view
        let c = checks[0];
        assert_eq!(
            v.check_at(rect.x as f64 + c.x as f64 + 6.0, rect.y as f64 + c.y as f64 + 8.0 - 5.0, rect, 5.0),
            Some(c.off)
        );
    }

    #[test]
    fn task_flip_swaps_the_mark() {
        use crate::buffer::Buffer;
        let b = Buffer::from_str("- [ ] a\n- [X] b\n- [-] c\n");
        assert_eq!(crate::app_md::task_flip(&b, 2), Some((3, "x")));
        assert_eq!(crate::app_md::task_flip(&b, 10), Some((11, " ")));
        assert_eq!(crate::app_md::task_flip(&b, 18), None);
        assert_eq!(crate::app_md::task_flip(&b, 0), None);
    }

    fn png_file(name: &str, w: u32, h: u32, rgba: [u8; 4]) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-md-images");
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&rgba.repeat((w * h) as usize)).unwrap();
        }
        std::fs::write(dir.join(name), out).unwrap();
        dir
    }

    #[test]
    fn local_images_are_drawn_and_remote_ones_stay_placeholders() {
        let dir = png_file("md-red.png", 64, 32, [255, 0, 0, 255]);
        let src = "![local](md-red.png)\n\n![remote](https://x.io/a.png)\n\n![gone](missing.png)\n\n<img src=\"md-red.png\" width=\"20\">\n";
        let mut tx = TextCtx::new(None);
        let t = theme();
        let mut v = view(src);
        v.set_base_dir(Some(&dir));
        v.ensure_layout(&mut tx, 800);
        let imgs: Vec<Rect> =
            v.ops().iter().filter_map(|o| if let Op::Image { r, .. } = &o.op { Some(*r) } else { None }).collect();
        assert_eq!(imgs.len(), 2, "only the two local files are images");
        assert_eq!((imgs[0].w, imgs[0].h), (64, 32), "natural size");
        assert_eq!((imgs[1].w, imgs[1].h), (20, 10), "width attribute keeps the ratio");
        assert_eq!(
            v.ops().iter().filter(|o| matches!(&o.op, Op::Card { .. })).count(),
            2,
            "remote + missing keep the card"
        );
        // the pixels arrive from a worker thread
        let (w, h) = (900, 500);
        let mut px = vec![0u32; (w * h) as usize];
        let t0 = std::time::Instant::now();
        loop {
            let mut cv = Canvas { px: &mut px, w, h };
            v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0);
            if !v.pending() || t0.elapsed().as_secs() > 10 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0);
        let red = px.iter().filter(|p| **p == 0x00ff0000).count();
        assert!(red >= 64 * 32 + 20 * 10 - 20, "{red} red pixels");
    }

    #[test]
    fn fenced_code_asks_the_highlighter_once_per_block_and_paints_its_colors() {
        let mut tx = TextCtx::new(None);
        let t = theme();
        let src = "```ts\nconst a = 1;\n```\n\n```\nplain\n```\n\n```ts\nconst a = 1;\n```\n";
        let mut v = view(src);
        let (w, h) = (700, 400);
        let mut px = vec![0u32; (w * h) as usize];
        let calls = std::cell::Cell::new(0);
        let mut hl = |lang: &str, text: &str| -> Option<HlLines> {
            calls.set(calls.get() + 1);
            assert_eq!(lang, "ts");
            Some(
                text.split('\n')
                    .map(|l| {
                        vec![
                            (0, l.len().min(5), Rgba(255, 0, 255, 255), 0u8),
                            (l.len().min(5), l.len(), t.foreground, 0u8),
                        ]
                    })
                    .collect(),
            )
        };
        for _ in 0..3 {
            let mut cv = Canvas { px: &mut px, w, h };
            v.draw_with(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0, Some(&mut hl));
        }
        assert_eq!(calls.get(), 1, "identical blocks share one result, and redraws reuse it");
        let magenta = |p: &u32| {
            let (r, g, b) = ((*p >> 16) & 255, (*p >> 8) & 255, *p & 255);
            r > g + 60 && b > g + 60
        };
        assert!(px.iter().filter(|p| magenta(p)).count() > 5, "the colored run reaches the canvas");
        // another theme asks again
        let other = Theme::load(&assets().join("themes/github-light.json")).unwrap();
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw_with(&mut cv, &mut tx, &other, Rect::new(0, 0, w, h), 0.0, Some(&mut hl));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn real_tokenizer_colors_a_fenced_block_like_the_editor() {
        let t = theme();
        let tk = crate::highlight::make_tokenizer(&assets(), "source.ts").unwrap();
        let runs = crate::highlight::highlight_text(&tk, &t, "const a = 1;");
        assert!(runs[0].iter().any(|r| r.2 != t.foreground), "keywords are colored: {:?}", runs[0]);
        assert_eq!(crate::app_md::scope_of(&assets(), "ts").as_deref(), Some("source.ts"));
        assert_eq!(crate::app_md::scope_of(&assets(), "bash").as_deref(), Some("source.shell"));
        assert_eq!(crate::app_md::scope_of(&assets(), "RS").as_deref(), Some("source.rust"));
        assert_eq!(crate::app_md::scope_of(&assets(), "no-such-lang"), None);
    }

    #[test]
    fn reference_style_links_resolve_across_segments() {
        let v = view("see [the docs][d] and [d]\n\n[d]: https://example.com/docs\n");
        let bl = v.blocks();
        let Kind::Para(inl) = &bl[0].kind else { panic!() };
        assert_eq!(inl.iter().filter(|i| matches!(i, Inl::Text { link, .. } if *link != 0)).count(), 2);
        assert_eq!(v.urls.list, ["https://example.com/docs"]);
    }

    #[test]
    #[ignore]
    fn profile_keystroke() {
        let mut tx = TextCtx::new(None);
        let t = theme();
        let src = doc_of(9500);
        let mut v = MdView::new();
        v.set_text(&src);
        v.ensure_layout(&mut tx, 900);
        let mut rope = ropey::Rope::from_str(&src);
        let at = rope.byte_to_char(src.find("Paragraph 4700 with").unwrap() + 14);
        let (w, h) = (900, 700);
        let mut px = vec![0u32; (w * h) as usize];
        let scroll = v.y_for_source_line(4700 * 17).unwrap() as f64;
        let ms = |t0: std::time::Instant| t0.elapsed().as_secs_f64() * 1000.0;
        for i in 0..8 {
            rope.insert(at + i, "x");
            let t0 = std::time::Instant::now();
            let text = rope.to_string();
            let a = ms(t0);
            let t0 = std::time::Instant::now();
            let sp = split(&text);
            let b = ms(t0);
            let t0 = std::time::Instant::now();
            let mut hh = 0u64;
            for s in &sp.segs {
                hh ^= fxhash(&text.as_bytes()[s.b0..s.b1], 0);
            }
            let c = ms(t0);
            let t0 = std::time::Instant::now();
            v.set_text(&text);
            let d = ms(t0);
            let t0 = std::time::Instant::now();
            v.ensure_layout(&mut tx, 900);
            let e = ms(t0);
            let t0 = std::time::Instant::now();
            let mut cv = Canvas { px: &mut px, w, h };
            v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), scroll);
            let f = ms(t0);
            let t0 = std::time::Instant::now();
            let mut cv = Canvas { px: &mut px, w, h };
            v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), scroll);
            let g = ms(t0);
            let t0 = std::time::Instant::now();
            let _ = tx.ui_width_sized(&format!("wordx{i}y"), false, 14.0 * 8.0);
            let k = ms(t0);
            eprintln!(
                "to_string {a:.1}  split {b:.1}  hash {c:.1} ({hh})  set_text(all) {d:.1}  ensure_layout {e:.1}  draw {f:.1} draw2 {g:.1} width_new_word {k:.1} work {:?}",
                v.work_counts()
            );
        }
    }

    #[test]
    #[ignore]
    fn dump_ppm() {
        let mut tx = TextCtx::new(None);
        let t = theme();
        let (w, h) = (900, 1500);
        let mut px = vec![0u32; (w * h) as usize];
        let mut v = view(&std::fs::read_to_string(std::env::var("MD_IN").unwrap()).unwrap());
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0);
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for p in &px {
            out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        std::fs::write(std::env::var("MD_OUT").unwrap(), out).unwrap();
    }
}
