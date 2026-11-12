//! Request routing for a `.ghp` language server.
//!
//! A page is HTML with Go islands (`<go= ... />`, `<go:if ... />`, ...). The server owns the
//! HTML side; everything that is Go goes through a [`Router`]:
//!
//! 1. the server keeps the router in sync with its documents ([`Router::open`],
//!    [`Router::change`], [`Router::replace`], [`Router::close`]);
//! 2. for completion / hover / definition / references / signatureHelp it calls
//!    [`Router::request`] with the cursor position. `Ok(None)` means "not mine": the position is
//!    in plain markup (or gopls knows nothing there) and the server answers on its own;
//! 3. for diagnostics it calls [`Router::diagnostics`] after every change and again whenever the
//!    router's [`Event::Diagnostics`] fires (gopls is asynchronous).
//!
//! Positions and ranges are LSP's: zero-based line, UTF-16 column. Results are LSP JSON in `.ghp`
//! coordinates, ready to be sent back verbatim.
//!
//! # Without gopls
//!
//! When gopls is not installed, disabled ([`Config::disabled`]), or has crashed too often, the
//! router keeps answering for *local* names from [`crate::scope`]: variables declared in
//! `<go ... />` blocks and loop headers, imports (with an unused-import warning) and the
//! handler's `w` / `r`. Member completion (`user.`), signature help and types need gopls.
//!
//! # Integration
//!
//! ```no_run
//! use ghp_lsp::gopls::CancelToken;
//! use ghp_lsp::line_index::Position;
//! use ghp_lsp::router::{ContentChange, RequestKind, Router};
//! use std::path::Path;
//!
//! // One router per workspace. It starts one gopls per Go module below the root, on demand,
//! // restarts it after a crash and gives up (with an `Event::Status`) if it keeps crashing.
//! let router = Router::new("/work/site");
//! let events = router.take_events().expect("taken once");
//!
//! // textDocument/didOpen
//! let page = Path::new("/work/site/pages/home.ghp");
//! router.open(page, "<go user := \"Ada\" /><p><go= user /></p>");
//!
//! // textDocument/didChange (incremental ranges, or `range: None` for the whole text)
//! let edit = ContentChange { range: None, text: "<go user := \"Bob\" /><p><go= user /></p>".into() };
//! router.change(page, &[edit]);
//!
//! // textDocument/hover (run it on a worker thread; `cancel` maps to $/cancelRequest)
//! let cancel = CancelToken::default();
//! let at = Position { line: 0, character: 33 };
//! match router.request(page, at, RequestKind::Hover, &cancel) {
//!     Ok(Some(hover)) => drop(hover), // send as the response result
//!     Ok(None) => {}                  // not Go: answer from the HTML side, or null
//!     Err(e) => drop(e),              // Cancelled / Timeout: RequestCancelled or null
//! }
//!
//! // textDocument/publishDiagnostics, after each change and on `Event::Diagnostics`.
//! for d in router.diagnostics(page) {
//!     let _lsp_diagnostic = d.to_lsp();
//! }
//! for event in events.try_iter() {
//!     drop(event); // Status -> window/showMessage, Diagnostics(path) -> republish, ...
//! }
//!
//! // textDocument/didClose
//! router.close(page);
//! ```

use crate::ast::{Span, TagKind, Tree};
use crate::diagnostics::{Diagnostic as GhpDiagnostic, Severity};
use crate::gopls::adapter::{live_tags, regions};
use crate::gopls::proxy::{LockExt, path_to_uri};
use crate::gopls::{Bridge, CancelToken, Config, Event, Position as GoPosition, ProxyError};
use crate::line_index::{LineIndex, Position, Range};
use crate::parser::{Parse, parse};
use crate::scope::{Scope, Symbol, SymbolKind};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------------------------
// Pure routing
// ---------------------------------------------------------------------------------------------

/// What the editor asks for at a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Completion,
    Hover,
    Definition,
    References,
    SignatureHelp,
}

/// A Go region that contains the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GoSite {
    /// The tag the region belongs to.
    pub tag: TagKind,
    /// Exact span of the Go text.
    pub span: Span,
}

/// The Go region of `tree` containing `offset` (a cursor right after the last byte counts), or
/// `None` when the offset is in markup or in a tag head (`<go:if`).
///
/// A tag that takes Go text but has none yet (`<go= />`, `<go />`) is a region too, spanning the
/// gap between its head and its `/>`: that is where the text is about to be typed, so completion
/// works there.
pub fn go_site_at(tree: &Tree, offset: usize) -> Option<GoSite> {
    live_tags(tree).into_iter().find_map(|t| {
        let p = match t.payload {
            Some(p) => p,
            None if t.kind.expr_kind().is_some() => t.inner,
            None => return None,
        };
        p.touches(offset).then_some(GoSite { tag: t.kind, span: p })
    })
}

/// [`go_site_at`] for an LSP position.
pub fn go_site_at_position(src: &str, parse: &Parse, pos: Position) -> Option<GoSite> {
    let offset = LineIndex::new(src).offset(pos);
    go_site_at(&parse.tree, offset)
}

/// One `textDocument/didChange` content change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentChange {
    /// `None` replaces the whole document.
    pub range: Option<Range>,
    pub text: String,
}

/// Apply content changes in order, each against the text produced by the previous one (the LSP
/// contract). Ranges outside the text clamp to its end.
pub fn apply_changes(text: &str, changes: &[ContentChange]) -> String {
    let mut cur = text.to_string();
    for c in changes {
        match c.range {
            None => cur = c.text.clone(),
            Some(r) => {
                let span = LineIndex::new(&cur).span(r);
                cur.replace_range(span.start..span.end, &c.text);
            }
        }
    }
    cur
}

/// A diagnostic in `.ghp` coordinates from either the GHP parser/scope analysis (`source`
/// `"ghp"`) or gopls (`"gopls"`).
#[derive(Clone, Debug, PartialEq)]
pub struct RouterDiagnostic {
    pub range: Range,
    /// LSP hints and informations are reported as warnings.
    pub severity: Severity,
    pub code: Option<String>,
    pub source: &'static str,
    pub message: String,
}

impl RouterDiagnostic {
    /// The LSP `Diagnostic` object.
    pub fn to_lsp(&self) -> Value {
        let severity = if self.severity == Severity::Error { 1 } else { 2 };
        let mut v = json!({
            "range": range_json(self.range),
            "severity": severity,
            "source": self.source,
            "message": self.message,
        });
        if let (Some(c), Some(o)) = (&self.code, v.as_object_mut()) {
            o.insert("code".into(), json!(c));
        }
        v
    }
}

fn from_ghp(d: &GhpDiagnostic, index: &LineIndex) -> RouterDiagnostic {
    RouterDiagnostic {
        range: d.range(index),
        severity: d.severity,
        code: Some(d.code.to_string()),
        source: "ghp",
        message: d.message.clone(),
    }
}

fn from_gopls(d: &crate::gopls::Diagnostic) -> RouterDiagnostic {
    let pos = |p: GoPosition| Position { line: p.line, character: p.character };
    RouterDiagnostic {
        range: Range { start: pos(d.range.start), end: pos(d.range.end) },
        severity: if d.severity.is_none_or(|s| s == 1) { Severity::Error } else { Severity::Warning },
        code: d.code.as_ref().map(|c| c.as_str().map_or_else(|| c.to_string(), str::to_string)),
        source: "gopls",
        message: d.message.clone(),
    }
}

fn overlaps(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

/// Merge the parser's diagnostics with Go diagnostics from gopls (or the scope fallback).
///
/// * parser diagnostics come first-class (`source: "ghp"`, their own severity);
/// * a Go diagnostic overlapping a parser *error* is dropped: it is almost always fallout of the
///   same broken tag (an unterminated `<go:if (a > b` makes gopls complain about the text);
/// * exact duplicates (same range and message) are reported once;
/// * the result is sorted by position.
pub fn merge_diagnostics(
    parser: &[GhpDiagnostic],
    index: &LineIndex,
    go: Vec<RouterDiagnostic>,
) -> Vec<RouterDiagnostic> {
    let mut out: Vec<RouterDiagnostic> = parser.iter().map(|d| from_ghp(d, index)).collect();
    let errors: Vec<Range> = out.iter().filter(|d| d.severity == Severity::Error).map(|d| d.range).collect();
    let mut seen: HashSet<(Range, String)> = out.iter().map(|d| (d.range, d.message.clone())).collect();
    for d in go {
        if errors.iter().any(|e| overlaps(*e, d.range)) {
            continue;
        }
        if seen.insert((d.range, d.message.clone())) {
            out.push(d);
        }
    }
    out.sort_by_key(|d| (d.range.start, d.range.end));
    out
}

fn range_json(r: Range) -> Value {
    json!({
        "start": {"line": r.start.line, "character": r.start.character},
        "end": {"line": r.end.line, "character": r.end.character},
    })
}

// ---------------------------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------------------------

struct DocState {
    text: String,
    parse: Parse,
    index: LineIndex,
    scope: Scope,
}

impl DocState {
    fn new(text: String) -> DocState {
        let parse = parse(&text);
        let index = LineIndex::new(&text);
        let scope = Scope::analyze(&text, &parse.tree);
        DocState { text, parse, index, scope }
    }
}

/// Routes language-server requests for `.ghp` pages: Go positions go to gopls (one process per
/// Go module under the root, supervised by the bridge), everything else is left to the caller.
/// All methods take `&self`; the router can be shared between threads.
pub struct Router {
    root: PathBuf,
    bridge: Bridge,
    events: Mutex<Option<Receiver<Event>>>,
    docs: Mutex<HashMap<PathBuf, Arc<DocState>>>,
}

impl Router {
    /// A router for the workspace `root`. Relative page paths are resolved against it. gopls
    /// processes are started lazily, one per Go module (the nearest `go.mod` above a page),
    /// restarted after a crash with their open pages replayed, and abandoned with an
    /// [`Event::Status`] after repeated crashes.
    pub fn new(root: impl Into<PathBuf>) -> Router {
        Router::with_config(root, Config::default())
    }

    pub fn with_config(root: impl Into<PathBuf>, cfg: Config) -> Router {
        let (bridge, rx) = Bridge::new(cfg);
        Router { root: root.into(), bridge, events: Mutex::new(Some(rx)), docs: Mutex::new(HashMap::new()) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The channel of [`Event`]s (status messages, "diagnostics changed", crash / restart). It can
    /// be taken once; later calls return `None`.
    pub fn take_events(&self) -> Option<Receiver<Event>> {
        self.events.lock_ok().take()
    }

    /// The underlying bridge, for hosts that need lower-level access.
    pub fn bridge(&self) -> &Bridge {
        &self.bridge
    }

    /// Whether gopls was found at all.
    pub fn gopls_available(&self) -> bool {
        self.bridge.gopls_available()
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() { path.to_path_buf() } else { self.root.join(path) }
    }

    fn doc(&self, resolved: &Path) -> Option<Arc<DocState>> {
        self.docs.lock_ok().get(resolved).cloned()
    }

    fn store(&self, resolved: PathBuf, text: String) {
        let doc = Arc::new(DocState::new(text));
        self.bridge.update(&resolved, &doc.text, &regions(&doc.parse.tree));
        self.docs.lock_ok().insert(resolved, doc);
    }

    /// `textDocument/didOpen`.
    pub fn open(&self, path: &Path, text: impl Into<String>) {
        self.store(self.resolve(path), text.into());
    }

    /// Replace the whole text of a page (full-sync `didChange`, or reload from disk).
    pub fn replace(&self, path: &Path, text: impl Into<String>) {
        self.store(self.resolve(path), text.into());
    }

    /// `textDocument/didChange` with incremental or full changes. Ignored for unopened pages.
    pub fn change(&self, path: &Path, changes: &[ContentChange]) {
        let path = self.resolve(path);
        if let Some(doc) = self.doc(&path) {
            let text = apply_changes(&doc.text, changes);
            self.store(path, text);
        }
    }

    /// `textDocument/didClose`.
    pub fn close(&self, path: &Path) {
        let path = self.resolve(path);
        self.docs.lock_ok().remove(&path);
        self.bridge.close(&path);
    }

    /// The current text of an open page.
    pub fn text(&self, path: &Path) -> Option<String> {
        self.doc(&self.resolve(path)).map(|d| d.text.clone())
    }

    /// Run `f` on the current parse and line index of an open page.
    pub fn with_parse<R>(&self, path: &Path, f: impl FnOnce(&Parse, &LineIndex) -> R) -> Option<R> {
        self.doc(&self.resolve(path)).map(|d| f(&d.parse, &d.index))
    }

    /// The Go region under the cursor, if any.
    pub fn site_at(&self, path: &Path, pos: Position) -> Option<GoSite> {
        let doc = self.doc(&self.resolve(path))?;
        go_site_at(&doc.parse.tree, doc.index.offset(pos))
    }

    /// Answer `kind` at `pos`.
    ///
    /// * `Ok(None)`: the position is not in Go text, the page is not open, or nothing is known
    ///   there. The caller answers from the HTML side (or with null).
    /// * `Ok(Some(json))`: the LSP result, in `.ghp` coordinates.
    /// * `Err(Cancelled)` / `Err(Timeout)`: the request was abandoned; reply with null or
    ///   `RequestCancelled`.
    ///
    /// When gopls is unavailable (or just crashed) completion, hover, definition and references
    /// of local names are answered by [`crate::scope`].
    pub fn request(
        &self,
        path: &Path,
        pos: Position,
        kind: RequestKind,
        cancel: &CancelToken,
    ) -> Result<Option<Value>, ProxyError> {
        let path = self.resolve(path);
        let Some(doc) = self.doc(&path) else { return Ok(None) };
        let offset = doc.index.offset(pos);
        let Some(site) = go_site_at(&doc.parse.tree, offset) else { return Ok(None) };

        let gpos = GoPosition { line: pos.line, character: pos.character };
        let routed = match kind {
            RequestKind::Completion => self.bridge.completion(&path, gpos, cancel),
            RequestKind::Hover => self.bridge.hover(&path, gpos, cancel),
            RequestKind::Definition => self.bridge.definition(&path, gpos, cancel),
            RequestKind::References => self.bridge.references(&path, gpos, cancel),
            RequestKind::SignatureHelp => self.bridge.signature_help(&path, gpos, cancel),
        };
        match routed {
            Ok(Some(v)) if v.is_null() => Ok(None),
            Ok(Some(v)) => Ok(Some(v)),
            // A gopls that is gone (or went away while we wrote to it) is a missing gopls.
            Ok(None) | Err(ProxyError::Crashed | ProxyError::Io(_)) => Ok(fallback(&doc, &path, kind, offset, site)),
            Err(e) => Err(e),
        }
    }

    /// Diagnostics of the Go side of a page: gopls' when it is running, otherwise the scope
    /// analysis' (unused imports).
    pub fn go_diagnostics(&self, path: &Path) -> Vec<RouterDiagnostic> {
        let path = self.resolve(path);
        let Some(doc) = self.doc(&path) else { return Vec::new() };
        if self.bridge.is_active(&path) {
            self.bridge.diagnostics(&path).iter().map(from_gopls).collect()
        } else {
            doc.scope.unused_imports(&doc.text).iter().map(|d| from_ghp(d, &doc.index)).collect()
        }
    }

    /// Everything to publish for a page: parser diagnostics merged with [`Router::go_diagnostics`]
    /// (see [`merge_diagnostics`]).
    pub fn diagnostics(&self, path: &Path) -> Vec<RouterDiagnostic> {
        let go = self.go_diagnostics(path);
        match self.doc(&self.resolve(path)) {
            Some(doc) => merge_diagnostics(&doc.parse.diagnostics, &doc.index, go),
            None => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Fallback answers from scope analysis
// ---------------------------------------------------------------------------------------------

const KIND_VARIABLE: u32 = 6;
const KIND_MODULE: u32 = 9;
const KIND_CONSTANT: u32 = 21;

fn completion_kind(k: SymbolKind) -> u32 {
    match k {
        SymbolKind::Variable => KIND_VARIABLE,
        SymbolKind::Constant => KIND_CONSTANT,
        SymbolKind::Import => KIND_MODULE,
    }
}

fn location(path: &Path, doc: &DocState, span: Span) -> Value {
    json!({"uri": path_to_uri(path), "range": range_json(doc.index.range(span))})
}

/// The symbol under an identifier token: its declaration, or what it resolves to at that point.
fn symbol_at(doc: &DocState, offset: usize) -> Option<(Span, &Symbol)> {
    let (tok, member) = doc.scope.ident_at(&doc.text, offset)?;
    if member {
        return None;
    }
    let declared = doc.scope.symbols.iter().find(|s| s.decl == tok.span);
    let sym = declared.or_else(|| doc.scope.resolve(tok.span.slice(&doc.text), tok.span.start))?;
    Some((tok.span, sym))
}

fn fallback(doc: &DocState, path: &Path, kind: RequestKind, offset: usize, site: GoSite) -> Option<Value> {
    if site.tag == TagKind::Import {
        return None;
    }
    match kind {
        RequestKind::Completion => {
            let (prefix, member) = doc.scope.completion_prefix(&doc.text, offset, site.span);
            if member {
                return None;
            }
            let typed = prefix.slice(&doc.text);
            let items: Vec<Value> = doc
                .scope
                .visible_at(offset)
                .into_iter()
                .filter(|s| s.name.starts_with(typed))
                .map(|s| json!({"label": s.name, "kind": completion_kind(s.kind), "detail": s.detail}))
                .collect();
            Some(json!({"isIncomplete": false, "items": items}))
        }
        RequestKind::Hover => {
            let (span, sym) = symbol_at(doc, offset)?;
            Some(json!({
                "contents": {"kind": "markdown", "value": format!("```go\n{}\n```", sym.detail)},
                "range": range_json(doc.index.range(span)),
            }))
        }
        RequestKind::Definition => {
            let (_, sym) = symbol_at(doc, offset)?;
            (!sym.decl.is_empty()).then(|| json!([location(path, doc, sym.decl)]))
        }
        RequestKind::References => {
            let (_, sym) = symbol_at(doc, offset)?;
            let refs: Vec<Value> =
                doc.scope.references(&doc.text, sym).into_iter().map(|s| location(path, doc, s)).collect();
            (!refs.is_empty()).then_some(Value::Array(refs))
        }
        RequestKind::SignatureHelp => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    fn offline() -> Router {
        Router::with_config("/ghp-test-root", Config { disabled: true, ..Config::default() })
    }

    fn page() -> PathBuf {
        PathBuf::from("/ghp-test-root/pages/home.ghp")
    }

    #[test]
    fn site_detection() {
        let src = "<p>hi</p><go= a.b /><go:if ok/>x<go:else/>y<go:endif/>\n<go\n  z := 1\n/>";
        let parse = parse(src);
        let at = |needle: &str, delta: usize| src.find(needle).unwrap() + delta;
        // Inside and at both edges of the payload.
        assert_eq!(go_site_at(&parse.tree, at("a.b", 1)).map(|s| s.span.slice(src)), Some("a.b"));
        assert!(go_site_at(&parse.tree, at("a.b", 0)).is_some());
        assert!(go_site_at(&parse.tree, at("a.b", 3)).is_some());
        // Markup, tag heads and the closing slash are not Go.
        assert!(go_site_at(&parse.tree, at("hi", 1)).is_none());
        assert!(go_site_at(&parse.tree, at("<go=", 2)).is_none());
        assert!(go_site_at(&parse.tree, at("/><go:if", 1)).is_none());
        // Structural tags carry no text.
        assert!(go_site_at(&parse.tree, at("<go:else", 4)).is_none());
        // Multi-line statement.
        let z = go_site_at(&parse.tree, at("z := 1", 2)).unwrap();
        assert_eq!((z.tag, z.span.slice(src)), (TagKind::Statement, "z := 1"));
        // By LSP position, with a multi-byte char before the tag.
        let src = "é😀<go= x />";
        let parse = crate::parser::parse(src);
        assert!(go_site_at_position(src, &parse, p(0, 8)).is_some());
        assert!(go_site_at_position(src, &parse, p(0, 2)).is_none());
    }

    #[test]
    fn empty_tags_are_sites_between_head_and_closer() {
        let src = "<go u := 1 /><a href=\"<go= />\"><go /><go:if/><go=/>";
        let parse = parse(src);
        let at = |needle: &str, delta: usize| src.find(needle).unwrap() + delta;
        for (needle, delta, kind) in [
            ("<go= />", 4, TagKind::Echo),
            ("<go= />", 5, TagKind::Echo),
            ("<go />", 3, TagKind::Statement),
            ("<go:if/>", 6, TagKind::If),
            ("<go=/>", 4, TagKind::Echo),
        ] {
            let site = go_site_at(&parse.tree, at(needle, delta)).unwrap_or_else(|| panic!("{needle} +{delta}"));
            assert_eq!(site.tag, kind, "{needle} +{delta}");
        }
        // The head and the closer stay out.
        assert!(go_site_at(&parse.tree, at("<go= />", 2)).is_none());
        assert!(go_site_at(&parse.tree, at("<go= />", 6)).is_none());
        // The parser-level lookup the server uses agrees.
        assert!(parse.tree.expr_region_at(at("<go= />", 5)).is_some());
        assert!(parse.tree.expr_region_at(at("<go= />", 2)).is_none());
    }

    #[test]
    fn completion_in_an_empty_tag_offers_the_names_in_scope() {
        let r = offline();
        let src = "<go count := 1 />\n<p><go= /></p>\n<a href=\"<go= />\">x</a>";
        r.open(&page(), src);
        for (needle, delta) in [("<go= /></p>", 5), ("<go= />\">", 5)] {
            let at = LineIndex::new(src).position(src.find(needle).unwrap() + delta);
            let got = r.request(&page(), at, RequestKind::Completion, &CancelToken::default()).unwrap().unwrap();
            let items = got["items"].as_array().unwrap();
            let labels: Vec<&str> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
            assert!(labels.contains(&"count"), "{needle}: {labels:?}");
        }
    }

    #[test]
    fn go_in_an_attribute_value_is_a_site_for_hover_and_completion() {
        let r = offline();
        let src = "<go url := \"/x\" />\n<a href=\"<go= url />\" title=\"<go= ur />\">x</a>";
        r.open(&page(), src);
        let at = |needle: &str, delta: usize| LineIndex::new(src).position(src.find(needle).unwrap() + delta);
        let ask = |pos, kind| r.request(&page(), pos, kind, &CancelToken::default()).unwrap();
        let hover = ask(at("url />", 1), RequestKind::Hover).unwrap();
        assert!(hover.to_string().contains("url := "), "{hover}");
        let got = ask(at("ur />", 2), RequestKind::Completion).unwrap();
        let labels: Vec<&str> = got["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert_eq!(labels, vec!["url"]);
        // The attribute text around the tag is plain markup.
        assert!(ask(at("title=", 3), RequestKind::Hover).is_none());
    }

    #[test]
    fn stray_tags_are_not_sites() {
        let src = "<go:endif/><go= x />";
        let parse = parse(src);
        assert_eq!(go_site_at(&parse.tree, 16).map(|s| s.tag), Some(TagKind::Echo));
    }

    #[test]
    fn content_changes_apply_in_order() {
        let r = |a: (u32, u32), b: (u32, u32)| Some(Range { start: p(a.0, a.1), end: p(b.0, b.1) });
        let changes = [
            ContentChange { range: r((0, 5), (0, 6)), text: "foo".into() },
            ContentChange { range: r((0, 8), (0, 8)), text: "\n  ".into() },
            // Straddles two lines and an emoji.
            ContentChange { range: r((0, 0), (1, 2)), text: "😀".into() },
        ];
        assert_eq!(apply_changes("<go= x />", &changes[..1]), "<go= foo />");
        assert_eq!(apply_changes("<go= x />", &changes[..2]), "<go= foo\n   />");
        assert_eq!(apply_changes("<go= x />", &changes), "😀 />");
        let full = [ContentChange { range: None, text: "new".into() }];
        assert_eq!(apply_changes("old", &full), "new");
        // Out of range clamps instead of panicking.
        let far = [ContentChange { range: r((9, 0), (9, 9)), text: "!".into() }];
        assert_eq!(apply_changes("ab", &far), "ab!");
    }

    fn rd(severity: Severity, a: (u32, u32), b: (u32, u32), source: &'static str, msg: &str) -> RouterDiagnostic {
        RouterDiagnostic {
            range: Range { start: p(a.0, a.1), end: p(b.0, b.1) },
            severity,
            code: None,
            source,
            message: msg.into(),
        }
    }

    #[test]
    fn merge_drops_fallout_of_parser_errors_and_duplicates() {
        let src = "<go:if (a > b\n<p>x</p>\n<go= y.z />";
        let parse = parse(src);
        let index = LineIndex::new(src);
        assert!(parse.diagnostics.iter().any(|d| d.severity == Severity::Error));
        let go = vec![
            // Inside the unterminated `<go:if`: dropped.
            rd(Severity::Error, (0, 8), (0, 9), "gopls", "syntax error"),
            // On the healthy echo (line 2): kept, once.
            rd(Severity::Error, (2, 8), (2, 9), "gopls", "undefined: y"),
            rd(Severity::Error, (2, 8), (2, 9), "gopls", "undefined: y"),
            rd(Severity::Warning, (2, 10), (2, 11), "gopls", "other"),
        ];
        let merged = merge_diagnostics(&parse.diagnostics, &index, go);
        let go_msgs: Vec<_> = merged.iter().filter(|d| d.source == "gopls").map(|d| d.message.as_str()).collect();
        assert_eq!(go_msgs, vec!["undefined: y", "other"]);
        assert!(merged.iter().any(|d| d.source == "ghp" && d.code.as_deref() == Some("unterminated-tag")));
        let starts: Vec<_> = merged.iter().map(|d| d.range.start).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "sorted: {starts:?}");
    }

    #[test]
    fn parser_warnings_do_not_hide_go_diagnostics() {
        let src = "<go:nope/> <go= q />";
        let parse = parse(src);
        let index = LineIndex::new(src);
        let go = vec![rd(Severity::Error, (0, 0), (0, 9), "gopls", "overlaps the warning")];
        let merged = merge_diagnostics(&parse.diagnostics, &index, go);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn diagnostics_serialise_to_lsp() {
        let d = RouterDiagnostic { code: Some("c".into()), ..rd(Severity::Warning, (1, 2), (1, 4), "ghp", "m") };
        let v = d.to_lsp();
        assert_eq!(v["severity"], 2);
        assert_eq!(v["source"], "ghp");
        assert_eq!(v["code"], "c");
        assert_eq!(v["range"]["end"], json!({"line": 1, "character": 4}));
        assert_eq!(rd(Severity::Error, (0, 0), (0, 0), "gopls", "m").to_lsp()["severity"], 1);
    }

    #[test]
    fn gopls_diagnostics_are_translated() {
        let g = crate::gopls::Diagnostic {
            range: crate::gopls::Range {
                start: GoPosition { line: 1, character: 2 },
                end: GoPosition { line: 1, character: 5 },
            },
            severity: Some(2),
            code: Some(json!("UndeclaredName")),
            source: Some("compiler".into()),
            message: "m".into(),
        };
        let d = from_gopls(&g);
        assert_eq!((d.severity, d.source, d.code.as_deref()), (Severity::Warning, "gopls", Some("UndeclaredName")));
        assert_eq!(d.range.end, p(1, 5));
        let numeric = crate::gopls::Diagnostic { code: Some(json!(42)), severity: None, ..g };
        let d = from_gopls(&numeric);
        assert_eq!((d.severity, d.code.as_deref()), (Severity::Error, Some("42")));
    }

    #[test]
    fn markup_is_never_routed() {
        let r = offline();
        r.open(&page(), "<p>hi</p><go= 1 />");
        let c = CancelToken::default();
        for kind in [RequestKind::Completion, RequestKind::Hover, RequestKind::Definition, RequestKind::References] {
            assert!(r.request(&page(), p(0, 4), kind, &c).unwrap().is_none(), "{kind:?}");
        }
        // Unknown page.
        assert!(r.request(Path::new("/ghp-test-root/none.ghp"), p(0, 0), RequestKind::Hover, &c).unwrap().is_none());
    }

    const SRC: &str = "<go:import (\"strings\"\n  \"os\")/>\n<go items := []string{\"a\"} />\n\
        <go:for i, it := range items/><go= strings.ToUpper(it) /><go= i /><go:endfor/>\n<go= it />";

    fn offset_to_pos(src: &str, off: usize) -> Position {
        LineIndex::new(src).position(off)
    }

    #[test]
    fn fallback_completion_hover_definition_references() {
        let r = offline();
        r.open(&page(), SRC);
        let c = CancelToken::default();
        let ask = |kind, off: usize| r.request(&page(), offset_to_pos(SRC, off), kind, &c).unwrap();

        // Completion in the loop body: loop variables, outer names, imports; filtered by prefix.
        let in_body = SRC.find("<go= i ").unwrap() + 5;
        let after_i = ask(RequestKind::Completion, in_body + 1).unwrap();
        let labels: Vec<_> =
            after_i["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert_eq!(labels, vec!["i", "io", "it", "items"]);
        let kinds: Vec<_> = after_i["items"].as_array().unwrap().iter().map(|i| i["kind"].as_u64().unwrap()).collect();
        assert_eq!(kinds, vec![6, 9, 6, 6]);
        let strs = ask(RequestKind::Completion, SRC.find("strings.To").unwrap() + 3).unwrap();
        let labels: Vec<_> = strs["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert_eq!(labels, vec!["strings"]);
        assert_eq!(strs["items"][0]["kind"], 9);
        // Member completion needs gopls.
        assert!(ask(RequestKind::Completion, SRC.find("ToUpper").unwrap()).is_none());
        // Outside the loop `it` is gone.
        let last = SRC.rfind("<go= it").unwrap() + 7;
        assert!(
            ask(RequestKind::Completion, last).unwrap()["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["label"] != "it" && i["label"] != "i")
        );

        // Hover.
        let h = ask(RequestKind::Hover, SRC.find("(it)").unwrap() + 2).unwrap();
        assert_eq!(h["contents"]["value"], "```go\ni, it := range items\n```");
        assert_eq!(h["range"]["start"]["line"], 3);
        let h = ask(RequestKind::Hover, SRC.find("strings.To").unwrap() + 2).unwrap();
        assert!(h["contents"]["value"].as_str().unwrap().contains("import \"strings\""));
        assert!(ask(RequestKind::Hover, SRC.find("ToUpper").unwrap() + 2).is_none());
        assert!(ask(RequestKind::Hover, last).is_none(), "`it` is out of scope after the loop");

        // Definition: the loop header.
        let d = ask(RequestKind::Definition, SRC.find("(it)").unwrap() + 2).unwrap();
        assert_eq!(d[0]["uri"], "file:///ghp-test-root/pages/home.ghp");
        assert_eq!(d[0]["range"]["start"], json!({"line": 3, "character": 11}));
        // Loop variables of the header too.
        assert!(ask(RequestKind::Definition, SRC.find("<go= i ").unwrap() + 5).is_some());

        // References of `it`: the declaration and its use in the loop body.
        let refs = ask(RequestKind::References, SRC.find("(it)").unwrap() + 2).unwrap();
        assert_eq!(refs.as_array().unwrap().len(), 2);

        assert!(ask(RequestKind::SignatureHelp, in_body).is_none());
    }

    #[test]
    fn unused_import_warning_without_gopls_and_edits_refresh_it() {
        let r = offline();
        r.open(&page(), SRC);
        let d = r.diagnostics(&page());
        let msgs: Vec<_> = d.iter().map(|d| (d.source, d.message.as_str())).collect();
        assert_eq!(msgs, vec![("ghp", "\"os\" imported and not used")]);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].range.start, p(1, 2));

        // Using it silences the warning (incremental edit appending an echo).
        let end = offset_to_pos(SRC, SRC.len());
        r.change(&page(), &[ContentChange { range: Some(Range { start: end, end }), text: "<go= os.Args />".into() }]);
        assert!(r.diagnostics(&page()).is_empty());
        assert!(r.text(&page()).unwrap().ends_with("<go= os.Args />"));
    }

    #[test]
    fn relative_paths_resolve_against_the_root_and_close_forgets() {
        let r = offline();
        r.open(Path::new("pages/home.ghp"), "<go= x />");
        assert!(r.text(&page()).is_some());
        assert!(r.site_at(Path::new("pages/home.ghp"), p(0, 6)).is_some());
        assert!(r.with_parse(&page(), |parse, _| parse.diagnostics.len()).is_some());
        r.close(&page());
        assert!(r.text(&page()).is_none());
        assert!(r.diagnostics(&page()).is_empty());
    }

    #[test]
    fn events_can_be_taken_once() {
        let r = offline();
        assert!(r.take_events().is_some());
        assert!(r.take_events().is_none());
        assert!(!r.gopls_available());
        assert_eq!(r.root(), Path::new("/ghp-test-root"));
    }

    #[test]
    fn disabled_gopls_stays_silent() {
        let r = offline();
        let rx = r.take_events().unwrap();
        r.open(&page(), "<go= x />");
        r.open(&page(), "<go= y />");
        assert!(rx.try_iter().next().is_none());
    }
}
