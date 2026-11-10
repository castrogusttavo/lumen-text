//! Emmet abbreviation expander producing snippet bodies (see `snippets.rs`).
//!
//! Output conventions:
//! - indentation unit is 2 spaces; an element with children puts each child on its own line,
//!   an element with only text (or nothing) stays on one line; top-level siblings are separated
//!   by a newline;
//! - every empty spot (empty element content, empty attribute value) becomes a tab stop
//!   `$1`, `$2`, ... in order of appearance, and `$0` is appended at the very end;
//! - attribute order: defaults (`href`, `src`, ...), `id`, `class`, then explicit attributes;
//! - every `$` in the abbreviation is consumed as item numbering, so the output never contains
//!   a `$` other than tab stops (limitation: a literal `$` cannot be produced from `{text}`).
#![allow(dead_code)]

/// Output flavor: `Jsx` renames `class`/`for` and self-closes void tags (`<br />`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavor {
    Html,
    Jsx,
}

const MARK: char = '\u{1}';
const MAX_ELEMENTS: usize = 5000;
const MAX_COUNT: usize = 500;

// ---------------------------------------------------------------- parsing

#[derive(Debug)]
struct Elem {
    name: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, Option<String>)>,
    text: Option<String>,
    children: Vec<Item>,
    count: Option<usize>,
}

#[derive(Debug)]
enum Item {
    Elem(Elem),
    Group { children: Vec<Item>, count: Option<usize> },
}

struct Parser {
    s: Vec<char>,
    pos: usize,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '$' | '@')
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn name(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(is_name_char) {
            self.pos += 1;
        }
        self.s[start..self.pos].iter().collect()
    }

    fn starts_unit(&self) -> bool {
        self.peek().is_some_and(|c| is_name_char(c) || matches!(c, '.' | '#' | '[' | '{' | '('))
    }

    /// `unit (('+' unit) | ('>' chain ['^' [unit]]))*`; stops (without consuming) at `^`, `)` or end.
    fn chain(&mut self) -> Option<Vec<Item>> {
        let mut items = vec![self.unit()?];
        loop {
            match self.peek() {
                Some('+') => {
                    self.pos += 1;
                    items.push(self.unit()?);
                }
                Some('>') => {
                    self.pos += 1;
                    let kids = self.chain()?;
                    match items.last_mut() {
                        Some(Item::Elem(e)) => e.children = kids,
                        _ => return None,
                    }
                    if self.peek() == Some('^') {
                        self.pos += 1;
                        if self.starts_unit() {
                            items.push(self.unit()?);
                        }
                    }
                }
                Some('^') | Some(')') | None => break,
                _ => return None,
            }
        }
        Some(items)
    }

    fn count(&mut self) -> Option<Option<usize>> {
        if self.peek() != Some('*') {
            return Some(None);
        }
        self.pos += 1;
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let n: usize = self.s[start..self.pos].iter().collect::<String>().parse().ok()?;
        (1..=MAX_COUNT).contains(&n).then_some(Some(n))
    }

    fn unit(&mut self) -> Option<Item> {
        if self.peek() == Some('(') {
            self.pos += 1;
            let children = self.chain()?;
            if self.peek() != Some(')') {
                return None;
            }
            self.pos += 1;
            let count = self.count()?;
            return Some(Item::Group { children, count });
        }
        let mut e =
            Elem { name: None, id: None, classes: vec![], attrs: vec![], text: None, children: vec![], count: None };
        let name = self.name();
        let mut any = !name.is_empty();
        if any {
            e.name = Some(name);
        }
        loop {
            match self.peek() {
                Some('.') => {
                    self.pos += 1;
                    let n = self.name();
                    if n.is_empty() {
                        return None;
                    }
                    e.classes.push(n);
                }
                Some('#') => {
                    self.pos += 1;
                    let n = self.name();
                    if n.is_empty() {
                        return None;
                    }
                    e.id = Some(n);
                }
                Some('[') => {
                    self.pos += 1;
                    self.attrs(&mut e.attrs)?;
                }
                Some('{') => {
                    self.pos += 1;
                    let t = self.text()?;
                    e.text.get_or_insert_with(String::new).push_str(&t);
                }
                _ => break,
            }
            any = true;
        }
        if !any {
            return None;
        }
        e.count = self.count()?;
        Some(Item::Elem(e))
    }

    fn text(&mut self) -> Option<String> {
        let start = self.pos;
        let mut depth = 0;
        while let Some(c) = self.peek() {
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => {
                    let t = self.s[start..self.pos].iter().collect();
                    self.pos += 1;
                    return Some(t);
                }
                '}' => depth -= 1,
                _ => {}
            }
            self.pos += 1;
        }
        None
    }

    fn attrs(&mut self, out: &mut Vec<(String, Option<String>)>) -> Option<()> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.pos += 1;
            }
            if self.peek()? == ']' {
                self.pos += 1;
                return Some(());
            }
            let start = self.pos;
            while self.peek().is_some_and(|c| !c.is_whitespace() && c != '=' && c != ']') {
                self.pos += 1;
            }
            let key: String = self.s[start..self.pos].iter().collect();
            if key.is_empty() {
                return None;
            }
            if self.peek() == Some('=') {
                self.pos += 1;
                let val = match self.peek()? {
                    q @ ('"' | '\'') => {
                        self.pos += 1;
                        let vs = self.pos;
                        while self.peek()? != q {
                            self.pos += 1;
                        }
                        let v = self.s[vs..self.pos].iter().collect();
                        self.pos += 1;
                        v
                    }
                    _ => {
                        let vs = self.pos;
                        while self.peek().is_some_and(|c| !c.is_whitespace() && c != ']') {
                            self.pos += 1;
                        }
                        self.s[vs..self.pos].iter().collect()
                    }
                };
                out.push((key, Some(val)));
            } else {
                out.push((key, None));
            }
        }
    }
}

// -------------------------------------------------------------- expansion

enum Node {
    Text(String),
    El(Out),
}

struct Out {
    tag: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Node>,
    void: bool,
}

/// Replaces runs of `$` by the (zero-padded) item number; `$@-` / `$@3` modifiers are skipped.
fn number(s: &str, n: usize) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] != '$' {
            out.push(cs[i]);
            i += 1;
            continue;
        }
        let mut w = 0;
        while i < cs.len() && cs[i] == '$' {
            w += 1;
            i += 1;
        }
        if i < cs.len() && cs[i] == '@' {
            i += 1;
            while i < cs.len() && (cs[i] == '-' || cs[i].is_ascii_digit()) {
                i += 1;
            }
        }
        out.push_str(&format!("{n:0w$}"));
    }
    out
}

fn implicit_tag(parent: Option<&str>) -> &'static str {
    match parent {
        Some("ul" | "ol") => "li",
        Some("table" | "tbody" | "thead" | "tfoot") => "tr",
        Some("tr") => "td",
        Some("select" | "optgroup" | "datalist") => "option",
        Some(
            "em" | "strong" | "span" | "a" | "b" | "i" | "u" | "small" | "code" | "label" | "abbr" | "cite" | "q" | "s"
            | "sub" | "sup" | "mark" | "kbd" | "var" | "p",
        ) => "span",
        _ => "div",
    }
}

fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "img"
            | "br"
            | "hr"
            | "input"
            | "meta"
            | "link"
            | "area"
            | "base"
            | "col"
            | "embed"
            | "source"
            | "track"
            | "wbr"
    )
}

fn default_attrs(tag: &str) -> &'static [(&'static str, &'static str)] {
    match tag {
        "a" => &[("href", "")],
        "img" => &[("src", ""), ("alt", "")],
        "input" => &[("type", "text")],
        "link" => &[("rel", "stylesheet"), ("href", "")],
        _ => &[],
    }
}

fn set_attr(attrs: &mut Vec<(String, String)>, k: String, v: String) {
    match attrs.iter_mut().find(|(n, _)| *n == k) {
        Some(slot) => slot.1 = v,
        None => attrs.push((k, v)),
    }
}

struct Ctx {
    flavor: Flavor,
    budget: usize,
}

impl Ctx {
    fn items(&mut self, items: &[Item], parent: Option<&str>, idx: usize, out: &mut Vec<Node>) -> Option<()> {
        for item in items {
            let (count, kids) = match item {
                Item::Elem(e) => (e.count, &e.children),
                Item::Group { children, count } => (*count, children),
            };
            for i in 1..=count.unwrap_or(1) {
                let cur = if count.is_some() { i } else { idx };
                match item {
                    Item::Elem(e) => out.push(self.elem(e, parent, cur)?),
                    Item::Group { .. } => self.items(kids, parent, cur, out)?,
                }
            }
        }
        Some(())
    }

    fn elem(&mut self, e: &Elem, parent: Option<&str>, n: usize) -> Option<Node> {
        let bare = e.name.is_none() && e.id.is_none() && e.classes.is_empty() && e.attrs.is_empty();
        if bare {
            // Only `{text}`: a plain text node.
            return Some(Node::Text(number(e.text.as_deref().unwrap_or(""), n)));
        }
        self.budget = self.budget.checked_sub(1)?;
        let tag = match &e.name {
            Some(name) => number(name, n),
            None => implicit_tag(parent).to_string(),
        };
        let void = is_void(&tag);
        let mut attrs: Vec<(String, String)> =
            default_attrs(&tag).iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut classes: Vec<String> = e.classes.iter().map(|c| number(c, n)).collect();
        let mut others = vec![];
        for (k, v) in &e.attrs {
            let k = number(k, n);
            let v = number(v.as_deref().unwrap_or(""), n);
            if k == "class" || k == "className" {
                classes.extend(v.split_whitespace().map(str::to_string));
            } else {
                others.push((k, v));
            }
        }
        if let Some(id) = &e.id {
            set_attr(&mut attrs, "id".into(), number(id, n));
        }
        if !classes.is_empty() {
            set_attr(&mut attrs, "class".into(), classes.join(" "));
        }
        for (k, v) in others {
            set_attr(&mut attrs, k, v);
        }
        if self.flavor == Flavor::Jsx {
            for (k, _) in attrs.iter_mut() {
                match k.as_str() {
                    "class" => *k = "className".into(),
                    "for" => *k = "htmlFor".into(),
                    _ => {}
                }
            }
        }
        let mut children = vec![];
        self.items(&e.children, Some(&tag), n, &mut children)?;
        let text = number(e.text.as_deref().unwrap_or(""), n);
        if void && (!children.is_empty() || !text.is_empty()) {
            return None;
        }
        Some(Node::El(Out { tag, attrs, text, children, void }))
    }
}

// -------------------------------------------------------------- rendering

fn pad(depth: usize) -> String {
    "  ".repeat(depth)
}

fn render(node: &Node, depth: usize, flavor: Flavor) -> String {
    let el = match node {
        Node::Text(t) => return t.clone(),
        Node::El(el) => el,
    };
    let mut open = format!("<{}", el.tag);
    for (k, v) in &el.attrs {
        let v = if v.is_empty() { MARK.to_string() } else { v.replace('"', "&quot;") };
        open.push_str(&format!(" {k}=\"{v}\""));
    }
    if el.void {
        open.push_str(if flavor == Flavor::Jsx { " />" } else { ">" });
        return open;
    }
    open.push('>');
    let close = format!("</{}>", el.tag);
    if el.children.is_empty() {
        let body = if el.text.is_empty() { MARK.to_string() } else { el.text.clone() };
        return format!("{open}{body}{close}");
    }
    let mut s = open;
    if !el.text.is_empty() {
        s.push_str(&format!("\n{}{}", pad(depth + 1), el.text));
    }
    for c in &el.children {
        s.push_str(&format!("\n{}{}", pad(depth + 1), render(c, depth + 1, flavor)));
    }
    s.push_str(&format!("\n{}{}", pad(depth), close));
    s
}

/// Expands an abbreviation into a snippet body, or `None` if it is invalid.
pub fn expand(abbr: &str, flavor: Flavor) -> Option<String> {
    let mut p = Parser { s: abbr.chars().collect(), pos: 0 };
    let items = p.chain()?;
    if p.pos != p.s.len() {
        return None;
    }
    let mut ctx = Ctx { flavor, budget: MAX_ELEMENTS };
    let mut nodes = vec![];
    ctx.items(&items, None, 1, &mut nodes)?;
    let text = nodes.iter().map(|n| render(n, 0, flavor)).collect::<Vec<_>>().join("\n");
    let mut out = String::with_capacity(text.len() + 8);
    let mut stop = 0;
    for c in text.chars() {
        if c == MARK {
            stop += 1;
            out.push_str(&format!("${stop}"));
        } else {
            out.push(c);
        }
    }
    out.push_str("$0");
    Some(out)
}

fn is_abbr_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '.' | '#'
                | '>'
                | '+'
                | '*'
                | '^'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '='
                | '"'
                | '\''
                | '$'
                | '@'
                | '-'
                | '_'
                | ':'
                | '/'
        )
}

/// Finds the abbreviation ending at the cursor: returns its start (char index) and text.
/// Whitespace is allowed inside `{...}` and `[...]`.
pub fn extract_abbr(line_prefix: &str) -> Option<(usize, String)> {
    let cs: Vec<char> = line_prefix.chars().collect();
    let mut i = cs.len();
    let mut inside: Option<char> = None;
    while i > 0 {
        let c = cs[i - 1];
        match inside {
            Some(open) => {
                if c == open {
                    inside = None;
                }
            }
            None => match c {
                '}' => inside = Some('{'),
                ']' => inside = Some('['),
                _ if is_abbr_char(c) => {}
                _ => break,
            },
        }
        i -= 1;
    }
    while i < cs.len() && cs[i].is_whitespace() {
        i += 1;
    }
    (i < cs.len()).then(|| (i, cs[i..].iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Flavor::{Html, Jsx};

    fn h(a: &str) -> String {
        expand(a, Html).unwrap_or_else(|| panic!("invalid: {a}"))
    }

    #[test]
    fn bare_tag() {
        assert_eq!(h("div"), "<div>$1</div>$0");
    }

    #[test]
    fn children_and_multiplication() {
        assert_eq!(h("ul>li*2"), "<ul>\n  <li>$1</li>\n  <li>$2</li>\n</ul>$0");
    }

    #[test]
    fn implicit_div_class_and_id() {
        assert_eq!(h(".box"), "<div class=\"box\">$1</div>$0");
        assert_eq!(h("#main"), "<div id=\"main\">$1</div>$0");
    }

    #[test]
    fn implicit_li_tr_td_option() {
        assert_eq!(h("ul>.a"), "<ul>\n  <li class=\"a\">$1</li>\n</ul>$0");
        assert_eq!(h("table>.r>.c"), "<table>\n  <tr class=\"r\">\n    <td class=\"c\">$1</td>\n  </tr>\n</table>$0");
        assert_eq!(h("select>.o"), "<select>\n  <option class=\"o\">$1</option>\n</select>$0");
    }

    #[test]
    fn implicit_span_in_inline_parent() {
        assert_eq!(h("em>.x"), "<em>\n  <span class=\"x\">$1</span>\n</em>$0");
    }

    #[test]
    fn classes_merge_and_id_order() {
        assert_eq!(h(".a.b"), "<div class=\"a b\">$1</div>$0");
        assert_eq!(h("p.a#x.b"), "<p id=\"x\" class=\"a b\">$1</p>$0");
    }

    #[test]
    fn attributes() {
        assert_eq!(h("a[title=\"v 2\" data-x]"), "<a href=\"$1\" title=\"v 2\" data-x=\"$2\">$3</a>$0");
        assert_eq!(h("a[href=x]"), "<a href=\"x\">$1</a>$0");
        assert_eq!(h("p[a='1' b=2]{t}"), "<p a=\"1\" b=\"2\">t</p>$0");
    }

    #[test]
    fn explicit_class_attr_merges() {
        assert_eq!(h("p.a[class=b]"), "<p class=\"a b\">$1</p>$0");
    }

    #[test]
    fn text() {
        assert_eq!(h("p{hello world}"), "<p>hello world</p>$0");
        assert_eq!(h("{just text}"), "just text$0");
    }

    #[test]
    fn text_with_children() {
        assert_eq!(h("p{hi}>b"), "<p>\n  hi\n  <b>$1</b>\n</p>$0");
    }

    #[test]
    fn siblings() {
        assert_eq!(h("h1+p"), "<h1>$1</h1>\n<p>$2</p>$0");
    }

    #[test]
    fn climb_up() {
        assert_eq!(h("div>p>span^em"), "<div>\n  <p>\n    <span>$1</span>\n  </p>\n  <em>$2</em>\n</div>$0");
        assert_eq!(
            h("div>p>span^^section"),
            "<div>\n  <p>\n    <span>$1</span>\n  </p>\n</div>\n<section>$2</section>$0"
        );
    }

    #[test]
    fn numbering() {
        assert_eq!(
            h("li.item$*3"),
            "<li class=\"item1\">$1</li>\n<li class=\"item2\">$2</li>\n<li class=\"item3\">$3</li>$0"
        );
        assert_eq!(h("p{n$$}*2"), "<p>n01</p>\n<p>n02</p>$0");
        assert_eq!(h("div#d$*2"), "<div id=\"d1\">$1</div>\n<div id=\"d2\">$2</div>$0");
        assert_eq!(h("h$[t=a$]{x}*2"), "<h1 t=\"a1\">x</h1>\n<h2 t=\"a2\">x</h2>$0");
    }

    #[test]
    fn numbering_inherited_by_children() {
        assert_eq!(h("ul>li*2>b{n$}"), "<ul>\n  <li>\n    <b>n1</b>\n  </li>\n  <li>\n    <b>n2</b>\n  </li>\n</ul>$0");
    }

    #[test]
    fn groups() {
        assert_eq!(h("(dt+dd)*2"), "<dt>$1</dt>\n<dd>$2</dd>\n<dt>$3</dt>\n<dd>$4</dd>$0");
        assert_eq!(h("ul>(li{x$})*2"), "<ul>\n  <li>x1</li>\n  <li>x2</li>\n</ul>$0");
        assert_eq!(h("(a{1}+b{2})"), "<a href=\"$1\">1</a>\n<b>2</b>$0");
        assert_eq!(h("ul>(.a)"), "<ul>\n  <li class=\"a\">$1</li>\n</ul>$0");
    }

    #[test]
    fn void_and_default_attrs_html() {
        assert_eq!(h("img"), "<img src=\"$1\" alt=\"$2\">$0");
        assert_eq!(h("br"), "<br>$0");
        assert_eq!(h("input"), "<input type=\"text\">$0");
        assert_eq!(h("input[type=checkbox]"), "<input type=\"checkbox\">$0");
        assert_eq!(h("link"), "<link rel=\"stylesheet\" href=\"$1\">$0");
        assert_eq!(h("a"), "<a href=\"$1\">$2</a>$0");
    }

    #[test]
    fn jsx_flavor() {
        let j = |a| expand(a, Jsx).unwrap();
        assert_eq!(j("br"), "<br />$0");
        assert_eq!(j("img"), "<img src=\"$1\" alt=\"$2\" />$0");
        assert_eq!(j(".a"), "<div className=\"a\">$1</div>$0");
        assert_eq!(j("label[for=x]"), "<label htmlFor=\"x\">$1</label>$0");
    }

    #[test]
    fn invalid_abbreviations() {
        for bad in [
            "", " ", ">", "a>", "a++b", "(a", "a)", "a[", "a{x", "a*", "a*0", "^a", "a^b", "+a", "br>b", "br{x}",
            "a[=x]", "a.", "a#", "(a)>b", "a*9999",
        ] {
            assert_eq!(expand(bad, Html), None, "{bad:?} should be invalid");
        }
    }

    #[test]
    fn element_budget() {
        assert_eq!(expand("div*500>div*500>div*500", Html), None);
    }

    #[test]
    fn extract_basic() {
        assert_eq!(extract_abbr("div"), Some((0, "div".into())));
        assert_eq!(extract_abbr("  div>p"), Some((2, "div>p".into())));
        assert_eq!(extract_abbr("foo ul>li*3"), Some((4, "ul>li*3".into())));
        assert_eq!(extract_abbr("<div"), Some((1, "div".into())));
    }

    #[test]
    fn extract_empty() {
        assert_eq!(extract_abbr(""), None);
        assert_eq!(extract_abbr("   "), None);
        assert_eq!(extract_abbr("div "), None);
    }

    #[test]
    fn extract_with_inner_whitespace() {
        assert_eq!(extract_abbr("x = a{hello world}"), Some((4, "a{hello world}".into())));
        assert_eq!(extract_abbr("a[b=\"c d\"]"), Some((0, "a[b=\"c d\"]".into())));
    }
}
