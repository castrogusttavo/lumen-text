//! The server against a scripted stand-in for gopls: a tiny Python program speaking just enough
//! LSP to misbehave on cue (never answer `initialize`, die in the middle of a request, answer
//! slowly, publish diagnostics late). It makes the timing-sensitive guarantees testable without
//! a Go toolchain:
//!
//! * the first gopls start never blocks the server loop (requests fall back to the page's own
//!   scope analysis meanwhile);
//! * a gopls that dies mid-request costs one fallback answer, not the session;
//! * late gopls diagnostics are published without any further edit;
//! * `$/cancelRequest` interrupts a request that is waiting on gopls.
//!
//! Unix only (the stand-in is an executable script) and skipped when `python3` is missing.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{Client, WAIT, pos};
use ghp_lsp::server::rpc;
use serde_json::{Value, json};

/// Scripts are written and executed one test at a time: executing a file while another thread of
/// this process has just closed it for writing can fail with `ETXTBSY`.
static SERIAL: Mutex<()> = Mutex::new(());

const FAKE_GOPLS: &str = r#"#!/usr/bin/env python3
import json, os, sys, time

mode = os.environ.get("FAKE_GOPLS_MODE", "")
mark = os.environ.get("FAKE_GOPLS_MARK")
inp, out = sys.stdin.buffer, sys.stdout.buffer


def read():
    n = None
    while True:
        line = inp.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            if n is None:
                continue
            break
        k, _, v = line.partition(b":")
        if k.strip().lower() == b"content-length":
            n = int(v)
    return json.loads(inp.read(n))


def send(o):
    b = json.dumps(o).encode()
    out.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    out.flush()


if mode == "hang":
    # Never answers `initialize`; leaves when the server closes the pipe.
    inp.read()
    sys.exit(0)

while True:
    m = read()
    if m is None:
        break
    method = m.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {}}})
    elif method == "textDocument/didOpen" and mode == "late_diag":
        d = m["params"]["textDocument"]
        for i, line in enumerate(d["text"].split("\n")):
            c = line.find("nope")
            if c >= 0:
                time.sleep(0.7)
                send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
                    "uri": d["uri"],
                    "diagnostics": [{
                        "range": {"start": {"line": i, "character": c}, "end": {"line": i, "character": c + 4}},
                        "severity": 1,
                        "message": "undefined: nope",
                    }],
                }})
                break
    elif method == "textDocument/completion":
        if mark:
            with open(mark, "a") as f:
                f.write("x")
        if mode == "die_on_completion":
            os._exit(0)
        if mode == "slow_completion":
            time.sleep(5)
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"isIncomplete": False, "items": []}})
    elif "id" in m:
        send({"jsonrpc": "2.0", "id": m["id"], "result": None})
"#;

struct Scenario {
    dir: PathBuf,
    uri: String,
    mark: PathBuf,
    client: Client,
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn have_python() -> bool {
    Command::new("python3").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// A module on disk, the stand-in gopls in `mode`, and a client with `GHP_GOPLS` pointing at it.
fn scenario(name: &str, mode: &str) -> Scenario {
    let dir = std::env::temp_dir().join(format!("ghp-lsp-fake-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pages")).unwrap();
    std::fs::write(dir.join("go.mod"), "module demo\n\ngo 1.22\n").unwrap();
    let script = dir.join("fake-gopls");
    std::fs::write(&script, FAKE_GOPLS).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mark = dir.join("completions");
    let page = dir.join("pages/page.ghp");
    let uri = format!("file://{}", page.display());
    let client = Client::start(&[
        ("GHP_GOPLS", script.to_str().unwrap()),
        ("FAKE_GOPLS_MODE", mode),
        ("FAKE_GOPLS_MARK", mark.to_str().unwrap()),
    ]);
    Scenario { dir, uri, mark, client }
}

fn labels(v: &Value) -> Vec<String> {
    let items = v["result"]["items"].as_array().cloned().unwrap_or_default();
    items.iter().filter_map(|i| i["label"].as_str().map(str::to_owned)).collect()
}

const PAGE: &str = "<go count := 1 />\n<p><go= co /></p>\n";

fn completion_params(uri: &str) -> Value {
    json!({"textDocument": {"uri": uri}, "position": pos(1, 10)})
}

#[test]
fn a_gopls_that_never_initializes_does_not_block_the_server() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_python() {
        eprintln!("python3 not found: skipped");
        return;
    }
    let mut s = scenario("hang", "hang");
    s.client.handshake();
    s.client.open(&s.uri, PAGE);
    // Each of these would wait for gopls' `initialize` (30 s) if the start were synchronous.
    let t = Instant::now();
    for _ in 0..5 {
        let r =
            s.client.call_with_timeout("textDocument/completion", completion_params(&s.uri), Duration::from_secs(5));
        let r = r.expect("completion answered while gopls is still starting");
        assert!(labels(&r).contains(&"count".to_string()), "{r}");
    }
    let hover = json!({"textDocument": {"uri": s.uri}, "position": pos(0, 5)});
    let r = s.client.call_with_timeout("textDocument/hover", hover, Duration::from_secs(5)).expect("hover answered");
    assert!(r["result"]["contents"]["value"].as_str().unwrap_or_default().contains("count"), "{r}");
    // Edits and diagnostics keep flowing too.
    s.client.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": s.uri, "version": 2}, "contentChanges": [{"text": "<go:if a/>"}]}),
    );
    let d = s.client.wait_for("textDocument/publishDiagnostics", |p| p["version"] == 2, Duration::from_secs(5));
    assert!(d.is_some_and(|d| d["diagnostics"][0]["code"] == "unclosed-if"));
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    assert_eq!(s.client.finish(), Some(0));
}

#[test]
fn a_gopls_that_dies_mid_request_costs_one_fallback_answer() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_python() {
        eprintln!("python3 not found: skipped");
        return;
    }
    let mut s = scenario("die", "die_on_completion");
    s.client.handshake();
    s.client.open(&s.uri, PAGE);
    // Ask until the stand-in has seen (and died on) a completion request. Every answer on the
    // way is the page's own: the request that hits the dying process falls back too.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut asked = 0;
    while !s.mark.exists() {
        assert!(Instant::now() < deadline, "gopls never received a request ({asked} asked)");
        let r = s.client.call("textDocument/completion", completion_params(&s.uri));
        assert!(r.get("error").is_none(), "{r}");
        assert!(labels(&r).contains(&"count".to_string()), "request {asked}: {r}");
        asked += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    // The session is intact.
    let r = s.client.call("textDocument/completion", completion_params(&s.uri));
    assert!(labels(&r).contains(&"count".to_string()), "{r}");
    let syms = s.client.call("textDocument/documentSymbol", json!({"textDocument": {"uri": s.uri}}));
    assert!(syms["result"].is_array());
    assert_eq!(s.client.finish(), Some(0));
}

#[test]
fn late_gopls_diagnostics_are_published_without_another_edit() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_python() {
        eprintln!("python3 not found: skipped");
        return;
    }
    let mut s = scenario("late", "late_diag");
    s.client.handshake();
    s.client.open(&s.uri, "<p><go= nope /></p>\n");
    let first = s.client.wait_for("textDocument/publishDiagnostics", |_| true, WAIT).expect("initial publish");
    assert!(
        first["diagnostics"].as_array().unwrap().iter().all(|d| d["source"] != "gopls"),
        "gopls has not spoken yet: {first}"
    );
    // No didChange from here on: gopls speaks up on its own, a moment after it started.
    let late = s
        .client
        .wait_for(
            "textDocument/publishDiagnostics",
            |p| p["diagnostics"].as_array().is_some_and(|d| d.iter().any(|d| d["source"] == "gopls")),
            WAIT,
        )
        .expect("late gopls diagnostics are republished");
    let d = &late["diagnostics"][0];
    assert_eq!(d["message"], "undefined: nope");
    assert_eq!(d["range"]["start"], pos(0, 8));
    assert_eq!(d["range"]["end"], pos(0, 12));
    assert_eq!(late["uri"], s.uri);
    assert_eq!(s.client.finish(), Some(0));
}

#[test]
fn cancel_request_interrupts_a_request_waiting_on_gopls() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_python() {
        eprintln!("python3 not found: skipped");
        return;
    }
    let mut s = scenario("cancel", "slow_completion");
    s.client.handshake();
    s.client.open(&s.uri, PAGE);
    // Until gopls is up, completion answers at once (fallback). Once it is, the stand-in sits on
    // the request for 5 s: that is the one to cancel.
    let deadline = Instant::now() + Duration::from_secs(40);
    let started = loop {
        assert!(Instant::now() < deadline, "gopls never got a completion request");
        let id = s.client.fresh_id();
        s.client.send(&json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/completion",
                              "params": completion_params(&s.uri)}));
        if s.client.response(id, Duration::from_millis(400)).is_none() {
            break (id, Instant::now());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (id, sent) = started;
    s.client.notify("$/cancelRequest", json!({"id": id}));
    let r = s.client.response(id, Duration::from_secs(3)).expect("the cancelled request is answered promptly");
    assert_eq!(r["error"]["code"], rpc::REQUEST_CANCELLED, "{r}");
    assert!(sent.elapsed() < Duration::from_secs(4), "{:?}", sent.elapsed());
    // And the server is free again.
    let syms = s.client.call("textDocument/foldingRange", json!({"textDocument": {"uri": s.uri}}));
    assert!(syms["result"].is_array());
    assert_eq!(s.client.finish(), Some(0));
}
