//! The Markdown of descriptions and comments, reduced to what a text page can show: headings,
//! bullets, quotes, fenced code, and plain paragraphs with the inline marks (`**`, `` ` ``,
//! links, images) taken out. Plain code, no drawing.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    H1,
    H2,
    Bullet,
    Quote,
    Code,
    Text,
    Blank,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdLine {
    pub kind: Kind,
    pub text: String,
}

/// `<!-- ... -->` removed (issue templates are full of them), also across lines.
pub fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(a) = rest.find("<!--") {
        out.push_str(&rest[..a]);
        match rest[a..].find("-->") {
            Some(z) => rest = &rest[a + z + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Inline marks out: `[text](url)` becomes `text`, `![alt](url)` becomes `[image: alt]`, bold /
/// italic stars and backticks go, `<br>` is a space.
pub fn inline(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let image = c == '!' && chars.get(i + 1) == Some(&'[');
        if c == '[' || image {
            let open = if image { i + 1 } else { i };
            if let Some(close) = chars[open..].iter().position(|&x| x == ']').map(|p| open + p)
                && chars.get(close + 1) == Some(&'(')
                && let Some(end) = chars[close + 2..].iter().position(|&x| x == ')').map(|p| close + 2 + p)
            {
                let label: String = chars[open + 1..close].iter().collect();
                if image {
                    out.push_str(&format!("[image: {}]", if label.is_empty() { "picture" } else { &label }));
                } else {
                    out.push_str(&label);
                }
                i = end + 1;
                continue;
            }
        }
        if c == '*' || c == '`' {
            i += 1;
            continue;
        }
        if c == '<'
            && chars[i..].iter().take(3).collect::<String>().eq_ignore_ascii_case("<br")
            && let Some(p) = chars[i..].iter().position(|&x| x == '>')
        {
            out.push(' ');
            i += p + 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The lines of a Markdown text, one `MdLine` per source line (long lines are wrapped by the
/// drawing). Runs of blank lines collapse to one.
pub fn lines(body: &str) -> Vec<MdLine> {
    let body = strip_comments(&body.replace("\r\n", "\n").replace('\r', "\n"));
    let mut out: Vec<MdLine> = Vec::new();
    let mut fenced = false;
    for raw in body.lines() {
        let t = raw.trim_end();
        let lead = t.trim_start();
        if lead.starts_with("```") || lead.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            out.push(MdLine { kind: Kind::Code, text: t.replace('\t', "    ") });
            continue;
        }
        if lead.is_empty() {
            if out.last().is_some_and(|l| l.kind != Kind::Blank) {
                out.push(MdLine { kind: Kind::Blank, text: String::new() });
            }
            continue;
        }
        let hashes = lead.chars().take_while(|&c| c == '#').count();
        let line = if (1..=6).contains(&hashes) && lead[hashes..].starts_with(' ') {
            let kind = if hashes == 1 { Kind::H1 } else { Kind::H2 };
            MdLine { kind, text: inline(lead[hashes..].trim()) }
        } else if let Some(rest) = lead.strip_prefix("> ").or_else(|| lead.strip_prefix('>')) {
            MdLine { kind: Kind::Quote, text: inline(rest.trim()) }
        } else if let Some(rest) = bullet(lead) {
            MdLine { kind: Kind::Bullet, text: inline(&rest) }
        } else {
            MdLine { kind: Kind::Text, text: inline(lead) }
        };
        out.push(line);
    }
    while out.last().is_some_and(|l| l.kind == Kind::Blank) {
        out.pop();
    }
    out
}

/// `- x`, `* x`, `+ x`, `1. x`, with task boxes: the text after the marker.
fn bullet(lead: &str) -> Option<String> {
    let rest = if let Some(r) =
        lead.strip_prefix("- ").or_else(|| lead.strip_prefix("* ")).or_else(|| lead.strip_prefix("+ "))
    {
        r
    } else {
        let digits = lead.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 3 {
            return None;
        }
        lead[digits..].strip_prefix(". ").or_else(|| lead[digits..].strip_prefix(") "))?
    };
    let rest = rest.trim();
    Some(if let Some(r) = rest.strip_prefix("[ ] ") {
        format!("[ ] {r}")
    } else if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
        format!("[x] {r}")
    } else {
        rest.to_string()
    })
}

/// Greedy word wrap by measured width (`measure` gives the width of a string in pixels). A word
/// wider than the line is cut. Empty text is one empty line.
pub fn wrap(text: &str, width: i32, measure: &mut dyn FnMut(&str) -> i32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ') {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if cur.is_empty() || measure(&cand) <= width {
            cur = cand;
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
        // A word wider than the line: cut it where it stops fitting.
        while measure(&cur) > width && cur.chars().count() > 1 {
            let chars: Vec<char> = cur.chars().collect();
            let mut n = chars.len() - 1;
            while n > 1 && measure(&chars[..n].iter().collect::<String>()) > width {
                n -= 1;
            }
            out.push(chars[..n].iter().collect());
            cur = chars[n..].iter().collect();
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_inline_marks_go() {
        assert_eq!(strip_comments("a<!-- hidden\nmore -->b"), "ab");
        assert_eq!(strip_comments("a<!-- open"), "a");
        assert_eq!(inline("see [the docs](https://x.y) and **bold** `code`"), "see the docs and bold code");
        assert_eq!(inline("![shot](a.png) x"), "[image: shot] x");
        assert_eq!(inline("a<br>b"), "a b");
        assert_eq!(inline("[broken](no close"), "[broken](no close");
    }

    #[test]
    fn lines_get_kinds() {
        let l =
            lines("# Title\r\n\r\n\r\ntext **x**\n- one\n* [x] done\n1. first\n> quoted\n```rust\nlet a = 1;\n```\n\n");
        let k: Vec<Kind> = l.iter().map(|m| m.kind).collect();
        assert_eq!(
            k,
            vec![Kind::H1, Kind::Blank, Kind::Text, Kind::Bullet, Kind::Bullet, Kind::Bullet, Kind::Quote, Kind::Code]
        );
        assert_eq!(l[2].text, "text x");
        assert_eq!(l[4].text, "[x] done");
        assert_eq!(l[5].text, "first");
        assert_eq!(l[7].text, "let a = 1;");
    }

    #[test]
    fn a_hash_without_a_space_is_text() {
        assert_eq!(lines("#nope")[0].kind, Kind::Text);
        assert_eq!(lines("## Sub")[0].kind, Kind::H2);
    }

    #[test]
    fn wrapping_by_width() {
        let mut m = |s: &str| s.chars().count() as i32;
        assert_eq!(wrap("aaa bbb ccc", 7, &mut m), vec!["aaa bbb", "ccc"]);
        assert_eq!(wrap("", 7, &mut m), vec![""]);
        assert_eq!(wrap("abcdefghij", 4, &mut m), vec!["abcd", "efgh", "ij"]);
    }
}
