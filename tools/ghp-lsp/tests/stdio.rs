//! Robustness of the real binary over stdio: requests before `initialize`, invalid and hostile
//! framing, partial writes, huge messages, CRLF and surrogate-pair edits, and a deterministic
//! fuzz of whole sessions. The server must answer or ignore, never die or hang, and never panic
//! (a panic that was caught at the dispatch boundary still prints to stderr, which is checked).

mod common;

use std::time::Duration;

use common::{Client, WAIT, pos};
use ghp_lsp::server::rpc;
use serde_json::{Value, json};

const URI: &str = "file:///stdio/page.ghp";

/// Gopls is not what these tests are about: keep the machine's one (if any) out of it.
const ENV: &[(&str, &str)] = &[("GHP_LSP_NO_GOPLS", "1")];

fn start() -> Client {
    Client::start(ENV)
}

#[test]
fn requests_before_initialize_are_refused_and_notifications_ignored() {
    let mut c = start();
    for method in ["textDocument/hover", "textDocument/completion", "textDocument/documentSymbol", "shutdown", "nope"] {
        let r = c.call(method, json!({"textDocument": {"uri": URI}, "position": pos(0, 0)}));
        assert_eq!(r["error"]["code"], rpc::SERVER_NOT_INITIALIZED, "{method}: {r}");
    }
    c.open(URI, "<go:if a/>");
    assert!(c.wait_for("textDocument/publishDiagnostics", |_| true, Duration::from_millis(400)).is_none());
    // The session is still good.
    c.handshake();
    c.open(URI, "<go:if a/>");
    let d = c.wait_for("textDocument/publishDiagnostics", |_| true, WAIT).expect("diagnostics after initialize");
    assert_eq!(d["diagnostics"][0]["code"], "unclosed-if");
    assert_eq!(c.finish(), Some(0));
}

#[test]
fn exit_before_initialize_is_a_failure() {
    let mut c = start();
    c.notify("exit", Value::Null);
    assert_eq!(c.child.wait().unwrap().code(), Some(1));
}

#[test]
fn invalid_json_and_non_utf8_bodies_get_an_error_and_the_session_goes_on() {
    let mut c = start();
    c.handshake();
    for body in [
        &b"{not json"[..],
        b"\"\xff\xfe\"",
        b"",
        b"\x00\x00",
        b"[1,2,3]",
        b"123",
        b"null",
        b"{}",
        b"{\"jsonrpc\":\"2.0\"}",
    ] {
        c.send_frame(body);
        let m = c.next_message(WAIT).expect("an error reply");
        assert_eq!(m["error"]["code"], rpc::INVALID_REQUEST, "{:?} -> {m}", String::from_utf8_lossy(body));
        assert_eq!(m["id"], Value::Null);
    }
    // A header that is not UTF-8, an unparsable length, and extra headers around a good frame.
    c.send_raw(b"X-Garbage: \xff\xfe\r\nContent-Length: nope\r\n\r\n{}\r\n");
    c.send_raw(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: 2\r\n\r\n{}");
    let m = c.next_message(WAIT).expect("an error reply for the empty object");
    assert_eq!(m["error"]["code"], rpc::INVALID_REQUEST);
    let r = c.call("textDocument/documentSymbol", json!({"textDocument": {"uri": URI}}));
    assert_eq!(r["result"], Value::Null);
    assert_eq!(c.finish(), Some(0));
}

#[test]
fn frames_that_arrive_in_pieces_even_inside_a_utf8_character() {
    let mut c = start();
    c.handshake();
    let text = "<p>é😀日本</p>\n<go:if a/>\n";
    let msg = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": {"textDocument": {"uri": URI, "languageId": "ghp", "version": 1, "text": text}}});
    let body = serde_json::to_vec(&msg).unwrap();
    // `serde_json` leaves the characters as UTF-8, so a cut at every other byte hits their middles.
    let frame = [format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes(), body].concat();
    for piece in frame.chunks(7) {
        c.send_raw(piece);
        std::thread::sleep(Duration::from_millis(2));
    }
    let d = c.wait_for("textDocument/publishDiagnostics", |_| true, WAIT).expect("diagnostics");
    assert_eq!(d["diagnostics"][0]["code"], "unclosed-if");
    assert_eq!(d["diagnostics"][0]["range"]["start"], pos(1, 0));
    assert_eq!(c.finish(), Some(0));
}

#[test]
fn an_absurd_content_length_is_neither_allocated_nor_waited_for() {
    let mut c = start();
    c.handshake();
    c.send_raw(b"Content-Length: 99999999999999\r\n\r\n{\"jsonrpc\":");
    c.stdin = None;
    let status = c.child.wait().unwrap();
    // End of input in the middle of a message, without a `shutdown`: the usual failure code.
    assert_eq!(status.code(), Some(1));
}

#[test]
fn a_twelve_megabyte_message_is_fine() {
    let mut c = start();
    c.handshake();
    let mut text = "<p>row</p>\n".repeat(12 * 1024 * 1024 / 11);
    text.push_str("<go:if a/>\n");
    c.open(URI, &text);
    let d = c.wait_for("textDocument/publishDiagnostics", |_| true, Duration::from_secs(120)).expect("diagnostics");
    assert_eq!(d["diagnostics"][0]["code"], "unclosed-if");
    let r = c.call("textDocument/foldingRange", json!({"textDocument": {"uri": URI}}));
    assert!(r["result"].is_array());
    assert_eq!(c.finish(), Some(0));
}

#[test]
fn crlf_documents_and_surrogate_pair_edits_end_to_end() {
    let mut c = start();
    c.handshake();
    c.open(URI, "<ul>\r\n  <go:for x := range xs/>\r\n    😀<go= x />\r\n  <go:endfor/>\r\n</ul>\r\n");
    c.wait_for("textDocument/publishDiagnostics", |_| true, WAIT).unwrap();
    // Insert after the emoji (UTF-16 column 6 on line 2), then break the line with CRLF.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 2}, "contentChanges": [
            {"range": {"start": pos(2, 6), "end": pos(2, 6)}, "text": "é"},
            {"range": {"start": pos(2, 3), "end": pos(2, 3)}, "text": "\r\n"},
        ]}),
    );
    let syms = c.call("textDocument/documentSymbol", json!({"textDocument": {"uri": URI}}));
    let sym = &syms["result"][0];
    assert_eq!(sym["name"], "for x := range xs");
    assert_eq!(sym["range"]["start"], pos(1, 2));
    // The `endfor` closer moved down one line with the inserted break.
    assert_eq!(sym["range"]["end"], pos(4, 14));
    // A position in the middle of the emoji (UTF-16 column 2 of line 3) is rounded, not fatal.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 3}, "contentChanges": [
            {"range": {"start": pos(3, 2), "end": pos(3, 2)}, "text": "!"},
        ]}),
    );
    let r = c.call("textDocument/foldingRange", json!({"textDocument": {"uri": URI}}));
    assert!(r["result"].is_array());
    assert_eq!(c.finish(), Some(0));
}

// -------------------------------------------------------------------------------------------
// Fuzz
// -------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.step() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const METHODS: &[&str] = &[
    "textDocument/didOpen",
    "textDocument/didChange",
    "textDocument/didClose",
    "textDocument/hover",
    "textDocument/completion",
    "textDocument/definition",
    "textDocument/documentSymbol",
    "textDocument/foldingRange",
    "textDocument/selectionRange",
    "textDocument/formatting",
    "textDocument/rangeFormatting",
    "textDocument/documentHighlight",
    "textDocument/semanticTokens/full",
    "textDocument/semanticTokens/full/delta",
    "textDocument/semanticTokens/range",
    "textDocument/codeAction",
    "$/cancelRequest",
    "$/setTrace",
    "workspace/didChangeConfiguration",
    "nope/nope",
    "initialized",
];

const TEXTS: &[&str] = &[
    "",
    "<go:if a/>",
    "<go:endif/>",
    "<go= x />",
    "<go= />",
    "<go />",
    "<go:for i := range xs/>\r\n<go= i />\r\n<go:endfor/>",
    "<a href=\"<go= u />\">😀é日本</a>",
    "<go= \"unterminated",
    "<go:import (\"fmt\", m \"x/y\")/>\n<go= fmt.Sprint(m.Z) />",
    "<go:switch v/><go:case 1/>a<go:default/>b<go:endswitch/>",
    "<<<<<<<<<<<<",
    "<go= `raw /> string` />",
];

fn small_int(r: &mut Rng) -> Value {
    match r.below(8) {
        0 => json!(0),
        1 => json!(1),
        2 => json!(3),
        3 => json!(40),
        4 => json!(u32::MAX),
        5 => json!(u64::MAX),
        6 => json!(-1),
        _ => json!(r.below(30)),
    }
}

fn random_pos(r: &mut Rng) -> Value {
    match r.below(10) {
        0 => Value::Null,
        1 => json!({"line": "x", "character": 1}),
        2 => json!({"line": 1}),
        _ => json!({"line": small_int(r), "character": small_int(r)}),
    }
}

fn random_range(r: &mut Rng) -> Value {
    match r.below(10) {
        0 => json!(7),
        1 => json!({"start": random_pos(r)}),
        _ => json!({"start": random_pos(r), "end": random_pos(r)}),
    }
}

fn random_json(r: &mut Rng, depth: usize) -> Value {
    match r.below(if depth == 0 { 5 } else { 8 }) {
        0 => Value::Null,
        1 => json!(r.below(100) as i64 - 50),
        2 => json!(r.pick(TEXTS)),
        3 => json!(r.below(2) == 0),
        4 => json!(1.5),
        5 => Value::Array((0..r.below(4)).map(|_| random_json(r, depth - 1)).collect()),
        _ => {
            let keys = ["textDocument", "position", "range", "text", "uri", "id", "context", "options"];
            Value::Object((0..r.below(4)).map(|_| (r.pick(&keys).to_string(), random_json(r, depth - 1))).collect())
        }
    }
}

fn random_params(r: &mut Rng) -> Value {
    if r.below(5) == 0 {
        return random_json(r, 3);
    }
    let uri = if r.below(6) == 0 { json!(r.pick(TEXTS)) } else { json!(URI) };
    let mut p =
        json!({"textDocument": {"uri": uri, "version": r.below(5), "languageId": "ghp", "text": r.pick(TEXTS)}});
    if r.below(2) == 0 {
        p["position"] = random_pos(r);
    }
    if r.below(2) == 0 {
        p["range"] = random_range(r);
    }
    if r.below(3) == 0 {
        p["positions"] = Value::Array((0..r.below(4)).map(|_| random_pos(r)).collect());
    }
    if r.below(2) == 0 {
        let changes: Vec<Value> = (0..r.below(4))
            .map(|_| match r.below(6) {
                0 => json!({"text": r.pick(TEXTS)}),
                1 => json!({"range": random_range(r)}),
                2 => random_json(r, 1),
                _ => json!({"range": random_range(r), "text": r.pick(TEXTS)}),
            })
            .collect();
        p["contentChanges"] = Value::Array(changes);
    }
    if r.below(3) == 0 {
        p["options"] = json!({"tabSize": small_int(r), "insertSpaces": r.below(2) == 0});
    }
    if r.below(3) == 0 {
        p["context"] = json!({"diagnostics": [random_json(r, 2)], "triggerKind": small_int(r)});
    }
    if r.below(4) == 0 {
        p["previousResultId"] = json!(r.below(6).to_string());
    }
    if r.below(5) == 0 {
        p["id"] = json!(r.below(40));
    }
    p
}

fn fuzz_frame(r: &mut Rng, c: &mut Client) {
    match r.below(12) {
        0 => {
            // Raw bytes inside a well-formed frame.
            let body: Vec<u8> = (0..r.below(120)).map(|_| r.step() as u8).collect();
            c.send_frame(&body);
        }
        1 => c.send_frame(&serde_json::to_vec(&random_json(r, 3)).unwrap()),
        2 => c.send_raw(b"X-Garbage: \xff\r\nContent-Length: nope\r\n\r\n{}\r\n"),
        3 => c.send_frame(b"{\"jsonrpc\":\"2.0\",\"method\":"),
        4 | 5 => {
            // A notification.
            let m = *r.pick(METHODS);
            c.send(&json!({"jsonrpc": "2.0", "method": m, "params": random_params(r)}));
        }
        _ => {
            // A request, with ids of every JSON type.
            let id = match r.below(6) {
                0 => json!("s"),
                1 => Value::Null,
                2 => json!(1.5),
                3 => json!({"a": 1}),
                _ => json!(1000 + r.below(1000)),
            };
            let m = *r.pick(METHODS);
            c.send(&json!({"jsonrpc": "2.0", "id": id, "method": m, "params": random_params(r)}));
        }
    }
}

#[test]
fn fuzzed_sessions_never_kill_or_wedge_the_server() {
    for seed in [0x9E37_79B9_7F4A_7C15u64, 0xD1B5_4A32_D192_ED03, 0x2545_F491_4F6C_DD1D] {
        let mut r = Rng(seed);
        let mut c = start();
        // Garbage before `initialize`, then the handshake, then garbage with documents open.
        for _ in 0..150 {
            fuzz_frame(&mut r, &mut c);
        }
        c.handshake();
        c.open(URI, "<go:for i := range xs/>\n<go= i />\n<go:endfor/>\n<go= />\n");
        for _ in 0..600 {
            fuzz_frame(&mut r, &mut c);
        }
        // Still alive and still correct after all of it.
        c.open("file:///stdio/after.ghp", "<go:if a/>\n");
        let d = c
            .wait_for("textDocument/publishDiagnostics", |p| p["uri"] == "file:///stdio/after.ghp", WAIT)
            .unwrap_or_else(|| panic!("seed {seed:#x}: no diagnostics after the fuzz\n{}", c.stderr_text()));
        assert_eq!(d["diagnostics"][0]["code"], "unclosed-if");
        let r = c.call("textDocument/documentSymbol", json!({"textDocument": {"uri": "file:///stdio/after.ghp"}}));
        assert_eq!(r["result"][0]["name"], "if a", "seed {seed:#x}: {r}");
        let code = c.finish();
        let stderr = c.stderr_text();
        assert_eq!(code, Some(0), "seed {seed:#x}\n{stderr}");
        assert!(!stderr.contains("panicked"), "seed {seed:#x} hit a panic:\n{stderr}");
    }
}
