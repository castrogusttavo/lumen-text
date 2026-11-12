//! Whitespace: which spaces and tabs the editor draws (`render_whitespace`), and the fixes made
//! on save (`trim_trailing_whitespace`, `insert_final_newline`). Pure logic over `Buffer`.

use crate::buffer::Buffer;

/// `render_whitespace` modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Render {
    /// "none": nothing drawn (the default).
    Off,
    /// Everything but single spaces between words.
    Boundary,
    /// Only whitespace at the end of a line (and on blank lines).
    Trailing,
    All,
}

#[cfg(test)]
pub const RENDER_NAMES: &[&str] = &["none", "boundary", "trailing", "all"];

impl Render {
    pub fn from_name(s: &str) -> Render {
        match s {
            "boundary" => Render::Boundary,
            "trailing" => Render::Trailing,
            "all" => Render::All,
            _ => Render::Off,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Render::Off => "none",
            Render::Boundary => "boundary",
            Render::Trailing => "trailing",
            Render::All => "all",
        }
    }
}

/// A `render_whitespace` value from config.json; anything unknown is "none".
pub fn normalize(v: Option<&str>) -> String {
    Render::from_name(v.unwrap_or("")).name().to_string()
}

fn is_ws(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Char columns of `line` (no line break) whose whitespace `mode` draws, ascending, with
/// whether it is a tab. `out` is cleared first (reused across lines).
pub fn marks(line: &str, mode: Render, out: &mut Vec<(usize, bool)>) {
    out.clear();
    if mode == Render::Off || !line.contains(is_ws) {
        return;
    }
    let n = line.chars().count();
    let lead = line.chars().take_while(|c| is_ws(*c)).count();
    let trail = n - line.chars().rev().take_while(|c| is_ws(*c)).count();
    let mut it = line.chars().enumerate().peekable();
    let mut prev_ws = false;
    while let Some((i, c)) = it.next() {
        let ws = is_ws(c);
        if ws {
            let next_ws = it.peek().is_some_and(|(_, d)| is_ws(*d));
            let show = match mode {
                Render::All => true,
                Render::Trailing => i >= trail,
                Render::Boundary => c == '\t' || i < lead || i >= trail || prev_ws || next_ws,
                Render::Off => false,
            };
            if show {
                out.push((i, c == '\t'));
            }
        }
        prev_ws = ws;
    }
}

/// One replacement in char positions of the current text: `(start, end, text)`.
pub type Fix = (usize, usize, String);

/// The edits saving makes: trailing whitespace removed from every line (`trim`) and a line break
/// added at the end when the text does not end with one (`final_newline`; never in an empty
/// file). Ascending and non-overlapping. A line with a cursor (`cursors`, char positions) inside
/// its trailing whitespace keeps the part before the cursor, so saving never pulls the caret
/// back while you type.
pub fn save_edits(buf: &Buffer, trim: bool, final_newline: bool, cursors: &[usize]) -> Vec<Fix> {
    let mut out = Vec::new();
    if trim {
        for l in 0..buf.line_count() {
            let start = buf.line_start(l);
            let end = start + buf.line_len(l);
            let mut keep = end;
            while keep > start && buf.char_at(keep - 1).is_some_and(char::is_whitespace) {
                keep -= 1;
            }
            if keep == end {
                continue;
            }
            for &c in cursors {
                if c > keep && c <= end {
                    keep = c;
                }
            }
            if keep < end {
                out.push((keep, end, String::new()));
            }
        }
    }
    let n = buf.len_chars();
    if final_newline && n > 0 && !matches!(buf.char_at(n - 1), Some('\n' | '\r')) {
        out.push((n, n, buf.eol().to_string()));
    }
    out
}

/// Where position `p` ends up after `edits` (ascending, non-overlapping): inside removed text it
/// moves to where the removal was; at an insertion point it stays before the inserted text.
pub fn map_pos(edits: &[Fix], p: usize) -> usize {
    let mut delta: isize = 0;
    for (s, e, t) in edits {
        if *s >= p {
            break;
        }
        if *e > p {
            return (*s as isize + delta).max(0) as usize;
        }
        delta += t.chars().count() as isize - (*e - *s) as isize;
    }
    (p as isize + delta).max(0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(line: &str, mode: Render) -> Vec<(usize, bool)> {
        let mut v = vec![(99, true)];
        marks(line, mode, &mut v);
        v
    }

    #[test]
    fn render_modes_pick_the_right_whitespace() {
        let line = "\tlet  a = 1; \t";
        // 0 tab, 4-5 double space, 7 and 9 single spaces, 12-13 trailing
        assert!(m(line, Render::Off).is_empty());
        assert_eq!(
            m(line, Render::All),
            vec![(0, true), (4, false), (5, false), (7, false), (9, false), (12, false), (13, true)]
        );
        assert_eq!(m(line, Render::Trailing), vec![(12, false), (13, true)]);
        assert_eq!(
            m(line, Render::Boundary),
            vec![(0, true), (4, false), (5, false), (12, false), (13, true)],
            "single spaces between words are skipped"
        );
        assert_eq!(m("a\tb", Render::Boundary), vec![(1, true)], "a lone tab still shows");
        assert_eq!(m("   ", Render::Trailing), vec![(0, false), (1, false), (2, false)], "blank line");
        assert!(m("abc", Render::All).is_empty());
        assert_eq!(m("é é", Render::All), vec![(1, false)], "columns are chars, not bytes");
        for name in RENDER_NAMES {
            assert_eq!(Render::from_name(name).name(), *name);
        }
        assert_eq!((normalize(Some("weird")), normalize(None)), ("none".to_string(), "none".to_string()));
    }

    #[test]
    fn trim_and_final_newline_edits() {
        let b = Buffer::from_str("a  \n\t\nb\t c \nlast ");
        let e = save_edits(&b, true, true, &[]);
        assert_eq!(
            e,
            vec![
                (1, 3, String::new()),
                (4, 5, String::new()),
                (10, 11, String::new()),
                (16, 17, String::new()),
                (17, 17, "\n".into())
            ]
        );
        assert!(save_edits(&b, false, false, &[]).is_empty());
        assert!(save_edits(&Buffer::from_str(""), true, true, &[]).is_empty(), "an empty file stays empty");
        assert!(save_edits(&Buffer::from_str("ok\n"), true, true, &[]).is_empty());
        let crlf = Buffer::from_str("x \r\ny");
        assert_eq!(save_edits(&crlf, true, true, &[]), vec![(1, 2, String::new()), (5, 5, "\r\n".into())]);
    }

    #[test]
    fn a_cursor_in_trailing_whitespace_keeps_what_is_before_it() {
        let b = Buffer::from_str("ab    \ncd  ");
        // cursor after two of the four trailing spaces of line 0: only the two after it go
        assert_eq!(save_edits(&b, true, false, &[4]), vec![(4, 6, String::new()), (9, 11, String::new())]);
        // cursor at the very end of the line: that line is left alone
        assert_eq!(save_edits(&b, true, false, &[6]), vec![(9, 11, String::new())]);
        // a cursor before the whitespace changes nothing
        assert_eq!(save_edits(&b, true, false, &[1]).len(), 2);
    }

    #[test]
    fn positions_follow_the_edits() {
        let e: Vec<Fix> = vec![(1, 3, String::new()), (5, 5, "\n".into())];
        assert_eq!(map_pos(&e, 0), 0);
        assert_eq!(map_pos(&e, 1), 1);
        assert_eq!(map_pos(&e, 2), 1, "inside the removed text");
        assert_eq!(map_pos(&e, 3), 1);
        assert_eq!(map_pos(&e, 4), 2);
        assert_eq!(map_pos(&e, 5), 3, "text inserted at a cursor goes after it");
    }
}
