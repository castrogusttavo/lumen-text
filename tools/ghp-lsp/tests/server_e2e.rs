//! End-to-end: drive the real `ghp-lsp` binary over stdio with scripted LSP messages.

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use ghp_lsp::server::rpc;
use serde_json::{Value, json};

const URI: &str = "file:///demo/page.ghp";
const WAIT: Duration = Duration::from_secs(20);

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    next_id: i64,
}

impl Client {
    fn start() -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ghp-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn ghp-lsp");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(msg)) = rpc::read_message(&mut stdout) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        Client { child, stdin: Some(stdin), rx, next_id: 0 }
    }

    fn send(&mut self, msg: Value) {
        rpc::write_message(self.stdin.as_mut().unwrap(), &msg).unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(rpc::notification(method, params));
    }

    /// Send a request and wait for its response, skipping notifications.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self.rx.recv_timeout(WAIT).unwrap_or_else(|_| panic!("no response to {method}"));
            if msg["id"] == json!(id) {
                assert!(msg.get("error").is_none(), "{method} failed: {msg}");
                return msg["result"].clone();
            }
        }
    }

    /// Wait for a notification of `method` matching `pred`.
    fn wait_for(&mut self, method: &str, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            let msg = self.rx.recv_timeout(WAIT).unwrap_or_else(|_| panic!("no {method} notification"));
            if msg["method"] == method && pred(&msg["params"]) {
                return msg["params"].clone();
            }
        }
    }
}

fn pos(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

#[test]
fn full_session() {
    let mut c = Client::start();

    // initialize
    let init = c.request("initialize", json!({"processId": null, "rootUri": null, "capabilities": {}}));
    let caps = &init["capabilities"];
    assert_eq!(caps["textDocumentSync"]["change"], 2);
    assert!(caps["completionProvider"]["triggerCharacters"].as_array().unwrap().contains(&json!("<")));
    for k in [
        "hoverProvider",
        "documentSymbolProvider",
        "foldingRangeProvider",
        "selectionRangeProvider",
        "documentFormattingProvider",
        "documentHighlightProvider",
        "codeActionProvider",
        "semanticTokensProvider",
    ] {
        assert!(!caps[k].is_null(), "missing capability {k}");
    }
    c.notify("initialized", json!({}));

    // open a broken document: diagnostics arrive right away
    let text = "<ul>\n  <go:for x := range xs/>\n    <li><go= x /></li>\n</ul>\n";
    c.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": URI, "languageId": "ghp", "version": 1, "text": text}}),
    );
    let diags = c.wait_for("textDocument/publishDiagnostics", |p| p["version"] == 1);
    assert_eq!(diags["uri"], URI);
    assert_eq!(diags["diagnostics"][0]["code"], "unclosed-for");
    assert_eq!(diags["diagnostics"][0]["range"]["start"], pos(1, 2));

    // quick fix inserts the closer
    let actions = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": URI}, "range": {"start": pos(1, 4), "end": pos(1, 4)},
               "context": {"diagnostics": diags["diagnostics"]}}),
    );
    let fix = &actions[0];
    assert_eq!(fix["kind"], "quickfix");
    let edit = fix["edit"]["changes"][URI][0].clone();

    // apply it as an incremental change
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 2},
               "contentChanges": [{"range": edit["range"], "text": edit["newText"]}]}),
    );
    let diags = c.wait_for("textDocument/publishDiagnostics", |p| p["version"] == 2);
    assert_eq!(diags["diagnostics"], json!([]), "the fix must clear the diagnostic");

    // completion of a tag, with an auto-closing snippet
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 3},
               "contentChanges": [{"range": {"start": pos(5, 0), "end": pos(5, 0)}, "text": "<go:i"}]}),
    );
    let list = c.request("textDocument/completion", json!({"textDocument": {"uri": URI}, "position": pos(5, 5)}));
    let item = &list["items"][0];
    assert_eq!(item["label"], "<go:if");
    assert!(item["textEdit"]["newText"].as_str().unwrap().contains("<go:endif/>"));
    assert_eq!(item["textEdit"]["range"], json!({"start": pos(5, 0), "end": pos(5, 5)}));

    // remove the partial tag again, then query the structure
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 4},
               "contentChanges": [{"range": {"start": pos(5, 0), "end": pos(5, 5)}, "text": ""}]}),
    );
    let syms = c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": URI}}));
    assert_eq!(syms[0]["name"], "for x := range xs");

    let folds = c.request("textDocument/foldingRange", json!({"textDocument": {"uri": URI}}));
    assert!(!folds.as_array().unwrap().is_empty());

    let hover = c.request("textDocument/hover", json!({"textDocument": {"uri": URI}, "position": pos(1, 5)}));
    assert!(hover["contents"]["value"].as_str().unwrap().contains("go:for"));

    let hl = c.request("textDocument/documentHighlight", json!({"textDocument": {"uri": URI}, "position": pos(1, 5)}));
    assert_eq!(hl.as_array().unwrap().len(), 2);

    let fmt = c.request(
        "textDocument/formatting",
        json!({"textDocument": {"uri": URI}, "options": {"tabSize": 4, "insertSpaces": true}}),
    );
    // The body of the `for` moves to one level (4) below the `for`, which sits at column 2.
    let first = &fmt[0];
    assert_eq!(first["range"]["start"]["line"], 2);
    assert_eq!(first["newText"], "      ");

    let tokens = c.request("textDocument/semanticTokens/full", json!({"textDocument": {"uri": URI}}));
    assert!(!tokens["data"].as_array().unwrap().is_empty());

    // a request inside a Go expression reaches the router (scope fallback without gopls)
    let go = c.request("textDocument/hover", json!({"textDocument": {"uri": URI}, "position": pos(2, 13)}));
    assert!(go["contents"]["value"].as_str().unwrap().contains("x := range xs"), "{go}");

    // an unknown method is an error, not a crash
    c.send(json!({"jsonrpc": "2.0", "id": 99, "method": "nope/never", "params": {}}));
    loop {
        let msg = c.rx.recv_timeout(WAIT).expect("error reply");
        if msg["id"] == json!(99) {
            assert_eq!(msg["error"]["code"], rpc::METHOD_NOT_FOUND);
            break;
        }
    }

    // clean shutdown
    assert_eq!(c.request("shutdown", Value::Null), Value::Null);
    c.notify("exit", Value::Null);
    c.stdin.as_mut().unwrap().flush().unwrap();
    let status = c.child.wait().unwrap();
    assert!(status.success(), "exit status {status:?}");
}

#[test]
fn exit_without_shutdown_fails() {
    let mut c = Client::start();
    c.request("initialize", json!({"capabilities": {}}));
    c.notify("exit", Value::Null);
    let status = c.child.wait().unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn closing_stdin_ends_the_server() {
    let mut c = Client::start();
    c.request("initialize", json!({"capabilities": {}}));
    c.stdin = None;
    let status = c.child.wait().unwrap();
    assert_eq!(status.code(), Some(1));
}
