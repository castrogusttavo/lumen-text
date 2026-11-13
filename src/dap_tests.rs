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

pub(crate) struct PipeR(pub Receiver<Vec<u8>>, pub Vec<u8>);
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
pub(crate) struct PipeW(pub Sender<Vec<u8>>);
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

/// A fake adapter: answers each request like a tiny debugger stopped on line 2 of `src` (thread 1
/// with two frames; a second thread `worker`). Command names go to `log`, whole requests to `reqs`.
/// Breakpoints from line 100 on are reported as not verified; `evaluate` of `bad` fails.
pub(crate) fn fake_adapter(
    from_client: Receiver<Vec<u8>>,
    to_client: Sender<Vec<u8>>,
    log: Sender<String>,
    reqs: Sender<Value>,
    src: String,
) {
    thread::spawn(move || {
        let mut fr = FrameReader::default();
        let send = |v: Value| {
            let _ = to_client.send(framed(&v));
        };
        while let Ok(chunk) = from_client.recv() {
            for req in fr.push(&chunk) {
                let cmd = req["command"].as_str().unwrap_or("").to_string();
                let _ = log.send(cmd.clone());
                let _ = reqs.send(req.clone());
                let ok = |body: Value| json!({"seq": 1000, "type": "response", "request_seq": req["seq"], "success": true, "command": cmd, "body": body});
                let ev = |e: &str, body: Value| json!({"seq": 1, "type": "event", "event": e, "body": body});
                match cmd.as_str() {
                    "initialize" => send(ok(json!({"supportsConfigurationDoneRequest": true}))),
                    "launch" => {
                        send(ok(json!({})));
                        send(ev("initialized", json!({})));
                    }
                    "setBreakpoints" => {
                        let bps: Vec<Value> = req["arguments"]["breakpoints"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|b| json!({"verified": b["line"].as_u64().unwrap_or(0) < 100}))
                            .collect();
                        send(ok(json!({"breakpoints": bps})));
                    }
                    "configurationDone" => {
                        send(ok(json!({})));
                        send(ev("output", json!({"category": "stdout", "output": "hello\n"})));
                        send(ev("output", json!({"category": "stderr", "output": "warn\n"})));
                        send(ev("stopped", json!({"reason": "breakpoint", "threadId": 1})));
                    }
                    "threads" => send(ok(json!({"threads": [{"id": 1, "name": "main"}, {"id": 2, "name": "worker"}]}))),
                    "stackTrace" => {
                        let frames = if req["arguments"]["threadId"] == 2 {
                            json!([{"id": 20, "name": "worker.run", "source": {"path": src}, "line": 4, "column": 1}])
                        } else {
                            json!([
                                {"id": 7, "name": "main.add", "source": {"path": src}, "line": 2, "column": 1},
                                {"id": 8, "name": "main.main", "source": {"path": src}, "line": 5, "column": 1}
                            ])
                        };
                        send(ok(json!({"stackFrames": frames})));
                    }
                    "scopes" => send(ok(json!({"scopes": [
                        {"name": "Locals", "variablesReference": 9},
                        {"name": "Globals", "variablesReference": 10, "expensive": true}]}))),
                    "variables" => {
                        let vars = match req["arguments"]["variablesReference"].as_i64() {
                            Some(9) => json!([
                                {"name": "a", "value": "1", "type": "int", "variablesReference": 0},
                                {"name": "obj", "value": "{...}", "type": "T", "variablesReference": 12}]),
                            Some(12) => json!([
                                {"name": "x", "value": "5", "type": "int", "variablesReference": 0},
                                {"name": "y", "value": "6", "type": "int", "variablesReference": 0}]),
                            Some(10) => json!([{"name": "G", "value": "7", "type": "int", "variablesReference": 0}]),
                            _ => json!([]),
                        };
                        send(ok(json!({"variables": vars})));
                    }
                    "evaluate" => {
                        if req["arguments"]["expression"] == "bad" {
                            send(json!({"seq": 1000, "type": "response", "request_seq": req["seq"], "success": false,
                                "command": cmd, "message": "no symbol bad in current context"}));
                        } else {
                            send(ok(json!({"result": "3", "variablesReference": 0})));
                        }
                    }
                    "next" => {
                        send(ok(json!({})));
                        send(ev("stopped", json!({"reason": "step", "threadId": 1})));
                    }
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
        s.output.extend(dap.drain_program_output());
    }
    until(s)
}

#[test]
fn fake_adapter_full_flow() {
    let (c2a_tx, c2a_rx) = channel();
    let (a2c_tx, a2c_rx) = channel();
    let (log_tx, log_rx) = channel();
    let (req_tx, _req_rx) = channel();
    fake_adapter(c2a_rx, a2c_tx, log_tx, req_tx, "/w/main.go".into());
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

fn started_fake(src: &str) -> (Dap, Session, Receiver<Value>) {
    let (c2a_tx, c2a_rx) = channel();
    let (a2c_tx, a2c_rx) = channel();
    let (log_tx, _log_rx) = channel();
    let (req_tx, req_rx) = channel();
    fake_adapter(c2a_rx, a2c_tx, log_tx, req_tx, src.into());
    let mut dap = Dap::from_io(PipeR(a2c_rx, Vec::new()), PipeW(c2a_tx), Arc::new(|| {}));
    dap.adapter_id = "go".into();
    let s = Registry::builtin().get("delve").unwrap().session(Path::new(src), Path::new("/w"));
    (dap, s, req_rx)
}

#[test]
fn breakpoint_specs_threads_tree_watch_and_eval_against_the_fake_adapter() {
    use crate::dap::{BpSpec, State as St};
    use crate::debug_model::{ConsoleKind, classify, flatten, verified_by_line};
    let (mut dap, mut s, reqs) = started_fake("/w/main.go");
    let specs = vec![
        BpSpec { line: 2, condition: Some("a > 1".into()), log_message: None },
        BpSpec { line: 200, condition: None, log_message: Some("a={a}".into()) },
    ];
    s.set_breakpoint_specs("/w/main.go", specs.clone(), &mut dap);
    s.set_watch(vec!["a+1".into(), "bad".into()], &mut dap);
    s.start(&mut dap);
    let ready = |s: &Session| {
        matches!(s.state, St::Stopped { .. })
            && s.var_children.contains_key(&9)
            && s.threads.len() == 2
            && s.watch_results.len() == 2
            && s.bp_results.contains_key("/w/main.go")
    };
    assert!(pump(&mut dap, &mut s, ready, 5), "{:?}", s.state);

    // Conditions and log messages reach the adapter; the answer says which were placed.
    let sent: Vec<Value> = reqs.try_iter().filter(|r| r["command"] == "setBreakpoints").collect();
    let bps = &sent[0]["arguments"]["breakpoints"];
    assert_eq!(bps[0], json!({"line": 2, "condition": "a > 1"}));
    assert_eq!(bps[1], json!({"line": 200, "logMessage": "a={a}"}));
    let v = verified_by_line(&s.bp_results["/w/main.go"]);
    assert_eq!((v[&1], v[&199]), (true, false), "line 200 is reported as not placed");

    // stderr is kept apart from stdout in the console.
    assert!(s.output.iter().any(|l| l == "hello"));
    assert!(s.output.iter().any(|l| classify(l) == (ConsoleKind::Stderr, "warn")));

    // Watch: a value and an error side by side.
    assert_eq!(s.watch_results["a+1"], Ok(("3".to_string(), String::new())));
    assert_eq!(s.watch_results["bad"], Err("no symbol bad in current context".to_string()));

    // The tree: scopes first, an expensive one stays closed until opened.
    let names = |s: &Session| flatten(s).into_iter().map(|r| (r.depth, r.name)).collect::<Vec<_>>();
    assert_eq!(names(&s), [(0, "Locals".into()), (1, "a".into()), (1, "obj".into()), (0, "Globals".into())]);
    s.toggle_var("Locals/obj", 12, &mut dap);
    assert!(pump(&mut dap, &mut s, |s| s.var_children.contains_key(&12), 5));
    assert_eq!(names(&s).iter().filter(|(d, _)| *d == 2).count(), 2, "x and y under obj");
    s.toggle_scope("Globals", 10, true, &mut dap);
    assert!(pump(&mut dap, &mut s, |s| s.var_children.contains_key(&10), 5));
    assert!(names(&s).contains(&(1, "G".into())));
    s.toggle_var("Locals/obj", 12, &mut dap);
    assert_eq!(names(&s).iter().filter(|(d, _)| *d == 2).count(), 0, "folded again");
    s.toggle_var("Locals/obj", 12, &mut dap);

    // Stepping clears the stop; the next stop re-opens what was open, by path.
    s.step_over(&mut dap);
    assert_eq!(s.state, St::Running);
    assert!(s.var_children.is_empty() && s.thread_frames.is_empty());
    let back = |s: &Session| matches!(s.state, St::Stopped { .. }) && s.var_children.contains_key(&12);
    assert!(pump(&mut dap, &mut s, back, 5), "expansion survives a step");
    assert_eq!(names(&s).iter().filter(|(d, _)| *d == 2).count(), 2);
    assert_eq!(s.last_stop.as_ref().unwrap().reason, "step");

    // Other threads: their stack is fetched when opened; selecting a frame follows it.
    s.toggle_thread(2, &mut dap);
    assert!(pump(&mut dap, &mut s, |s| s.thread_frames.contains_key(&2), 5));
    let _ = reqs.try_iter().count();
    s.select_frame(2, 0, &mut dap);
    assert_eq!(s.selected_frame().unwrap().name, "worker.run");
    assert!(pump(&mut dap, &mut s, |s| s.watch_results.contains_key("a+1") && s.var_children.contains_key(&9), 5));
    let scopes: Vec<Value> = reqs.try_iter().filter(|r| r["command"] == "scopes").collect();
    assert_eq!(scopes.last().unwrap()["arguments"]["frameId"], 20, "scopes of the selected frame");

    // Debug Console: echo + result, or the error.
    s.evaluate_repl("a+2", &mut dap);
    s.evaluate_repl("bad", &mut dap);
    let done = |s: &Session| {
        s.output.iter().any(|l| classify(l).0 == ConsoleKind::Result)
            && s.output.iter().any(|l| classify(l).0 == ConsoleKind::Error)
    };
    assert!(pump(&mut dap, &mut s, done, 5));
    assert!(s.output.iter().any(|l| classify(l) == (ConsoleKind::Echo, "a+2")));
    assert!(s.output.iter().any(|l| classify(l) == (ConsoleKind::Result, "3")));
    assert!(s.output.iter().any(|l| classify(l) == (ConsoleKind::Error, "no symbol bad in current context")));
    let ctx: Vec<String> = reqs
        .try_iter()
        .filter(|r| r["command"] == "evaluate")
        .filter_map(|r| r["arguments"]["context"].as_str().map(str::to_string))
        .collect();
    assert!(ctx.iter().any(|c| c == "repl"));

    // Hover evaluation lands in `hover_result`.
    s.evaluate_hover("a", &mut dap);
    assert!(pump(&mut dap, &mut s, |s| s.hover_result.is_some(), 5));
    assert_eq!(s.hover_result, Some(("a".to_string(), Ok("3".to_string()))));

    // Pause only applies to a running program; afterwards the session ends.
    s.pause(&mut dap);
    s.resume(&mut dap);
    assert!(pump(&mut dap, &mut s, |s| s.state == St::Terminated, 5));
    s.evaluate_repl("a", &mut dap);
    assert!(s.output.last().is_some_and(|l| classify(l).0 == ConsoleKind::Error), "not stopped: no request");
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
fn normalize_source_path_maps_relative_paths_to_the_project() {
    use crate::dap::normalize_source_path as n;
    let root = Path::new("/proj");
    assert_eq!(n("/proj/pages/../a.ghp", root), "/proj/a.ghp");
    assert_eq!(n("./pages/a.ghp", root), "/proj/pages/a.ghp");
    assert_eq!(n("/elsewhere/x.go", root), "/elsewhere/x.go");
}

/// A fake adapter for a GHP session: line 2 has no code.
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
                        "source": {"path": "/proj/index.ghp"}, "line": 4, "column": 0}]}))],
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
    assert_eq!(s.frames[0].path.as_deref(), Some("/proj/index.ghp"), "frame in the project page");
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

// ───────────────────────── program output, exit code, debuggee lifecycle ─────────────────────────

#[test]
fn exit_code_process_pid_and_launch_failure_are_reported() {
    use crate::debug_model::{ConsoleKind, classify};
    let (c2a_tx, _c2a_rx) = channel::<Vec<u8>>();
    let (_a2c_tx, a2c_rx) = channel::<Vec<u8>>();
    let mut dap = Dap::from_io(PipeR(a2c_rx, Vec::new()), PipeW(c2a_tx), Arc::new(|| {}));
    let mut s = Session::new("x", &[], Path::new("."), false);
    let ev = |event: &str, body: Value| DapMsg::Event { event: event.into(), body };
    s.handle(&ev("process", json!({"name": "app", "systemProcessId": 4242})), &mut dap);
    assert_eq!(dap.debuggee_pid, Some(4242));
    s.handle(&ev("exited", json!({"exitCode": 1})), &mut dap);
    assert_eq!((s.state.clone(), s.exit_code), (State::Terminated, Some(1)));
    let last = s.output.last().expect("a line about the exit");
    assert_eq!(classify(last), (ConsoleKind::Error, "Program exited with code 1"));

    // A refused `launch` ends the session with the reason in the console, so F5 can try again.
    let mut s = Session::new("x", &[], Path::new("."), false);
    let seq = dap.request("launch", json!({}), crate::dap::Pending::Launch);
    let resp = DapMsg::Response {
        seq_for: seq,
        command: "launch".into(),
        success: false,
        body: Value::Null,
        message: Some("listen tcp: bind: address already in use".into()),
    };
    s.handle(&resp, &mut dap);
    assert_eq!(s.state, State::Terminated);
    let want = (ConsoleKind::Error, "could not launch: listen tcp: bind: address already in use");
    assert!(s.output.iter().any(|l| classify(l) == want), "{:?}", s.output);
}

/// A stand-in adapter (`sh`) that announces a port, writes program-looking lines on stdout/stderr and
/// leaves a child behind (the "debuggee"). Dropping the `Dap` must end both, and the lines must reach
/// the console buffer, stderr marked as such, dlv-looking chatter tagged.
#[cfg(unix)]
#[test]
fn dropping_a_dap_kills_the_adapter_and_its_orphan_debuggee_and_forwards_output() {
    use crate::dap::pid_alive;
    use crate::debug_model::{ConsoleKind, classify};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let _hold = thread::spawn(move || {
        let c = l.accept();
        thread::sleep(Duration::from_secs(30));
        drop(c);
    });
    let script = format!(
        "sleep 300 & echo \"child $!\"; echo \"DAP server listening at: 127.0.0.1:{port}\"; echo prog-out; echo prog-err >&2; echo \"2024/01/01 debug layer=dap x\"; wait"
    );
    let mut dap = Dap::spawn_listening("sh", &["-c", &script], Path::new("."), Arc::new(|| {})).expect("spawn");
    let mut lines = Vec::new();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && lines.len() < 4 {
        lines.extend(dap.drain_program_output());
        thread::sleep(Duration::from_millis(20));
    }
    assert!(lines.iter().any(|l| l == "prog-out"), "{lines:?}");
    assert!(lines.iter().any(|l| classify(l) == (ConsoleKind::Stderr, "prog-err")), "{lines:?}");
    assert!(lines.iter().any(|l| classify(l).0 == ConsoleKind::Echo && l.contains("[dlv]")), "{lines:?}");
    let child: u32 = lines.iter().find_map(|l| l.strip_prefix("child ")).expect("child pid").trim().parse().unwrap();
    let adapter = dap.adapter_pid().unwrap();
    assert!(pid_alive(child) && pid_alive(adapter));
    drop(dap);
    let end = Instant::now() + Duration::from_secs(5);
    while (pid_alive(child) || pid_alive(adapter)) && Instant::now() < end {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!pid_alive(child), "the debuggee outlived its adapter");
    assert!(!pid_alive(adapter), "the adapter is still running");
}

// Real delve, `mode: exec` on a Go server that binds a port: the lifecycle bugs seen with GHP projects.

const SERVER_GO: &str = r#"package main

import (
	"fmt"
	"net"
	"os"
)

func main() {
	addr := "127.0.0.1:0"
	if len(os.Args) > 1 {
		addr = os.Args[1]
	}
	if addr == "exit1" {
		fmt.Println("about to fail")
		fmt.Fprintln(os.Stderr, "fatal: listen tcp: bind: address already in use")
		os.Exit(1)
	}
	l, err := net.Listen("tcp", addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, "listen failed:", err)
		os.Exit(1)
	}
	fmt.Println("listening on", l.Addr())
	for {
		c, err := l.Accept()
		if err != nil {
			return
		}
		c.Close()
	}
}
"#;

type Dbg = crate::app_debug::Debugger;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Build the server fixture; returns `(dir, binary)`.
fn build_server(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lumen-dap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("go.mod"), "module srv\n\ngo 1.21\n").unwrap();
    std::fs::write(dir.join("main.go"), SERVER_GO).unwrap();
    let bin = dir.join("app");
    let st = std::process::Command::new("go")
        .args(["build", "-gcflags=all=-N -l", "-o"])
        .arg(&bin)
        .current_dir(&dir)
        .status()
        .expect("go build");
    assert!(st.success());
    (dir, bin)
}

/// dlv in exec mode on `bin` with `args`, like the `ghp` adapter, wrapped the way the UI holds it.
fn start_exec(dir: &Path, bin: &Path, args: &[&str]) -> Dbg {
    let reg = Registry::builtin();
    let adapter = reg.get("ghp").unwrap();
    let mut dap = adapter.start(dir, Arc::new(|| {})).expect("start dlv (is it installed?)");
    let mut s = Session::with_request(
        "launch",
        json!({"type": "go", "mode": "exec", "program": bin, "cwd": dir, "args": args, "stopOnEntry": false}),
    );
    s.exception_bps = false;
    s.start(&mut dap);
    Dbg::Native { dap: Box::new(dap), session: Box::new(s) }
}

fn pump_debugger(d: &mut Dbg, until: impl Fn(&Dbg) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(120);
    while Instant::now() < end && !until(d) {
        d.pump();
        thread::sleep(Duration::from_millis(30));
    }
    until(d)
}

fn said(d: &Dbg, what: &str) -> bool {
    d.output().iter().any(|l| l.contains(what))
}

#[cfg(unix)]
fn debuggee_of(d: &Dbg) -> Vec<u32> {
    match d {
        Dbg::Native { dap, .. } => crate::dap::descendants(dap.adapter_pid().unwrap()),
        _ => Vec::new(),
    }
}

/// The port can be bound again within a few seconds (nothing holds it any more).
fn port_free(port: u16) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

#[cfg(unix)]
fn assert_gone(pids: &[u32], port: u16, what: &str) {
    let end = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|p| crate::dap::pid_alive(*p)) && Instant::now() < end {
        thread::sleep(Duration::from_millis(50));
    }
    for p in pids {
        assert!(!crate::dap::pid_alive(*p), "{what}: process {p} is still alive (orphaned debuggee)");
    }
    assert!(port_free(port), "{what}: port {port} is still taken");
}

/// The UI's Stop: the debuggee is gone and its port free soon after, and its output reached the console.
#[cfg(unix)]
#[test]
#[ignore = "needs go and dlv installed"]
fn delve_exec_stop_leaves_no_debuggee_and_shows_its_output() {
    let (dir, bin) = build_server("stop");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let mut d = start_exec(&dir, &bin, &[&addr]);
    let shown = pump_debugger(&mut d, |d| said(d, "listening on"));
    assert!(shown, "program output never reached the console: {:?}", d.output());
    let pids = debuggee_of(&d);
    assert!(!pids.is_empty(), "no debuggee below dlv");
    assert!(std::net::TcpListener::bind(&addr[..]).is_err(), "the server should hold the port");

    let t = Instant::now();
    d.stop_and_wait();
    assert!(t.elapsed() < Duration::from_secs(15), "stop took {:?}", t.elapsed());
    assert_gone(&pids, port, "stop");
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Restart: stop, then launch the same server on the same port at once; no "address already in use".
#[cfg(unix)]
#[test]
#[ignore = "needs go and dlv installed"]
fn delve_exec_restart_reuses_the_port() {
    let (dir, bin) = build_server("restart");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let mut first = start_exec(&dir, &bin, &[&addr]);
    assert!(pump_debugger(&mut first, |d| said(d, "listening on")), "{:?}", first.output());
    let pids = debuggee_of(&first);
    first.stop_and_wait();
    drop(first);
    let mut second = start_exec(&dir, &bin, &[&addr]);
    let up = pump_debugger(&mut second, |d| said(d, "listening on") || said(d, "address already in use"));
    assert!(up, "{:?}", second.output());
    assert!(said(&second, "listening on") && !said(&second, "already in use"), "{:?}", second.output());
    let pids2 = debuggee_of(&second);
    second.stop_and_wait();
    drop(second);
    assert_gone(&pids, port, "first run");
    assert_gone(&pids2, port, "second run");
    let _ = std::fs::remove_dir_all(&dir);
}

/// App exit / dropping the session without an explicit stop must not leave the program behind either.
#[cfg(unix)]
#[test]
#[ignore = "needs go and dlv installed"]
fn delve_exec_drop_leaves_no_debuggee() {
    let (dir, bin) = build_server("drop");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let mut d = start_exec(&dir, &bin, &[&addr]);
    assert!(pump_debugger(&mut d, |d| said(d, "listening on")), "{:?}", d.output());
    let pids = debuggee_of(&d);
    assert!(!pids.is_empty());
    drop(d);
    assert_gone(&pids, port, "drop");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program that dies at once: exit code and its last lines (stdout plain, stderr marked) reach the console.
#[cfg(unix)]
#[test]
#[ignore = "needs go and dlv installed"]
fn delve_exec_quick_exit_reports_code_and_output() {
    use crate::debug_model::{ConsoleKind, classify};
    let (dir, bin) = build_server("exit1");
    let mut d = start_exec(&dir, &bin, &["exit1"]);
    let done =
        |d: &Dbg| d.state() == State::Terminated && said(d, "about to fail") && said(d, "address already in use");
    assert!(pump_debugger(&mut d, done), "{:?}", d.output());
    let exit = (ConsoleKind::Error, "Program exited with code 1");
    assert!(d.output().iter().any(|l| classify(l) == exit), "{:?}", d.output());
    assert_eq!(d.session().and_then(|s| s.exit_code), Some(1));
    assert!(d.output().iter().any(|l| classify(l).0 == ConsoleKind::Stderr && l.contains("address already in use")));
    d.stop_and_wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn announced_address_covers_delve_and_codelldb() {
    use crate::dap::announced_addr;
    assert_eq!(announced_addr("DAP server listening at: 127.0.0.1:43211").as_deref(), Some("127.0.0.1:43211"));
    assert_eq!(announced_addr("DAP server listening at: [::1]:5").as_deref(), Some("[::1]:5"));
    assert_eq!(announced_addr("Listening on port 40123").as_deref(), Some("127.0.0.1:40123"));
    assert_eq!(announced_addr("Listening on port nope"), None);
    assert_eq!(announced_addr("starting up"), None);
}

/// A python session against the scripted adapter: debugpy's launch request (adapter id `python`, the
/// file as `program`, internal console) goes out and the stop/variables flow works as for any adapter.
#[test]
fn debugpy_session_sends_its_launch_request_to_the_fake_adapter() {
    let (c2a_tx, c2a_rx) = channel();
    let (a2c_tx, a2c_rx) = channel();
    let (log_tx, log_rx) = channel();
    let (req_tx, req_rx) = channel();
    fake_adapter(c2a_rx, a2c_tx, log_tx, req_tx, "/w/app.py".into());
    let mut dap = Dap::from_io(PipeR(a2c_rx, Vec::new()), PipeW(c2a_tx), Arc::new(|| {}));
    dap.adapter_id = "python".into();
    let mut s = Registry::builtin().get("debugpy").unwrap().session(Path::new("/w/app.py"), Path::new("/w"));
    s.set_breakpoints("/w/app.py", &[2], &mut dap);
    s.start(&mut dap);
    assert!(pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. }) && !s.locals.is_empty(), 5));
    let sent: Vec<Value> = req_rx.try_iter().collect();
    let init = sent.iter().find(|r| r["command"] == "initialize").unwrap();
    assert_eq!(init["arguments"]["adapterID"], "python");
    let launch = sent.iter().find(|r| r["command"] == "launch").unwrap();
    assert_eq!(launch["arguments"]["program"], "/w/app.py");
    assert_eq!(launch["arguments"]["type"], "python");
    assert_eq!(launch["arguments"]["console"], "internalConsole");
    assert!(launch["arguments"].get("request").is_none());
    s.resume(&mut dap);
    assert!(pump(&mut dap, &mut s, |s| s.state == State::Terminated, 5));
    dap.disconnect(true);
    let cmds: Vec<String> = log_rx.try_iter().collect();
    assert!(cmds.iter().any(|c| c == "configurationDone"), "{cmds:?}");
}

/// Attach requests (debugpy to a `--listen` server, lldb-dap to a pid) are sent as `attach`.
#[test]
fn attach_requests_go_out_as_attach() {
    let reg = Registry::builtin();
    let ov =
        crate::debug_model::LaunchOverride { request: Some("attach".into()), port: Some(5679), ..Default::default() };
    let s = reg.get("debugpy").unwrap().session_with(Path::new("/w/a.py"), Path::new("/w"), &[], Some(&ov));
    let (cmd, args) = s.start_request.unwrap();
    assert_eq!(cmd, "attach");
    assert_eq!((args["connect"]["host"].as_str(), args["connect"]["port"].as_u64()), (Some("127.0.0.1"), Some(5679)));
    let ov = crate::debug_model::LaunchOverride { request: Some("attach".into()), pid: Some(31), ..Default::default() };
    let s = reg.get("lldb-dap").unwrap().session_with(Path::new("/w/a.c"), Path::new("/w"), &[], Some(&ov));
    let (cmd, args) = s.start_request.unwrap();
    assert_eq!((cmd.as_str(), args["pid"].as_i64()), ("attach", Some(31)));
}

const PY_FIXTURE: &str = "def add(a, b):\n    s = a + b\n    return s\n\n\ntotal = 0\nfor i in range(3):\n    total = add(total, i)\nprint('total', total)\n";

/// Real debugpy: a breakpoint hit and variable read on a tiny script, with the interpreter
/// that `pyenv` finds (CI points `VIRTUAL_ENV` at a venv holding debugpy).
/// `python -m pip install debugpy`, then `cargo test --release dap_tests -- --ignored debugpy`.
#[test]
#[ignore = "needs python with debugpy installed"]
fn debugpy_breakpoint_and_variables() {
    let dir = std::env::temp_dir().join(format!("lumen-dap-py-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("main.py");
    std::fs::write(&src, PY_FIXTURE).unwrap();
    let adapter = Registry::builtin().for_language("python").unwrap().clone();
    let mut dap = adapter.start(&dir, Arc::new(|| {})).expect("start debugpy (pip install debugpy)");
    let mut s = adapter.session(&src, &dir);
    s.set_breakpoints(&src.to_string_lossy(), &[2], &mut dap);
    s.start(&mut dap);
    let hit = pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. }) && !s.locals.is_empty(), 120);
    eprintln!("state={:?} frames={:?} locals={:?} out={:?}", s.state, s.frames, s.locals, s.output);
    assert!(hit, "breakpoint never hit");
    assert_eq!(s.frames[0].line, 2);
    assert!(s.frames[0].name.contains("add"), "{:?}", s.frames[0]);
    let a = s.locals.iter().find(|v| v.name == "a").unwrap_or_else(|| panic!("no a in {:?}", s.locals));
    assert_eq!(a.value, "0", "first call add(0, 0)");
    let end = Instant::now() + Duration::from_secs(60);
    while s.state != State::Terminated && Instant::now() < end {
        s.resume(&mut dap);
        pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. } | State::Terminated), 10);
    }
    assert_eq!(s.state, State::Terminated);
    assert!(s.output.iter().any(|l| l.contains("total 3")), "program output reaches the console: {:?}", s.output);
    dap.disconnect(true);
    drop(dap);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real debugpy, attach: a script started with `python -m debugpy --listen PORT --wait-for-client`
/// is attached to through a `launch.json`-style override and stops at its breakpoint.
#[test]
#[ignore = "needs python with debugpy installed"]
fn debugpy_attach_to_a_listening_script() {
    let dir = std::env::temp_dir().join(format!("lumen-dap-pyattach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("main.py");
    std::fs::write(&src, PY_FIXTURE).unwrap();
    let adapter = Registry::builtin().for_language("python").unwrap().clone();
    let python = adapter.resolve(&dir).expect("python");
    let port = free_port();
    let mut target = std::process::Command::new(&python)
        .args(["-m", "debugpy", "--listen", &format!("127.0.0.1:{port}"), "--wait-for-client"])
        .arg(&src)
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the script under debugpy");
    let end = Instant::now() + Duration::from_secs(60);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() && Instant::now() < end {
        thread::sleep(Duration::from_millis(100));
    }
    let ov = crate::debug_model::LaunchOverride {
        config: "debugpy".into(),
        request: Some("attach".into()),
        port: Some(port),
        ..Default::default()
    };
    let mut dap = adapter.start(&dir, Arc::new(|| {})).expect("start debugpy adapter");
    let mut s = adapter.session_with(&src, &dir, &[], Some(&ov));
    s.set_breakpoints(&src.to_string_lossy(), &[2], &mut dap);
    s.start(&mut dap);
    let hit = pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. }) && !s.locals.is_empty(), 120);
    eprintln!("state={:?} frames={:?} locals={:?} out={:?}", s.state, s.frames, s.locals, s.output);
    assert!(hit, "breakpoint never hit after attach");
    assert_eq!(s.frames[0].line, 2);
    dap.disconnect(true);
    drop(dap);
    let _ = target.kill();
    let _ = target.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
