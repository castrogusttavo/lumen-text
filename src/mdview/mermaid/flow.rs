//! `flowchart` / `graph`.

use super::layered::{self, Dir};
use super::*;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Rect,
    Round,
    Stadium,
    Sub,
    Cyl,
    Circle,
    DCircle,
    Diamond,
    Hex,
    Para,
    ParaAlt,
    Trap,
    TrapAlt,
    Flag,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stroke {
    Solid,
    Dotted,
    Thick,
    Invisible,
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) shape: Shape,
    pub(crate) sub: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub(crate) stroke: Stroke,
    pub(crate) start: Mark,
    pub(crate) end: Mark,
    pub(crate) label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Edge {
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) link: Link,
}

#[derive(Clone, Debug)]
pub(crate) struct Sub {
    pub(crate) title: String,
    pub(crate) parent: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct Graph {
    pub(crate) dir: Dir,
    pub(crate) nodes: Vec<Node>,
    pub(crate) ids: HashMap<String, usize>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) subs: Vec<Sub>,
    open: Vec<usize>,
}

const MAX_NODES: usize = 400;

impl Graph {
    fn node(&mut self, id: &str) -> Option<usize> {
        if let Some(&i) = self.ids.get(id) {
            return Some(i);
        }
        if self.nodes.len() >= MAX_NODES {
            return None;
        }
        let i = self.nodes.len();
        self.nodes.push(Node {
            id: id.to_string(),
            label: id.to_string(),
            shape: Shape::Rect,
            sub: self.open.last().copied(),
        });
        self.ids.insert(id.to_string(), i);
        Some(i)
    }
}

fn id_char(s: &str, i: usize) -> bool {
    let Some(c) = s[i..].chars().next() else { return false };
    if c.is_alphanumeric() || c == '_' {
        return true;
    }
    // `my-node`: a hyphen between letters (not the start of a link `--`, `-.`, `->`).
    if c == '-' {
        let prev = s[..i].chars().last().is_some_and(|p| p.is_alphanumeric());
        let next = s[i + 1..].chars().next().is_some_and(|n| n.is_alphanumeric());
        return prev && next;
    }
    false
}

fn skip_ws(s: &str, i: &mut usize) {
    while let Some(c) = s[*i..].chars().next() {
        if !c.is_whitespace() {
            break;
        }
        *i += c.len_utf8();
    }
}

const SHAPES: &[(&str, &str, Shape)] = &[
    ("(((", ")))", Shape::DCircle),
    ("((", "))", Shape::Circle),
    ("([", "])", Shape::Stadium),
    ("[[", "]]", Shape::Sub),
    ("[(", ")]", Shape::Cyl),
    ("{{", "}}", Shape::Hex),
    ("[/", "]", Shape::Para),
    ("[\\", "]", Shape::ParaAlt),
    ("[", "]", Shape::Rect),
    ("(", ")", Shape::Round),
    ("{", "}", Shape::Diamond),
    (">", "]", Shape::Flag),
];

/// `id`, `id[label]`, `id(label)`, ... at `*i`; returns the node index.
fn parse_node(s: &str, i: &mut usize, g: &mut Graph) -> Option<usize> {
    let start = *i;
    while *i < s.len() && id_char(s, *i) {
        *i += s[*i..].chars().next().map_or(1, char::len_utf8);
    }
    if *i == start {
        return None;
    }
    let id = s[start..*i].to_string();
    let n = g.node(&id)?;
    // `:::class`
    if s[*i..].starts_with(":::") {
        *i += 3;
        while *i < s.len() && id_char(s, *i) {
            *i += s[*i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    let rest = &s[*i..];
    for (open, close, shape) in SHAPES {
        if !rest.starts_with(open) {
            continue;
        }
        let body = &rest[open.len()..];
        // A quoted label may hold the closing characters.
        let (inner_end, after) = if body.trim_start().starts_with('"') {
            let q0 = body.find('"').unwrap_or(0);
            match body[q0 + 1..].find('"') {
                Some(q1) => {
                    let qe = q0 + 1 + q1 + 1;
                    match body[qe..].find(close) {
                        Some(c) => (qe + c, qe + c + close.len()),
                        None => continue,
                    }
                }
                None => continue,
            }
        } else {
            match body.find(close) {
                Some(c) => (c, c + close.len()),
                None => continue,
            }
        };
        let mut label = body[..inner_end].to_string();
        let mut shape = *shape;
        // `[/text/]`, `[\text\]`, `[/text\]`, `[\text/]`
        if matches!(shape, Shape::Para | Shape::ParaAlt) {
            let last = label.chars().last();
            shape = match (shape, last) {
                (Shape::Para, Some('/')) => Shape::Para,
                (Shape::Para, Some('\\')) => Shape::Trap,
                (Shape::ParaAlt, Some('\\')) => Shape::ParaAlt,
                (Shape::ParaAlt, Some('/')) => Shape::TrapAlt,
                (s, _) => s,
            };
            if matches!(last, Some('/' | '\\')) {
                label.pop();
            }
        }
        *i += open.len() + after;
        if let Some(node) = g.nodes.get_mut(n) {
            node.label = label.trim().to_string();
            node.shape = shape;
        }
        break;
    }
    Some(n)
}

/// `a & b` groups.
fn parse_group(s: &str, i: &mut usize, g: &mut Graph) -> Vec<usize> {
    let mut v = vec![];
    loop {
        skip_ws(s, i);
        let Some(n) = parse_node(s, i, g) else { break };
        v.push(n);
        let save = *i;
        skip_ws(s, i);
        if s[*i..].starts_with('&') {
            *i += 1;
        } else {
            *i = save;
            break;
        }
    }
    v
}

fn body_at(s: &str, i: &mut usize) -> String {
    let st = *i;
    while let Some(c) = s[*i..].chars().next() {
        if !matches!(c, '-' | '=' | '.' | '~') {
            break;
        }
        *i += 1;
    }
    s[st..*i].to_string()
}

fn end_mark(s: &str, i: &mut usize) -> Mark {
    let rest = &s[*i..];
    let after = |n: usize| rest[n..].chars().next();
    let free = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    if rest.starts_with('>') {
        *i += 1;
        Mark::Arrow
    } else if rest.starts_with('o') && free(after(1)) {
        *i += 1;
        Mark::Circle
    } else if rest.starts_with('x') && free(after(1)) {
        *i += 1;
        Mark::Cross
    } else {
        Mark::None
    }
}

fn stroke_of(body: &str) -> Stroke {
    if body.contains('~') {
        Stroke::Invisible
    } else if body.contains('=') {
        Stroke::Thick
    } else if body.contains('.') {
        Stroke::Dotted
    } else {
        Stroke::Solid
    }
}

/// A link at `*i` (`-->`, `-- text -->`, `-->|text|`, `<-->`, `-.->`, `==>`, `~~~`, `--o`, `--x`).
fn parse_link(s: &str, i: &mut usize) -> Option<Link> {
    let st = *i;
    let rest = &s[*i..];
    let start = if rest.starts_with('<') {
        *i += 1;
        Mark::Arrow
    } else if (rest.starts_with('o') || rest.starts_with('x'))
        && matches!(rest[1..].chars().next(), Some('-' | '=' | '.'))
    {
        *i += 1;
        if rest.starts_with('o') { Mark::Circle } else { Mark::Cross }
    } else {
        Mark::None
    };
    let body = body_at(s, i);
    if body.is_empty() || (body.len() < 2 && !s[*i..].starts_with('>')) {
        *i = st;
        return None;
    }
    let mut end = end_mark(s, i);
    let mut stroke = stroke_of(&body);
    let mut label = String::new();
    // `A -- text --> B` / `A -. text .-> B` / `A == text ==> B`
    if end == Mark::None && matches!(body.as_str(), "--" | "==" | "-.") {
        let closers: &[&str] = match body.as_str() {
            "--" => &["-->", "---", "--o", "--x"],
            "==" => &["==>", "===", "==o", "==x"],
            _ => &[".->", ".-"],
        };
        let rest = &s[*i..];
        if let Some((p, _)) = closers.iter().filter_map(|c| rest.find(c).map(|p| (p, *c))).min()
            && !rest[..p].trim().is_empty()
        {
            label = rest[..p].trim().to_string();
            *i += p;
            let b2 = body_at(s, i);
            end = end_mark(s, i);
            stroke = stroke_of(&format!("{body}{b2}"));
        }
    }
    // `-->|text|`
    let save = *i;
    skip_ws(s, i);
    if s[*i..].starts_with('|') {
        if let Some(e) = s[*i + 1..].find('|') {
            label = s[*i + 1..*i + 1 + e].trim().to_string();
            *i += e + 2;
        } else {
            *i = save;
        }
    } else {
        *i = save;
    }
    Some(Link { stroke, start, end, label })
}

fn parse_stmt(s: &str, g: &mut Graph) {
    let mut i = 0;
    let mut prev = parse_group(s, &mut i, g);
    if prev.is_empty() {
        return;
    }
    loop {
        skip_ws(s, &mut i);
        if i >= s.len() {
            break;
        }
        let Some(link) = parse_link(s, &mut i) else { break };
        let next = parse_group(s, &mut i, g);
        if next.is_empty() {
            break;
        }
        for &a in &prev {
            for &b in &next {
                if g.edges.len() < 2000 {
                    g.edges.push(Edge { from: a, to: b, link: link.clone() });
                }
            }
        }
        prev = next;
    }
}

pub(crate) fn parse(lines: &[String]) -> Graph {
    let mut g = Graph { dir: Dir::Down, nodes: vec![], ids: HashMap::new(), edges: vec![], subs: vec![], open: vec![] };
    let mut first = true;
    for line in lines {
        for stmt in line.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            if first {
                first = false;
                let mut w = stmt.split_whitespace();
                w.next();
                if let Some(d) = w.next() {
                    g.dir = Dir::parse(d);
                }
                continue;
            }
            let kw = stmt.split_whitespace().next().unwrap_or("");
            match kw {
                "subgraph" => {
                    let rest = stmt["subgraph".len()..].trim();
                    let title = match (rest.find('['), rest.rfind(']')) {
                        (Some(a), Some(b)) if b > a => rest[a + 1..b].to_string(),
                        _ => rest.to_string(),
                    };
                    let title = text_lines(&title).join(" ");
                    g.subs.push(Sub { title, parent: g.open.last().copied() });
                    g.open.push(g.subs.len() - 1);
                }
                "end" => {
                    g.open.pop();
                }
                "direction" | "classDef" | "class" | "style" | "linkStyle" | "click" | "accTitle" | "accDescr"
                | "accTitle:" | "accDescr:" | "title" => {}
                _ => parse_stmt(stmt, &mut g),
            }
        }
    }
    g
}

/// Point where the segment from the center of node box (`c`, `w` x `h`, `shape`) toward `to`
/// leaves the node.
fn clip(c: (f32, f32), w: f32, h: f32, shape: Shape, to: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (to.0 - c.0, to.1 - c.1);
    if dx.abs() < 1e-3 && dy.abs() < 1e-3 {
        return c;
    }
    let (hw, hh) = (w / 2.0, h / 2.0);
    let t = match shape {
        Shape::Diamond => 1.0 / (dx.abs() / hw + dy.abs() / hh),
        Shape::Circle | Shape::DCircle => hw / (dx * dx + dy * dy).sqrt(),
        _ => (hw / dx.abs().max(1e-6)).min(hh / dy.abs().max(1e-6)),
    };
    let t = t.min(1.0);
    (c.0 + dx * t, c.1 + dy * t)
}

fn node_size(lines: &[String], shape: Shape, m: Meas) -> (f32, f32) {
    let tw = lines.iter().map(|l| m(l, false, FS)).fold(0.0f32, f32::max);
    let th = lines.len().max(1) as f32 * LINE;
    match shape {
        Shape::Diamond => {
            let s = (tw + th) * 0.75 + 34.0;
            ((tw + 40.0).max(s), (th + 26.0).max(s * 0.62))
        }
        Shape::Circle | Shape::DCircle => {
            let d = (tw + 26.0).max(th + 26.0);
            (d, d)
        }
        Shape::Stadium => (tw + th + 30.0, th + 18.0),
        Shape::Hex => (tw + 44.0, th + 20.0),
        Shape::Para | Shape::ParaAlt | Shape::Trap | Shape::TrapAlt => (tw + 46.0, th + 18.0),
        Shape::Cyl => ((tw + 28.0).max(60.0), th + 30.0),
        Shape::Flag => (tw + 40.0, th + 18.0),
        _ => ((tw + 30.0).max(56.0), th + 18.0),
    }
}

fn draw_node(d: &mut Diagram, c: (f32, f32), w: f32, h: f32, shape: Shape, lines: &[String]) {
    let (x, y) = (c.0 - w / 2.0, c.1 - h / 2.0);
    let (f, s) = (Some(Paint::Node), Some(Paint::NodeBorder));
    match shape {
        Shape::Rect => d.rect(x, y, w, h, 3.0, f, s),
        Shape::Round => d.rect(x, y, w, h, 10.0, f, s),
        Shape::Stadium => d.rect(x, y, w, h, h / 2.0, f, s),
        Shape::Sub => {
            d.rect(x, y, w, h, 2.0, f, s);
            d.line(&[(x + 8.0, y), (x + 8.0, y + h)], Paint::NodeBorder, 1.2, false);
            d.line(&[(x + w - 8.0, y), (x + w - 8.0, y + h)], Paint::NodeBorder, 1.2, false);
        }
        Shape::Cyl => {
            let ry = 7.0;
            let p = vec![
                P::M(x, y + ry),
                P::L(x, y + h - ry),
                P::C(x, y + h + ry * 0.33, x + w, y + h + ry * 0.33, x + w, y + h - ry),
                P::L(x + w, y + ry),
                P::C(x + w, y - ry * 0.33, x, y - ry * 0.33, x, y + ry),
                P::Z,
            ];
            d.shape(p, f, s, 1.2);
            let top = vec![P::M(x, y + ry), P::C(x, y + ry * 2.33, x + w, y + ry * 2.33, x + w, y + ry)];
            d.shape(top, None, s, 1.2);
        }
        Shape::Circle => d.ellipse(c.0, c.1, w / 2.0, h / 2.0, f, s),
        Shape::DCircle => {
            d.ellipse(c.0, c.1, w / 2.0, h / 2.0, f, s);
            d.ellipse(c.0, c.1, w / 2.0 - 4.0, h / 2.0 - 4.0, None, s);
        }
        Shape::Diamond => d.poly(&[(c.0, y), (x + w, c.1), (c.0, y + h), (x, c.1)], f, s, 1.2),
        Shape::Hex => {
            let k = (h / 2.0).min(w / 4.0);
            d.poly(&[(x + k, y), (x + w - k, y), (x + w, c.1), (x + w - k, y + h), (x + k, y + h), (x, c.1)], f, s, 1.2)
        }
        Shape::Para => d.poly(&[(x + 12.0, y), (x + w, y), (x + w - 12.0, y + h), (x, y + h)], f, s, 1.2),
        Shape::ParaAlt => d.poly(&[(x, y), (x + w - 12.0, y), (x + w, y + h), (x + 12.0, y + h)], f, s, 1.2),
        Shape::Trap => d.poly(&[(x + 12.0, y), (x + w - 12.0, y), (x + w, y + h), (x, y + h)], f, s, 1.2),
        Shape::TrapAlt => d.poly(&[(x, y), (x + w, y), (x + w - 12.0, y + h), (x + 12.0, y + h)], f, s, 1.2),
        Shape::Flag => d.poly(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x + 12.0, c.1)], f, s, 1.2),
    }
    let tx = if shape == Shape::Flag { c.0 + 6.0 } else { c.0 };
    d.text_block(tx, c.1, lines, FS, false, Paint::Text);
}

pub(super) fn build(lines: &[String], m: Meas) -> Result<Diagram, String> {
    let g = parse(lines);
    if g.nodes.is_empty() {
        return Err("empty flowchart".into());
    }
    let labels: Vec<Vec<String>> =
        g.nodes.iter().map(|n| wrap(&text_lines(&n.label), 200.0, false, FS, &mut *m)).collect();
    let sizes: Vec<(f32, f32)> = g.nodes.iter().zip(&labels).map(|(n, l)| node_size(l, n.shape, &mut *m)).collect();
    let elabels: Vec<Vec<String>> = g
        .edges
        .iter()
        .map(|e| {
            if e.link.label.is_empty() {
                vec![]
            } else {
                wrap(&text_lines(&e.link.label), 160.0, false, FS_SMALL, &mut *m)
            }
        })
        .collect();
    let esizes: Vec<(f32, f32)> = elabels
        .iter()
        .map(|l| {
            if l.is_empty() {
                (0.0, 0.0)
            } else {
                let w = l.iter().map(|t| m(t, false, FS_SMALL)).fold(0.0f32, f32::max);
                (w + 10.0, l.len() as f32 * LINE_SMALL + 4.0)
            }
        })
        .collect();
    let edges: Vec<(usize, usize, (f32, f32))> = g.edges.iter().zip(&esizes).map(|(e, s)| (e.from, e.to, *s)).collect();
    let has_subs = g.nodes.iter().any(|n| n.sub.is_some());
    let (ng, rg) = if has_subs { (44.0, 64.0) } else { (36.0, 48.0) };
    let o = layered::layout(&sizes, &edges, g.dir, ng, rg);
    let mut d = Diagram::default();
    // Subgraphs (innermost last, so they paint over their parents).
    let mut boxes: Vec<Option<(f32, f32, f32, f32)>> = vec![None; g.subs.len()];
    let depth = |mut s: usize| {
        let mut k = 0;
        while let Some(p) = g.subs.get(s).and_then(|x| x.parent) {
            s = p;
            k += 1;
            if k > 32 {
                break;
            }
        }
        k
    };
    let mut by_depth: Vec<usize> = (0..g.subs.len()).collect();
    by_depth.sort_by_key(|&s| std::cmp::Reverse(depth(s)));
    for &s in &by_depth {
        let mut bb: Option<(f32, f32, f32, f32)> = None;
        let mut grow = |b: (f32, f32, f32, f32)| {
            bb = Some(bb.map_or(b, |a| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))));
        };
        for (i, n) in g.nodes.iter().enumerate() {
            if n.sub == Some(s) {
                let ((x, y), (w, h)) = (o.pos[i], sizes[i]);
                grow((x - w / 2.0, y - h / 2.0, x + w / 2.0, y + h / 2.0));
            }
        }
        for (c, sub) in g.subs.iter().enumerate() {
            if sub.parent == Some(s)
                && let Some(b) = boxes[c]
            {
                grow(b);
            }
        }
        boxes[s] = bb.map(|b| (b.0 - 14.0, b.1 - 30.0, b.2 + 14.0, b.3 + 14.0));
    }
    for &s in by_depth.iter().rev() {
        if let Some((x0, y0, x1, y1)) = boxes[s] {
            d.rect(x0, y0, x1 - x0, y1 - y0, 8.0, Some(Paint::Cluster), Some(Paint::ClusterBorder));
            d.text(x0 + 10.0, y0 + 13.0, &g.subs[s].title, FS_SMALL, true, -1, Paint::Muted);
        }
    }
    // Edges under the nodes.
    for (ei, e) in g.edges.iter().enumerate() {
        if e.link.stroke == Stroke::Invisible {
            continue;
        }
        let (width, dash) = match e.link.stroke {
            Stroke::Thick => (2.6, false),
            Stroke::Dotted => (1.4, true),
            _ => (1.4, false),
        };
        let path = &o.paths[ei];
        if path.len() < 2 {
            // Self loop on the right of the node.
            if e.from == e.to
                && let (Some(&c), Some(&(w, h))) = (o.pos.get(e.from), sizes.get(e.from))
            {
                let (x, y) = (c.0 + w / 2.0, c.1);
                let pts = [(x, y - h / 4.0), (x + 22.0, y - h / 4.0), (x + 22.0, y + h / 4.0), (x, y + h / 4.0)];
                let mut p = pts.to_vec();
                if let Some(last) = p.last_mut() {
                    *last = toward(pts[3], pts[2], marker_len(e.link.end));
                }
                d.curve(&p, Paint::Line, width, dash, 6.0);
                d.marker(pts[3], pts[2], e.link.end, Paint::Line);
            }
            continue;
        }
        let mut pts = path.clone();
        let (a, b) = (e.from, e.to);
        let n = pts.len();
        pts[0] = clip(o.pos[a], sizes[a].0, sizes[a].1, g.nodes[a].shape, pts[1]);
        pts[n - 1] = clip(o.pos[b], sizes[b].0, sizes[b].1, g.nodes[b].shape, pts[n - 2]);
        let (tip_end, tip_start) = (pts[n - 1], pts[0]);
        let mut line = pts.clone();
        line[n - 1] = toward(tip_end, pts[n - 2], marker_len(e.link.end));
        line[0] = toward(tip_start, pts[1], marker_len(e.link.start));
        d.curve(&line, Paint::Line, width, dash, 14.0);
        d.marker(tip_end, pts[n - 2], e.link.end, Paint::Line);
        d.marker(tip_start, pts[1], e.link.start, Paint::Line);
    }
    // Edge labels on a page-colored chip.
    for (ei, l) in elabels.iter().enumerate() {
        if l.is_empty() || g.edges[ei].link.stroke == Stroke::Invisible {
            continue;
        }
        let (cx, cy) = o.label_at[ei];
        let (w, h) = esizes[ei];
        d.rect(cx - w / 2.0, cy - h / 2.0, w, h, 4.0, Some(Paint::LabelBg), None);
        d.text_block(cx, cy, l, FS_SMALL, false, Paint::Muted);
    }
    for (i, n) in g.nodes.iter().enumerate() {
        draw_node(&mut d, o.pos[i], sizes[i].0, sizes[i].1, n.shape, &labels[i]);
    }
    // Margins (subgraph boxes and self loops may stick out of the layout box).
    let mut x0 = 0.0f32;
    let mut y0 = 0.0f32;
    let mut x1 = o.w;
    let mut y1 = o.h;
    for b in boxes.iter().flatten() {
        x0 = x0.min(b.0);
        y0 = y0.min(b.1);
        x1 = x1.max(b.2);
        y1 = y1.max(b.3);
    }
    if g.edges.iter().any(|e| e.from == e.to) {
        x1 += 30.0;
    }
    let pad = 6.0;
    d.shift(pad - x0, pad - y0);
    d.w = x1 - x0 + 2.0 * pad;
    d.h = y1 - y0 + 2.0 * pad;
    Ok(d)
}
