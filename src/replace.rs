//! Search-and-replace engine shared by project search and project replace: one matcher for the
//! case / regex / whole-word options, `$1`-style replacement templates, and rewriting of text and
//! files line by line that keeps every line's own break (LF or CRLF) and a leading BOM.
//! Pure logic: the UI lives in `app_overlay/preplace.rs`.

use regex::{Captures, Regex, RegexBuilder};
use std::{io, path::Path};

/// A compiled query. Matching is per line (no line break inside `line`), like the file find bar.
#[derive(Clone, Debug)]
pub struct Matcher {
    re: Regex,
    word: bool,
    regex: bool,
}

/// One match of a line: byte range and the text that replaces it (template expanded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMatch {
    pub start: usize,
    pub end: usize,
    pub rep: String,
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

impl Matcher {
    /// `query` is literal unless `regex`; `word` keeps only matches that are not glued to a
    /// letter, digit or `_` on either side (a match edge that is itself punctuation always is).
    pub fn new(query: &str, case: bool, regex: bool, word: bool) -> Result<Matcher, String> {
        if query.is_empty() {
            return Err("empty query".into());
        }
        let pat = if regex { query.to_string() } else { regex::escape(query) };
        let re = RegexBuilder::new(&pat).case_insensitive(!case).build().map_err(|e| e.to_string())?;
        Ok(Matcher { re, word, regex })
    }

    fn word_ok(&self, line: &str, s: usize, e: usize) -> bool {
        if !self.word {
            return true;
        }
        let m = &line[s..e];
        let left = !is_word(m.chars().next()) || !is_word(line[..s].chars().next_back());
        let right = !is_word(m.chars().next_back()) || !is_word(line[e..].chars().next());
        left && right
    }

    /// Visit every non-empty match of `line` in order; `f` returns false to stop.
    fn each<'a>(&self, line: &'a str, mut f: impl FnMut(&Captures<'a>) -> bool) {
        let mut pos = 0;
        while pos <= line.len() {
            let Some(c) = self.re.captures_at(line, pos) else { break };
            let Some(m) = c.get(0) else { break };
            if m.start() == m.end() || !self.word_ok(line, m.start(), m.end()) {
                // Rejected (empty, or not a whole word): look again one char further.
                pos = line[m.start()..].chars().next().map_or(line.len() + 1, |ch| m.start() + ch.len_utf8());
                continue;
            }
            if !f(&c) {
                break;
            }
            pos = m.end();
        }
    }

    /// Byte range of the first match of `line`.
    pub fn first(&self, line: &str) -> Option<(usize, usize)> {
        let mut out = None;
        self.each(line, |c| {
            out = c.get(0).map(|m| (m.start(), m.end()));
            false
        });
        out
    }

    /// Every match of `line` with its replacement (`$1`, `${name}`, `$&`, `\n` expanded in regex mode;
    /// the replacement is literal otherwise).
    pub fn matches(&self, line: &str, rep: &str) -> Vec<LineMatch> {
        let mut out = Vec::new();
        self.each(line, |c| {
            let m = c.get(0).expect("group 0");
            let rep = if self.regex { expand(c, rep) } else { rep.to_string() };
            out.push(LineMatch { start: m.start(), end: m.end(), rep });
            true
        });
        out
    }

    /// `line` with every match replaced, and how many there were.
    pub fn replace_line(&self, line: &str, rep: &str, eol: &str) -> (String, usize) {
        let ms = self.matches(line, rep);
        if ms.is_empty() {
            return (line.to_string(), 0);
        }
        let mut out = String::with_capacity(line.len());
        let mut at = 0;
        for m in &ms {
            out.push_str(&line[at..m.start]);
            out.push_str(&fit_eol(&m.rep, eol));
            at = m.end;
        }
        out.push_str(&line[at..]);
        (out, ms.len())
    }
}

/// Expand a replacement template against one match: `$1`..`$99` (the longest group number that
/// exists, so `$1a` is group 1 then "a"), `${name}` / `${12}`, `$&` and `$0` (the whole match), `$$`
/// (a dollar), and the escapes `\n`, `\t`, `\\`. A `$n` naming no group stays literal.
pub fn expand(caps: &Captures, tpl: &str) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut i = 0;
    while i < tpl.len() {
        let rest = &tpl[i..];
        let ch = rest.chars().next().unwrap_or_default();
        if ch == '$' {
            let after = &rest[1..];
            if after.starts_with('$') {
                out.push('$');
                i += 2;
                continue;
            }
            if after.starts_with('&') {
                out.push_str(caps.get(0).map_or("", |m| m.as_str()));
                i += 2;
                continue;
            }
            if let Some(inner) = after.strip_prefix('{')
                && let Some(close) = inner.find('}')
            {
                let name = &inner[..close];
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    let g = match name.parse::<usize>() {
                        Ok(n) => caps.get(n),
                        Err(_) => caps.name(name),
                    };
                    out.push_str(g.map_or("", |m| m.as_str()));
                    i += 2 + close + 1;
                    continue;
                }
            }
            let digits = after.bytes().take(2).take_while(u8::is_ascii_digit).count();
            if digits > 0 {
                let two = (digits == 2).then(|| after[..2].parse::<usize>().ok()).flatten().filter(|&n| n < caps.len());
                let (n, used) = match two {
                    Some(n) => (n, 2),
                    None => (after[..1].parse::<usize>().unwrap_or(usize::MAX), 1),
                };
                if n < caps.len() {
                    out.push_str(caps.get(n).map_or("", |m| m.as_str()));
                } else {
                    out.push('$');
                    out.push_str(&after[..used]);
                }
                i += 1 + used;
                continue;
            }
            out.push('$');
            i += 1;
            continue;
        }
        if ch == '\\' {
            match rest[1..].chars().next() {
                Some('n') => {
                    out.push('\n');
                    i += 2;
                    continue;
                }
                Some('t') => {
                    out.push('\t');
                    i += 2;
                    continue;
                }
                Some('\\') => {
                    out.push('\\');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Line breaks inside a replacement follow the line they land in (CRLF files stay CRLF).
pub fn fit_eol(rep: &str, eol: &str) -> String {
    if eol == "\r\n" && rep.contains('\n') { rep.replace("\r\n", "\n").replace('\n', "\r\n") } else { rep.to_string() }
}

/// (content, line break) of one piece of `str::split_inclusive('\n')`.
fn split_eol(piece: &str) -> (&str, &str) {
    if let Some(c) = piece.strip_suffix("\r\n") {
        (c, "\r\n")
    } else if let Some(c) = piece.strip_suffix('\n') {
        (c, "\n")
    } else {
        (piece, "")
    }
}

/// The text's lines as (content, own break), a leading BOM kept apart; breaks default to the
/// text's main one for the last line.
fn lines_of(text: &str) -> (&'static str, &str, &'static str) {
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(b) => ("\u{feff}", b),
        None => ("", text),
    };
    (bom, body, if body.contains("\r\n") { "\r\n" } else { "\n" })
}

/// Replace every match of `text`, line by line, keeping each line's break and a leading BOM.
/// Returns the new text and the number of replacements.
pub fn replace_text(text: &str, m: &Matcher, rep: &str) -> (String, usize) {
    let (bom, body, main_eol) = lines_of(text);
    let mut out = String::with_capacity(text.len());
    out.push_str(bom);
    let mut n = 0;
    for piece in body.split_inclusive('\n') {
        let (content, eol) = split_eol(piece);
        let (new, k) = m.replace_line(content, rep, if eol.is_empty() { main_eol } else { eol });
        out.push_str(&new);
        out.push_str(eol);
        n += k;
    }
    (out, n)
}

/// Replace the single match at bytes `start..end` of line `line` (what a result row shows):
/// the new text and the inserted replacement. `None` when the text no longer has that match there
/// (the file changed since the search).
pub fn replace_one_in_text(
    text: &str,
    m: &Matcher,
    rep: &str,
    line: usize,
    start: usize,
    end: usize,
) -> Option<(String, String)> {
    let (bom, body, main_eol) = lines_of(text);
    let mut offset = bom.len();
    for (l, piece) in body.split_inclusive('\n').enumerate() {
        if l < line {
            offset += piece.len();
            continue;
        }
        let (content, eol) = split_eol(piece);
        let hit = m.matches(content, rep).into_iter().find(|x| x.start == start && x.end == end)?;
        let new = fit_eol(&hit.rep, if eol.is_empty() { main_eol } else { eol });
        let mut out = String::with_capacity(text.len() + new.len());
        out.push_str(&text[..offset + start]);
        out.push_str(&new);
        out.push_str(&text[offset + end..]);
        return Some((out, new));
    }
    None
}

/// Read a text file for replacing: binary files and files that are not UTF-8 are refused (they
/// would not survive a round trip through `String`).
fn read_text(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "binary file"));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}

/// Write `data` over `path` atomically: a temp file next to it (same permissions), then a rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?;
    let tmp = path.with_file_name(format!(".{}.lumen-tmp", name.to_string_lossy()));
    fn write_then_rename(path: &Path, tmp: &Path, data: &[u8]) -> io::Result<()> {
        std::fs::write(tmp, data)?;
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(tmp, meta.permissions());
        }
        std::fs::rename(tmp, path)
    }
    let res = write_then_rename(path, &tmp, data);
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Replace every match in a file on disk (atomically, line breaks kept). Returns the count; the
/// file is not touched when there is nothing to replace.
pub fn replace_file(path: &Path, m: &Matcher, rep: &str) -> io::Result<usize> {
    let text = read_text(path)?;
    let (new, n) = replace_text(&text, m, rep);
    if n > 0 {
        write_atomic(path, new.as_bytes())?;
    }
    Ok(n)
}

/// Replace one match of a file on disk (see [`replace_one_in_text`]): the inserted text, `None`
/// when the match is gone.
pub fn replace_one_in_file(
    path: &Path,
    m: &Matcher,
    rep: &str,
    line: usize,
    start: usize,
    end: usize,
) -> io::Result<Option<String>> {
    let text = read_text(path)?;
    match replace_one_in_text(&text, m, rep, line, start, end) {
        Some((new, inserted)) => write_atomic(path, new.as_bytes()).map(|_| Some(inserted)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(q: &str, case: bool, regex: bool, word: bool) -> Matcher {
        Matcher::new(q, case, regex, word).unwrap()
    }

    #[test]
    fn literal_case_and_whole_word() {
        let x = m("foo", false, false, false);
        assert_eq!(replace_text("Foo foo food", &x, "bar").0, "bar bar bard");
        let x = m("foo", true, false, false);
        assert_eq!(replace_text("Foo foo food", &x, "bar").0, "Foo bar bard");
        let x = m("foo", false, false, true);
        assert_eq!(replace_text("Foo foo food _foo foo.x", &x, "bar"), ("bar bar food _foo bar.x".into(), 3));
        // Punctuation at the edge of the query: the boundary there is always fine.
        let x = m("a(", true, false, true);
        assert_eq!(replace_text("a(x) ba(y)", &x, "b(").0, "b(x) ba(y)");
        // A literal query is not a regex and a literal replacement keeps its dollars.
        let x = m("a.b", true, false, false);
        assert_eq!(replace_text("a.b axb", &x, "$1$$").0, "$1$$ axb");
        assert!(Matcher::new("(", true, true, false).is_err());
        assert!(Matcher::new("", true, false, false).is_err());
    }

    #[test]
    fn regex_captures_in_the_replacement() {
        let x = m(r"(\w+)=(\d+)", true, true, false);
        assert_eq!(replace_text("a=1, bb=22", &x, "$2:$1").0, "1:a, 22:bb");
        assert_eq!(replace_text("a=1", &x, "${2}0 $10 $& $0 $$ $9").0, "10 a0 a=1 a=1 $ $9");
        let x = m(r"(?P<key>\w+)=(?P<val>\d+)", true, true, false);
        assert_eq!(replace_text("k=5", &x, "${val}<-${key} [$1a]").0, "5<-k [ka]");
        let x = m(r"\s*,\s*", true, true, false);
        assert_eq!(replace_text("a , b,c", &x, r",\n").0, "a,\nb,\nc");
        assert_eq!(replace_text("a,b", &x, r"\t\\").0, "a\t\\b");
    }

    #[test]
    fn several_matches_per_line_and_empty_matches() {
        let x = m("ab", true, false, false);
        assert_eq!(replace_text("abab xab\nab", &x, "-"), ("-- x-\n-".into(), 4));
        let ms = x.matches("abab", "Z");
        assert_eq!(ms.iter().map(|l| (l.start, l.end)).collect::<Vec<_>>(), vec![(0, 2), (2, 4)]);
        // Empty matches are skipped (an anchor alone replaces nothing).
        let x = m("^", true, true, false);
        assert_eq!(replace_text("abc\ndef", &x, "> ").1, 0);
        let x = m("x*", true, true, false);
        assert_eq!(replace_text("axxbx", &x, "-").0, "a-b-");
        // Anchors are per line.
        let x = m("^a", true, true, false);
        assert_eq!(replace_text("ab\nab\r\nba", &x, "A").0, "Ab\nAb\r\nba");
        // Multibyte text keeps byte offsets straight.
        let x = m("é", false, false, false);
        assert_eq!(replace_text("café É ée", &x, "e").0, "cafe e ee");
    }

    #[test]
    fn line_endings_and_bom_survive() {
        let x = m("o", true, false, false);
        let crlf = "foo\r\nbar\r\nboo";
        assert_eq!(replace_text(crlf, &x, "0"), ("f00\r\nbar\r\nb00".into(), 4));
        let mixed = "o\r\no\no";
        assert_eq!(replace_text(mixed, &x, "x").0, "x\r\nx\nx");
        // A replacement that adds a line takes the break of its line.
        let x = m(";", true, false, false);
        let x2 = m(";", true, true, false);
        assert_eq!(replace_text("a;b\r\nc", &x2, r";\n").0, "a;\r\nb\r\nc");
        assert_eq!(replace_text("a;b\nc", &x, "\n").0, "a\nb\nc");
        assert_eq!(replace_text("c\r\na;b", &x, "\n").0, "c\r\na\r\nb", "last line: the file's break");
        let bom = "\u{feff}abc\r\nabc\r\n";
        let x = m("^a", true, true, false);
        assert_eq!(replace_text(bom, &x, "A"), ("\u{feff}Abc\r\nAbc\r\n".into(), 2));
    }

    #[test]
    fn replace_one_targets_exactly_one_match() {
        let x = m("ab", true, false, false);
        let t = "ab ab\r\nxx ab ab\r\n";
        let one = |t: &str, x: &Matcher, rep: &str, l, s, e| replace_one_in_text(t, x, rep, l, s, e).map(|r| r.0);
        assert_eq!(one(t, &x, "Q", 1, 6, 8).as_deref(), Some("ab ab\r\nxx ab Q\r\n"));
        assert_eq!(one(t, &x, "Q", 0, 0, 2).as_deref(), Some("Q ab\r\nxx ab ab\r\n"));
        assert_eq!(replace_one_in_text(t, &x, "Q", 1, 4, 6), None, "no match there any more");
        assert_eq!(replace_one_in_text(t, &x, "Q", 9, 0, 2), None, "no such line");
        let x = m(r"(a)(b)", true, true, false);
        assert_eq!(
            replace_one_in_text("\u{feff}ab ab", &x, "$2$1", 0, 3, 5),
            Some(("\u{feff}ab ba".to_string(), "ba".to_string()))
        );
        let x = m(";", true, true, false);
        assert_eq!(replace_one_in_text("a;b\r\n", &x, r";\n", 0, 1, 2).map(|r| r.1).as_deref(), Some(";\r\n"));
    }

    #[test]
    fn files_are_rewritten_atomically_and_refused_when_binary() {
        let dir = std::env::temp_dir().join(format!("lumen-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "one two\r\ntwo\r\n").unwrap();
        let x = m("two", true, false, true);
        assert_eq!(replace_file(&f, &x, "2").unwrap(), 2);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "one 2\r\n2\r\n");
        assert_eq!(replace_file(&f, &x, "2").unwrap(), 0);
        assert_eq!(replace_one_in_file(&f, &m("2", true, false, false), "3", 1, 0, 1).unwrap().as_deref(), Some("3"));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "one 2\r\n3\r\n");
        let b = dir.join("b.bin");
        std::fs::write(&b, [b't', b'w', b'o', 0]).unwrap();
        assert!(replace_file(&b, &x, "2").is_err());
        let l = dir.join("latin1.txt");
        std::fs::write(&l, [b't', b'w', b'o', b' ', 0xe9]).unwrap();
        assert!(replace_file(&l, &x, "2").is_err(), "not UTF-8: left alone");
        assert_eq!(std::fs::read(&l).unwrap(), [b't', b'w', b'o', b' ', 0xe9]);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 3, "no temp files left: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
