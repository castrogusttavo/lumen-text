//! `classDiagram`.

use super::layered::{self, Dir};
use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct Class {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) stereo: String,
    pub(crate) attrs: Vec<String>,
    pub(crate) methods: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Rel {
    pub(crate) a: usize,
    pub(crate) b: usize,
    /// Marker at the `a` end and at the `b` end.
    pub(crate) ma: Mark,
    pub(crate) mb: Mark,
    pub(crate) dashed: bool,
    pub(crate) card_a: String,
    pub(crate) card_b: String,
    pub(crate) label: String,
}

#[derive(Debug)]
pub(crate) struct Classes {
    pub(crate) dir: Dir,
    pub(crate) classes: Vec<Class>,
    pub(crate) rels: Vec<Rel>,
}

/// `List~T~` -> `List<T>`.
fn generics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut open = false;
    for c in s.chars() {
        if c == '~' {
            out.push(if open { '>' } else { '<' });
            open = !open;
        } else {
            out.push(c);
        }
    }
    out
}

impl Classes {
    fn class(&mut self, raw: &str) -> Option<usize> {
        let raw = raw.trim().trim_end_matches('{').trim();
        // `Name~T~`, `Name["Label"]`
        let (id_part, label) = match (raw.find('['), raw.rfind(']')) {
            (Some(a), Some(b)) if b > a => (&raw[..a], Some(raw[a + 1..b].trim().trim_matches('"').to_string())),
            _ => (raw, None),
        };
        let id = id_part.split('~').next().unwrap_or("").trim();
        if id.is_empty() || id.contains(char::is_whitespace) || self.classes.len() >= 200 {
            return self.classes.iter().position(|c| c.id == id).filter(|_| !id.is_empty());
        }
        let name = label.unwrap_or_else(|| generics(id_part.trim()));
        if let Some(i) = self.classes.iter().position(|c| c.id == id) {
            if name.len() > self.classes[i].name.len() {
                self.classes[i].name = name;
            }
            return Some(i);
        }
        self.classes.push(Class { id: id.to_string(), name, ..Default::default() });
        Some(self.classes.len() - 1)
    }

    fn member(&mut self, c: usize, text: &str) {
        let t = text.trim();
        if t.is_empty() || t == "{" || t == "}" {
            return;
        }
        let Some(cl) = self.classes.get_mut(c) else { return };
        if t.starts_with("<<") && t.ends_with(">>") {
            cl.stereo = t.to_string();
        } else if t.contains('(') {
            cl.methods.push(generics(t));
        } else {
            cl.attrs.push(generics(t));
        }
    }
}

/// Split at whitespace, keeping `"quoted strings"` whole.
fn tokens(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut q = false;
    for c in s.chars() {
        match c {
            '"' => {
                cur.push(c);
                q = !q;
            }
            c if c.is_whitespace() && !q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A relation token (`<|--`, `*--`, `o--`, `-->`, `--`, `..>`, `..|>`, `..`): markers and dashes.
fn relation(tok: &str) -> Option<(Mark, Mark, bool)> {
    let (core, dashed) = if let Some(i) = tok.find("--") { (i, false) } else { (tok.find("..")?, true) };
    let left = &tok[..core];
    let rest = tok[core..].trim_start_matches(['-', '.']);
    let mark = |s: &str| match s {
        "<|" | "|>" => Some(Mark::Triangle),
        "*" => Some(Mark::Diamond),
        "o" => Some(Mark::DiamondOpen),
        "<" | ">" => Some(Mark::Arrow),
        "" => Some(Mark::None),
        _ => None,
    };
    Some((mark(left)?, mark(rest)?, dashed))
}

pub(crate) fn parse(lines: &[String]) -> Classes {
    let mut g = Classes { dir: Dir::Down, classes: vec![], rels: vec![] };
    let mut body: Option<usize> = None;
    for line in lines.iter().skip(1) {
        let l = line.trim();
        if let Some(c) = body {
            if l.starts_with('}') {
                body = None;
            } else {
                g.member(c, l);
            }
            continue;
        }
        if let Some(d) = l.strip_prefix("direction") {
            g.dir = Dir::parse(d);
            continue;
        }
        if l.starts_with("note")
            || l.starts_with("style")
            || l.starts_with("classDef")
            || l.starts_with("click")
            || l.starts_with("link ")
            || l.starts_with("callback")
            || l.starts_with("cssClass")
        {
            continue;
        }
        if let Some(rest) = l.strip_prefix("class ") {
            let rest = rest.trim();
            let opens = rest.ends_with('{');
            let name = rest.trim_end_matches('{').trim();
            // `class A:::css`
            let name = name.split(":::").next().unwrap_or(name);
            if let Some(c) = g.class(name)
                && opens
            {
                body = Some(c);
            }
            continue;
        }
        if l.starts_with("<<") {
            // `<<interface>> Name`
            if let Some(e) = l.find(">>") {
                let st = l[..e + 2].to_string();
                if let Some(c) = g.class(&l[e + 2..]) {
                    g.classes[c].stereo = st;
                }
            }
            continue;
        }
        // Relation `A "1" --> "*" B : label`, or member `A : +field`.
        let (main, label) = match split_label(l) {
            Some((a, b)) => (a, Some(b)),
            None => (l, None),
        };
        let toks = tokens(main);
        if let Some(ri) = toks
            .iter()
            .position(|t| !t.starts_with('"') && relation(t).is_some() && (t.contains("--") || t.contains("..")))
        {
            let Some((ma, mb, dashed)) = relation(&toks[ri]) else { continue };
            let names_a: Vec<&String> = toks[..ri].iter().filter(|t| !t.starts_with('"')).collect();
            let names_b: Vec<&String> = toks[ri + 1..].iter().filter(|t| !t.starts_with('"')).collect();
            let card = |v: &[String]| {
                v.iter().find(|t| t.starts_with('"')).map(|t| t.trim_matches('"').to_string()).unwrap_or_default()
            };
            let (Some(na), Some(nb)) = (names_a.last(), names_b.first()) else { continue };
            let (Some(a), Some(b)) = (g.class(na), g.class(nb)) else { continue };
            if g.rels.len() < 600 {
                g.rels.push(Rel {
                    a,
                    b,
                    ma,
                    mb,
                    dashed,
                    card_a: card(&toks[..ri]),
                    card_b: card(&toks[ri + 1..]),
                    label: label.map(|s| s.trim().to_string()).unwrap_or_default(),
                });
            }
            continue;
        }
        if let (Some(lab), [name]) = (label, toks.as_slice())
            && let Some(c) = g.class(name)
        {
            g.member(c, lab);
        }
    }
    g
}

/// `left : right` at the first colon outside quotes.
fn split_label(s: &str) -> Option<(&str, &str)> {
    let mut q = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => q = !q,
            ':' if !q => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
    }
    None
}

fn clip_rect(c: (f32, f32), w: f32, h: f32, to: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (to.0 - c.0, to.1 - c.1);
    if dx.abs() < 1e-3 && dy.abs() < 1e-3 {
        return c;
    }
    let t = ((w / 2.0) / dx.abs().max(1e-6)).min((h / 2.0) / dy.abs().max(1e-6)).min(1.0);
    (c.0 + dx * t, c.1 + dy * t)
}

pub(super) fn build(lines: &[String], m: Meas) -> Result<Diagram, String> {
    let g = parse(lines);
    if g.classes.is_empty() {
        return Err("empty class diagram".into());
    }
    const ROW: f32 = 18.0;
    const HEAD: f32 = 28.0;
    // (w, h, header h) of every class box.
    let dims: Vec<(f32, f32, f32)> = g
        .classes
        .iter()
        .map(|c| {
            let mut w = m(&c.name, true, FS) + 28.0;
            if !c.stereo.is_empty() {
                w = w.max(m(&c.stereo, false, FS_SMALL) + 28.0);
            }
            for t in c.attrs.iter().chain(&c.methods) {
                w = w.max(m(t, false, FS_SMALL) + 24.0);
            }
            let head = HEAD + if c.stereo.is_empty() { 0.0 } else { 14.0 };
            let sect = |n: usize| if n == 0 { 10.0 } else { n as f32 * ROW + 10.0 };
            (w.max(90.0), head + sect(c.attrs.len()) + sect(c.methods.len()), head)
        })
        .collect();
    let lsize: Vec<(f32, f32)> = g
        .rels
        .iter()
        .map(|r| if r.label.is_empty() { (0.0, 0.0) } else { (m(&r.label, false, FS_SMALL) + 10.0, LINE_SMALL + 4.0) })
        .collect();
    let sizes: Vec<(f32, f32)> = dims.iter().map(|d| (d.0, d.1)).collect();
    let edges: Vec<(usize, usize, (f32, f32))> = g.rels.iter().zip(&lsize).map(|(r, s)| (r.a, r.b, *s)).collect();
    let o = layered::layout(&sizes, &edges, g.dir, 50.0, 64.0);
    let mut d = Diagram::default();
    for (ri, r) in g.rels.iter().enumerate() {
        let path = &o.paths[ri];
        if path.len() < 2 {
            continue;
        }
        let mut pts = path.clone();
        let n = pts.len();
        pts[0] = clip_rect(o.pos[r.a], sizes[r.a].0, sizes[r.a].1, pts[1]);
        pts[n - 1] = clip_rect(o.pos[r.b], sizes[r.b].0, sizes[r.b].1, pts[n - 2]);
        let (ta, tb) = (pts[0], pts[n - 1]);
        let mut line = pts.clone();
        line[0] = toward(ta, pts[1], marker_len(r.ma));
        line[n - 1] = toward(tb, pts[n - 2], marker_len(r.mb));
        d.curve(&line, Paint::Line, 1.3, r.dashed, 12.0);
        d.marker(ta, pts[1], r.ma, Paint::Line);
        d.marker(tb, pts[n - 2], r.mb, Paint::Line);
        // Cardinalities just off each end, beside the line.
        for (tip, next, text) in [(ta, pts[1], &r.card_a), (tb, pts[n - 2], &r.card_b)] {
            if text.is_empty() {
                continue;
            }
            let p = toward(tip, next, 22.0);
            let l = dist(tip, next).max(1e-3);
            let (nx, ny) = (-(next.1 - tip.1) / l, (next.0 - tip.0) / l);
            d.text(p.0 + nx * 12.0, p.1 + ny * 12.0, text, FS_SMALL, false, 0, Paint::Muted);
        }
        if !r.label.is_empty() {
            let (cx, cy) = o.label_at[ri];
            let (w, h) = lsize[ri];
            d.rect(cx - w / 2.0, cy - h / 2.0, w, h, 4.0, Some(Paint::LabelBg), None);
            d.text(cx, cy, &r.label, FS_SMALL, false, 0, Paint::Muted);
        }
    }
    for (i, c) in g.classes.iter().enumerate() {
        let (w, h, head) = dims[i];
        let (x, y) = (o.pos[i].0 - w / 2.0, o.pos[i].1 - h / 2.0);
        d.rect(x, y, w, h, 4.0, Some(Paint::Node), Some(Paint::NodeBorder));
        let mut ty = y + 15.0;
        if !c.stereo.is_empty() {
            d.text(x + w / 2.0, ty - 1.0, &c.stereo, FS_SMALL, false, 0, Paint::Muted);
            ty += 14.0;
        }
        d.text(x + w / 2.0, ty, &c.name, FS, true, 0, Paint::Text);
        let mut sy = y + head;
        d.line(&[(x, sy), (x + w, sy)], Paint::NodeBorder, 1.0, false);
        for a in &c.attrs {
            d.text(x + 12.0, sy + 5.0 + ROW / 2.0, a, FS_SMALL, false, -1, Paint::Text);
            sy += ROW;
        }
        sy += 10.0;
        d.line(&[(x, sy), (x + w, sy)], Paint::NodeBorder, 1.0, false);
        for mt in &c.methods {
            d.text(x + 12.0, sy + 5.0 + ROW / 2.0, mt, FS_SMALL, false, -1, Paint::Text);
            sy += ROW;
        }
    }
    let pad = 8.0;
    d.shift(pad, pad);
    d.w = o.w + 2.0 * pad;
    d.h = o.h + 2.0 * pad;
    Ok(d)
}
