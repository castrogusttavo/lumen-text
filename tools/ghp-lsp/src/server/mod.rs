//! Stdio language server for GHP templates.
//!
//! [`Server`] is a pure message handler (`handle` / `flush_due` return the messages to send),
//! which keeps it unit-testable; [`run`] wires it to stdin/stdout with a reader thread and
//! debounced diagnostics.
//!
//! Everything outside the Go expressions is answered here. Requests whose cursor sits inside a
//! Go region (`<go= .../>`, `<go:if .../>`, ...) go through [`Server::route_go`].

pub mod actions;
pub mod completion;
pub mod conv;
pub mod docs;
pub mod folding;
pub mod format;
pub mod highlight;
pub mod hover;
pub mod rpc;
pub mod selection;
pub mod semantic;
pub mod symbols;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ast::ExprRegion;
use crate::gopls::CancelToken;
use crate::gopls::proxy::uri_to_path;
use crate::router::{RequestKind, Router};
use conv::{diag_json, pos_from, uri_of};
use docs::Doc;

/// How long after the last edit diagnostics are recomputed and published.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Which request is being routed to the Go side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoMethod {
    Completion,
    Hover,
    Definition,
}

/// A request whose cursor is inside a Go region.
pub struct GoRequest<'a> {
    pub method: GoMethod,
    pub uri: &'a str,
    pub doc: &'a Doc,
    /// Cursor as a byte offset into `doc.text`.
    pub offset: usize,
    pub region: ExprRegion,
    /// The raw LSP params of the request.
    pub params: &'a Value,
}

/// The Go side: one [`Router`] (gopls per module) fed lazily with the server's documents.
#[derive(Default)]
struct GoSide {
    router: OnceLock<Router>,
    /// Document versions already handed to the router, by URI.
    synced: Mutex<HashMap<String, i64>>,
}

impl GoSide {
    /// The router with `doc` brought up to date, and the page path.
    fn sync(&self, uri: &str, doc: &Doc) -> Option<(&Router, PathBuf)> {
        let path = uri_to_path(uri)?;
        let router = self.router.get_or_init(|| Router::new("/"));
        let mut synced = self.synced.lock().unwrap();
        if synced.get(uri) != Some(&doc.version) || router.text(&path).is_none() {
            router.replace(&path, doc.text.clone());
            synced.insert(uri.to_owned(), doc.version);
        }
        Some((router, path))
    }

    fn close(&self, uri: &str) {
        self.synced.lock().unwrap().remove(uri);
        if let (Some(router), Some(path)) = (self.router.get(), uri_to_path(uri)) {
            router.close(&path);
        }
    }
}

#[derive(Default)]
pub struct Server {
    go: GoSide,
    docs: HashMap<String, Doc>,
    /// Documents whose diagnostics are stale, with the time they become due.
    pending: HashMap<String, Instant>,
    initialized: bool,
    shutdown: bool,
    exited: bool,
}

impl Server {
    pub fn new() -> Server {
        Server::default()
    }

    /// `Some(exit code)` once the `exit` notification arrived (0 after a clean `shutdown`).
    pub fn exit_code(&self) -> Option<i32> {
        self.exited.then_some(if self.shutdown { 0 } else { 1 })
    }

    /// Handle one incoming message and return the messages to send back.
    pub fn handle(&mut self, msg: Value) -> Vec<Value> {
        let Some(obj) = msg.as_object() else {
            return vec![rpc::error(Value::Null, rpc::INVALID_REQUEST, "invalid request")];
        };
        let method = obj.get("method").and_then(Value::as_str).map(str::to_owned);
        let id = obj.get("id").cloned();
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (Some(m), Some(id)) => self.request(&m, id, &params),
            (Some(m), None) => self.notification(&m, &params),
            // A response to a server-initiated request: nothing to do.
            (None, Some(_)) => Vec::new(),
            (None, None) => vec![rpc::error(Value::Null, rpc::INVALID_REQUEST, "invalid request")],
        }
    }

    /// Publish diagnostics for every document whose debounce delay has elapsed.
    pub fn flush_due(&mut self, now: Instant) -> Vec<Value> {
        let mut due: Vec<String> = self.pending.iter().filter(|&(_, &t)| t <= now).map(|(u, _)| u.clone()).collect();
        due.sort();
        self.publish_all(due)
    }

    /// Publish diagnostics for every stale document right now (tests, shutdown).
    pub fn flush_all(&mut self) -> Vec<Value> {
        let mut all: Vec<String> = self.pending.keys().cloned().collect();
        all.sort();
        self.publish_all(all)
    }

    /// When the next debounced publish is due, if any.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().min().copied()
    }

    fn publish_all(&mut self, uris: Vec<String>) -> Vec<Value> {
        let mut out = Vec::new();
        for uri in uris {
            self.pending.remove(&uri);
            if let Some(m) = self.diagnostics_message(&uri) {
                out.push(m);
            }
        }
        out
    }

    fn diagnostics_message(&self, uri: &str) -> Option<Value> {
        let doc = self.docs.get(uri)?;
        let mut diags: Vec<Value> = doc.parse.diagnostics.iter().map(|d| diag_json(d, &doc.index)).collect();
        // Go diagnostics from gopls (already filtered against the parser's errors by the router).
        if let Some((router, path)) = self.go.sync(uri, doc) {
            diags.extend(router.diagnostics(&path).iter().filter(|d| d.source == "gopls").map(|d| d.to_lsp()));
        }
        Some(rpc::notification(
            "textDocument/publishDiagnostics",
            json!({"uri": uri, "version": doc.version, "diagnostics": diags}),
        ))
    }

    fn request(&mut self, method: &str, id: Value, params: &Value) -> Vec<Value> {
        if method == "initialize" {
            if self.initialized {
                return vec![rpc::error(id, rpc::INVALID_REQUEST, "already initialized")];
            }
            self.initialized = true;
            return vec![rpc::response(id, initialize_result())];
        }
        if !self.initialized {
            return vec![rpc::error(id, rpc::SERVER_NOT_INITIALIZED, "server not initialized")];
        }
        if self.shutdown {
            return vec![rpc::error(id, rpc::INVALID_REQUEST, "server is shutting down")];
        }
        if method == "shutdown" {
            self.shutdown = true;
            return vec![rpc::response(id, Value::Null)];
        }
        match self.dispatch(method, params) {
            Some(result) => vec![rpc::response(id, result)],
            None => {
                vec![rpc::error(id, rpc::METHOD_NOT_FOUND, &format!("method not found: {method}"))]
            }
        }
    }

    fn notification(&mut self, method: &str, params: &Value) -> Vec<Value> {
        match method {
            "exit" => {
                self.exited = true;
                Vec::new()
            }
            "textDocument/didOpen" if self.initialized && !self.shutdown => {
                let item = &params["textDocument"];
                let (Some(uri), Some(text)) = (item["uri"].as_str(), item["text"].as_str()) else {
                    return Vec::new();
                };
                let version = item["version"].as_i64().unwrap_or(0);
                self.docs.insert(uri.to_owned(), Doc::new(version, text.to_owned()));
                self.pending.remove(uri);
                self.diagnostics_message(uri).into_iter().collect()
            }
            "textDocument/didChange" if self.initialized && !self.shutdown => {
                let Some(uri) = uri_of(params) else { return Vec::new() };
                let version = params["textDocument"]["version"].as_i64().unwrap_or(0);
                let changes = params["contentChanges"].as_array().map_or(&[][..], Vec::as_slice);
                if let Some(doc) = self.docs.get_mut(uri) {
                    doc.apply_changes(version, changes);
                    self.pending.insert(uri.to_owned(), Instant::now() + DEBOUNCE);
                }
                Vec::new()
            }
            "textDocument/didClose" => {
                let Some(uri) = uri_of(params) else { return Vec::new() };
                self.pending.remove(uri);
                self.go.close(uri);
                if self.docs.remove(uri).is_some() {
                    vec![rpc::notification("textDocument/publishDiagnostics", json!({"uri": uri, "diagnostics": []}))]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Route a request to its feature. `None` means the method is unknown.
    fn dispatch(&self, method: &str, params: &Value) -> Option<Value> {
        let go = match method {
            "textDocument/completion" => Some(GoMethod::Completion),
            "textDocument/hover" => Some(GoMethod::Hover),
            "textDocument/definition" => Some(GoMethod::Definition),
            _ => None,
        };
        if let Some(go) = go {
            return Some(self.position_request(go, params));
        }
        let known = matches!(
            method,
            "textDocument/documentSymbol"
                | "textDocument/foldingRange"
                | "textDocument/selectionRange"
                | "textDocument/formatting"
                | "textDocument/rangeFormatting"
                | "textDocument/documentHighlight"
                | "textDocument/semanticTokens/full"
                | "textDocument/codeAction"
        );
        if !known {
            return None;
        }
        let Some(uri) = uri_of(params) else { return Some(Value::Null) };
        let Some(doc) = self.docs.get(uri) else { return Some(Value::Null) };
        Some(match method {
            "textDocument/documentSymbol" => symbols::document_symbols(doc),
            "textDocument/foldingRange" => folding::folding_ranges(doc),
            "textDocument/selectionRange" => selection::selection_ranges(doc, params),
            "textDocument/formatting" => {
                let edits = format::format(doc, &format::Options::from_params(params));
                format::edits_json(doc, &edits)
            }
            "textDocument/rangeFormatting" => {
                let edits = format::format(doc, &format::Options::from_params(params));
                let edits = format::restrict(doc, edits, &params["range"]);
                format::edits_json(doc, &edits)
            }
            "textDocument/documentHighlight" => match params.get("position").and_then(pos_from) {
                Some(p) => highlight::highlights(doc, doc.offset(p)),
                None => Value::Null,
            },
            "textDocument/semanticTokens/full" => semantic::full(doc),
            _ => actions::code_actions(doc, uri, params),
        })
    }

    /// Requests that depend on a cursor position: Go regions go to the router, the rest is ours.
    fn position_request(&self, method: GoMethod, params: &Value) -> Value {
        let Some(uri) = uri_of(params) else { return Value::Null };
        let Some(doc) = self.docs.get(uri) else { return Value::Null };
        let Some(pos) = params.get("position").and_then(pos_from) else { return Value::Null };
        let offset = doc.offset(pos);
        if let Some(region) = doc.parse.tree.expr_region_at(offset) {
            let req = GoRequest { method, uri, doc, offset, region, params };
            return self.route_go(&req).unwrap_or(Value::Null);
        }
        match method {
            GoMethod::Completion => completion::complete(doc, offset),
            GoMethod::Hover => hover::hover(doc, offset),
            GoMethod::Definition => Value::Null,
        }
    }

    /// ROUTER HOOK: completion/hover/definition inside Go regions.
    ///
    /// Called when the cursor is inside a Go expression. Return the LSP result for the request,
    /// or `None` to answer `null`. The Go-expression router plugs in here.
    fn route_go(&self, req: &GoRequest) -> Option<Value> {
        let (router, path) = self.go.sync(req.uri, req.doc)?;
        let kind = match req.method {
            GoMethod::Completion => RequestKind::Completion,
            GoMethod::Hover => RequestKind::Hover,
            GoMethod::Definition => RequestKind::Definition,
        };
        let pos = req.doc.index.position(req.offset);
        router.request(&path, pos, kind, &CancelToken::default()).ok().flatten()
    }
}

fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": {"openClose": true, "change": 2},
            "completionProvider": {"triggerCharacters": ["<", ":", "=", "."]},
            "hoverProvider": true,
            "definitionProvider": true,
            "documentSymbolProvider": true,
            "foldingRangeProvider": true,
            "selectionRangeProvider": true,
            "documentFormattingProvider": true,
            "documentRangeFormattingProvider": true,
            "documentHighlightProvider": true,
            "codeActionProvider": {"codeActionKinds": ["quickfix"]},
            "semanticTokensProvider": {"legend": semantic::legend(), "full": true},
        },
        "serverInfo": {"name": "ghp-lsp", "version": env!("CARGO_PKG_VERSION")},
    })
}

enum Event {
    Message(Value),
    Eof,
}

/// Serve LSP over the given streams until `exit` or end of input. Returns the process exit code.
pub fn run<R, W>(input: R, mut output: W) -> i32
where
    R: BufRead + Send + 'static,
    W: Write,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut input = input;
        loop {
            match rpc::read_message(&mut input) {
                Ok(Some(msg)) => {
                    if tx.send(Event::Message(msg)).is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = tx.send(Event::Eof);
                    return;
                }
            }
        }
    });

    let mut server = Server::new();
    loop {
        let event = match server.next_deadline() {
            Some(deadline) => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(ev) => Some(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => Some(Event::Eof),
            },
            None => Some(rx.recv().unwrap_or(Event::Eof)),
        };
        let out = match event {
            None => server.flush_due(Instant::now()),
            Some(Event::Message(msg)) => server.handle(msg),
            Some(Event::Eof) => return if server.shutdown { 0 } else { 1 },
        };
        for m in &out {
            if rpc::write_message(&mut output, m).is_err() {
                return 1;
            }
        }
        if let Some(code) = server.exit_code() {
            return code;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "file:///t/page.ghp";

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn note(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": method, "params": params})
    }

    fn started(text: &str) -> Server {
        let mut s = Server::new();
        s.handle(req(1, "initialize", json!({"capabilities": {}})));
        s.handle(note("initialized", json!({})));
        s.handle(note(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": URI, "languageId": "ghp", "version": 1, "text": text}}),
        ));
        s
    }

    fn ask(s: &mut Server, method: &str, extra: Value) -> Value {
        let mut params = json!({"textDocument": {"uri": URI}});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        let out = s.handle(req(9, method, params));
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].get("error").is_none(), "{out:?}");
        out[0]["result"].clone()
    }

    fn pos(line: u32, character: u32) -> Value {
        json!({"line": line, "character": character})
    }

    #[test]
    fn lifecycle() {
        let mut s = Server::new();
        let out = s.handle(req(1, "textDocument/hover", json!({})));
        assert_eq!(out[0]["error"]["code"], rpc::SERVER_NOT_INITIALIZED);
        let out = s.handle(req(2, "initialize", json!({})));
        assert!(out[0]["result"]["capabilities"]["completionProvider"].is_object());
        assert_eq!(s.handle(req(3, "initialize", json!({})))[0]["error"]["code"], rpc::INVALID_REQUEST);
        assert_eq!(s.handle(req(4, "no/such", json!({})))[0]["error"]["code"], rpc::METHOD_NOT_FOUND);
        assert_eq!(s.handle(req(5, "shutdown", Value::Null))[0]["result"], Value::Null);
        assert_eq!(s.handle(req(6, "textDocument/hover", json!({})))[0]["error"]["code"], rpc::INVALID_REQUEST);
        assert_eq!(s.exit_code(), None);
        s.handle(note("exit", Value::Null));
        assert_eq!(s.exit_code(), Some(0));
    }

    #[test]
    fn exit_without_shutdown_is_an_error() {
        let mut s = Server::new();
        s.handle(note("exit", Value::Null));
        assert_eq!(s.exit_code(), Some(1));
    }

    #[test]
    fn garbage_gets_an_error_reply() {
        let mut s = Server::new();
        assert_eq!(s.handle(Value::Null)[0]["error"]["code"], rpc::INVALID_REQUEST);
        assert_eq!(s.handle(json!({"jsonrpc": "2.0"}))[0]["error"]["code"], rpc::INVALID_REQUEST);
    }

    #[test]
    fn open_publishes_immediately_and_change_is_debounced() {
        let mut s = Server::new();
        s.handle(req(1, "initialize", json!({})));
        let out = s.handle(note(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": URI, "version": 1, "text": "<go:if a/>"}}),
        ));
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[0]["params"]["diagnostics"][0]["code"], "unclosed-if");
        assert_eq!(out[0]["params"]["diagnostics"][0]["source"], "ghp");

        let out = s.handle(note(
            "textDocument/didChange",
            json!({"textDocument": {"uri": URI, "version": 2},
                   "contentChanges": [{"range": {"start": pos(0, 10), "end": pos(0, 10)}, "text": "<go:endif/>"}]}),
        ));
        assert!(out.is_empty());
        assert!(s.next_deadline().is_some());
        let out = s.flush_all();
        assert_eq!(out[0]["params"]["version"], 2);
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
        assert!(s.next_deadline().is_none());
        assert!(s.flush_all().is_empty());
    }

    #[test]
    fn due_flush_after_the_delay() {
        let mut s = started("x");
        s.handle(note(
            "textDocument/didChange",
            json!({"textDocument": {"uri": URI, "version": 2}, "contentChanges": [{"text": "<go:endif/>"}]}),
        ));
        let out = s.flush_due(Instant::now() + DEBOUNCE * 2);
        assert_eq!(out[0]["params"]["diagnostics"][0]["code"], "unmatched-endif");
    }

    #[test]
    fn close_clears_diagnostics() {
        let mut s = started("<go:endif/>");
        let out = s.handle(note("textDocument/didClose", json!({"textDocument": {"uri": URI}})));
        assert_eq!(out[0]["params"]["diagnostics"], json!([]));
        assert_eq!(ask(&mut s, "textDocument/documentSymbol", json!({})), Value::Null);
    }

    #[test]
    fn features_answer_over_the_document() {
        let text = "<go:if a/>\n    <go:for x/>\n        <p/>\n    <go:endfor/>\n<go:endif/>\n<go= name />\n";
        let mut s = started(text);
        let c = ask(&mut s, "textDocument/completion", json!({"position": pos(5, 1)}));
        let _ = c;
        let syms = ask(&mut s, "textDocument/documentSymbol", json!({}));
        assert_eq!(syms[0]["name"], "if a");
        assert!(!ask(&mut s, "textDocument/foldingRange", json!({})).as_array().unwrap().is_empty());
        let sel = ask(&mut s, "textDocument/selectionRange", json!({"positions": [pos(0, 3)]}));
        assert!(sel[0]["parent"].is_object());
        let hl = ask(&mut s, "textDocument/documentHighlight", json!({"position": pos(0, 3)}));
        assert_eq!(hl.as_array().unwrap().len(), 2);
        let h = ask(&mut s, "textDocument/hover", json!({"position": pos(0, 3)}));
        assert!(h["contents"]["value"].as_str().unwrap().contains("go:if"));
        let f = ask(&mut s, "textDocument/formatting", json!({"options": {"tabSize": 4, "insertSpaces": true}}));
        assert!(f.as_array().unwrap().is_empty(), "already formatted: {f}");
        let t = ask(&mut s, "textDocument/semanticTokens/full", json!({}));
        assert!(!t["data"].as_array().unwrap().is_empty());
    }

    #[test]
    fn go_regions_go_to_the_router_hook() {
        let mut s = started("<go= user.Na");
        // Inside the (unterminated) Go expression the stub router answers null.
        let r = ask(&mut s, "textDocument/hover", json!({"position": pos(0, 8)}));
        assert_eq!(r, Value::Null);
        let r = ask(&mut s, "textDocument/completion", json!({"position": pos(0, 12)}));
        assert_eq!(r, Value::Null);
        let r = ask(&mut s, "textDocument/definition", json!({"position": pos(0, 8)}));
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn code_action_round_trip() {
        let mut s = started("<go:if a/>\nx");
        let r = ask(
            &mut s,
            "textDocument/codeAction",
            json!({
                "range": {"start": pos(0, 1), "end": pos(0, 1)},
                "context": {"diagnostics": []},
            }),
        );
        assert_eq!(r[0]["kind"], "quickfix");
    }
}
