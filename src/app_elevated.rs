//! "Save as administrator": when saving is refused for lack of permission (`/etc/hosts`), a card
//! offers to write the file through the system's elevation prompt.
//!
//! The text goes to a private temp file first (mode 0600). A tiny shell script, run as root through
//! `pkexec` (polkit, Linux and other Unixes) or `osascript … with administrator privileges`
//! (macOS), then copies it *over* the target in place, the way `sudo tee` does: the file keeps its
//! inode, owner, mode, ACLs and SELinux label. The script refuses a target that is not a regular
//! file or is a symbolic link (the path is the canonical one the document was opened with, so a
//! link there is new), and where `dd` knows `oflag=nofollow` it opens the target without following
//! links at all. Everything runs on a worker thread; the result comes back through a channel that
//! the event loop drains (`pump_elevated`). Windows has no such prompt: a clear message instead.

use crate::app::App;
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

/// Outcome of one elevated save.
pub struct Done {
    pub path: PathBuf,
    /// Buffer version the written text had (the buffer is clean again unless edited since).
    pub version: u64,
    pub result: Result<(), String>,
}

/// Run as root with `$1` = target, `$2` = the temp file holding the new text.
#[cfg_attr(windows, allow(dead_code))]
const SCRIPT: &str = r#"t=$1; s=$2
if [ -L "$t" ] || [ ! -f "$t" ]; then echo "not a regular file: $t" >&2; exit 3; fi
if dd if=/dev/null of=/dev/null oflag=nofollow status=none 2>/dev/null; then
  exec dd if="$s" of="$t" oflag=nofollow conv=fsync status=none
fi
cat -- "$s" > "$t""#;

/// Tests run the script directly, without the password prompt.
#[cfg(test)]
static DIRECT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The command that writes `src` over `target` with administrator rights.
#[cfg(unix)]
fn command(target: &Path, src: &Path) -> std::process::Command {
    #[cfg(test)]
    let direct = DIRECT.load(std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(test))]
    let direct = false;
    if direct {
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg(SCRIPT).arg("lumen-save").arg(target).arg(src);
        return c;
    }
    // macOS: the values travel as arguments of the AppleScript, quoted for the shell by `quoted form`.
    #[cfg(target_os = "macos")]
    let mut c = std::process::Command::new("osascript");
    #[cfg(target_os = "macos")]
    c.args([
        "-e",
        "on run argv",
        "-e",
        "do shell script \"/bin/sh -c \" & quoted form of item 1 of argv & \" lumen-save \" & quoted form of item 2 of argv & \" \" & quoted form of item 3 of argv with administrator privileges",
        "-e",
        "end run",
        SCRIPT,
    ]);
    #[cfg(not(target_os = "macos"))]
    let mut c = std::process::Command::new("pkexec");
    #[cfg(not(target_os = "macos"))]
    c.arg("/bin/sh").arg("-c").arg(SCRIPT).arg("lumen-save");
    c.arg(target).arg(src);
    c
}

/// What the user reads when the elevated write did not succeed (`code` = exit code).
#[cfg_attr(windows, allow(dead_code))]
pub fn failure_message(code: Option<i32>, stderr: &str) -> String {
    let err = stderr.trim();
    if cfg!(target_os = "macos") && err.contains("-128") {
        return "Save as administrator cancelled".into();
    }
    if !cfg!(target_os = "macos") {
        // pkexec: 126 = the authentication dialog was dismissed, 127 = not authorized / no agent.
        match code {
            Some(126) => return "Save as administrator cancelled".into(),
            Some(127) if err.is_empty() || err.contains("authoriz") || err.contains("agent") => {
                return "Save as administrator: not authorized (is a polkit authentication agent running?)".into();
            }
            _ => {}
        }
    }
    let why = match code {
        _ if !err.is_empty() => err.to_string(),
        Some(c) => format!("exit code {c}"),
        None => "stopped by a signal".to_string(),
    };
    format!("Could not save as administrator: {why}")
}

/// A new private temp file (0600, created exclusively) holding `bytes`.
#[cfg(unix)]
fn write_private_temp(bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    for _ in 0..8 {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("lumen-text-save-{}-{n}-{nanos}.tmp", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p) {
            Ok(mut f) => {
                f.write_all(bytes)?;
                f.flush()?;
                return Ok(p);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("could not create a temporary file"))
}

/// Write `text` over `target` with administrator rights (blocking: call it off the UI thread).
#[cfg(unix)]
pub fn write_elevated(target: &Path, text: &str) -> Result<(), String> {
    let tmp = write_private_temp(text.as_bytes()).map_err(|e| format!("Could not save as administrator: {e}"))?;
    let out = command(target, &tmp).stdin(std::process::Stdio::null()).output();
    let _ = std::fs::remove_file(&tmp);
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(failure_message(o.status.code(), &String::from_utf8_lossy(&o.stderr))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(if cfg!(target_os = "macos") {
            "Save as administrator needs osascript, which was not found".into()
        } else {
            "pkexec not found: install polkit to save as administrator".into()
        }),
        Err(e) => Err(format!("Could not save as administrator: {e}")),
    }
}

#[cfg(windows)]
pub fn write_elevated(_target: &Path, _text: &str) -> Result<(), String> {
    Err(WINDOWS_MSG.into())
}

#[cfg_attr(not(windows), allow(dead_code))]
const WINDOWS_MSG: &str =
    "Permission denied: saving as administrator is not supported on Windows (run Lumen as administrator)";

/// Was this save error a refusal for lack of permission?
pub fn is_permission_denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

impl App {
    /// Saving document `i` was refused for lack of permission: offer "Save as administrator"
    /// (only for an existing regular file: the elevated write replaces content, it does not create).
    pub fn offer_elevated_save(&mut self, i: usize) {
        let Some(d) = self.docs.get(i) else { return };
        let Some(path) = d.path().map(Path::to_path_buf) else { return };
        let name = d.name.clone();
        if cfg!(windows) {
            return self.say_for(format!("{name}: {WINDOWS_MSG}"), 8);
        }
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            return self.say(format!("Error saving {name}: permission denied"));
        }
        self.show_ask(crate::app_ask::elevated_question(&name, path));
    }

    /// "Save as administrator" was confirmed for the document at `path`.
    pub fn save_elevated(&mut self, path: &Path) {
        if self.elevated.is_some() {
            return self.say("A save as administrator is already in progress");
        }
        let Some(d) = self.docs.iter().find(|d| d.path() == Some(path)) else { return };
        let (text, version, name) = (d.buf.to_string(), d.buf.version(), d.name.clone());
        let (tx, rx) = mpsc::channel();
        let wake = self.wake();
        let target = path.to_path_buf();
        std::thread::spawn(move || {
            let result = write_elevated(&target, &text);
            let _ = tx.send(Done { path: target, version, result });
            wake();
        });
        self.elevated = Some(rx);
        self.say(format!("Saving {name} as administrator…"));
    }

    /// Pick up the result of an elevated save (called whenever the event loop is woken).
    pub fn pump_elevated(&mut self) {
        let Some(rx) = self.elevated.as_ref() else { return };
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.elevated = None;
                return;
            }
        };
        self.elevated = None;
        let i = self.docs.iter().position(|d| d.path() == Some(done.path.as_path()));
        match (done.result, i) {
            (Ok(()), Some(i)) => {
                let d = &mut self.docs[i];
                d.buf.mark_saved(done.version);
                d.git_stale = true;
                d.disk_mtime = d.stat_mtime();
                d.conflict = false;
                let name = d.name.clone();
                self.lsp_did_save(i);
                self.refresh_git_status();
                self.say(format!("Saved {name} as administrator"));
            }
            (Ok(()), None) => self.say(format!("Saved {} as administrator", done.path.display())),
            (Err(e), _) => self.say_for(e, 8),
        }
        self.request_redraw();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use std::os::unix::fs::PermissionsExt;

    fn run_script(target: &Path, src: &Path) -> std::process::Output {
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(SCRIPT)
            .arg("lumen-save")
            .arg(target)
            .arg(src)
            .output()
            .unwrap()
    }

    #[test]
    fn the_script_writes_in_place_and_refuses_links() {
        let dir = tmp("elevscript");
        let target = dir.join("hosts");
        std::fs::write(&target, "old old old old\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let ino = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&target).unwrap());
        let src = dir.join("new");
        std::fs::write(&src, "new\n").unwrap();
        let out = run_script(&target, &src);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n", "shorter text truncates the rest");
        let meta = std::fs::metadata(&target).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o640, "mode kept");
        assert_eq!(std::os::unix::fs::MetadataExt::ino(&meta), ino, "same file (in place)");
        // A symbolic link (or a folder) is refused and nothing is written through it.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        std::fs::write(&src, "evil\n").unwrap();
        let out = run_script(&link, &src);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("not a regular file"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
        assert!(!run_script(&dir, &src).status.success());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failures_read_clearly() {
        if !cfg!(target_os = "macos") {
            assert_eq!(failure_message(Some(126), ""), "Save as administrator cancelled");
            assert!(failure_message(Some(127), "").contains("not authorized"));
        }
        assert_eq!(
            failure_message(Some(3), "not a regular file: /x\n"),
            "Could not save as administrator: not a regular file: /x"
        );
        assert_eq!(failure_message(Some(1), ""), "Could not save as administrator: exit code 1");
    }

    #[test]
    fn a_refused_save_offers_the_elevated_one_and_it_marks_the_buffer_clean() {
        // Root may write anywhere: the refusal this test needs cannot happen.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let _g = lock();
        let dir = tmp("elevsave");
        let mut app = test_app(&dir);
        // A writable file in a folder we may not write: the atomic save (temp file + rename) is refused.
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        let f = locked.join("conf.txt");
        std::fs::write(&f, "a = 1\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let i = app.open_path(&f, None).unwrap();
        app.docs[i].replace_text("a = 2\n");
        assert!(app.docs[i].buf.is_dirty());
        app.save_active();
        let ask = app.ask_open().expect("the card offers to save as administrator");
        assert_eq!(ask.action, crate::app_ask::AskAction::SaveElevated(app.docs[i].path().unwrap().to_path_buf()));
        assert!(app.docs[i].buf.is_dirty(), "nothing saved yet");

        DIRECT.store(true, std::sync::atomic::Ordering::SeqCst);
        app.ask_key(&winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.elevated.is_some() && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.pump_elevated();
        }
        DIRECT.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!app.docs[i].buf.is_dirty(), "clean after the elevated save");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a = 2\n");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o644);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
