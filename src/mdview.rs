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
//! Supported: ATX/setext headings (H1-H6, hairline under H1/H2), paragraphs with word wrap,
//! **bold** (semibold face), *emphasis* (the UI family has no italic face: emphasis is drawn in the
//! text colour mixed toward the accent), ~~strikethrough~~ (line through), `inline code` (monospace
//! chip), links (accent + underline, hit-testable with [`MdView::link_at`]), images (bordered
//! placeholder with alt text and URL, nothing is decoded), bullet / ordered / nested / task lists,
//! block quotes, thematic breaks, fenced and indented code blocks (monospace, never wrapped, clipped
//! horizontally, language label), GFM tables (content-sized columns, ellipsized cells, alignment),
//! hard and soft breaks, and `<img>` / `<br>` / `<a href>` found in raw HTML (other tags are
//! stripped; HTML comments are dropped).
//! Not supported: footnotes, math, definition lists, metadata blocks, admonition quotes, real
//! italics, syntax highlighting inside code blocks, inline images in the middle of a text line
//! (an image ends the paragraph text before it; consecutive images share a row).
#![allow(dead_code)]

use crate::theme::{Rgba, Theme};
use crate::ui::{Canvas, Rect, TextCtx, font_size, line_h};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;

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
    let base = slugify(heading);
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
    Text { text: String, flags: u8, link: u32 },
    Soft,
    Hard,
}

#[derive(Clone, Debug, PartialEq)]
struct Img {
    alt: String,
    url: String,
    link: u32,
}

#[derive(Clone, Debug)]
struct LItem {
    task: Option<bool>,
    blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
enum Kind {
    Heading { level: u8, inl: Vec<Inl>, slug: String },
    Para(Vec<Inl>),
    Code { lang: String, text: String },
    Rule,
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<LItem>, loose: bool },
    Table { aligns: Vec<Alignment>, head: Vec<Vec<Inl>>, rows: Vec<Vec<Vec<Inl>>> },
    Images(Vec<Img>),
}

/// A block with its 0-based source line range (inclusive).
#[derive(Clone, Debug)]
struct Block {
    kind: Kind,
    line: usize,
    end: usize,
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
    Item { blocks: Vec<Block>, task: Option<bool> },
    List { start: Option<u64>, items: Vec<LItem>, loose: bool, l: (usize, usize) },
}

enum Seg {
    Spans(Vec<Inl>),
    Img(Img),
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
    urls: Vec<String>,
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
    seen: HashMap<String, usize>,
}

fn blank(spans: &[Inl]) -> bool {
    spans.iter().all(|s| match s {
        Inl::Text { text, .. } => text.trim().is_empty(),
        _ => true,
    })
}

fn plain(spans: &[Inl]) -> String {
    let mut s = String::new();
    for i in spans {
        match i {
            Inl::Text { text, .. } => s.push_str(text),
            Inl::Soft | Inl::Hard => s.push(' '),
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
            Some(Cont::Quote { blocks, .. }) | Some(Cont::Item { blocks, .. }) => blocks.push(b),
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
        if let Some(Inl::Text { text: t, flags: f, link: l }) = ic.spans.last_mut() {
            if *f == flags && *l == link && flags & F_CODE == 0 {
                t.push_str(text);
                return;
            }
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
                let slug = slugify_unique(&plain(&ic.spans), &mut self.seen);
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
        self.urls.push(url.to_string());
        self.urls.len() as u32
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
            imgs.push(Img { alt: attr(tag, "alt").unwrap_or_default(), url: attr(tag, "src").unwrap_or_default(), link });
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
                if t.starts_with("<br") {
                    if let Some(ic) = self.inl.as_mut() {
                        ic.spans.push(Inl::Hard);
                    }
                }
            }
            Event::Rule => {
                self.close_implicit();
                let l = self.idx.range(&range);
                self.push_block(Block { kind: Kind::Rule, line: l.0, end: l.1 });
            }
            Event::TaskListMarker(done) => {
                if let Some(Cont::Item { task, .. }) = self.stack.last_mut() {
                    *task = Some(done);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag, range: &std::ops::Range<usize>) {
        let l = self.idx.range(range);
        match tag {
            Tag::Paragraph => {
                self.close_implicit();
                let n = self.stack.len();
                if n >= 2 && matches!(self.stack[n - 1], Cont::Item { .. }) {
                    if let Cont::List { loose, .. } = &mut self.stack[n - 2] {
                        *loose = true;
                    }
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
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").trim_matches(|c| c == '{' || c == '}').to_string(),
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
                self.stack.push(Cont::Item { blocks: vec![], task: None });
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
                if let Some(Cont::Item { blocks, task }) = self.stack.pop() {
                    if let Some(Cont::List { items, .. }) = self.stack.last_mut() {
                        items.push(LItem { task, blocks });
                    }
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
                    self.push_block(Block { kind: Kind::Table { aligns: t.aligns, head: t.head, rows: t.rows }, line: t.l.0, end: t.l.1 });
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
                            ic.segs.push(Seg::Img(Img { alt, url, link }));
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
    Text { x: i32, w: i32, text: String, flags: u8, si: u8, lh: i32 },
    Fill { r: Rect, col: Col, rad: i32 },
    Card { r: Rect, rad: i32, fill: Col, border: Col },
    Stroke { r: Rect, col: Col },
    Bullet { cx: i32, cy: i32, level: u8 },
    Check { x: i32, y: i32, done: bool },
    Code { r: Rect, lang: String, lines: Vec<String>, lh: i32, top: i32 },
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

struct Layout {
    key: (i32, u32, u32, u64),
    ops: Vec<DrawOp>,
    /// Running max of `y1` over `ops` (sorted by `y0`), to find the first visible op by bisection.
    pmax: Vec<i32>,
    height: i32,
    anchors: Vec<Anchor>,
    links: Vec<LinkHit>,
    max_link_h: i32,
    heads: Vec<(String, i32)>,
    /// Left edge and width of the text column.
    col_x: i32,
    col_w: i32,
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
    if flags & F_CODE == 0 {
        if let Some(l) = line.last_mut() {
            if l.flags == flags && l.link == link && (l.x + l.w - x).abs() < 0.01 {
                l.text.push_str(text);
                l.w += w;
                return;
            }
        }
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
                let cw = if code { cell_count(ch) as f32 * tx.cell_w } else { m.w(tx, ch.encode_utf8(&mut [0; 4]), bold, self.si) };
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
    ops: Vec<DrawOp>,
    anchors: Vec<Anchor>,
    links: Vec<LinkHit>,
    heads: Vec<(String, i32)>,
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
        let mut f = Flow { lines: vec![vec![]], x: 0.0, chunk: vec![], chunk_w: 0.0, space: None, width: width.max(1) as f32, wrap, si, bold_base };
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
                let cw = if code { cell_count(ch) as f32 * self.tx.cell_w } else { self.m.w(self.tx, ch.encode_utf8(&mut [0; 4]), bold, si) };
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
                let lines = self.flow(inl, w, si, true, true);
                let n = lines.len().max(1) as i32;
                self.heads.push((slug.clone(), y));
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
                let lines = self.flow(inl, w, BODY, false, true);
                let h = lines.len().max(1) as i32 * LH[BODY];
                self.emit(lines, x, y, BODY, if st.dim { F_DIM } else { 0 });
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::Code { lang, text } => {
                let lh = line_h() as i32;
                let lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').replace('\t', "    ")).collect();
                let top = if lang.is_empty() { 12 } else { 18 };
                let h = top + lines.len() as i32 * lh + 12;
                let r = Rect::new(x, y, w, h);
                self.op(y, y + h, Op::Code { r, lang: lang.clone(), lines, lh, top });
                self.anchor(b, y, y + h);
                y + h
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
            } else if start.is_some() {
                let f = F_DIM;
                self.op(iy, iy + LH[BODY], Op::Text { x: x + indent - 8 - lws[i], w: lws[i], text: labels[i].clone(), flags: f, si: BODY as u8, lh: LH[BODY] });
            } else {
                self.op(mid - 4, mid + 4, Op::Bullet { cx: x + indent - 15, cy: mid, level: st.depth % 3 });
            }
            yy = ey;
        }
        yy
    }

    #[allow(clippy::too_many_arguments)]
    fn table(&mut self, b: &Block, aligns: &[Alignment], head: &[Vec<Inl>], rows: &[Vec<Vec<Inl>>], x: i32, w: i32, y: i32) -> i32 {
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
        let (mut cx, mut cy) = (0, y);
        for im in imgs {
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
                cy += H + GAP;
            }
            let r = Rect::new(x + cx, cy, bw, H);
            self.op(cy, cy + H, Op::Card { r, rad: 6, fill: Col::Widget, border: Col::Border });
            self.fill(Rect::new(r.x + 10, cy + 13, 20, 20), Col::AccentSoft, 5);
            self.fill(Rect::new(r.x + 15, cy + 18, 10, 10), Col::Accent, 2);
            let tw = bw - 44 - 10;
            let (alt, aw) = self.fit(&label, SMALL, false, tw);
            let (url, uw2) = self.fit(&im.url, TINY, false, tw);
            self.op(cy + 5, cy + 5 + LH[SMALL], Op::Text { x: r.x + 40, w: aw, text: alt, flags: 0, si: SMALL as u8, lh: LH[SMALL] });
            if !url.is_empty() {
                self.op(cy + 24, cy + 24 + LH[TINY], Op::Text { x: r.x + 40, w: uw2, text: url, flags: F_DIM, si: TINY as u8, lh: LH[TINY] });
            }
            if im.link != 0 {
                self.links.push(LinkHit { x: r.x, y: cy, w: bw, h: H, link: im.link });
            }
            cx += bw + GAP;
        }
        let y1 = cy + H;
        self.anchor(b, y, y1);
        y1
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

/// Markdown preview state: the parsed document plus the cached layout for the last width.
pub struct MdView {
    text: String,
    blocks: Vec<Block>,
    urls: Vec<String>,
    line_count: usize,
    version: u64,
    measure: Measure,
    lay: Option<Layout>,
    layouts: u32,
    last_layout_ms: f64,
}

impl Default for MdView {
    fn default() -> Self {
        Self::new()
    }
}

impl MdView {
    pub fn new() -> Self {
        MdView { text: String::new(), blocks: vec![], urls: vec![], line_count: 1, version: 0, measure: Measure::default(), lay: None, layouts: 0, last_layout_ms: 0.0 }
    }

    /// Replace the document. A no-op when the text is unchanged; otherwise re-parses (the layout is
    /// rebuilt lazily on the next `draw` / `ensure_layout`).
    pub fn set_text(&mut self, text: &str) {
        if self.version > 0 && self.text == text {
            return;
        }
        self.text.clear();
        self.text.push_str(text);
        self.version += 1;
        let idx = LineIndex::new(text);
        self.line_count = idx.starts.len();
        let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        let mut b = Builder {
            idx: &idx,
            urls: vec![],
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
            seen: HashMap::new(),
        };
        for (ev, range) in Parser::new_ext(text, opts).into_offset_iter() {
            b.event(ev, range);
        }
        b.close_implicit();
        while b.stack.len() > 1 {
            // unbalanced input cannot happen with pulldown-cmark, but never lose content
            match b.stack.pop() {
                Some(Cont::Quote { blocks, l }) => b.push_block(Block { kind: Kind::Quote(blocks), line: l.0, end: l.1 }),
                _ => {}
            }
        }
        let Some(Cont::Root(blocks)) = b.stack.pop() else { unreachable!() };
        self.urls = b.urls;
        self.blocks = blocks;
        self.lay = None;
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of full layouts done so far (diagnostics / tests).
    pub fn layout_count(&self) -> u32 {
        self.layouts
    }

    /// Wall time of the last layout in milliseconds.
    pub fn last_layout_ms(&self) -> f64 {
        self.last_layout_ms
    }

    /// Lay the document out for a pane `width` px wide (no-op when the layout for the current
    /// text / width / code font size is already there). `draw` calls this itself.
    pub fn ensure_layout(&mut self, tx: &mut TextCtx, width: i32) {
        let key = (width, font_size().to_bits(), tx.cell_w.to_bits(), self.version);
        if self.lay.as_ref().is_some_and(|l| l.key == key) {
            return;
        }
        let t0 = std::time::Instant::now();
        let col_w = (width - 2 * PAD).min(MAX_COL).max(40);
        let col_x = ((width - col_w) / 2).max(PAD.min(width / 2));
        let mut lay = Lay { tx, m: &mut self.measure, ops: vec![], anchors: vec![], links: vec![], heads: vec![] };
        let y_end = lay.blocks(&self.blocks, col_x, col_w, PAD, St { dim: false, depth: 0, tight: None });
        let height = y_end.max(PAD) + PAD;
        let mut ops = lay.ops;
        ops.sort_by_key(|o| o.y0);
        let mut pmax = Vec::with_capacity(ops.len());
        let mut m = i32::MIN;
        for o in &ops {
            m = m.max(o.y1);
            pmax.push(m);
        }
        let mut anchors = vec![Anchor { l0: -1, l1: -1, y0: 0, y1: 0 }];
        anchors.extend(lay.anchors);
        let n = self.line_count as i64;
        anchors.push(Anchor { l0: n, l1: n, y0: height, y1: height });
        let mut links = lay.links;
        links.sort_by_key(|l| l.y);
        let max_link_h = links.iter().map(|l| l.h).max().unwrap_or(0);
        self.lay = Some(Layout { key, ops, pmax, height, anchors, links, max_link_h, heads: lay.heads, col_x, col_w });
        self.layouts += 1;
        self.last_layout_ms = t0.elapsed().as_secs_f64() * 1000.0;
    }

    /// Paint the preview into `rect` (clipped to it): background, then the part of the document
    /// starting at content y = `scroll_px`.
    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect, scroll_px: f64) {
        let pal = Pal::new(theme);
        let rect = rect.intersect(&cv.bounds());
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        cv.fill_clip(rect, pal.bg, rect);
        self.ensure_layout(tx, rect.w);
        let Some(lay) = self.lay.as_ref() else { return };
        let sc = scroll_px.floor() as i32;
        let (top, bot) = (sc, sc + rect.h);
        let start = lay.pmax.partition_point(|&m| m <= top);
        let (ox, oy) = (rect.x, rect.y - sc);
        let clip = rect;
        let tr = |r: Rect| Rect::new(r.x + ox, r.y + oy, r.w, r.h);
        for d in &lay.ops[start..] {
            if d.y0 >= bot {
                break;
            }
            if d.y1 <= top {
                continue;
            }
            match &d.op {
                Op::Text { x, w, text, flags, si, lh } => {
                    let size = SIZES[*si as usize];
                    let (sx, sy) = (x + ox, d.y0 + oy);
                    let base = if flags & F_DIM != 0 { pal.dim } else { pal.fg };
                    let mut color = base;
                    if flags & F_I != 0 {
                        color = color.mix(pal.accent, 0.45);
                    }
                    if flags & F_LINK != 0 {
                        color = pal.accent;
                    }
                    if flags & F_CODE != 0 {
                        let ch = (*lh - 2).min(line_h() as i32 + 2);
                        let chip = Rect::new(sx, sy + (*lh - ch) / 2, *w, ch);
                        round_clip(cv, chip, 4, pal.input, clip);
                        let cl = line_h() as i32;
                        tx.draw_line(cv, sx + CHIP_PAD, sy + (*lh - cl) / 2, text, color, clip);
                    } else {
                        let off = (*lh - (size * 1.4).round() as i32) / 2;
                        tx.draw_ui_sized(cv, sx, sy + off, text, color, clip, flags & F_B != 0, size);
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
                Op::Card { r, rad, fill, border } => card_clip(cv, tr(*r), *rad, pal.get(*fill), pal.get(*border), clip),
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
                Op::Code { r, lang, lines, lh, top: ctop } => {
                    let sr = tr(*r);
                    card_clip(cv, sr, 6, pal.widget, pal.border, clip);
                    if !lang.is_empty() {
                        let lw = tx.ui_width_sized(lang, false, SIZES[TINY]);
                        tx.draw_ui_sized(cv, sr.right() - lw - 10, sr.y + 3, lang, pal.dim, clip.intersect(&sr), false, SIZES[TINY]);
                    }
                    let inner = Rect::new(sr.x + 1, sr.y + 1, sr.w - 2, sr.h - 2).intersect(&clip);
                    let first = ((top - sr.y - ctop) / lh).max(0) as usize;
                    let max_chars = ((sr.w as f32) / tx.cell_w.max(1.0)) as usize + 2;
                    for (i, line) in lines.iter().enumerate().skip(first) {
                        let ly = sr.y + ctop + i as i32 * lh;
                        if ly >= clip.bottom() {
                            break;
                        }
                        if line.is_empty() {
                            continue;
                        }
                        let s: String;
                        let shown = if line.len() > max_chars {
                            s = line.chars().take(max_chars).collect();
                            s.as_str()
                        } else {
                            line.as_str()
                        };
                        tx.draw_line(cv, sr.x + 14, ly, shown, pal.fg, inner);
                    }
                }
            }
        }
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
        let a = &lay.anchors;
        let line = line as i64;
        let i = a.partition_point(|p| p.l1 < line);
        let Some(p) = a.get(i) else { return Some(lay.height) };
        if p.l0 <= line {
            let n = (p.l1 - p.l0 + 1) as f64;
            let frac = (line - p.l0) as f64 / n;
            return Some(p.y0 + (frac * (p.y1 - p.y0) as f64).round() as i32);
        }
        let q = &a[i.saturating_sub(1)];
        let n = (p.l0 - q.l1 - 1).max(1) as f64;
        let frac = (line - q.l1 - 1) as f64 / n;
        Some(q.y1 + (frac * (p.y0 - q.y1) as f64).round() as i32)
    }

    /// 0-based source line shown at content y (inverse of [`y_for_source_line`](Self::y_for_source_line)).
    pub fn source_line_at_y(&self, y: i32) -> usize {
        let Some(lay) = self.lay.as_ref() else { return 0 };
        let a = &lay.anchors;
        let last = (self.line_count as i64 - 1).max(0);
        if y >= lay.height {
            return last as usize;
        }
        let y = y.max(0);
        let i = a.partition_point(|p| p.y1 <= y);
        let Some(p) = a.get(i) else { return last as usize };
        let line = if p.y0 <= y {
            let n = (p.l1 - p.l0 + 1) as f64;
            let h = (p.y1 - p.y0).max(1) as f64;
            (p.l0 + (((y - p.y0) as f64 + 0.5) / h * n).floor() as i64).min(p.l1)
        } else {
            let q = &a[i.saturating_sub(1)];
            let n = p.l0 - q.l1 - 1;
            if n <= 0 {
                p.l0
            } else {
                let frac = (y - q.y1) as f64 / (p.y0 - q.y1).max(1) as f64;
                (q.l1 + 1 + (frac * n as f64 + 0.5 / (p.y0 - q.y1).max(1) as f64 * n as f64).floor() as i64).min(p.l0 - 1)
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
        let from = lay.links.partition_point(|l| ((l.y + lay.max_link_h) as f64) <= cy);
        for l in &lay.links[from..] {
            if l.y as f64 > cy {
                break;
            }
            if cx >= l.x as f64 && cx < (l.x + l.w) as f64 && cy < (l.y + l.h) as f64 {
                return self.urls.get(l.link as usize - 1).cloned();
            }
        }
        None
    }

    /// `(github-style slug, content y)` of every heading in document order (duplicates get
    /// `-1`, `-2`, ... suffixes), for `#section` links.
    pub fn heading_anchors(&self) -> Vec<(String, i32)> {
        self.lay.as_ref().map_or_else(Vec::new, |l| l.heads.clone())
    }
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
        v.blocks
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
        assert_eq!(kinds(&v), ["h", "img", "p", "h", "list", "list", "list", "quote", "h", "code", "table", "rule", "h", "p"]);
    }

    #[test]
    fn heading_levels_and_source_lines() {
        let v = view(README);
        let Kind::Heading { level, .. } = &v.blocks[0].kind else { panic!() };
        assert_eq!(*level, 1);
        assert_eq!((v.blocks[0].line, v.blocks[0].end), (0, 0));
        let code = v.blocks.iter().find(|b| matches!(b.kind, Kind::Code { .. })).unwrap();
        let src_line = README.lines().position(|l| l.starts_with("```rust")).unwrap();
        assert_eq!(code.line, src_line);
        assert_eq!(code.end, src_line + 4);
        let hs: Vec<u8> = v.blocks.iter().filter_map(|b| if let Kind::Heading { level, .. } = &b.kind { Some(*level) } else { None }).collect();
        assert_eq!(hs, [1, 2, 3, 2]);
    }

    #[test]
    fn paragraph_ranges_cover_wrapped_source_lines() {
        let v = view(README);
        let p = &v.blocks[2];
        let start = README.lines().position(|l| l.starts_with("A *fast*")).unwrap();
        assert_eq!((p.line, p.end), (start, start + 1));
    }

    #[test]
    fn inline_styles_are_recorded() {
        let v = view("a *em* **st** ~~del~~ `co` [ln](http://u) z");
        let Kind::Para(inl) = &v.blocks[0].kind else { panic!() };
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
        assert_eq!(v.urls, ["http://u"]);
    }

    #[test]
    fn nested_emphasis_combines_flags() {
        let v = view("***both*** and **bold *inner* text**");
        let Kind::Para(inl) = &v.blocks[0].kind else { panic!() };
        assert!(inl.iter().any(|i| matches!(i, Inl::Text { text, flags, .. } if text == "both" && *flags == F_B | F_I)));
        assert!(inl.iter().any(|i| matches!(i, Inl::Text { text, flags, .. } if text == "inner" && *flags == F_B | F_I)));
    }

    #[test]
    fn tight_list_items_get_implicit_paragraphs_and_nest() {
        let v = view("- one\n- two\n  - sub a\n  - sub b\n    - deep\n- three\n");
        let Kind::List { items, loose, start } = &v.blocks[0].kind else { panic!() };
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
        let Kind::List { items, loose, start } = &v.blocks[0].kind else { panic!() };
        assert!(*loose);
        assert_eq!(*start, Some(3));
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn task_lists() {
        let v = view("- [ ] todo\n- [x] done\n- plain\n");
        let Kind::List { items, .. } = &v.blocks[0].kind else { panic!() };
        assert_eq!(items.iter().map(|i| i.task).collect::<Vec<_>>(), [Some(false), Some(true), None]);
    }

    #[test]
    fn code_blocks_fenced_and_indented() {
        let v = view("```js {1}\nlet a = 1;\n\nlet b;\n```\n\n    indented\n    code\n");
        let Kind::Code { lang, text } = &v.blocks[0].kind else { panic!() };
        assert_eq!(lang, "js");
        assert_eq!(text, "let a = 1;\n\nlet b;");
        let Kind::Code { lang, text } = &v.blocks[1].kind else { panic!() };
        assert_eq!((lang.as_str(), text.as_str()), ("", "indented\ncode"));
    }

    #[test]
    fn table_is_parsed_with_alignment() {
        let v = view(README);
        let Some(Kind::Table { aligns, head, rows }) = v.blocks.iter().find_map(|b| if let Kind::Table { .. } = &b.kind { Some(b.kind.clone()) } else { None }) else { panic!() };
        assert_eq!(aligns, [Alignment::Left, Alignment::Center, Alignment::Right]);
        assert_eq!(head.len(), 3);
        assert_eq!(rows.len(), 2);
        assert!(rows[1][2].iter().any(|i| matches!(i, Inl::Text { flags, .. } if *flags == F_CODE)));
    }

    #[test]
    fn images_become_boxes_and_keep_links() {
        let v = view("[![CI](a.svg)](http://ci) ![](b.png)\n\ntext ![inline](c.png) tail\n");
        let Kind::Images(im) = &v.blocks[0].kind else { panic!() };
        assert_eq!(im.len(), 2);
        assert_eq!(im[0].alt, "CI");
        assert_eq!(v.urls[im[0].link as usize - 1], "http://ci");
        assert_eq!(im[1].link, 0);
        assert_eq!(kinds(&v), ["img", "p", "img", "p"]);
    }

    #[test]
    fn raw_html_img_and_comments() {
        let v = view("<!-- hidden -->\n\n<p align=\"center\"><a href=\"http://h\"><img src=\"x/logo.png\" alt=\"Logo\"></a></p>\n\nafter <br> br\n");
        let Kind::Images(im) = &v.blocks[0].kind else { panic!("{:?}", kinds(&v)) };
        assert_eq!((im[0].alt.as_str(), im[0].url.as_str()), ("Logo", "x/logo.png"));
        assert_eq!(v.urls[im[0].link as usize - 1], "http://h");
        assert_eq!(v.blocks.len(), 2);
    }

    #[test]
    fn hard_and_soft_breaks() {
        let v = view("a  \nb\nc\\\nd");
        let Kind::Para(inl) = &v.blocks[0].kind else { panic!() };
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
        let s: Vec<String> = ["Intro", "Intro", "Intro", "Other", "intro"].iter().map(|h| slugify_unique(h, &mut seen)).collect();
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
        let lay = v.lay.as_ref().unwrap();
        let xs: Vec<i32> = lay.ops.iter().filter_map(|o| if let Op::Text { x, text, .. } = &o.op { Some((text.as_str(), *x)) } else { None }).map(|(_, x)| x).collect();
        assert_eq!(xs.len(), 3);
        assert_eq!(xs[1] - xs[0], LIST_BULLET_INDENT);
        assert_eq!(xs[2] - xs[1], LIST_BULLET_INDENT);
        // three bullets of increasing depth, hanging to the left of their text
        let bullets: Vec<(i32, u8)> = lay.ops.iter().filter_map(|o| if let Op::Bullet { cx, level, .. } = o.op { Some((cx, level)) } else { None }).collect();
        assert_eq!(bullets.iter().map(|b| b.1).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(bullets.iter().zip(&xs).all(|(b, x)| b.0 < *x));
    }

    #[test]
    fn ordered_list_indent_grows_with_number_width() {
        let items: String = (1..=12).map(|i| format!("{i}. item\n")).collect();
        let (v, _) = laid(&items, 800);
        let (v1, _) = laid("1. item\n2. item\n", 800);
        let x = |v: &MdView| {
            v.lay.as_ref().unwrap().ops.iter().filter_map(|o| if let Op::Text { x, text, .. } = &o.op { (text == "item").then_some(*x) } else { None }).next().unwrap()
        };
        assert!(x(&v) > x(&v1));
    }

    #[test]
    fn table_column_widths_follow_content() {
        let src = "| a | long header here |\n|---|---|\n| short | x |\n| much longer cell text | y |\n";
        let (v, _) = laid(src, 900);
        let lay = v.lay.as_ref().unwrap();
        let verts: Vec<i32> = lay.ops.iter().filter_map(|o| if let Op::Fill { r, .. } = &o.op { (r.w == 1 && r.h == 3 * ROW_H).then_some(r.x) } else { None }).collect();
        assert_eq!(verts.len(), 1, "one inner column rule");
        let first = verts[0] - lay.col_x;
        assert!(first > 100, "first column sized by 'much longer cell text', got {first}");
        let stroke = lay.ops.iter().find_map(|o| if let Op::Stroke { r, .. } = &o.op { Some(*r) } else { None }).unwrap();
        assert!(stroke.w < 900 - 2 * PAD);
        assert_eq!(stroke.h, 3 * ROW_H);
    }

    #[test]
    fn narrow_table_shrinks_columns_and_ellipsizes() {
        let src = "| a | b |\n|---|---|\n| aaaaaaaa bbbbbbbb cccccccc dddddddd eeeeeeee | zzzzzzzz yyyyyyyy xxxxxxxx wwwwwwww |\n";
        let (v, _) = laid(src, 260);
        let lay = v.lay.as_ref().unwrap();
        let stroke = lay.ops.iter().find_map(|o| if let Op::Stroke { r, .. } = &o.op { Some(*r) } else { None }).unwrap();
        assert!(stroke.w <= 260 - 2 * PAD + 2, "table width {}", stroke.w);
        assert!(lay.ops.iter().any(|o| matches!(&o.op, Op::Text { text, .. } if text.ends_with("..."))));
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
        let lay = v.lay.as_ref().unwrap();
        let hit = lay.links[0];
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
        let h = v.lay.as_ref().unwrap().links[0];
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
        v.draw(&mut cv, &mut tx, &Theme::load(&assets().join("themes/dark-plus.json")).unwrap(), Rect::new(10, 10, 500, 300), 0.0);
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
        let ink = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| rect.contains(x as f64, y as f64) && px[(y * w + x) as usize] != bgpx).count();
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
        eprintln!("[mdview] 5000 lines ({} KB): parse {parse_ms:.1} ms, layout cold {cold:.1} ms, relayout warm {warm:.1} ms, height {} px", src.len() / 1024, v.content_height());
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
        eprintln!("[mdview] varied prose {} KB: layout cold {cold:.0} ms, relayout warm {warm:.0} ms, height {} px", src.len() / 1024, v.content_height());
        assert!(warm < 1000.0);
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
