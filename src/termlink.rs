//! Links found in terminal text: `http(s)://` URLs and `path:line[:col]` references (compiler
//! errors, stack traces, grep output) that name a file that exists, relative to the shell's folder
//! or the project. The app underlines them on hover and opens them with Ctrl+click.

use crate::{terminal::Grid, termnav::line_chars};
use std::path::{Path, PathBuf};

/// What a link opens.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    Url(String),
    /// A file at a 0-based line and column.
    File {
        path: PathBuf,
        line: usize,
        col: usize,
    },
}

/// A link on a terminal line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TermLink {
    /// Absolute line and the columns it covers (`c1` exclusive).
    pub abs: usize,
    pub c0: usize,
    pub c1: usize,
    pub target: Target,
}

/// The `http(s)://` URL in `line` that covers char `idx`: (first char, end char, url).
pub fn url_span(line: &str, idx: usize) -> Option<(usize, usize, String)> {
    let chars: Vec<char> = line.chars().collect();
    let text: String = chars.iter().collect();
    for start in text.match_indices("http").map(|(i, _)| text[..i].chars().count()) {
        let rest: String = chars[start..].iter().take(8).collect();
        if !rest.starts_with("http://") && !rest.starts_with("https://") {
            continue;
        }
        let len = chars[start..]
            .iter()
            .position(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '│'))
            .unwrap_or(chars.len() - start);
        // Trailing punctuation belongs to the sentence, not the URL.
        let mut end = start + len;
        while end > start && matches!(chars[end - 1], '.' | ',' | ';' | ':' | ')' | ']' | '}' | '!' | '?') {
            end -= 1;
        }
        if idx >= start && idx < end {
            return Some((start, end, chars[start..end].iter().collect()));
        }
    }
    None
}

/// The existing file `p` names: `~/..`, absolute, or relative to one of `bases` in order.
pub fn resolve(p: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    let path = if let Some(rest) = p.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else {
        PathBuf::from(p)
    };
    if path.is_absolute() {
        return path.is_file().then_some(path);
    }
    let rel = p.trim_start_matches("./");
    bases.iter().map(|b| b.join(rel)).find(|c| c.is_file())
}

/// The link under column `col` of absolute line `abs`; file references must exist (checked on disk).
pub fn at(g: &Grid, abs: usize, col: usize, bases: &[PathBuf]) -> Option<TermLink> {
    let (text, cols) = line_chars(g, abs);
    let n = text.chars().count();
    if n == 0 || col >= cols[n] {
        return None;
    }
    let idx = cols[..n].partition_point(|c| *c <= col).checked_sub(1)?;
    let span = |a: usize, b: usize| (cols[a], cols[b.min(n)]);
    if let Some((a, b, url)) = url_span(&text, idx) {
        let (c0, c1) = span(a, b);
        return Some(TermLink { abs, c0, c1, target: Target::Url(url) });
    }
    let l = crate::tasks::find_links(&text).into_iter().find(|l| idx >= l.start && idx < l.end)?;
    let path = resolve(&l.path, bases)?;
    let (c0, c1) = span(l.start, l.end);
    Some(TermLink { abs, c0, c1, target: Target::File { path, line: l.line, col: l.col } })
}

/// One line for a tooltip: where the link goes.
pub fn describe(t: &Target, root: &Path) -> String {
    match t {
        Target::Url(u) => u.clone(),
        Target::File { path, line, col } => {
            let p = path.strip_prefix(root).unwrap_or(path);
            format!("{}:{}:{}", p.display(), line + 1, col + 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_with(text: &str) -> Grid {
        let mut g = Grid::new(4, 80);
        let mut p = vte::Parser::new();
        p.advance(&mut g, text.as_bytes());
        g
    }

    #[test]
    fn url_spans_drop_trailing_punctuation() {
        let line = "see (https://example.com/a?b=1), ok";
        let (a, b, u) = url_span(line, 10).unwrap();
        assert_eq!(u, "https://example.com/a?b=1");
        assert_eq!((a, b), (5, 30));
        assert!(url_span(line, 1).is_none());
        assert!(url_span("httpfoo bar", 2).is_none());
    }

    #[test]
    fn file_references_must_exist() {
        let d = std::env::temp_dir().join(format!("lumen-termlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() {}\n").unwrap();
        let bases = vec![d.join("nowhere"), d.clone()];
        let g = grid_with("error at src/main.rs:12:5 here\r\nmissing src/nope.rs:3 x\r\nsee https://a.io/x.");
        let l = at(&g, 0, 14, &bases).unwrap();
        assert_eq!(
            l.target,
            Target::File { path: d.join("src/main.rs"), line: 11, col: 4 },
            "1-based on screen, 0-based for the editor"
        );
        assert_eq!((l.c0, l.c1), (9, 25));
        assert!(at(&g, 0, 2, &bases).is_none(), "plain words are not links");
        assert!(at(&g, 1, 12, &bases).is_none(), "a file that does not exist is not a link");
        let u = at(&g, 2, 8, &bases).unwrap();
        assert_eq!(u.target, Target::Url("https://a.io/x".into()));
        assert!(at(&g, 0, 70, &bases).is_none(), "past the text");
        assert_eq!(describe(&l.target, &d), "src/main.rs:12:5");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn relative_paths_try_each_base_in_order() {
        let d = std::env::temp_dir().join(format!("lumen-termlink-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        std::fs::write(d.join("b/x.txt"), "x").unwrap();
        std::fs::write(d.join("a/x.txt"), "x").unwrap();
        assert_eq!(resolve("./x.txt", &[d.join("a"), d.join("b")]), Some(d.join("a/x.txt")));
        assert_eq!(resolve("x.txt", &[d.join("c"), d.join("b")]), Some(d.join("b/x.txt")));
        assert_eq!(resolve("y.txt", &[d.join("a")]), None);
        assert_eq!(resolve(d.join("b/x.txt").to_str().unwrap(), &[]), Some(d.join("b/x.txt")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
