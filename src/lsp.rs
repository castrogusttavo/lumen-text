//! Minimal LSP client: JSON-RPC over stdio, one reader thread and one writer thread.
//! The server is spawned lazily and with a Go memory limit (tsgo is written in Go).

use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
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
        loop {
            let Some(hdr_end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") else { break };
            let header = String::from_utf8_lossy(&self.buf[..hdr_end]).to_ascii_lowercase();
            let len = header
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok());
            let Some(len) = len else {
                // Malformed header: drop it and resync after the blank line.
                self.buf.drain(..hdr_end + 4);
                continue;
            };
            let total = hdr_end + 4 + len;
            if self.buf.len() < total {
                break;
            }
            if let Ok(v) = serde_json::from_slice::<Value>(&self.buf[hdr_end + 4..total]) {
                out.push(v);
            }
            self.buf.drain(..total);
        }
        out
    }
}

pub fn frame(v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

// ───────────────────────────── messages ─────────────────────────────

pub enum Msg {
    Response { id: i64, result: Result<Value, Value> },
    Notification { method: String, params: Value },
    Request { id: Value, method: String, params: Value },
    Exited,
}

/// What a pending request was for, so the response can be routed.
#[derive(Clone, Debug)]
pub enum Pending {
    Initialize,
    Diagnostic { uri: String },
    Hover { uri: String, at: usize },
    HoverExtra { uri: String, at: usize, serial: u64, what: crate::app_hover::HoverWhat },
    Completion { uri: String, at: usize },
    Definition { kind: DefKind },
    References,
    Rename,
    Format { uri: String, version: i32 },
    CodeActions { uri: String },
    Signature { uri: String },
    Symbols { uri: String },
    ResolveCompletion { uri: String, version: i32 },
    ResolveAction,
    SelectionRange,
    SymbolsCache { uri: String },
    Folding { uri: String },
    SemTokens { uri: String },
    CodeLens { uri: String },
    InlayHint { uri: String, version: i32, first: usize, last: usize },
    ResolveLens { uri: String, line: usize },
    PrepareCalls { incoming: bool },
    Calls { incoming: bool },
    Organize,
    WorkspaceSymbols { query: String },
    ExecuteCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefKind {
    Definition,
    TypeDefinition,
    Implementation,
}

pub struct Lsp {
    pub root: PathBuf,
    child: Child,
    tx: Sender<Vec<u8>>,
    pub rx: Receiver<Msg>,
    next_id: i64,
    pub pending: HashMap<i64, Pending>,
    pub ready: bool,
    queue: Vec<Value>,
    pub caps: Value,
    pub name: String,
    /// Narrow mirror this server runs in (None: the real project directory).
    pub mirror: Option<PathBuf>,
    pub includes: Vec<PathBuf>,
}

impl Lsp {
    /// Spawn `bin args...` as a language server rooted at `root`.
    pub fn spawn(name: &str, bin: &Path, args: &[&str], root: &Path, work_dir: &Path, mem_limit_mib: u32, wake: Wake) -> std::io::Result<Lsp> {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .current_dir(work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Go runtime: make the GC work harder before growing the heap.
            .env("GOMEMLIMIT", format!("{mem_limit_mib}MiB"))
            .env("GOGC", "50");
        let mut child = cmd.spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        let (tx, wrx) = channel::<Vec<u8>>();
        thread::spawn(move || {
            while let Ok(bytes) = wrx.recv() {
                if stdin.write_all(&bytes).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
        });

        let (mtx, rx) = channel::<Msg>();
        thread::spawn(move || {
            let mut fr = FrameReader::default();
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => {
                        let _ = mtx.send(Msg::Exited);
                        wake();
                        break;
                    }
                    Ok(n) => {
                        for v in fr.push(&chunk[..n]) {
                            if let Some(m) = classify(v) {
                                if mtx.send(m).is_err() {
                                    return;
                                }
                            }
                        }
                        wake();
                    }
                }
            }
        });

        let mut lsp = Lsp {
            root: root.to_path_buf(),
            child,
            tx,
            rx,
            next_id: 1,
            pending: HashMap::new(),
            ready: false,
            queue: Vec::new(),
            caps: Value::Null,
            name: name.to_string(),
            mirror: None,
            includes: Vec::new(),
        };
        let params = initialize_params(work_dir);
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
        self.send_or_queue(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        id
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send_or_queue(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    pub fn respond(&self, id: Value, result: Value) {
        self.write(&json!({"jsonrpc":"2.0","id":id,"result":result}));
    }

    /// Must be called with the `initialize` response.
    pub fn on_initialized(&mut self, result: &Value) {
        self.caps = result["capabilities"].clone();
        self.ready = true;
        self.write(&json!({"jsonrpc":"2.0","method":"initialized","params":{}}));
        for v in std::mem::take(&mut self.queue) {
            self.write(&v);
        }
    }

    pub fn shutdown(&mut self) {
        if self.ready {
            self.write(&json!({"jsonrpc":"2.0","id":self.next_id,"method":"shutdown","params":null}));
            self.write(&json!({"jsonrpc":"2.0","method":"exit"}));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn classify(v: Value) -> Option<Msg> {
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    match (method, v.get("id")) {
        (Some(method), Some(id)) => Some(Msg::Request { id: id.clone(), method, params: v.get("params").cloned().unwrap_or(Value::Null) }),
        (Some(method), None) => Some(Msg::Notification { method, params: v.get("params").cloned().unwrap_or(Value::Null) }),
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

fn initialize_params(root: &Path) -> Value {
    let root_uri = crate::doc::path_to_uri(root);
    json!({
        "processId": std::process::id(),
        "clientInfo": {"name": "lumen-text", "version": env!("CARGO_PKG_VERSION")},
        "rootUri": root_uri,
        "workspaceFolders": [{"uri": root_uri, "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}],
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
    })
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
    if let Some(home) = std::env::var_os("HOME") {
        let p = Path::new(&home).join(".local/share/lumen-text/tsgo/node_modules/@typescript/native-preview-linux-x64/lib/tsgo");
        if p.exists() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("tsgo")).find(|p| p.is_file())
}

/// The tsgo a project installed itself (`node_modules/@typescript/native-preview-*`).
fn project_tsgo(root: &Path) -> Option<PathBuf> {
    root.ancestors().find_map(|d| {
        std::fs::read_dir(d.join("node_modules/@typescript")).ok()?.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("native-preview-")).map(|e| e.path().join("lib/tsgo")).find(|p| p.exists())
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
    let h = root.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    let base = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache"))).unwrap_or_else(std::env::temp_dir);
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
            std::os::unix::fs::symlink(&target, &link)?;
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
    let mut rels: Vec<String> = includes.iter().filter_map(|p| p.strip_prefix(root).ok().map(|r| r.to_string_lossy().into_owned())).collect();
    rels.sort();
    rels.dedup();
    let mut cfg = json!({"include": rels, "compilerOptions": {"incremental": false, "noEmit": true}});
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
    fn frames_split_across_chunks_and_back_to_back() {
        let a = frame(&json!({"id": 1, "result": "ç"}));
        let b = frame(&json!({"method": "x"}));
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut fr = FrameReader::default();
        let mut got = Vec::new();
        for chunk in all.chunks(7) {
            got.extend(fr.push(chunk));
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["result"], "ç", "Content-Length counts bytes, not chars");
        assert_eq!(got[1]["method"], "x");
    }

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
        std::fs::write(root.join("tsconfig.json"), r#"{"compilerOptions":{"strict":true,"noEmit":true},"include":["*.ts"]}"#).unwrap();
        let file = root.join("bad.ts");
        let src = "const x: number = \"a\"\nexport const y = nope\n";
        std::fs::write(&file, src).unwrap();
        let uri = crate::doc::path_to_uri(&file);

        let mut lsp = Lsp::spawn("tsgo", &bin, &["--lsp", "--stdio"], &root, &root, 512, Arc::new(|| {})).unwrap();
        lsp.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": "typescript", "version": 1, "text": src}}));
        lsp.request("textDocument/diagnostic", json!({"textDocument": {"uri": uri}}), Pending::Diagnostic { uri: uri.clone() });

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
        let msgs: Vec<String> = items.as_array().unwrap().iter().map(|d| d["message"].as_str().unwrap_or("").to_string()).collect();
        assert!(msgs.iter().any(|m| m.contains("not assignable")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("Cannot find name")), "{msgs:?}");
    }
}
