//! Conservative, idempotent formatting: only the leading whitespace of lines changes.
//!
//! Each block (`if`, `switch`, `for`) puts its branch and closing tags on the indentation of its
//! opening line and indents the content between them one level deeper. The relative
//! indentation of the HTML inside a body is preserved (the body is shifted as a whole), and
//! `<pre>` / `<textarea>` content is never touched. A template with errors is left alone.

use serde_json::{Value, json};

use super::conv::{range_from, span_json};
use super::docs::Doc;
use crate::ast::{Node, Span};
use crate::diagnostics::Severity;

pub struct Options {
    pub tab_size: usize,
    pub insert_spaces: bool,
}

impl Options {
    pub fn from_params(params: &Value) -> Options {
        let o = &params["options"];
        let tab_size = o["tabSize"].as_u64().map_or(4, |n| n.clamp(1, 16) as usize);
        Options { tab_size, insert_spaces: o["insertSpaces"].as_bool().unwrap_or(true) }
    }
}

/// Replacement of one line's leading whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineEdit {
    pub span: Span,
    pub text: String,
}

fn measure(lead: &str, tab: usize) -> usize {
    lead.chars().fold(0, |col, c| if c == '\t' { col + tab - col % tab } else { col + 1 })
}

fn render(width: usize, opts: &Options) -> String {
    if opts.insert_spaces {
        " ".repeat(width)
    } else {
        "\t".repeat(width / opts.tab_size) + &" ".repeat(width % opts.tab_size)
    }
}

/// Byte ranges whose lines must keep their whitespace.
fn raw_ranges(text: &str) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    for name in ["pre", "textarea"] {
        let open = format!("<{name}");
        let close = format!("</{name}");
        let mut from = 0;
        while let Some(i) = lower[from..].find(&open) {
            let start = from + i;
            let after = start + open.len();
            let real = lower[after..].chars().next().is_none_or(|c| c == '>' || c == '/' || c.is_whitespace());
            if !real {
                from = after;
                continue;
            }
            let end = lower[after..].find(&close).map_or(lower.len(), |j| after + j);
            out.push((start, end));
            from = end.max(after);
        }
    }
    out
}

struct Fmt<'a> {
    doc: &'a Doc,
    unit: usize,
    ws_end: Vec<usize>,
    width: Vec<usize>,
    blank: Vec<bool>,
    raw: Vec<bool>,
    target: Vec<Option<usize>>,
}

impl<'a> Fmt<'a> {
    fn new(doc: &'a Doc, opts: &Options) -> Fmt<'a> {
        let n = doc.index.line_count();
        // Ranges sorted by start, with the furthest end seen so far: whether a line start lies in
        // any range is then one binary search (a scan over all ranges per line is quadratic on a
        // page with thousands of `<pre>` blocks).
        let mut raws = raw_ranges(&doc.text);
        raws.sort_unstable();
        let mut reach: Vec<usize> = Vec::with_capacity(raws.len());
        for &(_, e) in &raws {
            reach.push(reach.last().map_or(e, |&m| m.max(e)));
        }
        let in_raw = |at: usize| {
            let n = raws.partition_point(|&(s, _)| s < at);
            n > 0 && reach[n - 1] >= at
        };
        let mut f = Fmt {
            doc,
            unit: opts.tab_size,
            ws_end: Vec::with_capacity(n),
            width: Vec::with_capacity(n),
            blank: Vec::with_capacity(n),
            raw: Vec::with_capacity(n),
            target: vec![None; n],
        };
        for l in 0..n {
            let sp = doc.index.line_span(l).unwrap_or_default();
            let line = &doc.text[sp.start..sp.end];
            let rest = line.trim_start_matches([' ', '\t']);
            let lead = &line[..line.len() - rest.len()];
            f.ws_end.push(sp.start + lead.len());
            f.width.push(measure(lead, opts.tab_size));
            f.blank.push(rest.is_empty());
            f.raw.push(in_raw(sp.start));
        }
        f
    }

    fn line_start(&self, l: usize) -> usize {
        self.doc.index.line_span(l).map_or(0, |s| s.start)
    }

    /// Current (possibly already reassigned) indentation of the line containing `offset`.
    fn base(&self, offset: usize) -> usize {
        let l = self.doc.index.line_of(offset);
        self.target[l].unwrap_or(self.width[l])
    }

    /// Give a tag its indentation, when it is the first thing on its line.
    fn place(&mut self, tag: Span, indent: usize) {
        let l = self.doc.index.line_of(tag.start);
        if self.ws_end[l] == tag.start && !self.raw[l] {
            self.target[l] = Some(indent);
        }
    }

    /// Shift the lines owned directly by `list` (not by a nested block) so the shallowest sits
    /// at `base`. `end` is where the body stops (the next branch or closing tag).
    fn body(&mut self, list: &[Node], end: usize, base: usize) {
        let mut lines = Vec::new();
        for n in list {
            let (lo, hi) = match n {
                Node::Text(s) => (s.start, s.end),
                Node::Import(i) => (i.tag.span.start, i.tag.span.end),
                Node::Statement(t) | Node::Echo(t) | Node::Stray(t) => (t.span.start, t.span.end),
                Node::If(x) => (x.span.start, x.open.span.end),
                Node::Switch(x) => (x.span.start, x.open.span.end),
                Node::For(x) => (x.span.start, x.open.span.end),
            };
            let first = self.doc.index.line_of(lo);
            let last = self.doc.index.line_of(hi);
            for l in first..=last {
                let ls = self.line_start(l);
                if ls >= lo && ls < hi && !self.blank[l] && !self.raw[l] && self.ws_end[l] < end {
                    lines.push(l);
                }
            }
        }
        let Some(min) = lines.iter().map(|&l| self.width[l]).min() else { return };
        for l in lines {
            self.target[l] = Some(self.width[l] - min + base);
        }
    }

    fn walk(&mut self, nodes: &[Node]) {
        for n in nodes {
            match n {
                Node::If(x) => {
                    let base = self.base(x.open.span.start);
                    let last_end = x.end.map_or(x.span.end, |e| e.span.start);
                    let first_end = x.branches.first().map_or(last_end, |b| b.tag.span.start);
                    self.body(&x.then, first_end, base + self.unit);
                    for (i, b) in x.branches.iter().enumerate() {
                        let end = x.branches.get(i + 1).map_or(last_end, |nb| nb.tag.span.start);
                        self.place(b.tag.span, base);
                        self.body(&b.body, end, base + self.unit);
                    }
                    if let Some(e) = &x.end {
                        self.place(e.span, base);
                    }
                    self.walk(&x.then);
                    for b in &x.branches {
                        self.walk(&b.body);
                    }
                }
                Node::Switch(x) => {
                    let base = self.base(x.open.span.start);
                    let last_end = x.end.map_or(x.span.end, |e| e.span.start);
                    for (i, b) in x.cases.iter().enumerate() {
                        let end = x.cases.get(i + 1).map_or(last_end, |nb| nb.tag.span.start);
                        self.place(b.tag.span, base + self.unit);
                        let case_base = self.base(b.tag.span.start);
                        self.body(&b.body, end, case_base + self.unit);
                    }
                    if let Some(e) = &x.end {
                        self.place(e.span, base);
                    }
                    for b in &x.cases {
                        self.walk(&b.body);
                    }
                }
                Node::For(x) => {
                    let base = self.base(x.open.span.start);
                    let last_end = x.end.map_or(x.span.end, |e| e.span.start);
                    self.body(&x.body, last_end, base + self.unit);
                    if let Some(e) = &x.end {
                        self.place(e.span, base);
                    }
                    self.walk(&x.body);
                }
                _ => {}
            }
        }
    }
}

/// Edits that reindent the whole document. Empty when the template has errors.
pub fn format(doc: &Doc, opts: &Options) -> Vec<LineEdit> {
    if doc.parse.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Vec::new();
    }
    let mut f = Fmt::new(doc, opts);
    f.walk(&doc.parse.tree.nodes);
    let mut edits = Vec::new();
    for (l, t) in f.target.iter().enumerate() {
        let Some(width) = *t else { continue };
        let ls = f.line_start(l);
        let span = Span::new(ls, f.ws_end[l]);
        let new = render(width, opts);
        if span.slice(&doc.text) != new {
            edits.push(LineEdit { span, text: new });
        }
    }
    edits
}

/// Keep only the edits on lines touched by `range` (a range formatting request).
pub fn restrict(doc: &Doc, edits: Vec<LineEdit>, range: &Value) -> Vec<LineEdit> {
    let Some(r) = range_from(range) else { return edits };
    edits
        .into_iter()
        .filter(|e| {
            let l = doc.index.line_of(e.span.start) as u32;
            r.start.line <= l && l <= r.end.line
        })
        .collect()
}

pub fn edits_json(doc: &Doc, edits: &[LineEdit]) -> Value {
    Value::Array(edits.iter().map(|e| json!({"range": span_json(&doc.index, e.span), "newText": e.text})).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SP4: Options = Options { tab_size: 4, insert_spaces: true };

    fn apply(text: &str, edits: &[LineEdit]) -> String {
        let mut out = text.to_owned();
        for e in edits.iter().rev() {
            out.replace_range(e.span.start..e.span.end, &e.text);
        }
        out
    }

    fn fmt(text: &str, opts: &Options) -> String {
        let doc = Doc::new(1, text.into());
        apply(text, &format(&doc, opts))
    }

    #[test]
    fn nests_blocks() {
        let src = "<div>\n<go:if a/>\n<p>x</p>\n<go:for i/>\n<li/>\n<go:endfor/>\n<go:endif/>\n</div>";
        let want =
            "<div>\n<go:if a/>\n    <p>x</p>\n    <go:for i/>\n        <li/>\n    <go:endfor/>\n<go:endif/>\n</div>";
        assert_eq!(fmt(src, &SP4), want);
    }

    #[test]
    fn branches_line_up_with_their_opener() {
        let src = "  <go:if a/>\n      x\n          <go:elif b/>\n  y\n<go:else/>\n   z\n        <go:endif/>";
        let want = "  <go:if a/>\n      x\n  <go:elif b/>\n      y\n  <go:else/>\n      z\n  <go:endif/>";
        assert_eq!(fmt(src, &SP4), want);
    }

    #[test]
    fn switch_cases_are_indented() {
        let src = "<go:switch v/>\n<go:case 1/>\na\n<go:default/>\nb\n<go:endswitch/>";
        let want = "<go:switch v/>\n    <go:case 1/>\n        a\n    <go:default/>\n        b\n<go:endswitch/>";
        assert_eq!(fmt(src, &SP4), want);
    }

    #[test]
    fn keeps_relative_html_indentation() {
        let src = "<go:for a/>\n<ul>\n  <li>x</li>\n</ul>\n<go:endfor/>";
        let want = "<go:for a/>\n    <ul>\n      <li>x</li>\n    </ul>\n<go:endfor/>";
        assert_eq!(fmt(src, &SP4), want);
    }

    #[test]
    fn leaves_pre_and_blank_lines_alone() {
        let src = "<go:if a/>\n<pre>\n   keep\n</pre>\n\n  \n<b/>\n<go:endif/>";
        let out = fmt(src, &SP4);
        assert_eq!(out, "<go:if a/>\n    <pre>\n   keep\n</pre>\n\n  \n    <b/>\n<go:endif/>");
    }

    #[test]
    fn tabs_option() {
        let src = "<go:for a/>\nx\n<go:endfor/>";
        assert_eq!(fmt(src, &Options { tab_size: 4, insert_spaces: false }), "<go:for a/>\n\tx\n<go:endfor/>");
        assert_eq!(fmt(src, &Options { tab_size: 2, insert_spaces: true }), "<go:for a/>\n  x\n<go:endfor/>");
    }

    #[test]
    fn broken_templates_are_untouched() {
        let doc = Doc::new(1, "<go:if a/>\nx\n".into());
        assert!(format(&doc, &SP4).is_empty());
    }

    #[test]
    fn idempotent() {
        let samples = [
            "<go:if a/>\n<go:for i/>\n<go:if b/>\nx\n  y\n<go:else/>\nz\n<go:endif/>\n<go:endfor/>\n<go:endif/>",
            "<div><go:if a/><p>\n  x\n</p><go:endif/></div>",
            "<go:switch v/>\n<go:case 1/>\n<go:if a/>\nq\n<go:endif/>\n<go:default/>\nw\n<go:endswitch/>",
            "<go:for a/>\n        <go:if b/>\n  x\n<go:endif/>\n<go:endfor/>",
            "<go\n  a := 1\n/>\n<go:if a/>\n<go\nb := 2\n/>\n<go:endif/>",
            include_str!("../../tests/demo/pages/home.ghp"),
        ];
        for s in samples {
            for opts in [SP4, Options { tab_size: 3, insert_spaces: false }] {
                let once = fmt(s, &opts);
                assert_eq!(fmt(&once, &opts), once, "not idempotent for {s:?}");
                // No content other than whitespace changed.
                let strip = |t: &str| t.lines().map(str::trim_start).collect::<Vec<_>>().join("\n");
                assert_eq!(strip(&once), strip(s));
            }
        }
    }

    #[test]
    fn range_restriction() {
        let src = "<go:for a/>\nx\n<go:endfor/>\n<go:for b/>\ny\n<go:endfor/>";
        let doc = Doc::new(1, src.into());
        let all = format(&doc, &SP4);
        assert_eq!(all.len(), 2);
        let range = json!({"start": {"line": 3, "character": 0}, "end": {"line": 5, "character": 0}});
        assert_eq!(restrict(&doc, all, &range).len(), 1);
    }
}
