//! DAP client tests: a scripted in-memory adapter (no external binary), and one `#[ignore]`d
//! end-to-end run against a real `dlv dap` (CI job "Debug adapters (delve)").

use crate::dap::{Dap, DapMsg, FrameReader, Session, State};
use crate::dap_adapters::Registry;
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

struct PipeR(Receiver<Vec<u8>>, Vec<u8>);
impl Read for PipeR {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(b) => self.1 = b,
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.1.len());
        out[..n].copy_from_slice(&self.1[..n]);
        self.1.drain(..n);
        Ok(n)
    }
}
struct PipeW(Sender<Vec<u8>>);
impl Write for PipeW {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.send(b.to_vec()).map_err(|_| io::ErrorKind::BrokenPipe)?;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn framed(v: &Value) -> Vec<u8> {
    let b = v.to_string();
    format!("Content-Length: {}\r\n\r\n{b}", b.len()).into_bytes()
}

/// A fake adapter: answers each request like a tiny debugger stopped on line 2 of /w/main.go.
fn fake_adapter(from_client: Receiver<Vec<u8>>, to_client: Sender<Vec<u8>>, log: Sender<String>) {
    thread::spawn(move || {
        let mut fr = FrameReader::default();
        let send = |v: Value| {
            let _ = to_client.send(framed(&v));
        };
        while let Ok(chunk) = from_client.recv() {
            for req in fr.push(&chunk) {
                let cmd = req["command"].as_str().unwrap_or("").to_string();
                let _ = log.send(cmd.clone());
                let ok = |body: Value| json!({"seq": 1000, "type": "response", "request_seq": req["seq"], "success": true, "command": cmd, "body": body});
                let ev = |e: &str, body: Value| json!({"seq": 1, "type": "event", "event": e, "body": body});
                match cmd.as_str() {
                    "initialize" => send(ok(json!({"supportsConfigurationDoneRequest": true}))),
                    "launch" => {
                        send(ok(json!({})));
                        send(ev("initialized", json!({})));
                    }
                    "setBreakpoints" => {
                        let n = req["arguments"]["breakpoints"].as_array().map_or(0, Vec::len);
                        send(ok(json!({"breakpoints": vec![json!({"verified": true}); n]})));
                    }
                    "configurationDone" => {
                        send(ok(json!({})));
                        send(ev("output", json!({"category": "stdout", "output": "hello\n"})));
                        send(ev("stopped", json!({"reason": "breakpoint", "threadId": 1})));
                    }
                    "threads" => send(ok(json!({"threads": [{"id": 1, "name": "main"}]}))),
                    "stackTrace" => send(ok(json!({"stackFrames": [{"id": 7, "name": "main.add",
                        "source": {"path": "/w/main.go"}, "line": 2, "column": 1}]}))),
                    "scopes" => send(ok(json!({"scopes": [{"name": "Locals", "variablesReference": 9}]}))),
                    "variables" => send(ok(json!({"variables": [
                        {"name": "a", "value": "1", "type": "int", "variablesReference": 0}]}))),
                    "evaluate" => send(ok(json!({"result": "3", "variablesReference": 0}))),
                    "continue" => {
                        send(ok(json!({"allThreadsContinued": true})));
                        send(ev("exited", json!({"exitCode": 0})));
                        send(ev("terminated", json!({})));
                    }
                    "disconnect" => send(ok(json!({}))),
                    _ => {}
                }
            }
        }
    });
}

fn pump(dap: &mut Dap, s: &mut Session, until: impl Fn(&Session) -> bool, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end && !until(s) {
        if let Ok(m) = dap.rx.recv_timeout(Duration::from_millis(50)) {
            s.handle(&m, dap);
        }
    }
    until(s)
}

#[test]
fn fake_adapter_full_flow() {
    let (c2a_tx, c2a_rx) = channel();
    let (a2c_tx, a2c_rx) = channel();
    let (log_tx, log_rx) = channel();
    fake_adapter(c2a_rx, a2c_tx, log_tx);
    let mut dap = Dap::from_io(PipeR(a2c_rx, Vec::new()), PipeW(c2a_tx), Arc::new(|| {}));
    dap.adapter_id = "go".into();

    let reg = Registry::builtin();
    let mut s = reg.get("delve").unwrap().session(Path::new("/w/main.go"), Path::new("/w"));
    s.set_breakpoints("/w/main.go", &[2], &mut dap);
    s.start(&mut dap);

    assert!(pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. }) && !s.locals.is_empty(), 5));
    assert_eq!(s.last_stop.as_ref().unwrap().reason, "breakpoint");
    assert_eq!(s.frames[0].line, 2);
    assert_eq!(s.frames[0].path.as_deref(), Some("/w/main.go"));
    assert_eq!(s.locals[0].name, "a");
    assert!(s.output.iter().any(|l| l == "hello"));

    // evaluate: the response is correlated back by request seq.
    let seq = dap.evaluate("a+2", 7);
    let end = Instant::now() + Duration::from_secs(5);
    let mut result = None;
    while result.is_none() && Instant::now() < end {
        if let Ok(DapMsg::Response { seq_for, body, .. }) = dap.rx.recv_timeout(Duration::from_millis(50))
            && seq_for == seq
        {
            result = body["result"].as_str().map(str::to_string);
        }
    }
    assert_eq!(result.as_deref(), Some("3"));

    s.resume(&mut dap);
    assert!(pump(&mut dap, &mut s, |s| s.state == State::Terminated, 5));
    dap.disconnect(true);

    let cmds: Vec<String> = log_rx.try_iter().collect();
    let pos = |c: &str| cmds.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c} not sent: {cmds:?}"));
    assert!(pos("initialize") < pos("launch"));
    assert!(pos("setBreakpoints") < pos("configurationDone"));
    assert!(!cmds.iter().any(|c| c == "setExceptionBreakpoints"), "delve session must not send it");
}

#[test]
fn adapter_exit_terminates_session() {
    let (c2a_tx, _c2a_rx) = channel::<Vec<u8>>();
    let (a2c_tx, a2c_rx) = channel::<Vec<u8>>();
    let mut dap = Dap::from_io(PipeR(a2c_rx, Vec::new()), PipeW(c2a_tx), Arc::new(|| {}));
    let mut s = Session::new("x", &[], Path::new("."), false);
    drop(a2c_tx); // adapter stdout closes
    assert!(pump(&mut dap, &mut s, |s| s.state == State::Terminated, 5));
}

/// Real delve: breakpoint hit + variable read on a tiny Go program.
/// `go install github.com/go-delve/delve/cmd/dlv@latest`, then
/// `cargo test --release dap_tests -- --ignored --nocapture`.
#[test]
#[ignore = "needs go and dlv installed"]
fn delve_breakpoint_and_variables() {
    let dir = std::env::temp_dir().join(format!("lumen-dap-go-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("main.go");
    std::fs::write(dir.join("go.mod"), "module daptest\n\ngo 1.21\n").unwrap();
    std::fs::write(
        &src,
        "package main\n\nimport \"fmt\"\n\nfunc add(a, b int) int {\n\tsum := a + b\n\treturn sum\n}\n\nfunc main() {\n\ttotal := 0\n\tfor i := 0; i < 3; i++ {\n\t\ttotal = add(total, i)\n\t}\n\tfmt.Println(\"total\", total)\n}\n",
    )
    .unwrap();

    let reg = Registry::builtin();
    let adapter = reg.for_language("go").unwrap();
    let mut dap = adapter.start(&dir, Arc::new(|| {})).expect("start dlv (is it installed?)");
    let mut s = adapter.session(&src, &dir);
    let src_s = src.to_string_lossy().to_string();
    s.set_breakpoints(&src_s, &[7], &mut dap); // `return sum`
    s.start(&mut dap);

    let hit = pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. }) && !s.locals.is_empty(), 120);
    eprintln!("state={:?} frames={:?} locals={:?} out={:?}", s.state, s.frames, s.locals, s.output);
    assert!(hit, "breakpoint never hit");
    assert_eq!(s.frames[0].line, 7);
    assert!(s.frames[0].name.contains("add"), "{:?}", s.frames[0]);
    let sum = s.locals.iter().find(|v| v.name == "sum").unwrap_or_else(|| panic!("no sum in {:?}", s.locals));
    assert_eq!(sum.value, "0", "first call add(0,0)");

    let end = Instant::now() + Duration::from_secs(60);
    while s.state != State::Terminated && Instant::now() < end {
        s.resume(&mut dap);
        pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. } | State::Terminated), 10);
    }
    assert_eq!(s.state, State::Terminated);
    // (delve prints the debuggee's stdout on its own stdout, not as DAP `output` events.)
    dap.disconnect(true);
    drop(dap);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parse_breakpoints_pairs_requested_lines_and_flags_unverified() {
    let body = json!({"breakpoints": [
        {"verified": false, "message": "could not find statement"},
        {"verified": true, "line": 5}]});
    let r = crate::dap::parse_breakpoints(&body, &[2, 4]);
    assert_eq!(r.len(), 2);
    assert!(!r[0].verified && r[0].requested == 2 && r[0].line == 2 && r[0].message.contains("statement"));
    assert!(r[1].verified && r[1].requested == 4 && r[1].line == 5, "adapter moved it to line 5");
    assert!(crate::dap::parse_breakpoints(&json!({}), &[1]).is_empty());
}

#[test]
fn normalize_source_path_maps_staged_and_relative_paths_to_the_project() {
    use crate::dap::normalize_source_path as n;
    let root = Path::new("/proj");
    assert_eq!(n("/proj/pages/../a.ghp", root), "/proj/a.ghp");
    assert_eq!(n("./pages/a.ghp", root), "/proj/pages/a.ghp");
    assert_eq!(n("/tmp/ghp-stage-123456/blog/post.ghp", root), "/proj/blog/post.ghp", "deleted staged copy");
    assert_eq!(n("/elsewhere/x.go", root), "/elsewhere/x.go");
}

/// A fake adapter for a GHP session: line 2 has no code, and frames name a staged copy.
fn fake_ghp_adapter(from_client: Receiver<Vec<u8>>, to_client: Sender<Vec<u8>>) {
    thread::spawn(move || {
        let mut fr = FrameReader::default();
        while let Ok(chunk) = from_client.recv() {
            for req in fr.push(&chunk) {
                let cmd = req["command"].as_str().unwrap_or("").to_string();
                let ok = |body: Value| {
                    framed(
                        &json!({"seq": 9, "type": "response", "request_seq": req["seq"], "success": true, "command": cmd, "body": body}),
                    )
                };
                let ev = |e: &str, body: Value| framed(&json!({"seq": 1, "type": "event", "event": e, "body": body}));
                let out: Vec<Vec<u8>> = match cmd.as_str() {
                    "initialize" => vec![ok(json!({}))],
                    "launch" => vec![ok(json!({})), ev("initialized", json!({}))],
                    "setBreakpoints" => vec![ok(json!({"breakpoints": [
                        {"verified": false, "message": "could not find statement"},
                        {"verified": true}]}))],
                    "configurationDone" => {
                        vec![ok(json!({})), ev("stopped", json!({"reason": "breakpoint", "threadId": 1}))]
                    }
                    "stackTrace" => vec![ok(json!({"stackFrames": [{"id": 1, "name": "main.Index",
                        "source": {"path": "/tmp/ghp-stage-99/index.ghp"}, "line": 4, "column": 0}]}))],
                    _ => vec![],
                };
                for o in out {
                    let _ = to_client.send(o);
                }
            }
        }
    });
}

#[test]
fn ghp_session_normalises_paths_and_records_unverified_breakpoints() {
    let (c2a_tx, c2a_rx) = channel();
    let (a2c_tx, a2c_rx) = channel();
    fake_ghp_adapter(c2a_rx, a2c_tx);
    let mut dap = Dap::from_io(PipeR(a2c_rx, Vec::new()), PipeW(c2a_tx), Arc::new(|| {}));
    let reg = Registry::builtin();
    let mut s = reg.get("ghp").unwrap().session(Path::new("/proj/index.ghp"), Path::new("/proj"));
    s.set_breakpoints("/proj/./index.ghp", &[2, 4], &mut dap);
    s.start(&mut dap);

    assert!(pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. }) && !s.frames.is_empty(), 5));
    assert_eq!(s.frames[0].path.as_deref(), Some("/proj/index.ghp"), "staged path mapped back");
    assert_eq!(s.unverified("/proj/index.ghp"), vec![(2, "could not find statement".to_string())]);
    assert!(s.output.iter().any(|l| l.contains("index.ghp:2 not verified")), "{:?}", s.output);
    dap.disconnect(true);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// GET / on a server that may not be listening yet; blocks until the handler answers.
fn http_get(port: u16, secs: u64) -> Option<String> {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Ok(mut c) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            c.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
            let mut body = String::new();
            c.read_to_string(&mut body).ok()?;
            return Some(body);
        }
        thread::sleep(Duration::from_millis(200));
    }
    None
}

/// Real GHP + delve: `ghp build --debug` as the prelaunch step, a breakpoint set in the `.ghp`
/// page fires when a request reaches the route, the frame is reported in the `.ghp` file and a
/// variable declared in a `<go ... />` block is readable.
/// Needs `go`, `dlv` and `ghp` on PATH (CI job "Debug adapters (delve)").
#[test]
#[ignore = "needs go, dlv and ghp installed"]
fn ghp_breakpoint_in_page_is_hit_by_http_request() {
    let root = std::env::temp_dir().join(format!("lumen-dap-ghp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ghp-demo"), &root);
    let page = root.join("index.ghp");
    let page_s = page.to_string_lossy().to_string();

    let reg = Registry::builtin();
    let adapter = reg.get("ghp").unwrap();
    adapter.run_prelaunch(&page, &root).expect("prelaunch: ghp build --debug");
    assert!(root.join(".lumen/ghp-debug/app").exists(), "debug binary not built");

    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut dap = adapter.start(&root, Arc::new(|| {})).expect("start dlv (is it installed?)");
    let mut s = adapter.session_with_args(&page, &root, &[format!("-addr=127.0.0.1:{port}")]);
    // line 2 is blank (no code), 4 is `<go count := ... />`, 9 is the `<h1><go= greeting />` line.
    s.set_breakpoints(&page_s, &[2, 4, 9], &mut dap);
    s.start(&mut dap);
    let request = thread::spawn(move || http_get(port, 120));

    let stopped = |s: &Session| matches!(s.state, State::Stopped { .. }) && !s.locals.is_empty();
    assert!(pump(&mut dap, &mut s, stopped, 120), "breakpoint never hit: {:?} {:?}", s.state, s.output);
    eprintln!("frames={:?} locals={:?} out={:?}", s.frames, s.locals, s.output);
    assert_eq!(s.unverified(&page_s).iter().map(|b| b.0).collect::<Vec<_>>(), vec![2], "{:?}", s.bp_results);
    let top = &s.frames[0];
    assert_eq!(top.path.as_deref(), Some(page_s.as_str()), "stopped in the .ghp file");
    assert_eq!(top.line, 4);
    assert!(top.name.contains("Index"), "{top:?}");
    let greeting = s.locals.iter().find(|v| v.name == "greeting").unwrap_or_else(|| panic!("{:?}", s.locals));
    assert_eq!(greeting.value, "\"HELLO\"");

    // Second breakpoint, later in the same request: both variables.
    s.resume(&mut dap);
    assert!(pump(&mut dap, &mut s, stopped, 30), "second breakpoint never hit");
    assert_eq!(s.frames[0].path.as_deref(), Some(page_s.as_str()));
    assert_eq!(s.frames[0].line, 9);
    let count = s.locals.iter().find(|v| v.name == "count").unwrap_or_else(|| panic!("{:?}", s.locals));
    assert_eq!(count.value, "5");

    s.resume(&mut dap);
    let body = request.join().unwrap().expect("the request was never answered");
    assert!(body.contains("<h1>HELLO5</h1>"), "{body}");
    dap.disconnect(true);
    drop(dap);
    let _ = std::fs::remove_dir_all(&root);
}
