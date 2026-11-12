//! Byte offset <-> LSP position (zero-based line, UTF-16 code unit column) mapping.
//!
//! Line endings are `\n`, `\r\n` and a lone `\r`, as in the LSP specification.

use crate::ast::Span;

/// An LSP position: zero-based line and UTF-16 column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// An LSP range (end exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A non-ASCII char: its byte length differs from its UTF-8-agnostic column width.
#[derive(Debug, Clone, Copy)]
struct Wide {
    start: usize,
    len8: u8,
    len16: u8,
}

/// Maps between byte offsets and LSP positions for one version of a text. It does not keep the
/// text itself; build a new one whenever the text changes.
#[derive(Debug, Clone)]
pub struct LineIndex {
    len: usize,
    /// Byte offset where each line starts.
    starts: Vec<usize>,
    /// Byte offset where each line's content ends (before its line terminator).
    ends: Vec<usize>,
    wide: Vec<Wide>,
    /// `saved[i]`: UTF-16 units saved over UTF-8 bytes by `wide[..i]` (`wide.len() + 1` entries).
    saved: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let b = text.as_bytes();
        let mut starts = vec![0];
        let mut ends = Vec::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\n' => {
                    ends.push(i);
                    i += 1;
                    starts.push(i);
                }
                b'\r' => {
                    ends.push(i);
                    i += if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                    starts.push(i);
                }
                _ => i += 1,
            }
        }
        ends.push(b.len());

        let wide: Vec<Wide> = text
            .char_indices()
            .filter(|(_, c)| c.len_utf8() > 1)
            .map(|(start, c)| Wide { start, len8: c.len_utf8() as u8, len16: c.len_utf16() as u8 })
            .collect();

        let mut saved = Vec::with_capacity(wide.len() + 1);
        saved.push(0);
        for w in &wide {
            saved.push(saved[saved.len() - 1] + (w.len8 - w.len16) as usize);
        }

        LineIndex { len: b.len(), starts, ends, wide, saved }
    }

    /// Number of lines (an empty text has one).
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Byte span of a line's content, without its terminator.
    pub fn line_span(&self, line: usize) -> Option<Span> {
        Some(Span::new(*self.starts.get(line)?, self.ends[line]))
    }

    /// Zero-based line containing `offset` (clamped to the text).
    pub fn line_of(&self, offset: usize) -> usize {
        let offset = offset.min(self.len);
        self.starts.partition_point(|&s| s <= offset) - 1
    }

    /// Position of a byte offset. Offsets past the end clamp to the end; an offset inside a
    /// multi-byte char snaps back to the start of that char.
    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.len);
        let line = self.line_of(offset);
        let ls = self.starts[line];

        // Wide characters are sorted and never straddle a line start: those wholly before `offset`
        // shorten the column by their precomputed saving; one that straddles it rounds down.
        let lo = self.wide.partition_point(|w| w.start < ls);
        let hi = self.wide.partition_point(|w| w.start + w.len8 as usize <= offset);
        let saved = self.saved[hi.max(lo)] - self.saved[lo];
        let mut eff = offset;
        if let Some(w) = self.wide.get(hi.max(lo))
            && w.start < offset
        {
            eff = w.start;
        }
        Position { line: line as u32, character: (eff - ls - saved) as u32 }
    }

    /// Byte offset of a position. A line past the end gives the text length; a column past the
    /// end of the line clamps to the end of its content; a column in the middle of a surrogate
    /// pair snaps back to the start of that char.
    pub fn offset(&self, pos: Position) -> usize {
        let line = pos.line as usize;
        if line >= self.starts.len() {
            return self.len;
        }
        let ls = self.starts[line];
        let le = self.ends[line];
        let target = pos.character as usize;

        let lo = self.wide.partition_point(|w| w.start < ls);
        let mut col16 = 0usize;
        let mut byte = ls;
        for w in &self.wide[lo..] {
            if w.start >= le {
                break;
            }
            let ascii = w.start - byte;
            if col16 + ascii >= target {
                return (byte + target - col16).min(le);
            }
            col16 += ascii;
            if col16 + w.len16 as usize > target {
                return w.start;
            }
            col16 += w.len16 as usize;
            byte = w.start + w.len8 as usize;
        }
        (byte + (target - col16)).min(le)
    }

    pub fn range(&self, span: Span) -> Range {
        Range { start: self.position(span.start), end: self.position(span.end) }
    }

    pub fn span(&self, range: Range) -> Span {
        let start = self.offset(range.start);
        let end = self.offset(range.end).max(start);
        Span::new(start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn ascii_lines() {
        let idx = LineIndex::new("ab\ncd\n\nx");
        assert_eq!(idx.line_count(), 4);
        let cases = [
            (0, pos(0, 0)),
            (2, pos(0, 2)),
            (3, pos(1, 0)),
            (5, pos(1, 2)),
            (6, pos(2, 0)),
            (7, pos(3, 0)),
            (8, pos(3, 1)),
            (99, pos(3, 1)),
        ];
        for (off, p) in cases {
            assert_eq!(idx.position(off), p, "offset {off}");
        }
    }

    #[test]
    fn crlf_and_lone_cr() {
        let t = "a\r\nb\rc\n";
        let idx = LineIndex::new(t);
        assert_eq!(idx.line_count(), 4);
        assert_eq!(idx.position(3), pos(1, 0));
        assert_eq!(idx.position(5), pos(2, 0));
        assert_eq!(idx.position(7), pos(3, 0));
        assert_eq!(idx.line_span(0), Some(Span::new(0, 1)));
        assert_eq!(idx.line_span(1), Some(Span::new(3, 4)));
        // Column past the line content clamps before the terminator.
        assert_eq!(idx.offset(pos(0, 50)), 1);
        assert_eq!(idx.offset(pos(1, 0)), 3);
        assert_eq!(idx.offset(pos(9, 0)), t.len());
    }

    #[test]
    fn utf16_columns() {
        // 'é' is 2 bytes / 1 unit; the emoji is 4 bytes / 2 units; '中' is 3 bytes / 1 unit.
        let t = "é😀中x\nz";
        let idx = LineIndex::new(t);
        let x = t.find('x').unwrap();
        assert_eq!(idx.position(x), pos(0, 1 + 2 + 1));
        assert_eq!(idx.offset(pos(0, 4)), x);
        assert_eq!(idx.position(t.len()), pos(1, 1));
        // Inside the emoji (surrogate pair): both directions snap to the char start.
        assert_eq!(idx.offset(pos(0, 2)), 2);
        assert_eq!(idx.position(3), pos(0, 1));
        assert_eq!(idx.position(1), pos(0, 0));
    }

    #[test]
    fn roundtrip_every_char_boundary() {
        let texts = ["", "abc", "a\r\nb😀c\n\nçé\r中\n", "😀😀😀", "\n\n", "x\r"];
        for t in texts {
            let idx = LineIndex::new(t);
            for (off, _) in t.char_indices().chain(std::iter::once((t.len(), ' '))) {
                let p = idx.position(off);
                let back = idx.offset(p);
                // Offsets inside a line terminator have no position of their own.
                let in_terminator = t.as_bytes().get(off) == Some(&b'\n') && off > 0 && t.as_bytes()[off - 1] == b'\r';
                if !in_terminator {
                    assert_eq!(back, off, "text {t:?} offset {off} pos {p:?}");
                }
            }
        }
    }

    #[test]
    fn span_range_roundtrip() {
        let t = "<go= \"😀\" />\n<go:if x/>";
        let idx = LineIndex::new(t);
        let s = Span::new(5, 11);
        assert_eq!(idx.span(idx.range(s)), s);
    }

    #[test]
    fn empty_text() {
        let idx = LineIndex::new("");
        assert_eq!(idx.line_count(), 1);
        assert_eq!(idx.position(0), pos(0, 0));
        assert_eq!(idx.offset(pos(0, 3)), 0);
    }
}
