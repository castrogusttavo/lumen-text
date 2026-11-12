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
    Rule,
    Bar,
    Code,
    Head,
    Widget,
    Input,
    Bg,
    Text,
    Faint,
    /// GitHub alert color (0 note, 1 tip, 2 important, 3 warning, 4 caution) and its soft tint.
    Alert(u8),
    AlertBg(u8),
}

#[derive(Debug)]
pub(super) enum Op {
    Text {
        x: i32,
        w: i32,
        text: String,
        flags: Fl,
        si: u8,
        lh: i32,
        /// Link id (0 = none), for the hover highlight.
        link: u32,
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
    /// Code block (`math` = the source of a `$$` block that failed to typeset: centered, tinted,
    /// `err` = (line, byte range) to show in the error color). `key` identifies lang + text for the
    /// highlight cache.
    Code {
        r: Rect,
        lang: String,
        lines: Vec<String>,
        lh: i32,
        top: i32,
        key: u64,
        math: bool,
        err: Option<(usize, usize, usize)>,
    },
    /// A typeset formula: left edge `x`, width `w`, baseline `base`; `row` is the top of the text
    /// line it sits in (or of its display block); `src` is its TeX source (what a copy yields).
    Math {
        x: i32,
        w: i32,
        base: i32,
        row: i32,
        lay: Rc<math::MathLay>,
        src: String,
        flags: Fl,
        display: bool,
    },
    /// A local image drawn at `r` (logical px); `id` is its [`ImgStore`] id.
    Image {
        r: Rect,
        id: u32,
    },
    /// UI text in a palette color (alert titles, captions, notes).
    Label {
        x: i32,
        w: i32,
        text: String,
        col: Col,
        si: u8,
        lh: i32,
        bold: bool,
    },
    /// An icon from the bundled set (`ui/...`) in a `size` px box.
    Icon {
        x: i32,
        y: i32,
        size: i32,
        name: &'static str,
        col: Col,
    },
    /// A tint drawn only while the pointer is inside `r` (a clickable row).
    Hover {
        r: Rect,
        rad: i32,
    },
    /// A button drawn while the pointer is inside `area` (the block it belongs to).
    Button {
        r: Rect,
        area: Rect,
        kind: Btn,
    },
    /// A Mermaid diagram scaled into `r`.
    Diagram {
        r: Rect,
        key: u64,
        dia: Rc<mermaid::Diagram>,
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
    /// Buttons and toggles (details summaries, Copy, Mermaid code / diagram).
    pub(super) hits: Vec<Hit>,
    /// Blocks whose buttons show on hover, with a key identifying them.
    pub(super) areas: Vec<(Rect, u64)>,
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
    /// Baseline of a line of text below its line top, per size index (0 = not measured yet), for
    /// the font generation `base_gen`.
    pub(super) base: [f32; 11],
    pub(super) base_gen: u32,
}

impl Measure {
    /// Baseline of a text line of size index `si`, px below the line's top.
    pub(super) fn baseline(&mut self, tx: &mut TextCtx, si: usize) -> f32 {
        let g = crate::fontcfg::generation();
        if g != self.base_gen {
            self.base = [0.0; 11];
            self.base_gen = g;
        }
        if self.base[si] == 0.0 {
            let size = SIZES[si];
            let off = (LH[si] - (size * 1.4).round() as i32) / 2;
            self.base[si] = (off as f32 + tx.ui_baseline_sized(size)).max(1.0);
        }
        self.base[si]
    }

    /// Width in px with 1/8 px precision (measured at 8x size): per-word integer rounding would
    /// drift by several px over a line.
    pub(super) fn w(&mut self, tx: &mut TextCtx, s: &str, bold: bool, si: usize) -> f32 {
        if self.maps.is_empty() {
            self.maps = (0..SIZES.len() * 2).map(|_| HashMap::new()).collect();
        }
        // Size indices that share a font size share their widths.
        let si = SIZES.iter().position(|&s| s == SIZES[si]).unwrap_or(si);
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
    pub(super) flags: Fl,
    pub(super) link: u32,
}

pub(super) struct Atom {
    pub(super) text: String,
    pub(super) flags: Fl,
    pub(super) link: u32,
    pub(super) w: f32,
}

/// Typeset inline math (one unbreakable piece whose text is the TeX source).
pub(super) fn is_math(flags: Fl) -> bool {
    flags & F_MATH != 0 && flags & F_CODE == 0
}

/// Measures `\text{..}` runs of formulas with the preview's body font.
pub(super) struct UiMeasure<'a> {
    pub(super) tx: &'a mut TextCtx,
}

impl math::TextMeasure for UiMeasure<'_> {
    fn text(&mut self, s: &str, size: f32, bold: bool) -> math::TextMetrics {
        let w = self.tx.ui_width_sized(s, bold, size * 8.0) as f32 / 8.0;
        let bl = self.tx.ui_baseline_sized(size);
        math::TextMetrics { w, asc: size * 0.75, desc: size * 0.22, bl }
    }
}

/// Right edge of a laid-out line (once right-to-left text is reordered, the last run is not
/// necessarily the rightmost).
pub(super) fn line_right(line: &[Run]) -> f32 {
    line.iter().map(|r| r.x + r.w).fold(0.0, f32::max)
}

pub(super) fn push_piece(line: &mut Vec<Run>, x: f32, text: &str, flags: Fl, link: u32, w: f32) {
    if flags & (F_CODE | F_KBD | F_MATH) == 0
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
    pub(super) space: Option<(Fl, u32, f32)>,
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
            if is_math(a.flags) {
                // a formula never breaks: it moves to the next line whole
                if self.x > 0.0 && self.x + a.w > self.width {
                    self.lines.push(vec![]);
                    self.x = 0.0;
                }
                push_piece(self.lines.last_mut().unwrap(), self.x, &a.text, a.flags, a.link, a.w);
                self.x += a.w;
                continue;
            }
            let code = a.flags & F_CODE != 0;
            let kbd = a.flags & F_KBD != 0;
            let pad = if code {
                CHIP_PAD as f32
            } else if kbd {
                KBD_PAD as f32
            } else {
                0.0
            };
            let bold = (self.bold_base || a.flags & F_B != 0) && !kbd;
            let csi = run_si(a.flags, self.si);
            let (mut buf, mut bw) = (String::new(), 0.0f32);
            for ch in a.text.chars() {
                let cw = if code {
                    cell_count(ch) as f32 * tx.cell_w
                } else {
                    m.w(tx, ch.encode_utf8(&mut [0; 4]), bold, csi)
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
    pub(super) math: &'a mut math::MathCache,
    /// Footnote label -> number.
    pub(super) fnum: &'a HashMap<String, usize>,
    pub(super) ext: &'a mut Ext,
    pub(super) ops: Vec<DrawOp>,
    pub(super) anchors: Vec<Anchor>,
    pub(super) links: Vec<LinkHit>,
    pub(super) checks: Vec<CheckHit>,
    pub(super) heads: Vec<(String, i32, bool)>,
    pub(super) hits: Vec<Hit>,
    pub(super) areas: Vec<(Rect, u64)>,
}

/// Space a block wants above it: headings open a section (the block gap plus ~1rem).
pub(super) fn before(b: &Block) -> i32 {
    match &b.kind {
        Kind::Heading { level, .. } | Kind::HtmlPara { head: level @ 1..=6, .. } => {
            if *level <= 2 {
                BLOCK_GAP + 16
            } else {
                BLOCK_GAP + 12
            }
        }
        _ => 0,
    }
}

/// Space a block wants below it: a heading stays close to the text it introduces.
pub(super) fn after(b: &Block) -> i32 {
    match &b.kind {
        Kind::Heading { .. } | Kind::HtmlPara { head: 1..=6, .. } => 8,
        _ => BLOCK_GAP,
    }
}

/// Line height of code blocks: the editor's monospace size on an airier ~1.7 line.
pub(super) fn code_lh() -> i32 {
    ((font_size() * 1.7).round() as i32).max(line_h() as i32)
}

/// Line, and byte range inside it, of the bytes `at` of a multi-line formula source.
pub(super) fn err_line(text: &str, at: (usize, usize)) -> Option<(usize, usize, usize)> {
    let a = at.0.min(text.len().saturating_sub(1));
    let mut off = 0;
    for (i, l) in text.split('\n').enumerate() {
        let end = off + l.len();
        if a <= end {
            return Some((i, a - off, at.1.clamp(a, end) - off));
        }
        off = end + 1;
    }
    None
}

impl Lay<'_> {
    /// Inline formula `src` typeset for text of size index `si` (`None`: it does not parse).
    pub(super) fn math_inline(&mut self, src: &str, si: usize) -> Option<Rc<math::MathLay>> {
        let size = SIZES[si] * INLINE_MATH_SCALE;
        self.math.get(src, size, false, &mut UiMeasure { tx: &mut *self.tx }).ok()
    }

    /// Display formula `src` for a column `w` px wide: shrunk (down to [`MIN_MATH_SHRINK`]) when
    /// it is wider than the column.
    pub(super) fn math_display(&mut self, src: &str, w: i32) -> Result<Rc<math::MathLay>, math::MathErr> {
        let size = SIZES[BODY] * DISPLAY_MATH_SCALE;
        let ml = self.math.get(src, size, true, &mut UiMeasure { tx: &mut *self.tx })?;
        if ml.w <= w as f32 || ml.w <= 0.0 {
            return Ok(ml);
        }
        let k = (w as f32 / ml.w).max(MIN_MATH_SHRINK);
        // quarter-pixel sizes keep the cache keys stable across small width changes
        let small = ((size * k * 4.0).floor() / 4.0).max(1.0);
        self.math.get(src, small, true, &mut UiMeasure { tx: &mut *self.tx })
    }

    /// (space added above the text box, total height) of each line: a line grows when inline
    /// math on it is taller or deeper than the text.
    pub(super) fn line_mets(&mut self, lines: &[Vec<Run>], si: usize) -> Vec<(i32, i32)> {
        let lh = LH[si];
        let mut out = Vec::with_capacity(lines.len());
        for line in lines {
            let (mut asc, mut desc) = (0.0f32, 0.0f32);
            for r in line {
                if is_math(r.flags)
                    && let Some(ml) = self.math_inline(&r.text, si)
                {
                    asc = asc.max(ml.h);
                    desc = desc.max(ml.d);
                }
            }
            if asc == 0.0 && desc == 0.0 {
                out.push((0, lh));
                continue;
            }
            let b = self.m.baseline(self.tx, si);
            let top = (asc + 2.0 - b).ceil().max(0.0) as i32;
            let bot = (desc + 2.0 - (lh as f32 - b)).ceil().max(0.0) as i32;
            out.push((top, lh + top + bot));
        }
        out
    }

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
        let (mut lf, mut ll): (Fl, u32) = (0, 0);
        for item in inl {
            match item {
                Inl::Text { text, flags, link } => {
                    lf = *flags & !(F_CODE | F_KBD | F_MARK | F_MATH | F_ERR);
                    ll = *link;
                    if flags & F_MATH != 0 {
                        let (fl, w) = match self.math_inline(text, si) {
                            Some(ml) => (*flags & !F_CODE, ml.w),
                            None => (*flags | F_CODE | F_ERR, code_w(self.tx, text) + 2.0 * CHIP_PAD as f32),
                        };
                        f.chunk.push(Atom { text: text.clone(), flags: fl, link: *link, w });
                        f.chunk_w += w;
                        continue;
                    }
                    if flags & F_CODE != 0 {
                        let w = code_w(self.tx, text) + 2.0 * CHIP_PAD as f32;
                        f.chunk.push(Atom { text: text.clone(), flags: *flags, link: *link, w });
                        f.chunk_w += w;
                        continue;
                    }
                    if flags & F_KBD != 0 {
                        let t = text.trim();
                        if !t.is_empty() {
                            let w = self.m.w(self.tx, t, false, SMALL) + 2.0 * KBD_PAD as f32;
                            f.chunk.push(Atom { text: t.to_string(), flags: *flags, link: *link, w });
                            f.chunk_w += w;
                        }
                        continue;
                    }
                    let bold = bold_base || flags & F_B != 0;
                    let si = run_si(*flags, si);
                    let mut rest = text.as_str();
                    while !rest.is_empty() {
                        let is_sp = rest.starts_with(char::is_whitespace);
                        let end = rest.find(|c: char| c.is_whitespace() != is_sp).unwrap_or(rest.len());
                        let (tok, r) = rest.split_at(end);
                        rest = r;
                        if is_sp {
                            f.flush(self.m, self.tx);
                            f.space = Some((*flags & !(F_CODE | F_MARK), *link, self.m.w(self.tx, " ", bold, si)));
                        } else {
                            let w = self.m.w(self.tx, tok, bold, si);
                            f.chunk.push(Atom { text: tok.to_string(), flags: *flags, link: *link, w });
                            f.chunk_w += w;
                        }
                    }
                }
                Inl::FootRef(label) => {
                    // A superscript number, like GitHub.
                    let text = self.fnum.get(label).copied().unwrap_or(0).to_string();
                    let link = self.urls.id(&format!("#fn-{label}"));
                    let w = self.m.w(self.tx, &text, bold_base, SUP) + 2.0;
                    f.chunk.push(Atom { text, flags: F_LINK | F_SUP, link, w });
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
        if wrap {
            self.bidi_lines(&mut lines, width.max(1) as f32, si, bold_base);
        }
        lines
    }

    /// Right-to-left text in a wrapped block: the runs of each line (split into words and spaces)
    /// are put in visual order (UAX #9 over the words, see `bidi::place`), and a block whose first
    /// strong char is right-to-left is right-aligned in `width`. Blocks without RTL text are left
    /// untouched (one byte scan).
    pub(super) fn bidi_lines(&mut self, lines: &mut [Vec<Run>], width: f32, si: usize, bold_base: bool) {
        if !lines.iter().flatten().any(|r| !is_math(r.flags) && crate::bidi::has_rtl(&r.text)) {
            return;
        }
        let rtl = lines
            .iter()
            .flatten()
            .filter(|r| !is_math(r.flags))
            .find_map(|r| crate::bidi::first_strong_rtl(&r.text))
            .unwrap_or(false);
        for line in lines.iter_mut() {
            let mut units: Vec<Run> = Vec::with_capacity(line.len());
            for r in line.drain(..) {
                if r.flags & (F_CODE | F_KBD | F_MATH) != 0 || !r.text.contains(' ') {
                    units.push(r);
                    continue;
                }
                let bold = bold_base || r.flags & F_B != 0;
                let rsi = run_si(r.flags, si);
                let mut rest = r.text.as_str();
                while !rest.is_empty() {
                    let sp = rest.starts_with(' ');
                    let end = rest.find(|c: char| (c == ' ') != sp).unwrap_or(rest.len());
                    let (tok, tail) = rest.split_at(end);
                    rest = tail;
                    let w = self.m.w(self.tx, tok, bold, rsi);
                    units.push(Run { x: 0.0, w, text: tok.to_string(), flags: r.flags, link: r.link });
                }
            }
            let widths: Vec<f32> = units.iter().map(|u| u.w).collect();
            let xs = {
                let texts: Vec<&str> =
                    units.iter().map(|u| if is_math(u.flags) { "\u{FFFC}" } else { u.text.as_str() }).collect();
                crate::bidi::place(&texts, &widths, rtl)
            };
            let total: f32 = widths.iter().sum();
            let off = if rtl { (width - total).max(0.0) } else { 0.0 };
            for (u, x) in units.iter_mut().zip(xs) {
                u.x = x + off;
            }
            *line = units;
        }
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

    /// Emit the lines (text runs and inline formulas) from `y`; returns their total height (a line
    /// with tall math is taller than `LH[si]`, its text staying on the common baseline).
    pub(super) fn emit(&mut self, lines: Vec<Vec<Run>>, x: i32, y: i32, si: usize, base: Fl) -> i32 {
        let lh = LH[si];
        let mets = self.line_mets(&lines, si);
        let mut ly = y;
        for (line, (top, total)) in lines.into_iter().zip(mets) {
            let ty = ly + top;
            for r in line {
                let rx = (x as f32 + r.x).round() as i32;
                let rw = ((x as f32 + r.x + r.w).round() as i32 - rx).max(1);
                if r.link != 0 {
                    self.links.push(LinkHit { x: rx, y: ly, w: rw, h: total, link: r.link });
                }
                if is_math(r.flags)
                    && let Some(ml) = self.math_inline(&r.text, si)
                {
                    let b = ty + self.m.baseline(self.tx, si).round() as i32;
                    let op = Op::Math {
                        x: rx,
                        w: rw,
                        base: b,
                        row: ty,
                        lay: ml,
                        src: r.text,
                        flags: r.flags | base,
                        display: false,
                    };
                    self.op(ly, ly + total, op);
                    continue;
                }
                let link = r.link;
                let op = Op::Text { x: rx, w: rw, text: r.text, flags: r.flags | base, si: si as u8, lh, link };
                self.op(ty, ty + lh, op);
            }
            ly += total;
        }
        ly - y
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
                self.heads.push((slug.clone(), y, false));
                let base = F_B | if *level == 6 || st.dim { F_DIM } else { 0 };
                let h = self.emit(lines, x, y, si, base).max(LH[si]);
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::Para(inl) => {
                self.ref_anchors(inl, y);
                let lines = self.flow(inl, w, BODY, false, true);
                let h = self.emit(lines, x, y, BODY, if st.dim { F_DIM } else { 0 }).max(LH[BODY]);
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::Code { lang, text } if lang.eq_ignore_ascii_case("mermaid") => self.mermaid(b, text, x, w, y),
            Kind::Code { lang, text } => {
                let lh = code_lh();
                let lines: Vec<String> =
                    text.split('\n').map(|l| l.trim_end_matches('\r').replace('\t', "    ")).collect();
                let top = CODE_PAD_Y;
                let h = top + lines.len() as i32 * lh + CODE_PAD_Y;
                let r = Rect::new(x, y, w, h);
                let key = fxhash(text.as_bytes(), fxhash(lang.as_bytes(), 3));
                self.op(y, y + h, Op::Code { r, lang: lang.clone(), lines, lh, top, key, math: false, err: None });
                self.copy_button(r, key);
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::Math(text) => {
                let err = match self.math_display(text, w) {
                    Ok(ml) => {
                        let (asc, desc) = (ml.h.ceil() as i32, ml.d.ceil() as i32);
                        let h = MATH_PAD_Y + asc + desc + MATH_PAD_Y;
                        let mw = ml.w.ceil() as i32;
                        let mx = x + ((w - mw) / 2).max(0);
                        let flags = if st.dim { F_DIM } else { 0 };
                        let base = y + MATH_PAD_Y + asc;
                        let op =
                            Op::Math { x: mx, w: mw, base, row: y, lay: ml, src: text.clone(), flags, display: true };
                        self.op(y, y + h, op);
                        self.anchor(b, y, y + h);
                        return y + h;
                    }
                    Err(e) => err_line(text, e.at),
                };
                let lh = code_lh();
                let lines: Vec<String> =
                    text.split('\n').map(|l| l.trim_end_matches('\r').replace('\t', "    ")).collect();
                let top = CODE_PAD_Y + 4;
                let h = top + lines.len() as i32 * lh + top;
                let r = Rect::new(x, y, w, h);
                self.op(y, y + h, Op::Code { r, lang: "math".into(), lines, lh, top, key: 0, math: true, err });
                self.anchor(b, y, y + h);
                y + h
            }
            Kind::FootDef { .. } => {
                self.anchor(b, y, y);
                y
            }
            Kind::Rule => {
                self.fill(Rect::new(x, y, w, 1), Col::Rule, 0);
                self.anchor(b, y, y + 1);
                y + 1
            }
            Kind::Quote(bl) => {
                // A neutral bar; the quoted text keeps the body color (dimming it hurt legibility).
                let y1 = self.blocks(bl, x + QUOTE_INDENT, w - QUOTE_INDENT, y, st);
                if y1 > y {
                    self.fill(Rect::new(x, y, QUOTE_BAR, y1 - y), Col::Bar, QUOTE_BAR / 2);
                }
                y1
            }
            Kind::List { start, items, loose } => self.list(*start, items, *loose, x, w, y, st),
            Kind::Table { aligns, head, rows } => self.table(b, aligns, head, rows, x, w, y),
            Kind::Images(imgs) => self.images(b, imgs, x, w, y),
            Kind::Alert(k, bl) => self.alert(*k, bl, x, w, y, st),
            Kind::Details { id, summary, blocks, open } => self.details(b, (*id, *open), summary, blocks, x, w, y, st),
            Kind::HtmlPara { align, inl, head } => self.html_para(b, (*align, *head), inl, x, w, y),
            Kind::HtmlTable(rows) => self.html_table(b, rows, x, w, y),
            Kind::DefList(items) => self.def_list(b, items, x, w, y, st),
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
                let (bx, by) = (x + indent - CHECK - 6, iy + (LH[BODY] - CHECK) / 2);
                self.op(by, by + CHECK, Op::Check { x: bx, y: by, done });
                self.checks.push(CheckHit { x: bx - 4, y: iy, w: CHECK + 8, h: LH[BODY], off: it.task_off });
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
                        link: 0,
                    },
                );
            } else {
                self.op(mid - 4, mid + 4, Op::Bullet { cx: x + indent - 15, cy: mid, level: st.depth % 3 });
            }
            yy = ey;
        }
        yy
    }

    /// GFM table: columns sized by content (each capped at ~48ch), cells wrapping inside their column
    /// (rows as tall as their tallest cell, text top-aligned), a rounded 1px frame, hairline grid and a
    /// tinted, semibold header row.
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
        let cap = (self.m.w(self.tx, "0", false, CELL) * CELL_MAX_CH).ceil() as i32;
        let mut nat = vec![36; ncols];
        for (ri, row) in all.iter().enumerate() {
            for (c, nat_c) in nat.iter_mut().enumerate() {
                let inl = row.get(c).unwrap_or(&empty);
                let one = self.flow(inl, i32::MAX / 4, CELL, ri == 0, false);
                let wide = one.iter().map(|l| l.last().map_or(0.0, |r| r.x + r.w)).fold(0.0f32, f32::max);
                *nat_c = (*nat_c).max((wide.ceil() as i32).min(cap) + 2 * CELL_PAD_X);
            }
        }
        let colw = fit_columns(&nat, w);
        let tw: i32 = colw.iter().sum();
        // Wrap every cell in its column; a row is as tall as its tallest cell.
        let lh = LH[CELL];
        let mut grid: Vec<Vec<Vec<Vec<Run>>>> = Vec::with_capacity(all.len());
        let mut row_h: Vec<i32> = Vec::with_capacity(all.len());
        for (ri, row) in all.iter().enumerate() {
            let cells: Vec<Vec<Vec<Run>>> = (0..ncols)
                .map(|c| self.flow(row.get(c).unwrap_or(&empty), colw[c] - 2 * CELL_PAD_X, CELL, ri == 0, true))
                .collect();
            let mut tallest = lh;
            for lines in &cells {
                let h: i32 = self.line_mets(lines, CELL).iter().map(|m| m.1).sum();
                tallest = tallest.max(h);
            }
            row_h.push(tallest + 2 * CELL_PAD_Y);
            grid.push(cells);
        }
        let h: i32 = row_h.iter().sum();
        let frame = Rect::new(x, y, tw, h);
        self.op(y, y + h, Op::Card { r: frame, rad: TABLE_RAD, fill: Col::Bg, border: Col::Border });
        // Header tint inside the frame: rounded at the top, square where it meets the first rule. Both
        // are pushed with the frame's y0 so the (stable) sort keeps them under the grid lines.
        let hh = row_h[0];
        let top = Rect::new(x + 1, y + 1, tw - 2, hh - 1);
        self.op(y, top.bottom(), Op::Fill { r: top, col: Col::Head, rad: TABLE_RAD - 1 });
        let low = Rect::new(x + 1, y + hh - TABLE_RAD, tw - 2, TABLE_RAD);
        self.op(y, low.bottom(), Op::Fill { r: low, col: Col::Head, rad: 0 });
        let mut cx = x;
        for (c, cw) in colw.iter().enumerate() {
            if c > 0 {
                self.fill(Rect::new(cx, y, 1, h), Col::Border, 0);
            }
            cx += cw;
        }
        let mut ry = y;
        for rh in &row_h[..row_h.len() - 1] {
            ry += rh;
            self.fill(Rect::new(x, ry - 1, tw, 1), Col::Border, 0);
        }
        let mut ry = y;
        for (ri, cells) in grid.into_iter().enumerate() {
            let mut cx = x;
            for (c, lines) in cells.into_iter().enumerate() {
                let avail = colw[c] - 2 * CELL_PAD_X;
                let mut ty = ry + CELL_PAD_Y;
                for line in lines {
                    let lw = line_right(&line).round() as i32;
                    let ox = match aligns.get(c) {
                        Some(Alignment::Right) => (avail - lw).max(0),
                        Some(Alignment::Center) => ((avail - lw) / 2).max(0),
                        _ => 0,
                    };
                    ty += self.emit(vec![line], cx + CELL_PAD_X + ox, ty, CELL, if ri == 0 { F_B } else { 0 });
                }
                cx += colw[c];
            }
            ry += row_h[ri];
        }
        self.anchor(b, y, y + h);
        y + h
    }

    /// The footnotes collected at the end of the document: a rule, then `n.` and the definition's
    /// blocks for each label, in reference order. Registers `fn-label` anchors.
    pub(super) fn foot_section(&mut self, defs: &[(usize, &str, &[Block])], x: i32, w: i32, y: i32) -> i32 {
        self.fill(Rect::new(x, y, w, 1), Col::Border, 0);
        let title = "Footnotes";
        let tw = self.m.w(self.tx, title, true, SMALL).ceil() as i32;
        let ty = y + 14;
        let col = Col::Dim;
        self.op(
            ty,
            ty + LH[SMALL],
            Op::Label { x, w: tw, text: title.into(), col, si: SMALL as u8, lh: LH[SMALL], bold: true },
        );
        let mut y = ty + LH[SMALL] + 8;
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
            let (si, lh) = (BODY as u8, LH[BODY]);
            let op = Op::Text { x: x + indent - 8 - lw, w: lw, text: num, flags: F_DIM, si, lh, link: 0 };
            self.op(iy, iy + LH[BODY], op);
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
