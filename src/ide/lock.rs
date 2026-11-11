//! The discovery file Claude Code looks for: `~/.claude/ide/<port>.lock` (or under
//! `$CLAUDE_CONFIG_DIR/ide`), holding the port's owner, its workspace and the auth token. The
//! folder is private (0700) and the file too (0600); it goes away with the server, and files left
//! by a Lumen that died are swept at the next start.

use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// The name the CLI shows for us (`/ide`), and the mark of our own lock files.
pub const IDE_NAME: &str = "Lumen";

/// Where lock files live: `$CLAUDE_CONFIG_DIR/ide`, else `~/.claude/ide`.
pub fn ide_dir_from(config_dir: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match config_dir.filter(|d| !d.is_empty()) {
        Some(d) => Some(PathBuf::from(d).join("ide")),
        None => home.map(|h| h.join(".claude").join("ide")),
    }
}

pub fn ide_dir() -> Option<PathBuf> {
    ide_dir_from(std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(), crate::paths::home().as_deref())
}

/// The lock file's JSON.
pub fn contents(pid: u32, folders: &[PathBuf], token: &str) -> Value {
    let folders: Vec<String> = folders.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    json!({
        "pid": pid,
        "workspaceFolders": folders,
        "ideName": IDE_NAME,
        "transport": "ws",
        "authToken": token,
    })
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)
}

/// Write (or rewrite) `<dir>/<port>.lock`; returns its path.
pub fn write(dir: &Path, port: u16, pid: u32, folders: &[PathBuf], token: &str) -> io::Result<PathBuf> {
    private_dir(dir)?;
    let path = dir.join(format!("{port}.lock"));
    let mut f = create_private(&path)?;
    // A file that existed before keeps its mode under `open`: tighten it either way.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(contents(pid, folders, token).to_string().as_bytes())?;
    Ok(path)
}

/// Is process `pid` still there?
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // Signal 0 checks existence; EPERM means it exists but belongs to someone else.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Elsewhere a stale file is left alone (the CLI checks the port anyway).
#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}

/// Remove lock files Lumen left behind: ours by name, whose process is gone. Other IDEs' files,
/// and live ones, are never touched. Returns how many were removed.
pub fn sweep(dir: &Path) -> usize {
    sweep_with(dir, pid_alive)
}

pub fn sweep_with(dir: &Path, alive: impl Fn(u32) -> bool) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "lock") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let ours = v["ideName"].as_str() == Some(IDE_NAME);
        let pid = v["pid"].as_u64().unwrap_or(0) as u32;
        if ours && !alive(pid) && std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    n
}
