//! Lightweight remote development over SSH, with nothing installed on the remote side.
//!
//! Everything goes through the system `ssh` binary (OpenSSH `ControlMaster` multiplexing on Unix,
//! so every call after the first reuses one authenticated connection) running small POSIX `sh`
//! scripts: `ls`, `stat`, `cat`, `mv`, and `rg` / `grep` for search.
//!
//! The editor itself stays local-path based. A remote folder is *mirrored* under the cache
//! directory: `<cache>/lumen-text/remote/<user@host>/root/<remote path>`. Listing a folder creates
//! placeholder entries (empty files, folders) in the mirror, opening a file downloads it, saving a
//! file uploads it atomically (temp file + `mv`, refused when the remote copy changed since it was
//! fetched). Because of that the tree, tabs, session restore, highlighting... all keep using the
//! plain `std::fs` code paths; the only hooks are a few one-line calls guarded by one atomic flag
//! ([`ACTIVE`]), so a purely local session pays nothing.
//!
//! Not available on a remote folder (and said so in the status bar): language servers, git, replace
//! in files, file operations of the explorer (they act on the mirror only).
//!
//! [`Runner`] is the seam for tests: [`SshRunner`] runs the real `ssh`, the tests run the very
//! same scripts through a local `sh` against a temp directory.

use crate::search::{Hit, Query};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// A remote file larger than this is not downloaded for editing.
pub const MAX_REMOTE_FILE: u64 = 20 * 1024 * 1024;
const T_PROBE: Duration = Duration::from_secs(15);
const T_LIST: Duration = Duration::from_secs(30);
const T_FILE: Duration = Duration::from_secs(90);
const T_SEARCH: Duration = Duration::from_secs(90);
/// Exit status of `ssh` itself when the connection fails (the remote command never ran).
const SSH_FAILED: i32 = 255;
/// Exit status of the write script: the remote file is not the one that was fetched.
const EXIT_CONFLICT: i32 = 75;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ───────────── addresses ─────────────

/// Quote `s` as one word for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A remote path as a shell word; `~` and `~/x` expand to the remote home.
fn rq(path: &str) -> String {
    if path == "~" {
        "\"$HOME\"".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sh_quote(rest))
    } else {
        sh_quote(path)
    }
}

/// Where to connect: `[user@]host[:port]`, and optionally the folder to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub path: Option<String>,
}

fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '%'))
}

fn valid_user(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

impl Target {
    /// Accepts `ssh://user@host:port/path`, `user@host:/path` (also `host:~/dir`), `user@host` and a
    /// bare alias from `~/.ssh/config`. Anything that could be read as an `ssh` option is refused.
    pub fn parse(input: &str) -> Result<Target, String> {
        let s = input.trim();
        if s.is_empty() {
            return Err("empty address".to_string());
        }
        if s.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("the address cannot contain spaces".to_string());
        }
        let (auth, path): (&str, Option<String>) = if let Some(rest) = s.strip_prefix("ssh://") {
            match rest.find('/') {
                Some(i) => (&rest[..i], Some(rest[i..].to_string())),
                None => (rest, None),
            }
        } else if let Some((a, p)) = s.split_once(':') {
            (a, if p.is_empty() { None } else { Some(p.to_string()) })
        } else {
            (s, None)
        };
        let path = path.map(|p| match p.strip_prefix("/~") {
            Some(h) if h.is_empty() || h.starts_with('/') => format!("~{h}"),
            _ => p,
        });
        let (user, hostport) = match auth.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, auth),
        };
        let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
            let (h, rest) = inner.split_once(']').ok_or("unclosed [ in the host")?;
            let port = match rest.strip_prefix(':') {
                Some(p) => Some(p.parse::<u16>().map_err(|_| format!("bad port: {p}"))?),
                None if rest.is_empty() => None,
                None => return Err("bad address".to_string()),
            };
            (h.to_string(), port)
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p.parse::<u16>().map_err(|_| format!("bad port: {p}"))?)),
                None => (hostport.to_string(), None),
            }
        };
        if !valid_host(&host) {
            return Err(format!("not a valid host: {host}"));
        }
        if let Some(u) = &user
            && !valid_user(u)
        {
            return Err(format!("not a valid user: {u}"));
        }
        Ok(Target { user, host, port, path })
    }

    /// `user@host` (or `host`): what `ssh` takes as destination.
    pub fn dest(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// Short name for the status bar and titles.
    pub fn label(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.dest()),
            None => self.dest(),
        }
    }

    /// Directory name of this host in the mirror.
    pub fn key(&self) -> String {
        self.label()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '@') { c } else { '_' })
            .collect()
    }

    /// `ssh://user@host:port`, what the mirror remembers to reconnect.
    pub fn spec(&self) -> String {
        format!("ssh://{}", self.label())
    }
}

/// The concrete `Host` names of an ssh config (wildcards and negations left out).
pub fn config_hosts(text: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let mut it = line.splitn(2, |c: char| c.is_whitespace() || c == '=');
        let (Some(key), Some(rest)) = (it.next(), it.next()) else { continue };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for name in rest.trim_start_matches(['=', ' ', '\t']).split_whitespace() {
            if !name.contains(['*', '?', '!']) && valid_host(name) && !v.iter().any(|x| x == name) {
                v.push(name.to_string());
            }
        }
    }
    v
}

/// Hosts of `~/.ssh/config` (empty when there is none).
pub fn ssh_config_hosts() -> Vec<String> {
    let Some(home) = crate::paths::home() else { return Vec::new() };
    std::fs::read_to_string(home.join(".ssh").join("config")).map(|t| config_hosts(&t)).unwrap_or_default()
}

// ───────────── running ssh ─────────────

/// What a remote command printed and how it ended.
#[derive(Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Runs one `sh` script on the remote host. The only thing tests replace.
pub trait Runner: Send + Sync {
    fn run(&self, t: &Target, script: &str, stdin: Option<&[u8]>, timeout: Duration) -> io::Result<Output>;
    /// Drop the shared connection (the next call connects again).
    fn reset(&self, _t: &Target) {}
}

/// Directory of the multiplexing sockets (short: socket paths are limited to ~100 bytes).
#[cfg(unix)]
fn mux_dir() -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let d = PathBuf::from("/tmp").join(format!("lumen-ssh-{uid}"));
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d).ok()?;
    Some(d)
}

#[cfg(not(unix))]
fn mux_dir() -> Option<PathBuf> {
    None
}

/// `ssh` options shared by scripts and terminals. `batch`: never ask anything (scripts have no tty).
pub fn ssh_args(t: &Target, batch: bool) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut o = |s: String| {
        a.push("-o".to_string());
        a.push(s);
    };
    if batch {
        o("BatchMode=yes".to_string());
        o("ConnectTimeout=10".to_string());
    }
    o("ServerAliveInterval=15".to_string());
    o("ServerAliveCountMax=3".to_string());
    if let Some(d) = mux_dir() {
        o("ControlMaster=auto".to_string());
        o(format!("ControlPath={}/%C", d.display()));
        o("ControlPersist=10m".to_string());
    }
    if let Some(p) = t.port {
        a.push("-p".to_string());
        a.push(p.to_string());
    }
    a
}

/// Spawn `cmd`, feed `stdin`, collect its output, kill it after `timeout`.
fn exec(mut cmd: Command, stdin: Option<&[u8]>, timeout: Duration) -> io::Result<Output> {
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
    let mut err = child.stderr.take().ok_or_else(|| io::Error::other("no stderr"))?;
    let t_out = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out.read_to_end(&mut v);
        v
    });
    let t_err = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err.read_to_end(&mut v);
        v
    });
    let t_in = stdin.map(|data| {
        let data = data.to_vec();
        let w = child.stdin.take();
        thread::spawn(move || {
            if let Some(mut w) = w {
                let _ = w.write_all(&data);
            }
        })
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the remote command timed out"));
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    };
    if let Some(h) = t_in {
        let _ = h.join();
    }
    let stdout = t_out.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
    Ok(Output { code: status.code().unwrap_or(SSH_FAILED), stdout, stderr })
}

/// The real thing: the system `ssh`.
pub struct SshRunner {
    pub program: String,
}

impl Default for SshRunner {
    fn default() -> Self {
        SshRunner { program: std::env::var("LUMEN_SSH").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "ssh".into()) }
    }
}

impl Runner for SshRunner {
    fn run(&self, t: &Target, script: &str, stdin: Option<&[u8]>, timeout: Duration) -> io::Result<Output> {
        let mut cmd = Command::new(&self.program);
        cmd.args(ssh_args(t, true)).arg("--").arg(t.dest()).arg(format!("sh -c {}", sh_quote(script)));
        exec(cmd, stdin, timeout)
    }

    fn reset(&self, t: &Target) {
        let mut cmd = Command::new(&self.program);
        cmd.args(ssh_args(t, true)).arg("-O").arg("exit").arg("--").arg(t.dest());
        let _ = exec(cmd, None, Duration::from_secs(5));
    }
}

// ───────────── scripts ─────────────

/// `st FILE`: "mtime size" of a file, on GNU and BSD `stat`.
const ST_FN: &str = "st() { stat -c '%Y %s' -- \"$1\" 2>/dev/null || stat -f '%m %z' -- \"$1\" 2>/dev/null; }";

fn script_list(dir: &str) -> String {
    format!("cd -- {} 2>/dev/null || exit 2; ls -1ApL 2>/dev/null; exit 0", rq(dir))
}

fn script_stat(file: &str) -> String {
    format!("{ST_FN}; st {}", rq(file))
}

fn script_read(file: &str) -> String {
    format!(
        "{ST_FN}; f={}; [ -f \"$f\" ] || exit 3; s=$(st \"$f\"); echo \"$s\"; \
         [ \"${{s#* }}\" -le {MAX_REMOTE_FILE} ] || exit 4; cat -- \"$f\"",
        rq(file)
    )
}

/// Upload from stdin: temp file next to the target, the mtime/size check, permissions, `mv`.
fn script_write(file: &str, expect: &str) -> String {
    format!(
        "{ST_FN}; f={}; r=$(readlink -f -- \"$f\" 2>/dev/null) && [ -n \"$r\" ] && f=$r; \
         t=\"$f.lumen-tmp.$$\"; trap 'rm -f \"$t\"' EXIT; cat > \"$t\" || exit 1; \
         if [ -n {e} ]; then cur=$(st \"$f\"); [ \"$cur\" = {e} ] || exit {EXIT_CONFLICT}; fi; \
         if [ -e \"$f\" ]; then m=$(stat -c %a -- \"$f\" 2>/dev/null || stat -f %Lp -- \"$f\" 2>/dev/null); \
         [ -n \"$m\" ] && chmod \"$m\" \"$t\"; fi; mv -f -- \"$t\" \"$f\" || exit 1; st \"$f\"",
        rq(file),
        e = sh_quote(expect),
    )
}

fn script_search(root: &str, q: &Query, cap: usize) -> String {
    let (rg_case, grep_case) = if q.case { ("-s", "") } else { ("-i", "-i") };
    let rg_fixed = if q.regex { "" } else { "-F" };
    let grep_mode = if q.regex { "-E" } else { "-F" };
    let word = if q.word { "-w" } else { "" };
    let text = sh_quote(&q.text);
    format!(
        "cd -- {root} 2>/dev/null || exit 2; \
         if command -v rg >/dev/null 2>&1; then echo '#rg'; \
         rg --no-heading --line-number --color never --max-columns 400 --max-filesize 1M {rg_case} {rg_fixed} {word} -e {text} -- . 2>/dev/null; \
         else echo '#grep'; \
         grep -rnIH {grep_case} {grep_mode} {word} --exclude-dir=.git --exclude-dir=node_modules --exclude-dir=target -e {text} -- . 2>/dev/null; \
         fi | head -n {cap}; exit 0",
        root = rq(root),
    )
}

/// `path:line:text` lines of `rg` / `grep` to hits (paths under `base`, the folder the search ran in).
fn parse_search(out: &str, q: &Query, local_of: impl Fn(&str) -> PathBuf) -> Vec<Hit> {
    let Ok(m) = crate::replace::Matcher::new(&q.text, q.case, q.regex, q.word) else { return Vec::new() };
    let mut hits = Vec::new();
    for line in out.lines() {
        if line.starts_with('#') && hits.is_empty() && (line == "#rg" || line == "#grep") {
            continue;
        }
        let Some((path, rest)) = line.split_once(':') else { continue };
        let Some((ln, text)) = rest.split_once(':') else { continue };
        let Ok(ln) = ln.parse::<usize>() else { continue };
        let Some((start, end)) = m.first(text) else { continue };
        let (shown, mark) = crate::search::snippet(text, start, end);
        hits.push(Hit {
            path: local_of(path.strip_prefix("./").unwrap_or(path)),
            line: ln.saturating_sub(1),
            col: text[..start].chars().count(),
            text: shown,
            start,
            end,
            mark,
            rep: None,
        });
    }
    hits
}

// ───────────── session ─────────────

/// State of the link, as the status bar shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Online,
    Offline,
    Connecting,
}

impl Link {
    fn from_u8(n: u8) -> Link {
        match n {
            0 => Link::Online,
            1 => Link::Offline,
            _ => Link::Connecting,
        }
    }
}

/// Something the UI thread should react to (drained by [`take_events`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// The connection works: open this mirror folder as the project.
    Connected(PathBuf),
    Failed(String),
    /// A folder was listed: rebuild the tree.
    Listed,
    /// The link changed state: a message for the status line.
    Link(String),
}

#[derive(Debug)]
pub enum PushErr {
    /// The remote file changed since it was downloaded.
    Conflict,
    Offline(String),
    Failed(String),
}

impl PushErr {
    pub fn message(&self) -> String {
        match self {
            PushErr::Conflict => {
                "the remote file changed since it was opened: saved here only, not uploaded (Remote: Save and overwrite)"
                    .to_string()
            }
            PushErr::Offline(e) => format!("saved here only, the connection is down: {e}"),
            PushErr::Failed(e) => format!("could not upload: {e}"),
        }
    }
}

/// One remote host and its local mirror.
pub struct Session {
    pub target: Target,
    /// Local folder that stands for the remote `/`.
    pub mirror: PathBuf,
    runner: Arc<dyn Runner>,
    link: AtomicU8,
    /// Remote folders already listed into the mirror.
    listed: Mutex<HashSet<String>>,
    inflight: Mutex<HashSet<String>>,
    /// "mtime size" of each downloaded file as it was on the remote.
    stamps: Mutex<HashMap<String, String>>,
    probing: AtomicBool,
    last_probe: Mutex<Option<Instant>>,
}

impl Session {
    /// A session whose mirror lives under `base/<host>/root`.
    pub fn new(target: Target, base: &Path, runner: Arc<dyn Runner>) -> Session {
        let host_dir = base.join(target.key());
        let _ = std::fs::create_dir_all(host_dir.join("root"));
        let host_dir = crate::paths::canonical(&host_dir).unwrap_or(host_dir);
        Session {
            mirror: host_dir.join("root"),
            target,
            runner,
            link: AtomicU8::new(2),
            listed: Mutex::new(HashSet::new()),
            inflight: Mutex::new(HashSet::new()),
            stamps: Mutex::new(HashMap::new()),
            probing: AtomicBool::new(false),
            last_probe: Mutex::new(None),
        }
    }

    pub fn link(&self) -> Link {
        Link::from_u8(self.link.load(Ordering::Relaxed))
    }

    fn set_link(&self, l: Link) {
        let n = match l {
            Link::Online => 0,
            Link::Offline => 1,
            Link::Connecting => 2,
        };
        let prev = self.link.swap(n, Ordering::Relaxed);
        if prev != n && l != Link::Connecting {
            let msg = match l {
                Link::Online => format!("ssh {}: connected", self.target.label()),
                _ => format!("ssh {}: connection lost, retrying (Remote: Reconnect)", self.target.label()),
            };
            push_event(Event::Link(msg));
        }
    }

    /// Run a script; a failed connection flips the link to offline, any answer flips it online.
    fn run(&self, script: &str, stdin: Option<&[u8]>, timeout: Duration) -> Result<Output, String> {
        match self.runner.run(&self.target, script, stdin, timeout) {
            Ok(o) if o.code == SSH_FAILED => {
                self.set_link(Link::Offline);
                let why = o.stderr.lines().next().unwrap_or("connection failed").trim().to_string();
                Err(why)
            }
            Ok(o) => {
                self.set_link(Link::Online);
                Ok(o)
            }
            Err(e) => {
                self.set_link(Link::Offline);
                Err(e.to_string())
            }
        }
    }

    // ── paths ──

    /// The remote path a mirror path stands for (`/a/b`), `None` outside this mirror.
    pub fn remote_path(&self, local: &Path) -> Option<String> {
        let rel = local.strip_prefix(&self.mirror).ok()?;
        let mut s = String::new();
        for c in rel.components() {
            s.push('/');
            s.push_str(&c.as_os_str().to_string_lossy());
        }
        if s.is_empty() {
            s.push('/');
        }
        Some(s)
    }

    /// The mirror path of a remote one.
    pub fn local_path(&self, remote: &str) -> PathBuf {
        let mut p = self.mirror.clone();
        for part in remote.split('/').filter(|x| !x.is_empty()) {
            p.push(part);
        }
        p
    }

    // ── connection ──

    /// Check the link (one cheap round trip).
    pub fn probe(&self) -> Result<(), String> {
        *lock(&self.last_probe) = Some(Instant::now());
        self.run("echo lumen-ok", None, T_PROBE)
            .and_then(|o| if o.code == 0 { Ok(()) } else { Err(format!("the remote shell answered {}", o.code)) })
    }

    /// Connect and resolve the folder to open into an absolute remote path.
    pub fn resolve_root(&self, path: Option<&str>) -> Result<String, String> {
        let cd = match path {
            Some(p) if !p.is_empty() => format!("cd -- {}", rq(p)),
            _ => "cd".to_string(),
        };
        let o = self.run(&format!("{cd} && pwd -P"), None, T_PROBE)?;
        let root = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.code != 0 || !root.starts_with('/') {
            let why = o.stderr.trim();
            return Err(if why.is_empty() { "no such folder on the remote host".to_string() } else { why.to_string() });
        }
        Ok(root)
    }

    /// Resolve `path`, remember the host in the mirror and list the root. Returns the mirror folder
    /// to open as the project.
    pub fn connect(&self, path: Option<&str>) -> Result<PathBuf, String> {
        self.set_link(Link::Connecting);
        let root = self.resolve_root(path)?;
        self.sync_dir(&root)?;
        // `<base>/<host>/target` tells a later run which host a mirror folder belongs to.
        if let Some(host_dir) = self.mirror.parent() {
            let _ = std::fs::write(host_dir.join("target"), self.target.spec());
        }
        Ok(self.local_path(&root))
    }

    /// Drop the shared connection and probe again.
    pub fn reconnect(&self) -> Result<(), String> {
        self.set_link(Link::Connecting);
        self.runner.reset(&self.target);
        self.probe()
    }

    /// Time for the periodic probe of an offline link? Starts it on a thread when so.
    pub fn maybe_probe(self: &Arc<Self>) {
        if self.link() != Link::Offline {
            return;
        }
        let due = lock(&self.last_probe).is_none_or(|t| t.elapsed() > Duration::from_secs(10));
        if !due || self.probing.swap(true, Ordering::SeqCst) {
            return;
        }
        let s = self.clone();
        thread::spawn(move || {
            let _ = s.probe();
            s.probing.store(false, Ordering::SeqCst);
            wake();
        });
    }

    // ── folders ──

    /// Entries of a remote folder: (name, is_dir).
    pub fn list(&self, dir: &str) -> Result<Vec<(String, bool)>, String> {
        let o = self.run(&script_list(dir), None, T_LIST)?;
        if o.code == 2 {
            return Err(format!("cannot read {dir}"));
        }
        let text = String::from_utf8_lossy(&o.stdout);
        Ok(text
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| match l.strip_suffix('/') {
                Some(n) => (n.to_string(), true),
                None => (l.to_string(), false),
            })
            .collect())
    }

    fn is_hydrated(&self, local: &Path) -> bool {
        self.remote_path(local).is_some_and(|r| lock(&self.stamps).contains_key(&r))
    }

    /// List `dir` and make the mirror folder match: placeholders for new entries, stale ones gone
    /// (a downloaded file is kept).
    pub fn sync_dir(&self, dir: &str) -> Result<(), String> {
        let entries = self.list(dir)?;
        let local = self.local_path(dir);
        std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
        for (name, is_dir) in &entries {
            let p = local.join(name);
            if *is_dir {
                if p.is_file() {
                    let _ = std::fs::remove_file(&p);
                }
                let _ = std::fs::create_dir_all(&p);
            } else {
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                }
                if !p.exists() {
                    let _ = std::fs::File::create(&p);
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir(&local) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if entries.iter().any(|(n, _)| *n == name) {
                    continue;
                }
                let p = e.path();
                if p.is_dir() {
                    let _ = std::fs::remove_dir(&p);
                } else if !self.is_hydrated(&p) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        lock(&self.listed).insert(dir.to_string());
        Ok(())
    }

    /// Make sure the mirror folder `local` has been listed: starts a background listing when not.
    pub fn ensure_listed(self: &Arc<Self>, local: &Path) {
        let Some(dir) = self.remote_path(local) else { return };
        if lock(&self.listed).contains(&dir) || !lock(&self.inflight).insert(dir.clone()) {
            return;
        }
        let s = self.clone();
        thread::spawn(move || {
            let r = s.sync_dir(&dir);
            lock(&s.inflight).remove(&dir);
            if r.is_ok() {
                push_event(Event::Listed);
            }
            wake();
        });
    }

    /// Forget what was listed: folders are listed again the next time the tree draws them.
    pub fn invalidate(&self) {
        lock(&self.listed).clear();
    }

    // ── files ──

    /// "mtime size" of a remote file, `None` when it does not exist.
    pub fn stat(&self, file: &str) -> Result<Option<String>, String> {
        let o = self.run(&script_stat(file), None, T_PROBE)?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        Ok(if o.code == 0 && !s.is_empty() { Some(s) } else { None })
    }

    /// Download a file into the mirror and remember its stamp.
    pub fn fetch(&self, file: &str) -> Result<(), String> {
        let o = self.run(&script_read(file), None, T_FILE)?;
        match o.code {
            0 => {}
            3 => return Err("no such file on the remote host".to_string()),
            4 => return Err(format!("larger than {} MB, not downloaded", MAX_REMOTE_FILE / (1024 * 1024))),
            c => return Err(format!("the remote read failed ({c}) {}", o.stderr.trim())),
        }
        let nl = o.stdout.iter().position(|b| *b == b'\n').ok_or("bad answer from the remote host")?;
        let stamp = String::from_utf8_lossy(&o.stdout[..nl]).trim().to_string();
        let local = self.local_path(file);
        if let Some(parent) = local.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&local, &o.stdout[nl + 1..]).map_err(|e| e.to_string())?;
        lock(&self.stamps).insert(file.to_string(), stamp);
        Ok(())
    }

    /// Have the file in the mirror current: download when never fetched or changed on the remote.
    pub fn hydrate(&self, local: &Path) -> Result<(), String> {
        let Some(file) = self.remote_path(local) else { return Ok(()) };
        if local.is_dir() {
            return Ok(());
        }
        let known = lock(&self.stamps).get(&file).cloned();
        if let Some(k) = &known
            && self.stat(&file)?.as_deref() == Some(k.as_str())
        {
            return Ok(());
        }
        self.fetch(&file)
    }

    /// Upload the mirror file `local` atomically. Unless `force`, refused when the remote copy is
    /// not the one that was downloaded.
    pub fn push(&self, local: &Path, force: bool) -> Result<(), PushErr> {
        let file = self.remote_path(local).ok_or_else(|| PushErr::Failed("not a remote file".into()))?;
        let data = std::fs::read(local).map_err(|e| PushErr::Failed(e.to_string()))?;
        let expect = if force { String::new() } else { lock(&self.stamps).get(&file).cloned().unwrap_or_default() };
        let o = self.run(&script_write(&file, &expect), Some(&data), T_FILE).map_err(PushErr::Offline)?;
        match o.code {
            0 => {
                let stamp = String::from_utf8_lossy(&o.stdout).trim().to_string();
                lock(&self.stamps).insert(file, stamp);
                Ok(())
            }
            EXIT_CONFLICT => Err(PushErr::Conflict),
            c => Err(PushErr::Failed(format!("exit {c}: {}", o.stderr.trim()))),
        }
    }

    // ── search and terminal ──

    /// Find-in-files on the remote host (`rg` when installed there, else `grep`).
    pub fn search(&self, root_local: &Path, q: &Query, cap: usize) -> Result<Vec<Hit>, String> {
        let root = self.remote_path(root_local).ok_or("not a remote folder")?;
        let o = self.run(&script_search(&root, q, cap), None, T_SEARCH)?;
        if o.code == 2 {
            return Err(format!("cannot read {root}"));
        }
        let out = String::from_utf8_lossy(&o.stdout);
        Ok(parse_search(&out, q, |rel| root_local.join(rel)))
    }

    /// Arguments of the interactive `ssh -t` that opens a shell in the mirror folder `local`.
    pub fn terminal_args(&self, local: &Path) -> Vec<String> {
        let dir = self.remote_path(local).unwrap_or_else(|| "/".to_string());
        let inner = format!("cd -- {} 2>/dev/null; exec \"${{SHELL:-/bin/sh}}\" -l", rq(&dir));
        let mut a = vec!["-t".to_string()];
        a.extend(ssh_args(&self.target, false));
        a.push("--".to_string());
        a.push(self.target.dest());
        a.push(format!("sh -c {}", sh_quote(&inner)));
        a
    }
}

// ───────────── process-wide registry (the hooks of the app) ─────────────

/// Set once a remote session exists; every hook returns at once while it is false.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ADOPTED: AtomicBool = AtomicBool::new(false);
static FORCE: AtomicBool = AtomicBool::new(false);
static SESSIONS: Mutex<Vec<Arc<Session>>> = Mutex::new(Vec::new());
static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());
static WAKE: Mutex<Option<crate::lsp::Wake>> = Mutex::new(None);

fn push_event(e: Event) {
    lock(&EVENTS).push(e);
}

fn wake() {
    let w = lock(&WAKE).clone();
    if let Some(w) = w {
        w();
    }
}

/// Show `msg` in the status line from a background thread.
pub fn report(msg: String) {
    push_event(Event::Link(msg));
}

/// Let background work wake the UI.
pub fn set_wake(w: crate::lsp::Wake) {
    *lock(&WAKE) = Some(w);
}

/// Where mirrors live.
pub fn mirror_base() -> Option<PathBuf> {
    crate::paths::cache_home().map(|h| h.join("lumen-text").join("remote"))
}

pub fn register(s: Arc<Session>) {
    let mut v = lock(&SESSIONS);
    v.retain(|x| x.mirror != s.mirror);
    v.push(s);
    ACTIVE.store(true, Ordering::SeqCst);
}

/// The session whose mirror holds `path`.
pub fn session_for(path: &Path) -> Option<Arc<Session>> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    lock(&SESSIONS).iter().find(|s| path.starts_with(&s.mirror)).cloned()
}

pub fn is_remote(path: &Path) -> bool {
    session_for(path).is_some()
}

/// When `path` is inside a mirror of an earlier run (reopened from the recent projects), register
/// a session for it, read from the `target` file the mirror keeps.
pub fn adopt(path: &Path) {
    let Some(base) = mirror_base() else { return };
    let base = crate::paths::canonical(&base).unwrap_or(base);
    let Ok(rel) = path.strip_prefix(&base) else { return };
    let Some(host) = rel.components().next() else { return };
    let host_dir = base.join(host.as_os_str());
    if session_for(&host_dir.join("root")).is_some() {
        return;
    }
    let Ok(spec) = std::fs::read_to_string(host_dir.join("target")) else { return };
    let Ok(t) = Target::parse(spec.trim()) else { return };
    register(Arc::new(Session::new(t, &base, Arc::new(SshRunner::default()))));
}

/// Once per run, from the UI loop: adopt the project folder when it is a mirror.
pub fn adopt_once(root: &Path) {
    if !ADOPTED.swap(true, Ordering::SeqCst) {
        adopt(root);
    }
}

/// Drain what background threads reported.
pub fn take_events() -> Vec<Event> {
    std::mem::take(&mut *lock(&EVENTS))
}

/// Start connecting to `t` on a thread; the result arrives as [`Event::Connected`] / [`Event::Failed`].
pub fn connect_async(t: Target) {
    let Some(base) = mirror_base() else {
        push_event(Event::Failed("no cache folder for the remote mirror".into()));
        return;
    };
    thread::spawn(move || {
        let path = t.path.clone();
        let s = Arc::new(Session::new(t, &base, Arc::new(SshRunner::default())));
        match s.connect(path.as_deref()) {
            Ok(root) => {
                register(s);
                push_event(Event::Connected(root));
            }
            Err(e) => push_event(Event::Failed(e)),
        }
        wake();
    });
}

/// Tree hook: a folder of a remote mirror is about to be listed.
pub fn before_list(dir: &Path) {
    if let Some(s) = session_for(dir) {
        s.ensure_listed(dir);
    }
}

/// Open hook: have the file current in the mirror (no-op for local files).
pub fn hydrate(path: &Path) -> Result<(), String> {
    match session_for(path) {
        Some(s) => s.hydrate(path),
        None => Ok(()),
    }
}

/// The next upload ignores the conflict check.
pub fn force_next_push() {
    FORCE.store(true, Ordering::SeqCst);
}

/// Save hook: upload a just-saved mirror file. `None` for local files.
pub fn push_saved(path: &Path) -> Option<Result<(), PushErr>> {
    let s = session_for(path)?;
    let force = FORCE.swap(false, Ordering::SeqCst);
    Some(s.push(path, force))
}

/// Terminal hook: the `ssh -t` arguments when `root` is remote.
pub fn terminal_args(root: &Path) -> Option<Vec<String>> {
    session_for(root).map(|s| s.terminal_args(root))
}

/// Reload the listings of the remote folder `root` belongs to (no-op for local ones).
pub fn invalidate(root: &Path) {
    if let Some(s) = session_for(root) {
        s.invalidate();
    }
}

/// Reconnect on a thread.
pub fn reconnect_async(root: &Path) -> bool {
    let Some(s) = session_for(root) else { return false };
    thread::spawn(move || {
        let _ = s.reconnect();
        wake();
    });
    true
}

/// Probe an offline link now and then (UI loop).
pub fn tick(root: &Path) {
    if let Some(s) = session_for(root) {
        s.maybe_probe();
    }
}

/// Status bar text and state for `root`.
pub fn badge(root: &Path) -> Option<(String, Link)> {
    let s = session_for(root)?;
    let l = s.link();
    let state = match l {
        Link::Online => "connected",
        Link::Offline => "offline",
        Link::Connecting => "connecting",
    };
    Some((format!("SSH {}: {state} (no LSP/git)", s.target.label()), l))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Runs the scripts through a local `sh`, so the tests exercise the real remote commands.
    struct LocalSh {
        calls: Mutex<Vec<String>>,
        down: AtomicBool,
    }

    impl LocalSh {
        fn new() -> Arc<LocalSh> {
            Arc::new(LocalSh { calls: Mutex::new(Vec::new()), down: AtomicBool::new(false) })
        }
    }

    impl Runner for Arc<LocalSh> {
        fn run(&self, _t: &Target, script: &str, stdin: Option<&[u8]>, timeout: Duration) -> io::Result<Output> {
            lock(&self.calls).push(script.to_string());
            if self.down.load(Ordering::SeqCst) {
                return Ok(Output { code: SSH_FAILED, stdout: Vec::new(), stderr: "ssh: connect: refused\n".into() });
            }
            let mut c = Command::new("sh");
            c.arg("-c").arg(script);
            exec(c, stdin, timeout)
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-remote-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        crate::paths::canonical(&d).unwrap()
    }

    /// A "remote" tree in a temp dir plus a session over it.
    fn setup(name: &str) -> (PathBuf, PathBuf, Session, Arc<LocalSh>) {
        let remote = tmp(&format!("{name}-r"));
        let base = tmp(&format!("{name}-m"));
        std::fs::create_dir_all(remote.join("src")).unwrap();
        std::fs::write(remote.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(remote.join("README.md"), "hello needle\n").unwrap();
        let sh = LocalSh::new();
        let t = Target::parse("me@box").unwrap();
        (remote, base.clone(), Session::new(t, &base, Arc::new(sh.clone())), sh)
    }

    #[test]
    fn parses_addresses() {
        let t = Target::parse("ssh://me@host:2222/srv/app").unwrap();
        assert_eq!(
            (t.user.as_deref(), t.host.as_str(), t.port, t.path.as_deref()),
            (Some("me"), "host", Some(2222), Some("/srv/app"))
        );
        let t = Target::parse("me@host:/etc").unwrap();
        assert_eq!((t.host.as_str(), t.path.as_deref()), ("host", Some("/etc")));
        let t = Target::parse("host:~/code").unwrap();
        assert_eq!(t.path.as_deref(), Some("~/code"));
        let t = Target::parse("ssh://host/~/x").unwrap();
        assert_eq!(t.path.as_deref(), Some("~/x"));
        let t = Target::parse("alias").unwrap();
        assert_eq!((t.host.as_str(), t.path), ("alias", None));
        let t = Target::parse("ssh://me@[::1]:22/x").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", Some(22)));
    }

    #[test]
    fn refuses_option_injection_and_junk() {
        assert!(Target::parse("-oProxyCommand=evil").is_err());
        assert!(Target::parse("ssh://-oFoo/x").is_err());
        assert!(Target::parse("me@-host").is_err());
        assert!(Target::parse("a b").is_err());
        assert!(Target::parse("").is_err());
        assert!(Target::parse("ssh://host:badport/x").is_err());
    }

    #[test]
    fn labels_and_keys() {
        let t = Target::parse("ssh://me@h:2200").unwrap();
        assert_eq!(
            (t.label().as_str(), t.dest().as_str(), t.spec().as_str()),
            ("me@h:2200", "me@h", "ssh://me@h:2200")
        );
        assert_eq!(t.key(), "me@h_2200");
    }

    #[test]
    fn config_hosts_skips_wildcards() {
        let cfg =
            "Host *\n  User x\nHost web db\n  HostName 1.2.3.4\nhost=dev\nHost *.corp !bad\n# Host nope\nHost web\n";
        assert_eq!(config_hosts(cfg), vec!["web", "db", "dev"]);
    }

    #[test]
    fn quoting_survives_quotes_and_home() {
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
        assert_eq!(rq("~"), "\"$HOME\"");
        assert_eq!(rq("~/my dir"), "\"$HOME\"/'my dir'");
        assert_eq!(rq("/x y"), "'/x y'");
    }

    #[test]
    fn ssh_args_multiplex_and_never_prompt() {
        let t = Target::parse("ssh://me@h:2200").unwrap();
        let a = ssh_args(&t, true).join(" ");
        assert!(a.contains("BatchMode=yes") && a.contains("ControlMaster=auto") && a.contains("ControlPersist"), "{a}");
        assert!(a.ends_with("-p 2200"), "{a}");
        assert!(!ssh_args(&t, false).join(" ").contains("BatchMode"));
    }

    #[test]
    fn terminal_command_enters_the_folder() {
        let (remote, _b, s, _sh) = setup("term");
        s.connect(Some(remote.to_str().unwrap())).unwrap();
        let dir = s.local_path(remote.to_str().unwrap()).join("src");
        let a = s.terminal_args(&dir);
        assert_eq!(a[0], "-t");
        assert!(a.contains(&"me@box".to_string()));
        let cmd = a.last().unwrap();
        assert!(cmd.starts_with("sh -c ") && cmd.contains("/src") && cmd.contains("exec"), "{cmd}");
    }

    #[test]
    fn connect_lists_the_root_into_the_mirror() {
        let (remote, _b, s, _sh) = setup("conn");
        let local = s.connect(Some(remote.to_str().unwrap())).unwrap();
        assert!(local.join("README.md").is_file() && local.join("src").is_dir());
        assert_eq!(std::fs::read(local.join("README.md")).unwrap(), b"", "placeholders are empty until opened");
        assert_eq!(s.link(), Link::Online);
        assert_eq!(s.remote_path(&local.join("src")).unwrap(), format!("{}/src", remote.display()));
        assert!(!s.mirror.parent().unwrap().join("target").to_string_lossy().is_empty());
        assert_eq!(std::fs::read_to_string(s.mirror.parent().unwrap().join("target")).unwrap(), "ssh://me@box");
    }

    #[test]
    fn open_downloads_and_save_uploads_atomically() {
        let (remote, _b, s, _sh) = setup("rw");
        let local_root = s.connect(Some(remote.to_str().unwrap())).unwrap();
        let f = local_root.join("src/main.rs");
        s.sync_dir(&format!("{}/src", remote.display())).unwrap();
        s.hydrate(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "fn main() {}\n");
        std::fs::write(&f, "fn main() { 1 }\n").unwrap();
        s.push(&f, false).unwrap();
        assert_eq!(std::fs::read_to_string(remote.join("src/main.rs")).unwrap(), "fn main() { 1 }\n");
        let left: Vec<_> = std::fs::read_dir(remote.join("src")).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no temp file left behind");
        // A second save works: the stamp followed the first upload.
        std::fs::write(&f, "fn main() { 2 }\n").unwrap();
        s.push(&f, false).unwrap();
        assert_eq!(std::fs::read_to_string(remote.join("src/main.rs")).unwrap(), "fn main() { 2 }\n");
    }

    #[test]
    fn save_keeps_the_remote_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (remote, _b, s, _sh) = setup("perm");
        let root = s.connect(Some(remote.to_str().unwrap())).unwrap();
        let run = remote.join("run.sh");
        std::fs::write(&run, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o750)).unwrap();
        s.sync_dir(remote.to_str().unwrap()).unwrap();
        let f = root.join("run.sh");
        s.hydrate(&f).unwrap();
        std::fs::write(&f, "#!/bin/sh\necho hi\n").unwrap();
        s.push(&f, false).unwrap();
        let mode = std::fs::metadata(&run).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[test]
    fn save_is_refused_when_the_remote_file_changed() {
        let (remote, _b, s, _sh) = setup("conf");
        let root = s.connect(Some(remote.to_str().unwrap())).unwrap();
        let f = root.join("README.md");
        s.hydrate(&f).unwrap();
        std::fs::write(remote.join("README.md"), "changed elsewhere, longer\n").unwrap();
        std::fs::write(&f, "mine\n").unwrap();
        assert!(matches!(s.push(&f, false), Err(PushErr::Conflict)));
        assert_eq!(std::fs::read_to_string(remote.join("README.md")).unwrap(), "changed elsewhere, longer\n");
        assert!(left_no_tmp(&remote));
        // Overwriting on purpose goes through.
        s.push(&f, true).unwrap();
        assert_eq!(std::fs::read_to_string(remote.join("README.md")).unwrap(), "mine\n");
    }

    fn left_no_tmp(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("lumen-tmp"))
    }

    #[test]
    fn hydrate_refetches_only_when_the_remote_changed() {
        let (remote, _b, s, sh) = setup("rehyd");
        let root = s.connect(Some(remote.to_str().unwrap())).unwrap();
        let f = root.join("README.md");
        s.hydrate(&f).unwrap();
        let reads = |sh: &Arc<LocalSh>| lock(&sh.calls).iter().filter(|c| c.contains("cat --")).count();
        let n = reads(&sh);
        s.hydrate(&f).unwrap();
        assert_eq!(reads(&sh), n, "unchanged: only a stat");
        std::fs::write(remote.join("README.md"), "new content here\n").unwrap();
        s.hydrate(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new content here\n");
    }

    #[test]
    fn missing_and_oversized_files_say_so() {
        let (remote, _b, s, _sh) = setup("miss");
        s.connect(Some(remote.to_str().unwrap())).unwrap();
        let e = s.fetch(&format!("{}/nope.txt", remote.display())).unwrap_err();
        assert!(e.contains("no such file"), "{e}");
    }

    #[test]
    fn paths_with_spaces_and_quotes_roundtrip() {
        let (remote, _b, s, _sh) = setup("odd");
        let root = s.connect(Some(remote.to_str().unwrap())).unwrap();
        let name = "it's a file $x.txt";
        std::fs::write(remote.join(name), "odd\n").unwrap();
        s.sync_dir(remote.to_str().unwrap()).unwrap();
        let f = root.join(name);
        s.hydrate(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "odd\n");
        std::fs::write(&f, "odd2\n").unwrap();
        s.push(&f, false).unwrap();
        assert_eq!(std::fs::read_to_string(remote.join(name)).unwrap(), "odd2\n");
    }

    #[test]
    fn relisting_adds_removes_and_keeps_downloaded_files() {
        let (remote, _b, s, _sh) = setup("relist");
        let root = s.connect(Some(remote.to_str().unwrap())).unwrap();
        s.hydrate(&root.join("README.md")).unwrap();
        std::fs::write(remote.join("new.txt"), "x").unwrap();
        std::fs::remove_file(remote.join("README.md")).unwrap();
        s.sync_dir(remote.to_str().unwrap()).unwrap();
        assert!(root.join("new.txt").exists());
        assert!(root.join("README.md").exists(), "a downloaded file is not deleted under the user's tab");
        std::fs::write(remote.join("tmp.txt"), "x").unwrap();
        s.sync_dir(remote.to_str().unwrap()).unwrap();
        std::fs::remove_file(remote.join("tmp.txt")).unwrap();
        s.sync_dir(remote.to_str().unwrap()).unwrap();
        assert!(!root.join("tmp.txt").exists(), "a placeholder of a deleted file goes");
    }

    #[test]
    fn dropped_connection_is_offline_and_recovers() {
        let (remote, _b, s, sh) = setup("down");
        s.connect(Some(remote.to_str().unwrap())).unwrap();
        sh.down.store(true, Ordering::SeqCst);
        let e = s.probe().unwrap_err();
        assert!(e.contains("refused"), "{e}");
        assert_eq!(s.link(), Link::Offline);
        let root = s.local_path(remote.to_str().unwrap());
        let f = root.join("README.md");
        std::fs::write(&f, "edit\n").unwrap();
        assert!(matches!(s.push(&f, false), Err(PushErr::Offline(_))));
        sh.down.store(false, Ordering::SeqCst);
        s.reconnect().unwrap();
        assert_eq!(s.link(), Link::Online);
    }

    #[test]
    fn remote_search_finds_lines_with_grep_or_rg() {
        let (remote, _b, s, _sh) = setup("find");
        let root = s.connect(Some(remote.to_str().unwrap())).unwrap();
        std::fs::write(remote.join("src/a.txt"), "x\nsay NEEDLE twice needle\n").unwrap();
        let q = Query { text: "needle".into(), ..Query::default() };
        let hits = s.search(&root, &q, 100).unwrap();
        let mut seen: Vec<(String, usize)> =
            hits.iter().map(|h| (h.path.strip_prefix(&root).unwrap().to_string_lossy().into_owned(), h.line)).collect();
        seen.sort();
        assert_eq!(seen, vec![("README.md".to_string(), 0), ("src/a.txt".to_string(), 1)]);
        let cased = Query { text: "needle".into(), case: true, ..Query::default() };
        let hits = s.search(&root, &cased, 100).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.text.contains("needle")));
    }

    #[test]
    fn parse_search_reads_rg_and_grep_lines() {
        let q = Query { text: "foo".into(), ..Query::default() };
        let out = "#rg\n./a/b.rs:12:let foo = 1;\n./c.rs:3:  foo()\nnoise\n";
        let hits = parse_search(out, &q, |r| PathBuf::from("/m").join(r));
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].path.clone(), hits[0].line, hits[0].col), (PathBuf::from("/m/a/b.rs"), 11, 4));
        assert_eq!(hits[1].line, 2);
    }

    #[test]
    fn registry_maps_paths_and_stays_off_for_local_ones() {
        let (_remote, base, _s, _sh) = setup("reg");
        let s = Arc::new(Session::new(Target::parse("reg@host").unwrap(), &base, Arc::new(LocalSh::new())));
        register(s.clone());
        assert!(is_remote(&s.mirror.join("x/y")));
        assert!(!is_remote(Path::new("/definitely/local")));
        assert_eq!(s.remote_path(&s.mirror), Some("/".to_string()));
        assert_eq!(s.local_path("/a/b"), s.mirror.join("a").join("b"));
        assert!(badge(&s.mirror).unwrap().0.starts_with("SSH reg@host"));
    }

    #[test]
    fn real_runner_passes_the_script_to_the_ssh_binary() {
        let dir = tmp("shim");
        let shim = dir.join("fake-ssh");
        // Ignores every option, takes the last argument (the remote command) and runs it locally.
        std::fs::write(&shim, "#!/bin/sh\neval \"$(for a; do last=$a; done; printf '%s' \"$last\")\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = SshRunner { program: shim.to_string_lossy().into_owned() };
        let t = Target::parse("me@box").unwrap();
        let o = r.run(&t, "cat; echo done", Some(b"in\n"), Duration::from_secs(10)).unwrap();
        assert_eq!((o.code, String::from_utf8_lossy(&o.stdout).into_owned()), (0, "in\ndone\n".to_string()));
        let o = r.run(&t, "exit 7", None, Duration::from_secs(10)).unwrap();
        assert_eq!(o.code, 7);
    }

    #[test]
    fn timeouts_kill_the_command() {
        let mut c = Command::new("sh");
        c.arg("-c").arg("sleep 5");
        let e = exec(c, None, Duration::from_millis(100)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }
}
