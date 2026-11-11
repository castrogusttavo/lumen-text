//! Compact binary form of the undo tree, for persistent undo (`undo_store.rs`).
//!
//! The history is only meaningful for the exact text it was recorded against, so the encoding
//! carries a hash of that text and [`Buffer::adopt_history`] refuses it for any other text.
//! Numbers are LEB128 varints; strings are a varint byte length plus UTF-8.

use super::{Buffer, Edit, EditKind, EditShape, Group, History, Node, Sel};

const MAGIC: &[u8; 8] = b"LTUNDO1\n";

/// Which version of an adopted history the file on disk holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedAt {
    /// The current text is what is on disk (a file that was just opened).
    Current,
    /// The file on disk is unchanged since the history was written: its saved point still holds.
    Recorded,
    /// Unknown (the file changed meanwhile): no state of the history counts as saved.
    Unknown,
}

/// FNV-1a over the text, mixed with its length.
pub fn hash_text<'a>(chunks: impl Iterator<Item = &'a str>) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    let mut n = 0u64;
    for c in chunks {
        for &b in c.as_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        n += c.len() as u64;
    }
    (h ^ n).wrapping_mul(0x100000001b3)
}

fn put(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_opt(out: &mut Vec<u8>, v: Option<usize>) {
    put(out, v.map_or(0, |x| x as u64 + 1));
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl Rd<'_> {
    fn u(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.b.get(self.i)?;
            self.i += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn n(&mut self) -> Option<usize> {
        usize::try_from(self.u()?).ok()
    }
    fn opt(&mut self) -> Option<Option<usize>> {
        Some(self.n()?.checked_sub(1))
    }
    fn s(&mut self) -> Option<String> {
        let n = self.n()?;
        let bytes = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        String::from_utf8(bytes.to_vec()).ok()
    }
    fn byte(&mut self) -> Option<u8> {
        let b = *self.b.get(self.i)?;
        self.i += 1;
        Some(b)
    }
}

fn kind_id(k: EditKind) -> u8 {
    match k {
        EditKind::Typing => 0,
        EditKind::Backspace => 1,
        EditKind::Delete => 2,
        EditKind::Other => 3,
    }
}

fn kind_of(b: u8) -> Option<EditKind> {
    Some(match b {
        0 => EditKind::Typing,
        1 => EditKind::Backspace,
        2 => EditKind::Delete,
        3 => EditKind::Other,
        _ => return None,
    })
}

/// Rough encoded size of a node (for trimming before encoding).
fn node_cost(n: &Node) -> usize {
    32 + n.group.edits.iter().map(|e| 12 + e.removed.len() + e.inserted.len()).sum::<usize>()
}

impl History {
    /// Path from a root to the current node, root first.
    fn chain_to_cur(&self) -> Vec<usize> {
        let mut v = Vec::new();
        let mut at = self.cur;
        while let Some(i) = at {
            v.push(i);
            at = self.nodes[i].parent;
        }
        v.reverse();
        v
    }

    /// A smaller, linear history that fits in about `max` bytes: other branches go first, then the
    /// redo chain, then the oldest steps of the undo chain. `None` when nothing fits.
    fn trimmed(&self, max: usize) -> Option<History> {
        let mut redo = Vec::new();
        let mut at = self.redo_target();
        while let Some(i) = at {
            redo.push(i);
            at = self.nodes[i].last_child;
        }
        let undo = self.chain_to_cur();
        let cost = |v: &[usize]| v.iter().map(|&i| node_cost(&self.nodes[i])).sum::<usize>();
        let mut redo_keep = redo.len();
        let mut from = 0;
        while cost(&undo[from..]) + cost(&redo[..redo_keep]) > max {
            if redo_keep > 0 {
                redo_keep -= 1;
            } else if from < undo.len() {
                from += 1;
            } else {
                return None;
            }
        }
        // Rebuild a linear history: undo[from..] then redo[..redo_keep].
        let keep: Vec<usize> = undo[from..].iter().chain(&redo[..redo_keep]).copied().collect();
        if keep.is_empty() {
            return None;
        }
        let cur_pos = undo.len() - from; // number of applied nodes kept
        let mut h = History::default();
        for (k, &old) in keep.iter().enumerate() {
            let g = &self.nodes[old].group;
            let group = Group {
                edits: g
                    .edits
                    .iter()
                    .map(|e| Edit {
                        at: e.at,
                        removed: e.removed.clone(),
                        inserted: e.inserted.clone(),
                        shape: e.shape,
                    })
                    .collect(),
                before: g.before,
                after: g.after,
                kind: g.kind,
                ver_before: g.ver_before,
                ver_after: g.ver_after,
            };
            let parent = k.checked_sub(1);
            h.nodes.push(Node { group, parent, children: Vec::new(), last_child: None });
            match parent {
                Some(p) => {
                    h.nodes[p].children.push(k);
                    h.nodes[p].last_child = Some(k);
                }
                None => {
                    h.roots.push(k);
                    h.root_last = Some(k);
                }
            }
        }
        h.cur = cur_pos.checked_sub(1);
        Some(h)
    }

    fn encode(&self, out: &mut Vec<u8>) {
        put_opt(out, self.cur);
        put_opt(out, self.root_last);
        put(out, self.roots.len() as u64);
        for &r in &self.roots {
            put(out, r as u64);
        }
        put(out, self.nodes.len() as u64);
        for n in &self.nodes {
            put_opt(out, n.parent);
            put(out, n.children.len() as u64);
            for &c in &n.children {
                put(out, c as u64);
            }
            put_opt(out, n.last_child);
            let g = &n.group;
            for v in [g.before.anchor, g.before.head, g.after.anchor, g.after.head] {
                put(out, v as u64);
            }
            out.push(kind_id(g.kind));
            put(out, g.ver_before);
            put(out, g.ver_after);
            put(out, g.edits.len() as u64);
            for e in &g.edits {
                put(out, e.at as u64);
                put_str(out, &e.removed);
                put_str(out, &e.inserted);
                match e.shape {
                    EditShape::Splice => out.push(0),
                    EditShape::Swap { a_lines, b_lines, a_chars, b_chars, eol } => {
                        out.push(1);
                        for v in [a_lines, b_lines, a_chars, b_chars, eol] {
                            put(out, v as u64);
                        }
                    }
                }
            }
        }
    }

    /// Decode and check the tree's shape (every index in range, parents and children agree).
    fn decode(r: &mut Rd) -> Option<History> {
        let cur = r.opt()?;
        let root_last = r.opt()?;
        let nr = r.n()?;
        let mut roots = Vec::with_capacity(nr.min(1 << 16));
        for _ in 0..nr {
            roots.push(r.n()?);
        }
        let nn = r.n()?;
        let mut nodes = Vec::with_capacity(nn.min(1 << 16));
        for _ in 0..nn {
            let parent = r.opt()?;
            let nc = r.n()?;
            let mut children = Vec::with_capacity(nc.min(1 << 12));
            for _ in 0..nc {
                children.push(r.n()?);
            }
            let last_child = r.opt()?;
            let before = Sel { anchor: r.n()?, head: r.n()? };
            let after = Sel { anchor: r.n()?, head: r.n()? };
            let kind = kind_of(r.byte()?)?;
            let ver_before = r.u()?;
            let ver_after = r.u()?;
            let ne = r.n()?;
            let mut edits = Vec::with_capacity(ne.min(1 << 12));
            for _ in 0..ne {
                let at = r.n()?;
                let removed = r.s()?;
                let inserted = r.s()?;
                let shape = match r.byte()? {
                    0 => EditShape::Splice,
                    1 => EditShape::Swap {
                        a_lines: r.n()?,
                        b_lines: r.n()?,
                        a_chars: r.n()?,
                        b_chars: r.n()?,
                        eol: r.n()?,
                    },
                    _ => return None,
                };
                edits.push(Edit { at, removed, inserted, shape });
            }
            nodes.push(Node {
                group: Group { edits, before, after, kind, ver_before, ver_after },
                parent,
                children,
                last_child,
            });
        }
        let ok = |i: &usize| *i < nn;
        let shape_ok = cur.is_none_or(|c| c < nn)
            && root_last.is_none_or(|c| roots.contains(&c))
            && roots.iter().all(|r| ok(r) && nodes[*r].parent.is_none())
            && nodes.iter().enumerate().all(|(i, n)| {
                // Parents always come first, which also rules out cycles.
                n.parent.is_none_or(|p| p < i && nodes[p].children.contains(&i))
                    && n.children.iter().all(|c| ok(c) && nodes[*c].parent == Some(i))
                    && n.last_child.is_none_or(|c| n.children.contains(&c))
            });
        shape_ok.then_some(History { nodes, cur, roots, root_last })
    }

    /// Add `d` to every version number (see [`Buffer::adopt_history`]).
    fn shift_versions(&mut self, d: u64) {
        for n in &mut self.nodes {
            n.group.ver_before = n.group.ver_before.wrapping_add(d);
            n.group.ver_after = n.group.ver_after.wrapping_add(d);
        }
    }
}

impl Buffer {
    /// Hash of the whole text (what a persisted history is checked against).
    pub fn content_hash(&self) -> u64 {
        hash_text(self.rope.chunks())
    }

    /// Is there anything to undo or redo?
    #[cfg(test)]
    pub fn has_history(&self) -> bool {
        !self.hist.nodes.is_empty()
    }

    /// The undo history and the text hash it belongs to, encoded in at most about `max` bytes
    /// (older steps and other branches are dropped to fit). `None` when there is no history.
    pub fn history_bytes(&self, max: usize) -> Option<Vec<u8>> {
        if self.hist.nodes.is_empty() {
            return None;
        }
        let total: usize = self.hist.nodes.iter().map(node_cost).sum();
        let trimmed;
        let hist = if total > max {
            trimmed = self.hist.trimmed(max)?;
            &trimmed
        } else {
            &self.hist
        };
        let mut out = Vec::with_capacity(total.min(max) + 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.content_hash().to_le_bytes());
        put(&mut out, self.version);
        put(&mut out, self.counter);
        put(&mut out, self.saved_version);
        hist.encode(&mut out);
        (out.len() <= max.saturating_add(max / 4)).then_some(out)
    }

    /// Hash of the text a history encoded by [`history_bytes`](Self::history_bytes) belongs to.
    pub fn history_hash(data: &[u8]) -> Option<u64> {
        let h = data.strip_prefix(MAGIC.as_slice())?.get(..8)?;
        Some(u64::from_le_bytes(h.try_into().ok()?))
    }

    /// Replace the undo history with an encoded one, if it was recorded against exactly this text.
    /// The text itself is not touched. Its version numbers are shifted past every number this
    /// buffer used, so the version changes once (caches keyed by it refresh) and never repeats.
    pub fn adopt_history(&mut self, data: &[u8], saved: SavedAt) -> bool {
        if Self::history_hash(data) != Some(self.content_hash()) {
            return false;
        }
        let mut r = Rd { b: data, i: MAGIC.len() + 8 };
        let (Some(version), Some(counter), Some(saved_version)) = (r.u(), r.u(), r.u()) else { return false };
        let Some(mut hist) = History::decode(&mut r) else { return false };
        if r.i != data.len() {
            return false;
        }
        let d = self.counter.max(self.version).wrapping_add(1);
        hist.shift_versions(d);
        self.hist = hist;
        self.open = false;
        self.compound = false;
        self.version = version.wrapping_add(d);
        self.counter = counter.wrapping_add(d);
        self.saved_version = match saved {
            SavedAt::Current => self.version,
            SavedAt::Recorded => saved_version.wrapping_add(d),
            // A number no state of the history has.
            SavedAt::Unknown => self.counter.wrapping_add(1 << 62),
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(b: &mut Buffer, s: usize, e: usize, t: &str) {
        b.replace(s, e, t, EditKind::Other, Sel::caret(s));
        b.set_after(Sel::caret(s + t.chars().count()));
    }

    #[test]
    fn history_roundtrips_with_branches_and_redo() {
        let mut b = Buffer::from_str("hello\n");
        rep(&mut b, 5, 5, " world");
        rep(&mut b, 0, 0, "> ");
        b.undo();
        rep(&mut b, 0, 0, "# "); // a second branch from the same point
        assert_eq!(b.to_string(), "# hello world\n");
        let data = b.history_bytes(1 << 20).unwrap();
        // A fresh buffer with the same text (the file reopened) adopts it.
        let mut c = Buffer::from_str("# hello world\n");
        assert!(c.adopt_history(&data, SavedAt::Current));
        assert!(!c.is_dirty());
        assert_eq!(c.undo_branches().len(), 0);
        c.undo();
        assert_eq!(c.to_string(), "hello world\n");
        assert!(c.is_dirty());
        assert_eq!(c.undo_branches().len(), 2, "both branches survived");
        c.undo();
        assert_eq!(c.to_string(), "hello\n");
        assert!(c.undo().is_none());
        c.redo();
        c.redo();
        assert_eq!(c.to_string(), "# hello world\n", "redo follows the last branch");
        assert!(!c.is_dirty(), "back at the saved text");
        let alt = c.undo_branches();
        assert!(alt.is_empty());
        c.undo();
        let other = c.undo_branches().into_iter().map(|(i, _)| i).find(|&i| i != 2).unwrap();
        c.redo_branch(other);
        assert_eq!(c.to_string(), "> hello world\n");
        // Editing after adopting keeps working (new versions never collide with old ones).
        rep(&mut c, 0, 0, "x");
        c.undo();
        assert_eq!(c.to_string(), "> hello world\n");
    }

    #[test]
    fn swapped_lines_roundtrip() {
        let mut b = Buffer::from_str("aa\nbbb\nc\nd");
        b.tag_swap(1, 2, 2, 5, 1);
        b.replace(0, 8, "bbb\nc\naa", EditKind::Other, Sel::caret(0));
        let data = b.history_bytes(1 << 20).unwrap();
        let mut c = Buffer::from_str("bbb\nc\naa\nd");
        assert!(c.adopt_history(&data, SavedAt::Current));
        c.undo();
        assert_eq!(c.to_string(), "aa\nbbb\nc\nd");
        let u = c.take_edits();
        assert_eq!(u.last().unwrap().line_ops(), vec![super::super::LineOp::Swap { first: 0, a_lines: 2, b_lines: 1 }]);
    }

    #[test]
    fn another_text_is_refused() {
        let mut b = Buffer::from_str("a");
        rep(&mut b, 1, 1, "b");
        let data = b.history_bytes(1 << 20).unwrap();
        let mut c = Buffer::from_str("ab ");
        assert!(!c.adopt_history(&data, SavedAt::Current));
        assert!(!c.has_history());
        // Garbage and truncated data are refused too.
        let mut d = Buffer::from_str("ab");
        assert!(!d.adopt_history(&data[..data.len() - 1], SavedAt::Current));
        assert!(!d.adopt_history(b"junk", SavedAt::Current));
        assert!(d.adopt_history(&data, SavedAt::Current));
        assert!(Buffer::from_str("x").history_bytes(1 << 20).is_none(), "nothing to keep");
    }

    #[test]
    fn saved_point_follows_what_is_on_disk() {
        let mut b = Buffer::from_str("one");
        rep(&mut b, 3, 3, " two");
        b.saved_version = b.version; // pretend " two" was saved
        rep(&mut b, 7, 7, " three"); // unsaved
        let data = b.history_bytes(1 << 20).unwrap();
        // Restored dirty text, file unchanged since: one undo reaches the saved state.
        let mut c = Buffer::from_str("one two three");
        assert!(c.adopt_history(&data, SavedAt::Recorded));
        assert!(c.is_dirty());
        c.undo();
        assert!(!c.is_dirty());
        // The file changed meanwhile: nothing is clean.
        let mut c = Buffer::from_str("one two three");
        assert!(c.adopt_history(&data, SavedAt::Unknown));
        assert!(c.is_dirty());
        c.undo();
        assert!(c.is_dirty());
        c.undo();
        assert!(c.is_dirty());
    }

    #[test]
    fn big_histories_are_trimmed_to_the_recent_undo_chain() {
        let mut b = Buffer::from_str("");
        for i in 0..200 {
            let at = b.len_chars();
            rep(&mut b, at, at, &format!("{i:03}{}\n", "x".repeat(100)));
        }
        let full = b.history_bytes(1 << 20).unwrap();
        let small = b.history_bytes(4000).unwrap();
        assert!(small.len() < full.len() && small.len() <= 5000, "{} vs {}", small.len(), full.len());
        let mut c = Buffer::from_str(&b.to_string());
        assert!(c.adopt_history(&small, SavedAt::Current));
        let mut steps = 0;
        while c.undo().is_some() {
            steps += 1;
        }
        assert!(steps > 5 && steps < 200, "{steps}");
        assert!(c.to_string().starts_with("000"), "the oldest lines stay: only recent steps are undone");
        // A single step bigger than the cap is not kept at all.
        let mut d = Buffer::from_str("");
        rep(&mut d, 0, 0, &"y".repeat(10_000));
        assert!(d.history_bytes(1000).is_none());
    }
}
