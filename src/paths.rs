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
