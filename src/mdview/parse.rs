use super::*;

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
pub(super) fn dedupe_slug(base: String, seen: &mut HashMap<String, usize>) -> String {
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
pub(super) enum Inl {
    /// `link` is 0 for none, else index + 1 into `MdView::urls`.
    Text {
        text: String,
        flags: Fl,
        link: u32,
    },
    Soft,
    Hard,
    /// `[^label]`: shown as `[n]`, `n` being the order of first reference in the document.
    FootRef(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Img {
    pub(super) alt: String,
    pub(super) url: String,
    pub(super) link: u32,
    /// `width` / `height` attributes of a raw HTML `<img>`, in px.
    pub(super) size: (Option<i32>, Option<i32>),
    /// A video (`<video>`, or a link / bare URL to a video file): drawn as a card that opens it.
    pub(super) video: bool,
}

/// A cell of a raw HTML `<table>`.
#[derive(Clone, Debug)]
pub(super) struct HCell {
    pub(super) inl: Vec<Inl>,
    /// `colspan` (at least 1).
    pub(super) span: u16,
    /// 0 left, 1 center, 2 right.
    pub(super) align: u8,
    /// `<th>`.
    pub(super) head: bool,
}

/// One term of a definition list with its definitions.
#[derive(Clone, Debug)]
pub(super) struct DefItem {
    pub(super) title: Vec<Inl>,
    pub(super) defs: Vec<Vec<Block>>,
}

#[derive(Clone, Debug)]
pub(super) struct LItem {
    pub(super) task: Option<bool>,
    /// Byte offset (inside the segment) of the `[` of the task marker.
    pub(super) task_off: usize,
    pub(super) blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
pub(super) enum Kind {
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
    /// GitHub alert (`> [!NOTE]`): 0 note, 1 tip, 2 important, 3 warning, 4 caution.
    Alert(u8, Vec<Block>),
    /// `<details>`: `id` keys its open / closed state, `open` is the `open` attribute.
    Details {
        id: u64,
        summary: Vec<Inl>,
        blocks: Vec<Block>,
        open: bool,
    },
    /// Text of a raw HTML block: `align` 0 left / 1 center / 2 right, `head` 1..=6 for `<hN>`.
    HtmlPara {
        align: u8,
        inl: Vec<Inl>,
        head: u8,
    },
    /// Raw HTML `<table>` (rows of cells with `colspan`).
    HtmlTable(Vec<Vec<HCell>>),
    /// `Term` / `: definition`.
    DefList(Vec<DefItem>),
}

/// A block with its 0-based source line range (inclusive).
#[derive(Clone, Debug)]
pub(super) struct Block {
    pub(super) kind: Kind,
    pub(super) line: usize,
    pub(super) end: usize,
}

// ---------------------------------------------------------------------------------------------
// shared tables

/// Link targets of the whole document, interned so that the ids stored in cached segments stay
/// valid when other segments change. Ids are 1-based (0 = no link).
#[derive(Default)]
pub(super) struct Urls {
    pub(super) list: Vec<String>,
    pub(super) map: HashMap<String, u32>,
}

impl Urls {
    pub(super) fn id(&mut self, url: &str) -> u32 {
        if let Some(&i) = self.map.get(url) {
            return i;
        }
        self.list.push(url.to_string());
        let i = self.list.len() as u32;
        self.map.insert(url.to_string(), i);
        i
    }
    pub(super) fn get(&self, id: u32) -> Option<&String> {
        self.list.get((id as usize).checked_sub(1)?)
    }
}

/// 64-bit hash of `bytes` (8 bytes per step), mixed with `seed`.
pub(super) fn fxhash(bytes: &[u8], seed: u64) -> u64 {
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
pub(super) struct Ctx {
    pub(super) refs: HashMap<String, String>,
    pub(super) foot: HashSet<String>,
    pub(super) hash: u64,
}

pub(super) fn norm_label(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

// ---------------------------------------------------------------------------------------------
// segmenting

/// A run of source lines that parses on its own: `[b0, b1)` bytes, `nl` lines from line `l0`.
#[derive(Clone, Copy, Debug)]
pub(super) struct SegSpan {
    pub(super) b0: usize,
    pub(super) b1: usize,
    pub(super) l0: usize,
    pub(super) nl: usize,
}

pub(super) struct Split {
    pub(super) segs: Vec<SegSpan>,
    pub(super) ctx: Ctx,
    /// Number of lines (`\n` count + 1, like [`LineIndex`]).
    pub(super) lines: usize,
}

/// `(columns, bytes)` of leading blanks (a tab goes to the next multiple of 4).
pub(super) fn indent_of(line: &str) -> (usize, usize) {
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

pub(super) fn is_list_marker(rest: &str) -> bool {
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
pub(super) fn fence_open(rest: &str) -> Option<(u8, usize)> {
    let c = *rest.as_bytes().first().filter(|c| **c == b'`' || **c == b'~')?;
    let n = rest.bytes().take_while(|b| *b == c).count();
    (n >= 3 && !(c == b'`' && rest[n..].contains('`'))).then_some((c, n))
}

pub(super) fn fence_close(line: &str, c: u8, n: usize) -> bool {
    let t = line.trim_start();
    let k = t.bytes().take_while(|b| *b == c).count();
    k >= n && t[k..].trim().is_empty()
}

/// Cut `text` into segments at blank lines outside code fences and raw HTML blocks that must stay
/// together. Merging is always safe (a merged segment parses exactly as the document would), so
/// the rules lean towards it: a line indented two columns or more continues the segment, and a
/// list marker continues a segment that starts with a list.
pub(super) fn split(text: &str) -> Split {
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
    // Open `<details>` elements: their content (blank lines included) stays in one segment.
    let mut details = 0usize;
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
            blank_pending = cur.is_some() && details == 0;
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
                let (opens, closes) = (l.matches("<details").count(), l.matches("</details").count());
                details = (details + opens).saturating_sub(closes).min(16);
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

pub(super) struct LineIndex {
    pub(super) starts: Vec<usize>,
}

impl LineIndex {
    pub(super) fn new(s: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(s.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
        Self { starts }
    }
    pub(super) fn line_of(&self, off: usize) -> usize {
        self.starts.partition_point(|&s| s <= off).saturating_sub(1)
    }
    pub(super) fn range(&self, r: &std::ops::Range<usize>) -> (usize, usize) {
        let a = self.line_of(r.start);
        let b = self.line_of(r.end.saturating_sub(1).max(r.start));
        (a, b.max(a))
    }
}

pub(super) enum Cont {
    Root(Vec<Block>),
    Quote { blocks: Vec<Block>, l: (usize, usize), alert: Option<u8> },
    Item { blocks: Vec<Block>, task: Option<bool>, task_off: usize },
    Foot { blocks: Vec<Block>, label: String, l: (usize, usize) },
    List { start: Option<u64>, items: Vec<LItem>, loose: bool, l: (usize, usize) },
    Details { id: u64, summary: Vec<Inl>, blocks: Vec<Block>, l: (usize, usize), open: bool },
    DefList { items: Vec<DefItem>, l: (usize, usize) },
    DefDef { blocks: Vec<Block> },
}

/// The block a finished `>` quote becomes: an alert when it had a `[!KIND]` marker.
pub(super) fn quote_block(blocks: Vec<Block>, l: (usize, usize), alert: Option<u8>) -> Block {
    let kind = match alert {
        Some(k) => Kind::Alert(k, blocks),
        None => Kind::Quote(blocks),
    };
    Block { kind, line: l.0, end: l.1 }
}

pub(super) enum Seg {
    Spans(Vec<Inl>),
    Img(Img),
    Math(String),
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum IK {
    Para,
    Head(u8),
    Cell,
    /// The term of a definition list.
    DefTitle,
}

pub(super) struct InlCtx {
    pub(super) kind: IK,
    pub(super) spans: Vec<Inl>,
    pub(super) segs: Vec<Seg>,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) implicit: bool,
}

#[derive(Default)]
pub(super) struct TableB {
    pub(super) aligns: Vec<Alignment>,
    pub(super) head: Vec<Vec<Inl>>,
    pub(super) rows: Vec<Vec<Vec<Inl>>>,
    pub(super) cur: Vec<Vec<Inl>>,
    pub(super) l: (usize, usize),
}

pub(super) struct Builder<'a> {
    pub(super) idx: &'a LineIndex,
    pub(super) urls: &'a mut Urls,
    /// Footnote labels (lowercase) defined somewhere in the document.
    pub(super) known: &'a HashSet<String>,
    /// Footnote references met in this segment, in order.
    pub(super) refs: Vec<String>,
    pub(super) stack: Vec<Cont>,
    pub(super) inl: Option<InlCtx>,
    pub(super) strong: u32,
    pub(super) em: u32,
    pub(super) strike: u32,
    pub(super) links: Vec<u32>,
    pub(super) code: Option<(String, String, (usize, usize))>,
    pub(super) table: Option<TableB>,
    pub(super) html: Option<(String, (usize, usize))>,
    pub(super) image: Option<(String, String, u32)>,
    /// Open inline styles from raw HTML tags and `^sup^` / `~sub~`: (tag, flag, pushed a link).
    pub(super) hstack: Vec<(String, Fl, bool)>,
}

pub(super) fn blank(spans: &[Inl]) -> bool {
    spans.iter().all(|s| match s {
        Inl::Text { text, .. } => text.trim().is_empty(),
        Inl::FootRef(_) => false,
        _ => true,
    })
}

pub(super) fn plain(spans: &[Inl]) -> String {
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
    pub(super) fn flags(&self) -> (Fl, u32) {
        let mut f = self.hstack.iter().fold(0, |a, h| a | h.1);
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

    pub(super) fn push_block(&mut self, b: Block) {
        match self.stack.last_mut() {
            Some(Cont::Root(v)) => v.push(b),
            Some(Cont::Quote { blocks, .. })
            | Some(Cont::Item { blocks, .. })
            | Some(Cont::Foot { blocks, .. })
            | Some(Cont::Details { blocks, .. })
            | Some(Cont::DefDef { blocks }) => blocks.push(b),
            _ => {}
        }
    }

    /// Close the `<details>` left open inside a Markdown container that is ending.
    pub(super) fn unwind_details(&mut self) {
        while let Some(Cont::Details { .. }) = self.stack.last() {
            if let Some(Cont::Details { id, summary, blocks, l, open }) = self.stack.pop() {
                self.push_block(Block { kind: Kind::Details { id, summary, blocks, open }, line: l.0, end: l.1 });
            }
        }
    }

    pub(super) fn start_inline(&mut self, kind: IK, start: usize, end: usize, implicit: bool) {
        self.inl = Some(InlCtx { kind, spans: vec![], segs: vec![], start, end, implicit });
    }

    pub(super) fn push_span(&mut self, text: &str, extra: Fl) {
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

    pub(super) fn text(&mut self, t: &str, range: &std::ops::Range<usize>) {
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
    pub(super) fn close_implicit(&mut self) {
        if self.inl.as_ref().is_some_and(|i| i.implicit) {
            self.finish_inline();
        }
    }

    pub(super) fn finish_inline(&mut self) {
        self.end_inline_html();
        let Some(mut ic) = self.inl.take() else { return };
        let l = self.idx.range(&(ic.start..ic.end));
        // Emoji shortcodes and bare URLs, once the text events of the paragraph are merged.
        ic.spans = super::inline::rewrite(std::mem::take(&mut ic.spans), self.urls);
        for s in ic.segs.iter_mut() {
            if let Seg::Spans(v) = s {
                *v = super::inline::rewrite(std::mem::take(v), self.urls);
            }
        }
        match ic.kind {
            IK::DefTitle => {
                if let Some(Cont::DefList { items, .. }) = self.stack.last_mut() {
                    items.push(DefItem { title: ic.spans, defs: vec![] });
                }
            }
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
                            // A paragraph that is just a link to a video file becomes a video card.
                            if let Some(v) = super::inline::lone_video(&s, self.urls) {
                                imgs.push(v);
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

    pub(super) fn url_index(&mut self, url: &str) -> u32 {
        self.urls.id(url)
    }

    pub(super) fn event(&mut self, ev: Event, range: std::ops::Range<usize>) {
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
            Event::InlineHtml(h) => self.inline_html(&h, &range),
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

    pub(super) fn start(&mut self, tag: Tag, range: &std::ops::Range<usize>) {
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
            Tag::BlockQuote(kind) => {
                self.close_implicit();
                let alert = kind.map(|k| match k {
                    pulldown_cmark::BlockQuoteKind::Note => 0,
                    pulldown_cmark::BlockQuoteKind::Tip => 1,
                    pulldown_cmark::BlockQuoteKind::Important => 2,
                    pulldown_cmark::BlockQuoteKind::Warning => 3,
                    pulldown_cmark::BlockQuoteKind::Caution => 4,
                });
                self.stack.push(Cont::Quote { blocks: vec![], l, alert });
            }
            Tag::DefinitionList => {
                self.close_implicit();
                self.stack.push(Cont::DefList { items: vec![], l });
            }
            Tag::DefinitionListTitle => {
                self.close_implicit();
                self.start_inline(IK::DefTitle, range.start, range.end, false);
            }
            Tag::DefinitionListDefinition => {
                self.close_implicit();
                self.stack.push(Cont::DefDef { blocks: vec![] });
            }
            Tag::Superscript => self.hstack.push(("^sup".into(), F_SUP, false)),
            Tag::Subscript => self.hstack.push(("^sub".into(), F_SUB, false)),
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
            Tag::Link { link_type, dest_url, .. } => {
                // `<a@b.c>` comes without its scheme.
                let i = if link_type == LinkType::Email && !dest_url.contains(':') {
                    self.url_index(&format!("mailto:{dest_url}"))
                } else {
                    self.url_index(&dest_url)
                };
                self.links.push(i);
            }
            Tag::Image { dest_url, .. } => {
                let link = self.links.last().copied().unwrap_or(0);
                self.image = Some((String::new(), dest_url.to_string(), link));
            }
            _ => {}
        }
    }

    pub(super) fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell => self.finish_inline(),
            TagEnd::BlockQuote(_) => {
                self.close_implicit();
                self.unwind_details();
                if let Some(Cont::Quote { blocks, l, alert }) = self.stack.pop() {
                    self.push_block(quote_block(blocks, l, alert));
                }
            }
            TagEnd::DefinitionListTitle => self.finish_inline(),
            TagEnd::DefinitionListDefinition => {
                self.close_implicit();
                self.unwind_details();
                if let Some(Cont::DefDef { blocks }) = self.stack.pop()
                    && let Some(Cont::DefList { items, .. }) = self.stack.last_mut()
                {
                    match items.last_mut() {
                        Some(it) => it.defs.push(blocks),
                        None => items.push(DefItem { title: vec![], defs: vec![blocks] }),
                    }
                }
            }
            TagEnd::DefinitionList => {
                self.close_implicit();
                if let Some(Cont::DefList { items, l }) = self.stack.pop() {
                    self.push_block(Block { kind: Kind::DefList(items), line: l.0, end: l.1 });
                }
            }
            TagEnd::Superscript => self.pop_style("^sup"),
            TagEnd::Subscript => self.pop_style("^sub"),
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
                self.unwind_details();
                if let Some(Cont::List { start, items, loose, l }) = self.stack.pop() {
                    self.push_block(Block { kind: Kind::List { start, items, loose }, line: l.0, end: l.1 });
                }
            }
            TagEnd::Item => {
                self.close_implicit();
                self.unwind_details();
                if let Some(Cont::Item { blocks, task, task_off }) = self.stack.pop()
                    && let Some(Cont::List { items, .. }) = self.stack.last_mut()
                {
                    items.push(LItem { task, task_off, blocks });
                }
            }
            TagEnd::FootnoteDefinition => {
                self.close_implicit();
                self.unwind_details();
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
                    let video = super::inline::is_video(&url);
                    self.push_img(Img { alt, url, link, size: (None, None), video });
                }
            }
            _ => {}
        }
    }
}

/// Value of `name="..."` (or single quoted) inside an HTML tag.
pub(super) fn attr(tag: &str, name: &str) -> Option<String> {
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

pub(super) fn strip_tags(s: &str) -> String {
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
