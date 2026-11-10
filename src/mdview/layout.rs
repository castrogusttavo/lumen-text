use super::*;

// ---------------------------------------------------------------------------------------------
// layout output

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Col {
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
pub(super) enum Op {
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
pub(super) struct DrawOp {
    pub(super) y0: i32,
    pub(super) y1: i32,
    pub(super) op: Op,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Anchor {
    pub(super) l0: i64,
    pub(super) l1: i64,
    pub(super) y0: i32,
    pub(super) y1: i32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LinkHit {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) w: i32,
    pub(super) h: i32,
    pub(super) link: u32,
}

/// A task checkbox: its hit box and the byte offset of its `[` (inside the segment).
#[derive(Clone, Copy, Debug)]
pub(super) struct CheckHit {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) w: i32,
    pub(super) h: i32,
    pub(super) off: usize,
}

/// Cache key of a laid-out segment: width, code font size, cell width, footnote numbering, image base.
pub(super) type SegKey = (i32, u32, u32, u64, u32);

/// One segment laid out on its own: ops and hit boxes with y relative to the segment's top (x is
/// absolute: the text column is baked in).
pub(super) struct SegLay {
    pub(super) key: SegKey,
    pub(super) ops: Vec<DrawOp>,
    /// Running max of `y1` over `ops` (sorted by `y0`), to find the first visible op by bisection.
    pub(super) pmax: Vec<i32>,
    pub(super) height: i32,
    /// Anchors with lines relative to the segment's first line.
    pub(super) anchors: Vec<Anchor>,
    pub(super) links: Vec<LinkHit>,
    pub(super) checks: Vec<CheckHit>,
    /// `(name, y, exact)`: headings carry their base slug (deduplicated when flattened), footnotes
    /// an exact `fn-label`.
    pub(super) heads: Vec<(String, i32, bool)>,
    pub(super) max_link_h: i32,
    pub(super) first_before: i32,
    pub(super) last_after: i32,
    /// Nothing to show (blank, only definitions): takes no space and no gap.
    pub(super) empty: bool,
}

/// The whole document: segments stacked, with everything hit-testing and line mapping need
/// flattened into absolute coordinates.
pub(super) struct Layout {
    pub(super) key: (i32, u32, u32, u64, u32, u32),
    pub(super) height: i32,
    /// The segments that take space, in document order (nothing is copied out of them: line
    /// mapping and hit-testing look into the segment that owns the line / point).
    pub(super) segs: Vec<SegRef>,
    /// Left edge and width of the text column.
    pub(super) col_x: i32,
    pub(super) col_w: i32,
}

/// Where one non-empty segment sits: index into `MdView::order`, first source line, line count
/// and y range.
#[derive(Clone, Copy, Debug)]
pub(super) struct SegRef {
    pub(super) ix: usize,
    pub(super) l0: i64,
    pub(super) nl: i64,
    pub(super) y0: i32,
    pub(super) y1: i32,
}

/// Cheap hasher for keys that already are hashes.
#[derive(Default)]
pub(super) struct IdHasher(u64);

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

pub(super) type KeyMap<V> = HashMap<u64, V, std::hash::BuildHasherDefault<IdHasher>>;
pub(super) type KeySet = HashSet<u64, std::hash::BuildHasherDefault<IdHasher>>;

/// Parsed (and maybe laid out) segment, shared by every identical piece of source.
pub(super) struct SegData {
    pub(super) blocks: Vec<Block>,
    /// Footnote labels referenced, in order.
    pub(super) refs: Vec<String>,
    pub(super) lay: Option<SegLay>,
    pub(super) used: u64,
}

/// A segment of the current text and where it sits.
#[derive(Clone, Copy, Debug)]
pub(super) struct Placed {
    pub(super) key: u64,
    pub(super) l0: usize,
    pub(super) nl: usize,
    pub(super) b0: usize,
    pub(super) y0: i32,
    pub(super) y1: i32,
}

// ---------------------------------------------------------------------------------------------
// measuring and inline flow

#[derive(Default)]
pub(super) struct Measure {
    pub(super) maps: Vec<HashMap<String, f32>>,
}

impl Measure {
    /// Width in px with 1/8 px precision (measured at 8x size): per-word integer rounding would
    /// drift by several px over a line.
    pub(super) fn w(&mut self, tx: &mut TextCtx, s: &str, bold: bool, si: usize) -> f32 {
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
pub(super) fn cell_count(c: char) -> usize {
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

pub(super) fn code_w(tx: &TextCtx, s: &str) -> f32 {
    s.chars().map(cell_count).sum::<usize>() as f32 * tx.cell_w
}

#[derive(Clone, Debug)]
pub(super) struct Run {
    pub(super) x: f32,
    pub(super) w: f32,
    pub(super) text: String,
    pub(super) flags: u8,
    pub(super) link: u32,
}

pub(super) struct Atom {
    pub(super) text: String,
    pub(super) flags: u8,
    pub(super) link: u32,
    pub(super) w: f32,
}

pub(super) fn push_piece(line: &mut Vec<Run>, x: f32, text: &str, flags: u8, link: u32, w: f32) {
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

pub(super) struct Flow {
    pub(super) lines: Vec<Vec<Run>>,
    pub(super) x: f32,
    pub(super) chunk: Vec<Atom>,
    pub(super) chunk_w: f32,
    pub(super) space: Option<(u8, u32, f32)>,
    pub(super) width: f32,
    pub(super) wrap: bool,
    pub(super) si: usize,
    pub(super) bold_base: bool,
}

impl Flow {
    pub(super) fn flush(&mut self, m: &mut Measure, tx: &mut TextCtx) {
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
    pub(super) fn split_chunk(&mut self, m: &mut Measure, tx: &mut TextCtx) {
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
pub(super) struct St {
    pub(super) dim: bool,
    pub(super) depth: u8,
    pub(super) tight: Option<i32>,
}

pub(super) struct Lay<'a> {
    pub(super) tx: &'a mut TextCtx,
    pub(super) m: &'a mut Measure,
    pub(super) urls: &'a mut Urls,
    pub(super) imgs: &'a mut ImgStore,
    /// Footnote label -> number.
    pub(super) fnum: &'a HashMap<String, usize>,
    pub(super) ops: Vec<DrawOp>,
    pub(super) anchors: Vec<Anchor>,
    pub(super) links: Vec<LinkHit>,
    pub(super) checks: Vec<CheckHit>,
    pub(super) heads: Vec<(String, i32, bool)>,
}

pub(super) fn before(b: &Block) -> i32 {
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

pub(super) fn after(b: &Block) -> i32 {
    match &b.kind {
        Kind::Heading { .. } => 10,
        _ => 12,
    }
}

impl Lay<'_> {
    pub(super) fn op(&mut self, y0: i32, y1: i32, op: Op) {
        self.ops.push(DrawOp { y0, y1, op });
    }

    pub(super) fn fill(&mut self, r: Rect, col: Col, rad: i32) {
        self.op(r.y, r.bottom(), Op::Fill { r, col, rad });
    }

    /// Register the `fnref-<label>` anchor of every footnote reference in `inl` (laid out at `y`):
    /// the back-link of the footnote returns to the block that first mentions it.
    pub(super) fn ref_anchors(&mut self, inl: &[Inl], y: i32) {
        for item in inl {
            if let Inl::FootRef(label) = item {
                self.heads.push((format!("fnref-{label}"), y, true));
            }
        }
    }

    pub(super) fn anchor(&mut self, b: &Block, y0: i32, y1: i32) {
        let (mut l0, mut l1) = (b.line as i64, b.end as i64);
        if let Some(p) = self.anchors.last() {
            l0 = l0.max(p.l0);
            l1 = l1.max(p.l1);
        }
        self.anchors.push(Anchor { l0, l1: l1.max(l0), y0, y1 });
    }

    /// Flow inline content into lines of runs (x relative to the column).
    pub(super) fn flow(&mut self, inl: &[Inl], width: i32, si: usize, bold_base: bool, wrap: bool) -> Vec<Vec<Run>> {
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
    pub(super) fn ellipsize(&mut self, line: Vec<Run>, max_w: f32, si: usize, bold_base: bool) -> Vec<Run> {
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
    pub(super) fn fit(&mut self, s: &str, si: usize, bold: bool, max_w: i32) -> (String, i32) {
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

    pub(super) fn emit(&mut self, lines: Vec<Vec<Run>>, x: i32, y: i32, si: usize, base: u8) {
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

    pub(super) fn blocks(&mut self, bl: &[Block], x: i32, w: i32, mut y: i32, st: St) -> i32 {
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

    pub(super) fn block(&mut self, b: &Block, x: i32, w: i32, y: i32, st: St) -> i32 {
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
    pub(super) fn list(
        &mut self,
        start: Option<u64>,
        items: &[LItem],
        loose: bool,
        x: i32,
        w: i32,
        y: i32,
        st: St,
    ) -> i32 {
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
    pub(super) fn table(
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

    pub(super) fn images(&mut self, b: &Block, imgs: &[Img], x: i32, w: i32, y: i32) -> i32 {
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
    pub(super) fn foot_section(&mut self, defs: &[(usize, &str, &[Block])], x: i32, w: i32, y: i32) -> i32 {
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
pub(super) fn fit_columns(nat: &[i32], avail: i32) -> Vec<i32> {
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
