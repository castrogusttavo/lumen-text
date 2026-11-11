//! `sequenceDiagram`.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct Actor {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) person: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NotePos {
    Left,
    Right,
    Over,
}

#[derive(Clone, Debug)]
pub(crate) enum Item {
    Msg { from: usize, to: usize, text: String, dotted: bool, end: Mark, start: Mark },
    Note { pos: NotePos, a: usize, b: usize, text: String },
    Open { kind: String, label: String },
    Else { label: String },
    Close,
}

#[derive(Debug, Default)]
pub(crate) struct Seq {
    pub(crate) actors: Vec<Actor>,
    pub(crate) items: Vec<Item>,
    pub(crate) autonumber: bool,
    pub(crate) title: String,
}

impl Seq {
    fn actor(&mut self, id: &str) -> usize {
        let id = id.trim();
        if let Some(i) = self.actors.iter().position(|a| a.id == id) {
            return i;
        }
        self.actors.push(Actor { id: id.to_string(), label: id.to_string(), person: false });
        self.actors.len() - 1
    }
}

/// Arrows, longest first: (token, dotted, end marker, start marker).
const ARROWS: &[(&str, bool, Mark, Mark)] = &[
    ("<<-->>", true, Mark::Arrow, Mark::Arrow),
    ("<<->>", false, Mark::Arrow, Mark::Arrow),
    ("-->>", true, Mark::Arrow, Mark::None),
    ("->>", false, Mark::Arrow, Mark::None),
    ("--x", true, Mark::Cross, Mark::None),
    ("-x", false, Mark::Cross, Mark::None),
    ("--)", true, Mark::Open, Mark::None),
    ("-)", false, Mark::Open, Mark::None),
    ("-->", true, Mark::None, Mark::None),
    ("->", false, Mark::None, Mark::None),
];

pub(crate) fn parse(lines: &[String]) -> Seq {
    let mut q = Seq::default();
    for line in lines.iter().skip(1) {
        let l = line.trim().trim_end_matches(';');
        let (kw, rest) = l.split_once(char::is_whitespace).map_or((l, ""), |(a, b)| (a, b.trim()));
        match kw {
            "participant" | "actor" => {
                let (id, label) = match rest.split_once(" as ") {
                    Some((a, b)) => (a.trim(), b.trim()),
                    None => (rest, rest),
                };
                if id.is_empty() {
                    continue;
                }
                let i = q.actor(id);
                q.actors[i].label = text_lines(label).join(" ");
                q.actors[i].person = kw == "actor";
            }
            "autonumber" => q.autonumber = true,
            "title" => q.title = rest.trim_start_matches(':').trim().to_string(),
            "loop" | "alt" | "opt" | "par" | "critical" | "break" | "rect" => {
                q.items.push(Item::Open { kind: kw.to_string(), label: rest.to_string() })
            }
            "else" | "and" | "option" => q.items.push(Item::Else { label: rest.to_string() }),
            "end" => q.items.push(Item::Close),
            "activate" | "deactivate" | "create" | "destroy" | "box" | "links" | "link" | "properties" => {}
            _ if kw.eq_ignore_ascii_case("note") => {
                let Some((head, text)) = rest.split_once(':') else { continue };
                let head = head.trim();
                let (pos, who) = if let Some(w) = head.strip_prefix("right of") {
                    (NotePos::Right, w)
                } else if let Some(w) = head.strip_prefix("left of") {
                    (NotePos::Left, w)
                } else if let Some(w) = head.strip_prefix("over") {
                    (NotePos::Over, w)
                } else {
                    continue;
                };
                let mut ids = who.split(',').map(str::trim).filter(|s| !s.is_empty());
                let Some(a) = ids.next() else { continue };
                let a = q.actor(a);
                let b = ids.next().map_or(a, |b| q.actor(b));
                q.items.push(Item::Note { pos, a, b, text: text.trim().to_string() });
            }
            _ => {
                let Some((left, text)) = l.split_once(':') else { continue };
                let Some(p) = left.find(['-', '<']) else { continue };
                let from = left[..p].trim();
                let arrow = &left[p..];
                let Some(&(tok, dotted, end, start)) = ARROWS.iter().find(|a| arrow.starts_with(a.0)) else { continue };
                let to = arrow[tok.len()..].trim().trim_start_matches(['+', '-']).trim();
                if from.is_empty() || to.is_empty() || q.items.len() > 2000 {
                    continue;
                }
                let (from, to) = (q.actor(from), q.actor(to));
                q.items.push(Item::Msg { from, to, text: text.trim().to_string(), dotted, end, start });
            }
        }
    }
    q
}

pub(super) fn build(lines: &[String], m: Meas) -> Result<Diagram, String> {
    let q = parse(lines);
    let n = q.actors.len();
    if n == 0 {
        return Err("empty sequence diagram".into());
    }
    let box_h = 38.0;
    let bw: Vec<f32> = q.actors.iter().map(|a| (m(&a.label, true, FS) + 28.0).max(84.0)).collect();
    // Distance from actor i to i+1, grown until every message label fits between its ends.
    let mut gap: Vec<f32> = (0..n.saturating_sub(1)).map(|i| bw[i] / 2.0 + 36.0 + bw[i + 1] / 2.0).collect();
    let mut extra_right = 0.0f32;
    let mut num = 0;
    let texts: Vec<String> = q
        .items
        .iter()
        .map(|it| match it {
            Item::Msg { text, .. } => {
                num += 1;
                let t = text_lines(text).join(" ");
                if q.autonumber { format!("{num}. {t}") } else { t }
            }
            _ => String::new(),
        })
        .collect();
    for (it, t) in q.items.iter().zip(&texts) {
        if let Item::Msg { from, to, .. } = it {
            let need = m(t, false, FS_SMALL) + 28.0;
            let (a, b) = ((*from).min(*to), (*from).max(*to));
            if a == b {
                // Self message: the loop and its label sit right of the lifeline.
                if a + 1 < n {
                    gap[a] = gap[a].max(need + 30.0);
                } else {
                    extra_right = extra_right.max(need + 30.0);
                }
                continue;
            }
            let have: f32 = gap[a..b].iter().sum();
            if have < need {
                let add = (need - have) / (b - a) as f32;
                for g in &mut gap[a..b] {
                    *g += add;
                }
            }
        } else if let Item::Note { pos, a, b, text } = it {
            let w = m(&text_lines(text).join(" "), false, FS_SMALL) + 24.0;
            match pos {
                NotePos::Right if *a + 1 < n => gap[*a] = gap[*a].max(w + bw[*a] / 2.0 + 16.0),
                NotePos::Right => extra_right = extra_right.max(w + 16.0),
                NotePos::Over if a != b => {
                    let (lo, hi) = ((*a).min(*b), (*a).max(*b));
                    let have: f32 = gap[lo..hi].iter().sum();
                    if have + 40.0 < w {
                        let add = (w - have - 40.0) / (hi - lo) as f32;
                        for g in &mut gap[lo..hi] {
                            *g += add;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let left_need = q
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Note { pos: NotePos::Left, a: 0, text, .. } => {
                Some(m(&text_lines(text).join(" "), false, FS_SMALL) + 40.0)
            }
            _ => None,
        })
        .fold(0.0f32, f32::max);
    let x0 = (bw[0] / 2.0).max(left_need) + 10.0;
    let mut xs = vec![x0];
    for g in &gap {
        let last = xs.last().copied().unwrap_or(x0);
        xs.push(last + g);
    }
    let mut d = Diagram::default();
    let title_h = if q.title.is_empty() { 0.0 } else { 30.0 };
    let top = title_h;
    let mut y = top + box_h + 26.0;
    let left = xs[0] - bw[0] / 2.0 - 8.0;
    let right = xs[n - 1] + bw[n - 1] / 2.0 + 8.0 + extra_right;
    // Rows: messages, notes and frames (frames are drawn after, behind nothing: just outlines).
    let mut frames: Vec<(String, String, f32, Vec<(f32, String)>)> = vec![];
    let mut done_frames: Vec<(String, String, f32, f32, Vec<(f32, String)>, usize)> = vec![];
    for (it, t) in q.items.iter().zip(&texts) {
        match it {
            Item::Msg { from, to, dotted, end, start, .. } => {
                let (xa, xb) = (xs[*from], xs[*to]);
                if from == to {
                    let ly = y + 8.0;
                    d.text(xa + 12.0, ly, t, FS_SMALL, false, -1, Paint::Text);
                    let (y1, y2) = (y + 20.0, y + 46.0);
                    let pts = [(xa, y1), (xa + 34.0, y1), (xa + 34.0, y2), (xa + marker_len(*end), y2)];
                    d.curve(&pts, Paint::Line, 1.4, *dotted, 8.0);
                    d.marker((xa, y2), (xa + 34.0, y2), *end, Paint::Line);
                    y += 62.0;
                } else {
                    let ly = y + 6.0;
                    d.text((xa + xb) / 2.0, ly, t, FS_SMALL, false, 0, Paint::Text);
                    let ay = y + 22.0;
                    let dir = if xb > xa { 1.0 } else { -1.0 };
                    let a = xa + dir * marker_len(*start);
                    let b = xb - dir * marker_len(*end);
                    d.line(&[(a, ay), (b, ay)], Paint::Line, 1.4, *dotted);
                    d.marker((xb, ay), (xa, ay), *end, Paint::Line);
                    d.marker((xa, ay), (xb, ay), *start, Paint::Line);
                    y += 40.0;
                }
            }
            Item::Note { pos, a, b, text } => {
                let lines = text_lines(text);
                let tw = lines.iter().map(|l| m(l, false, FS_SMALL)).fold(0.0f32, f32::max);
                let h = lines.len() as f32 * LINE_SMALL + 14.0;
                let w = tw + 24.0;
                let (nx, nw) = match pos {
                    NotePos::Right => (xs[*a] + 12.0, w),
                    NotePos::Left => (xs[*a] - 12.0 - w, w),
                    NotePos::Over => {
                        let (lo, hi) = (xs[(*a).min(*b)], xs[(*a).max(*b)]);
                        let span = (hi - lo + 40.0).max(w);
                        ((lo + hi) / 2.0 - span / 2.0, span)
                    }
                };
                let ny = y + 4.0;
                d.rect(nx, ny, nw, h, 3.0, Some(Paint::Note), Some(Paint::NoteBorder));
                d.text_block(nx + nw / 2.0, ny + h / 2.0, &lines, FS_SMALL, false, Paint::Text);
                y += h + 16.0;
            }
            Item::Open { kind, label } => {
                frames.push((kind.clone(), label.clone(), y, vec![]));
                y += 30.0;
            }
            Item::Else { label } => {
                if let Some(f) = frames.last_mut() {
                    f.3.push((y, label.clone()));
                    y += 30.0;
                }
            }
            Item::Close => {
                if let Some((kind, label, fy, elses)) = frames.pop() {
                    let depth = frames.len();
                    done_frames.push((kind, label, fy, y + 4.0, elses, depth));
                    y += 14.0;
                }
            }
        }
    }
    // Frames left open close at the end.
    while let Some((kind, label, fy, elses)) = frames.pop() {
        let depth = frames.len();
        done_frames.push((kind, label, fy, y, elses, depth));
    }
    let bottom = y + 6.0;
    // Lifelines first (under everything else drawn later? shapes paint in order, so put them in
    // front of the list).
    let mut under = Diagram::default();
    for (i, a) in q.actors.iter().enumerate() {
        let x = xs[i];
        under.line(&[(x, top + box_h), (x, bottom)], Paint::ClusterBorder, 1.2, true);
        for by in [top, bottom] {
            if a.person {
                under.rect(x - bw[i] / 2.0, by, bw[i], box_h, box_h / 2.0, Some(Paint::Node), Some(Paint::NodeBorder));
            } else {
                under.rect(x - bw[i] / 2.0, by, bw[i], box_h, 4.0, Some(Paint::Node), Some(Paint::NodeBorder));
            }
            under.text(x, by + box_h / 2.0, &a.label, FS, true, 0, Paint::Text);
        }
    }
    for (kind, label, fy, fy2, elses, depth) in &done_frames {
        let inset = *depth as f32 * 6.0;
        let (fx0, fx1) = (left + inset, right - inset);
        let fill = if kind == "rect" { Some(Paint::Cluster) } else { None };
        under.rect(fx0, *fy, fx1 - fx0, fy2 - fy, 4.0, fill, Some(Paint::ClusterBorder));
        if kind != "rect" {
            let tw = m(kind, true, FS_SMALL) + 16.0;
            under.poly(
                &[(fx0, *fy), (fx0 + tw, *fy), (fx0 + tw, fy + 14.0), (fx0 + tw - 6.0, fy + 20.0), (fx0, fy + 20.0)],
                Some(Paint::Cluster),
                Some(Paint::ClusterBorder),
                1.0,
            );
            under.text(fx0 + 8.0, fy + 10.0, kind, FS_SMALL, true, -1, Paint::Muted);
            if !label.is_empty() {
                under.text(fx0 + tw + 8.0, fy + 10.0, &format!("[{label}]"), FS_SMALL, false, -1, Paint::Muted);
            }
        }
        for (ey, el) in elses {
            under.line(&[(fx0, *ey), (fx1, *ey)], Paint::ClusterBorder, 1.0, true);
            if !el.is_empty() {
                under.text(fx0 + 8.0, ey + 11.0, &format!("[{el}]"), FS_SMALL, false, -1, Paint::Muted);
            }
        }
    }
    if !q.title.is_empty() {
        under.text((left + right) / 2.0, 12.0, &q.title, FS_TITLE, true, 0, Paint::Text);
    }
    under.shapes.append(&mut d.shapes);
    under.labels.append(&mut d.labels);
    let mut d = under;
    d.shift(4.0 - left.min(0.0), 4.0);
    d.w = right - left.min(0.0) + 8.0;
    d.h = bottom + box_h + 8.0;
    Ok(d)
}
