//! Several windows, one process each, and the handoff between invocations.
//!
//! Every window process listens on its own endpoint in a per-user directory (a Unix socket; on
//! Windows a localhost TCP port recorded in a file) and describes itself next to it in
//! `<id>.json`: project root, slot (which of the project's windows it is, see `app_window`) and when
//! it last had the focus. A later `lumen-text` invocation reads that directory and [`route`]s its
//! arguments: files go to the window whose project contains them (else to the last focused one),
//! a folder that is already open focuses its window, a folder that is not opens a new window (the
//! invocation itself becomes it). Language servers, fonts and themes stay per process: the windows
//! share nothing but this directory and the session files.

use crate::lsp::Wake;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, channel},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

/// A message from another invocation or window.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    /// Open these targets (`file`, `file:line:col`); none just raises the window.
    Open(Vec<String>),
    /// "Quit" in another window: close this one too, keeping it in the set reopened next time.
    Quit,
}

/// Wire format: a first line naming the message, then its arguments one per line.
pub fn encode(m: &Msg) -> String {
    match m {
        Msg::Open(args) => {
            std::iter::once("open").chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join("\n")
        }
        Msg::Quit => "quit".into(),
    }
}

/// None for a liveness probe ("ping"). Text without a known first line is a list of files (the
/// format of older versions).
pub fn decode(text: &str) -> Option<Msg> {
    let mut lines = text.lines().map(str::trim_end).filter(|l| !l.is_empty());
    let all = || text.lines().map(str::trim_end).filter(|l| !l.is_empty()).map(str::to_string).collect();
    match lines.next() {
        Some("ping") => None,
        Some("quit") => Some(Msg::Quit),
        Some("open") => Some(Msg::Open(lines.map(str::to_string).collect())),
        _ => Some(Msg::Open(all())),
    }
}

/// A running window as its `<id>.json` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct WinInfo {
    pub id: String,
    pub root: PathBuf,
    pub slot: u32,
    /// Last time it had the focus (ms since the epoch).
    pub focus: u64,
}

/// One command-line target, made absolute.
#[derive(Clone, Debug)]
pub struct Target {
    /// What to hand over (`/abs/file:line:col`).
    pub arg: String,
    pub path: PathBuf,
    pub dir: bool,
}

impl Target {
    pub fn new(arg: &str) -> Target {
        let arg = absolutize(arg);
        let (p, _) = crate::parse_target(&arg);
        let path = crate::paths::canonical(&p).unwrap_or(p);
        Target { dir: path.is_dir(), arg, path }
    }
}

/// Where an invocation's targets go.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// This process becomes a new window.
    Here,
    /// Hand each window (by id) its arguments, then exit.
    Hand(Vec<(String, Vec<String>)>),
}

pub fn last_focused(wins: &[WinInfo]) -> Option<&WinInfo> {
    wins.iter().max_by_key(|w| w.focus)
}

/// The window a file belongs to: the deepest project root containing it, the last focused one
/// among windows of the same project.
pub fn owner<'a>(path: &Path, wins: &'a [WinInfo]) -> Option<&'a WinInfo> {
    wins.iter().filter(|w| path.starts_with(&w.root)).max_by_key(|w| (w.root.components().count(), w.focus))
}

/// Pure routing decision (see the module doc). `force_new` is `--new-window` and friends.
pub fn route(targets: &[Target], wins: &[WinInfo], force_new: bool) -> Route {
    let Some(last) = last_focused(wins) else { return Route::Here };
    if force_new {
        return Route::Here;
    }
    let files: Vec<&Target> = targets.iter().filter(|t| !t.dir).collect();
    if let Some(dir) = targets.iter().find(|t| t.dir) {
        // A folder: its window if it is open (with the files given alongside), else a new one.
        let open = wins.iter().filter(|w| w.root == dir.path).max_by_key(|w| w.focus);
        return match open {
            Some(w) => Route::Hand(vec![(w.id.clone(), files.iter().map(|t| t.arg.clone()).collect())]),
            None => Route::Here,
        };
    }
    if files.is_empty() {
        return Route::Hand(vec![(last.id.clone(), Vec::new())]);
    }
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for t in files {
        let w = owner(&t.path, wins).unwrap_or(last);
        match out.iter_mut().find(|(id, _)| *id == w.id) {
            Some((_, v)) => v.push(t.arg.clone()),
            None => out.push((w.id.clone(), vec![t.arg.clone()])),
        }
    }
    Route::Hand(out)
}

/// Lowest slot no other window of `root` uses.
pub fn free_slot(wins: &[WinInfo], root: &Path) -> u32 {
    (0..).find(|n| !wins.iter().any(|w| w.root == root && w.slot == *n)).unwrap_or(0)
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

// ───────────── endpoints ─────────────

/// Per-user directory holding every window's endpoint and description.
#[cfg(unix)]
pub fn instance_dir() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let uid = unsafe { libc::getuid() };
    dir.join(format!("lumen-text-{uid}.d"))
}

#[cfg(windows)]
pub fn instance_dir() -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_default();
    std::env::temp_dir().join(format!("lumen-text-{user}.d"))
}

#[cfg(unix)]
fn make_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    // Private to the user: the directory may be under a shared /tmp.
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn make_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

fn endpoint(dir: &Path, id: &str) -> PathBuf {
    dir.join(if cfg!(windows) { format!("{id}.port") } else { format!("{id}.sock") })
}

fn info_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

#[cfg(unix)]
fn connect(sock: &Path) -> std::io::Result<impl Write> {
    std::os::unix::net::UnixStream::connect(sock)
}

/// Bind the listener; yields one stream per incoming connection.
#[cfg(unix)]
fn bind(sock: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<impl Read>> + Send + 'static> {
    let l = std::os::unix::net::UnixListener::bind(sock)?;
    Ok(std::iter::from_fn(move || Some(l.accept().map(|c| c.0))))
}

#[cfg(windows)]
fn connect(sock: &Path) -> std::io::Result<impl Write> {
    let port: u16 = std::fs::read_to_string(sock)?.trim().parse().map_err(std::io::Error::other)?;
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port));
    // A closed localhost port is refused only after retries on Windows: do not wait for that.
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300))
}

#[cfg(windows)]
fn bind(sock: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<impl Read>> + Send + 'static> {
    let l = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    std::fs::write(sock, l.local_addr()?.port().to_string())?;
    Ok(std::iter::from_fn(move || Some(l.accept().map(|c| c.0))))
}

/// Deliver a message to window `id`.
pub fn send(dir: &Path, id: &str, m: &Msg) -> std::io::Result<()> {
    connect(&endpoint(dir, id))?.write_all(encode(m).as_bytes())
}

fn forget(dir: &Path, id: &str) {
    let _ = std::fs::remove_file(endpoint(dir, id));
    let _ = std::fs::remove_file(info_path(dir, id));
}

fn parse_info(id: &str, text: &str) -> Option<WinInfo> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(WinInfo {
        id: id.to_string(),
        root: PathBuf::from(v["root"].as_str()?),
        slot: v["slot"].as_u64().unwrap_or(0) as u32,
        focus: v["focus"].as_u64().unwrap_or(0),
    })
}

/// Every window described in `dir` (not checked for liveness).
pub fn list(dir: &Path) -> Vec<WinInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<WinInfo> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?;
            parse_info(id, &std::fs::read_to_string(e.path()).ok()?)
        })
        .collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

/// Windows that answer; the leftovers of crashed ones are removed.
pub fn live(dir: &Path) -> Vec<WinInfo> {
    list(dir)
        .into_iter()
        .filter(|w| {
            let ok = connect(&endpoint(dir, &w.id)).and_then(|mut s| s.write_all(b"ping")).is_ok();
            if !ok {
                forget(dir, &w.id);
            }
            ok
        })
        .collect()
}

/// This window's entry in the instance directory.
#[derive(Debug)]
pub struct Instance {
    pub dir: PathBuf,
    pub id: String,
    /// The other live windows when this one started.
    pub others: Vec<WinInfo>,
    pub root: PathBuf,
    pub slot: u32,
    pub focus: u64,
}

impl Instance {
    /// (Re)write `<id>.json` so other invocations can route to this window.
    pub fn publish(&self) {
        let v = serde_json::json!({
            "root": self.root.to_string_lossy(),
            "slot": self.slot,
            "focus": self.focus,
            "pid": std::process::id(),
        });
        let path = info_path(&self.dir, &self.id);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// The other windows that are running now.
    pub fn others_live(&self) -> Vec<WinInfo> {
        live(&self.dir).into_iter().filter(|w| w.id != self.id).collect()
    }

    /// The window is closing: stop being a handoff target.
    pub fn leave(&self) {
        forget(&self.dir, &self.id);
    }
}

pub enum Role {
    /// This process is a window; messages from other invocations arrive on the receiver. The
    /// instance is None when the directory could not be used (the window then runs on its own).
    Primary(Receiver<Msg>, Option<Instance>),
    /// The arguments went to running windows; this process should exit.
    Secondary,
}

/// Make `file:line:col` arguments absolute so the other process (different cwd) can open them.
pub fn absolutize(arg: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    // A Windows drive prefix (`C:\`) is not a `:line` suffix.
    let skip = if cfg!(windows) && arg.as_bytes().get(1) == Some(&b':') { 2 } else { 0 };
    let (path, suffix) = match arg[skip..].find(':').map(|i| i + skip) {
        Some(i) if !Path::new(arg).exists() => (&arg[..i], &arg[i..]),
        _ => (arg, ""),
    };
    let p = Path::new(path);
    let abs = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    format!("{}{suffix}", abs.display())
}

pub fn claim_in(dir: &Path, args: &[String], force_new: bool, wake: Wake) -> Role {
    let targets: Vec<Target> = args.iter().map(|a| Target::new(a)).collect();
    let mut others = live(dir);
    // A window may die between the listing and the handoff: route again without it.
    for _ in 0..3 {
        let Route::Hand(plan) = route(&targets, &others, force_new) else { break };
        let failed: Vec<String> = plan
            .iter()
            .filter(|(id, a)| send(dir, id, &Msg::Open(a.clone())).is_err())
            .map(|(id, _)| id.clone())
            .collect();
        if failed.is_empty() {
            return Role::Secondary;
        }
        for id in &failed {
            forget(dir, id);
        }
        others.retain(|w| !failed.contains(&w.id));
        if failed.len() < plan.len() {
            // Some windows took their files: those meant for the dead ones go to the last
            // focused window still running.
            let rest: Vec<String> =
                plan.iter().filter(|(id, _)| failed.contains(id)).flat_map(|(_, a)| a.clone()).collect();
            if let Some(w) = last_focused(&others) {
                let _ = send(dir, &w.id, &Msg::Open(rest));
            }
            return Role::Secondary;
        }
    }
    let id = std::process::id().to_string();
    let sock = endpoint(dir, &id);
    let _ = std::fs::remove_file(&sock);
    let listener = match make_dir(dir).and_then(|_| bind(&sock)) {
        Ok(l) => l,
        Err(_) => {
            // Could not bind (read-only dir…): just run as an independent window.
            let (_tx, rx) = channel();
            return Role::Primary(rx, None);
        }
    };
    let (tx, rx) = channel();
    thread::spawn(move || {
        for conn in listener.flatten() {
            let mut conn = conn;
            let mut text = String::new();
            if conn.read_to_string(&mut text).is_err() {
                continue;
            }
            let Some(msg) = decode(&text) else { continue };
            if tx.send(msg).is_err() {
                return;
            }
            wake();
        }
    });
    let inst = Instance { dir: dir.to_path_buf(), id, others, root: PathBuf::new(), slot: 0, focus: now_ms() };
    Role::Primary(rx, Some(inst))
}

pub fn claim(args: &[String], force_new: bool, wake: Wake) -> Role {
    claim_in(&instance_dir(), args, force_new, wake)
}

/// Open `path` in a window of its own: a plain `lumen-text <path>` invocation, which raises the
/// window that has it open or starts a new one with its saved tabs.
pub fn spawn_window(path: &Path) -> std::io::Result<()> {
    crate::app_window::spawn_window(&[path.to_string_lossy().into_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn win(id: &str, root: &str, slot: u32, focus: u64) -> WinInfo {
        WinInfo { id: id.into(), root: PathBuf::from(root), slot, focus }
    }

    fn t(arg: &str, dir: bool) -> Target {
        let (p, _) = crate::parse_target(arg);
        Target { arg: arg.into(), path: p, dir }
    }

    fn hand(v: &[(&str, &[&str])]) -> Route {
        Route::Hand(v.iter().map(|(id, a)| (id.to_string(), a.iter().map(|s| s.to_string()).collect())).collect())
    }

    #[test]
    fn routing_files_folders_and_focus() {
        let wins = [win("a", "/w/api", 0, 10), win("b", "/w/web", 0, 30), win("c", "/w/api/sub", 0, 5)];
        // Nobody running, or a forced new window: this process opens.
        assert_eq!(route(&[t("/w/api/x.rs", false)], &[], false), Route::Here);
        assert_eq!(route(&[t("/w/api/x.rs", false)], &wins, true), Route::Here);
        // No targets: raise the last focused window.
        assert_eq!(route(&[], &wins, false), hand(&[("b", &[])]));
        // Files go to the window whose project contains them (the deepest root wins); others to
        // the last focused window, in one batch per window.
        let r = route(
            &[t("/w/api/x.rs:3:4", false), t("/w/api/sub/y.rs", false), t("/elsewhere/z.md", false)],
            &wins,
            false,
        );
        assert_eq!(r, hand(&[("a", &["/w/api/x.rs:3:4"]), ("c", &["/w/api/sub/y.rs"]), ("b", &["/elsewhere/z.md"])]));
        // A folder that is open focuses its window (with any files given alongside)…
        assert_eq!(
            route(&[t("/w/web", true), t("/w/api/x.rs", false)], &wins, false),
            hand(&[("b", &["/w/api/x.rs"])])
        );
        // …and one that is not opens a new window, even if a parent project is open.
        assert_eq!(route(&[t("/w/other", true)], &wins, false), Route::Here);
        assert_eq!(route(&[t("/w/api/deep", true)], &wins, false), Route::Here);
    }

    #[test]
    fn two_windows_of_one_project_route_by_focus_and_take_free_slots() {
        let wins = [win("a", "/p", 0, 10), win("b", "/p", 1, 20)];
        assert_eq!(route(&[t("/p/f.rs", false)], &wins, false), hand(&[("b", &["/p/f.rs"])]));
        assert_eq!(route(&[t("/p", true)], &wins, false), hand(&[("b", &[])]));
        assert_eq!(free_slot(&wins, Path::new("/p")), 2);
        assert_eq!(free_slot(&wins[1..], Path::new("/p")), 0, "slot 0 is free again");
        assert_eq!(free_slot(&wins, Path::new("/q")), 0);
    }

    #[test]
    fn messages_round_trip() {
        let open = Msg::Open(vec!["/a.ts:3:4".into(), "/b.ts".into()]);
        assert_eq!(decode(&encode(&open)), Some(open));
        assert_eq!(decode(&encode(&Msg::Open(vec![]))), Some(Msg::Open(vec![])));
        assert_eq!(decode(&encode(&Msg::Quit)), Some(Msg::Quit));
        assert_eq!(decode("ping"), None);
        assert_eq!(decode("/old/format.ts\n/x.ts"), Some(Msg::Open(vec!["/old/format.ts".into(), "/x.ts".into()])));
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-inst-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn second_invocation_hands_over_by_project_and_folders_open_new_windows() {
        let dir = scratch("route");
        let wake: Wake = std::sync::Arc::new(|| {});
        let root = std::env::temp_dir().join(format!("lumen-inst-proj-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::paths::canonical(&root).unwrap();
        let Role::Primary(rx, Some(mut inst)) = claim_in(&dir, &[], false, wake.clone()) else {
            panic!("first claim must be a window")
        };
        // The window describes itself once it knows its project.
        inst.root = root.clone();
        inst.publish();
        let file = format!("{}:3:4", root.join("a.ts").display());
        assert!(matches!(claim_in(&dir, std::slice::from_ref(&file), false, wake.clone()), Role::Secondary));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Msg::Open(vec![file]));
        // The open folder focuses this window.
        let r = root.to_string_lossy().into_owned();
        assert!(matches!(claim_in(&dir, &[r], false, wake.clone()), Role::Secondary));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Msg::Open(vec![]));
        // Pings (liveness checks) are not delivered.
        assert!(rx.try_recv().is_err());
        assert_eq!(live(&dir).len(), 1);
        inst.leave();
        assert!(list(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dead_windows_are_forgotten() {
        let dir = scratch("stale");
        std::fs::create_dir_all(&dir).unwrap();
        // A description left behind by a crashed window, nobody listening.
        std::fs::write(dir.join("999999.json"), r#"{"root":"/x","slot":0,"focus":1}"#).unwrap();
        std::fs::write(endpoint(&dir, "999999"), "").unwrap();
        assert_eq!(list(&dir).len(), 1);
        let wake: Wake = std::sync::Arc::new(|| {});
        let Role::Primary(_, Some(inst)) = claim_in(&dir, &["/x/a.ts".into()], false, wake) else {
            panic!("the dead window must not take the file")
        };
        assert!(inst.others.is_empty());
        assert!(!dir.join("999999.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_paths_become_absolute_and_keep_line_suffix() {
        let a = absolutize("Cargo.toml:12:3");
        assert!(Path::new(&a).is_absolute() && a.ends_with("Cargo.toml:12:3"), "{a}");
        #[cfg(unix)]
        assert_eq!(absolutize("/x/y.ts"), "/x/y.ts");
    }
}
