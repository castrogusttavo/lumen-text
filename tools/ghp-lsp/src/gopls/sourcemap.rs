//! Bidirectional position mapping between a `.ghp` source and its virtual `.go` document.
//!
//! The virtual document copies every Go region of the page verbatim, so the mapping is a list of
//! equal-length segments `(ghp_start, go_start, len)`. Anything outside a segment (HTML text,
//! the scaffolding we generate) has no counterpart and maps to `None`.
//!
//! Offsets are UTF-8 byte offsets. LSP positions use UTF-16 code units for the column, which
//! [`TextIndex`] converts.

/// LSP position: zero-based line, UTF-16 column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// LSP range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Line table of one text, converting between byte offsets and LSP positions.
#[derive(Clone, Debug)]
pub struct TextIndex {
    text: String,
    line_starts: Vec<usize>,
}

impl TextIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        TextIndex { text: text.to_string(), line_starts }
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Byte offset to LSP position. Offsets past the end clamp to the end; offsets inside a
    /// multi-byte character round down to its start.
    pub fn position(&self, offset: usize) -> Position {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = match self.line_starts.binary_search(&offset) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let character = self.text[self.line_starts[line]..offset].chars().map(|c| c.len_utf16() as u32).sum();
        Position { line: line as u32, character }
    }

    /// LSP position to byte offset. Columns past the end of the line clamp to the line end
    /// (before its `\n`); a column in the middle of a surrogate pair rounds down.
    pub fn offset(&self, pos: Position) -> Option<usize> {
        let line = pos.line as usize;
        let start = *self.line_starts.get(line)?;
        let end = self.line_starts.get(line + 1).map(|e| e - 1).unwrap_or(self.text.len());
        let mut units = 0u32;
        let mut off = start;
        for c in self.text[start..end].chars() {
            if units + c.len_utf16() as u32 > pos.character {
                break;
            }
            units += c.len_utf16() as u32;
            off += c.len_utf8();
        }
        Some(off)
    }
}

/// A run of text present, byte for byte, in both documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub ghp_start: usize,
    pub go_start: usize,
    pub len: usize,
}

/// The mapping between one `.ghp` text and the virtual `.go` text generated from it.
#[derive(Clone, Debug)]
pub struct SourceMap {
    by_ghp: Vec<Segment>,
    by_go: Vec<Segment>,
    /// Zero-length segments: the place where Go text would go in a tag that has none yet
    /// (`<go= | />`). They map a cursor (and an empty edit range) but never a diagnostic.
    slots: Vec<Segment>,
    ghp: TextIndex,
    go: TextIndex,
}

impl SourceMap {
    /// Zero-length segments are ignored (they map nothing).
    pub fn new(mut segments: Vec<Segment>, ghp_text: &str, go_text: &str) -> Self {
        segments.retain(|s| s.len > 0);
        let mut by_ghp = segments.clone();
        by_ghp.sort_by_key(|s| s.ghp_start);
        let mut by_go = segments;
        by_go.sort_by_key(|s| s.go_start);
        SourceMap { by_ghp, by_go, slots: Vec::new(), ghp: TextIndex::new(ghp_text), go: TextIndex::new(go_text) }
    }

    /// Add the empty-tag slots (see the field). Only zero-length segments are kept.
    pub fn with_slots(mut self, mut slots: Vec<Segment>) -> Self {
        slots.retain(|s| s.len == 0);
        self.slots = slots;
        self
    }

    pub fn segments(&self) -> &[Segment] {
        &self.by_ghp
    }

    pub fn ghp_index(&self) -> &TextIndex {
        &self.ghp
    }

    pub fn go_index(&self) -> &TextIndex {
        &self.go
    }

    /// Find the segment containing `off` (end inclusive, so a cursor right after the last
    /// character still maps); a segment that strictly contains the offset wins over one that
    /// merely ends there.
    fn find(segs: &[Segment], off: usize, key: impl Fn(&Segment) -> usize) -> Option<&Segment> {
        let mut ending_here = None;
        for s in segs {
            let start = key(s);
            if off >= start && off < start + s.len {
                return Some(s);
            }
            if off == start + s.len {
                ending_here = Some(s);
            }
        }
        ending_here
    }

    pub fn ghp_to_go(&self, off: usize) -> Option<usize> {
        Self::find(&self.by_ghp, off, |s| s.ghp_start)
            .map(|s| s.go_start + (off - s.ghp_start))
            .or_else(|| self.slots.iter().find(|s| s.ghp_start == off).map(|s| s.go_start))
    }

    pub fn go_to_ghp(&self, off: usize) -> Option<usize> {
        Self::find(&self.by_go, off, |s| s.go_start)
            .map(|s| s.ghp_start + (off - s.go_start))
            .or_else(|| self.slots.iter().find(|s| s.go_start == off).map(|s| s.ghp_start))
    }

    pub fn ghp_pos_to_go(&self, pos: Position) -> Option<Position> {
        let off = self.ghp.offset(pos)?;
        self.ghp_to_go(off).map(|o| self.go.position(o))
    }

    pub fn go_pos_to_ghp(&self, pos: Position) -> Option<Position> {
        let off = self.go.offset(pos)?;
        self.go_to_ghp(off).map(|o| self.ghp.position(o))
    }

    /// Both ends must land in the same segment, otherwise the range straddles scaffolding and
    /// has no faithful counterpart.
    pub fn go_range_to_ghp(&self, r: Range) -> Option<Range> {
        let (a, b) = (self.go.offset(r.start)?, self.go.offset(r.end)?);
        let Some(seg) = self.by_go.iter().find(|s| a >= s.go_start && b <= s.go_start + s.len) else {
            // An empty range at an empty tag's slot (the edit of a completion item typed there).
            let slot = self.slots.iter().find(|s| a == b && s.go_start == a)?;
            let at = self.ghp.position(slot.ghp_start);
            return Some(Range { start: at, end: at });
        };
        Some(Range {
            start: self.ghp.position(seg.ghp_start + (a - seg.go_start)),
            end: self.ghp.position(seg.ghp_start + (b - seg.go_start)),
        })
    }

    pub fn ghp_range_to_go(&self, r: Range) -> Option<Range> {
        let (a, b) = (self.ghp.offset(r.start)?, self.ghp.offset(r.end)?);
        let seg = self.by_ghp.iter().find(|s| a >= s.ghp_start && b <= s.ghp_start + s.len)?;
        Some(Range {
            start: self.go.position(seg.go_start + (a - seg.ghp_start)),
            end: self.go.position(seg.go_start + (b - seg.ghp_start)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_columns() {
        // "a😀b": the emoji is 4 bytes in UTF-8 and 2 UTF-16 units.
        let idx = TextIndex::new("a😀b\nxy");
        assert_eq!(idx.position(0), Position { line: 0, character: 0 });
        assert_eq!(idx.position(1), Position { line: 0, character: 1 });
        assert_eq!(idx.position(5), Position { line: 0, character: 3 });
        assert_eq!(idx.position(6), Position { line: 0, character: 4 });
        assert_eq!(idx.position(7), Position { line: 1, character: 0 });
        assert_eq!(idx.offset(Position { line: 0, character: 3 }), Some(5));
        // Middle of the surrogate pair rounds down.
        assert_eq!(idx.offset(Position { line: 0, character: 2 }), Some(1));
        // Past the end of the line clamps before the newline.
        assert_eq!(idx.offset(Position { line: 0, character: 99 }), Some(6));
        assert_eq!(idx.offset(Position { line: 5, character: 0 }), None);
    }

    #[test]
    fn position_offset_round_trip_every_char_boundary() {
        let text = "héllo\n😀 wörld\r\n\nend";
        let idx = TextIndex::new(text);
        for off in 0..=text.len() {
            if text.is_char_boundary(off) {
                let p = idx.position(off);
                // `\r` and `\n` are ordinary columns/terminators; a column on the `\n` itself
                // is only reachable as the line end, which `offset` clamps to.
                assert_eq!(idx.offset(p), Some(off), "offset {off} -> {p:?}");
            }
        }
    }

    fn sample() -> SourceMap {
        // ghp:  "<p><go= x /></p>\n<go= 😀y /> z"
        // go:   "// \n_ x\n_ 😀y"
        let ghp = "<p><go= x /></p>\n<go= 😀y /> z";
        let go = "pre\nx\npre\n😀y\n";
        let a = ghp.find('x').unwrap();
        let b = ghp.find('😀').unwrap();
        let segs = vec![
            Segment { ghp_start: a, go_start: 4, len: 1 },
            Segment { ghp_start: b, go_start: go.find('😀').unwrap(), len: '😀'.len_utf8() + 1 },
        ];
        SourceMap::new(segs, ghp, go)
    }

    #[test]
    fn round_trip_all_offsets() {
        let m = sample();
        for s in m.segments().to_vec() {
            for d in 0..=s.len {
                let ghp = s.ghp_start + d;
                let go = m.ghp_to_go(ghp).expect("inside segment");
                assert_eq!(go, s.go_start + d);
                assert_eq!(m.go_to_ghp(go), Some(ghp));
            }
        }
    }

    #[test]
    fn outside_regions_is_none() {
        let m = sample();
        assert_eq!(m.ghp_to_go(0), None);
        assert_eq!(m.ghp_pos_to_go(Position { line: 0, character: 1 }), None);
        assert_eq!(m.go_pos_to_ghp(Position { line: 0, character: 1 }), None);
        let tail = m.ghp_index().len();
        assert_eq!(m.ghp_to_go(tail), None);
    }

    #[test]
    fn utf16_round_trip_through_map() {
        let m = sample();
        // 😀 starts at line 1 col 4 in the ghp ("<go= "), at line 3 col 0 in the go.
        let g = m.ghp_pos_to_go(Position { line: 1, character: 5 }).unwrap();
        assert_eq!(g, Position { line: 3, character: 0 });
        // After the emoji: +2 UTF-16 units on both sides.
        let g = m.ghp_pos_to_go(Position { line: 1, character: 7 }).unwrap();
        assert_eq!(g, Position { line: 3, character: 2 });
        assert_eq!(m.go_pos_to_ghp(g), Some(Position { line: 1, character: 7 }));
    }

    #[test]
    fn slots_map_a_cursor_in_an_empty_tag_but_not_ranges_elsewhere() {
        let ghp = "<go=/>";
        let go = "pre";
        let m = SourceMap::new(Vec::new(), ghp, go).with_slots(vec![Segment { ghp_start: 4, go_start: 3, len: 0 }]);
        assert_eq!(m.ghp_to_go(4), Some(3));
        assert_eq!(m.go_to_ghp(3), Some(4));
        assert_eq!(m.ghp_to_go(3), None);
        let at = |c| Range { start: Position { line: 0, character: c }, end: Position { line: 0, character: c } };
        assert_eq!(m.go_range_to_ghp(at(3)), Some(at(4)));
        assert_eq!(m.go_range_to_ghp(at(2)), None);
        // Segments (len > 0) passed as slots are ignored.
        let bad = SourceMap::new(Vec::new(), ghp, go).with_slots(vec![Segment { ghp_start: 1, go_start: 1, len: 2 }]);
        assert_eq!(bad.ghp_to_go(1), None);
    }

    #[test]
    fn range_must_stay_in_one_segment() {
        let m = sample();
        let inside = Range { start: Position { line: 1, character: 0 }, end: Position { line: 1, character: 1 } };
        assert!(m.go_range_to_ghp(inside).is_some());
        let straddle = Range { start: Position { line: 1, character: 0 }, end: Position { line: 3, character: 1 } };
        assert_eq!(m.go_range_to_ghp(straddle), None);
    }
}
