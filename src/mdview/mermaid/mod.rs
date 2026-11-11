//! Native renderer for Mermaid fenced blocks (no JavaScript): the source is parsed and laid out
//! into a [`Diagram`] of vector shapes (rasterized with tiny-skia, see [`render`]) and text labels
//! (drawn by the preview with its UI font, so they match the prose).
//!
//! Supported: `flowchart` / `graph` (TB / TD / BT / LR / RL; node shapes `[]`, `()`, `([])`,
//! `[[]]`, `[()]`, `(())`, `((()))`, `{}`, `{{}}`, `[//]`, `[\\]`, `>]`; links `-->`, `---`, `-.->`,
//! `==>`, `~~~`, `--o`, `--x`, `<-->`, with `-- text -->` / `-->|text|` labels and `&` groups;
//! subgraphs) on a layered layout ([`layered`]); `sequenceDiagram` (participants / actors with
//! aliases, `->>` `-->>` `->` `-->` `-x` `--x` `-)` `--)`, notes, loop / alt / opt / par / critical /
//! break frames, autonumber); `pie` (title, showData, legend with percentages); `classDiagram`
//! (classes with attributes / methods / annotations, generics, relations `<|--` `*--` `o--` `-->`
//! `--` `..>` `..|>` `..` with cardinalities and labels). Other diagram types are reported as not
//! supported and the preview keeps the source. Parsing never panics on bad input.

mod class;
mod flow;
pub(super) mod layered;
mod pie;
pub(crate) mod render;
mod seq;

/// Colors are roles; the preview maps them to its theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    Node,
    NodeBorder,
    Line,
    Text,
    Muted,
    Cluster,
    ClusterBorder,
    Note,
    NoteBorder,
    /// The page color (behind edge labels, hollow markers).
    LabelBg,
    /// Pie / series color `i`, and text that reads on it.
    Series(u8),
    OnSeries(u8),
}

/// Path commands in diagram units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum P {
    M(f32, f32),
    L(f32, f32),
    Q(f32, f32, f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    Z,
}

#[derive(Clone, Debug)]
pub struct Shape {
    pub path: Vec<P>,
    pub fill: Option<Paint>,
    pub stroke: Option<Paint>,
    pub width: f32,
    pub dash: bool,
}

/// One line of text centered vertically on `y`; `anchor` -1 left, 0 center, 1 right of `x`.
#[derive(Clone, Debug)]
pub struct Label {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub size: f32,
    pub bold: bool,
    pub anchor: i8,
    pub col: Paint,
}

#[derive(Clone, Debug, Default)]
pub struct Diagram {
    pub w: f32,
    pub h: f32,
    pub shapes: Vec<Shape>,
    pub labels: Vec<Label>,
}

/// Text width in px: `(text, bold, size)`.
pub type Meas<'a> = &'a mut dyn FnMut(&str, bool, f32) -> f32;

/// Node / class text size, edge label size, title size, and the line height of node text.
pub(crate) const FS: f32 = 13.0;
pub(crate) const FS_SMALL: f32 = 11.0;
pub(crate) const FS_TITLE: f32 = 16.0;
pub(crate) const LINE: f32 = 19.0;
pub(crate) const LINE_SMALL: f32 = 16.0;

/// Parse and lay out a Mermaid source. `Err` says why it can't be drawn (unsupported type, empty).
pub fn build(src: &str, m: Meas) -> Result<Diagram, String> {
    let lines = clean(src);
    let Some(first) = lines.first() else { return Err("empty diagram".into()) };
    let kw = first.split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("");
    let mut d = match kw {
        "flowchart" | "graph" | "flowchart-elk" => flow::build(&lines, m)?,
        "sequenceDiagram" => seq::build(&lines, m)?,
        "pie" => pie::build(&lines, m)?,
        "classDiagram" | "classDiagram-v2" => class::build(&lines, m)?,
        other => {
            let name = other.trim_end_matches("-beta").trim_end_matches("-v2");
            return Err(format!("{name} diagrams are not supported yet"));
        }
    };
    if !(d.w.is_finite() && d.h.is_finite()) || d.w <= 0.0 || d.h <= 0.0 {
        return Err("empty diagram".into());
    }
    d.w = d.w.min(20_000.0);
    d.h = d.h.min(20_000.0);
    Ok(d)
}

/// Source lines without front matter, `%%` comments, init directives and blank lines (trimmed).
fn clean(src: &str) -> Vec<String> {
    let mut out = vec![];
    let mut it = src.lines().peekable();
    // `---` front matter (title / config) at the very top.
    if it.peek().is_some_and(|l| l.trim() == "---") {
        it.next();
        for l in it.by_ref() {
            if l.trim() == "---" {
                break;
            }
        }
    }
    for l in it {
        let t = l.trim();
        if t.is_empty() || t.starts_with("%%") {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

/// Display text of a label: quotes and simple Markdown / HTML markup removed, split into lines at
/// `<br>` and `\n`.
pub(crate) fn text_lines(s: &str) -> Vec<String> {
    let mut t = s.trim();
    if let Some(inner) = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        t = inner;
    }
    if let Some(inner) = t.strip_prefix('`').and_then(|x| x.strip_suffix('`')) {
        t = inner;
    }
    let t = t.replace("<br/>", "\n").replace("<br />", "\n").replace("<br>", "\n").replace("\\n", "\n");
    let t = t.replace("**", "").replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
    let t = t.replace("#quot;", "\"").replace("&quot;", "\"");
    t.split('\n').map(|l| l.trim().to_string()).collect()
}

/// Greedy word wrap of `lines` at `max_w` px.
pub(crate) fn wrap(
    lines: &[String],
    max_w: f32,
    bold: bool,
    size: f32,
    m: &mut dyn FnMut(&str, bool, f32) -> f32,
) -> Vec<String> {
    let mut out = vec![];
    for l in lines {
        if m(l, bold, size) <= max_w {
            out.push(l.clone());
            continue;
        }
        let mut cur = String::new();
        for w in l.split_whitespace() {
            let cand = if cur.is_empty() { w.to_string() } else { format!("{cur} {w}") };
            if !cur.is_empty() && m(&cand, bold, size) > max_w {
                out.push(std::mem::take(&mut cur));
                cur = w.to_string();
            } else {
                cur = cand;
            }
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

impl Diagram {
    pub(crate) fn shape(&mut self, path: Vec<P>, fill: Option<Paint>, stroke: Option<Paint>, width: f32) {
        self.shapes.push(Shape { path, fill, stroke, width, dash: false });
    }

    pub(crate) fn line(&mut self, pts: &[(f32, f32)], col: Paint, width: f32, dash: bool) {
        if pts.len() < 2 {
            return;
        }
        let mut path = vec![P::M(pts[0].0, pts[0].1)];
        path.extend(pts[1..].iter().map(|p| P::L(p.0, p.1)));
        self.shapes.push(Shape { path, fill: None, stroke: Some(col), width, dash });
    }

    /// A polyline whose corners are rounded (radius up to `r`).
    pub(crate) fn curve(&mut self, pts: &[(f32, f32)], col: Paint, width: f32, dash: bool, r: f32) {
        if pts.len() < 3 {
            return self.line(pts, col, width, dash);
        }
        let mut path = vec![P::M(pts[0].0, pts[0].1)];
        for i in 1..pts.len() - 1 {
            let (a, b, c) = (pts[i - 1], pts[i], pts[i + 1]);
            let (l1, l2) = (dist(a, b), dist(b, c));
            let rr = r.min(l1 / 2.0).min(l2 / 2.0);
            if rr < 0.5 {
                path.push(P::L(b.0, b.1));
                continue;
            }
            let p = (b.0 + (a.0 - b.0) * rr / l1, b.1 + (a.1 - b.1) * rr / l1);
            let q = (b.0 + (c.0 - b.0) * rr / l2, b.1 + (c.1 - b.1) * rr / l2);
            path.push(P::L(p.0, p.1));
            path.push(P::Q(b.0, b.1, q.0, q.1));
        }
        let e = pts[pts.len() - 1];
        path.push(P::L(e.0, e.1));
        self.shapes.push(Shape { path, fill: None, stroke: Some(col), width, dash });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, fill: Option<Paint>, stroke: Option<Paint>) {
        self.shape(rect_path(x, y, w, h, r), fill, stroke, 1.2);
    }

    pub(crate) fn poly(&mut self, pts: &[(f32, f32)], fill: Option<Paint>, stroke: Option<Paint>, width: f32) {
        if pts.len() < 2 {
            return;
        }
        let mut path = vec![P::M(pts[0].0, pts[0].1)];
        path.extend(pts[1..].iter().map(|p| P::L(p.0, p.1)));
        path.push(P::Z);
        self.shape(path, fill, stroke, width);
    }

    pub(crate) fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, fill: Option<Paint>, stroke: Option<Paint>) {
        self.shape(ellipse_path(cx, cy, rx, ry), fill, stroke, 1.2);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text(&mut self, x: f32, y: f32, text: &str, size: f32, bold: bool, anchor: i8, col: Paint) {
        self.labels.push(Label { x, y, text: text.to_string(), size, bold, anchor, col });
    }

    /// Lines of text centered on (`cx`, `cy`).
    pub(crate) fn text_block(&mut self, cx: f32, cy: f32, lines: &[String], size: f32, bold: bool, col: Paint) {
        let lh = if size <= FS_SMALL { LINE_SMALL } else { LINE };
        let top = cy - lh * lines.len() as f32 / 2.0 + lh / 2.0;
        for (i, l) in lines.iter().enumerate() {
            self.text(cx, top + i as f32 * lh, l, size, bold, 0, col);
        }
    }

    /// An arrowhead / marker at `tip`, pointing away from `from`.
    pub(crate) fn marker(&mut self, tip: (f32, f32), from: (f32, f32), kind: Mark, col: Paint) {
        let len = dist(tip, from).max(1e-3);
        let (ux, uy) = ((tip.0 - from.0) / len, (tip.1 - from.1) / len);
        let (px, py) = (-uy, ux);
        let at = |a: f32, b: f32| (tip.0 - ux * a + px * b, tip.1 - uy * a + py * b);
        match kind {
            Mark::None => {}
            Mark::Arrow => self.poly(&[tip, at(9.0, 4.5), at(7.0, 0.0), at(9.0, -4.5)], Some(col), Some(col), 1.0),
            Mark::Open => {
                self.line(&[at(9.0, 5.0), tip, at(9.0, -5.0)], col, 1.3, false);
            }
            Mark::Circle => {
                let c = at(4.5, 0.0);
                self.ellipse(c.0, c.1, 4.0, 4.0, Some(col), Some(col));
            }
            Mark::Cross => {
                let c = at(5.0, 0.0);
                let (a, b) =
                    ((c.0 + (ux + px) * 4.0, c.1 + (uy + py) * 4.0), (c.0 - (ux + px) * 4.0, c.1 - (uy + py) * 4.0));
                let (e, f) =
                    ((c.0 + (ux - px) * 4.0, c.1 + (uy - py) * 4.0), (c.0 - (ux - px) * 4.0, c.1 - (uy - py) * 4.0));
                self.line(&[a, b], col, 1.6, false);
                self.line(&[e, f], col, 1.6, false);
            }
            Mark::Triangle => {
                self.poly(&[tip, at(13.0, 7.0), at(13.0, -7.0)], Some(Paint::LabelBg), Some(col), 1.2);
            }
            Mark::Diamond | Mark::DiamondOpen => {
                let fill = if kind == Mark::Diamond { col } else { Paint::LabelBg };
                self.poly(&[tip, at(8.0, 5.0), at(16.0, 0.0), at(8.0, -5.0)], Some(fill), Some(col), 1.2);
            }
        }
    }

    /// Move everything by (`dx`, `dy`).
    pub(crate) fn shift(&mut self, dx: f32, dy: f32) {
        for s in &mut self.shapes {
            for p in &mut s.path {
                *p = match *p {
                    P::M(x, y) => P::M(x + dx, y + dy),
                    P::L(x, y) => P::L(x + dx, y + dy),
                    P::Q(a, b, x, y) => P::Q(a + dx, b + dy, x + dx, y + dy),
                    P::C(a, b, c, d, x, y) => P::C(a + dx, b + dy, c + dx, d + dy, x + dx, y + dy),
                    P::Z => P::Z,
                };
            }
        }
        for l in &mut self.labels {
            l.x += dx;
            l.y += dy;
        }
    }
}

/// Line-end markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    None,
    /// Filled arrowhead.
    Arrow,
    /// Open `>` arrowhead (async messages).
    Open,
    Circle,
    Cross,
    /// Hollow triangle (inheritance / realization).
    Triangle,
    /// Filled diamond (composition).
    Diamond,
    /// Hollow diamond (aggregation).
    DiamondOpen,
}

/// How far a marker of `kind` reaches back from its tip (the line stops there).
pub(crate) fn marker_len(kind: Mark) -> f32 {
    match kind {
        Mark::None | Mark::Open => 0.0,
        Mark::Arrow => 7.0,
        Mark::Circle => 8.5,
        Mark::Cross => 0.0,
        Mark::Triangle => 13.0,
        Mark::Diamond | Mark::DiamondOpen => 16.0,
    }
}

pub(crate) fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// `p` moved `d` px toward `q`.
pub(crate) fn toward(p: (f32, f32), q: (f32, f32), d: f32) -> (f32, f32) {
    let l = dist(p, q);
    if l < 1e-3 {
        return p;
    }
    let t = (d / l).min(1.0);
    (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t)
}

pub(crate) fn rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<P> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    if r < 0.1 {
        return vec![P::M(x, y), P::L(x + w, y), P::L(x + w, y + h), P::L(x, y + h), P::Z];
    }
    vec![
        P::M(x + r, y),
        P::L(x + w - r, y),
        P::Q(x + w, y, x + w, y + r),
        P::L(x + w, y + h - r),
        P::Q(x + w, y + h, x + w - r, y + h),
        P::L(x + r, y + h),
        P::Q(x, y + h, x, y + h - r),
        P::L(x, y + r),
        P::Q(x, y, x + r, y),
        P::Z,
    ]
}

pub(crate) fn ellipse_path(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<P> {
    const K: f32 = 0.552_284_8;
    let (kx, ky) = (rx * K, ry * K);
    vec![
        P::M(cx + rx, cy),
        P::C(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry),
        P::C(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy),
        P::C(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry),
        P::C(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy),
        P::Z,
    ]
}

/// Arc of a circle as cubic Béziers (each at most a quarter turn), from angle `a0` to `a1`
/// (radians, clockwise on screen), continuing the current path.
pub(crate) fn arc(path: &mut Vec<P>, c: (f32, f32), r: f32, a0: f32, a1: f32) {
    let n = ((a1 - a0).abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = (a1 - a0) / n as f32;
    for i in 0..n {
        let (s, e) = (a0 + step * i as f32, a0 + step * (i + 1) as f32);
        let k = 4.0 / 3.0 * ((e - s) / 4.0).tan();
        let (p0, p3) = ((c.0 + r * s.cos(), c.1 + r * s.sin()), (c.0 + r * e.cos(), c.1 + r * e.sin()));
        let p1 = (p0.0 - k * r * s.sin(), p0.1 + k * r * s.cos());
        let p2 = (p3.0 + k * r * e.sin(), p3.1 - k * r * e.cos());
        path.push(P::C(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1));
    }
}

#[cfg(test)]
mod tests;
