//! Single-instance support: the first process listens on a Unix socket (on Windows: a localhost TCP
//! port recorded in a file); later invocations hand their file arguments to it and exit.
//!
//! A folder other than the running window's project is not handed over: the running instance
//! answers `new` and the invocation opens its own window (so `lumen-text ~/other-project` and
//! "Open in new window" work). `--new-window` skips the hand-over altogether. Older instances do
//! not answer at all, which reads as "taken" (the previous behaviour).

use crate::lsp::Wake;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        mpsc::{Receiver, channel},
    },
    thread,
    time::Duration,
};

/// Project folder of this window, for deciding about folders other invocations name.
static ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The window's project changed (startup, project switch).
pub fn set_root(p: &Path) {
    if let Ok(mut r) = ROOT.lock() {
        *r = Some(p.to_path_buf());
    }
}

/// Reply of the running instance telling the invocation to open its own window.
const REPLY_NEW: &str = "new";

/// Do the arguments of another invocation belong in the window whose project is `root`? Files
/// do; a folder only when it is that project.
pub fn belongs_here(args: &[String], root: Option<&Path>) -> bool {
    args.iter().all(|a| {
        let (p, _) = crate::parse_target(a);
        !p.is_dir() || root.is_some_and(|r| crate::worktree::same_path(&p, r))
    })
}

#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let uid = unsafe { libc::getuid() };
    dir.join(format!("lumen-text-{uid}.sock"))
}

#[cfg(windows)]
pub fn socket_path() -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_default();
    std::env::temp_dir().join(format!("lumen-text-{user}.port"))
}

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::net::TcpStream;

#[cfg(unix)]
fn connect(sock: &Path) -> std::io::Result<Stream> {
    std::os::unix::net::UnixStream::connect(sock)
}

/// Bind the listener; yields one stream per incoming connection.
#[cfg(unix)]
fn bind(sock: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<Stream>> + Send + 'static> {
    let l = std::os::unix::net::UnixListener::bind(sock)?;
    Ok(std::iter::from_fn(move || Some(l.accept().map(|c| c.0))))
}

#[cfg(windows)]
fn connect(sock: &Path) -> std::io::Result<Stream> {
    let port: u16 = std::fs::read_to_string(sock)?.trim().parse().map_err(std::io::Error::other)?;
    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
}

#[cfg(windows)]
fn bind(sock: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<Stream>> + Send + 'static> {
    let l = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    std::fs::write(sock, l.local_addr()?.port().to_string())?;
    Ok(std::iter::from_fn(move || Some(l.accept().map(|c| c.0))))
}

pub enum Role {
    /// We own the socket; file arguments from later invocations arrive on the receiver.
    Primary(Receiver<Vec<String>>),
    /// Another instance took the arguments; this process should exit.
    Secondary,
    /// Another instance runs, but the arguments name another project: open our own window
    /// (without listening).
    Own,
}

/// A receiver nothing is ever sent to (a window that does not take other invocations' files).
pub fn no_inbox() -> Receiver<Vec<String>> {
    channel().1
}

/// Send `msg` to the running instance and read its answer ("" from instances that do not
/// answer). None when nobody listens.
fn hand_over(sock: &Path, msg: &str) -> Option<String> {
    let mut s = connect(sock).ok()?;
    s.write_all(msg.as_bytes()).ok()?;
    // End of the arguments; the answer follows.
    let _ = s.shutdown(std::net::Shutdown::Write);
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let mut reply = String::new();
    let _ = s.read_to_string(&mut reply);
    Some(reply)
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

/// Take the socket at `sock`, or hand `args` to whoever has it. `here` decides, in the instance
/// that owns the socket, whether another invocation's arguments open in this window.
pub fn claim_at(sock: &Path, args: &[String], wake: Wake, here: impl Fn(&[String]) -> bool + Send + 'static) -> Role {
    let msg = args.iter().map(|a| absolutize(a)).collect::<Vec<_>>().join("\n");
    if let Some(reply) = hand_over(sock, &msg) {
        return if reply.trim() == REPLY_NEW { Role::Own } else { Role::Secondary };
    }
    // Nobody answered: any socket file left behind is stale.
    let _ = std::fs::remove_file(sock);
    let Ok(listener) = bind(sock) else {
        // Could not bind (read-only dir…): just run as an independent instance.
        let (_tx, rx) = channel();
        return Role::Primary(rx);
    };
    let (tx, rx) = channel();
    thread::spawn(move || {
        for conn in listener.flatten() {
            let mut conn = conn;
            let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
            let mut text = String::new();
            if conn.read_to_string(&mut text).is_ok() {
                let args: Vec<String> = text.lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
                if !here(&args) {
                    let _ = conn.write_all(REPLY_NEW.as_bytes());
                    continue;
                }
                let _ = conn.write_all(b"ok");
                if tx.send(args).is_err() {
                    return;
                }
                wake();
            }
        }
    });
    Role::Primary(rx)
}

pub fn claim(args: &[String], wake: Wake) -> Role {
    claim_at(&socket_path(), args, wake, |a| {
        let root = ROOT.lock().ok().and_then(|r| r.clone());
        belongs_here(a, root.as_deref())
    })
}

/// Arguments of a new window on `path`.
pub fn window_args(path: &Path) -> Vec<std::ffi::OsString> {
    vec!["--new-window".into(), path.as_os_str().to_os_string()]
}

/// Start another window on `path` (`lumen-text --new-window <path>`), detached from this process.
pub fn spawn_window(path: &Path) -> std::io::Result<()> {
    // Inside an AppImage the binary lives in a mount that goes away with us: start the AppImage.
    let appimage = std::env::var_os("APPIMAGE").filter(|a| !a.is_empty());
    let exe = match &appimage {
        Some(a) => PathBuf::from(a),
        None => std::env::current_exe()?,
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(window_args(path));
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if appimage.is_some() {
        // The running mount's AppRun exports these; the new AppImage sets its own.
        for k in ["APPDIR", "ARGV0", "OWD", "LUMEN_ASSETS"] {
            cmd.env_remove(k);
        }
    }
    // SAFETY: setsid is async-signal-safe and runs in the forked child before exec.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    // Reap it when it exits.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_process_hands_over_its_files() {
        let sock = std::env::temp_dir().join(format!("lumen-inst-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let wake: Wake = std::sync::Arc::new(|| {});
        let Role::Primary(rx) = claim_at(&sock, &[], wake.clone(), |_| true) else {
            panic!("first claim must be primary")
        };
        let root = if cfg!(windows) { "C:\\tmp" } else { "/tmp" };
        let args = vec![format!("{root}/a.ts:3:4"), format!("{root}/b.ts")];
        assert!(matches!(claim_at(&sock, &args, wake, |_| true), Role::Secondary));
        let got = rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        assert_eq!(got, args);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn stale_socket_is_replaced() {
        let sock = std::env::temp_dir().join(format!("lumen-stale-{}.sock", std::process::id()));
        std::fs::write(&sock, "").unwrap(); // a leftover file, nobody listening
        let wake: Wake = std::sync::Arc::new(|| {});
        assert!(matches!(claim_at(&sock, &[], wake, |_| true), Role::Primary(_)));
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn another_project_folder_opens_its_own_window() {
        let base = std::env::temp_dir().join(format!("lumen-inst-own-{}", std::process::id()));
        let (mine, other) = (base.join("mine"), base.join("other"));
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let sock = base.join("s.sock");
        let wake: Wake = std::sync::Arc::new(|| {});
        let root = mine.clone();
        let Role::Primary(rx) = claim_at(&sock, &[], wake.clone(), move |a| belongs_here(a, Some(&root))) else {
            panic!("first claim must be primary")
        };
        let arg = |p: &Path| vec![p.to_string_lossy().into_owned()];
        assert!(matches!(claim_at(&sock, &arg(&other), wake.clone(), |_| true), Role::Own));
        assert!(rx.try_recv().is_err(), "nothing is handed over for another project");
        // Our own folder (or a file) is taken as before.
        assert!(matches!(claim_at(&sock, &arg(&mine), wake.clone(), |_| true), Role::Secondary));
        let got = rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        assert_eq!(got.len(), 1);
        let f = mine.join("f.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(belongs_here(&arg(&f), Some(&mine)));
        assert!(belongs_here(&arg(&f), None));
        assert!(!belongs_here(&arg(&other), None));
        assert!(!belongs_here(&arg(&other), Some(&mine)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn new_window_arguments() {
        let a = window_args(Path::new("/w/x"));
        assert_eq!(a, vec![std::ffi::OsString::from("--new-window"), std::ffi::OsString::from("/w/x")]);
    }

    #[test]
    fn relative_paths_become_absolute_and_keep_line_suffix() {
        let a = absolutize("Cargo.toml:12:3");
        assert!(Path::new(&a).is_absolute() && a.ends_with("Cargo.toml:12:3"), "{a}");
        #[cfg(unix)]
        assert_eq!(absolutize("/x/y.ts"), "/x/y.ts");
    }
}
