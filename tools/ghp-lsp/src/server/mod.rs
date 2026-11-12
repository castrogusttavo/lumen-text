//! Stdio language server for GHP templates.
//!
//! [`Server`] is a pure message handler (`handle` / `flush_due` return the messages to send),
//! which keeps it unit-testable; [`run`] wires it to stdin/stdout with a reader thread and
//! debounced diagnostics.
//!
//! Everything outside the Go expressions is answered here. Requests whose cursor sits inside a
//! Go region (`<go= .../>`, `<go:if .../>`, ...) go through [`Server::route_go`].

pub mod conv;
pub mod docs;
pub mod rpc;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ast::ExprRegion;
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

#[derive(Default)]
pub struct Server {
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
        let diags: Vec<Value> = doc.parse.diagnostics.iter().map(|d| diag_json(d, &doc.index)).collect();
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
    fn dispatch(&mut self, method: &str, params: &Value) -> Option<Value> {
        let go = match method {
            "textDocument/completion" => Some(GoMethod::Completion),
            "textDocument/hover" => Some(GoMethod::Hover),
            "textDocument/definition" => Some(GoMethod::Definition),
            _ => None,
        };
        if let Some(go) = go {
            return Some(self.position_request(go, params));
        }
        None
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
        Value::Null
    }

    /// ROUTER HOOK: completion/hover/definition inside Go regions.
    ///
    /// Called when the cursor is inside a Go expression. Return the LSP result for the request,
    /// or `None` to answer `null`. The Go-expression router plugs in here.
    fn route_go(&self, req: &GoRequest) -> Option<Value> {
        let _ = req;
        None
    }
}

fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": {"openClose": true, "change": 2},
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
