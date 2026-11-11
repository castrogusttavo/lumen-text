//! JSON-RPC 2.0 framed as MCP (spec 2025-03-26), the way Claude Code talks to an IDE. The
//! handshake (`initialize`, `tools/list`, `ping`) is answered right on the connection's thread;
//! `tools/call` needs the editor's state and goes to the UI thread, which answers when it can
//! (`openDiff` only once the user decides). Unknown methods get "method not found"; unknown
//! notifications are passed on (and logged by the UI).

use serde_json::{Value, json};

/// The MCP revision we speak when the client does not say.
pub const PROTOCOL: &str = "2025-03-26";

/// What to do with one incoming message.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// Answer at once with this (a whole JSON-RPC response).
    Reply(Value),
    /// A tool call for the UI thread.
    Call { id: Value, name: String, args: Value },
    /// A notification (no answer).
    Note { method: String, params: Value },
    /// A response to something we never asked, or junk without an id.
    Ignore,
}

pub fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A tool result made of text items.
pub fn texts(items: &[&str]) -> Value {
    let content: Vec<Value> = items.iter().map(|t| json!({ "type": "text", "text": t })).collect();
    json!({ "content": content })
}

pub fn text(s: &str) -> Value {
    texts(&[s])
}

/// A tool result whose text is `v` as JSON (what the VS Code extension returns).
pub fn json_text(v: &Value) -> Value {
    text(&v.to_string())
}

/// A failed tool call (MCP `isError`).
pub fn failure(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
}

/// Route one message (a batch is split by the caller).
pub fn route(msg: &Value) -> Route {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").filter(|v| !v.is_null()).cloned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match (method, id) {
        (None, _) => Route::Ignore,
        (Some(m), None) => Route::Note { method: m.to_string(), params },
        (Some("initialize"), Some(id)) => {
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
            Route::Reply(reply(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": { "name": "lumen-text", "version": env!("CARGO_PKG_VERSION") },
                }),
            ))
        }
        (Some("ping"), Some(id)) => Route::Reply(reply(&id, json!({}))),
        (Some("tools/list"), Some(id)) => Route::Reply(reply(&id, json!({ "tools": tools() }))),
        (Some("prompts/list"), Some(id)) => Route::Reply(reply(&id, json!({ "prompts": [] }))),
        (Some("resources/list"), Some(id)) => Route::Reply(reply(&id, json!({ "resources": [] }))),
        (Some("tools/call"), Some(id)) => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            if name.is_empty() {
                return Route::Reply(error(&id, -32602, "tools/call needs a tool name"));
            }
            Route::Call { id, name, args }
        }
        (Some(_), Some(id)) => Route::Reply(error(&id, -32601, "Method not found")),
    }
}

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
    })
}

/// The tools Lumen offers (the VS Code set, minus Jupyter's `executeCode`).
pub fn tools() -> Value {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let b = |d: &str| json!({ "type": "boolean", "description": d });
    let path = || json!({ "filePath": s("Absolute path of the file") });
    Value::Array(vec![
        tool(
            "openFile",
            "Open a file in the editor and optionally select a range of text",
            json!({
                "filePath": s("Path to the file to open"),
                "preview": b("Open in preview mode"),
                "startText": s("Text that starts the selection"),
                "endText": s("Text that ends the selection"),
                "selectToEndOfLine": b("Extend the selection to the end of the line"),
                "makeFrontmost": b("Make the file the active editor tab"),
            }),
            &["filePath"],
        ),
        tool(
            "openDiff",
            "Open a diff of a file against new contents; blocks until the user accepts or rejects it",
            json!({
                "old_file_path": s("Path of the file as it is"),
                "new_file_path": s("Path the new contents belong to"),
                "new_file_contents": s("The proposed contents"),
                "tab_name": s("Name of the diff tab"),
            }),
            &["old_file_path", "new_file_path", "new_file_contents", "tab_name"],
        ),
        tool("getCurrentSelection", "Get the current text selection in the active editor", json!({}), &[]),
        tool(
            "getLatestSelection",
            "Get the most recent text selection (even if not in the active editor)",
            json!({}),
            &[],
        ),
        tool("getOpenEditors", "Get information about currently open editors", json!({}), &[]),
        tool("getWorkspaceFolders", "Get all workspace folders currently open in the IDE", json!({}), &[]),
        tool(
            "getDiagnostics",
            "Get language diagnostics (language servers and the last build) for a file or for all files",
            json!({ "uri": s("File URI; all files when omitted") }),
            &[],
        ),
        tool("checkDocumentDirty", "Check if a document has unsaved changes", path(), &["filePath"]),
        tool("saveDocument", "Save a document with unsaved changes", path(), &["filePath"]),
        tool(
            "close_tab",
            "Close a diff tab by name",
            json!({ "tab_name": s("The tab_name given to openDiff") }),
            &["tab_name"],
        ),
        tool("closeAllDiffTabs", "Close all diff tabs", json!({}), &[]),
    ])
}
