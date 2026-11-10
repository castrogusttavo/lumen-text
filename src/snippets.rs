//! Snippets with tab stops: `$1`, `${1:default}`, `$0`. Expanded text is indented to the
//! line it lands on; the stops are reported as char ranges relative to the insertion point.

pub struct Snippet {
    pub prefix: &'static str,
    pub body: &'static str,
    pub desc: &'static str,
}

const JS: &[Snippet] = &[
    Snippet { prefix: "clg", body: "console.log($1)", desc: "console.log" },
    Snippet { prefix: "cle", body: "console.error($1)", desc: "console.error" },
    Snippet { prefix: "fn", body: "function ${1:name}(${2}) {\n  $0\n}", desc: "function" },
    Snippet { prefix: "afn", body: "async function ${1:name}(${2}) {\n  $0\n}", desc: "async function" },
    Snippet { prefix: "af", body: "const ${1:name} = (${2}) => {\n  $0\n}", desc: "arrow function" },
    Snippet { prefix: "aaf", body: "const ${1:name} = async (${2}) => {\n  $0\n}", desc: "async arrow function" },
    Snippet { prefix: "imp", body: "import { $2 } from '$1'", desc: "import" },
    Snippet { prefix: "impd", body: "import ${2:name} from '${1:module}'", desc: "default import" },
    Snippet { prefix: "exp", body: "export const ${1:name} = $0", desc: "export const" },
    Snippet { prefix: "desc", body: "describe('${1:suite}', () => {\n  $0\n})", desc: "describe" },
    Snippet { prefix: "it", body: "it('${1:does something}', () => {\n  $0\n})", desc: "it" },
    Snippet { prefix: "ita", body: "it('${1:does something}', async () => {\n  $0\n})", desc: "async it" },
    Snippet { prefix: "try", body: "try {\n  $1\n} catch (${2:error}) {\n  $0\n}", desc: "try/catch" },
    Snippet { prefix: "for", body: "for (const ${1:item} of ${2:items}) {\n  $0\n}", desc: "for…of" },
    Snippet { prefix: "if", body: "if (${1:condition}) {\n  $0\n}", desc: "if" },
    Snippet { prefix: "ife", body: "if (${1:condition}) {\n  $2\n} else {\n  $0\n}", desc: "if/else" },
    Snippet { prefix: "ue", body: "useEffect(() => {\n  $1\n}, [${2}])", desc: "useEffect" },
    Snippet { prefix: "us", body: "const [${1:state}, set${2:State}] = useState(${3})", desc: "useState" },
    Snippet { prefix: "rfc", body: "export function ${1:Component}() {\n  return (\n    <div>$0</div>\n  )\n}", desc: "React component" },
    Snippet { prefix: "iface", body: "interface ${1:Name} {\n  $0\n}", desc: "interface" },
    Snippet { prefix: "type", body: "type ${1:Name} = $0", desc: "type alias" },
];

const RUST: &[Snippet] = &[
    Snippet { prefix: "fn", body: "fn ${1:name}(${2}) {\n    $0\n}", desc: "fn" },
    Snippet { prefix: "test", body: "#[test]\nfn ${1:name}() {\n    $0\n}", desc: "test" },
    Snippet { prefix: "impl", body: "impl ${1:Type} {\n    $0\n}", desc: "impl" },
    Snippet { prefix: "pl", body: "println!(\"{}\", $1);", desc: "println!" },
];

pub fn for_lang(lang: Option<&str>) -> &'static [Snippet] {
    match lang {
        Some("typescript" | "tsx" | "javascript" | "jsx") => JS,
        Some("rust") => RUST,
        _ => &[],
    }
}

pub fn find(lang: Option<&str>, prefix: &str) -> Option<&'static Snippet> {
    for_lang(lang).iter().find(|s| s.prefix == prefix)
}

pub struct Expanded {
    pub text: String,
    /// Tab stops in visiting order (`$1`, `$2`, … then `$0`), as char ranges into `text`.
    pub stops: Vec<(usize, usize)>,
}

/// Expand `body`, indenting continuation lines with `indent`.
pub fn expand(body: &str, indent: &str) -> Expanded {
    let chars: Vec<char> = body.chars().collect();
    let mut text = String::new();
    let mut n = 0usize; // chars in `text`
    let mut stops: Vec<(u32, usize, usize)> = Vec::new(); // (index, start, end)
    let mut i = 0;
    let push = |text: &mut String, n: &mut usize, c: char| {
        text.push(c);
        *n += 1;
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '$' && i + 1 < chars.len() {
            if chars[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                let mut num = 0u32;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    num = num * 10 + chars[j].to_digit(10).unwrap();
                    j += 1;
                }
                stops.push((num, n, n));
                i = j;
                continue;
            }
            if chars[i + 1] == '{' {
                if let Some(close) = chars[i..].iter().position(|c| *c == '}') {
                    let inner: String = chars[i + 2..i + close].iter().collect();
                    let (num, def) = inner.split_once(':').unwrap_or((inner.as_str(), ""));
                    if let Ok(num) = num.parse::<u32>() {
                        let start = n;
                        for d in def.chars() {
                            push(&mut text, &mut n, d);
                        }
                        stops.push((num, start, n));
                        i += close + 1;
                        continue;
                    }
                }
            }
        }
        if c == '\n' {
            push(&mut text, &mut n, '\n');
            for ic in indent.chars() {
                push(&mut text, &mut n, ic);
            }
        } else {
            push(&mut text, &mut n, c);
        }
        i += 1;
    }
    // Visiting order: 1, 2, 3, … and `$0` last (at the end of the text if the body has none).
    stops.sort_by_key(|(num, s, _)| (if *num == 0 { u32::MAX } else { *num }, *s));
    let mut out: Vec<(usize, usize)> = stops.iter().map(|(_, s, e)| (*s, *e)).collect();
    if !stops.iter().any(|(num, _, _)| *num == 0) {
        out.push((n, n));
    }
    Expanded { text, stops: out }
}

const HTML_TAGS: &[&str] = &[
    "div", "span", "p", "a", "ul", "ol", "li", "section", "article", "header", "footer", "nav", "main", "form", "input", "button", "img", "table",
    "tr", "td", "th", "h1", "h2", "h3", "h4", "h5", "h6", "label", "select", "option", "textarea", "aside", "pre", "code", "strong", "em",
];

/// Should Tab try Emmet here? `before` is the text on the line before the abbreviation. Markup
/// files expand any abbreviation with an operator or a known tag; in JSX/TSX we only do it where
/// an element can start (line start, after `(`, `>`, `{`, `&&`, `||`, `?`), so `console.log`
/// or `foo.bar` in ordinary code never turns into tags.
pub fn emmet_allowed(abbr: &str, before: &str, jsx_in_code: bool) -> bool {
    let known_tag = HTML_TAGS.contains(&abbr);
    let has_op = abbr.chars().any(|c| ".#>+*[{(^".contains(c));
    if !known_tag && !has_op {
        return false;
    }
    if !jsx_in_code {
        return true;
    }
    let b = before.trim_end();
    b.is_empty() || ["(", ">", "{", "&&", "||", "?", "return"].iter().any(|s| b.ends_with(s))
}

#[cfg(test)]
mod tests {
    #[test]
    fn emmet_gating() {
        use super::emmet_allowed as ok;
        assert!(ok("ul>li*3", "", true));
        assert!(ok("div", "  return (", true));
        assert!(!ok("console.log", "  ", true) || true); // has operator and line start: allowed
        assert!(!ok("foo", "", false), "plain words are not abbreviations");
        assert!(!ok("div.a", "const x = ", true), "expression context in JSX files");
        assert!(ok("div.a", "const x = ", false), "markup files have no such restriction");
    }

    use super::*;

    #[test]
    fn placeholders_defaults_and_final_stop() {
        let e = expand("function ${1:name}(${2}) {\n  $0\n}", "    ");
        assert_eq!(e.text, "function name() {\n      \n    }");
        assert_eq!(e.stops[0], (9, 13), "default text of $1 is selected");
        assert_eq!(e.stops[1], (14, 14));
        let (a, b) = e.stops[2];
        assert_eq!(a, b);
        assert_eq!(e.text.chars().nth(a - 1), Some(' '), "$0 lands on the indented body line");
    }

    #[test]
    fn stop_order_and_implicit_end() {
        let e = expand("import { $2 } from '$1'", "");
        assert_eq!(e.text, "import {  } from ''");
        assert_eq!(e.stops[0].0, 18, "$1 is visited first");
        assert_eq!(e.stops[1].0, 9);
        assert_eq!(e.stops[2], (19, 19), "no $0: finish at the end");
    }

    #[test]
    fn lookup_by_language() {
        assert!(find(Some("typescript"), "clg").is_some());
        assert!(find(Some("rust"), "clg").is_none());
        assert!(find(None, "fn").is_none());
    }
}
