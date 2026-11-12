//! A scripted LSP client driving the real `ghp-lsp` binary over stdio, shared by the end-to-end
//! tests that need more control than `server_e2e.rs` (environment, raw bytes, timeouts, stderr).

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ghp_lsp::server::rpc;
use serde_json::{Value, json};

/// Default wait for a message the server owes us.
pub const WAIT: Duration = Duration::from_secs(30);

pub struct Client {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    backlog: VecDeque<Value>,
    stderr: Arc<Mutex<Vec<u8>>>,
    next_id: i64,
}

impl Client {
    /// Start `ghp-lsp` with extra environment variables.
    pub fn start(env: &[(&str, &str)]) -> Client {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ghp-lsp"));
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn ghp-lsp");
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
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = stderr.clone();
        let mut err = child.stderr.take().unwrap();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Client { child, stdin: Some(stdin), rx, backlog: VecDeque::new(), stderr, next_id: 100 }
    }

    /// Everything the server wrote to stderr so far.
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    pub fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).expect("write to ghp-lsp");
        stdin.flush().expect("flush to ghp-lsp");
    }

    pub fn send(&mut self, msg: &Value) {
        let body = serde_json::to_vec(msg).unwrap();
        self.send_frame(&body);
    }

    /// One `Content-Length` frame around arbitrary bytes.
    pub fn send_frame(&mut self, body: &[u8]) {
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(body);
        self.send_raw(&frame);
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&rpc::notification(method, params));
    }

    /// The next message from the server, or `None` after `timeout`.
    pub fn next_message(&mut self, timeout: Duration) -> Option<Value> {
        if let Some(m) = self.backlog.pop_front() {
            return Some(m);
        }
        self.rx.recv_timeout(timeout).ok()
    }

    /// Send a request and return the complete response (`result` or `error`). Messages that
    /// arrive meanwhile stay available to [`Client::wait_for`].
    pub fn call_with_timeout(&mut self, method: &str, params: Value, timeout: Duration) -> Option<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.response(id, timeout)
    }

    pub fn call(&mut self, method: &str, params: Value) -> Value {
        self.call_with_timeout(method, params, WAIT).unwrap_or_else(|| {
            let status = self.child.try_wait();
            panic!("no response to {method}; server status {status:?}; stderr:\n{}", self.stderr_text())
        })
    }

    /// Wait for the response with this id.
    pub fn response(&mut self, id: i64, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        let mut skipped = Vec::new();
        let found = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(msg) = self.next_message(left) else { break None };
            if msg["id"] == json!(id) && msg.get("method").is_none() {
                break Some(msg);
            }
            skipped.push(msg);
            if left.is_zero() {
                break None;
            }
        };
        for m in skipped.into_iter().rev() {
            self.backlog.push_front(m);
        }
        found
    }

    /// Id of the next request [`Client::call`] will send plus one: for hand-sent requests.
    pub fn fresh_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// Wait for a notification of `method` whose params satisfy `pred`.
    pub fn wait_for(&mut self, method: &str, pred: impl Fn(&Value) -> bool, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = self.next_message(left)?;
            if msg["method"] == method && pred(&msg["params"]) {
                return Some(msg["params"].clone());
            }
            if left.is_zero() {
                return None;
            }
        }
    }

    /// `initialize` + `initialized`.
    pub fn handshake(&mut self) -> Value {
        let init = self.call("initialize", json!({"processId": null, "rootUri": null, "capabilities": {}}));
        self.notify("initialized", json!({}));
        init["result"].clone()
    }

    pub fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "ghp", "version": 1, "text": text}}),
        );
    }

    /// Clean shutdown; returns the exit code.
    pub fn finish(&mut self) -> Option<i32> {
        let r = self.call("shutdown", Value::Null);
        assert!(r.get("error").is_none(), "shutdown failed: {r}");
        self.notify("exit", Value::Null);
        self.stdin = None;
        self.child.wait().ok().and_then(|s| s.code())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn pos(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}
