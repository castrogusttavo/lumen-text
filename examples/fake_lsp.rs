//! A tiny language server for the editor's tests (`cargo test` builds it; it is not shipped).
//!
//! Behaviour is picked with environment variables so one binary plays every role:
//! `FAKE_NAME` (hover/diagnostic source), `FAKE_PULL=1` (pull diagnostics instead of push),
//! `FAKE_FORMAT=1` (documentFormattingProvider), `FAKE_DYNAMIC=1` (register formatting dynamically),
//! `FAKE_CONFIG=1` (ask `workspace/configuration`), `FAKE_PROGRESS=1` (`$/progress`),
//! `FAKE_EXIT_AFTER_INIT=1` (die right after `initialized`), `FAKE_NO_HOVER=1`.
//! A document containing `CRASH` kills the server. Every line containing `FIXME` gets a warning.
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

fn env(k: &str) -> bool {
    std::env::var(k).is_ok_and(|v| v == "1")
}

fn send(v: &Value) {
    let body = v.to_string();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = out.flush();
}

fn diags(text: &str, name: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (l, line) in text.lines().enumerate() {
        if let Some(at) = line.find("FIXME") {
            let c0: usize = line[..at].encode_utf16().count();
            out.push(json!({
                "range": {"start": {"line": l, "character": c0}, "end": {"line": l, "character": c0 + 5}},
                "severity": 2, "source": name, "message": format!("fixme from {name}"), "code": "F1"
            }));
            // The same finding from every fake server: merged views must show it once.
            out.push(json!({
                "range": {"start": {"line": l, "character": c0}, "end": {"line": l, "character": c0 + 5}},
                "severity": 2, "message": "common finding"
            }));
        }
    }
    out
}

fn main() {
    let name = std::env::var("FAKE_NAME").unwrap_or_else(|_| "fake".into());
    eprintln!("fake server {name} started");
    let mut docs: std::collections::HashMap<String, String> = Default::default();
    let stdin = std::io::stdin();
    let mut r = std::io::BufReader::new(stdin.lock());
    loop {
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim();
            if h.is_empty() {
                break;
            }
            if let Some(n) = h.strip_prefix("Content-Length:") {
                len = n.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; len];
        if r.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(m): Result<Value, _> = serde_json::from_slice(&body) else { continue };
        let method = m["method"].as_str().unwrap_or("");
        let id = m.get("id").cloned();
        let reply = |result: Value| {
            if let Some(id) = &id {
                send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
            }
        };
        match method {
            "initialize" => {
                let mut caps = json!({
                    "textDocumentSync": 1,
                    "hoverProvider": !env("FAKE_NO_HOVER"),
                    "completionProvider": {},
                    "definitionProvider": true,
                });
                if env("FAKE_FORMAT") {
                    caps["documentFormattingProvider"] = json!(true);
                }
                if env("FAKE_PULL") {
                    caps["diagnosticProvider"] = json!({"interFileDependencies": false, "workspaceDiagnostics": false});
                }
                reply(json!({"capabilities": caps, "serverInfo": {"name": format!("fake-{name}"), "version": "0.1"}}));
            }
            "initialized" => {
                if env("FAKE_EXIT_AFTER_INIT") {
                    eprintln!("fake server {name} dying on purpose");
                    std::process::exit(3);
                }
                if env("FAKE_DYNAMIC") {
                    send(&json!({"jsonrpc": "2.0", "id": 900, "method": "client/registerCapability",
                        "params": {"registrations": [{"id": "fmt", "method": "textDocument/formatting"}]}}));
                }
                if env("FAKE_CONFIG") {
                    send(&json!({"jsonrpc": "2.0", "id": 901, "method": "workspace/configuration",
                        "params": {"items": [{"section": "fake.level"}, {"section": "nothing"}]}}));
                }
                if env("FAKE_PROGRESS") {
                    send(
                        &json!({"jsonrpc": "2.0", "id": 902, "method": "window/workDoneProgress/create", "params": {"token": "t1"}}),
                    );
                    send(
                        &json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "begin", "title": "Indexing", "percentage": 0}}}),
                    );
                    send(
                        &json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "report", "message": "1/2", "percentage": 50}}}),
                    );
                    send(
                        &json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "end"}}}),
                    );
                }
            }
            "textDocument/didOpen" | "textDocument/didChange" => {
                let uri = m["params"]["textDocument"]["uri"].as_str().unwrap_or("").to_string();
                let text = m["params"]["textDocument"]["text"]
                    .as_str()
                    .or_else(|| m["params"]["contentChanges"][0]["text"].as_str())
                    .unwrap_or("")
                    .to_string();
                if text.contains("CRASH") {
                    eprintln!("fake server {name} crashing on purpose");
                    std::process::exit(3);
                }
                docs.insert(uri.clone(), text.clone());
                if !env("FAKE_PULL") {
                    send(&json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                        "params": {"uri": uri, "diagnostics": diags(&text, &name)}}));
                }
            }
            "textDocument/didClose" => {
                let uri = m["params"]["textDocument"]["uri"].as_str().unwrap_or("");
                docs.remove(uri);
                send(&json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                    "params": {"uri": uri, "diagnostics": []}}));
            }
            "textDocument/diagnostic" => {
                let uri = m["params"]["textDocument"]["uri"].as_str().unwrap_or("");
                let text = docs.get(uri).cloned().unwrap_or_default();
                reply(json!({"kind": "full", "items": diags(&text, &name)}));
            }
            "textDocument/hover" => {
                reply(json!({"contents": {"kind": "markdown", "value": format!("hover from {name}")}}))
            }
            "textDocument/completion" => reply(json!([
                {"label": format!("{name}_item"), "kind": 3},
                {"label": "shared_item", "kind": 3}
            ])),
            "textDocument/definition" => {
                let uri = m["params"]["textDocument"]["uri"].clone();
                reply(
                    json!({"uri": uri, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}),
                );
            }
            "textDocument/formatting" => {
                let nl = if env("FAKE_FORMAT") || env("FAKE_DYNAMIC") {
                    format!("// formatted by {name}\n")
                } else {
                    String::new()
                };
                reply(
                    json!([{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": nl}]),
                );
            }
            "textDocument/codeAction" => reply(json!([{"title": format!("Fix with {name}"), "kind": "quickfix",
                "edit": {"changes": {}}}, {"title": "Shared action", "kind": "quickfix", "edit": {"changes": {}}}])),
            "shutdown" => reply(Value::Null),
            "exit" => return,
            _ => {
                // Unknown request: answer null so the client never waits; ignore notifications and replies.
                if id.is_some() && !method.is_empty() {
                    reply(Value::Null);
                } else if method.is_empty() && m.get("id").is_some() {
                    if m["id"] == 901 {
                        eprintln!("config reply: {}", m["result"]);
                    }
                }
            }
        }
    }
}
