//! Per-user directories by platform convention. The XDG variables win everywhere when set;
//! otherwise Linux uses `~/.config`, `~/.local/share`, `~/.local/state`, `~/.cache`; macOS uses
//! `~/Library/Application Support` and `~/Library/Caches`; Windows uses `%APPDATA%` and `%LOCALAPPDATA%`.
//! Callers append `lumen-text/...` themselves.

use std::{ffi::OsString, path::PathBuf};

fn var(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// The user's home directory (`HOME`, or `USERPROFILE` on Windows).
pub fn home() -> Option<PathBuf> {
    var("HOME").or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None }).map(PathBuf::from)
}

fn pick(xdg: &str, linux: &str, mac: &str, win: &str) -> Option<PathBuf> {
    if let Some(x) = var(xdg) {
        return Some(PathBuf::from(x));
    }
    if cfg!(windows) {
        return var(win).map(PathBuf::from).or_else(|| home().map(|h| h.join("AppData").join(win_sub(win))));
    }
    let sub = if cfg!(target_os = "macos") { mac } else { linux };
    home().map(|h| h.join(sub))
}

fn win_sub(var: &str) -> &'static str {
    if var == "APPDATA" { "Roaming" } else { "Local" }
}

pub fn config_home() -> Option<PathBuf> {
    pick("XDG_CONFIG_HOME", ".config", "Library/Application Support", "APPDATA")
}

pub fn data_home() -> Option<PathBuf> {
    pick("XDG_DATA_HOME", ".local/share", "Library/Application Support", "LOCALAPPDATA")
}

pub fn state_home() -> Option<PathBuf> {
    pick("XDG_STATE_HOME", ".local/state", "Library/Application Support", "LOCALAPPDATA")
}

pub fn cache_home() -> Option<PathBuf> {
    pick("XDG_CACHE_HOME", ".cache", "Library/Caches", "LOCALAPPDATA")
}

/// `std::fs::canonicalize`, minus the `\\?\` verbatim prefix it adds on Windows (see [`simplify`]).
pub fn canonical(p: &std::path::Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(p).map(simplify)
}

/// A Windows verbatim path (`\\?\C:\x`, `\\?\UNC\server\share\x`, what `canonicalize` returns) as
/// the plain path it stands for (`C:\x`, `\\server\share\x`), when that plain form means the same
/// file. Verbatim paths leak into the UI, session files, `file://` URIs for language servers and
/// git arguments, and never compare equal to the plain paths tools print. Elsewhere: unchanged.
pub fn simplify(p: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return p;
    }
    match p.to_str().and_then(strip_verbatim) {
        Some(s) => PathBuf::from(s),
        None => p,
    }
}

/// The plain form of a verbatim path string, or `None` when it is not verbatim or has no plain
/// equivalent (too long for `MAX_PATH`, or a component Win32 would rewrite: a reserved device
/// name, a trailing dot or space, a forward slash).
fn strip_verbatim(s: &str) -> Option<String> {
    let rest = s.strip_prefix(r"\\?\")?;
    let (plain, tail) = if let Some(unc) = rest.strip_prefix(r"UNC\") {
        (format!(r"\\{unc}"), unc)
    } else {
        let b = rest.as_bytes();
        if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
            return None;
        }
        (rest.to_string(), rest.get(3..).unwrap_or(""))
    };
    const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    let bad = |c: &str| {
        let stem = c.split('.').next().unwrap_or(c).trim_end();
        let numbered = stem.len() == 4
            && ["COM", "LPT"].iter().any(|d| stem.get(..3).is_some_and(|s| s.eq_ignore_ascii_case(d)))
            && stem.as_bytes()[3].is_ascii_digit();
        c == "."
            || c == ".."
            || c.ends_with('.')
            || c.ends_with(' ')
            || numbered
            || RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r))
    };
    (plain.len() < 260 && !plain.contains('/') && !tail.split('\\').filter(|c| !c.is_empty()).any(bad)).then_some(plain)
}

/// Create a symbolic link at `link` pointing to `target` (on Windows this needs developer mode or admin rights).
#[cfg(unix)]
pub fn symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
pub fn symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Broken-down local time of a Unix timestamp: (year, month 1-12, day, hour, minute, second).
#[cfg(unix)]
pub fn local_time(ts: i64) -> (i32, u32, u32, u32, u32, u32) {
    let t = ts as libc::time_t;
    // SAFETY: `localtime_r` only writes into the zeroed `tm` we own.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (
            tm.tm_year + 1900,
            (tm.tm_mon + 1) as u32,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
            tm.tm_sec as u32,
        )
    }
}

#[cfg(windows)]
pub fn local_time(ts: i64) -> (i32, u32, u32, u32, u32, u32) {
    use windows_sys::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
    };
    let ticks = ((ts + 11_644_473_600).max(0) as u64) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    // SAFETY: plain-data out-parameters.
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
        {
            local = utc;
        }
        (
            local.wYear as i32,
            local.wMonth as u32,
            local.wDay as u32,
            local.wHour as u32,
            local.wMinute as u32,
            local.wSecond as u32,
        )
    }
}

/// Seconds since the Unix epoch now.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Open a URL or file with the system's default handler (`xdg-open`, `open`, the Windows shell), detached.
pub fn open_url(url: &str) {
    use std::process::{Command, Stdio};
    let url = url.to_string();
    std::thread::spawn(move || {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("rundll32");
            c.args(["url.dll,FileProtocolHandler", &url]);
            c
        } else if cfg!(target_os = "macos") {
            let mut c = Command::new("open");
            c.arg(&url);
            c
        } else {
            let mut c = Command::new("xdg-open");
            c.arg(&url);
            c
        };
        if let Ok(mut c) = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            let _ = c.wait();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_stripped_when_the_plain_path_is_equivalent() {
        assert_eq!(strip_verbatim(r"\\?\C:\Users\a b\x.rs").as_deref(), Some(r"C:\Users\a b\x.rs"));
        assert_eq!(strip_verbatim(r"\\?\C:\").as_deref(), Some(r"C:\"));
        assert_eq!(strip_verbatim(r"\\?\d:"), None, "drive-relative");
        assert_eq!(strip_verbatim(r"\\?\UNC\srv\share\x").as_deref(), Some(r"\\srv\share\x"));
        assert_eq!(strip_verbatim(r"C:\plain"), None, "not verbatim");
        assert_eq!(strip_verbatim(r"\\?\Volume{1234}\x"), None, "no drive letter");
        assert_eq!(strip_verbatim(r"\\?\C:\a\nul.txt"), None, "reserved device name");
        assert_eq!(strip_verbatim(r"\\?\C:\a\COM1"), None, "reserved device name");
        assert_eq!(strip_verbatim(r"\\?\C:\a\com10"), Some(r"C:\a\com10".into()));
        assert_eq!(strip_verbatim(r"\\?\C:\a\dots."), None, "trailing dot");
        assert_eq!(strip_verbatim(&format!(r"\\?\C:\{}", "x".repeat(300))), None, "past MAX_PATH");
    }

    #[test]
    fn canonical_paths_are_plain() {
        let d = canonical(&std::env::temp_dir()).unwrap();
        assert!(d.is_absolute());
        assert!(!d.to_string_lossy().starts_with(r"\\?\"), "{d:?}");
        #[cfg(unix)]
        assert_eq!(d, std::fs::canonicalize(std::env::temp_dir()).unwrap());
    }
}
