//! Spawns and supervises `gopls` over stdio and translates between `.ghp` and the virtual `.go`
//! documents.
//!
//! Overlay design: a page `dir/home.ghp` is opened in gopls as `file://dir/home.ghp.go` with
//! `textDocument/didOpen` and is never written to disk. gopls treats open documents as an
//! overlay, so a file that does not exist on disk still joins the package of its directory and
//! sees the user's whole module (imports, `go.mod`, vendor, workspace). `.ghp.go` ends in `.go`
//! so gopls accepts it, and its handler is named `ghp_virtual_<stem>` so it can never redeclare
//! the real transpiled `<stem>` function if that exists on disk.
//!
//! One gopls process runs per module root (nearest `go.mod` above the page). A dead process is
//! restarted on the next call and its open documents are replayed; after repeated crashes the
//! bridge gives up with a single status message and degrades to no-ops.

use super::sourcemap::{Position, Range};
use super::virtual_doc::{ExprRegion, Options, VirtualDoc, build};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// JSON-RPC framing
// ---------------------------------------------------------------------------------------------

/// Write one `Content-Length` framed message.
pub fn write_message(w: &mut impl Write, v: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(v).map_err(io::Error::other)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

/// Read one framed message; `Ok(None)` on a clean EOF before any header.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return if saw_header { Err(io::ErrorKind::UnexpectedEof.into()) } else { Ok(None) };
        }
        saw_header = true;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let len = len.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ---------------------------------------------------------------------------------------------
// URIs
// ---------------------------------------------------------------------------------------------

pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

// ---------------------------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Config {
    /// Explicit gopls binary; otherwise `GHP_GOPLS`, `PATH`, `$GOPATH/bin`, `~/go/bin`.
    pub gopls: Option<PathBuf>,
    pub request_timeout: Duration,
    /// Restarts allowed per module root inside `restart_window` before giving up.
    pub max_restarts: usize,
    pub restart_window: Duration,
    /// Package clause for the virtual file; detected from sibling `.go` files when `None`.
    pub package: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            gopls: None,
            request_timeout: Duration::from_secs(15),
            max_restarts: 3,
            restart_window: Duration::from_secs(60),
            package: None,
        }
    }
}

#[derive(Debug)]
pub enum ProxyError {
    Cancelled,
    Timeout,
    Crashed,
    Server(String),
    Io(String),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::Cancelled => write!(f, "request cancelled"),
            ProxyError::Timeout => write!(f, "gopls request timed out"),
            ProxyError::Crashed => write!(f, "gopls exited"),
            ProxyError::Server(m) => write!(f, "gopls error: {m}"),
            ProxyError::Io(m) => write!(f, "gopls i/o error: {m}"),
        }
    }
}

impl std::error::Error for ProxyError {}

/// Cooperative cancellation of one in-flight request (maps to `$/cancelRequest`).
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Things the bridge tells its host.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A user-facing status line, sent at most once per condition (gopls missing, gave up).
    Status(String),
    /// New diagnostics are available for this page: call [`Bridge::diagnostics`].
    Diagnostics(PathBuf),
    /// The gopls process of this module root exited (it is restarted on the next call).
    Crashed(PathBuf),
    /// gopls was restarted after a crash.
    Restarted(PathBuf),
}

/// A gopls diagnostic translated into `.ghp` coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Option<u64>,
    pub code: Option<Value>,
    pub source: Option<String>,
    pub message: String,
}

// ---------------------------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------------------------

struct Doc {
    root: PathBuf,
    go_uri: String,
    version: i64,
    vdoc: VirtualDoc,
}

#[derive(Default)]
struct Shared {
    docs: Mutex<HashMap<PathBuf, Doc>>,
    /// Latest raw diagnostics per virtual-document URI.
    diags: Mutex<HashMap<String, Vec<Value>>>,
}

type Pending = Arc<Mutex<HashMap<i64, Sender<Value>>>>;

struct Server {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    next_id: AtomicI64,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Server {
    fn spawn(
        bin: &Path,
        root: &Path,
        shared: Arc<Shared>,
        events: Sender<Event>,
        timeout: Duration,
    ) -> Result<Arc<Server>, ProxyError> {
        let mut child = Command::new(bin)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ProxyError::Io(e.to_string()))?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let server = Arc::new(Server {
            child: Mutex::new(child),
            stdin: stdin.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            alive: Arc::new(AtomicBool::new(true)),
            next_id: AtomicI64::new(1),
        });

        let pending = server.pending.clone();
        let alive = server.alive.clone();
        let root_for_thread = root.to_path_buf();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            while let Ok(Some(msg)) = read_message(&mut r) {
                dispatch(&msg, &stdin, &pending, &shared, &events);
            }
            alive.store(false, Ordering::SeqCst);
            pending.lock().unwrap().clear(); // drops senders: waiting requests see `Crashed`
            let _ = events.send(Event::Crashed(root_for_thread));
        });

        let root_uri = path_to_uri(root);
        let init = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "ghp-lsp"},
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}],
            "capabilities": {
                "workspace": {"configuration": true},
                "textDocument": {
                    "synchronization": {},
                    "completion": {"completionItem": {"snippetSupport": false, "documentationFormat": ["markdown", "plaintext"]}},
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "signatureHelp": {"signatureInformation": {"documentationFormat": ["markdown", "plaintext"]}},
                    "definition": {"linkSupport": true},
                    "references": {},
                    "publishDiagnostics": {}
                }
            }
        });
        server.request("initialize", init, &CancelToken::default(), timeout.max(Duration::from_secs(30)))?;
        server.notify("initialized", json!({}))?;
        Ok(server)
    }

    fn send(&self, v: &Value) -> Result<(), ProxyError> {
        let mut g = self.stdin.lock().unwrap();
        write_message(&mut *g, v).map_err(|e| ProxyError::Io(e.to_string()))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), ProxyError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn request(
        &self,
        method: &str,
        params: Value,
        cancel: &CancelToken,
        timeout: Duration,
    ) -> Result<Value, ProxyError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        if !self.alive.load(Ordering::SeqCst) {
            self.pending.lock().unwrap().remove(&id);
            return Err(ProxyError::Crashed);
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let started = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(resp) => {
                    if let Some(err) = resp.get("error") {
                        let m = err.get("message").and_then(Value::as_str).unwrap_or("unknown");
                        return Err(ProxyError::Server(m.to_string()));
                    }
                    return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(ProxyError::Crashed),
                Err(RecvTimeoutError::Timeout) => {
                    let cancelled = cancel.is_cancelled();
                    if cancelled || started.elapsed() > timeout {
                        self.pending.lock().unwrap().remove(&id);
                        let _ = self.notify("$/cancelRequest", json!({"id": id}));
                        return Err(if cancelled { ProxyError::Cancelled } else { ProxyError::Timeout });
                    }
                }
            }
        }
    }
}

/// Route one message from gopls.
fn dispatch(
    msg: &Value,
    stdin: &Arc<Mutex<ChildStdin>>,
    pending: &Pending,
    shared: &Arc<Shared>,
    events: &Sender<Event>,
) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id");
    match (method, id) {
        // Request from the server: answer with the most neutral reply so it never blocks.
        (Some(m), Some(id)) => {
            let result = if m == "workspace/configuration" {
                let n = msg["params"]["items"].as_array().map_or(0, Vec::len);
                Value::Array(vec![Value::Null; n])
            } else {
                Value::Null
            };
            let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
            let _ = write_message(&mut *stdin.lock().unwrap(), &reply);
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            let uri = msg["params"]["uri"].as_str().unwrap_or_default().to_string();
            let list = msg["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
            shared.diags.lock().unwrap().insert(uri.clone(), list);
            let ghp = shared.docs.lock().unwrap().iter().find(|(_, d)| d.go_uri == uri).map(|(p, _)| p.clone());
            if let Some(p) = ghp {
                let _ = events.send(Event::Diagnostics(p));
            }
        }
        (None, Some(id)) => {
            if let Some(id) = id.as_i64()
                && let Some(tx) = pending.lock().unwrap().remove(&id)
            {
                let _ = tx.send(msg.clone());
            }
        }
        _ => {}
    }
}

/// Locate gopls: explicit config, `GHP_GOPLS`, `PATH`, then Go's default install dirs.
pub fn find_gopls(cfg: &Config) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "gopls.exe" } else { "gopls" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(cfg.gopls.clone());
    candidates.extend(std::env::var_os("GHP_GOPLS").map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(exe)));
    }
    if let Some(gp) = std::env::var_os("GOPATH") {
        candidates.extend(std::env::split_paths(&gp).map(|d| d.join("bin").join(exe)));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        candidates.push(PathBuf::from(home).join("go").join("bin").join(exe));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Nearest directory at or above `dir` that holds a `go.mod` (or `go.work`).
pub fn module_root(dir: &Path) -> PathBuf {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join("go.mod").is_file() || d.join("go.work").is_file() {
            return d.to_path_buf();
        }
        cur = d.parent();
    }
    dir.to_path_buf()
}

/// `package x` of the first real `.go` file next to the page.
fn detect_package(dir: &Path) -> Option<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with(".go") && !n.ends_with(".ghp.go") && !n.ends_with("_test.go")
        })
        .collect();
    names.sort();
    for p in names {
        let mut s = String::new();
        if std::fs::File::open(&p).and_then(|f| f.take(8192).read_to_string(&mut s)).is_ok() {
            for line in s.lines() {
                if let Some(rest) = line.trim().strip_prefix("package ") {
                    return rest.split_whitespace().next().map(str::to_string);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Bridge
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Supervision {
    servers: HashMap<PathBuf, Arc<Server>>,
    restarts: HashMap<PathBuf, Vec<Instant>>,
    gave_up: Vec<PathBuf>,
    notified_missing: bool,
}

/// The gopls bridge: owns the child processes, the open virtual documents and the translation.
pub struct Bridge {
    cfg: Config,
    bin: Option<PathBuf>,
    shared: Arc<Shared>,
    events: Sender<Event>,
    sup: Mutex<Supervision>,
}

impl Bridge {
    /// Create the bridge and the channel its [`Event`]s arrive on. Never fails: when gopls is
    /// absent the bridge reports one [`Event::Status`] on first use and answers `None` after.
    pub fn new(cfg: Config) -> (Bridge, Receiver<Event>) {
        let (tx, rx) = channel();
        let bin = find_gopls(&cfg);
        (
            Bridge {
                cfg,
                bin,
                shared: Arc::new(Shared::default()),
                events: tx,
                sup: Mutex::new(Supervision::default()),
            },
            rx,
        )
    }

    pub fn gopls_available(&self) -> bool {
        self.bin.is_some()
    }

    /// The server for `root`, starting or restarting it as needed. `None` = unavailable.
    fn server(&self, root: &Path) -> Option<Arc<Server>> {
        let Some(bin) = self.bin.as_ref() else {
            let mut sup = self.sup.lock().unwrap();
            if !sup.notified_missing {
                sup.notified_missing = true;
                let _ = self.events.send(Event::Status(
                    "gopls not found: Go expression intelligence in .ghp files is disabled \
                     (install with `go install golang.org/x/tools/gopls@latest`)"
                        .into(),
                ));
            }
            return None;
        };
        let mut guard = self.sup.lock().unwrap();
        let sup = &mut *guard;
        if sup.gave_up.iter().any(|r| r == root) {
            return None;
        }
        let was_running = if let Some(s) = sup.servers.get(root) {
            if s.alive.load(Ordering::SeqCst) {
                return Some(s.clone());
            }
            true
        } else {
            false
        };
        sup.servers.remove(root);
        if was_running {
            let now = Instant::now();
            let window = self.cfg.restart_window;
            let hist = sup.restarts.entry(root.to_path_buf()).or_default();
            hist.retain(|t| now.duration_since(*t) < window);
            if hist.len() >= self.cfg.max_restarts {
                sup.gave_up.push(root.to_path_buf());
                let _ = self.events.send(Event::Status(format!(
                    "gopls keeps crashing in {}: Go intelligence disabled for this module",
                    root.display()
                )));
                return None;
            }
            hist.push(now);
        }
        match Server::spawn(bin, root, self.shared.clone(), self.events.clone(), self.cfg.request_timeout) {
            Ok(s) => {
                sup.servers.insert(root.to_path_buf(), s.clone());
                drop(guard);
                if was_running {
                    self.replay(root, &s);
                    let _ = self.events.send(Event::Restarted(root.to_path_buf()));
                }
                Some(s)
            }
            Err(e) => {
                sup.gave_up.push(root.to_path_buf());
                let _ = self.events.send(Event::Status(format!("could not start gopls: {e}")));
                None
            }
        }
    }

    /// Re-open every document of `root` on a fresh server.
    fn replay(&self, root: &Path, s: &Server) {
        let opens: Vec<Value> = {
            let docs = self.shared.docs.lock().unwrap();
            docs.values().filter(|d| d.root == root).map(did_open).collect()
        };
        for p in opens {
            let _ = s.notify("textDocument/didOpen", p);
        }
    }

    // ---- document lifecycle ------------------------------------------------------------------

    /// Open or update a page. Cheap to call on every edit.
    pub fn update(&self, ghp_path: &Path, text: &str, regions: &[ExprRegion]) {
        let dir = ghp_path.parent().unwrap_or(Path::new("."));
        let root = module_root(dir);
        let stem = ghp_path.file_stem().and_then(|s| s.to_str()).unwrap_or("page");
        let package = self.cfg.package.clone().or_else(|| detect_package(dir)).unwrap_or_else(|| "main".into());
        let vdoc = build(text, regions, &Options::for_page(stem, &package));
        let go_uri = path_to_uri(&PathBuf::from(format!("{}.go", ghp_path.display())));

        let (first, version) = {
            let mut docs = self.shared.docs.lock().unwrap();
            match docs.get_mut(ghp_path) {
                Some(d) => {
                    d.version += 1;
                    d.vdoc = vdoc.clone();
                    (false, d.version)
                }
                None => {
                    docs.insert(
                        ghp_path.to_path_buf(),
                        Doc { root: root.clone(), go_uri: go_uri.clone(), version: 1, vdoc: vdoc.clone() },
                    );
                    (true, 1)
                }
            }
        };
        let Some(server) = self.server(&root) else { return };
        let _ = if first {
            let open = did_open(&self.shared.docs.lock().unwrap()[ghp_path]);
            server.notify("textDocument/didOpen", open)
        } else {
            server.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": go_uri, "version": version},
                    "contentChanges": [{"text": vdoc.text}]
                }),
            )
        };
    }

    pub fn close(&self, ghp_path: &Path) {
        let doc = self.shared.docs.lock().unwrap().remove(ghp_path);
        if let Some(d) = doc {
            self.shared.diags.lock().unwrap().remove(&d.go_uri);
            let server = self.sup.lock().unwrap().servers.get(&d.root).cloned();
            if let Some(s) = server {
                let _ = s.notify("textDocument/didClose", json!({"textDocument": {"uri": d.go_uri}}));
            }
        }
    }

    // ---- requests ----------------------------------------------------------------------------

    /// Map a `.ghp` position to the virtual document and run `method` there.
    fn position_request(
        &self,
        ghp_path: &Path,
        method: &str,
        pos: Position,
        extra: Value,
        cancel: &CancelToken,
    ) -> Result<Option<Value>, ProxyError> {
        let (root, go_uri, gpos) = {
            let docs = self.shared.docs.lock().unwrap();
            let Some(d) = docs.get(ghp_path) else { return Ok(None) };
            let Some(gp) = d.vdoc.map.ghp_pos_to_go(pos) else { return Ok(None) };
            (d.root.clone(), d.go_uri.clone(), gp)
        };
        let Some(server) = self.server(&root) else { return Ok(None) };
        let mut params = json!({
            "textDocument": {"uri": go_uri},
            "position": {"line": gpos.line, "character": gpos.character},
        });
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        server.request(method, params, cancel, self.cfg.request_timeout).map(Some)
    }

    pub fn completion(&self, ghp: &Path, pos: Position, cancel: &CancelToken) -> Result<Option<Value>, ProxyError> {
        let r = self.position_request(ghp, "textDocument/completion", pos, json!({}), cancel)?;
        Ok(r.map(|v| self.translate(ghp, v, Translate::Completion)))
    }

    pub fn hover(&self, ghp: &Path, pos: Position, cancel: &CancelToken) -> Result<Option<Value>, ProxyError> {
        let r = self.position_request(ghp, "textDocument/hover", pos, json!({}), cancel)?;
        Ok(r.map(|v| self.translate(ghp, v, Translate::Hover)))
    }

    pub fn definition(&self, ghp: &Path, pos: Position, cancel: &CancelToken) -> Result<Option<Value>, ProxyError> {
        let r = self.position_request(ghp, "textDocument/definition", pos, json!({}), cancel)?;
        Ok(r.map(|v| self.translate(ghp, v, Translate::Locations)))
    }

    pub fn references(&self, ghp: &Path, pos: Position, cancel: &CancelToken) -> Result<Option<Value>, ProxyError> {
        let extra = json!({"context": {"includeDeclaration": true}});
        let r = self.position_request(ghp, "textDocument/references", pos, extra, cancel)?;
        Ok(r.map(|v| self.translate(ghp, v, Translate::Locations)))
    }

    pub fn signature_help(&self, ghp: &Path, pos: Position, cancel: &CancelToken) -> Result<Option<Value>, ProxyError> {
        self.position_request(ghp, "textDocument/signatureHelp", pos, json!({}), cancel)
    }

    /// Diagnostics for a page in `.ghp` coordinates. Anything that does not map entirely onto
    /// Go text of the page (scaffolding, structural syntax errors) is dropped: the GHP parser
    /// reports those.
    pub fn diagnostics(&self, ghp_path: &Path) -> Vec<Diagnostic> {
        let docs = self.shared.docs.lock().unwrap();
        let Some(d) = docs.get(ghp_path) else { return vec![] };
        let raw = self.shared.diags.lock().unwrap().get(&d.go_uri).cloned().unwrap_or_default();
        raw.iter().filter_map(|v| translate_diagnostic(&d.vdoc, v)).collect()
    }

    // ---- result translation ------------------------------------------------------------------

    fn translate(&self, ghp: &Path, v: Value, kind: Translate) -> Value {
        let docs = self.shared.docs.lock().unwrap();
        let Some(d) = docs.get(ghp) else { return v };
        let ghp_uri = path_to_uri(ghp);
        match kind {
            Translate::Completion => translate_completion(&d.vdoc, v),
            Translate::Hover => translate_hover(&d.vdoc, v),
            Translate::Locations => translate_locations(&d.vdoc, &d.go_uri, &ghp_uri, v),
        }
    }
}

#[derive(Clone, Copy)]
enum Translate {
    Completion,
    Hover,
    Locations,
}

fn did_open(d: &Doc) -> Value {
    json!({"textDocument": {
        "uri": d.go_uri, "languageId": "go", "version": d.version, "text": d.vdoc.text
    }})
}

// ---------------------------------------------------------------------------------------------
// Translation helpers (pure, unit-tested)
// ---------------------------------------------------------------------------------------------

fn parse_pos(v: &Value) -> Option<Position> {
    Some(Position { line: v.get("line")?.as_u64()? as u32, character: v.get("character")?.as_u64()? as u32 })
}

fn parse_range(v: &Value) -> Option<Range> {
    Some(Range { start: parse_pos(v.get("start")?)?, end: parse_pos(v.get("end")?)? })
}

fn range_json(r: Range) -> Value {
    json!({
        "start": {"line": r.start.line, "character": r.start.character},
        "end": {"line": r.end.line, "character": r.end.character},
    })
}

fn go_range_to_ghp_json(doc: &VirtualDoc, v: &Value) -> Option<Value> {
    let r = parse_range(v)?;
    doc.map.go_range_to_ghp(r).map(range_json)
}

pub(crate) fn translate_diagnostic(doc: &VirtualDoc, v: &Value) -> Option<Diagnostic> {
    let range = doc.map.go_range_to_ghp(parse_range(v.get("range")?)?)?;
    Some(Diagnostic {
        range,
        severity: v.get("severity").and_then(Value::as_u64),
        code: v.get("code").cloned(),
        source: v.get("source").and_then(Value::as_str).map(str::to_string),
        message: v.get("message")?.as_str()?.to_string(),
    })
}

pub(crate) fn translate_hover(doc: &VirtualDoc, mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        match obj.get("range").and_then(|r| go_range_to_ghp_json(doc, r)) {
            Some(r) => {
                obj.insert("range".into(), r);
            }
            None => {
                obj.remove("range");
            }
        }
    }
    v
}

pub(crate) fn translate_completion(doc: &VirtualDoc, mut v: Value) -> Value {
    let items = match &mut v {
        Value::Array(a) => Some(a),
        Value::Object(o) => o.get_mut("items").and_then(Value::as_array_mut),
        _ => None,
    };
    if let Some(items) = items {
        for it in items.iter_mut() {
            let Some(o) = it.as_object_mut() else { continue };
            // Edits against the virtual file (auto-import, ...) have no meaning in the page.
            o.remove("additionalTextEdits");
            o.remove("command");
            if let Some(te) = o.get_mut("textEdit").and_then(Value::as_object_mut) {
                let mut ok = true;
                for key in ["range", "insert", "replace"] {
                    if let Some(r) = te.get(key) {
                        match go_range_to_ghp_json(doc, r) {
                            Some(m) => {
                                te.insert(key.into(), m);
                            }
                            None => ok = false,
                        }
                    }
                }
                if !ok {
                    o.remove("textEdit");
                }
            }
        }
    }
    v
}

pub(crate) fn translate_locations(doc: &VirtualDoc, go_uri: &str, ghp_uri: &str, v: Value) -> Value {
    fn one(doc: &VirtualDoc, go_uri: &str, ghp_uri: &str, mut loc: Value) -> Option<Value> {
        let o = loc.as_object_mut()?;
        if o.contains_key("targetUri") {
            // LocationLink
            if o["targetUri"].as_str() == Some(go_uri) {
                let tr = go_range_to_ghp_json(doc, &o["targetRange"])?;
                let sel = o
                    .get("targetSelectionRange")
                    .and_then(|r| go_range_to_ghp_json(doc, r))
                    .unwrap_or_else(|| tr.clone());
                o.insert("targetUri".into(), json!(ghp_uri));
                o.insert("targetRange".into(), tr);
                o.insert("targetSelectionRange".into(), sel);
            }
            o.remove("originSelectionRange");
            return Some(loc);
        }
        if o.get("uri").and_then(Value::as_str) == Some(go_uri) {
            let r = go_range_to_ghp_json(doc, o.get("range")?)?;
            o.insert("uri".into(), json!(ghp_uri));
            o.insert("range".into(), r);
        }
        Some(loc)
    }
    match v {
        Value::Array(a) => Value::Array(a.into_iter().filter_map(|l| one(doc, go_uri, ghp_uri, l)).collect()),
        Value::Object(_) => one(doc, go_uri, ghp_uri, v).unwrap_or(Value::Null),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gopls::scan::scan_regions;
    use std::io::Cursor;

    fn doc(src: &str) -> VirtualDoc {
        build(src, &scan_regions(src), &Options::for_page("p", "main"))
    }

    #[test]
    fn framing_round_trip_and_eof() {
        let mut buf = Vec::new();
        write_message(&mut buf, &json!({"a": "é"})).unwrap();
        write_message(&mut buf, &json!({"b": 2})).unwrap();
        let mut c = Cursor::new(buf);
        assert_eq!(read_message(&mut c).unwrap(), Some(json!({"a": "é"})));
        assert_eq!(read_message(&mut c).unwrap(), Some(json!({"b": 2})));
        assert_eq!(read_message(&mut c).unwrap(), None);
    }

    #[test]
    fn framing_tolerates_extra_headers_and_rejects_garbage() {
        let body = br#"{"x":1}"#;
        let mut raw =
            format!("Content-Type: application/vscode-jsonrpc\r\ncontent-length: {}\r\n\r\n", body.len()).into_bytes();
        raw.extend_from_slice(body);
        assert_eq!(read_message(&mut Cursor::new(raw)).unwrap(), Some(json!({"x": 1})));
        assert!(read_message(&mut Cursor::new(b"\r\n{}".to_vec())).is_err());
        assert!(read_message(&mut Cursor::new(b"Content-Length: 10\r\n\r\n{}".to_vec())).is_err());
    }

    #[test]
    fn uri_round_trip() {
        let p = PathBuf::from("/tmp/my dir/é/home.ghp.go");
        let u = path_to_uri(&p);
        assert_eq!(u, "file:///tmp/my%20dir/%C3%A9/home.ghp.go");
        assert_eq!(uri_to_path(&u), Some(p));
    }

    #[test]
    fn diagnostics_outside_go_text_are_dropped() {
        let src = "<p><go= user.Nope /></p>";
        let d = doc(src);
        let nope = d.text.find("user.Nope").unwrap();
        let pos = d.map.go_index().position(nope);
        let end = d.map.go_index().position(nope + "user.Nope".len());
        let inside = json!({"range": range_json(Range { start: pos, end }), "message": "m", "severity": 1});
        let got = translate_diagnostic(&d, &inside).unwrap();
        assert_eq!(got.range.start, Position { line: 0, character: 8 });
        assert_eq!(got.range.end, Position { line: 0, character: 17 });

        // `undefined: http` style error on the scaffolding signature.
        let scaffold = json!({"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 4}}, "message": "m"});
        assert!(translate_diagnostic(&d, &scaffold).is_none());
        // Straddling scaffolding and Go text.
        let wide = json!({"range": {"start": pos, "end": {"line": pos.line + 1, "character": 0}}, "message": "m"});
        assert!(translate_diagnostic(&d, &wide).is_none());
    }

    #[test]
    fn completion_edits_are_mapped_and_scaffolding_edits_dropped() {
        let src = "<go= us />";
        let d = doc(src);
        let off = d.map.segments()[0].go_start;
        let r = range_json(Range { start: d.map.go_index().position(off), end: d.map.go_index().position(off + 2) });
        let v = json!({"items": [
            {"label": "user", "textEdit": {"newText": "user", "range": r}, "additionalTextEdits": [{"newText": "import"}]},
            {"label": "x", "textEdit": {"newText": "x", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}},
        ]});
        let out = translate_completion(&d, v);
        let items = out["items"].as_array().unwrap();
        assert_eq!(items[0]["textEdit"]["range"]["start"], json!({"line": 0, "character": 6}));
        assert!(items[0].get("additionalTextEdits").is_none());
        assert!(items[1].get("textEdit").is_none());
    }

    #[test]
    fn locations_in_virtual_doc_map_back_and_others_pass_through() {
        let src = "<go\n  a := 1\n/><go= a />";
        let d = doc(src);
        let off = d.text.find("a := 1").unwrap();
        let r = range_json(Range { start: d.map.go_index().position(off), end: d.map.go_index().position(off + 1) });
        let other = json!({"uri": "file:///m/user.go", "range": range_json(Range { start: Position { line: 3, character: 0 }, end: Position { line: 3, character: 4 } })});
        let v = json!([{"uri": "file:///p.ghp.go", "range": r}, other.clone()]);
        let out = translate_locations(&d, "file:///p.ghp.go", "file:///p.ghp", v);
        let a = out.as_array().unwrap();
        assert_eq!(a[0]["uri"], "file:///p.ghp");
        assert_eq!(a[0]["range"]["start"], json!({"line": 1, "character": 2}));
        assert_eq!(a[1], other);
    }

    #[test]
    fn hover_range_is_mapped_or_removed() {
        let d = doc("<go= x />");
        let off = d.map.segments()[0].go_start;
        let r = range_json(Range { start: d.map.go_index().position(off), end: d.map.go_index().position(off + 1) });
        let out = translate_hover(&d, json!({"contents": "t", "range": r}));
        assert_eq!(out["range"]["start"], json!({"line": 0, "character": 6}));
        let out = translate_hover(
            &d,
            json!({"contents": "t", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}),
        );
        assert!(out.get("range").is_none());
    }

    #[test]
    fn missing_gopls_is_a_graceful_noop_with_one_status() {
        let cfg = Config { gopls: None, ..Config::default() };
        let (mut b, rx) = Bridge::new(cfg);
        b.bin = None; // force "not installed" regardless of the host
        let p = Path::new("/nonexistent/home.ghp");
        let src = "<go= x />";
        b.update(p, src, &scan_regions(src));
        b.update(p, src, &scan_regions(src));
        let c = CancelToken::default();
        assert!(b.hover(p, Position { line: 0, character: 6 }, &c).unwrap().is_none());
        assert!(b.completion(p, Position { line: 0, character: 6 }, &c).unwrap().is_none());
        assert!(b.diagnostics(p).is_empty());
        let statuses: Vec<_> = rx.try_iter().collect();
        assert_eq!(statuses.len(), 1, "{statuses:?}");
        assert!(matches!(&statuses[0], Event::Status(s) if s.contains("gopls not found")));
    }

    #[test]
    fn position_outside_go_regions_returns_none_without_a_server() {
        let (mut b, _rx) = Bridge::new(Config::default());
        b.bin = None;
        let p = Path::new("/nonexistent/home.ghp");
        let src = "<p>hi</p><go= x />";
        b.update(p, src, &scan_regions(src));
        let c = CancelToken::default();
        assert!(b.hover(p, Position { line: 0, character: 4 }, &c).unwrap().is_none());
    }
}
