//! Commit graph of the History tab, as plain data.
//!
//! Lanes are assigned one commit at a time over `git log --topo-order` (children always come
//! before their parents), so pages are laid out as they stream in and earlier rows are never
//! revisited. Every row stores its node column and the line segments crossing it, in two halves:
//!
//! * the upper half joins the row's top edge to its centre: lanes passing through, and the lanes
//!   of the children that end in this commit (they bend into the node);
//! * the lower half joins the centre to the bottom edge: lanes passing through, the first parent
//!   continuing the commit's own lane, and one line per extra parent of a merge.
//!
//! The columns at the bottom of a row are exactly the columns at the top of the next one, with
//! the same colours (checked by the tests on random histories). Freed columns are reused by the
//! next branch that needs one, so the graph stays as narrow as the history allows.

use crate::theme::Rgba;

/// Number of distinct lane colours (the palette itself depends on the theme: [`lane_colors`]).
pub const LANE_COLORS: usize = 8;

/// One line segment inside a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    /// Column where it starts: the top edge (upper half) or the centre (lower half).
    pub from: u16,
    /// Column where it ends: the centre (upper half) or the bottom edge (lower half).
    pub to: u16,
    /// Index into the lane palette.
    pub color: u8,
    pub lower: bool,
}

/// Graph data of one commit row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GRow {
    /// Column of the commit's node.
    pub col: u16,
    pub color: u8,
    /// Two or more parents.
    pub merge: bool,
    /// This row's segments: `Graph::segs[start..start + len]`.
    pub start: u32,
    pub len: u32,
    /// Columns this row uses (node and segments).
    pub width: u16,
}

/// Rows and segments of the loaded part of the history (segments of all rows in one vector:
/// no allocation per row).
#[derive(Default, Debug)]
pub struct Graph {
    pub rows: Vec<GRow>,
    pub segs: Vec<Seg>,
}

impl Graph {
    pub fn segs_of(&self, i: usize) -> &[Seg] {
        match self.rows.get(i) {
            Some(r) => &self.segs[r.start as usize..(r.start + r.len) as usize],
            None => &[],
        }
    }

    /// Append rows laid out by a [`Lanes`] whose `start` offsets count from 0 (a page).
    pub fn append(&mut self, rows: Vec<GRow>, segs: Vec<Seg>) {
        let base = self.segs.len() as u32;
        self.rows.extend(rows.into_iter().map(|mut r| {
            r.start += base;
            r
        }));
        self.segs.extend(segs);
    }

    pub fn clear(&mut self) {
        self.rows = Vec::new();
        self.segs = Vec::new();
    }

    /// Columns (and colours) of the lines that leave row `i` at its bottom edge: what crosses
    /// the file rows listed under an expanded commit.
    pub fn below(&self, i: usize) -> Vec<(u16, u8)> {
        let mut v: Vec<(u16, u8)> = self.segs_of(i).iter().filter(|s| s.lower).map(|s| (s.to, s.color)).collect();
        v.sort_unstable();
        v.dedup_by_key(|x| x.0);
        v
    }
}

/// Lane state carried from one row to the next.
#[derive(Default, Debug)]
pub struct Lanes {
    /// Per column: the commit the lane is waiting for, and its colour.
    lanes: Vec<Option<(String, u8)>>,
    next_color: u8,
}

impl Lanes {
    fn new_color(&mut self) -> u8 {
        let c = self.next_color;
        self.next_color = (c + 1) % LANE_COLORS as u8;
        c
    }

    /// The leftmost free column that is not in `avoid` (a new one at the right when none is).
    fn free_slot(&mut self, avoid: &[usize]) -> usize {
        match (0..self.lanes.len()).find(|k| self.lanes[*k].is_none() && !avoid.contains(k)) {
            Some(k) => k,
            None => {
                self.lanes.push(None);
                self.lanes.len() - 1
            }
        }
    }

    fn waiting_for(&self, hash: &str) -> Option<usize> {
        self.lanes.iter().position(|l| l.as_ref().is_some_and(|(h, _)| h == hash))
    }

    /// Columns currently in use (tests, diagnostics).
    pub fn active(&self) -> usize {
        self.lanes.iter().filter(|l| l.is_some()).count()
    }

    /// Lay out the next commit in topological order; its segments are appended to `segs`.
    pub fn push(&mut self, hash: &str, parents: &[&str], segs: &mut Vec<Seg>) -> GRow {
        let start = segs.len();
        // Lanes of the children that were waiting for this commit: the leftmost one carries on.
        let hits: Vec<usize> =
            (0..self.lanes.len()).filter(|&k| self.lanes[k].as_ref().is_some_and(|(h, _)| h == hash)).collect();
        let (col, color) = match hits.first() {
            Some(&k) => (k, self.lanes[k].as_ref().map_or(0, |l| l.1)),
            // A branch tip: nothing points at it yet.
            None => {
                let k = self.free_slot(&[]);
                (k, self.new_color())
            }
        };
        for (k, l) in self.lanes.iter().enumerate() {
            if let Some((h, c)) = l {
                let to = if h == hash { col } else { k };
                segs.push(Seg { from: k as u16, to: to as u16, color: *c, lower: false });
            }
        }
        for &k in &hits {
            self.lanes[k] = None;
        }
        if let Some(p0) = parents.first() {
            self.lanes[col] = Some((p0.to_string(), color));
        }
        // Extra parents of a merge: join a lane already waiting for that parent, or open one.
        // Columns freed by this very row are not reused (a line would come in and go out of the
        // same column with two colours).
        let mut fresh: Vec<usize> = Vec::new();
        let mut joins: Vec<(usize, u8)> = Vec::new();
        for p in parents.iter().skip(1) {
            if *p == parents[0] {
                continue;
            }
            match self.waiting_for(p) {
                Some(j) => joins.push((j, self.lanes[j].as_ref().map_or(0, |l| l.1))),
                None => {
                    let mut avoid = hits.clone();
                    avoid.push(col);
                    let j = self.free_slot(&avoid);
                    let c = self.new_color();
                    self.lanes[j] = Some((p.to_string(), c));
                    fresh.push(j);
                    joins.push((j, c));
                }
            }
        }
        for (k, l) in self.lanes.iter().enumerate() {
            if let Some((_, c)) = l
                && !fresh.contains(&k)
            {
                segs.push(Seg { from: k as u16, to: k as u16, color: *c, lower: true });
            }
        }
        for (j, c) in joins {
            segs.push(Seg { from: col as u16, to: j as u16, color: c, lower: true });
        }
        while self.lanes.last().is_some_and(|l| l.is_none()) {
            self.lanes.pop();
        }
        let width = segs[start..].iter().map(|s| s.from.max(s.to) + 1).max().unwrap_or(0).max(col as u16 + 1);
        GRow {
            col: col as u16,
            color,
            merge: parents.len() > 1,
            start: start as u32,
            len: (segs.len() - start) as u32,
            width,
        }
    }
}

// ───────────── ref labels ─────────────

/// What a label on a commit row names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChipKind {
    /// Detached HEAD.
    Head,
    /// The checked-out branch.
    Current,
    Branch,
    Tag,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub kind: ChipKind,
    pub name: String,
}

/// Parse `%D` printed with `--decorate=full` ("HEAD -> refs/heads/main, tag: refs/tags/v1,
/// refs/remotes/origin/main"): checked-out branch first, then branches, tags and remotes.
/// `origin/HEAD`, the stash and other refs (notes, ...) are left out.
pub fn parse_decorations(d: &str) -> Vec<Chip> {
    let mut v: Vec<Chip> = Vec::new();
    for item in d.split(", ").map(str::trim).filter(|s| !s.is_empty()) {
        let chip = if item == "HEAD" {
            Chip { kind: ChipKind::Head, name: "HEAD".into() }
        } else if let Some(b) = item.strip_prefix("HEAD -> ") {
            Chip { kind: ChipKind::Current, name: b.strip_prefix("refs/heads/").unwrap_or(b).to_string() }
        } else if let Some(t) = item.strip_prefix("tag: ") {
            Chip { kind: ChipKind::Tag, name: t.strip_prefix("refs/tags/").unwrap_or(t).to_string() }
        } else if let Some(b) = item.strip_prefix("refs/heads/") {
            Chip { kind: ChipKind::Branch, name: b.to_string() }
        } else if let Some(r) = item.strip_prefix("refs/remotes/") {
            if r.ends_with("/HEAD") {
                continue;
            }
            Chip { kind: ChipKind::Remote, name: r.to_string() }
        } else {
            continue;
        };
        v.push(chip);
    }
    v.sort_by(|a, b| a.kind.cmp(&b.kind));
    v
}

// ───────────── log lines ─────────────

/// `git log` format of the graph: hash, short hash, parents, author, commit time, decorations,
/// subject (NUL-separated: none of them can contain a NUL).
pub const GRAPH_FORMAT: &str = "--format=%H%x00%h%x00%P%x00%an%x00%ct%x00%D%x00%s";

/// One parsed line of [`GRAPH_FORMAT`]: the entry, its parents and its labels.
pub fn parse_graph_line(line: &str) -> Option<(super::LogEntry, Vec<&str>, Vec<Chip>)> {
    let mut p = line.split('\0');
    let hash = p.next().filter(|h| !h.is_empty())?;
    let short = p.next()?;
    let parents: Vec<&str> = p.next()?.split_whitespace().collect();
    let author = p.next()?;
    let ts = p.next()?.parse().ok()?;
    let deco = p.next()?;
    let subject = p.next().unwrap_or("");
    let e = super::LogEntry {
        hash: hash.to_string(),
        short: short.to_string(),
        author: author.to_string(),
        ts,
        subject: subject.to_string(),
    };
    Some((e, parents, parse_decorations(deco)))
}

// ───────────── colours ─────────────

/// Base hues of the lanes, in order: blue, orange, green, magenta, teal, yellow, red, violet.
const BASE: [Rgba; LANE_COLORS] = [
    Rgba(0x4f, 0x8f, 0xe8, 255),
    Rgba(0xe8, 0x87, 0x3a, 255),
    Rgba(0x3f, 0xb2, 0x7f, 255),
    Rgba(0xd4, 0x5d, 0xb5, 255),
    Rgba(0x2b, 0xb3, 0xc0, 255),
    Rgba(0xc9, 0xa2, 0x27, 255),
    Rgba(0xe0, 0x52, 0x52, 255),
    Rgba(0x8e, 0x6b, 0xe0, 255),
];

/// Minimum contrast of a lane against the panel and its highlighted rows (WCAG asks 3:1 for
/// graphics that carry meaning).
pub const LANE_CONTRAST: f32 = 3.0;

/// Lane colours for a panel background `bg` (and its highlighted rows `on`): each hue is pushed
/// toward white on dark themes and toward black on light ones until it reaches
/// [`LANE_CONTRAST`] on all of them.
pub fn lane_colors(bg: Rgba, on: &[Rgba]) -> [Rgba; LANE_COLORS] {
    let mut all = vec![bg];
    all.extend(on.iter().map(|c| c.over(bg)));
    BASE.map(|c| c.readable_on(&all, LANE_CONTRAST))
}
