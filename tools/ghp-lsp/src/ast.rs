//! Spanned syntax tree of a `.ghp` template.
//!
//! Every token and node carries an exact byte [`Span`] into the source. The tree is lossless: the
//! text runs and tags, taken in source order (see [`Tree::segments`]), cover the whole input with
//! no gaps and no overlaps, even when the input is broken.

/// Half-open byte range `start..end` into the source text. Always on UTF-8 char boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// `start <= offset < end`.
    pub const fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }

    /// `start <= offset <= end`: a cursor sitting right after the last byte still counts.
    pub const fn touches(&self, offset: usize) -> bool {
        self.start <= offset && offset <= self.end
    }

    /// The source text this span covers.
    pub fn slice<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

/// Which GHP tag a [`Tag`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    Import,
    Statement,
    Echo,
    If,
    Elif,
    Else,
    EndIf,
    Switch,
    Case,
    Default,
    EndSwitch,
    For,
    EndFor,
}

impl TagKind {
    /// Human name as written in templates, e.g. `<go:endif/>`.
    pub const fn name(self) -> &'static str {
        match self {
            TagKind::Import => "<go:import/>",
            TagKind::Statement => "<go/>",
            TagKind::Echo => "<go=/>",
            TagKind::If => "<go:if/>",
            TagKind::Elif => "<go:elif/>",
            TagKind::Else => "<go:else/>",
            TagKind::EndIf => "<go:endif/>",
            TagKind::Switch => "<go:switch/>",
            TagKind::Case => "<go:case/>",
            TagKind::Default => "<go:default/>",
            TagKind::EndSwitch => "<go:endswitch/>",
            TagKind::For => "<go:for/>",
            TagKind::EndFor => "<go:endfor/>",
        }
    }

    /// The Go-code region this kind of tag carries, if any (`<go:import>` is not Go code).
    pub const fn expr_kind(self) -> Option<ExprKind> {
        match self {
            TagKind::Statement => Some(ExprKind::Statement),
            TagKind::Echo => Some(ExprKind::Echo),
            TagKind::If => Some(ExprKind::If),
            TagKind::Elif => Some(ExprKind::Elif),
            TagKind::Switch => Some(ExprKind::Switch),
            TagKind::Case => Some(ExprKind::Case),
            TagKind::For => Some(ExprKind::For),
            _ => None,
        }
    }
}

/// One `<go...>` tag as lexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    pub kind: TagKind,
    /// The whole tag, from `<` to the byte after `>` (or to where the input/recovery stopped).
    pub span: Span,
    /// The fixed head, e.g. `<go:if` or `<go=`.
    pub head: Span,
    /// The payload (expression / code / import list) with surrounding whitespace and the closing
    /// `/` trimmed; `None` when it is empty.
    pub payload: Option<Span>,
    /// Properly terminated by `/>`.
    pub closed: bool,
    /// Everything between the head and the closing `/>` (or where an unterminated tag stops),
    /// whitespace included. It is where a cursor sits while the payload is still being typed:
    /// `<go= | />`.
    pub inner: Span,
}

impl Tag {
    pub fn payload_text<'a>(&self, src: &'a str) -> &'a str {
        self.payload.map_or("", |p| p.slice(src))
    }
}

/// A branch of an `if` (`elif` / `else`) or `switch` (`case` / `default`): the tag and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub tag: Tag,
    pub body: Vec<Node>,
}

/// One package in a `<go:import (...)/>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSpec {
    pub alias: Option<String>,
    /// Import path without quotes.
    pub path: String,
    /// The whole item (`alias "path"`).
    pub span: Span,
    /// The path without quotes.
    pub path_span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub tag: Tag,
    pub specs: Vec<ImportSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct If {
    pub open: Tag,
    pub then: Vec<Node>,
    /// `elif` / `else` branches in source order.
    pub branches: Vec<Branch>,
    /// `None` when the `<go:endif/>` is missing.
    pub end: Option<Tag>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    pub open: Tag,
    /// Anything between `<go:switch>` and the first case (only whitespace is valid).
    pub before: Vec<Node>,
    /// `case` / `default` branches in source order.
    pub cases: Vec<Branch>,
    pub end: Option<Tag>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct For {
    pub open: Tag,
    pub body: Vec<Node>,
    pub end: Option<Tag>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Plain markup copied to the output.
    Text(Span),
    Import(Import),
    /// `<go ... />`
    Statement(Tag),
    /// `<go= expr />`
    Echo(Tag),
    If(If),
    Switch(Switch),
    For(For),
    /// A tag that has no construct to belong to (e.g. a lone `<go:endif/>`), kept so the tree
    /// stays lossless. Also used for openers beyond the nesting limit.
    Stray(Tag),
}

impl Node {
    pub fn span(&self) -> Span {
        match self {
            Node::Text(s) => *s,
            Node::Import(i) => i.tag.span,
            Node::Statement(t) | Node::Echo(t) | Node::Stray(t) => t.span,
            Node::If(n) => n.span,
            Node::Switch(n) => n.span,
            Node::For(n) => n.span,
        }
    }

    /// Direct child nodes in source order.
    pub fn children(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        match self {
            Node::If(n) => {
                out.extend(n.then.iter());
                for b in &n.branches {
                    out.extend(b.body.iter());
                }
            }
            Node::Switch(n) => {
                out.extend(n.before.iter());
                for b in &n.cases {
                    out.extend(b.body.iter());
                }
            }
            Node::For(n) => out.extend(n.body.iter()),
            _ => {}
        }
        out
    }
}

/// What kind of Go code a region holds; tells the consumer how to wrap it for gopls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExprKind {
    /// Statements of a `<go ... />` block.
    Statement,
    /// Expression of `<go= ... />`.
    Echo,
    /// Condition of `<go:if>`.
    If,
    /// Condition of `<go:elif>`.
    Elif,
    /// Tag expression of `<go:switch>` (may be absent: `switch {`).
    Switch,
    /// Value list of `<go:case>`.
    Case,
    /// Clause of `<go:for>` (e.g. `i := 0; i < n; i++`, `_, v := range xs`).
    For,
}

/// A Go-code region of the template: the hand-off point for gopls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExprRegion {
    pub kind: ExprKind,
    /// Exact span of the Go text (trimmed of surrounding whitespace and the closing `/`).
    pub span: Span,
    /// The tag it belongs to.
    pub tag: Span,
    /// The tag was properly terminated.
    pub closed: bool,
}

/// A lossless piece of the source: a text run or a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(Span),
    Tag(&'a Tag),
}

impl Segment<'_> {
    pub fn span(&self) -> Span {
        match self {
            Segment::Text(s) => *s,
            Segment::Tag(t) => t.span,
        }
    }
}

/// The parsed template.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// Length of the source in bytes.
    pub len: usize,
}

impl Tree {
    /// Every node, depth first in source order (parents before their children).
    pub fn iter(&self) -> NodeIter<'_> {
        let mut stack: Vec<&Node> = self.nodes.iter().collect();
        stack.reverse();
        NodeIter { stack }
    }

    /// All tags in source order: openers, branch tags, closers and stray ones.
    pub fn tags(&self) -> Vec<&Tag> {
        let mut out = Vec::new();
        collect_tags(&self.nodes, &mut |t, _| out.push(t));
        out
    }

    /// Text runs and tags in source order. Always contiguous and covering `0..len`.
    pub fn segments(&self) -> Vec<Segment<'_>> {
        let mut out = Vec::new();
        collect_segments(&self.nodes, &mut out);
        out
    }

    /// The Go-code regions in source order, for handing to gopls. Stray tags are skipped.
    pub fn expr_regions(&self) -> Vec<ExprRegion> {
        let mut out = Vec::new();
        collect_tags(&self.nodes, &mut |t, stray| {
            if stray {
                return;
            }
            if let (Some(kind), Some(span)) = (t.kind.expr_kind(), t.payload) {
                out.push(ExprRegion { kind, span, tag: t.span, closed: t.closed });
            }
        });
        out.sort_by_key(|r| r.span.start);
        out
    }

    /// The Go region containing `offset` (a cursor right after its last byte counts).
    ///
    /// A tag that takes Go text but has none yet (`<go= />`, `<go />`, `<go:if/>`) counts too while
    /// the cursor is between its head and its `/>`: that is where the text is about to be typed.
    pub fn expr_region_at(&self, offset: usize) -> Option<ExprRegion> {
        if let Some(r) = self.expr_regions().into_iter().find(|r| r.span.touches(offset)) {
            return Some(r);
        }
        let mut hit = None;
        collect_tags(&self.nodes, &mut |t, stray| {
            if hit.is_none()
                && !stray
                && t.payload.is_none()
                && t.inner.touches(offset)
                && let Some(kind) = t.kind.expr_kind()
            {
                hit = Some(ExprRegion { kind, span: t.inner, tag: t.span, closed: t.closed });
            }
        });
        hit
    }

    /// Deepest node containing `offset`.
    pub fn node_at(&self, offset: usize) -> Option<&Node> {
        self.iter().filter(|n| n.span().contains(offset)).last()
    }

    /// Import specs of every `<go:import>` in the template, in source order.
    pub fn imports(&self) -> Vec<&ImportSpec> {
        self.iter()
            .filter_map(|n| if let Node::Import(i) = n { Some(i) } else { None })
            .flat_map(|i| i.specs.iter())
            .collect()
    }
}

/// Depth-first iterator over a tree's nodes. See [`Tree::iter`].
pub struct NodeIter<'a> {
    stack: Vec<&'a Node>,
}

impl<'a> Iterator for NodeIter<'a> {
    type Item = &'a Node;

    fn next(&mut self) -> Option<&'a Node> {
        let node = self.stack.pop()?;
        let mut kids = node.children();
        kids.reverse();
        self.stack.extend(kids);
        Some(node)
    }
}

fn collect_tags<'a>(nodes: &'a [Node], f: &mut dyn FnMut(&'a Tag, bool)) {
    for n in nodes {
        match n {
            Node::Text(_) => {}
            Node::Import(i) => f(&i.tag, false),
            Node::Statement(t) | Node::Echo(t) => f(t, false),
            Node::Stray(t) => f(t, true),
            Node::If(n) => {
                f(&n.open, false);
                collect_tags(&n.then, f);
                for b in &n.branches {
                    f(&b.tag, false);
                    collect_tags(&b.body, f);
                }
                if let Some(e) = &n.end {
                    f(e, false);
                }
            }
            Node::Switch(n) => {
                f(&n.open, false);
                collect_tags(&n.before, f);
                for b in &n.cases {
                    f(&b.tag, false);
                    collect_tags(&b.body, f);
                }
                if let Some(e) = &n.end {
                    f(e, false);
                }
            }
            Node::For(n) => {
                f(&n.open, false);
                collect_tags(&n.body, f);
                if let Some(e) = &n.end {
                    f(e, false);
                }
            }
        }
    }
}

fn collect_segments<'a>(nodes: &'a [Node], out: &mut Vec<Segment<'a>>) {
    for n in nodes {
        match n {
            Node::Text(s) => out.push(Segment::Text(*s)),
            Node::Import(i) => out.push(Segment::Tag(&i.tag)),
            Node::Statement(t) | Node::Echo(t) | Node::Stray(t) => out.push(Segment::Tag(t)),
            Node::If(n) => {
                out.push(Segment::Tag(&n.open));
                collect_segments(&n.then, out);
                for b in &n.branches {
                    out.push(Segment::Tag(&b.tag));
                    collect_segments(&b.body, out);
                }
                if let Some(e) = &n.end {
                    out.push(Segment::Tag(e));
                }
            }
            Node::Switch(n) => {
                out.push(Segment::Tag(&n.open));
                collect_segments(&n.before, out);
                for b in &n.cases {
                    out.push(Segment::Tag(&b.tag));
                    collect_segments(&b.body, out);
                }
                if let Some(e) = &n.end {
                    out.push(Segment::Tag(e));
                }
            }
            Node::For(n) => {
                out.push(Segment::Tag(&n.open));
                collect_segments(&n.body, out);
                if let Some(e) = &n.end {
                    out.push(Segment::Tag(e));
                }
            }
        }
    }
}
