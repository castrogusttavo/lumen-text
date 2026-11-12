//! Debug Adapter Protocol client: an adapter process spoken to over stdio with
//! `Content-Length` framing (one reader thread, one writer thread), plus a small
//! `Session` state machine that drives the usual launch/breakpoint/stop flow.
//!
//! gdb (`gdb --interpreter=dap`) findings, encoded in `Dap::launch`:
//! - `launch` takes `program`, `args`, `cwd` and `stopAtBeginningOfMainSubprogram`.
//! - gdb answers `launch` only after `configurationDone`, so the client must not wait
//!   for the launch response before sending breakpoints; it sends `initialized` right
//!   after receiving `launch`. `Session` therefore reacts to the `initialized` event.
//! - breakpoints are sent as `{source:{path}, breakpoints:[{line}]}`.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

pub type Wake = Arc<dyn Fn() + Send + Sync>;

// ───────────────────────────── framing ─────────────────────────────

/// Incremental `Content-Length` frame parser.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(hend) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&self.buf[..hend]).to_string();
            let len = head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok()).flatten()
            });
            let Some(len) = len else {
                // Malformed header block: drop it and resync.
                self.buf.drain(..hend + 4);
                continue;
            };
            let start = hend + 4;
            if self.buf.len() < start + len {
                break;
            }
            if let Ok(v) = serde_json::from_slice::<Value>(&self.buf[start..start + len]) {
                out.push(v);
            }
            self.buf.drain(..start + len);
        }
        out
    }
}

fn frame(v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

// ───────────────────────────── messages ─────────────────────────────

/// What a request was for; stored per seq so responses can be interpreted.
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    Initialize,
    Launch,
    SetBreakpoints(String),
    ConfigurationDone,
    Resume,
    Threads,
    StackTrace(i64),
    Scopes(i64),
    /// Variables of the top frame's first scope (shown as locals).
    Locals,
    Variables(i64),
    Evaluate,
    /// `evaluate` typed in the Debug Console, in a watch row, or by hovering the editor.
    EvalRepl(String),
    EvalWatch(String),
    EvalHover(String),
    Disconnect,
    Other,
}

#[derive(Clone, Debug)]
pub enum DapMsg {
    Response {
        seq_for: i64,
        command: String,
        success: bool,
        body: Value,
        message: Option<String>,
    },
    Event {
        event: String,
        body: Value,
    },
    /// A request sent by the adapter (`startDebugging`, ...). Answer with `Dap::respond`.
    ReverseRequest {
        seq: i64,
        command: String,
        arguments: Value,
    },
    /// The adapter process ended (stdout closed).
    Exited,
}

fn classify(v: Value, tx: &Sender<Vec<u8>>, seq: &AtomicI64) -> Option<DapMsg> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let body = v.get("body").cloned().unwrap_or(Value::Null);
    match v.get("type").and_then(Value::as_str)? {
        "response" => Some(DapMsg::Response {
            seq_for: v.get("request_seq").and_then(Value::as_i64).unwrap_or(0),
            command: s("command"),
            success: v.get("success").and_then(Value::as_bool).unwrap_or(false),
            body,
            message: v.get("message").and_then(Value::as_str).map(str::to_string),
        }),
        "event" => Some(DapMsg::Event { event: s("event"), body }),
        "request" => {
            let command = s("command");
            let rseq = v.get("seq").and_then(Value::as_i64).unwrap_or(0);
            if command == "startDebugging" {
                // The owner (MultiSession) creates the child session and answers.
                return Some(DapMsg::ReverseRequest {
                    seq: rseq,
                    command,
                    arguments: v.get("arguments").cloned().unwrap_or(Value::Null),
                });
            }
            // Other reverse requests (runInTerminal, ...): refuse so the adapter never hangs.
            let resp = json!({
                "seq": seq.fetch_add(1, Ordering::SeqCst), "type": "response",
                "request_seq": rseq, "success": false, "command": command,
                "message": "not supported by this client",
            });
            let _ = tx.send(frame(&resp));
            None
        }
        _ => None,
    }
}

// ───────────────────────────── client ─────────────────────────────

pub struct Dap {
    child: Option<Child>,
    /// TCP transport: kept to shut the socket down on drop (unblocks the reader thread).
    sock: Option<TcpStream>,
    /// `adapterID` sent in `initialize`.
    pub adapter_id: String,
    tx: Sender<Vec<u8>>,
    pub rx: Receiver<DapMsg>,
    seq: Arc<AtomicI64>,
    pending: HashMap<i64, Pending>,
}

impl Drop for Dap {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(s) = self.sock.as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

/// Reader/writer threads shared by every transport.
fn start_io(
    mut input: impl Read + Send + 'static,
    mut output: impl Write + Send + 'static,
    wake: Wake,
) -> (Sender<Vec<u8>>, Receiver<DapMsg>, Arc<AtomicI64>) {
    let seq = Arc::new(AtomicI64::new(1));
    let (tx, wrx) = channel::<Vec<u8>>();
    thread::spawn(move || {
        while let Ok(bytes) = wrx.recv() {
            if output.write_all(&bytes).is_err() || output.flush().is_err() {
                break;
            }
        }
    });

    let (mtx, rx) = channel::<DapMsg>();
    let (rtx, rseq) = (tx.clone(), seq.clone());
    thread::spawn(move || {
        let mut fr = FrameReader::default();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match input.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    let _ = mtx.send(DapMsg::Exited);
                    wake();
                    break;
                }
                Ok(n) => {
                    for v in fr.push(&chunk[..n]) {
                        if let Some(m) = classify(v, &rtx, &rseq)
                            && mtx.send(m).is_err()
                        {
                            return;
                        }
                    }
                    wake();
                }
            }
        }
    });
    (tx, rx, seq)
}

impl Dap {
    pub fn spawn(cmd: &str, args: &[&str], cwd: &Path, wake: Wake) -> io::Result<Dap> {
        let mut child = Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        let (tx, rx, seq) = start_io(stdout, stdin, wake);
        Ok(Dap { child: Some(child), sock: None, adapter_id: "gdb".into(), tx, rx, seq, pending: HashMap::new() })
    }

    /// Connect to a DAP server over TCP (same framing), retrying for up to ~6 s.
    pub fn connect_tcp(host: &str, port: u16, wake: Wake) -> io::Result<Dap> {
        let end = Instant::now() + Duration::from_secs(6);
        let stream = loop {
            match TcpStream::connect((host, port)) {
                Ok(s) => break s,
                Err(e) if Instant::now() >= end => return Err(e),
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        };
        let _ = stream.set_nodelay(true);
        let (tx, rx, seq) = start_io(stream.try_clone()?, stream.try_clone()?, wake);
        Ok(Dap { child: None, sock: Some(stream), adapter_id: "gdb".into(), tx, rx, seq, pending: HashMap::new() })
    }

    /// A client over any byte transport (in-memory pipes in the tests).
    pub fn from_io(input: impl Read + Send + 'static, output: impl Write + Send + 'static, wake: Wake) -> Dap {
        let (tx, rx, seq) = start_io(input, output, wake);
        Dap { child: None, sock: None, adapter_id: "gdb".into(), tx, rx, seq, pending: HashMap::new() }
    }

    /// Spawn an adapter that serves DAP on a TCP port it picks itself and announces on stdout
    /// (`... listening at: 127.0.0.1:41234`, as `dlv dap --listen=127.0.0.1:0` does), then connect.
    /// The child is killed when the `Dap` is dropped.
    pub fn spawn_listening(cmd: &str, args: &[&str], cwd: &Path, wake: Wake) -> io::Result<Dap> {
        use std::io::{BufRead, BufReader};
        let mut child = Command::new(cmd)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        let (atx, arx) = channel::<String>();
        thread::spawn(move || {
            let mut sent = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if !sent && let Some((_, addr)) = line.split_once("listening at:") {
                    sent = true;
                    let _ = atx.send(addr.trim().to_string());
                }
            }
        });
        let addr = match arx.recv_timeout(Duration::from_secs(15)) {
            Ok(a) => a,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other(format!("{cmd} did not announce a listening address")));
            }
        };
        let port = addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        let host = addr.rsplit_once(':').map_or("127.0.0.1", |(h, _)| h.trim_matches(['[', ']']));
        let Some(port) = port else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(format!("bad listening address: {addr}")));
        };
        match Dap::connect_tcp(if host.is_empty() { "127.0.0.1" } else { host }, port, wake) {
            Ok(mut d) => {
                d.child = Some(child);
                Ok(d)
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }

    /// Answer a `DapMsg::ReverseRequest`.
    pub fn respond(&mut self, seq: i64, command: &str, success: bool, body: Value) {
        let seq_out = self.seq.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(frame(&json!({
            "seq": seq_out, "type": "response", "request_seq": seq,
            "success": success, "command": command, "body": body,
        })));
    }

    /// Send a request; returns its seq. `what` is remembered for `take_pending`.
    pub fn request(&mut self, command: &str, args: Value, what: Pending) -> i64 {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        self.pending.insert(seq, what);
        let _ = self.tx.send(frame(&json!({"seq": seq, "type": "request", "command": command, "arguments": args})));
        seq
    }

    /// Look up (and forget) what request `seq` was for.
    pub fn take_pending(&mut self, seq: i64) -> Option<Pending> {
        self.pending.remove(&seq)
    }

    pub fn initialize(&mut self) -> i64 {
        self.request(
            "initialize",
            json!({
                "clientID": "lumen-text", "clientName": "lumen-text", "adapterID": self.adapter_id,
                "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path",
                "supportsRunInTerminalRequest": false, "supportsVariableType": true,
            }),
            Pending::Initialize,
        )
    }

    pub fn set_exception_breakpoints(&mut self) -> i64 {
        self.request("setExceptionBreakpoints", json!({"filters": []}), Pending::Other)
    }

    pub fn launch(&mut self, program: &str, args: &[String], cwd: &Path, stop_on_entry: bool) -> i64 {
        self.request(
            "launch",
            json!({
                "program": program, "args": args, "cwd": cwd.to_string_lossy(),
                "stopAtBeginningOfMainSubprogram": stop_on_entry,
                "stopOnEntry": stop_on_entry, "noDebug": false,
            }),
            Pending::Launch,
        )
    }

    pub fn set_breakpoints(&mut self, source_path: &str, lines: &[usize]) -> i64 {
        let bps: Vec<Value> = lines.iter().map(|l| json!({"line": l})).collect();
        self.request(
            "setBreakpoints",
            json!({"source": {"path": source_path}, "breakpoints": bps, "lines": lines}),
            Pending::SetBreakpoints(source_path.to_string()),
        )
    }

    /// `setBreakpoints` with conditions and log messages (a logpoint has `log_message`).
    pub fn set_breakpoints_spec(&mut self, source_path: &str, specs: &[BpSpec]) -> i64 {
        let bps: Vec<Value> = specs
            .iter()
            .map(|b| {
                let mut o = json!({"line": b.line});
                if let Some(c) = b.condition.as_deref().filter(|c| !c.trim().is_empty()) {
                    o["condition"] = json!(c);
                }
                if let Some(m) = b.log_message.as_deref().filter(|m| !m.is_empty()) {
                    o["logMessage"] = json!(m);
                }
                o
            })
            .collect();
        let lines: Vec<usize> = specs.iter().map(|b| b.line).collect();
        self.request(
            "setBreakpoints",
            json!({"source": {"path": source_path}, "breakpoints": bps, "lines": lines}),
            Pending::SetBreakpoints(source_path.to_string()),
        )
    }

    /// `evaluate` in `context` (`repl`, `watch`, `hover`); `frame_id` is left out when unknown.
    pub fn evaluate_in(&mut self, expr: &str, frame_id: Option<i64>, context: &str, what: Pending) -> i64 {
        let mut a = json!({"expression": expr, "context": context});
        if let Some(f) = frame_id {
            a["frameId"] = json!(f);
        }
        self.request("evaluate", a, what)
    }

    pub fn configuration_done(&mut self) -> i64 {
        self.request("configurationDone", json!({}), Pending::ConfigurationDone)
    }
    pub fn continue_(&mut self, thread: i64) -> i64 {
        self.request("continue", json!({"threadId": thread}), Pending::Resume)
    }
    pub fn next(&mut self, thread: i64) -> i64 {
        self.request("next", json!({"threadId": thread}), Pending::Resume)
    }
    pub fn step_in(&mut self, thread: i64) -> i64 {
        self.request("stepIn", json!({"threadId": thread}), Pending::Resume)
    }
    pub fn step_out(&mut self, thread: i64) -> i64 {
        self.request("stepOut", json!({"threadId": thread}), Pending::Resume)
    }
    pub fn pause(&mut self, thread: i64) -> i64 {
        self.request("pause", json!({"threadId": thread}), Pending::Other)
    }
    pub fn threads(&mut self) -> i64 {
        self.request("threads", json!({}), Pending::Threads)
    }
    pub fn stack_trace(&mut self, thread: i64) -> i64 {
        self.request(
            "stackTrace",
            json!({"threadId": thread, "startFrame": 0, "levels": 50}),
            Pending::StackTrace(thread),
        )
    }
    pub fn scopes(&mut self, frame_id: i64) -> i64 {
        self.request("scopes", json!({"frameId": frame_id}), Pending::Scopes(frame_id))
    }
    pub fn variables(&mut self, vars_ref: i64) -> i64 {
        self.request("variables", json!({"variablesReference": vars_ref}), Pending::Variables(vars_ref))
    }
    pub fn evaluate(&mut self, expr: &str, frame_id: i64) -> i64 {
        self.request("evaluate", json!({"expression": expr, "frameId": frame_id, "context": "repl"}), Pending::Evaluate)
    }
    pub fn disconnect(&mut self, terminate: bool) -> i64 {
        self.request("disconnect", json!({"terminateDebuggee": terminate}), Pending::Disconnect)
    }
}

// ───────────────────────────── parsing ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    pub path: Option<String>,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub name: String,
    pub value: String,
    pub type_: String,
    pub vars_ref: i64,
}

/// One breakpoint to send: a plain line, a conditional one or a logpoint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BpSpec {
    /// 1-based, as the protocol wants it.
    pub line: usize,
    pub condition: Option<String>,
    pub log_message: Option<String>,
}

/// What the adapter answered for one requested breakpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct BpResult {
    /// 1-based line the breakpoint ended up on.
    pub line: usize,
    pub verified: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadInfo {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    pub name: String,
    pub vars_ref: i64,
    pub expensive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stop {
    pub reason: String,
    pub thread_id: i64,
    pub text: String,
}

pub fn parse_stack_trace(body: &Value) -> Vec<Frame> {
    let Some(arr) = body.get("stackFrames").and_then(Value::as_array) else { return Vec::new() };
    arr.iter()
        .map(|f| Frame {
            id: f.get("id").and_then(Value::as_i64).unwrap_or(0),
            name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            path: f.pointer("/source/path").and_then(Value::as_str).map(str::to_string),
            line: f.get("line").and_then(Value::as_u64).unwrap_or(0) as usize,
            col: f.get("column").and_then(Value::as_u64).unwrap_or(0) as usize,
        })
        .collect()
}

pub fn parse_variables(body: &Value) -> Vec<Var> {
    let Some(arr) = body.get("variables").and_then(Value::as_array) else { return Vec::new() };
    arr.iter()
        .map(|v| Var {
            name: v.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            value: v.get("value").and_then(Value::as_str).unwrap_or("").to_string(),
            type_: v.get("type").and_then(Value::as_str).unwrap_or("").to_string(),
            vars_ref: v.get("variablesReference").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect()
}

pub fn parse_stopped(body: &Value) -> Stop {
    let text = body.get("text").or_else(|| body.get("description")).and_then(Value::as_str).unwrap_or("");
    Stop {
        reason: body.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
        thread_id: body.get("threadId").and_then(Value::as_i64).unwrap_or(1),
        text: text.to_string(),
    }
}

/// `(result, type)` of an `evaluate` answer.
fn eval_text(body: &Value) -> (String, String) {
    let s = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    (s("result"), s("type"))
}

pub fn parse_threads(body: &Value) -> Vec<ThreadInfo> {
    let Some(arr) = body.get("threads").and_then(Value::as_array) else { return Vec::new() };
    arr.iter()
        .map(|t| ThreadInfo {
            id: t.get("id").and_then(Value::as_i64).unwrap_or(0),
            name: t.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect()
}

pub fn parse_scopes(body: &Value) -> Vec<Scope> {
    let Some(arr) = body.get("scopes").and_then(Value::as_array) else { return Vec::new() };
    arr.iter()
        .map(|s| Scope {
            name: s.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            vars_ref: s.get("variablesReference").and_then(Value::as_i64).unwrap_or(0),
            expensive: s.get("expensive").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect()
}

/// The `setBreakpoints` answer matched to the lines that were requested (same order).
pub fn parse_breakpoint_results(body: &Value, requested: &[usize]) -> Vec<BpResult> {
    let arr = body.get("breakpoints").and_then(Value::as_array);
    requested
        .iter()
        .enumerate()
        .map(|(i, &asked)| {
            let b = arr.and_then(|a| a.get(i));
            BpResult {
                line: b.and_then(|b| b.get("line")).and_then(Value::as_u64).map_or(asked, |l| l as usize),
                verified: b.and_then(|b| b.get("verified")).and_then(Value::as_bool).unwrap_or(false),
                message: b.and_then(|b| b.get("message")).and_then(Value::as_str).map(str::to_string),
            }
        })
        .collect()
}

/// Pick the scope to show as "locals": the first non-expensive one, else the first.
pub fn parse_locals_scope(body: &Value) -> Option<i64> {
    let arr = body.get("scopes")?.as_array()?;
    let pick =
        arr.iter().find(|s| s.get("expensive").and_then(Value::as_bool) != Some(true)).or_else(|| arr.first())?;
    pick.get("variablesReference").and_then(Value::as_i64)
}

// ───────────────────────────── session ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Running,
    Stopped { thread: i64 },
    Terminated,
}

pub struct Session {
    pub state: State,
    pub frames: Vec<Frame>,
    pub locals: Vec<Var>,
    pub output: Vec<String>,
    pub breakpoints: HashMap<String, Vec<usize>>,
    pub last_stop: Option<Stop>,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    pub stop_on_entry: bool,
    /// Replaces the gdb-style `launch` sent after `initialize`: `(command, arguments)`.
    pub start_request: Option<(String, Value)>,
    /// Send `setExceptionBreakpoints` before `configurationDone` (js-debug expects it).
    pub exception_bps: bool,
    /// Breakpoints with conditions / log messages; wins over `breakpoints` for the same file.
    pub bp_specs: HashMap<String, Vec<BpSpec>>,
    /// What the adapter said about each file's breakpoints (verified or not).
    pub bp_results: HashMap<String, Vec<BpResult>>,
    pub threads: Vec<ThreadInfo>,
    /// Stack of each thread whose stack was fetched (the stopped one always; others on demand).
    pub thread_frames: HashMap<i64, Vec<Frame>>,
    /// Threads shown expanded in the call stack.
    pub thread_open: HashSet<i64>,
    /// The frame the Variables / Watch views follow: `(thread, index in its stack)`.
    pub selected: Option<(i64, usize)>,
    pub scopes: Vec<Scope>,
    /// Loaded children of a `variablesReference` (scopes and expanded variables).
    pub var_children: HashMap<i64, Vec<Var>>,
    /// Tree path ("Locals/obj/field") of each reference seen, so expansions survive stepping.
    pub ref_path: HashMap<i64, String>,
    /// Tree paths the user expanded.
    pub expanded: HashSet<String>,
    /// Non-expensive scopes the user folded (they are open by default).
    pub collapsed: HashSet<String>,
    pub watch: Vec<String>,
    /// Watch expression -> `(value, type)` or the error text.
    pub watch_results: HashMap<String, Result<(String, String), String>>,
    /// The last hover evaluation: `(expression, value or error)`.
    pub hover_result: Option<(String, Result<String, String>)>,
    locals_ref: i64,
    last_thread: i64,
}

impl Session {
    pub fn new(program: &str, args: &[String], cwd: &Path, stop_on_entry: bool) -> Session {
        Session {
            state: State::Idle,
            frames: Vec::new(),
            locals: Vec::new(),
            output: Vec::new(),
            breakpoints: HashMap::new(),
            last_stop: None,
            program: program.to_string(),
            args: args.to_vec(),
            cwd: cwd.to_path_buf(),
            stop_on_entry,
            start_request: None,
            exception_bps: false,
            bp_specs: HashMap::new(),
            bp_results: HashMap::new(),
            threads: Vec::new(),
            thread_frames: HashMap::new(),
            thread_open: HashSet::new(),
            selected: None,
            scopes: Vec::new(),
            var_children: HashMap::new(),
            ref_path: HashMap::new(),
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            watch: Vec::new(),
            watch_results: HashMap::new(),
            hover_result: None,
            locals_ref: 0,
            last_thread: 1,
        }
    }

    /// A session whose handshake is `initialize` + `<command>(arguments)` (js-debug style).
    pub fn with_request(command: &str, arguments: Value) -> Session {
        let mut s = Session::new("", &[], Path::new("."), false);
        s.start_request = Some((command.to_string(), arguments));
        s.exception_bps = true;
        s
    }

    /// Begin the handshake. Breakpoints may be added before or after (re-sent when live).
    pub fn start(&mut self, dap: &mut Dap) {
        dap.initialize();
    }

    /// Replace the breakpoints of one file; sent immediately if the debuggee is live.
    pub fn set_breakpoints(&mut self, path: &str, lines: &[usize], dap: &mut Dap) {
        self.breakpoints.insert(path.to_string(), lines.to_vec());
        self.bp_specs.remove(path);
        if matches!(self.state, State::Running | State::Stopped { .. }) {
            dap.set_breakpoints(path, lines);
        }
    }

    /// Like [`set_breakpoints`](Self::set_breakpoints) with conditions and logpoints.
    pub fn set_breakpoint_specs(&mut self, path: &str, specs: Vec<BpSpec>, dap: &mut Dap) {
        self.breakpoints.insert(path.to_string(), specs.iter().map(|b| b.line).collect());
        if matches!(self.state, State::Running | State::Stopped { .. }) {
            dap.set_breakpoints_spec(path, &specs);
        }
        self.bp_specs.insert(path.to_string(), specs);
    }

    /// Interrupt a running debuggee (the adapter answers with a `stopped` event).
    pub fn pause(&mut self, dap: &mut Dap) {
        if self.state == State::Running {
            dap.pause(self.last_thread.max(1));
        }
    }

    /// The frame the Variables and Watch views follow.
    pub fn selected_frame(&self) -> Option<&Frame> {
        match self.selected {
            Some((t, i)) => self.thread_frames.get(&t).and_then(|f| f.get(i)),
            None => self.frames.first(),
        }
    }

    /// Follow frame `idx` of thread `tid`: its scopes and the watch values are fetched again.
    pub fn select_frame(&mut self, tid: i64, idx: usize, dap: &mut Dap) {
        let Some(f) = self.thread_frames.get(&tid).and_then(|v| v.get(idx)).cloned() else { return };
        if !matches!(self.state, State::Stopped { .. }) {
            return;
        }
        self.selected = Some((tid, idx));
        self.scopes.clear();
        self.var_children.clear();
        self.ref_path.clear();
        self.locals.clear();
        dap.scopes(f.id);
        self.refresh_watch(dap);
    }

    /// Fold or unfold a thread of the call stack (its stack is fetched the first time).
    pub fn toggle_thread(&mut self, tid: i64, dap: &mut Dap) {
        if !self.thread_open.remove(&tid) {
            self.thread_open.insert(tid);
            if !self.thread_frames.contains_key(&tid) && matches!(self.state, State::Stopped { .. }) {
                dap.stack_trace(tid);
            }
        }
    }

    /// Expand or collapse the variable at tree path `key`; children are fetched when missing.
    pub fn toggle_var(&mut self, key: &str, vars_ref: i64, dap: &mut Dap) {
        if self.expanded.remove(key) {
            return;
        }
        self.expanded.insert(key.to_string());
        if vars_ref > 0 && !self.var_children.contains_key(&vars_ref) {
            dap.variables(vars_ref);
        }
    }

    /// Fold or unfold a scope row (`Locals`, ...).
    pub fn toggle_scope(&mut self, name: &str, vars_ref: i64, expensive: bool, dap: &mut Dap) {
        if expensive {
            return self.toggle_var(name, vars_ref, dap);
        }
        if !self.collapsed.remove(name) {
            self.collapsed.insert(name.to_string());
        }
    }

    /// Replace the watch list and evaluate it in the selected frame.
    pub fn set_watch(&mut self, list: Vec<String>, dap: &mut Dap) {
        self.watch_results.retain(|k, _| list.contains(k));
        self.watch = list;
        self.refresh_watch(dap);
    }

    pub fn refresh_watch(&mut self, dap: &mut Dap) {
        if !matches!(self.state, State::Stopped { .. }) {
            return;
        }
        let fid = self.selected_frame().map(|f| f.id);
        for e in self.watch.clone() {
            dap.evaluate_in(&e, fid, "watch", Pending::EvalWatch(e.clone()));
        }
    }

    /// Debug Console input: echoed, then evaluated in the selected frame.
    pub fn evaluate_repl(&mut self, expr: &str, dap: &mut Dap) {
        self.output.push(crate::debug_model::console_line(crate::debug_model::ConsoleKind::Echo, expr));
        if !matches!(self.state, State::Stopped { .. }) {
            let m = "the program is not stopped: evaluate works at a breakpoint";
            self.output.push(crate::debug_model::console_line(crate::debug_model::ConsoleKind::Error, m));
            return;
        }
        let fid = self.selected_frame().map(|f| f.id);
        dap.evaluate_in(expr, fid, "repl", Pending::EvalRepl(expr.to_string()));
    }

    /// Evaluate for the hover popup (answer lands in `hover_result`).
    pub fn evaluate_hover(&mut self, expr: &str, dap: &mut Dap) {
        if !matches!(self.state, State::Stopped { .. }) {
            return;
        }
        let fid = self.selected_frame().map(|f| f.id);
        dap.evaluate_in(expr, fid, "hover", Pending::EvalHover(expr.to_string()));
    }

    fn on_children(&mut self, r: i64, vars: Vec<Var>, dap: &mut Dap) {
        let base = self.ref_path.get(&r).cloned().unwrap_or_default();
        for v in &vars {
            if v.vars_ref > 0 {
                let key = format!("{base}/{}", v.name);
                if self.expanded.contains(&key) {
                    dap.variables(v.vars_ref);
                }
                self.ref_path.insert(v.vars_ref, key);
            }
        }
        self.var_children.insert(r, vars);
    }

    pub fn resume(&mut self, dap: &mut Dap) {
        if let State::Stopped { thread } = self.state {
            dap.continue_(thread);
            self.state = State::Running;
            self.clear_stop();
        }
    }

    fn step(&mut self, dap: &mut Dap, f: fn(&mut Dap, i64) -> i64) {
        if let State::Stopped { thread } = self.state {
            f(dap, thread);
            self.state = State::Running;
            self.clear_stop();
        }
    }
    pub fn step_over(&mut self, dap: &mut Dap) {
        self.step(dap, Dap::next);
    }
    pub fn step_in(&mut self, dap: &mut Dap) {
        self.step(dap, Dap::step_in);
    }
    pub fn step_out(&mut self, dap: &mut Dap) {
        self.step(dap, Dap::step_out);
    }

    fn clear_stop(&mut self) {
        self.frames.clear();
        self.locals.clear();
        self.thread_frames.clear();
        self.thread_open.clear();
        self.selected = None;
        self.scopes.clear();
        self.var_children.clear();
        self.ref_path.clear();
        self.hover_result = None;
    }

    pub fn handle(&mut self, msg: &DapMsg, dap: &mut Dap) {
        match msg {
            DapMsg::Exited => self.state = State::Terminated,
            // Reverse requests are answered by the owner (see `MultiSession`).
            DapMsg::ReverseRequest { .. } => {}
            DapMsg::Event { event, body } => match event.as_str() {
                "initialized" => {
                    for (path, lines) in &self.breakpoints {
                        match self.bp_specs.get(path) {
                            Some(specs) => dap.set_breakpoints_spec(path, specs),
                            None => dap.set_breakpoints(path, lines),
                        };
                    }
                    if self.exception_bps {
                        dap.set_exception_breakpoints();
                    }
                    dap.configuration_done();
                    if self.state == State::Idle {
                        self.state = State::Running;
                    }
                }
                "stopped" => {
                    let stop = parse_stopped(body);
                    self.state = State::Stopped { thread: stop.thread_id };
                    self.clear_stop();
                    self.last_thread = stop.thread_id;
                    self.selected = Some((stop.thread_id, 0));
                    self.thread_open.insert(stop.thread_id);
                    dap.threads();
                    dap.stack_trace(stop.thread_id);
                    self.last_stop = Some(stop);
                }
                "breakpoint" => {
                    // A breakpoint changed after the fact (verified late, or moved to another line).
                    if let Some(b) = body.get("breakpoint")
                        && let Some(line) = b.get("line").and_then(Value::as_u64)
                    {
                        let verified = b.get("verified").and_then(Value::as_bool).unwrap_or(true);
                        for r in self.bp_results.values_mut().flatten() {
                            if r.line == line as usize {
                                r.verified = verified;
                            }
                        }
                    }
                }
                "continued" => {
                    self.state = State::Running;
                    self.clear_stop();
                }
                "exited" | "terminated" => self.state = State::Terminated,
                "output" => {
                    if let Some(t) = body.get("output").and_then(Value::as_str) {
                        let cat = body.get("category").and_then(Value::as_str).unwrap_or("console");
                        if cat == "stderr" {
                            let k = crate::debug_model::ConsoleKind::Stderr;
                            self.output.extend(t.lines().map(|l| crate::debug_model::console_line(k, l)));
                        } else if cat != "telemetry" {
                            self.output.extend(t.lines().map(str::to_string));
                        }
                    }
                }
                _ => {}
            },
            DapMsg::Response { seq_for, command, success, body, message } => {
                let what = dap.take_pending(*seq_for).unwrap_or(Pending::Other);
                if !success {
                    let text = message
                        .clone()
                        .or_else(|| body.pointer("/error/format").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_else(|| format!("{command} failed"));
                    match &what {
                        Pending::EvalRepl(_) => {
                            let k = crate::debug_model::ConsoleKind::Error;
                            self.output.push(crate::debug_model::console_line(k, &text));
                            return;
                        }
                        Pending::EvalWatch(e) => {
                            self.watch_results.insert(e.clone(), Err(text));
                            return;
                        }
                        Pending::EvalHover(e) => {
                            self.hover_result = Some((e.clone(), Err(text)));
                            return;
                        }
                        _ => {}
                    }
                    let stale = matches!(
                        what,
                        Pending::StackTrace(_) | Pending::Scopes(_) | Pending::Locals | Pending::Variables(_)
                    ) && !matches!(self.state, State::Stopped { .. });
                    if stale {
                        return;
                    }
                    if let Some(m) = message {
                        self.output.push(format!("[dap] {command} failed: {m}"));
                    }
                    return;
                }
                match what {
                    Pending::Initialize => {
                        if let Some((cmd, args)) = self.start_request.clone() {
                            dap.request(&cmd, args, Pending::Launch);
                            return;
                        }
                        dap.launch(&self.program, &self.args, &self.cwd, self.stop_on_entry);
                    }
                    Pending::SetBreakpoints(path) => {
                        let requested: Vec<usize> = match self.bp_specs.get(&path) {
                            Some(s) => s.iter().map(|b| b.line).collect(),
                            None => self.breakpoints.get(&path).cloned().unwrap_or_default(),
                        };
                        self.bp_results.insert(path, parse_breakpoint_results(body, &requested));
                    }
                    Pending::Threads => {
                        if matches!(self.state, State::Stopped { .. }) {
                            self.threads = parse_threads(body);
                        }
                    }
                    Pending::StackTrace(tid) => {
                        if !matches!(self.state, State::Stopped { .. }) {
                            return;
                        }
                        let fr = parse_stack_trace(body);
                        self.thread_frames.insert(tid, fr.clone());
                        if tid == self.last_thread {
                            self.frames = fr;
                            if let Some(top) = self.frames.first() {
                                dap.scopes(top.id);
                            }
                            self.refresh_watch(dap);
                        }
                    }
                    Pending::Scopes(_) => {
                        if !matches!(self.state, State::Stopped { .. }) {
                            return;
                        }
                        self.scopes = parse_scopes(body);
                        for s in &self.scopes {
                            self.ref_path.insert(s.vars_ref, s.name.clone());
                        }
                        let first = parse_locals_scope(body);
                        if let Some(r) = first {
                            self.locals_ref = r;
                            dap.request("variables", json!({"variablesReference": r}), Pending::Locals);
                        }
                        let others: Vec<i64> = self
                            .scopes
                            .iter()
                            .filter(|s| !s.expensive && s.vars_ref > 0 && Some(s.vars_ref) != first)
                            .map(|s| s.vars_ref)
                            .collect();
                        for r in others {
                            dap.variables(r);
                        }
                        // A scope the user opened earlier (an expensive one) is loaded again.
                        let reopen: Vec<i64> = self
                            .scopes
                            .iter()
                            .filter(|s| s.expensive && s.vars_ref > 0 && self.expanded.contains(&s.name))
                            .map(|s| s.vars_ref)
                            .collect();
                        for r in reopen {
                            dap.variables(r);
                        }
                    }
                    Pending::Locals => {
                        if !matches!(self.state, State::Stopped { .. }) {
                            return;
                        }
                        let vars = parse_variables(body);
                        self.locals = vars.clone();
                        let r = self.locals_ref;
                        self.on_children(r, vars, dap);
                    }
                    Pending::Variables(r) => {
                        if matches!(self.state, State::Stopped { .. }) {
                            let vars = parse_variables(body);
                            self.on_children(r, vars, dap);
                        }
                    }
                    Pending::EvalRepl(_) => {
                        let (res, ty) = eval_text(body);
                        let k = crate::debug_model::ConsoleKind::Result;
                        let line = if ty.is_empty() { res } else { format!("{res}  ({ty})") };
                        self.output.push(crate::debug_model::console_line(k, &line));
                        // The evaluation may have changed state the views show.
                        let fid = self.selected_frame().map(|f| f.id);
                        if let Some(f) = fid {
                            dap.scopes(f);
                        }
                        self.refresh_watch(dap);
                    }
                    Pending::EvalWatch(e) => {
                        let (res, ty) = eval_text(body);
                        self.watch_results.insert(e, Ok((res, ty)));
                    }
                    Pending::EvalHover(e) => {
                        let (res, ty) = eval_text(body);
                        let shown = if ty.is_empty() { res } else { format!("{res}\n// {ty}") };
                        self.hover_result = Some((e, Ok(shown)));
                    }
                    _ => {}
                }
            }
        }
    }
}

// ───────────────────────────── js-debug / Node ─────────────────────────────

/// Start js-debug's TCP DAP server: `node server_js <port> 127.0.0.1`. Returns the process
/// and the port once the server accepts connections. The caller owns (and must kill) the child.
pub fn spawn_js_debug(node_bin: &str, server_js: &Path, cwd: &Path, _wake: Wake) -> io::Result<(Child, u16)> {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0")?;
        l.local_addr()?.port()
    };
    let mut child = Command::new(node_bin)
        .arg(server_js)
        .arg(port.to_string())
        .arg("127.0.0.1")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok((child, port));
        }
        if let Ok(Some(st)) = child.try_wait() {
            return Err(io::Error::other(format!("js-debug exited early: {st}")));
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("js-debug did not start listening"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn node_major(node_bin: &str) -> Option<u32> {
    let out = Command::new(node_bin).arg("--version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().trim_start_matches('v').split('.').next()?.parse().ok()
}

/// Launch configuration for js-debug's `pwa-node`. TypeScript files run directly (node strips
/// types natively from v23; older nodes get `--experimental-strip-types`).
pub fn node_launch_args(program: &Path) -> Value {
    let ext = program.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let cwd = program.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let mut v = json!({
        "type": "pwa-node", "request": "launch",
        "name": program.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "program": program.to_string_lossy(),
        "cwd": cwd.to_string_lossy(),
        "runtimeExecutable": "node",
        "args": [],
        "stopOnEntry": false,
        "console": "internalConsole",
        "sourceMaps": true,
        "skipFiles": ["<node_internals>/**"],
        "outputCapture": "std",
    });
    if matches!(ext.as_str(), "ts" | "mts" | "cts") && node_major("node").is_some_and(|m| m < 23) {
        v["runtimeArgs"] = json!(["--experimental-strip-types"]);
    }
    v
}

// ───────────────────────────── multi-session ─────────────────────────────

/// A root DAP connection plus the child sessions js-debug asks for through `startDebugging`,
/// presented to the UI as one debugger. Drive it with `handle_all` on every wake.
pub struct MultiSession {
    pub root: Dap,
    pub root_session: Session,
    pub children: Vec<(Dap, Session)>,
    pub host: String,
    pub port: u16,
    wake: Wake,
    /// Breakpoints applied to every session (path -> lines).
    pub breakpoints: HashMap<String, Vec<usize>>,
    /// Same with conditions / logpoints (wins over `breakpoints` for a file).
    pub bp_specs: HashMap<String, Vec<BpSpec>>,
    output: Vec<String>,
    /// Adapter process, killed on drop.
    adapter: Option<Child>,
    /// Every raw message seen (debug aid); only kept when `trace` is set.
    pub trace: bool,
    pub log: Vec<String>,
}

impl Drop for MultiSession {
    fn drop(&mut self) {
        // Close the sockets first (fields drop after this), then kill the adapter.
        if let Some(c) = self.adapter.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl MultiSession {
    /// Connect to a running js-debug server and begin the handshake (`initialize` + `launch`).
    pub fn connect(host: &str, port: u16, launch: Value, wake: Wake) -> io::Result<MultiSession> {
        let mut root = Dap::connect_tcp(host, port, wake.clone())?;
        root.adapter_id = "pwa-node".into();
        let root_session = Session::with_request("launch", launch);
        Ok(MultiSession {
            root,
            root_session,
            children: Vec::new(),
            host: host.to_string(),
            port,
            wake,
            breakpoints: HashMap::new(),
            bp_specs: HashMap::new(),
            output: Vec::new(),
            adapter: None,
            trace: false,
            log: Vec::new(),
        })
    }

    /// Spawn js-debug, connect, and debug `program` with `node_launch_args`.
    pub fn start_node(node_bin: &str, server_js: &Path, program: &Path, wake: Wake) -> io::Result<MultiSession> {
        Self::start_node_with(node_bin, server_js, program, None, |_| {}, wake)
    }

    /// [`start_node`](Self::start_node) with a working directory and a last chance to adjust the
    /// launch configuration (program arguments, runtime flags).
    pub fn start_node_with(
        node_bin: &str,
        server_js: &Path,
        program: &Path,
        cwd: Option<&Path>,
        tweak: impl FnOnce(&mut Value),
        wake: Wake,
    ) -> io::Result<MultiSession> {
        let cwd = cwd.or(program.parent()).unwrap_or(Path::new("."));
        let (child, port) = spawn_js_debug(node_bin, server_js, cwd, wake.clone())?;
        let mut args = node_launch_args(program);
        args["runtimeExecutable"] = json!(node_bin);
        args["cwd"] = json!(cwd.to_string_lossy());
        tweak(&mut args);
        match MultiSession::connect("127.0.0.1", port, args, wake) {
            Ok(mut m) => {
                m.adapter = Some(child);
                Ok(m)
            }
            Err(e) => {
                let mut c = child;
                let _ = c.kill();
                let _ = c.wait();
                Err(e)
            }
        }
    }

    /// Hand over the adapter process so it is killed when this is dropped.
    pub fn own_adapter(&mut self, child: Child) {
        self.adapter = Some(child);
    }

    /// Begin the handshake. Breakpoints may be set before or after.
    pub fn start(&mut self) {
        self.root_session.start(&mut self.root);
    }

    pub fn stopped_child(&self) -> Option<usize> {
        self.children.iter().position(|(_, s)| matches!(s.state, State::Stopped { .. }))
    }

    pub fn state(&self) -> State {
        if let Some(i) = self.stopped_child() {
            return self.children[i].1.state.clone();
        }
        let live = self.children.iter().any(|(_, s)| s.state != State::Terminated);
        if !self.children.is_empty() && !live {
            return State::Terminated;
        }
        if live {
            return State::Running;
        }
        self.root_session.state.clone()
    }

    pub fn frames(&self) -> &[Frame] {
        self.stopped_child().map(|i| self.children[i].1.frames.as_slice()).unwrap_or(&[])
    }

    pub fn locals(&self) -> &[Var] {
        self.stopped_child().map(|i| self.children[i].1.locals.as_slice()).unwrap_or(&[])
    }

    pub fn last_stop(&self) -> Option<&Stop> {
        self.stopped_child().and_then(|i| self.children[i].1.last_stop.as_ref())
    }

    /// Merged output lines of every session.
    pub fn output(&self) -> &[String] {
        &self.output
    }

    pub fn clear_output(&mut self) {
        self.output.clear();
    }

    pub fn resume(&mut self) {
        if let Some(i) = self.stopped_child() {
            let (d, s) = &mut self.children[i];
            s.resume(d);
        }
    }
    pub fn step_over(&mut self) {
        if let Some(i) = self.stopped_child() {
            let (d, s) = &mut self.children[i];
            s.step_over(d);
        }
    }
    pub fn step_in(&mut self) {
        if let Some(i) = self.stopped_child() {
            let (d, s) = &mut self.children[i];
            s.step_in(d);
        }
    }
    pub fn step_out(&mut self) {
        if let Some(i) = self.stopped_child() {
            let (d, s) = &mut self.children[i];
            s.step_out(d);
        }
    }

    /// Interrupt every running session.
    pub fn pause(&mut self) {
        for (d, s) in &mut self.children {
            s.pause(d);
        }
    }

    /// Replace the breakpoints of one file (with conditions / logpoints) in the root and every child.
    pub fn set_breakpoint_specs(&mut self, path: &str, specs: &[BpSpec]) {
        self.breakpoints.insert(path.to_string(), specs.iter().map(|b| b.line).collect());
        self.bp_specs.insert(path.to_string(), specs.to_vec());
        self.root_session.set_breakpoint_specs(path, specs.to_vec(), &mut self.root);
        for (d, s) in &mut self.children {
            s.set_breakpoint_specs(path, specs.to_vec(), d);
        }
    }

    /// Replace the breakpoints of one file in the root and every child.
    pub fn set_breakpoints(&mut self, path: &str, lines: &[usize]) {
        self.bp_specs.remove(path);
        self.breakpoints.insert(path.to_string(), lines.to_vec());
        self.root_session.set_breakpoints(path, lines, &mut self.root);
        for (d, s) in &mut self.children {
            s.set_breakpoints(path, lines, d);
        }
    }

    /// Disconnect every session (terminating the debuggee) and wait briefly for the adapter.
    pub fn stop(&mut self) {
        for (d, s) in &mut self.children {
            if s.state != State::Terminated {
                d.disconnect(true);
            }
        }
        self.root.disconnect(true);
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            self.handle_all();
            let done = self.root_session.state == State::Terminated
                && self.children.iter().all(|(_, s)| s.state == State::Terminated);
            if done {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.root_session.state = State::Terminated;
        for (_, s) in &mut self.children {
            s.state = State::Terminated;
        }
        if let Some(c) = self.adapter.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.adapter = None;
    }

    fn open_child(&mut self, request: &str, configuration: Value) {
        let mut dap = match Dap::connect_tcp(&self.host, self.port, self.wake.clone()) {
            Ok(d) => d,
            Err(e) => {
                self.output.push(format!("[dap] child connect failed: {e}"));
                return;
            }
        };
        dap.adapter_id = "pwa-node".into();
        let mut s = Session::with_request(request, configuration);
        s.breakpoints = self.breakpoints.clone();
        s.bp_specs = self.bp_specs.clone();
        s.start(&mut dap);
        self.children.push((dap, s));
    }

    /// Drain every session's receiver; returns true if anything changed.
    pub fn handle_all(&mut self) -> bool {
        let mut changed = false;
        let mut new_children: Vec<(String, Value)> = Vec::new();

        // Root first, then each child.
        while let Ok(m) = self.root.rx.try_recv() {
            changed = true;
            if self.trace {
                self.log.push(format!("root < {m:?}"));
            }
            if let DapMsg::ReverseRequest { seq, command, arguments } = &m {
                self.root.respond(*seq, command, true, json!({}));
                new_children.push(Self::child_config(arguments));
            }
            self.root_session.handle(&m, &mut self.root);
        }
        for i in 0..self.children.len() {
            let (d, s) = &mut self.children[i];
            while let Ok(m) = d.rx.try_recv() {
                changed = true;
                if self.trace {
                    self.log.push(format!("child{i} < {m:?}"));
                }
                if let DapMsg::ReverseRequest { seq, command, arguments } = &m {
                    d.respond(*seq, command, true, json!({}));
                    new_children.push(Self::child_config(arguments));
                }
                s.handle(&m, d);
            }
        }
        for (req, cfg) in new_children {
            self.open_child(&req, cfg);
        }

        // Merge output.
        let mut out = std::mem::take(&mut self.root_session.output);
        for (_, s) in &mut self.children {
            out.append(&mut s.output);
        }
        if !out.is_empty() {
            self.output.append(&mut out);
            changed = true;
        }
        changed
    }

    fn child_config(arguments: &Value) -> (String, Value) {
        let cfg = arguments.get("configuration").cloned().unwrap_or(Value::Null);
        let req = arguments
            .get("request")
            .or_else(|| cfg.get("request"))
            .and_then(Value::as_str)
            .unwrap_or("attach")
            .to_string();
        (req, cfg)
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn frame_reader_splits_across_chunks() {
        let a = frame(&json!({"type":"event","event":"x"}));
        let b = frame(&json!({"type":"event","event":"y"}));
        let all: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
        let mut fr = FrameReader::default();
        let mut got = Vec::new();
        for c in all.chunks(7) {
            got.extend(fr.push(c));
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[1]["event"], "y");
    }

    #[test]
    fn parses_bodies() {
        let st = json!({"stackFrames":[{"id":3,"name":"main","source":{"path":"/a/b.rs"},"line":4,"column":2},{"id":4,"name":"?","line":0,"column":0}]});
        let f = parse_stack_trace(&st);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], Frame { id: 3, name: "main".into(), path: Some("/a/b.rs".into()), line: 4, col: 2 });
        assert_eq!(f[1].path, None);
        let v = parse_variables(&json!({"variables":[{"name":"x","value":"1","type":"i32","variablesReference":0}]}));
        assert_eq!(v, vec![Var { name: "x".into(), value: "1".into(), type_: "i32".into(), vars_ref: 0 }]);
        let s = parse_stopped(&json!({"reason":"breakpoint","threadId":2,"description":"hit"}));
        assert_eq!(s, Stop { reason: "breakpoint".into(), thread_id: 2, text: "hit".into() });
        assert_eq!(
            parse_locals_scope(&json!({"scopes":[{"expensive":true,"variablesReference":9},{"variablesReference":5}]})),
            Some(5)
        );
    }

    fn pump(dap: &mut Dap, s: &mut Session, until: impl Fn(&Session) -> bool, secs: u64) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if until(s) {
                return true;
            }
            match dap.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(m) => s.handle(&m, dap),
                Err(_) => {}
            }
        }
        until(s)
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn gdb_session_breakpoint_and_terminate() {
        let have = |c: &str| {
            Command::new(c)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if !have("gdb") || !have("rustc") {
            eprintln!("skipping: gdb or rustc missing");
            return;
        }
        let dir = std::env::temp_dir().join(format!("lumen-dap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("prog.rs");
        let bin = dir.join("prog");
        std::fs::write(
            &src,
            "fn add(a: i32, b: i32) -> i32 {\n    let sum = a + b;\n    sum\n}\n\nfn main() {\n    let mut total = 0;\n    for i in 0..3 {\n        total = add(total, i);\n    }\n    println!(\"{}\", total);\n}\n",
        )
        .unwrap();
        let ok = Command::new("rustc")
            .args(["-g", "-C", "opt-level=0", "-o"])
            .arg(&bin)
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("skipping: rustc failed");
            return;
        }
        let mut dap = match Dap::spawn("gdb", &["--interpreter=dap"], &dir, Arc::new(|| {})) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skipping: cannot spawn gdb: {e}");
                return;
            }
        };
        let src_s = src.to_string_lossy().to_string();
        let mut s = Session::new(&bin.to_string_lossy(), &[], &dir, false);
        s.breakpoints.insert(src_s.clone(), vec![2]);
        s.start(&mut dap);

        let stopped = pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. }) && !s.locals.is_empty(), 20);
        eprintln!("state={:?} frames={:?} locals={:?} output={:?}", s.state, s.frames, s.locals, s.output);
        assert!(stopped, "never stopped at breakpoint");
        let top = &s.frames[0];
        assert_eq!(top.line, 2);
        assert!(top.path.as_deref().is_some_and(|p| p.ends_with("prog.rs")), "{top:?}");
        assert!(s.locals.iter().any(|v| v.name == "a" || v.name == "sum"));

        // Keep continuing past the remaining two breakpoint hits until the program ends.
        let end = Instant::now() + Duration::from_secs(20);
        while s.state != State::Terminated && Instant::now() < end {
            s.resume(&mut dap);
            pump(&mut dap, &mut s, |s| matches!(s.state, State::Stopped { .. } | State::Terminated), 5);
        }
        assert_eq!(s.state, State::Terminated);
        dap.disconnect(true);
        drop(dap);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn js_debug_server() -> Option<PathBuf> {
        let p = PathBuf::from(std::env::var("HOME").ok()?)
            .join(".local/share/lumen-text/js-debug/js-debug/src/dapDebugServer.js");
        p.exists().then_some(p)
    }

    fn wait_multi(m: &mut MultiSession, until: impl Fn(&MultiSession) -> bool, secs: u64) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            m.handle_all();
            if until(m) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        m.handle_all();
        until(m)
    }

    fn js_debug_case(ext: &str) {
        let Some(server) = js_debug_server() else {
            eprintln!("skipping: js-debug not installed");
            return;
        };
        if node_major("node").is_none() {
            eprintln!("skipping: node missing");
            return;
        }
        let dir = std::env::temp_dir().join(format!("lumen-dap-js-{}-{ext}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join(format!("prog.{ext}"));
        let src = if ext == "ts" {
            "function add(a: number, b: number): number {\n  const sum = a + b;\n  return sum;\n}\n\nlet total = 0;\nfor (let i = 0; i < 3; i++) {\n  total = add(total, i);\n}\nconsole.log('total', total);\n"
        } else {
            "function add(a, b) {\n  const sum = a + b;\n  return sum;\n}\n\nlet total = 0;\nfor (let i = 0; i < 3; i++) {\n  total = add(total, i);\n}\nconsole.log('total', total);\n"
        };
        std::fs::write(&prog, src).unwrap();

        let mut m = MultiSession::start_node("node", &server, &prog, Arc::new(|| {})).expect("start js-debug");
        m.trace = std::env::var("DAP_TRACE").is_ok();
        let prog_s = prog.to_string_lossy().to_string();
        m.set_breakpoints(&prog_s, &[2]);
        m.start();

        let ok = wait_multi(&mut m, |m| matches!(m.state(), State::Stopped { .. }) && !m.locals().is_empty(), 30);
        if m.trace {
            for l in &m.log {
                eprintln!("{l}");
            }
        }
        eprintln!(
            "[{ext}] state={:?} frames={:?} locals={:?} output={:?} children={}",
            m.state(),
            m.frames(),
            m.locals(),
            m.output(),
            m.children.len()
        );
        if !ok {
            m.stop();
            panic!("never stopped at breakpoint ({ext})");
        }
        let top = m.frames()[0].clone();
        assert_eq!(top.line, 2, "{top:?}");
        assert!(top.path.as_deref().is_some_and(|p| p.ends_with(&format!("prog.{ext}"))), "{top:?}");
        assert!(m.locals().iter().any(|v| v.name == "a"), "{:?}", m.locals());
        assert!(m.locals().iter().any(|v| v.name == "b"), "{:?}", m.locals());

        let end = Instant::now() + Duration::from_secs(30);
        while m.state() != State::Terminated && Instant::now() < end {
            m.resume();
            wait_multi(&mut m, |m| matches!(m.state(), State::Stopped { .. } | State::Terminated), 10);
        }
        let fin = m.state();
        eprintln!("[{ext}] final={fin:?} output={:?}", m.output());
        assert_eq!(fin, State::Terminated);
        assert!(m.output().iter().any(|l| l.contains("total")), "program output not captured: {:?}", m.output());
        m.stop();
        drop(m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn js_debug_javascript_breakpoint() {
        js_debug_case("js");
    }

    #[test]
    fn js_debug_typescript_breakpoint() {
        js_debug_case("ts");
    }

    #[test]
    fn launch_args_shape() {
        let v = node_launch_args(Path::new("/x/a.ts"));
        assert_eq!(v["type"], "pwa-node");
        assert_eq!(v["console"], "internalConsole");
        assert_eq!(v["program"], "/x/a.ts");
    }
}
