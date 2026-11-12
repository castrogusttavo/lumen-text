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

use super::sourcemap::{Position, Range, TextIndex};
use super::virtual_doc::{ExprRegion, Options, VirtualDoc, build};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// `Mutex::lock` that ignores poisoning. A handler that panicked (the server catches that and
/// answers with an error) must not turn every later request into a panic of its own: the data
/// behind these locks is plain bookkeeping that stays consistent between statements.
pub trait LockExt<T> {
    fn lock_ok(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_ok(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

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

/// `file://` URI of a path. A Windows drive path becomes `file:///C:/dir/x`, a UNC path
/// becomes `file://host/share/x`; backslashes turn into slashes on any host OS.
pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    let body = if let Some(unc) = s.strip_prefix("//") {
        unc
    } else {
        if !s.starts_with('/') {
            out.push('/');
        }
        s.as_str()
    };
    for b in body.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
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
    String::from_utf8(out).ok()
}

/// Path of a `file://` URI, using the conventions of the running OS.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    uri_to_path_for(uri, cfg!(windows))
}

/// [`uri_to_path`] with the target OS chosen explicitly (so Windows handling is testable
/// everywhere). On Windows `file:///C:/x` and `file:///c%3A/x` give `C:\x`, and
/// `file://host/share/x` gives the UNC path `\\host\share\x`.
pub fn uri_to_path_for(uri: &str, windows: bool) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest)?;
    if !windows {
        let local = decoded.strip_prefix("localhost").filter(|r| r.starts_with('/')).unwrap_or(&decoded);
        return Some(PathBuf::from(local));
    }
    let b = decoded.as_bytes();
    let path = if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        decoded[1..].to_string()
    } else if decoded.starts_with('/') {
        decoded
    } else {
        format!("//{decoded}")
    };
    Some(PathBuf::from(path.replace('/', "\\")))
}

/// Canonical form for comparing URIs that name the same file: percent-decoded, forward slashes,
/// lower-case drive letter. gopls on Windows answers with `file:///c%3A/...` for a document we
/// opened as `file:///C:/...`.
pub fn normalize_uri(uri: &str) -> String {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    let mut s = percent_decode(rest).unwrap_or_else(|| rest.to_string()).replace('\\', "/");
    let b = s.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        let lower = b[1].to_ascii_lowercase() as char;
        s.replace_range(1..2, &lower.to_string());
    }
    s
}

/// Whether two URIs name the same file (see [`normalize_uri`]).
pub fn uri_eq(a: &str, b: &str) -> bool {
    a == b || normalize_uri(a) == normalize_uri(b)
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
    /// Never look for or start gopls (the host falls back to its own scope analysis).
    pub disabled: bool,
    /// Start (and restart) gopls on a background thread. The call that needs it returns at once
    /// with "unavailable" (so the host answers from its own scope analysis) until gopls has
    /// initialized; [`Event::Started`] / [`Event::Restarted`] and the first diagnostics follow.
    /// With `false` the call that needs gopls waits for it, which suits tests and simple hosts.
    pub background_start: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            gopls: None,
            request_timeout: Duration::from_secs(15),
            max_restarts: 3,
            restart_window: Duration::from_secs(60),
            package: None,
            disabled: false,
            background_start: false,
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
    /// The first gopls of this module root finished starting in the background
    /// ([`Config::background_start`]); its open documents were handed over.
    Started(PathBuf),
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

/// What `update` needs to sync one document to a server.
struct Snapshot {
    uri: String,
    version: i64,
    text: String,
}

#[derive(Default)]
struct Shared {
    docs: Mutex<HashMap<PathBuf, Doc>>,
    /// Latest raw diagnostics per virtual-document URI ([`normalize_uri`] form).
    diags: Mutex<HashMap<String, Vec<Value>>>,
}

type Pending = Arc<Mutex<HashMap<i64, Sender<Value>>>>;

struct Server {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    next_id: AtomicI64,
    /// Text of every document as last sent to this process, by URI. Changes are sent as a
    /// minimal edit against it, so a fresh (restarted) process always starts from `didOpen`.
    synced: Mutex<HashMap<String, String>>,
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
            synced: Mutex::new(HashMap::new()),
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
            pending.lock_ok().clear(); // drops senders: waiting requests see `Crashed`
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
        let mut g = self.stdin.lock_ok();
        write_message(&mut *g, v).map_err(|e| ProxyError::Io(e.to_string()))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), ProxyError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Bring this process up to date with `snap`: `didOpen` the first time, afterwards a single
    /// incremental `didChange` (nothing at all when the text is unchanged).
    fn sync_doc(&self, snap: &Snapshot) -> Result<(), ProxyError> {
        let mut synced = self.synced.lock_ok();
        match synced.get(&snap.uri) {
            None => self.notify(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": snap.uri, "languageId": "go", "version": snap.version, "text": snap.text
                }}),
            )?,
            Some(prev) if *prev == snap.text => return Ok(()),
            Some(prev) => self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": snap.uri, "version": snap.version},
                    "contentChanges": [incremental_change(prev, &snap.text)]
                }),
            )?,
        }
        synced.insert(snap.uri.clone(), snap.text.clone());
        Ok(())
    }

    fn request(
        &self,
        method: &str,
        params: Value,
        cancel: &CancelToken,
        timeout: Duration,
    ) -> Result<Value, ProxyError> {
        if cancel.is_cancelled() {
            return Err(ProxyError::Cancelled);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending.lock_ok().insert(id, tx);
        if !self.alive.load(Ordering::SeqCst) {
            self.pending.lock_ok().remove(&id);
            return Err(ProxyError::Crashed);
        }
        if let Err(e) = self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})) {
            self.pending.lock_ok().remove(&id);
            return Err(e);
        }
        let started = Instant::now();
        loop {
            let cancelled = cancel.is_cancelled();
            let elapsed = started.elapsed();
            if cancelled || elapsed >= timeout {
                self.pending.lock_ok().remove(&id);
                let _ = self.notify("$/cancelRequest", json!({"id": id}));
                return Err(if cancelled { ProxyError::Cancelled } else { ProxyError::Timeout });
            }
            let wait = Duration::from_millis(10).min(timeout - elapsed);
            match rx.recv_timeout(wait) {
                Ok(resp) => {
                    if let Some(err) = resp.get("error") {
                        let m = err.get("message").and_then(Value::as_str).unwrap_or("unknown");
                        return Err(ProxyError::Server(m.to_string()));
                    }
                    return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(ProxyError::Crashed),
                Err(RecvTimeoutError::Timeout) => {}
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
            let _ = write_message(&mut *stdin.lock_ok(), &reply);
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            let uri = msg["params"]["uri"].as_str().unwrap_or_default().to_string();
            let list = msg["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
            shared.diags.lock_ok().insert(normalize_uri(&uri), list);
            let ghp = shared.docs.lock_ok().iter().find(|(_, d)| uri_eq(&d.go_uri, &uri)).map(|(p, _)| p.clone());
            if let Some(p) = ghp {
                let _ = events.send(Event::Diagnostics(p));
            }
        }
        (None, Some(id)) => {
            if let Some(id) = id.as_i64()
                && let Some(tx) = pending.lock_ok().remove(&id)
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
    /// Module roots whose gopls is being started on a background thread.
    starting: HashSet<PathBuf>,
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
    sup: Arc<Mutex<Supervision>>,
}

impl Bridge {
    /// Create the bridge and the channel its [`Event`]s arrive on. Never fails: when gopls is
    /// absent the bridge reports one [`Event::Status`] on first use and answers `None` after.
    pub fn new(cfg: Config) -> (Bridge, Receiver<Event>) {
        let (tx, rx) = channel();
        let bin = if cfg.disabled { None } else { find_gopls(&cfg) };
        (
            Bridge {
                cfg,
                bin,
                shared: Arc::new(Shared::default()),
                events: tx,
                sup: Arc::new(Mutex::new(Supervision::default())),
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
            let mut sup = self.sup.lock_ok();
            if !sup.notified_missing && !self.cfg.disabled {
                sup.notified_missing = true;
                let _ = self.events.send(Event::Status(
                    "gopls not found: Go expression intelligence in .ghp files is disabled \
                     (install with `go install golang.org/x/tools/gopls@latest`)"
                        .into(),
                ));
            }
            return None;
        };
        let mut guard = self.sup.lock_ok();
        let sup = &mut *guard;
        if sup.gave_up.iter().any(|r| r == root) || sup.starting.contains(root) {
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
        if self.cfg.background_start {
            sup.starting.insert(root.to_path_buf());
            drop(guard);
            self.start_in_background(bin.clone(), root.to_path_buf(), was_running);
            return None;
        }
        match Server::spawn(bin, root, self.shared.clone(), self.events.clone(), self.cfg.request_timeout) {
            Ok(s) => {
                sup.servers.insert(root.to_path_buf(), s.clone());
                drop(guard);
                if was_running {
                    replay(&self.shared, root, &s);
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

    /// Spawn gopls for `root` off the caller's thread, then hand it every open document.
    ///
    /// The documents are synced twice, around publishing the server: edits that arrive while the
    /// first pass runs find no server yet (and are skipped), the second pass picks them up.
    fn start_in_background(&self, bin: PathBuf, root: PathBuf, restart: bool) {
        let (shared, events, sup) = (self.shared.clone(), self.events.clone(), self.sup.clone());
        let timeout = self.cfg.request_timeout;
        std::thread::spawn(move || match Server::spawn(&bin, &root, shared.clone(), events.clone(), timeout) {
            Ok(s) => {
                replay(&shared, &root, &s);
                {
                    let mut sup = sup.lock_ok();
                    sup.starting.remove(&root);
                    sup.servers.insert(root.clone(), s.clone());
                }
                replay(&shared, &root, &s);
                let _ = events.send(if restart { Event::Restarted(root) } else { Event::Started(root) });
            }
            Err(e) => {
                let mut sup = sup.lock_ok();
                sup.starting.remove(&root);
                sup.gave_up.push(root);
                let _ = events.send(Event::Status(format!("could not start gopls: {e}")));
            }
        });
    }

    /// Whether the gopls of `ghp_path`'s module is still starting in the background.
    pub fn is_starting(&self, ghp_path: &Path) -> bool {
        let root = module_root(ghp_path.parent().unwrap_or(Path::new(".")));
        self.sup.lock_ok().starting.contains(&root)
    }

    // ---- document lifecycle ------------------------------------------------------------------

    /// Open or update a page. Cheap to call on every edit: gopls receives one minimal
    /// incremental edit of the virtual document, or nothing when it did not change.
    pub fn update(&self, ghp_path: &Path, text: &str, regions: &[ExprRegion]) {
        let dir = ghp_path.parent().unwrap_or(Path::new("."));
        let root = module_root(dir);
        let stem = ghp_path.file_stem().and_then(|s| s.to_str()).unwrap_or("page");
        let package = self.cfg.package.clone().or_else(|| detect_package(dir)).unwrap_or_else(|| "main".into());
        let vdoc = build(text, regions, &Options::for_page(stem, &package));
        let go_uri = path_to_uri(&PathBuf::from(format!("{}.go", ghp_path.display())));

        let snap = {
            let mut docs = self.shared.docs.lock_ok();
            let d = docs.entry(ghp_path.to_path_buf()).or_insert_with(|| Doc {
                root: root.clone(),
                go_uri: go_uri.clone(),
                version: 0,
                vdoc: vdoc.clone(),
            });
            d.version += 1;
            d.vdoc = vdoc;
            d.snapshot()
        };
        let Some(server) = self.server(&root) else { return };
        let _ = server.sync_doc(&snap);
    }

    pub fn close(&self, ghp_path: &Path) {
        let doc = self.shared.docs.lock_ok().remove(ghp_path);
        if let Some(d) = doc {
            self.shared.diags.lock_ok().remove(&normalize_uri(&d.go_uri));
            let server = self.sup.lock_ok().servers.get(&d.root).cloned();
            if let Some(s) = server {
                let _ = s.notify("textDocument/didClose", json!({"textDocument": {"uri": d.go_uri}}));
                s.synced.lock_ok().remove(&d.go_uri);
            }
        }
    }

    /// Whether Go intelligence is currently usable for the module of `ghp_path`: gopls was found
    /// and the bridge has not given up on that module after repeated crashes.
    pub fn is_active(&self, ghp_path: &Path) -> bool {
        if self.bin.is_none() {
            return false;
        }
        let root = module_root(ghp_path.parent().unwrap_or(Path::new(".")));
        !self.sup.lock_ok().gave_up.contains(&root)
    }

    /// Kill the gopls process serving `ghp_path`'s module, as a crash would. Test hook: the bridge
    /// notices through [`Event::Crashed`] and restarts on the next call.
    #[doc(hidden)]
    pub fn kill_server_for_test(&self, ghp_path: &Path) {
        let root = module_root(ghp_path.parent().unwrap_or(Path::new(".")));
        let server = self.sup.lock_ok().servers.get(&root).cloned();
        if let Some(s) = server {
            let _ = s.child.lock_ok().kill();
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
            let docs = self.shared.docs.lock_ok();
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
        let docs = self.shared.docs.lock_ok();
        let Some(d) = docs.get(ghp_path) else { return vec![] };
        let raw = self.shared.diags.lock_ok().get(&normalize_uri(&d.go_uri)).cloned().unwrap_or_default();
        raw.iter().filter_map(|v| translate_diagnostic(&d.vdoc, v)).collect()
    }

    // ---- result translation ------------------------------------------------------------------

    fn translate(&self, ghp: &Path, v: Value, kind: Translate) -> Value {
        let docs = self.shared.docs.lock_ok();
        let Some(d) = docs.get(ghp) else { return v };
        let ghp_uri = path_to_uri(ghp);
        match kind {
            Translate::Completion => translate_completion(&d.vdoc, v),
            Translate::Hover => translate_hover(&d.vdoc, v),
            Translate::Locations => translate_locations(&d.vdoc, &d.go_uri, &ghp_uri, v),
        }
    }
}

/// Re-open every document of `root` on `s` (a fresh process, or a late sync).
fn replay(shared: &Shared, root: &Path, s: &Server) {
    let snaps: Vec<Snapshot> = {
        let docs = shared.docs.lock_ok();
        docs.values().filter(|d| d.root == root).map(Doc::snapshot).collect()
    };
    for snap in &snaps {
        let _ = s.sync_doc(snap);
    }
}

#[derive(Clone, Copy)]
enum Translate {
    Completion,
    Hover,
    Locations,
}

impl Doc {
    fn snapshot(&self) -> Snapshot {
        Snapshot { uri: self.go_uri.clone(), version: self.version, text: self.vdoc.text.clone() }
    }
}

/// The smallest single edit turning `prev` into `new`, as an LSP content change: common prefix
/// and suffix are kept, the middle is replaced. Multi-line and multi-region edits of a page come
/// out as one range because the virtual document is regenerated as a whole.
pub(crate) fn incremental_change(prev: &str, new: &str) -> Value {
    let (pb, nb) = (prev.as_bytes(), new.as_bytes());
    let mut p = pb.iter().zip(nb).take_while(|(a, b)| a == b).count();
    while !(prev.is_char_boundary(p) && new.is_char_boundary(p)) {
        p -= 1;
    }
    let max_suffix = pb.len().min(nb.len()) - p;
    let mut s = pb.iter().rev().zip(nb.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    while !(prev.is_char_boundary(pb.len() - s) && new.is_char_boundary(nb.len() - s)) {
        s -= 1;
    }
    let idx = TextIndex::new(prev);
    let range = Range { start: idx.position(p), end: idx.position(pb.len() - s) };
    json!({"range": range_json(range), "text": &new[p..nb.len() - s]})
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
            if o["targetUri"].as_str().is_some_and(|u| uri_eq(u, go_uri)) {
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
        if o.get("uri").and_then(Value::as_str).is_some_and(|u| uri_eq(u, go_uri)) {
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
    use crate::gopls::adapter::regions_of;
    use std::io::Cursor;

    fn doc(src: &str) -> VirtualDoc {
        build(src, &regions_of(src), &Options::for_page("p", "main"))
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
        let wide = json!({"range": {"start": {"line": pos.line, "character": pos.character}, "end": {"line": pos.line + 1, "character": 0}}, "message": "m"});
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
        assert_eq!(items[0]["textEdit"]["range"]["start"], json!({"line": 0, "character": 5}));
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
        assert_eq!(out["range"]["start"], json!({"line": 0, "character": 5}));
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
        b.update(p, src, &regions_of(src));
        b.update(p, src, &regions_of(src));
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
        b.update(p, src, &regions_of(src));
        let c = CancelToken::default();
        assert!(b.hover(p, Position { line: 0, character: 4 }, &c).unwrap().is_none());
    }

    /// Apply a change produced by `incremental_change` to `prev` the way a server would.
    fn apply(prev: &str, change: &Value) -> String {
        let idx = TextIndex::new(prev);
        let r = parse_range(&change["range"]).unwrap();
        let (a, b) = (idx.offset(r.start).unwrap(), idx.offset(r.end).unwrap());
        format!("{}{}{}", &prev[..a], change["text"].as_str().unwrap(), &prev[b..])
    }

    #[test]
    fn incremental_change_reproduces_the_new_text() {
        let cases = [
            ("", "abc"),
            ("abc", ""),
            ("abc", "abc"),
            ("abc", "abXc"),
            ("a\nb\nc", "a\nXb\nc"),
            ("héllo wörld", "héllo wxrld"),
            ("😀a😀", "😀b😀"),
            ("😀", "😁"), // same first bytes, differ inside a 4-byte char
            ("line1\nline2\nline3\nline4", "line1\nNEW\nline4"),
            ("aaa", "aaaa"),
            ("aaaa", "aa"),
        ];
        for (a, b) in cases {
            let c = incremental_change(a, b);
            assert_eq!(apply(a, &c), b, "{a:?} -> {b:?}: {c}");
        }
    }

    #[test]
    fn incremental_change_is_minimal_and_positions_are_utf16() {
        let c = incremental_change("x😀 = 1\ny", "x😀 = 22\ny");
        assert_eq!(c["range"]["start"], json!({"line": 0, "character": 6}));
        assert_eq!(c["range"]["end"], json!({"line": 0, "character": 7}));
        assert_eq!(c["text"], "22");
        // A multi-line replacement is a single range.
        let c = incremental_change("a\nb\nc\nd", "a\nX\nd");
        assert_eq!(c["range"]["start"], json!({"line": 1, "character": 0}));
        assert_eq!(c["range"]["end"], json!({"line": 2, "character": 1}));
        assert_eq!(c["text"], "X");
    }

    #[test]
    fn windows_uris() {
        let p = PathBuf::from("C:\\Users\\me\\my site\\home.ghp");
        assert_eq!(path_to_uri(&p), "file:///C:/Users/me/my%20site/home.ghp");
        assert_eq!(uri_to_path_for("file:///C:/Users/me/my%20site/home.ghp", true), Some(p.clone()));
        assert_eq!(
            uri_to_path_for("file:///c%3A/Users/me/my%20site/home.ghp", true),
            Some(PathBuf::from("c:\\Users\\me\\my site\\home.ghp"))
        );
        // UNC share.
        let unc = PathBuf::from("\\\\srv\\share\\a.ghp");
        assert_eq!(path_to_uri(&unc), "file://srv/share/a.ghp");
        assert_eq!(uri_to_path_for("file://srv/share/a.ghp", true), Some(unc));
        // Unix-style URIs are untouched, `localhost` authority is dropped.
        assert_eq!(uri_to_path_for("file:///tmp/a%20b", false), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(uri_to_path_for("file://localhost/tmp/a", false), Some(PathBuf::from("/tmp/a")));
        assert_eq!(uri_to_path_for("http://x", false), None);
    }

    #[test]
    fn uri_equality_ignores_drive_case_and_escaping() {
        assert!(uri_eq("file:///C:/a/p.ghp.go", "file:///c%3A/a/p.ghp.go"));
        assert!(uri_eq("file:///tmp/a b/p.ghp.go", "file:///tmp/a%20b/p.ghp.go"));
        assert!(!uri_eq("file:///tmp/a.go", "file:///tmp/b.go"));
        assert_eq!(normalize_uri("file:///D:/x"), "/d:/x");
    }

    #[test]
    fn locations_match_gopls_spelling_of_the_uri() {
        let src = "<go\n  a := 1\n/><go= a />";
        let d = doc(src);
        let off = d.text.find("a := 1").unwrap();
        let r = range_json(Range { start: d.map.go_index().position(off), end: d.map.go_index().position(off + 1) });
        let v = json!([{"uri": "file:///c%3A/p.ghp.go", "range": r}]);
        let out = translate_locations(&d, "file:///C:/p.ghp.go", "file:///C:/p.ghp", v);
        assert_eq!(out[0]["uri"], "file:///C:/p.ghp");
    }

    #[test]
    fn sync_sends_open_then_minimal_changes_and_nothing_when_unchanged() {
        // A fake server: `cat` swallows whatever we write, so only the bookkeeping is observed.
        let mut child = Command::new(if cfg!(windows) { "cmd" } else { "cat" })
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let server = Server {
            child: Mutex::new(child),
            stdin,
            pending: Arc::new(Mutex::new(HashMap::new())),
            alive: Arc::new(AtomicBool::new(true)),
            next_id: AtomicI64::new(1),
            synced: Mutex::new(HashMap::new()),
        };
        let snap = |v: i64, t: &str| Snapshot { uri: "file:///p.ghp.go".into(), version: v, text: t.into() };
        server.sync_doc(&snap(1, "a")).unwrap();
        assert_eq!(server.synced.lock_ok()["file:///p.ghp.go"], "a");
        server.sync_doc(&snap(2, "ab")).unwrap();
        server.sync_doc(&snap(2, "ab")).unwrap();
        assert_eq!(server.synced.lock_ok()["file:///p.ghp.go"], "ab");
    }
}
