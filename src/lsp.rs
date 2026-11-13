//! Minimal LSP client: JSON-RPC over stdio, one reader thread and one writer thread.
//! The server is spawned lazily and with a Go memory limit (tsgo is written in Go).

use crate::lsp_stream::{Frame, Limits, Policy, policy_for, read_message};
use crate::lsplog::{self, SharedLog};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::Instant,
};

pub type Wake = Arc<dyn Fn() + Send + Sync>;

// ───────────────────────────── framing ─────────────────────────────

pub fn frame(v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

// ───────────────────────────── messages ─────────────────────────────

pub enum Msg {
    Response {
        id: i64,
        result: Result<Value, Value>,
    },
    /// A `semanticTokens/*` response, with its integers packed (see `lsp_stream`).
    Tokens {
        id: i64,
        data: Vec<u32>,
    },
    /// A `documentSymbol` response, flattened (and trimmed when huge; see `lsp_stream`).
    Symbols {
        id: i64,
        syms: Vec<crate::doc::Sym>,
    },
    /// A `workspace/diagnostic` response, parsed and capped on the reader thread (see `problems`).
    WsDiag {
        id: i64,
        report: crate::problems::WsReport,
    },
    /// A partial result (`$/progress` with one of our `workspace/diagnostic` tokens).
    WsPartial {
        token: String,
        report: crate::problems::WsReport,
    },
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Exited,
}

/// What a pending request was for, so the response can be routed.
#[derive(Clone, Debug)]
pub enum Pending {
    Initialize,
    Diagnostic {
        uri: String,
    },
    Hover {
        uri: String,
        at: usize,
    },
    HoverExtra {
        uri: String,
        at: usize,
        serial: u64,
        what: crate::app_hover::HoverWhat,
    },
    Completion {
        uri: String,
        at: usize,
    },
    Definition {
        kind: DefKind,
    },
    References,
    Rename,
    /// A request the caller waits for synchronously (`App::lsp_request_wait`); a late answer is dropped.
    Blocking,
    CodeActions,
    Signature {
        uri: String,
    },
    Symbols {
        uri: String,
    },
    ResolveCompletion {
        uri: String,
        version: i32,
    },
    ResolveAction,
    SelectionRange,
    SymbolsCache {
        uri: String,
    },
    Folding {
        uri: String,
    },
    SemTokens {
        uri: String,
    },
    /// `semanticTokens/range` for lines `first..=last` of document `version` (big files).
    SemRange {
        uri: String,
        version: i32,
        first: usize,
        last: usize,
    },
    CodeLens {
        uri: String,
    },
    InlayHint {
        uri: String,
        version: i32,
        first: usize,
        last: usize,
    },
    ResolveLens {
        uri: String,
        line: usize,
    },
    PrepareCalls {
        incoming: bool,
    },
    Calls {
        incoming: bool,
    },
    Organize,
    /// `workspace/symbol` for the Go to picker's `#` mode; `serial` tells a stale answer (see `app_goto`).
    WorkspaceSymbols {
        serial: u64,
    },
    ExecuteCommand,
    /// `workspace/diagnostic` for the Problems panel's whole-project scope (see `app_problems`).
    WorkspaceDiag {
        token: String,
    },
    /// One answer of a request sent to several servers of a document (see `lspmerge`).
    Multi {
        kind: crate::lspmerge::MultiKind,
        serial: u64,
    },
    /// `textDocument/documentHighlight` for the document at (real) `uri`; `order` 0 is the primary.
    Highlight {
        uri: String,
        serial: u64,
        order: usize,
    },
    /// `textDocument/linkedEditingRange` for the document at (real) `uri`, buffer version `ver`.
    LinkedEditing {
        uri: String,
        serial: u64,
        ver: u64,
        at: usize,
    },
    /// `textDocument/inlineCompletion` number `serial` (see `inline`).
    InlineCompletion {
        serial: u64,
    },
    /// A step of GitHub Copilot's sign-in (see `app_inline`).
    Copilot(crate::inline::CopilotStep),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefKind {
    Definition,
    TypeDefinition,
    Implementation,
}

pub struct Lsp {
    pub root: PathBuf,
    child: Option<Child>,
    pid: u32,
    tx: Sender<Vec<u8>>,
    pub rx: Receiver<Msg>,
    next_id: i64,
    pub pending: HashMap<i64, Pending>,
    /// How the reader thread should parse each in-flight response (see `lsp_stream`).
    policies: Arc<Mutex<HashMap<i64, Policy>>>,
    pub ready: bool,
    queue: Vec<Value>,
    pub caps: Value,
    pub name: String,
    /// Narrow mirror this server runs in (None: the real project directory).
    pub mirror: Option<PathBuf>,
    pub includes: Vec<PathBuf>,
    /// Set for servers from the registry (everything but tsgo and Biome): the registry id, and what they need at runtime.
    pub generic: Option<Generic>,
    /// Documents (server-side URIs) currently open on this server.
    pub open_uris: HashSet<String>,
    /// When the last document closed (the server is stopped after an idle grace period).
    pub idle_since: Option<Instant>,
    pub started: Instant,
    pub log: Option<SharedLog>,
    /// Latest `$/progress` title, shown in the status bar while it is running.
    pub progress: HashMap<String, String>,
    /// Dynamic registrations by registration id -> method.
    pub registrations: HashMap<String, String>,
    pub shutting_down: bool,
}

/// What a registry server needs after the handshake.
#[derive(Clone, Debug)]
pub struct Generic {
    pub id: String,
    pub settings: Value,
    pub primary: bool,
    pub command: PathBuf,
}

/// Everything `Lsp::spawn_with` needs beyond the plain `spawn` arguments.
pub struct Extra {
    pub env: Vec<(String, String)>,
    pub go_limit: bool,
    pub generic: Option<Generic>,
    pub init_options: Value,
}

impl Lsp {
    /// Spawn `bin args...` as a language server rooted at `root`.
    pub fn spawn(
        name: &str,
        bin: &Path,
        args: &[&str],
        root: &Path,
        work_dir: &Path,
        mem_limit_mib: u32,
        wake: Wake,
    ) -> std::io::Result<Lsp> {
        let extra = Extra { env: Vec::new(), go_limit: true, generic: None, init_options: Value::Null };
        Lsp::spawn_with(name, bin, args, root, work_dir, mem_limit_mib, wake, extra)
    }

    /// `spawn` with environment, registry metadata and `initializationOptions`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with(
        name: &str,
        bin: &Path,
        args: &[&str],
        root: &Path,
        work_dir: &Path,
        mem_limit_mib: u32,
        wake: Wake,
        extra: Extra,
    ) -> std::io::Result<Lsp> {
        let mut cmd = Command::new(bin);
        cmd.args(args).current_dir(work_dir).stdin(Stdio::piped()).stdout(Stdio::piped());
        let log: Option<SharedLog> = extra.generic.as_ref().map(|_| SharedLog::default());
        cmd.stderr(if log.is_some() { Stdio::piped() } else { Stdio::null() });
        if extra.go_limit {
            // Go runtime: make the GC work harder before growing the heap.
            cmd.env("GOMEMLIMIT", format!("{mem_limit_mib}MiB")).env("GOGC", "50");
        }
        cmd.envs(extra.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut child = cmd.spawn()?;
        crate::startup::mark("lsp spawned");
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        if let (Some(log), Some(err)) = (log.clone(), child.stderr.take()) {
            thread::spawn(move || {
                for line in BufReader::new(err).lines() {
                    let Ok(line) = line else { break };
                    lsplog::push(&log, "stderr", &line);
                }
            });
        }

        let (tx, wrx) = channel::<Vec<u8>>();
        thread::spawn(move || {
            while let Ok(bytes) = wrx.recv() {
                if stdin.write_all(&bytes).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
        });

        let (mtx, rx) = channel::<Msg>();
        let policies: Arc<Mutex<HashMap<i64, Policy>>> = Arc::default();
        let reader_policies = policies.clone();
        let rlog = log.clone();
        thread::spawn(move || {
            // Frames are parsed while they are read, so a huge response never sits in memory.
            let mut input = BufReader::with_capacity(64 * 1024, stdout);
            let lim = Limits::default();
            let policy_of = |id: i64| reader_policies.lock().ok().and_then(|m| m.get(&id).copied()).unwrap_or_default();
            loop {
                match read_message(&mut input, &lim, &policy_of) {
                    Ok(Frame::Eof) | Err(_) => {
                        let _ = mtx.send(Msg::Exited);
                        wake();
                        break;
                    }
                    Ok(Frame::Skipped) => {}
                    Ok(Frame::Msg(m)) => {
                        if let Some(l) = &rlog {
                            lsplog::push(l, "<-", &summary(&m));
                        }
                        if let Msg::Response { id, .. }
                        | Msg::Tokens { id, .. }
                        | Msg::Symbols { id, .. }
                        | Msg::WsDiag { id, .. } = &m
                            && let Ok(mut p) = reader_policies.lock()
                        {
                            p.remove(id);
                        }
                        if mtx.send(m).is_err() {
                            return;
                        }
                        wake();
                    }
                }
            }
        });

        let mut lsp = Lsp {
            root: root.to_path_buf(),
            pid: child.id(),
            child: Some(child),
            tx,
            rx,
            next_id: 1,
            pending: HashMap::new(),
            policies,
            ready: false,
            queue: Vec::new(),
            caps: Value::Null,
            name: name.to_string(),
            mirror: None,
            includes: Vec::new(),
            open_uris: HashSet::new(),
            idle_since: None,
            started: Instant::now(),
            log,
            progress: HashMap::new(),
            registrations: HashMap::new(),
            shutting_down: false,
            generic: extra.generic,
        };
        let params = initialize_params(work_dir, lsp.generic.as_ref().map(|_| &extra.init_options));
        let id = lsp.alloc_id();
        lsp.pending.insert(id, Pending::Initialize);
        lsp.write(&json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":params}));
        Ok(lsp)
    }

    fn alloc_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn write(&self, v: &Value) {
        if let Some(l) = &self.log {
            let m = v["method"].as_str().unwrap_or("response");
            let id = v.get("id").map(|i| format!(" #{i}")).unwrap_or_default();
            lsplog::push(l, "->", &format!("{m}{id}"));
        }
        let _ = self.tx.send(frame(v));
    }

    fn send_or_queue(&mut self, v: Value) {
        if self.ready {
            self.write(&v);
        } else {
            self.queue.push(v);
        }
    }

    pub fn request(&mut self, method: &str, params: Value, what: Pending) -> i64 {
        let id = self.alloc_id();
        self.pending.insert(id, what);
        let policy = policy_for(method);
        if policy != Policy::Value
            && let Ok(mut p) = self.policies.lock()
        {
            p.insert(id, policy);
        }
        self.send_or_queue(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        id
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        // Track which documents the server has open (idle shutdown, companion sync).
        match method {
            "textDocument/didOpen" => {
                if let Some(u) = params["textDocument"]["uri"].as_str() {
                    self.open_uris.insert(u.to_string());
                    self.idle_since = None;
                }
            }
            "textDocument/didClose" => {
                if let Some(u) = params["textDocument"]["uri"].as_str() {
                    self.open_uris.remove(u);
                    if self.open_uris.is_empty() {
                        self.idle_since = Some(Instant::now());
                    }
                }
            }
            _ => {}
        }
        self.send_or_queue(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    /// Give up on request `id`: `$/cancelRequest`, and its answer (if one still comes) is dropped.
    pub fn cancel(&mut self, id: i64) {
        if self.pending.remove(&id).is_some() {
            self.send_or_queue(json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id": id}}));
        }
    }

    pub fn respond(&self, id: Value, result: Value) {
        self.write(&json!({"jsonrpc":"2.0","id":id,"result":result}));
    }

    /// Must be called with the `initialize` response.
    pub fn on_initialized(&mut self, result: &Value) {
        self.on_initialized_then(result, None);
    }

    /// `on_initialized`, sending `first` (a notification: method, params) right after `initialized`
    /// and before the queued `didOpen`s: settings must reach a server before it lints the first
    /// document (taplo, among others, does not re-lint when the configuration arrives later).
    pub fn on_initialized_then(&mut self, result: &Value, first: Option<(&str, Value)>) {
        self.caps = result["capabilities"].clone();
        self.ready = true;
        self.write(&json!({"jsonrpc":"2.0","method":"initialized","params":{}}));
        if let Some((method, params)) = first {
            self.write(&json!({"jsonrpc":"2.0","method":method,"params":params}));
        }
        for v in std::mem::take(&mut self.queue) {
            self.write(&v);
        }
    }

    pub fn shutdown(&mut self) {
        self.shutting_down = true;
        if self.ready {
            self.write(&json!({"jsonrpc":"2.0","id":self.next_id,"method":"shutdown","params":null}));
            self.write(&json!({"jsonrpc":"2.0","method":"exit"}));
        }
        let Some(mut child) = self.child.take() else { return };
        if self.generic.is_some() && self.ready {
            // Give a registry server a moment to stop its own helpers (proc-macro servers, node
            // workers) after `exit`, without blocking the UI: a thread reaps it, killing it if it lingers.
            thread::spawn(move || {
                let end = Instant::now() + std::time::Duration::from_millis(1500);
                while Instant::now() < end {
                    if child.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    thread::sleep(std::time::Duration::from_millis(20));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
            return;
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_generic(&self) -> bool {
        self.generic.is_some()
    }

    /// The server's own log (stderr and a one-line summary of the traffic), oldest first.
    pub fn log_text(&self) -> String {
        self.log.as_ref().and_then(|l| l.lock().ok().map(|l| l.text())).unwrap_or_default()
    }

    pub fn log_line(&self, tag: &str, text: &str) {
        if let Some(l) = &self.log {
            lsplog::push(l, tag, text);
        }
    }

    /// Does the server (statically or through dynamic registration) offer `provider`
    /// (a capability key such as `hoverProvider`)? Unknown servers are assumed not to.
    pub fn has_cap(&self, provider: &str) -> bool {
        let c = &self.caps[provider];
        !c.is_null() && c.as_bool() != Some(false)
    }
}

/// One line describing a received message for the server log.
fn summary(m: &Msg) -> String {
    match m {
        Msg::Response { id, result: Ok(_) } => format!("response #{id}"),
        Msg::Response { id, result: Err(e) } => {
            format!("response #{id} error: {}", e["message"].as_str().unwrap_or("?"))
        }
        Msg::Tokens { id, .. } | Msg::Symbols { id, .. } => format!("response #{id} (streamed)"),
        Msg::WsDiag { id, report } => format!("response #{id} (workspace diagnostics, {} files)", report.files.len()),
        Msg::WsPartial { report, .. } => format!("$/progress (workspace diagnostics, {} files)", report.files.len()),
        Msg::Notification { method, params } => match method.as_str() {
            "window/logMessage" | "window/showMessage" => {
                format!("{method}: {}", params["message"].as_str().unwrap_or(""))
            }
            _ => method.clone(),
        },
        Msg::Request { id, method, .. } => format!("request {method} #{id}"),
        Msg::Exited => "exited".into(),
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        if let Some(c) = &mut self.child {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub(crate) fn classify(v: Value) -> Option<Msg> {
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    match (method, v.get("id")) {
        (Some(method), Some(id)) => {
            Some(Msg::Request { id: id.clone(), method, params: v.get("params").cloned().unwrap_or(Value::Null) })
        }
        (Some(method), None) => {
            Some(Msg::Notification { method, params: v.get("params").cloned().unwrap_or(Value::Null) })
        }
        (None, Some(id)) => {
            let id = id.as_i64()?;
            let result = match v.get("error") {
                Some(e) => Err(e.clone()),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Msg::Response { id, result })
        }
        _ => None,
    }
}

fn initialize_params(root: &Path, generic: Option<&Value>) -> Value {
    let root_uri = crate::doc::path_to_uri(root);
    let mut p = json!({
        "processId": std::process::id(),
        "clientInfo": {"name": "lumen-text", "version": env!("CARGO_PKG_VERSION")},
        "rootUri": root_uri,
        "workspaceFolders": crate::workspace::folders_json(&crate::workspace::lsp_folders(root)),
        "capabilities": {
            "general": {"positionEncodings": ["utf-16"]},
            "workspace": {
                "configuration": true,
                "workspaceFolders": true,
                "applyEdit": true,
                "workspaceEdit": {"documentChanges": true},
                "symbol": {}
            },
            "textDocument": {
                "synchronization": {"didSave": true, "dynamicRegistration": false},
                "diagnostic": {"dynamicRegistration": false, "relatedDocumentSupport": true},
                "publishDiagnostics": {},
                "hover": {"contentFormat": ["markdown", "plaintext"]},
                "completion": {
                    "completionItem": {"snippetSupport": false, "documentationFormat": ["markdown", "plaintext"], "insertReplaceSupport": false, "resolveSupport": {"properties": ["additionalTextEdits", "detail", "documentation"]}},
                    "contextSupport": true
                },
                "signatureHelp": {"signatureInformation": {"documentationFormat": ["plaintext", "markdown"], "parameterInformation": {"labelOffsetSupport": false}}},
                "definition": {"linkSupport": false},
                "typeDefinition": {"linkSupport": false},
                "implementation": {"linkSupport": false},
                "references": {},
                "rename": {"prepareSupport": false},
                "formatting": {},
                "codeAction": {"resolveSupport": {"properties": ["edit"]}, "dataSupport": true, "codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["quickfix", "refactor", "source", "source.organizeImports"]}}},
                "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                "selectionRange": {},
                "foldingRange": {"lineFoldingOnly": true},
                "callHierarchy": {},
                "documentHighlight": {"dynamicRegistration": false},
                "linkedEditingRange": {"dynamicRegistration": false},
                "codeLens": {},
                "inlayHint": {"dynamicRegistration": false},
                "semanticTokens": {
                    "requests": {"full": true},
                    "tokenTypes": ["namespace", "type", "class", "enum", "interface", "struct", "typeParameter", "parameter", "variable", "property", "enumMember", "event", "function", "method", "macro", "keyword", "modifier", "comment", "string", "number", "regexp", "operator", "decorator"],
                    "tokenModifiers": ["declaration", "definition", "readonly", "static", "deprecated", "abstract", "async", "modification", "documentation", "defaultLibrary"],
                    "formats": ["relative"]
                }
            }
        }
    });
    // Servers from the registry get the extra client features generic servers expect; tsgo's
    // handshake stays exactly as it was.
    if let Some(opts) = generic {
        let caps = &mut p["capabilities"];
        caps["workspace"]["didChangeConfiguration"] = json!({"dynamicRegistration": false});
        caps["workspace"]["didChangeWatchedFiles"] = json!({"dynamicRegistration": true});
        caps["workspace"]["diagnostics"] = json!({"refreshSupport": true});
        caps["window"] = json!({"workDoneProgress": true, "showMessage": {}});
        caps["general"]["positionEncodings"] = json!(["utf-16"]);
        let td = &mut caps["textDocument"];
        td["synchronization"]["dynamicRegistration"] = json!(true);
        td["formatting"] = json!({"dynamicRegistration": true});
        td["rename"] = json!({"prepareSupport": false, "dynamicRegistration": true});
        td["completion"]["completionItem"]["deprecatedSupport"] = json!(true);
        td["codeAction"]["isPreferredSupport"] = json!(true);
        td["inlineCompletion"] = json!({"dynamicRegistration": true});
        td["documentSymbol"]["symbolKind"] = json!({});
        td["publishDiagnostics"] =
            json!({"relatedInformation": false, "versionSupport": true, "codeDescriptionSupport": true});
        if !opts.is_null() {
            p["initializationOptions"] = opts.clone();
        }
    }
    p
}

// ───────────────────────────── server discovery ─────────────────────────────

/// The tsgo bundled with the editor (env override, user data dir, or PATH).
fn bundled_tsgo() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LUMEN_TSGO") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(data) = crate::paths::data_home() {
        let p = data.join(format!(
            "lumen-text/tsgo/node_modules/@typescript/native-preview-{}/lib/tsgo{}",
            tsgo_platform(),
            std::env::consts::EXE_SUFFIX
        ));
        if p.exists() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    let names: &[&str] = if cfg!(windows) { &["tsgo.exe", "tsgo.cmd"] } else { &["tsgo"] };
    std::env::split_paths(&path).flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

/// `<os>-<arch>` as the `@typescript/native-preview-*` packages name their platforms.
fn tsgo_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        o => o,
    };
    let arch = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "x64" };
    format!("{os}-{arch}")
}

/// The tsgo a project installed itself (`node_modules/@typescript/native-preview-*`).
fn project_tsgo(root: &Path) -> Option<PathBuf> {
    root.ancestors().find_map(|d| {
        std::fs::read_dir(d.join("node_modules/@typescript"))
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("native-preview-"))
            .map(|e| e.path().join(format!("lib/tsgo{}", std::env::consts::EXE_SUFFIX)))
            .find(|p| p.exists())
    })
}

/// Pick the tsgo binary: `source` is "project", "bundled" or anything else for auto
/// (the project's own version when it has one, otherwise the bundled one).
pub fn find_tsgo(root: &Path, source: &str) -> Option<PathBuf> {
    match source {
        "project" => project_tsgo(root),
        "bundled" => bundled_tsgo(),
        _ => project_tsgo(root).or_else(bundled_tsgo),
    }
}

pub fn tsgo_version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Nearest ancestor with a tsconfig/jsconfig/package.json (or .git as a last resort).
pub fn project_root(file: &Path) -> Option<PathBuf> {
    let mut best_git = None;
    for d in file.ancestors().skip(1) {
        if d.join("tsconfig.json").exists() || d.join("jsconfig.json").exists() {
            return Some(d.to_path_buf());
        }
        if best_git.is_none() && d.join(".git").exists() {
            best_git = Some(d.to_path_buf());
        }
    }
    best_git.or_else(|| file.parent().map(Path::to_path_buf))
}

// ───────────────────────────── narrow mirror ─────────────────────────────

/// Where the mirror for `root` lives.
pub fn mirror_dir(root: &Path) -> PathBuf {
    let h =
        root.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    let base = crate::paths::cache_home().unwrap_or_else(std::env::temp_dir);
    base.join("lumen-text/mirrors").join(format!("{h:016x}"))
}

/// Make `mirror` look like `root` (symlinks to every top-level entry) but with its own
/// `tsconfig.json` that extends the project's and includes only `includes`. The language
/// server then loads just those files (+ their imports) instead of the whole project, which
/// cut tsgo's RAM 3-5x in measurements. Returns true when the tsconfig changed.
pub fn write_mirror(root: &Path, mirror: &Path, includes: &[PathBuf]) -> std::io::Result<bool> {
    std::fs::create_dir_all(mirror)?;
    let mut wanted: Vec<std::ffi::OsString> = Vec::new();
    for e in std::fs::read_dir(root)?.flatten() {
        let name = e.file_name();
        let n = name.to_string_lossy();
        if n.starts_with("tsconfig") || n.starts_with("jsconfig") || n == ".git" {
            continue;
        }
        let link = mirror.join(&name);
        let target = e.path();
        let ok = std::fs::read_link(&link).map(|t| t == target).unwrap_or(false);
        if !ok {
            let _ = std::fs::remove_file(&link);
            crate::paths::symlink(&target, &link)?;
        }
        wanted.push(name);
    }
    // Drop links to things that no longer exist at the top level.
    for e in std::fs::read_dir(mirror)?.flatten() {
        let name = e.file_name();
        if name == "tsconfig.json" || wanted.contains(&name) {
            continue;
        }
        let _ = std::fs::remove_file(e.path());
    }
    let mut rels: Vec<String> =
        includes.iter().filter_map(|p| p.strip_prefix(root).ok().map(|r| r.to_string_lossy().into_owned())).collect();
    rels.sort();
    rels.dedup();
    // `rootDir` must contain the mirror (the included files) AND the real project (imports resolve
    // through the symlinks to real paths, and the project's own `rootDir` may be inherited): "." made
    // every such import a TS6059. Nothing is emitted, so the filesystem root is harmless.
    let fs_root = mirror.ancestors().last().unwrap_or(mirror).to_string_lossy().into_owned();
    let mut cfg =
        json!({"include": rels, "compilerOptions": {"incremental": false, "noEmit": true, "rootDir": fs_root}});
    let real_cfg = root.join("tsconfig.json");
    if real_cfg.exists() {
        cfg["extends"] = json!(real_cfg.to_string_lossy());
    }
    let text = serde_json::to_string_pretty(&cfg).unwrap_or_default();
    let path = mirror.join("tsconfig.json");
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    std::fs::write(path, text)?;
    Ok(true)
}

// ───────────────────────────── response helpers ─────────────────────────────

/// Plain text out of the many shapes of hover `contents`, with markdown fences removed.
pub fn hover_text(v: &Value) -> String {
    fn one(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or("").to_string(),
            Value::Array(a) => a.iter().map(one).collect::<Vec<_>>().join("\n\n"),
            _ => String::new(),
        }
    }
    let raw = one(&v["contents"]);
    let mut out = Vec::new();
    for line in raw.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        out.push(line.trim_end().to_string());
    }
    out.join("\n").trim().to_string()
}

/// (uri, line, utf16 col) of the first location in a definition-like response.
pub fn locations(v: &Value) -> Vec<(String, usize, usize)> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => vec![],
    };
    items
        .into_iter()
        .filter_map(|l| {
            let uri = l.get("uri").or_else(|| l.get("targetUri"))?.as_str()?.to_string();
            let r = l.get("range").or_else(|| l.get("targetSelectionRange"))?;
            Some((uri, r["start"]["line"].as_u64()? as usize, r["start"]["character"].as_u64()? as usize))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_routes_messages() {
        assert!(matches!(classify(json!({"id": 3, "result": null})), Some(Msg::Response { id: 3, .. })));
        assert!(matches!(classify(json!({"id": 3, "error": {"code": 1}})), Some(Msg::Response { result: Err(_), .. })));
        assert!(matches!(classify(json!({"method": "a"})), Some(Msg::Notification { .. })));
        assert!(matches!(classify(json!({"id": "s", "method": "a"})), Some(Msg::Request { .. })));
    }

    #[test]
    fn hover_and_locations_shapes() {
        let h = json!({"contents": {"kind": "markdown", "value": "```ts\nconst x: number\n```\nA number"}});
        assert_eq!(hover_text(&h), "const x: number\nA number");
        let h2 = json!({"contents": ["a", {"language": "ts", "value": "b"}]});
        assert_eq!(hover_text(&h2), "a\n\nb");
        let l = json!([{"uri": "file:///a.ts", "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 9}}}]);
        assert_eq!(locations(&l), vec![("file:///a.ts".to_string(), 3, 4)]);
        let link = json!([{"targetUri": "file:///b.ts", "targetSelectionRange": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 3}}}]);
        assert_eq!(locations(&link), vec![("file:///b.ts".to_string(), 1, 2)]);
    }

    #[test]
    fn mirror_links_top_level_and_tracks_includes() {
        let base = std::env::temp_dir().join(format!("lumen-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("proj");
        let mirror = base.join("mirror");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("tsconfig.json"), "{}").unwrap();
        std::fs::write(root.join("tsconfig.build.json"), "{}").unwrap();
        std::fs::write(root.join("src/a.ts"), "").unwrap();
        let a = root.join("src/a.ts");
        assert!(write_mirror(&root, &mirror, &[a.clone()]).unwrap(), "first write changes the tsconfig");
        assert!(mirror.join("src/a.ts").exists(), "src is linked through");
        assert!(mirror.join("node_modules").exists());
        assert!(!mirror.join("tsconfig.build.json").exists(), "project tsconfigs are not mirrored");
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(mirror.join("tsconfig.json")).unwrap()).unwrap();
        assert_eq!(cfg["include"], json!(["src/a.ts"]));
        assert_eq!(cfg["extends"], json!(root.join("tsconfig.json").to_string_lossy()));
        assert!(!write_mirror(&root, &mirror, &[a.clone()]).unwrap(), "same includes: no change");
        std::fs::write(root.join("src/b.ts"), "").unwrap();
        assert!(write_mirror(&root, &mirror, &[a, root.join("src/b.ts")]).unwrap());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn project_root_prefers_tsconfig() {
        let root = std::env::temp_dir().join(format!("lumen-root-{}", std::process::id()));
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/tsconfig.json"), "{}").unwrap();
        std::fs::write(root.join("a/b/x.ts"), "").unwrap();
        assert_eq!(project_root(&root.join("a/b/x.ts")), Some(root.join("a")));
        std::fs::remove_dir_all(&root).ok();
    }

    /// End-to-end against a real tsgo when present: initialize, open a bad file, pull diagnostics.
    #[test]
    fn real_tsgo_reports_type_errors() {
        let Some(bin) = find_tsgo(Path::new("/nonexistent"), "bundled") else {
            eprintln!("tsgo not installed, skipping");
            return;
        };
        let root = std::env::temp_dir().join(format!("lumen-tsgo-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{"compilerOptions":{"strict":true,"noEmit":true},"include":["*.ts"]}"#,
        )
        .unwrap();
        let file = root.join("bad.ts");
        let src = "const x: number = \"a\"\nexport const y = nope\n";
        std::fs::write(&file, src).unwrap();
        let uri = crate::doc::path_to_uri(&file);

        let mut lsp = Lsp::spawn("tsgo", &bin, &["--lsp", "--stdio"], &root, &root, 512, Arc::new(|| {})).unwrap();
        lsp.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "typescript", "version": 1, "text": src}}),
        );
        lsp.request(
            "textDocument/diagnostic",
            json!({"textDocument": {"uri": uri}}),
            Pending::Diagnostic { uri: uri.clone() },
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut items = None;
        while std::time::Instant::now() < deadline && items.is_none() {
            let Ok(m) = lsp.rx.recv_timeout(std::time::Duration::from_millis(200)) else { continue };
            match m {
                Msg::Response { id, result } => match lsp.pending.remove(&id) {
                    Some(Pending::Initialize) => lsp.on_initialized(&result.unwrap()),
                    Some(Pending::Diagnostic { .. }) => items = Some(result.unwrap()["items"].clone()),
                    _ => {}
                },
                Msg::Request { id, .. } => lsp.respond(id, Value::Null),
                _ => {}
            }
        }
        lsp.shutdown();
        std::fs::remove_dir_all(&root).ok();
        let items = items.expect("no diagnostic response within 20s");
        let msgs: Vec<String> =
            items.as_array().unwrap().iter().map(|d| d["message"].as_str().unwrap_or("").to_string()).collect();
        assert!(msgs.iter().any(|m| m.contains("not assignable")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("Cannot find name")), "{msgs:?}");
    }

    /// Big-file path against a real tsgo: a range of semantic tokens (absolute lines, packed by the
    /// reader) and a trimmed outline for a 30k-line file, in a throwaway project under the cache dir.
    #[test]
    fn real_tsgo_serves_token_ranges_and_outline_of_a_big_file() {
        let Some(bin) = find_tsgo(Path::new("/nonexistent"), "bundled") else {
            eprintln!("tsgo not installed, skipping");
            return;
        };
        let base = crate::paths::cache_home().unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("lumen-bigtest-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{"compilerOptions":{"strict":true,"noEmit":true},"include":["*.ts"]}"#,
        )
        .unwrap();
        let mut src = String::new();
        for i in 0..3000 {
            src.push_str(&format!("export class C{i} {{\n  m{i}(x: number): number {{\n    const y = x + {i}\n    return y\n  }}\n}}\n\n\n\n\n"));
        }
        let file = root.join("big.ts");
        std::fs::write(&file, &src).unwrap();
        let uri = crate::doc::path_to_uri(&file);
        let mut lsp = Lsp::spawn("tsgo", &bin, &["--lsp", "--stdio"], &root, &root, 512, Arc::new(|| {})).unwrap();
        lsp.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "typescript", "version": 1, "text": src}}),
        );
        let (first, last) = (15_000u64, 15_050u64);
        lsp.request("textDocument/semanticTokens/range", json!({"textDocument": {"uri": uri}, "range": {"start": {"line": first, "character": 0}, "end": {"line": last, "character": 0}}}), Pending::SemRange { uri: uri.clone(), version: 1, first: first as usize, last: last as usize });
        lsp.request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
            Pending::SymbolsCache { uri: uri.clone() },
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let (mut tokens, mut syms) = (None, None);
        while std::time::Instant::now() < deadline && (tokens.is_none() || syms.is_none()) {
            let Ok(m) = lsp.rx.recv_timeout(std::time::Duration::from_millis(200)) else { continue };
            match m {
                Msg::Response { id, result } => {
                    if let Some(Pending::Initialize) = lsp.pending.remove(&id) {
                        lsp.on_initialized(&result.unwrap());
                    }
                }
                Msg::Tokens { id, data } => {
                    lsp.pending.remove(&id);
                    tokens = Some(data);
                }
                Msg::Symbols { id, syms: s } => {
                    lsp.pending.remove(&id);
                    syms = Some(s);
                }
                Msg::Request { id, .. } => lsp.respond(id, Value::Null),
                _ => {}
            }
        }
        lsp.shutdown();
        std::fs::remove_dir_all(&root).ok();
        let tokens = tokens.expect("no semantic token response");
        assert!(!tokens.is_empty() && tokens.len() % 5 == 0, "{} ints", tokens.len());
        assert!(tokens[0] as u64 >= first, "the first token's delta line is absolute from line 0: {}", tokens[0]);
        let syms = syms.expect("no outline response");
        assert!(syms.len() >= 3000, "{} symbols", syms.len());
        assert!(syms.iter().all(|s| s.depth <= 2));
    }

    /// Pull diagnostics of `uri` from a live server and return the messages.
    fn pull_msgs(lsp: &mut Lsp, uri: &str) -> Vec<String> {
        lsp.request(
            "textDocument/diagnostic",
            json!({"textDocument": {"uri": uri}}),
            Pending::Diagnostic { uri: uri.to_string() },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let Ok(m) = lsp.rx.recv_timeout(std::time::Duration::from_millis(200)) else { continue };
            match m {
                Msg::Response { id, result } => match lsp.pending.remove(&id) {
                    Some(Pending::Initialize) => lsp.on_initialized(&result.unwrap()),
                    Some(Pending::Diagnostic { .. }) => {
                        let items = result.unwrap()["items"].clone();
                        return items
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|d| d["message"].as_str().unwrap_or("").to_string())
                            .collect();
                    }
                    _ => {}
                },
                Msg::Request { id, .. } => lsp.respond(id, Value::Null),
                _ => {}
            }
        }
        panic!("no diagnostic response");
    }

    fn cache_base() -> PathBuf {
        crate::paths::cache_home().unwrap_or_else(std::env::temp_dir)
    }

    /// The bug behind "red imports until the target is opened": with only `a.ts` open, imports of
    /// unopened files (`./b.js` -> `b.ts`, a directory index) must resolve, both in the default
    /// full-project mode and in the opt-in lean mirror (whose rootDir used to clash with the
    /// project's `rootDir: "."`); deleting and re-creating the dependency on disk must refresh the
    /// importer after `didChangeWatchedFiles`.
    #[test]
    fn real_tsgo_resolves_unopened_imports_and_follows_disk_changes() {
        let Some(bin) = find_tsgo(Path::new("/nonexistent"), "bundled") else {
            eprintln!("tsgo not installed, skipping");
            return;
        };
        for lean in [false, true] {
            let base = cache_base().join(format!("lumen-imports-{}-{lean}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let root = base.join("proj");
            std::fs::create_dir_all(root.join("src/util")).unwrap();
            std::fs::write(
                root.join("tsconfig.json"),
                r#"{"compilerOptions":{"module":"ESNext","moduleResolution":"Bundler","strict":true,"noEmit":true,"rootDir":"."},"include":["src"]}"#,
            )
            .unwrap();
            std::fs::write(root.join("src/b.ts"), "export const b: number = 1\n").unwrap();
            std::fs::write(root.join("src/util/index.ts"), "export const u = 2\n").unwrap();
            let a = root.join("src/a.ts");
            let src = "import { b } from \"./b.js\"\nimport { u } from \"./util\"\nexport const s: number = b + u\n";
            std::fs::write(&a, src).unwrap();
            let (work, mirror) = if lean {
                let m = base.join("mirror");
                write_mirror(&root, &m, &[a.clone()]).unwrap();
                (m.clone(), Some(m))
            } else {
                (root.clone(), None)
            };
            let open = if let Some(m) = &mirror { m.join("src/a.ts") } else { a.clone() };
            let uri = crate::doc::path_to_uri(&open);
            let mut lsp = Lsp::spawn("tsgo", &bin, &["--lsp", "--stdio"], &root, &work, 512, Arc::new(|| {})).unwrap();
            lsp.notify(
                "textDocument/didOpen",
                json!({"textDocument": {"uri": uri, "languageId": "typescript", "version": 1, "text": src}}),
            );
            let msgs = pull_msgs(&mut lsp, &uri);
            assert!(msgs.is_empty(), "lean={lean}: imports of unopened files must resolve: {msgs:?}");

            let b_uri = crate::doc::path_to_uri(&work.join("src/b.ts"));
            std::fs::remove_file(root.join("src/b.ts")).unwrap();
            lsp.notify("workspace/didChangeWatchedFiles", json!({"changes": [{"uri": b_uri, "type": 3}]}));
            let msgs = pull_msgs(&mut lsp, &uri);
            assert!(msgs.iter().any(|m| m.contains("Cannot find module")), "lean={lean}: deleted dependency: {msgs:?}");

            std::fs::write(root.join("src/b.ts"), "export const b: number = 1\n").unwrap();
            lsp.notify("workspace/didChangeWatchedFiles", json!({"changes": [{"uri": b_uri, "type": 1}]}));
            let msgs = pull_msgs(&mut lsp, &uri);
            assert!(msgs.is_empty(), "lean={lean}: re-created dependency: {msgs:?}");
            lsp.shutdown();
            std::fs::remove_dir_all(&base).ok();
        }
    }

    #[test]
    fn lean_mirror_overrides_root_dir() {
        let base = cache_base().join(format!("lumen-mirror-rd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("tsconfig.json"), "{}").unwrap();
        write_mirror(&root, &base.join("m"), &[]).unwrap();
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(base.join("m/tsconfig.json")).unwrap()).unwrap();
        let rd = cfg["compilerOptions"]["rootDir"].as_str().unwrap();
        assert!(base.join("m").starts_with(rd) && root.starts_with(rd), "rootDir {rd} must contain mirror and project");
        std::fs::remove_dir_all(&base).ok();
    }
}
