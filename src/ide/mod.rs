//! IDE bridge: Lumen as the "IDE" Claude Code connects to, the way it connects to VS Code or
//! JetBrains. A WebSocket server on 127.0.0.1 (random port) speaks JSON-RPC framed as MCP; a lock
//! file in `~/.claude/ide/` tells the CLI where it is and which token to send. The agent the AI
//! panel starts gets `CLAUDE_CODE_SSE_PORT` and connects by itself; any other `claude` can attach
//! with `/ide`.
//!
//! The same listener takes plain HTTP POSTs from Claude Code's `http` hooks (`/lumen/hook/…`,
//! bearer token), which report the agent's status more reliably than watching its output.
//!
//! Threads: one accepts, one per connection reads, one per connection writes (the UI thread only
//! queues bytes, so it never blocks on a socket). Everything the editor must answer goes to the UI
//! thread as an [`Event`] and wakes the event loop. The protocol modules (`ws`, `mcp`, `lock`) are
//! plain code covered by tests; the editor side lives in `app_aipanel::bridge`.
//!
//! Security: loopback only (bind and peer check), the auth token is compared in constant time and
//! never logged, the lock file is private, and frames, messages, heads and bodies are capped.

pub mod lock;
pub mod mcp;
#[cfg(test)]
mod tests;
pub mod ws;

use crate::lsp::Wake;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

/// Largest message (or frame) accepted: an `openDiff` carries a whole file.
pub const MAX_MESSAGE: usize = 32 << 20;
const MAX_HEAD: usize = 16 << 10;
const MAX_HOOK_BODY: usize = 1 << 20;
const MAX_CLIENTS: usize = 16;
/// Path prefix of the hook endpoints (`/lumen/hook/working`, …).
pub const HOOK_PREFIX: &str = "/lumen/hook/";
/// Header carrying the lock file's token in the WebSocket upgrade.
pub const AUTH_HEADER: &str = "x-claude-code-ide-authorization";
/// Environment variable holding the hooks' bearer token in the agent's environment.
pub const HOOK_ENV: &str = "LUMEN_HOOK_TOKEN";

/// What the connections tell the UI thread.
#[derive(Debug)]
pub enum Event {
    /// A client finished the (authenticated) WebSocket handshake.
    Open(u64),
    Closed(u64),
    /// `tools/call`: answer with [`Server::send`] (`openDiff` later, when the user decides).
    Call {
        client: u64,
        id: Value,
        name: String,
        args: Value,
    },
    Note {
        client: u64,
        method: String,
        params: Value,
    },
    /// A hook POST: the route after [`HOOK_PREFIX`] and the hook's JSON input.
    Hook {
        route: String,
        body: Value,
    },
}

struct Client {
    out: Sender<Vec<u8>>,
    sock: TcpStream,
}

struct Shared {
    token: String,
    hook_token: String,
    tx: Mutex<Sender<Event>>,
    wake: Wake,
    clients: Mutex<HashMap<u64, Client>>,
    next: AtomicU64,
    stop: AtomicBool,
}

impl Shared {
    fn emit(&self, e: Event) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(e);
        }
        (self.wake)();
    }
}

pub struct Server {
    port: u16,
    shared: Arc<Shared>,
    rx: Receiver<Event>,
    lock_dir: Option<PathBuf>,
    lock: Option<PathBuf>,
}

impl Server {
    /// Listen on a random loopback port and (with `lock_dir`) publish the lock file; stale lock
    /// files of dead Lumens are swept first.
    pub fn start(folders: Vec<PathBuf>, lock_dir: Option<PathBuf>, wake: Wake) -> io::Result<Server> {
        let listener = bind()?;
        let port = listener.local_addr()?.port();
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            token: random_hex(16),
            hook_token: random_hex(16),
            tx: Mutex::new(tx),
            wake,
            clients: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let sh = shared.clone();
        thread::Builder::new().name("ide-accept".into()).spawn(move || {
            for s in listener.incoming() {
                if sh.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(s) = s else { continue };
                let sh = sh.clone();
                let _ = thread::Builder::new().name("ide-conn".into()).spawn(move || serve(s, sh));
            }
        })?;
        let mut srv = Server { port, shared, rx, lock_dir, lock: None };
        // Without its lock file the CLI cannot find us, but hooks still work: keep listening.
        if let Err(e) = srv.set_folders(&folders) {
            eprintln!("lumen ide: could not write the lock file: {e}");
        }
        Ok(srv)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The lock file's auth token (what `X-Claude-Code-Ide-Authorization` must carry).
    #[cfg(test)]
    pub fn token(&self) -> &str {
        &self.shared.token
    }

    /// The hooks' bearer token (given to the agent as `$LUMEN_HOOK_TOKEN`).
    pub fn hook_token(&self) -> &str {
        &self.shared.hook_token
    }

    #[cfg(test)]
    pub fn lock_path(&self) -> Option<&std::path::Path> {
        self.lock.as_deref()
    }

    /// (Re)write the lock file for these workspace folders.
    pub fn set_folders(&mut self, folders: &[PathBuf]) -> io::Result<()> {
        let Some(dir) = self.lock_dir.as_deref() else { return Ok(()) };
        if self.lock.is_none() {
            lock::sweep(dir);
        }
        self.lock = Some(lock::write(dir, self.port, std::process::id(), folders, &self.shared.token)?);
        Ok(())
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Queue a JSON-RPC message to one client (never blocks).
    pub fn send(&self, client: u64, msg: &Value) {
        if let Ok(cs) = self.shared.clients.lock()
            && let Some(c) = cs.get(&client)
        {
            let _ = c.out.send(text_frame(msg));
        }
    }

    pub fn broadcast(&self, msg: &Value) {
        if let Ok(cs) = self.shared.clients.lock() {
            let f = text_frame(msg);
            for c in cs.values() {
                let _ = c.out.send(f.clone());
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Unblock `accept` with a connection of our own, then hang up on every client.
        let _ =
            TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)), Duration::from_millis(200));
        if let Ok(mut cs) = self.shared.clients.lock() {
            for (_, c) in cs.drain() {
                let _ = c.sock.shutdown(Shutdown::Both);
            }
        }
        if let Some(p) = self.lock.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A random port in 10000–65535 (as the IDE extensions do), else any free one.
fn bind() -> io::Result<TcpListener> {
    let r = random_bytes(32);
    for pair in r.chunks_exact(2) {
        let port = 10000 + (u16::from_le_bytes([pair[0], pair[1]]) % 55536);
        if let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            return Ok(l);
        }
    }
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
}

fn text_frame(msg: &Value) -> Vec<u8> {
    ws::encode(ws::OP_TEXT, msg.to_string().as_bytes(), None)
}

/// Bytes from the OS's CSPRNG (`/dev/urandom`); elsewhere SipHash under the std's OS-seeded
/// random keys, mixed with the time and the pid.
pub fn random_bytes(n: usize) -> Vec<u8> {
    #[cfg(unix)]
    {
        let mut b = vec![0u8; n];
        if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_ok() {
            return b;
        }
    }
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut out = Vec::with_capacity(n + 8);
    let mut i = 0u64;
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        out.extend_from_slice(&h.finish().to_le_bytes());
        i += 1;
    }
    out.truncate(n);
    out
}

/// `n` random bytes as lowercase hex.
pub fn random_hex(n: usize) -> String {
    random_bytes(n).iter().map(|b| format!("{b:02x}")).collect()
}

/// Equal without an early exit (the length is not secret: tokens have a fixed size).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Column `col` (in chars) of `line` counted in UTF-16 code units, as VS Code and LSP count.
pub fn utf16_col(line: &str, col: usize) -> usize {
    line.chars().take(col).map(char::len_utf16).sum()
}

/// The `--settings` JSON that makes Claude Code report its status to us: `http` hooks posting to
/// this server (merged with the user's own hooks — hook entries from every settings source add up).
pub fn hook_settings(port: u16) -> String {
    let h = |route: &str| {
        json!({
            "type": "http",
            "url": format!("http://127.0.0.1:{port}{HOOK_PREFIX}{route}"),
            "headers": { "Authorization": format!("Bearer ${HOOK_ENV}") },
            "allowedEnvVars": [HOOK_ENV],
            "timeout": 5,
        })
    };
    json!({
        "hooks": {
            "UserPromptSubmit": [{ "hooks": [h("working")] }],
            "Notification": [
                { "matcher": "permission_prompt", "hooks": [h("waiting")] },
                { "matcher": "idle_prompt", "hooks": [h("waiting")] },
            ],
            "Stop": [{ "hooks": [h("done")] }],
            "PostToolUse": [{ "matcher": "Edit|Write|MultiEdit", "hooks": [h("edited")] }],
        }
    })
    .to_string()
}

// ───────────── HTTP ─────────────

/// An HTTP request head.
#[derive(Debug, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Names in lowercase.
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// Parse a request head (everything before the blank line).
pub fn parse_head(head: &str) -> Option<Request> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(Request { method, path, headers })
}

/// Read up to the end of the head; returns it and the bytes read past it.
fn read_head(s: &mut TcpStream) -> io::Result<(Request, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let rest = buf[end + 4..].to_vec();
            let req = parse_head(&head).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad request"))?;
            return Ok((req, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "head too large"));
        }
    }
}

fn respond(s: &mut TcpStream, status: &str) {
    let _ = s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
}

fn serve(mut s: TcpStream, sh: Arc<Shared>) {
    if sh.stop.load(Ordering::SeqCst) || !s.peer_addr().is_ok_and(|a| a.ip().is_loopback()) {
        return;
    }
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
    let Ok((req, rest)) = read_head(&mut s) else { return };
    let upgrade = req.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    if upgrade {
        ws_session(s, &req, rest, &sh);
    } else if req.method == "POST" && req.path.starts_with(HOOK_PREFIX) {
        hook(s, &req, rest, &sh);
    } else {
        respond(&mut s, "404 Not Found");
    }
}

fn hook(mut s: TcpStream, req: &Request, mut body: Vec<u8>, sh: &Shared) {
    let want = format!("Bearer {}", sh.hook_token);
    if !req.header("authorization").is_some_and(|a| ct_eq(a.as_bytes(), want.as_bytes())) {
        return respond(&mut s, "401 Unauthorized");
    }
    let len: usize = req.header("content-length").and_then(|l| l.parse().ok()).unwrap_or(body.len());
    if len > MAX_HOOK_BODY {
        return respond(&mut s, "413 Payload Too Large");
    }
    if body.len() < len {
        let mut more = vec![0u8; len - body.len()];
        if s.read_exact(&mut more).is_err() {
            return respond(&mut s, "400 Bad Request");
        }
        body.extend_from_slice(&more);
    }
    body.truncate(len);
    // Answer first: the agent waits on the hook.
    respond(&mut s, "200 OK");
    let route = req.path[HOOK_PREFIX.len()..].split(['?', '/']).next().unwrap_or_default().to_string();
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    sh.emit(Event::Hook { route, body });
}

fn ws_session(mut s: TcpStream, req: &Request, rest: Vec<u8>, sh: &Arc<Shared>) {
    if !req.header(AUTH_HEADER).is_some_and(|t| ct_eq(t.as_bytes(), sh.token.as_bytes())) {
        return respond(&mut s, "401 Unauthorized");
    }
    let Some(key) = req.header("sec-websocket-key") else { return respond(&mut s, "400 Bad Request") };
    if sh.clients.lock().map(|c| c.len() >= MAX_CLIENTS).unwrap_or(true) {
        return respond(&mut s, "503 Service Unavailable");
    }
    // A subprotocol the client asks for is echoed (one of its own), or its library would refuse us.
    let proto = req
        .header("sec-websocket-protocol")
        .and_then(|p| p.split(',').map(str::trim).find(|p| !p.is_empty()))
        .map(|p| format!("Sec-WebSocket-Protocol: {p}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n{proto}\r\n",
        ws::accept_key(key)
    );
    if s.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = s.set_read_timeout(None);
    let _ = s.set_nodelay(true);
    let (Ok(mut w), Ok(sock)) = (s.try_clone(), s.try_clone()) else { return };
    let id = sh.next.fetch_add(1, Ordering::SeqCst) + 1;
    let (out, orx) = mpsc::channel::<Vec<u8>>();
    let writer = thread::Builder::new().name("ide-write".into()).spawn(move || {
        for buf in orx {
            if w.write_all(&buf).is_err() {
                break;
            }
        }
        let _ = w.shutdown(Shutdown::Both);
    });
    if writer.is_err() {
        return;
    }
    if let Ok(mut cs) = sh.clients.lock() {
        cs.insert(id, Client { out: out.clone(), sock });
    }
    sh.emit(Event::Open(id));
    let mut r = io::Cursor::new(rest).chain(s);
    let mut asm = ws::Assembler::new(MAX_MESSAGE);
    while let Ok(m) = ws::read_message(&mut r, &mut asm, true) {
        match m {
            ws::Msg::Text(t) => on_text(&t, id, &out, sh),
            ws::Msg::Ping(p) => {
                let _ = out.send(ws::encode(ws::OP_PONG, &p, None));
            }
            ws::Msg::Close(code) => {
                // Echo the peer's status code, as RFC 6455 asks.
                let _ = out.send(ws::encode(ws::OP_CLOSE, &ws::close_payload(code.unwrap_or(1000)), None));
                break;
            }
            ws::Msg::Binary | ws::Msg::Pong => {}
        }
    }
    if let Ok(mut cs) = sh.clients.lock() {
        cs.remove(&id);
    }
    sh.emit(Event::Closed(id));
}

fn on_text(t: &str, client: u64, out: &Sender<Vec<u8>>, sh: &Shared) {
    let v: Value = match serde_json::from_str(t) {
        Ok(v) => v,
        Err(_) => {
            let _ = out.send(text_frame(&mcp::error(&Value::Null, -32700, "Parse error")));
            return;
        }
    };
    let msgs = match v {
        Value::Array(a) => a,
        one => vec![one],
    };
    for m in &msgs {
        match mcp::route(m) {
            mcp::Route::Reply(r) => {
                let _ = out.send(text_frame(&r));
            }
            mcp::Route::Call { id, name, args } => sh.emit(Event::Call { client, id, name, args }),
            mcp::Route::Note { method, params } => sh.emit(Event::Note { client, method, params }),
            mcp::Route::Ignore => {}
        }
    }
}
