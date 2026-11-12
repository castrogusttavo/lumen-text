//! `.editorconfig` support (<https://editorconfig.org>), hand-written: the INI-like parser, the
//! section glob matcher, and the lookup that walks up from a file to the `root = true` file.
//!
//! Files closer to the edited file win over those above it, and later sections win over earlier
//! ones in the same file; `unset` removes a property again. Globs support `*`, `**`, `?`,
//! `[abc]`, `[!abc]`, `[a-z]`, `{a,b}` (nestable), `{1..3}` and `\` escapes; a glob without `/`
//! matches the file name in any folder, one with `/` is relative to the `.editorconfig`.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentStyle {
    Tab,
    Space,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
    Cr,
}

/// The properties lumen-text understands, for one file. `None` = not set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Props {
    pub indent_style: Option<IndentStyle>,
    /// Columns per indentation level (`indent_size = tab` is resolved to `tab_width`).
    pub indent_size: Option<usize>,
    pub tab_width: Option<usize>,
    pub end_of_line: Option<Eol>,
    /// Lowercase `charset`: `latin1`, `utf-8`, `utf-8-bom`, `utf-16be` or `utf-16le`.
    pub charset: Option<String>,
    pub trim_trailing_whitespace: Option<bool>,
    pub insert_final_newline: Option<bool>,
    /// Kept for rulers / wrapping (not used yet); `off` = `None`.
    pub max_line_length: Option<usize>,
}

/// One `[glob]` section and its `key = value` pairs (keys lowercase), in file order.
#[derive(Debug, PartialEq, Eq)]
pub struct Section {
    pub glob: String,
    pub pairs: Vec<(String, String)>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct File {
    /// `root = true` before the first section: stop looking further up.
    pub root: bool,
    pub sections: Vec<Section>,
}

/// Parse the text of an `.editorconfig`. Lines are `[glob]`, `key = value`, or comments
/// starting with `#` or `;`; anything else is ignored, never an error.
pub fn parse(text: &str) -> File {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut f = File::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(glob) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            f.sections.push(Section { glob: glob.to_string(), pairs: Vec::new() });
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim().to_ascii_lowercase();
        let val = line[eq + 1..].trim().to_string();
        if key.is_empty() {
            continue;
        }
        match f.sections.last_mut() {
            Some(s) => s.pairs.push((key, val)),
            None if key == "root" => f.root = val.eq_ignore_ascii_case("true"),
            None => {}
        }
    }
    f
}

/// Properties from `key = value` pairs in the order they apply (later ones win).
pub fn resolve(pairs: &[(String, String)]) -> Props {
    let mut map: Vec<(&str, String)> = Vec::new();
    for (k, v) in pairs {
        map.retain(|(mk, _)| *mk != k.as_str());
        if !v.eq_ignore_ascii_case("unset") {
            map.push((k.as_str(), v.to_ascii_lowercase()));
        }
    }
    let get = |k: &str| map.iter().find(|(mk, _)| *mk == k).map(|(_, v)| v.as_str());
    let num = |k: &str| get(k).and_then(|v| v.parse::<usize>().ok()).filter(|n| *n > 0);
    let flag = |k: &str| match get(k) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let indent_style = match get("indent_style") {
        Some("tab") => Some(IndentStyle::Tab),
        Some("space") => Some(IndentStyle::Space),
        _ => None,
    };
    let tab_width = num("tab_width");
    // `indent_size = tab` (also implied by `indent_style = tab`) means "the tab width".
    let size_is_tab =
        get("indent_size") == Some("tab") || (indent_style == Some(IndentStyle::Tab) && get("indent_size").is_none());
    let indent_size = if size_is_tab { tab_width } else { num("indent_size") };
    Props {
        indent_style,
        indent_size,
        // `tab_width` defaults to `indent_size`.
        tab_width: tab_width.or(if size_is_tab { None } else { indent_size }),
        end_of_line: match get("end_of_line") {
            Some("lf") => Some(Eol::Lf),
            Some("crlf") => Some(Eol::Crlf),
            Some("cr") => Some(Eol::Cr),
            _ => None,
        },
        charset: get("charset")
            .filter(|c| matches!(*c, "latin1" | "utf-8" | "utf-8-bom" | "utf-16be" | "utf-16le"))
            .map(str::to_string),
        trim_trailing_whitespace: flag("trim_trailing_whitespace"),
        insert_final_newline: flag("insert_final_newline"),
        max_line_length: num("max_line_length"),
    }
}

/// The `.editorconfig` properties of the file at `path` (absolute): every `.editorconfig` from
/// its folder up to the first `root = true` one (or the file system root).
pub fn props_for(path: &Path) -> Props {
    let mut files: Vec<(PathBuf, File)> = Vec::new();
    let mut dir = path.parent();
    while let Some(d) = dir {
        if let Ok(text) = std::fs::read_to_string(d.join(".editorconfig")) {
            let f = parse(&text);
            let root = f.root;
            files.push((d.to_path_buf(), f));
            if root {
                break;
            }
        }
        dir = d.parent();
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (d, f) in files.iter().rev() {
        let Ok(rel) = path.strip_prefix(d) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        for s in &f.sections {
            if glob_match(&s.glob, &rel) {
                pairs.extend(s.pairs.iter().cloned());
            }
        }
    }
    resolve(&pairs)
}

/// Does section glob `glob` match `rel`, a path relative to the `.editorconfig`'s folder with
/// `/` separators?
pub fn glob_match(glob: &str, rel: &str) -> bool {
    let anchored = glob.contains('/');
    let chars: Vec<char> = glob.strip_prefix('/').unwrap_or(glob).chars().collect();
    let mut ranges: Vec<(i64, i64)> = Vec::new();
    let body = translate(&chars, &mut ranges);
    let full = if anchored { format!("^{body}$") } else { format!("^(?:.*/)?{body}$") };
    let Ok(re) = regex::Regex::new(&full) else { return false };
    if ranges.is_empty() {
        return re.is_match(rel);
    }
    // Each `{a..b}` is a capture group (the only ones): its number must fall in the range.
    re.captures(rel).is_some_and(|c| {
        ranges.iter().enumerate().all(|(k, (lo, hi))| match c.get(k + 1) {
            None => true,
            Some(m) => m.as_str().parse::<i64>().is_ok_and(|n| n >= *lo && n <= *hi),
        })
    })
}

/// Regex (without anchors) for glob characters `p`; `{a..b}` ranges are appended to `ranges` in
/// the order of their capture groups.
fn translate(p: &[char], ranges: &mut Vec<(i64, i64)>) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '\\' if i + 1 < p.len() => {
                push_literal(&mut out, p[i + 1]);
                i += 2;
                continue;
            }
            '*' if p.get(i + 1) == Some(&'*') => {
                // `**/` at the start or after a `/` also matches no folder at all.
                if (i == 0 || p[i - 1] == '/') && p.get(i + 2) == Some(&'/') {
                    out.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    out.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => {
                if let Some(end) = class_end(p, i) {
                    let inner = &p[i + 1..end];
                    out.push('[');
                    let mut k = 0;
                    if matches!(inner.first(), Some('!' | '^')) {
                        out.push('^');
                        k = 1;
                    }
                    for &ch in &inner[k..] {
                        if ch == '-' {
                            out.push('-');
                        } else {
                            push_literal(&mut out, ch);
                        }
                    }
                    out.push(']');
                    i = end + 1;
                    continue;
                }
                out.push_str("\\[");
            }
            '{' => {
                if let Some(end) = brace_end(p, i) {
                    let inner = &p[i + 1..end];
                    let text: String = inner.iter().collect();
                    if let Some((a, b)) = parse_range(&text) {
                        ranges.push((a.min(b), a.max(b)));
                        out.push_str("([+-]?[0-9]+)");
                    } else {
                        let alts = split_top(inner);
                        if alts.len() > 1 {
                            out.push_str("(?:");
                            for (k, alt) in alts.iter().enumerate() {
                                if k > 0 {
                                    out.push('|');
                                }
                                out.push_str(&translate(alt, ranges));
                            }
                            out.push(')');
                        } else {
                            // `{single}` is not a choice: it matches itself, braces included.
                            out.push_str("\\{");
                            out.push_str(&translate(inner, ranges));
                            out.push_str("\\}");
                        }
                    }
                    i = end + 1;
                    continue;
                }
                out.push_str("\\{");
            }
            c => push_literal(&mut out, c),
        }
        i += 1;
    }
    out
}

fn push_literal(out: &mut String, c: char) {
    // The regex crate accepts an escape before any ASCII punctuation except `<` and `>`.
    if c.is_ascii_punctuation() && c != '<' && c != '>' {
        out.push('\\');
    }
    out.push(c);
}

/// Index of the `]` closing the class opened at `start`; `None` when there is none or the class
/// would span a `/` (then the `[` is literal).
fn class_end(p: &[char], start: usize) -> Option<usize> {
    let mut j = start + 1;
    if matches!(p.get(j), Some('!' | '^')) {
        j += 1;
    }
    // A `]` right after the opening is part of the class.
    if p.get(j) == Some(&']') {
        j += 1;
    }
    while j < p.len() {
        match p[j] {
            ']' => return Some(j),
            '/' => return None,
            '\\' => j += 2,
            _ => j += 1,
        }
    }
    None
}

/// Index of the `}` matching the `{` at `start` (nesting and escapes respected).
fn brace_end(p: &[char], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = start;
    while j < p.len() {
        match p[j] {
            '\\' => j += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// The comma-separated alternatives of a brace body, splitting only at its own level.
fn split_top(p: &[char]) -> Vec<&[char]> {
    let mut out = Vec::new();
    let (mut depth, mut from, mut j) = (0usize, 0usize, 0usize);
    while j < p.len() {
        match p[j] {
            '\\' => j += 1,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&p[from..j]);
                from = j + 1;
            }
            _ => {}
        }
        j += 1;
    }
    out.push(&p[from.min(p.len())..]);
    out
}

/// `"1..3"` / `"-2..+5"` -> the two integers.
fn parse_range(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.split_once("..")?;
    let int = |t: &str| -> Option<i64> {
        let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
        if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        t.parse().ok()
    };
    Some((int(a)?, int(b)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_pairs_comments_and_root() {
        let f = parse(
            "\u{feff}# top\nroot = TRUE\n\n[*]\nindent_style = space\n; comment\nIndent_Size=2\n\n[*.{js,py}]\ncharset = utf-8\nnot a pair\n",
        );
        assert!(f.root);
        assert_eq!(f.sections.len(), 2);
        assert_eq!(f.sections[0].glob, "*");
        assert_eq!(
            f.sections[0].pairs,
            vec![("indent_style".into(), "space".into()), ("indent_size".into(), "2".into())],
            "keys are lowercase"
        );
        assert_eq!(f.sections[1].glob, "*.{js,py}");
        assert!(!parse("[*]\nroot = true\n").root, "root only counts before the first section");
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn resolves_values_defaults_and_unset() {
        let p = resolve(&pairs(&[
            ("indent_style", "Space"),
            ("indent_size", "2"),
            ("end_of_line", "CRLF"),
            ("charset", "UTF-8-BOM"),
            ("trim_trailing_whitespace", "true"),
            ("insert_final_newline", "false"),
            ("max_line_length", "100"),
        ]));
        assert_eq!(p.indent_style, Some(IndentStyle::Space));
        assert_eq!((p.indent_size, p.tab_width), (Some(2), Some(2)), "tab_width defaults to indent_size");
        assert_eq!(p.end_of_line, Some(Eol::Crlf));
        assert_eq!(p.charset.as_deref(), Some("utf-8-bom"));
        assert_eq!((p.trim_trailing_whitespace, p.insert_final_newline), (Some(true), Some(false)));
        assert_eq!(p.max_line_length, Some(100));
        // indent_style = tab: the indent size is the tab width.
        let p = resolve(&pairs(&[("indent_style", "tab"), ("tab_width", "8")]));
        assert_eq!((p.indent_size, p.tab_width), (Some(8), Some(8)));
        let p = resolve(&pairs(&[("indent_size", "tab"), ("tab_width", "3"), ("max_line_length", "off")]));
        assert_eq!((p.indent_size, p.tab_width, p.max_line_length), (Some(3), Some(3), None));
        // Later pairs win; `unset` removes; junk values are ignored.
        let p = resolve(&pairs(&[
            ("indent_size", "4"),
            ("indent_size", "unset"),
            ("charset", "ebcdic"),
            ("trim_trailing_whitespace", "maybe"),
            ("end_of_line", "lf"),
            ("end_of_line", "cr"),
        ]));
        assert_eq!(p.indent_size, None);
        assert_eq!(p.charset, None);
        assert_eq!(p.trim_trailing_whitespace, None);
        assert_eq!(p.end_of_line, Some(Eol::Cr));
        assert_eq!(resolve(&[]), Props::default());
    }

    #[test]
    fn globs() {
        let yes = |g: &str, p: &str| assert!(glob_match(g, p), "{g} should match {p}");
        let no = |g: &str, p: &str| assert!(!glob_match(g, p), "{g} should not match {p}");
        yes("*", "a.txt");
        yes("*", "src/deep/a.txt");
        yes("*.py", "lib/x/a.py");
        no("*.py", "a.pyc");
        yes("Makefile", "sub/Makefile");
        no("Makefile", "Makefile.am");
        yes("*.{js,py}", "a.js");
        yes("*.{js,py}", "src/a.py");
        no("*.{js,py}", "a.rs");
        yes("{package.json,.travis.yml}", ".travis.yml");
        yes("lib/**.js", "lib/a/b/c.js");
        no("lib/**.js", "src/lib/c.js");
        yes("/lib/*.js", "lib/c.js");
        no("lib/*.js", "lib/a/c.js");
        yes("src/**/*.rs", "src/a.rs");
        yes("src/**/*.rs", "src/x/y/a.rs");
        yes("**/test/*.ts", "test/a.ts");
        yes("**/test/*.ts", "pkg/test/a.ts");
        yes("a?c", "abc");
        no("a?c", "a/c");
        yes("[abc].txt", "b.txt");
        no("[abc].txt", "d.txt");
        yes("[!abc].txt", "d.txt");
        no("[!abc].txt", "a.txt");
        yes("file[0-9].md", "file7.md");
        no("file[0-9].md", "filex.md");
        yes("file{1..3}.txt", "file2.txt");
        no("file{1..3}.txt", "file4.txt");
        yes("v{-1..+1}", "v0");
        yes("{a,{b,c}}.x", "c.x");
        yes("{single}.x", "{single}.x");
        no("{single}.x", "single.x");
        yes("a\\*b", "a*b");
        no("a\\*b", "axb");
        yes("[a.txt", "[a.txt");
        yes("*.c++", "x.c++");
        yes("ç*.txt", "çé.txt");
    }

    #[test]
    fn lookup_walks_up_to_root_and_closer_files_win() {
        let base = std::env::temp_dir().join(format!("lumen-ec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("proj/src/vendor")).unwrap();
        // Above the root file: must never be read.
        std::fs::write(base.join(".editorconfig"), "[*]\nindent_size = 7\ncharset = latin1\n").unwrap();
        std::fs::write(
            base.join("proj/.editorconfig"),
            "root = true\n[*]\nindent_style = space\nindent_size = 4\ninsert_final_newline = true\n[*.md]\ntrim_trailing_whitespace = false\n[Makefile]\nindent_style = tab\n",
        )
        .unwrap();
        std::fs::write(
            base.join("proj/src/.editorconfig"),
            "[*.rs]\nindent_size = 2\n[vendor/**]\ninsert_final_newline = unset\n",
        )
        .unwrap();
        let p = props_for(&base.join("proj/src/main.rs"));
        assert_eq!((p.indent_style, p.indent_size, p.tab_width), (Some(IndentStyle::Space), Some(2), Some(2)));
        assert_eq!(p.insert_final_newline, Some(true));
        assert_eq!(p.charset, None, "the file above root = true is ignored");
        let p = props_for(&base.join("proj/src/vendor/x.rs"));
        assert_eq!(p.insert_final_newline, None, "unset in the closer file");
        let p = props_for(&base.join("proj/README.md"));
        assert_eq!((p.indent_size, p.trim_trailing_whitespace), (Some(4), Some(false)));
        let p = props_for(&base.join("proj/Makefile"));
        assert_eq!(p.indent_style, Some(IndentStyle::Tab));
        let _ = std::fs::remove_dir_all(&base);
    }
}
