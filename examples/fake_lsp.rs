//! A tiny language server for the editor's tests (`cargo test` builds it; it is not shipped).
//!
//! Behaviour is picked with environment variables so one binary plays every role:
//! `FAKE_NAME` (hover/diagnostic source), `FAKE_PULL=1` (pull diagnostics instead of push),
//! `FAKE_FORMAT=1` (documentFormattingProvider), `FAKE_DYNAMIC=1` (register formatting dynamically),
//! `FAKE_CONFIG=1` (ask `workspace/configuration`), `FAKE_PROGRESS=1` (`$/progress`),
//! `FAKE_EXIT_AFTER_INIT=1` (die right after `initialized`), `FAKE_NO_HOVER=1`,
//! `FAKE_HIGHLIGHT=1` (documentHighlight: every whole-word occurrence, the first as a write) or
//! `FAKE_HIGHLIGHT=first` (only the first occurrence), `FAKE_LINKED=1` (linkedEditingRange for
//! `<tag>...</tag>` names).
//! `FAKE_WORKSPACE=1` (`workspace/diagnostic` over the `*.fk` files of the root: the first report as a
//! partial result, `unchanged` for files whose `previousResultIds` still match; a file named `hang`
//! in the root keeps the request open until `$/cancelRequest`), `FAKE_PUSH_CLOSED=1` (publish
//! diagnostics for every `*.fk` file of the root right after `initialized`), `FAKE_ACTIONS=1`
//! (`source.organizeImports` with an edit, `source.fixAll` through `codeAction/resolve`),
//! `FAKE_HANG_ACTIONS=1` (never answer `source.*` code action requests).
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

fn ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c == b'-'
}

/// Byte offset of an LSP position (the test documents are ASCII, so UTF-16 columns are bytes).
fn offset(text: &str, p: &Value) -> usize {
    let (line, ch) = (p["line"].as_u64().unwrap_or(0) as usize, p["character"].as_u64().unwrap_or(0) as usize);
    let start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    (start + ch).min(text.len())
}

fn pos(text: &str, off: usize) -> Value {
    let line = text[..off].matches('\n').count();
    let col = off - text[..off].rfind('\n').map_or(0, |i| i + 1);
    json!({"line": line, "character": col})
}

fn range(text: &str, a: usize, b: usize) -> Value {
    json!({"start": pos(text, a), "end": pos(text, b)})
}

/// The identifier around byte `off`.
fn word_at(text: &str, off: usize) -> (usize, usize) {
    let b = text.as_bytes();
    let (mut s, mut e) = (off, off);
    while s > 0 && ident(b[s - 1]) {
        s -= 1;
    }
    while e < b.len() && ident(b[e]) {
        e += 1;
    }
    (s, e)
}

fn highlights(text: &str, off: usize, first_only: bool) -> Value {
    let (s, e) = word_at(text, off);
    let b = text.as_bytes();
    let mut out = Vec::new();
    if s < e {
        let word = &text[s..e];
        let mut from = 0;
        while let Some(m) = text[from..].find(word) {
            let (a, z) = (from + m, from + m + word.len());
            if (a == 0 || !ident(b[a - 1])) && (z >= b.len() || !ident(b[z])) {
                out.push(json!({"range": range(text, a, z), "kind": if out.is_empty() { 3 } else { 2 }}));
                if first_only {
                    break;
                }
            }
            from = z;
        }
    }
    json!(out)
}

fn linked(text: &str, off: usize) -> Value {
    let (s, e) = word_at(text, off);
    if s == e {
        return Value::Null;
    }
    let name = &text[s..e];
    let other = if text[..s].ends_with("</") {
        text[..s].rfind(&format!("<{name}")).map(|i| i + 1)
    } else if text[..s].ends_with('<') {
        text[e..].find(&format!("</{name}>")).map(|i| e + i + 2)
    } else {
        None
    };
    let Some(o) = other else { return Value::Null };
    let (a, b) = if o < s { ((o, o + name.len()), (s, e)) } else { ((s, e), (o, o + name.len())) };
    json!({"ranges": [range(text, a.0, a.1), range(text, b.0, b.1)], "wordPattern": "[a-zA-Z0-9:\\-\\._$]*"})
}

/// `*.fk` files of `root`, sorted, with the open document's text when it has one.
fn project_files(root: &str, docs: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    let mut paths: Vec<std::path::PathBuf> =
        rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "fk")).collect();
    paths.sort();
    for p in paths {
        let uri = format!("file://{}", p.display());
        let text = docs.get(&uri).cloned().unwrap_or_else(|| std::fs::read_to_string(&p).unwrap_or_default());
        out.push((uri, text));
    }
    out
}

/// A stable id for a text (the `resultId` of a report).
fn result_id(text: &str) -> String {
    let h = text.bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{h:x}")
}

/// One whole-document edit inserting `line` at the top of `uri`.
fn insert_top(uri: &Value, line: &str) -> Value {
    let mut changes = serde_json::Map::new();
    let edit =
        json!([{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": line}]);
    changes.insert(uri.as_str().unwrap_or("").to_string(), edit);
    json!({"changes": changes})
}

fn main() {
    let name = std::env::var("FAKE_NAME").unwrap_or_else(|_| "fake".into());
    let mut root = String::new();
    // The open workspace pull kept open by a `hang` file: its id.
    let mut hanging: Option<Value> = None;
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
                if std::env::var("FAKE_HIGHLIGHT").is_ok() {
                    caps["documentHighlightProvider"] = json!(true);
                }
                if env("FAKE_LINKED") {
                    caps["linkedEditingRangeProvider"] = json!(true);
                }
                if env("FAKE_PULL") {
                    caps["diagnosticProvider"] =
                        json!({"interFileDependencies": false, "workspaceDiagnostics": env("FAKE_WORKSPACE")});
                }
                if env("FAKE_ACTIONS") || env("FAKE_HANG_ACTIONS") {
                    caps["codeActionProvider"] = json!({"codeActionKinds": ["quickfix", "source.organizeImports", "source.fixAll"], "resolveProvider": true});
                }
                root = m["params"]["rootUri"].as_str().unwrap_or("").trim_start_matches("file://").to_string();
                reply(json!({"capabilities": caps, "serverInfo": {"name": format!("fake-{name}"), "version": "0.1"}}));
            }
            "initialized" => {
                if env("FAKE_PUSH_CLOSED") {
                    for (uri, text) in project_files(&root, &docs) {
                        send(&json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                            "params": {"uri": uri, "diagnostics": diags(&text, &name)}}));
                    }
                }
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
            "textDocument/documentHighlight" | "textDocument/linkedEditingRange" => {
                let uri = m["params"]["textDocument"]["uri"].as_str().unwrap_or("");
                let text = docs.get(uri).cloned().unwrap_or_default();
                let off = offset(&text, &m["params"]["position"]);
                if method == "textDocument/linkedEditingRange" {
                    reply(linked(&text, off));
                } else {
                    let first = std::env::var("FAKE_HIGHLIGHT").is_ok_and(|v| v == "first");
                    reply(highlights(&text, off, first));
                }
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
            "textDocument/codeAction" => {
                let only = m["params"]["context"]["only"][0].as_str().unwrap_or("");
                let uri = m["params"]["textDocument"]["uri"].clone();
                if only.starts_with("source") && env("FAKE_HANG_ACTIONS") {
                    eprintln!("ignoring {only} on purpose");
                } else if only == "source.organizeImports" {
                    reply(json!([{"title": "Organize imports", "kind": "source.organizeImports",
                        "edit": insert_top(&uri, "// imports organized\n")}]));
                } else if only == "source.fixAll" {
                    // No edit: the client resolves it.
                    reply(json!([{"title": "Fix all", "kind": "source.fixAll.fake", "data": {"uri": uri}}]));
                } else if only.is_empty() {
                    reply(json!([{"title": format!("Fix with {name}"), "kind": "quickfix",
                        "edit": {"changes": {}}}, {"title": "Shared action", "kind": "quickfix", "edit": {"changes": {}}}]));
                } else {
                    reply(json!([]));
                }
            }
            "codeAction/resolve" => {
                let mut a = m["params"].clone();
                a["edit"] = insert_top(&a["data"]["uri"], "// fixed\n");
                reply(a);
            }
            "workspace/diagnostic" => {
                let prev: std::collections::HashMap<String, String> = m["params"]["previousResultIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| Some((p["uri"].as_str()?.to_string(), p["value"].as_str()?.to_string())))
                    .collect();
                if std::path::Path::new(&root).join("hang").exists() {
                    hanging = id.clone();
                    continue;
                }
                let mut reports = Vec::new();
                let mut unchanged = 0;
                for (uri, text) in project_files(&root, &docs) {
                    let rid = result_id(&text);
                    if prev.get(&uri) == Some(&rid) {
                        unchanged += 1;
                        reports.push(json!({"uri": uri, "kind": "unchanged", "resultId": rid, "version": null}));
                    } else {
                        reports.push(json!({"uri": uri, "kind": "full", "resultId": rid, "version": null, "items": diags(&text, &name)}));
                    }
                }
                eprintln!("workspace pull: unchanged {unchanged}");
                let token = m["params"]["partialResultToken"].clone();
                if !token.is_null() && !reports.is_empty() {
                    let first = reports.remove(0);
                    send(
                        &json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": token, "value": {"items": [first]}}}),
                    );
                }
                reply(json!({"items": reports}));
            }
            "$/cancelRequest" => {
                if hanging.as_ref() == Some(&m["params"]["id"]) {
                    eprintln!("cancelled workspace pull");
                    send(
                        &json!({"jsonrpc": "2.0", "id": hanging.take(), "error": {"code": -32800, "message": "cancelled"}}),
                    );
                }
            }
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
