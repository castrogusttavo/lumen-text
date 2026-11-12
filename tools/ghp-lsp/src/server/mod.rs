//! Stdio language server for GHP templates.
//!
//! [`Server`] is a pure message handler (`handle` / `flush_due` / `on_go_event` return the
//! messages to send), which keeps it unit-testable; [`run`] wires it to stdin/stdout with a reader
//! thread, debounced diagnostics, gopls events and `$/cancelRequest`.
//!
//! Everything outside the Go expressions is answered here. Requests whose cursor sits inside a
//! Go region (`<go= .../>`, `<go:if .../>`, ...) go through [`Server::route_go`].
//!
//! Failure containment: a panic inside any handler is caught at [`Server::handle`] and answered
//! with an `InternalError` response (the session goes on), a request cancelled by the client
//! answers `RequestCancelled`, and malformed parameters answer `InvalidParams`.

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

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ast::ExprRegion;
use crate::gopls::proxy::{LockExt, uri_to_path};
use crate::gopls::{CancelToken, Config, Event as GoEvent, ProxyError};
use crate::router::{RequestKind, Router};
use conv::{diag_json, pos_from, range_from, uri_of};
use docs::Doc;

/// How long after the last edit diagnostics are recomputed and published.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// How long gopls news (new diagnostics, a finished start) is held back to coalesce bursts.
pub const GO_DEBOUNCE: Duration = Duration::from_millis(40);

/// Environment variable that turns the gopls bridge off (`GHP_LSP_NO_GOPLS=1`): Go expressions
/// are then answered from the page's own scope analysis only.
pub const NO_GOPLS_ENV: &str = "GHP_LSP_NO_GOPLS";

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

/// What a handler answers: the `result`, or a JSON-RPC error `(code, message)`.
type Reply = Result<Value, (i64, String)>;

fn invalid(what: &str) -> Reply {
    Err((rpc::INVALID_PARAMS, format!("invalid params: {what}")))
}

/// In-flight requests that the client may cancel with `$/cancelRequest`.
///
/// The reader thread calls [`Cancels::cancel`] the moment the notification is read, so a request
/// that is blocked on gopls (or still queued behind one) is interrupted without waiting for the
/// main loop. A cancel that arrives before its request is remembered and applied when the request
/// starts.
#[derive(Default)]
pub struct Cancels {
    state: Mutex<CancelState>,
}

#[derive(Default)]
struct CancelState {
    live: HashMap<String, CancelToken>,
    early: HashSet<String>,
}

/// Ids remembered for requests that have not started (bounded: a stale id is harmless).
const MAX_EARLY_CANCELS: usize = 1024;

impl Cancels {
    /// Cancel the request with this JSON-RPC id.
    pub fn cancel(&self, id: &Value) {
        let key = id.to_string();
        let mut st = self.state.lock_ok();
        match st.live.get(&key) {
            Some(token) => token.cancel(),
            None => {
                if st.early.len() >= MAX_EARLY_CANCELS {
                    st.early.clear();
                }
                st.early.insert(key);
            }
        }
    }

    fn begin(&self, id: &Value) -> CancelToken {
        let key = id.to_string();
        let mut st = self.state.lock_ok();
        let token = CancelToken::default();
        if st.early.remove(&key) {
            token.cancel();
        }
        st.live.insert(key, token.clone());
        token
    }

    fn end(&self, id: &Value) {
        self.state.lock_ok().live.remove(&id.to_string());
    }
}

/// The path the Go side knows a document by. Documents without a `file:` URI (`untitled:`) get a
/// stable placeholder, so they still get the scope-based answers.
fn go_path(uri: &str) -> PathBuf {
    uri_to_path(uri).unwrap_or_else(|| {
        let name: String = uri.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        PathBuf::from(format!("/__ghp_untitled__/{name}.ghp"))
    })
}

/// The Go side: one [`Router`] (gopls per module, started in the background) fed lazily with the
/// server's documents.
struct GoSide {
    router: Router,
    events: Option<Receiver<GoEvent>>,
    /// Document versions already handed to the router, by URI.
    synced: Mutex<HashMap<String, i64>>,
}

impl GoSide {
    fn new() -> GoSide {
        let disabled = std::env::var_os(NO_GOPLS_ENV).is_some_and(|v| !v.is_empty() && v != "0");
        let cfg = Config {
            background_start: true,
            disabled,
            // The main loop waits for a Go request, so keep a stuck gopls from holding it for long.
            request_timeout: Duration::from_secs(10),
            ..Config::default()
        };
        let router = Router::with_config("/", cfg);
        let events = router.take_events();
        GoSide { router, events, synced: Mutex::new(HashMap::new()) }
    }

    /// The router with `doc` brought up to date, and the page path.
    fn sync(&self, uri: &str, doc: &Doc) -> (&Router, PathBuf) {
        let path = go_path(uri);
        let mut synced = self.synced.lock_ok();
        if synced.get(uri) != Some(&doc.version) || self.router.text(&path).is_none() {
            self.router.replace(&path, doc.text.clone());
            synced.insert(uri.to_owned(), doc.version);
        }
        (&self.router, path)
    }

    fn close(&self, uri: &str) {
        self.synced.lock_ok().remove(uri);
        self.router.close(&go_path(uri));
    }
}

pub struct Server {
    go: GoSide,
    docs: HashMap<String, Doc>,
    /// Documents whose diagnostics are stale, with the time they become due.
    pending: HashMap<String, Instant>,
    cancels: Arc<Cancels>,
    /// Last semantic tokens sent per document (`resultId`, data), for `full/delta`.
    sem_cache: HashMap<String, (String, Vec<u32>)>,
    sem_seq: u64,
    initialized: bool,
    shutdown: bool,
    exited: bool,
}

impl Default for Server {
    fn default() -> Self {
        Server::new()
    }
}

impl Server {
    pub fn new() -> Server {
        Server {
            go: GoSide::new(),
            docs: HashMap::new(),
            pending: HashMap::new(),
            cancels: Arc::new(Cancels::default()),
            sem_cache: HashMap::new(),
            sem_seq: 0,
            initialized: false,
            shutdown: false,
            exited: false,
        }
    }

    /// `Some(exit code)` once the `exit` notification arrived (0 after a clean `shutdown`).
    pub fn exit_code(&self) -> Option<i32> {
        self.exited.then_some(if self.shutdown { 0 } else { 1 })
    }

    /// The registry [`run`]'s reader thread cancels requests through.
    pub fn cancels(&self) -> Arc<Cancels> {
        self.cancels.clone()
    }

    /// The channel of gopls events (new diagnostics, status, start / crash). Taken once; [`run`]
    /// feeds it to [`Server::on_go_event`].
    pub fn take_go_events(&mut self) -> Option<Receiver<GoEvent>> {
        self.go.events.take()
    }

    /// React to news from the gopls bridge: late diagnostics are republished (after a short
    /// coalescing delay), a start / restart / crash refreshes every document, a status line is
    /// shown to the user.
    pub fn on_go_event(&mut self, event: GoEvent) -> Vec<Value> {
        let due = Instant::now() + GO_DEBOUNCE;
        let stale: Vec<String> = match &event {
            GoEvent::Diagnostics(path) => self.docs.keys().filter(|u| go_path(u) == *path).cloned().collect(),
            GoEvent::Started(_) | GoEvent::Restarted(_) | GoEvent::Crashed(_) => self.docs.keys().cloned().collect(),
            GoEvent::Status(msg) => {
                // Missing gopls is a fact, a crash loop is a problem.
                let kind = if msg.contains("not found") { 3 } else { 2 };
                return vec![rpc::notification("window/showMessage", json!({"type": kind, "message": msg}))];
            }
        };
        for uri in stale {
            self.pending.entry(uri).or_insert(due);
        }
        Vec::new()
    }

    /// Handle one incoming message and return the messages to send back. Never panics: a panic
    /// in a handler is answered with an `InternalError` (requests) or dropped (notifications).
    pub fn handle(&mut self, msg: Value) -> Vec<Value> {
        let id = msg.get("id").cloned();
        let is_request = msg.get("method").is_some() && id.is_some();
        match catch_unwind(AssertUnwindSafe(|| self.handle_message(msg))) {
            Ok(out) => out,
            Err(panic) => {
                eprintln!("ghp-lsp: internal error: {}", panic_text(panic.as_ref()));
                match id {
                    Some(id) if is_request => {
                        self.cancels.end(&id);
                        vec![rpc::error(id, rpc::INTERNAL_ERROR, "internal error")]
                    }
                    _ => Vec::new(),
                }
            }
        }
    }

    fn handle_message(&mut self, msg: Value) -> Vec<Value> {
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
            match catch_unwind(AssertUnwindSafe(|| self.diagnostics_message(&uri))) {
                Ok(m) => out.extend(m),
                Err(panic) => {
                    eprintln!("ghp-lsp: internal error publishing diagnostics: {}", panic_text(panic.as_ref()));
                }
            }
        }
        out
    }

    fn diagnostics_message(&self, uri: &str) -> Option<Value> {
        let doc = self.docs.get(uri)?;
        let mut diags: Vec<Value> = doc.parse.diagnostics.iter().map(|d| diag_json(d, &doc.index)).collect();
        // Go diagnostics from gopls (already filtered against the parser's errors by the router).
        let (router, path) = self.go.sync(uri, doc);
        diags.extend(router.diagnostics(&path).iter().filter(|d| d.source == "gopls").map(|d| d.to_lsp()));
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
        let token = self.cancels.begin(&id);
        let reply = if token.is_cancelled() {
            Err((rpc::REQUEST_CANCELLED, "request cancelled".to_owned()))
        } else {
            self.dispatch(method, params, &token)
        };
        self.cancels.end(&id);
        vec![match reply {
            Ok(result) => rpc::response(id, result),
            Err((code, message)) => rpc::error(id, code, &message),
        }]
    }

    fn notification(&mut self, method: &str, params: &Value) -> Vec<Value> {
        match method {
            "exit" => {
                self.exited = true;
                Vec::new()
            }
            "$/cancelRequest" => {
                // The reader thread already acted on it; this covers hosts that call `handle` directly.
                if let Some(id) = params.get("id") {
                    self.cancels.cancel(id);
                }
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
                self.sem_cache.remove(uri);
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

    /// Route a request to its feature.
    fn dispatch(&mut self, method: &str, params: &Value, cancel: &CancelToken) -> Reply {
        #[cfg(test)]
        {
            if method == "ghp/testPanic" {
                panic!("requested by the test");
            }
        }
        let go = match method {
            "textDocument/completion" => Some(GoMethod::Completion),
            "textDocument/hover" => Some(GoMethod::Hover),
            "textDocument/definition" => Some(GoMethod::Definition),
            _ => None,
        };
        if let Some(go) = go {
            return self.position_request(go, params, cancel);
        }
        if method.starts_with("textDocument/semanticTokens/") {
            return self.semantic_tokens(method, params);
        }
        let known = matches!(
            method,
            "textDocument/documentSymbol"
                | "textDocument/foldingRange"
                | "textDocument/selectionRange"
                | "textDocument/formatting"
                | "textDocument/rangeFormatting"
                | "textDocument/documentHighlight"
                | "textDocument/codeAction"
        );
        if !known {
            return Err((rpc::METHOD_NOT_FOUND, format!("method not found: {method}")));
        }
        let Some(uri) = uri_of(params) else { return invalid("missing textDocument.uri") };
        let Some(doc) = self.docs.get(uri) else { return Ok(Value::Null) };
        Ok(match method {
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
                None => return invalid("missing or malformed position"),
            },
            _ => actions::code_actions(doc, uri, params),
        })
    }

    /// `textDocument/semanticTokens/{full,full/delta,range}`.
    fn semantic_tokens(&mut self, method: &str, params: &Value) -> Reply {
        let Some(uri) = uri_of(params) else { return invalid("missing textDocument.uri") };
        let Some(doc) = self.docs.get(uri) else { return Ok(Value::Null) };
        match method {
            "textDocument/semanticTokens/range" => {
                let Some(r) = params.get("range").and_then(range_from) else { return invalid("missing range") };
                Ok(semantic::range(doc, r.start, r.end))
            }
            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/full/delta" => {
                let data = semantic::data(doc, None);
                self.sem_seq += 1;
                let result_id = self.sem_seq.to_string();
                let previous = params.get("previousResultId").and_then(Value::as_str);
                let old = self.sem_cache.get(uri).filter(|(id, _)| Some(id.as_str()) == previous).map(|(_, d)| d);
                let out = match old {
                    Some(old) if method.ends_with("/delta") => {
                        json!({"resultId": result_id, "edits": semantic::delta(old, &data)})
                    }
                    _ => json!({"resultId": result_id, "data": data}),
                };
                self.sem_cache.insert(uri.to_owned(), (result_id, data));
                Ok(out)
            }
            _ => Err((rpc::METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }

    /// Requests that depend on a cursor position: Go regions go to the router, the rest is ours.
    fn position_request(&self, method: GoMethod, params: &Value, cancel: &CancelToken) -> Reply {
        let Some(uri) = uri_of(params) else { return invalid("missing textDocument.uri") };
        let Some(pos) = params.get("position").and_then(pos_from) else {
            return invalid("missing or malformed position");
        };
        let Some(doc) = self.docs.get(uri) else { return Ok(Value::Null) };
        let offset = doc.offset(pos);
        if let Some(region) = doc.parse.tree.expr_region_at(offset) {
            let req = GoRequest { method, uri, doc, offset, region, params };
            return self.route_go(&req, cancel);
        }
        Ok(match method {
            GoMethod::Completion => completion::complete(doc, offset),
            GoMethod::Hover => hover::hover(doc, offset),
            GoMethod::Definition => Value::Null,
        })
    }

    /// Completion/hover/definition inside Go regions: gopls through the router, the page's own
    /// scope analysis while gopls is missing, still starting or crashed.
    fn route_go(&self, req: &GoRequest, cancel: &CancelToken) -> Reply {
        let (router, path) = self.go.sync(req.uri, req.doc);
        let kind = match req.method {
            GoMethod::Completion => RequestKind::Completion,
            GoMethod::Hover => RequestKind::Hover,
            GoMethod::Definition => RequestKind::Definition,
        };
        let pos = req.doc.index.position(req.offset);
        match router.request(&path, pos, kind, cancel) {
            Ok(v) => Ok(v.unwrap_or(Value::Null)),
            Err(ProxyError::Cancelled) => Err((rpc::REQUEST_CANCELLED, "request cancelled".to_owned())),
            // Timeout, gopls error: nothing to offer, but not a failure of the editor's request.
            Err(_) => Ok(Value::Null),
        }
    }
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
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
            "semanticTokensProvider": {"legend": semantic::legend(), "full": {"delta": true}, "range": true},
        },
        "serverInfo": {"name": "ghp-lsp", "version": env!("CARGO_PKG_VERSION")},
    })
}

enum Event {
    Message(Value),
    Go(GoEvent),
    Eof,
}

/// Serve LSP over the given streams until `exit` or end of input. Returns the process exit code.
pub fn run<R, W>(input: R, mut output: W) -> i32
where
    R: BufRead + Send + 'static,
    W: Write,
{
    let mut server = Server::new();
    let (tx, rx) = mpsc::channel();

    // gopls news arrives on its own channel: forward it into the main loop's queue.
    if let Some(go_events) = server.take_go_events() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for ev in go_events {
                if tx.send(Event::Go(ev)).is_err() {
                    return;
                }
            }
        });
    }

    // Reader: frames messages, and acts on `$/cancelRequest` immediately (the main loop may be
    // blocked on gopls).
    let cancels = server.cancels();
    std::thread::spawn(move || {
        let mut input = input;
        loop {
            match rpc::read_message(&mut input) {
                Ok(Some(msg)) => {
                    if msg.get("method").and_then(Value::as_str) == Some("$/cancelRequest")
                        && let Some(id) = msg.get("params").and_then(|p| p.get("id"))
                    {
                        cancels.cancel(id);
                    }
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
            Some(Event::Go(ev)) => server.on_go_event(ev),
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

    fn change(s: &mut Server, version: i64, changes: Value) {
        s.handle(note(
            "textDocument/didChange",
            json!({"textDocument": {"uri": URI, "version": version}, "contentChanges": changes}),
        ));
    }

    #[test]
    fn cancel_before_the_request_runs() {
        let mut s = started("<p>x</p>");
        s.cancels().cancel(&json!(9));
        let out = s.handle(req(9, "textDocument/foldingRange", json!({"textDocument": {"uri": URI}})));
        assert_eq!(out[0]["error"]["code"], rpc::REQUEST_CANCELLED);
        // The cancel is spent: the same id works again, and the notification form is honoured too.
        assert!(ask(&mut s, "textDocument/foldingRange", json!({})).is_array());
        s.handle(note("$/cancelRequest", json!({"id": 9})));
        let out = s.handle(req(9, "textDocument/foldingRange", json!({"textDocument": {"uri": URI}})));
        assert_eq!(out[0]["error"]["code"], rpc::REQUEST_CANCELLED);
        // Cancelling something unknown or malformed is harmless.
        s.handle(note("$/cancelRequest", json!({})));
        s.handle(note("$/cancelRequest", Value::Null));
        s.handle(note("$/cancelRequest", json!({"id": "never-sent"})));
    }

    #[test]
    fn a_panicking_handler_is_an_internal_error_and_the_server_lives_on() {
        let mut s = started("<go:if a/>");
        let out = s.handle(req(5, "ghp/testPanic", json!({})));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 5);
        assert_eq!(out[0]["error"]["code"], rpc::INTERNAL_ERROR);
        assert!(ask(&mut s, "textDocument/documentSymbol", json!({})).is_array());
    }

    #[test]
    fn invalid_params_are_errors_not_nulls_or_panics() {
        let mut s = started("x");
        let pos0 = json!({"line": 0, "character": 0});
        let cases = [
            ("textDocument/hover", json!({"textDocument": {"uri": URI}})),
            ("textDocument/completion", Value::Null),
            ("textDocument/definition", json!([1, 2])),
            ("textDocument/documentSymbol", json!([1, 2])),
            ("textDocument/foldingRange", json!({"textDocument": 3})),
            ("textDocument/hover", json!({"textDocument": {"uri": 5}, "position": pos0})),
            ("textDocument/hover", json!({"textDocument": {"uri": URI}, "position": {"line": -1, "character": "x"}})),
            ("textDocument/semanticTokens/range", json!({"textDocument": {"uri": URI}})),
            ("textDocument/semanticTokens/full", Value::Null),
            ("textDocument/documentHighlight", json!({"textDocument": {"uri": URI}})),
        ];
        for (method, params) in cases {
            let out = s.handle(req(3, method, params.clone()));
            assert_eq!(out[0]["error"]["code"], rpc::INVALID_PARAMS, "{method} {params}: {out:?}");
        }
        // An unknown document is not an error: there is just nothing to say.
        let other = json!({"textDocument": {"uri": "file:///nowhere.ghp"}, "position": pos0});
        assert_eq!(s.handle(req(4, "textDocument/hover", other))[0]["result"], Value::Null);
    }

    #[test]
    fn garbage_notifications_are_ignored() {
        let mut s = started("<p/>");
        let weird = [
            Value::Null,
            json!(1),
            json!({"textDocument": 3}),
            json!({"textDocument": {"uri": URI, "version": "x", "text": 4}, "contentChanges": 7}),
            json!({"textDocument": {"uri": URI, "version": 9},
                   "contentChanges": [{"range": 5, "text": 7}, 12, {"text": null},
                       {"range": {"start": {"line": 99999999, "character": 99999999}, "end": pos(0, 0)}, "text": "é"}]}),
        ];
        for params in weird {
            for method in ["textDocument/didOpen", "textDocument/didChange", "$/setTrace", "initialized", "nope"] {
                s.handle(note(method, params.clone()));
            }
        }
        s.flush_all();
        for params in [Value::Null, json!({"textDocument": {}}), json!({"textDocument": {"uri": URI}})] {
            s.handle(note("textDocument/didClose", params));
        }
    }

    #[test]
    fn edits_inside_surrogate_pairs_and_around_crlf_apply_cleanly() {
        let mut s = started("a😀b\r\n<go= x />\r\n");
        // Column 2 is the middle of the emoji (UTF-16): it rounds down, nothing panics.
        let edits = json!([
            {"range": {"start": pos(0, 2), "end": pos(0, 2)}, "text": "é"},
            {"range": {"start": pos(0, 1), "end": pos(0, 3)}, "text": ""},
            {"range": {"start": pos(1, 0), "end": pos(1, 0)}, "text": "\r\n"},
        ]);
        change(&mut s, 2, edits);
        assert_eq!(s.docs[URI].text, "a😀b\r\n\r\n<go= x />\r\n");
        // A range running past the end of the file and one that is backwards.
        let edits = json!([
            {"range": {"start": pos(40, 0), "end": pos(50, 0)}, "text": "!"},
            {"range": {"start": pos(1, 0), "end": pos(0, 0)}, "text": "?"},
        ]);
        change(&mut s, 3, edits);
        assert!(s.docs[URI].text.ends_with('!'));
        // A lone CR is a line break too.
        change(&mut s, 4, json!([{"text": "a\rb"}, {"range": {"start": pos(1, 0), "end": pos(1, 1)}, "text": "B"}]));
        assert_eq!(s.docs[URI].text, "a\rB");
    }

    #[test]
    fn semantic_tokens_range_and_delta() {
        let mut s = started("<go:if a/>\n<go:endif/>\n");
        let full = ask(&mut s, "textDocument/semanticTokens/full", json!({}));
        let id = full["resultId"].as_str().unwrap().to_owned();
        let d = ask(&mut s, "textDocument/semanticTokens/full/delta", json!({"previousResultId": id}));
        assert_eq!(d["edits"], json!([]));

        change(&mut s, 2, json!([{"range": {"start": pos(0, 0), "end": pos(0, 0)}, "text": "x\n"}]));
        let d2 = ask(&mut s, "textDocument/semanticTokens/full/delta", json!({"previousResultId": d["resultId"]}));
        assert!(!d2["edits"].as_array().unwrap().is_empty(), "{d2}");

        let unknown = ask(&mut s, "textDocument/semanticTokens/full/delta", json!({"previousResultId": "nope"}));
        assert!(unknown["data"].is_array() && unknown.get("edits").is_none());

        let r =
            ask(&mut s, "textDocument/semanticTokens/range", json!({"range": {"start": pos(2, 0), "end": pos(3, 0)}}));
        // Only the `endif` tokens (line 2 of the edited text), relative to the document start.
        assert_eq!(r["data"].as_array().unwrap().len(), 10);
        assert_eq!(r["data"][0], 2);
    }

    #[test]
    fn late_gopls_diagnostics_schedule_a_republish() {
        let mut s = started("<go= x />");
        assert!(s.next_deadline().is_none());
        assert!(s.on_go_event(GoEvent::Diagnostics(go_path(URI))).is_empty());
        assert!(s.next_deadline().is_some());
        let out = s.flush_due(Instant::now() + Duration::from_secs(1));
        assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
        assert_eq!(out[0]["params"]["uri"], URI);
        assert!(s.next_deadline().is_none());

        // News about a page that is not open is ignored; a start or crash refreshes everything.
        s.on_go_event(GoEvent::Diagnostics(PathBuf::from("/elsewhere/other.ghp")));
        assert!(s.next_deadline().is_none());
        s.on_go_event(GoEvent::Started(PathBuf::from("/t")));
        assert!(s.next_deadline().is_some());
        s.flush_all();
        s.on_go_event(GoEvent::Crashed(PathBuf::from("/t")));
        assert!(s.next_deadline().is_some());

        // Status lines are shown to the user.
        let out = s.on_go_event(GoEvent::Status("gopls not found: x".into()));
        assert_eq!(out[0]["method"], "window/showMessage");
        assert_eq!(out[0]["params"]["type"], 3);
        let out = s.on_go_event(GoEvent::Status("gopls keeps crashing".into()));
        assert_eq!(out[0]["params"]["type"], 2);
    }

    #[test]
    fn untitled_documents_get_the_scope_answers_too() {
        let mut s = Server::new();
        s.handle(req(1, "initialize", json!({})));
        let uri = "untitled:Untitled-1";
        s.handle(note(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "version": 1, "text": "<go count := 1 /><go= co />"}}),
        ));
        let out =
            s.handle(req(2, "textDocument/completion", json!({"textDocument": {"uri": uri}, "position": pos(0, 24)})));
        let labels: Vec<&str> =
            out[0]["result"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
        assert!(labels.contains(&"count"), "{out:?}");
    }

    #[test]
    fn completion_in_an_empty_go_tag_and_in_an_attribute() {
        let text = "<go url := \"/x\" />\n<a href=\"<go= />\" title=\"<go= ur />\">x</a>\n<go /><go= />";
        let mut s = started(text);
        let labels = |v: Value| -> Vec<String> {
            v["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_owned()).collect()
        };
        // Inside the empty `<go= />` of the attribute, in a prefix of the other one, in `<go />`.
        let at = |needle: &str, delta: usize| {
            let off = text.find(needle).unwrap() + delta;
            let line = text[..off].matches('\n').count() as u32;
            let col = off - text[..off].rfind('\n').map_or(0, |i| i + 1);
            pos(line, col as u32)
        };
        for p in [at("<go= />\" title", 5), at("<go /><go", 4), at("<go /><go= />", 11)] {
            let v = ask(&mut s, "textDocument/completion", json!({"position": p}));
            assert!(labels(v).contains(&"url".to_string()), "{p}");
        }
        let v = ask(&mut s, "textDocument/completion", json!({"position": at("ur />", 2)}));
        assert_eq!(labels(v), vec!["url"]);
        // The head of the tag is not Go: that is tag completion territory.
        let v = ask(&mut s, "textDocument/hover", json!({"position": at("<go= />\" title", 2)}));
        assert!(v["contents"]["value"].as_str().unwrap().contains("go="), "{v}");
    }
}
