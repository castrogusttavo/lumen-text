//! Line bookmarks: toggle with Ctrl+F2, jump with F2 / Shift+F2, a mark in the gutter, kept in the
//! session. Marks are 0-based lines that follow edits (lines inserted or removed above move them,
//! a removed line takes its mark with it).

use crate::{
    app::App,
    buffer::{EditRec, LineOp},
};

/// Update `marks` (sorted, unique) for the edits `recs`, applied in order.
pub fn on_edits(marks: &mut Vec<usize>, recs: &[EditRec]) {
    if marks.is_empty() {
        return;
    }
    for r in recs {
        for op in r.line_ops() {
            match op {
                LineOp::Remove { after, n } => {
                    let first = (after + 1) as usize;
                    marks.retain(|&m| m < first || m >= first + n);
                    for m in marks.iter_mut().filter(|m| **m >= first + n) {
                        *m -= n;
                    }
                }
                LineOp::Insert { after, n } => {
                    let first = (after + 1) as usize;
                    for m in marks.iter_mut().filter(|m| **m >= first) {
                        *m += n;
                    }
                }
                LineOp::Swap { first, a_lines, b_lines } => {
                    let (alen, blen) = (a_lines, b_lines);
                    for m in marks.iter_mut() {
                        if *m >= first && *m < first + alen {
                            *m += blen;
                        } else if *m >= first + alen && *m < first + alen + blen {
                            *m -= alen;
                        }
                    }
                }
            }
        }
    }
    marks.sort_unstable();
    marks.dedup();
}

/// Bookmarks from the session file: valid, sorted, unique lines of a document with `lines` lines.
pub fn from_json(v: &serde_json::Value, lines: usize) -> Vec<usize> {
    let mut m: Vec<usize> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_u64())
        .map(|x| x as usize)
        .filter(|l| *l < lines)
        .collect();
    m.sort_unstable();
    m.dedup();
    m
}

/// Add `line` to `marks`, or remove it when present. Returns true when it is now marked.
pub fn toggle(marks: &mut Vec<usize>, line: usize) -> bool {
    match marks.binary_search(&line) {
        Ok(i) => {
            marks.remove(i);
            false
        }
        Err(i) => {
            marks.insert(i, line);
            true
        }
    }
}

/// The next marked line after `line`, wrapping to the first.
pub fn next(marks: &[usize], line: usize) -> Option<usize> {
    marks.iter().copied().find(|&m| m > line).or_else(|| marks.first().copied())
}

/// The previous marked line before `line`, wrapping to the last.
pub fn prev(marks: &[usize], line: usize) -> Option<usize> {
    marks.iter().rev().copied().find(|&m| m < line).or_else(|| marks.last().copied())
}

impl App {
    /// Ctrl+F2: bookmark the caret line, or remove its bookmark.
    pub fn toggle_bookmark(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let (line, _) = d.buf.line_col(d.ed.head());
        let on = toggle(&mut d.marks, line);
        self.say(if on { "bookmark added" } else { "bookmark removed" });
        self.request_redraw();
    }

    /// F2 / Shift+F2: go to the next / previous bookmark of this file (wraps around).
    pub fn goto_bookmark(&mut self, forward: bool) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let (line, _) = d.buf.line_col(d.ed.head());
        let target = if forward { next(&d.marks, line) } else { prev(&d.marks, line) };
        match target {
            Some(l) if l < d.buf.line_count() => {
                let p = d.buf.line_start(l);
                d.ed.click(&mut d.buf, p, false);
                d.reveal = true;
            }
            _ => self.say("no bookmarks in this file"),
        }
        self.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::EditShape;

    fn rec(line: usize, col: usize, rem: usize, ins: usize, tail_empty: bool) -> EditRec {
        EditRec {
            at: 0,
            removed: 0,
            inserted: 0,
            line,
            col,
            rem_lines: rem,
            ins_lines: ins,
            tail_empty,
            shape: EditShape::Splice,
        }
    }

    #[test]
    fn toggle_next_prev() {
        let mut m = Vec::new();
        assert!(toggle(&mut m, 7));
        assert!(toggle(&mut m, 2));
        assert_eq!(m, [2, 7]);
        assert_eq!(next(&m, 2), Some(7));
        assert_eq!(next(&m, 7), Some(2), "wraps");
        assert_eq!(prev(&m, 7), Some(2));
        assert_eq!(prev(&m, 2), Some(7), "wraps");
        assert!(!toggle(&mut m, 7));
        assert_eq!(m, [2]);
        assert_eq!(next(&[], 0), None);
    }

    #[test]
    fn lines_inserted_above_move_marks_down() {
        let mut m = vec![3, 10];
        // Enter in the middle of line 5 (col > 0): one line break inserted after line 5.
        on_edits(&mut m, &[rec(5, 4, 0, 1, false)]);
        assert_eq!(m, [3, 11]);
        // Enter at the start of line 3: the old line 3 is pushed down with its mark.
        on_edits(&mut m, &[rec(3, 0, 0, 1, false)]);
        assert_eq!(m, [4, 12]);
    }

    #[test]
    fn removed_lines_take_their_marks() {
        let mut m = vec![2, 5, 9];
        // Delete the break after line 4 and the one after line 5 (joins 4,5,6): line 5's mark goes.
        on_edits(&mut m, &[rec(4, 3, 2, 0, false)]);
        assert_eq!(m, [2, 7]);
    }

    #[test]
    fn swapped_blocks_carry_their_marks() {
        let mut m = vec![4, 5];
        let r = EditRec {
            shape: EditShape::Swap { a_lines: 1, b_lines: 1, a_chars: 3, b_chars: 3, eol: 1 },
            ..rec(4, 0, 0, 0, false)
        };
        on_edits(&mut m, &[r]);
        assert_eq!(m, [4, 5], "two marks on the two swapped lines stay a pair");
        let mut m = vec![4];
        on_edits(&mut m, &[r]);
        assert_eq!(m, [5], "moving line 4 down takes its mark along");
    }
}
