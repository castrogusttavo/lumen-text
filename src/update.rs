//! Auto-update engine: no UI, no async runtime, tiny memory.
//!
//! * `latest.json` (published on every GitHub release) says which version is current and where
//!   the signed files live (format: see CONTRIBUTING.md; modeled on Tauri's updater manifest).
//! * Files are signed with minisign; the public key is embedded in the binary
//!   (`src/update_key.pub`) and checked with the tiny `minisign-verify` crate (streaming, no
//!   file is ever fully loaded in memory).
//! * Downloads go through the system `curl` (or `wget`) into the user cache dir, never /tmp.
//! * Install kinds: AppImage (replace the file, keep `.old`), user layout
//!   `~/.local/share/lumen-text/<version>/` + `current` symlink, or system-managed (notify only).
//!
//! Everything here is plain functions over plain data so it can be unit-tested; the GUI side
//! lives in `app_update.rs`.

use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub const REPO: &str = "castrogusttavo/lumen-text";
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/castrogusttavo/lumen-text/releases/latest/download/latest.json";
/// The minisign public key every release file must be signed with.
const PUBLIC_KEY: &str = include_str!("update_key.pub");

pub const STARTUP_DELAY: Duration = Duration::from_secs(20);
pub const CHECK_EVERY: Duration = Duration::from_secs(4 * 3600);
const MANIFEST_CAP: u64 = 256 * 1024;
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(25);
const ASSET_CAP: u64 = 400 * 1024 * 1024;
const ASSET_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The running version: stamped by CI (`LUMEN_VERSION`, CalVer `YYYY.M.D.N`) or Cargo's.
pub fn current_version() -> &'static str {
    option_env!("LUMEN_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

// ───────────────────────────── versions ─────────────────────────────

/// CalVer `YYYY.M.D.N`, compared numerically per component. A missing `N` (Cargo's stamped
/// `2026.11.9`) counts as `.0`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Version([u64; 4]);

impl Version {
    /// `2026.11.10.1`, `v2026.11.10.1` or `2026.11.9`; anything else is rejected.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let parts: Vec<&str> = s.split('.').collect();
        if !(3..=4).contains(&parts.len()) {
            return None;
        }
        let mut v = [0u64; 4];
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            v[i] = p.parse().ok()?;
        }
        Some(Version(v))
    }
}

/// Is `remote` strictly newer than `current`? Unparsable versions are never newer.
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (Version::parse(remote), Version::parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

/// Release builds carry all four CalVer components; Cargo's own `2026.11.9` marks a local build.
pub fn is_release_build(version: &str) -> bool {
    version.trim_start_matches('v').split('.').count() == 4 && Version::parse(version).is_some()
}

// ───────────────────────────── schedule ─────────────────────────────

/// When to check next: once shortly after startup, then every [`CHECK_EVERY`]; after a failure
/// retry with a growing delay (5 min, 10 min, ... capped at the normal interval), never give up.
#[derive(Clone, Debug)]
pub struct Schedule {
    next: Instant,
    failures: u32,
}

/// Delay before the next try after `failures` consecutive failures (>= 1).
pub fn backoff(failures: u32) -> Duration {
    let secs = 300u64.saturating_mul(1u64 << failures.saturating_sub(1).min(10));
    Duration::from_secs(secs).min(CHECK_EVERY)
}

impl Schedule {
    pub fn new(now: Instant, first_delay: Duration) -> Schedule {
        Schedule { next: now + first_delay, failures: 0 }
    }
    pub fn next(&self) -> Instant {
        self.next
    }
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }
    #[cfg(test)]
    pub fn failures(&self) -> u32 {
        self.failures
    }
    pub fn on_success(&mut self, now: Instant) {
        self.failures = 0;
        self.next = now + CHECK_EVERY;
    }
    pub fn on_failure(&mut self, now: Instant) {
        self.failures += 1;
        self.next = now + backoff(self.failures);
    }
}

/// Announce each version once (a version older than or equal to the last announced one is silent).
#[derive(Clone, Debug, Default)]
pub struct Announcer {
    last: Option<Version>,
}

impl Announcer {
    pub fn should_announce(&mut self, version: &str) -> bool {
        let Some(v) = Version::parse(version) else { return false };
        if self.last.is_some_and(|l| l >= v) {
            return false;
        }
        self.last = Some(v);
        true
    }
}

// ───────────────────────────── manifest ─────────────────────────────

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Platform {
    pub url: String,
    /// Base64 of the `.minisig` file (Tauri style); empty for informational entries.
    #[serde(default)]
    pub signature: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub platforms: BTreeMap<String, Platform>,
}

pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let m: Manifest = serde_json::from_str(text).map_err(|e| format!("invalid latest.json: {e}"))?;
    if Version::parse(&m.version).is_none() {
        return Err(format!("invalid version in latest.json: {:?}", m.version));
    }
    Ok(m)
}

/// Key of a file in `platforms`: `linux-x86_64-appimage`, `linux-aarch64-tarball`, ...
pub fn platform_key(arch: &str, format: &str) -> String {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    format!("{os}-{arch}-{format}")
}

// ───────────────────────────── install kinds ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Running from an AppImage; the path is the AppImage file itself (`$APPIMAGE`).
    AppImage(PathBuf),
    /// `<data>/lumen-text/<version>/bin/lumen-text` with a `current` symlink; the path is `<data>/lumen-text`.
    User(PathBuf),
    /// Windows: installed per user by the Inno Setup installer (`lumen-text-<v>-windows-x86_64-setup.exe`);
    /// the path is the install directory (it holds `unins000.exe`). Updates by running the newer setup silently.
    Setup(PathBuf),
    /// Installed by a package manager under /usr/bin: only notify.
    System,
    /// A tarball run in place, a local build, /usr/local...: only notify.
    Other,
}

impl InstallKind {
    pub fn can_self_update(&self) -> bool {
        matches!(self, InstallKind::AppImage(_) | InstallKind::User(_) | InstallKind::Setup(_))
    }
    /// Short description for the UI (Portuguese until the UI is translated).
    pub fn label(&self) -> &'static str {
        match self {
            InstallKind::AppImage(_) => "AppImage",
            InstallKind::User(_) => "user install",
            InstallKind::Setup(_) => "installer",
            InstallKind::System => "system package",
            InstallKind::Other => "manual",
        }
    }
    /// Same for the command line.
    pub fn name(&self) -> &'static str {
        match self {
            InstallKind::AppImage(_) => "AppImage",
            InstallKind::User(_) => "user install",
            InstallKind::Setup(_) => "installed with the Windows installer",
            InstallKind::System => "system package",
            InstallKind::Other => "manual install",
        }
    }
    /// Which `platforms` entry matches this install (system installs depend on the distro).
    pub fn format_key(&self, pkg: PkgFormat) -> &'static str {
        match self {
            InstallKind::AppImage(_) => "appimage",
            InstallKind::Setup(_) => "setup",
            InstallKind::System => pkg.format_key().unwrap_or("tarball"),
            // Windows and macOS builds are portable archives: the update is announced, not applied.
            _ if cfg!(windows) => "zip",
            _ if cfg!(target_os = "macos") => "dmg",
            _ => "tarball",
        }
    }
}

/// Work out the install kind from the running executable (already resolved) and the environment.
pub fn detect_kind(exe: &Path, appimage_env: Option<&str>, data_home: Option<&Path>) -> InstallKind {
    if let Some(a) = appimage_env.filter(|a| !a.is_empty()) {
        return InstallKind::AppImage(PathBuf::from(a));
    }
    if cfg!(windows)
        && let Some(dir) = setup_install_dir(exe, crate::paths::home().as_deref())
    {
        return InstallKind::Setup(dir);
    }
    if let Some(data) = data_home {
        let root = data.join("lumen-text");
        if let Ok(rel) = exe.strip_prefix(&root) {
            let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            if let [ver, bin, name] = parts.as_slice()
                && bin == "bin"
                && name == "lumen-text"
                && (ver == "current" || Version::parse(ver).is_some())
            {
                return InstallKind::User(root);
            }
        }
    }
    if exe.starts_with("/usr/bin") || exe.starts_with("/bin") || exe.starts_with("/usr/sbin") {
        return InstallKind::System;
    }
    InstallKind::Other
}

/// The install directory when `exe` is `lumen-text.exe` from the per-user Windows installer: the
/// directory holds the uninstaller (`unins000.exe`) and sits under the user's profile. A portable
/// zip has no uninstaller; an all-users install (Program Files) is left alone because updating it
/// would need elevation, so neither self-updates (they announce the release instead).
pub fn setup_install_dir(exe: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if !exe.file_name()?.to_string_lossy().eq_ignore_ascii_case("lumen-text.exe") || !dir.join("unins000.exe").is_file()
    {
        return None;
    }
    let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    if !lower(dir).starts_with(lower(home?)) {
        return None;
    }
    Some(dir.to_path_buf())
}

/// Command-line switches for a silent in-place upgrade with the setup (`relaunch`: start Lumen again afterwards).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn setup_args(dir: &Path, relaunch: bool) -> Vec<String> {
    let mut a: Vec<String> = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER", "/CLOSEAPPLICATIONS"]
        .map(String::from)
        .to_vec();
    a.push(format!("/DIR=\"{}\"", dir.display()));
    if relaunch {
        a.push("/RELAUNCH=1".into());
    }
    a
}

/// Start the verified setup silently, detached from this process (which is about to exit: the
/// installer closes any remaining Lumen through Restart Manager, replaces the files, keeps the
/// settings, and with `relaunch` starts the new Lumen).
#[cfg(windows)]
pub fn spawn_setup(setup: &Path, dir: &Path, relaunch: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let mut cmd = Command::new(setup);
    for a in setup_args(dir, relaunch) {
        cmd.raw_arg(a);
    }
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot start the installer {}: {e}", setup.display()))
}

#[cfg(not(windows))]
pub fn spawn_setup(_setup: &Path, _dir: &Path, _relaunch: bool) -> Result<(), String> {
    Err("the Windows installer only runs on Windows".into())
}

/// Package format of the running distribution (`/etc/os-release`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PkgFormat {
    Deb,
    Rpm,
    Zypper,
    Arch,
    Unknown,
}

impl PkgFormat {
    pub fn format_key(self) -> Option<&'static str> {
        match self {
            PkgFormat::Deb => Some("deb"),
            PkgFormat::Rpm | PkgFormat::Zypper => Some("rpm"),
            PkgFormat::Arch => Some("arch"),
            PkgFormat::Unknown => None,
        }
    }
}

pub fn pkg_format(os_release: &str) -> PkgFormat {
    let mut ids: Vec<String> = Vec::new();
    for line in os_release.lines() {
        if let Some((k, v)) = line.split_once('=')
            && (k == "ID" || k == "ID_LIKE")
        {
            ids.extend(v.trim().trim_matches('"').trim_matches('\'').split_whitespace().map(|s| s.to_lowercase()));
        }
    }
    let has = |names: &[&str]| ids.iter().any(|i| names.contains(&i.as_str()));
    if has(&["suse", "opensuse", "opensuse-leap", "opensuse-tumbleweed", "sles"]) {
        PkgFormat::Zypper
    } else if has(&["debian", "ubuntu", "linuxmint", "pop", "raspbian", "elementary", "kali", "neon", "zorin"]) {
        PkgFormat::Deb
    } else if has(&["fedora", "rhel", "centos", "rocky", "almalinux", "amzn", "ol", "nobara"]) {
        PkgFormat::Rpm
    } else if has(&["arch", "manjaro", "endeavouros", "cachyos", "garuda", "artix"]) {
        PkgFormat::Arch
    } else {
        PkgFormat::Unknown
    }
}

/// The command a user of a system-managed install runs to upgrade to the file at `url`.
pub fn install_command(fmt: PkgFormat, url: &str) -> Option<String> {
    let file = url.rsplit('/').next().unwrap_or(url);
    Some(match fmt {
        PkgFormat::Deb => format!("curl -fLO {url} && sudo apt install ./{file}"),
        PkgFormat::Rpm => format!("sudo dnf install {url}"),
        PkgFormat::Zypper => format!("sudo zypper install {url}"),
        PkgFormat::Arch => format!("sudo pacman -U {url}"),
        PkgFormat::Unknown => return None,
    })
}

// ───────────────────────────── decisions ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub url: String,
    /// Base64 `.minisig` as found in `latest.json`.
    pub signature: String,
    /// File name (last URL segment); it is part of the signed trusted comment.
    pub name: String,
}

/// A newer release and what can be done with it on this machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub pub_date: String,
    /// GitHub release page (release notes).
    pub page: String,
    /// Signed file for self-updating installs (AppImage / user layout).
    pub asset: Option<Asset>,
    /// Command to run for system-managed installs, when the distro is known.
    pub command: Option<String>,
}

fn url_ok(url: &str, https_only: bool) -> bool {
    url.starts_with("https://") || (!https_only && url.starts_with("http://"))
}

pub fn release_page(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
}

/// `None` when `m` is not newer than `current`.
pub fn evaluate(
    m: &Manifest,
    current: &str,
    kind: &InstallKind,
    arch: &str,
    pkg: PkgFormat,
    https_only: bool,
) -> Option<Release> {
    if !is_newer(&m.version, current) {
        return None;
    }
    let key = platform_key(arch, kind.format_key(pkg));
    let plat = m.platforms.get(&key).filter(|p| url_ok(&p.url, https_only));
    let asset = plat.filter(|p| kind.can_self_update() && !p.signature.is_empty()).map(|p| Asset {
        url: p.url.clone(),
        signature: p.signature.clone(),
        name: p.url.split(['?', '#']).next().unwrap_or(&p.url).rsplit('/').next().unwrap_or("").to_string(),
    });
    let command = match kind {
        InstallKind::System => plat.and_then(|p| install_command(pkg, &p.url)),
        _ => None,
    };
    Some(Release {
        version: m.version.trim_start_matches('v').to_string(),
        notes: m.notes.clone(),
        pub_date: m.pub_date.clone(),
        page: release_page(m.version.trim_start_matches('v')),
        asset,
        command,
    })
}

/// First meaningful lines of the (markdown) release notes, for the update card.
pub fn notes_summary(notes: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
    notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("**Full Changelog"))
        .map(|l| l.trim_start_matches('#').trim().trim_start_matches(['*', '-']).trim())
        .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("what's changed"))
        .take(max_lines)
        .map(|l| {
            if l.chars().count() > max_chars {
                let cut: String = l.chars().take(max_chars.saturating_sub(1)).collect();
                format!("{cut}…")
            } else {
                l.to_string()
            }
        })
        .collect()
}

// ───────────────────────────── signatures ─────────────────────────────

/// Standard base64 (padding optional, whitespace ignored).
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err("invalid base64".into()),
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// A `.minisig` given either as its text or as base64 of that text (as in `latest.json`).
fn signature_text(sig: &str) -> Result<String, String> {
    if sig.trim_start().starts_with("untrusted comment:") {
        return Ok(sig.to_string());
    }
    String::from_utf8(base64_decode(sig)?).map_err(|_| "signature is not text".to_string())
}

/// The trusted comment CI signs into every release file: ties the signature to one version and
/// one file name, so an old (validly signed) file can't be passed off as a newer release.
pub fn expected_comment(version: &str, file: &str) -> String {
    format!("lumen-text {version} {file}")
}

/// The keys in a public key text: one or more minisign `.pub` files concatenated (a comment line
/// plus a base64 line each) or bare base64 lines. Several keys allow rotating the signing key.
fn parse_public_keys(text: &str) -> Result<Vec<minisign_verify::PublicKey>, String> {
    let keys: Result<Vec<_>, _> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .map(minisign_verify::PublicKey::from_base64)
        .collect();
    let keys = keys.map_err(|e| format!("bad public key: {e}"))?;
    if keys.is_empty() {
        return Err("no public key".into());
    }
    Ok(keys)
}

/// Verify `path` against `sig` with any key of `pubkey` (see [`parse_public_keys`]), streaming.
/// `comment` (when given) must equal the signature's trusted comment.
pub fn verify_file(pubkey: &str, sig: &str, path: &Path, comment: Option<&str>) -> Result<(), String> {
    let keys = parse_public_keys(pubkey)?;
    let sig = minisign_verify::Signature::decode(&signature_text(sig)?).map_err(|e| format!("bad signature: {e}"))?;
    // The signature names the key that made it; only that one can verify it.
    let mut v = keys
        .iter()
        .find_map(|k| k.verify_stream(&sig).ok())
        .ok_or("signature rejected: it was not made by a key this app trusts")?;
    let mut f = fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("read error: {e}"))?;
        if n == 0 {
            break;
        }
        v.update(&buf[..n]);
    }
    v.finalize().map_err(|e| format!("signature does not match the file: {e}"))?;
    if let Some(want) = comment
        && sig.trusted_comment() != want
    {
        return Err(format!("signature is for another file or version ({:?})", sig.trusted_comment()));
    }
    Ok(())
}

// ───────────────────────────── download ─────────────────────────────

/// Download `url` to `dest` with curl (or wget when curl is missing). Aborts when the file grows
/// past `cap` bytes or the transfer takes longer than `timeout`. `progress` receives the size so far.
pub fn download(
    url: &str,
    dest: &Path,
    cap: u64,
    timeout: Duration,
    https_only: bool,
    progress: Option<&AtomicU64>,
) -> Result<u64, String> {
    if !url_ok(url, https_only) {
        return Err(format!("refusing to download {url}"));
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let _ = fs::remove_file(dest);
    let ua = format!("lumen-text/{}", current_version());
    let secs = timeout.as_secs().max(1).to_string();
    let mut curl: Vec<String> =
        ["-fsSL", "--connect-timeout", "15", "--max-time"].iter().map(|s| s.to_string()).collect();
    curl.extend([
        secs.clone(),
        "--max-filesize".into(),
        cap.to_string(),
        "--retry".into(),
        "1".into(),
        "-A".into(),
        ua.clone(),
    ]);
    if https_only {
        curl.extend(["--proto".into(), "=https".into(), "--proto-redir".into(), "=https".into()]);
    }
    curl.extend(["-o".into(), dest.display().to_string(), url.into()]);
    let mut wget: Vec<String> = vec!["-q".into(), "-T".into(), "20".into(), "-t".into(), "2".into(), "-U".into(), ua];
    if https_only {
        wget.push("--https-only".into());
    }
    wget.extend(["-O".into(), dest.display().to_string(), url.into()]);

    for (bin, args) in [("curl", curl), ("wget", wget)] {
        let mut child = match Command::new(bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot run {bin}: {e}")),
        };
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {}
                Err(e) => return Err(format!("{bin}: {e}")),
            }
            let len = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            if let Some(p) = progress {
                p.store(len, Ordering::Relaxed);
            }
            let fail = if len > cap {
                Some("download is larger than allowed")
            } else if start.elapsed() > timeout {
                Some("download timed out")
            } else {
                None
            };
            if let Some(why) = fail {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(dest);
                return Err(why.into());
            }
            std::thread::sleep(Duration::from_millis(80));
        };
        let len = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        if !status.success() {
            let _ = fs::remove_file(dest);
            return Err(format!(
                "{bin} failed ({})",
                status.code().map_or("signal".to_string(), |c| format!("exit {c}"))
            ));
        }
        if len == 0 || len > cap {
            let _ = fs::remove_file(dest);
            return Err(if len == 0 { "empty download".into() } else { "download is larger than allowed".into() });
        }
        if let Some(p) = progress {
            p.store(len, Ordering::Relaxed);
        }
        return Ok(len);
    }
    Err("neither curl nor wget is installed".into())
}

// ───────────────────────────── environment ─────────────────────────────

/// Everything the engine needs from the machine, so tests can build one by hand.
#[derive(Clone, Debug)]
pub struct Env {
    pub manifest_url: String,
    pub pubkey: String,
    pub https_only: bool,
    pub cache: PathBuf,
    pub arch: String,
    pub exe: PathBuf,
    pub kind: InstallKind,
    pub pkg: PkgFormat,
    pub current: String,
    /// `LUMEN_UPDATE_FORCE` / `LUMEN_UPDATE_URL`: behave like a release build even when local.
    pub forced: bool,
}

pub fn cache_dir() -> PathBuf {
    let base = crate::paths::cache_home().unwrap_or_else(|| PathBuf::from("."));
    base.join("lumen-text").join("update")
}

pub fn data_home() -> Option<PathBuf> {
    crate::paths::data_home()
}

fn pubkey_from_env(v: &str) -> String {
    if v.contains("untrusted comment") || v.trim().starts_with("RW") {
        v.to_string()
    } else {
        fs::read_to_string(v).unwrap_or_else(|_| v.to_string())
    }
}

impl Env {
    pub fn detect() -> Env {
        let url_override = std::env::var("LUMEN_UPDATE_URL").ok().filter(|v| !v.is_empty());
        let exe = std::env::current_exe().ok().map(|p| crate::paths::canonical(&p).unwrap_or(p)).unwrap_or_default();
        let appimage = std::env::var("APPIMAGE").ok();
        let kind = detect_kind(&exe, appimage.as_deref(), data_home().as_deref());
        let pkg = pkg_format(&fs::read_to_string("/etc/os-release").unwrap_or_default());
        Env {
            https_only: url_override.is_none(),
            forced: url_override.is_some() || std::env::var("LUMEN_UPDATE_FORCE").is_ok_and(|v| v == "1"),
            manifest_url: url_override.unwrap_or_else(|| DEFAULT_MANIFEST_URL.to_string()),
            pubkey: std::env::var("LUMEN_UPDATE_PUBKEY")
                .ok()
                .filter(|v| !v.is_empty())
                .map_or_else(|| PUBLIC_KEY.to_string(), |v| pubkey_from_env(&v)),
            cache: cache_dir(),
            arch: std::env::consts::ARCH.to_string(),
            exe,
            kind,
            pkg,
            current: current_version().to_string(),
        }
    }

    /// Should this process check for updates by itself (startup + every 4 h)? Local builds don't.
    pub fn auto_checks(&self) -> bool {
        self.forced || is_release_build(&self.current)
    }
}

// ───────────────────────────── pipeline ─────────────────────────────

/// Fetch and parse `latest.json`; `Some` when a newer release exists.
pub fn check(env: &Env) -> Result<Option<Release>, String> {
    let tmp = env.cache.join("latest.json.part");
    download(&env.manifest_url, &tmp, MANIFEST_CAP, MANIFEST_TIMEOUT, env.https_only, None)?;
    let text = fs::read_to_string(&tmp).map_err(|e| format!("cannot read latest.json: {e}"));
    let _ = fs::remove_file(&tmp);
    let m = parse_manifest(&text?)?;
    Ok(evaluate(&m, &env.current, &env.kind, &env.arch, env.pkg, env.https_only))
}

fn staged_path(env: &Env, rel: &Release, asset: &Asset) -> PathBuf {
    env.cache.join(&rel.version).join(&asset.name)
}

/// Download the release file into the cache and verify its signature (reusing a good earlier download).
pub fn stage(env: &Env, rel: &Release, progress: Option<&AtomicU64>) -> Result<PathBuf, String> {
    let asset = rel.asset.as_ref().ok_or("this release has no file for this installation")?;
    if asset.name.is_empty() || asset.name.contains(['/', '\\']) || asset.name.starts_with('.') {
        return Err("unsafe file name in latest.json".into());
    }
    // Old downloads (other versions) are not needed any more.
    if let Ok(rd) = fs::read_dir(&env.cache) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy() != rel.version.as_str() && e.path().is_dir() {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    let path = staged_path(env, rel, asset);
    let comment = expected_comment(&rel.version, &asset.name);
    if path.is_file() && verify_file(&env.pubkey, &asset.signature, &path, Some(&comment)).is_ok() {
        return Ok(path);
    }
    let part = path.with_extension("part");
    download(&asset.url, &part, ASSET_CAP, ASSET_TIMEOUT, env.https_only, progress)?;
    if let Err(e) = verify_file(&env.pubkey, &asset.signature, &part, Some(&comment)) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, &path).map_err(|e| format!("cannot move download: {e}"))?;
    Ok(path)
}

/// Put a verified file in place. Returns the executable to start on "Restart".
pub fn install(env: &Env, rel: &Release, staged: &Path) -> Result<PathBuf, String> {
    match &env.kind {
        InstallKind::AppImage(target) => apply_appimage(staged, target).map(|_| target.clone()),
        InstallKind::User(root) => {
            apply_user(staged, &rel.version, root, running_version_dir(&env.exe, root).as_deref())?;
            Ok(root.join("current/bin/lumen-text"))
        }
        // The installer replaces the running exe, so it runs only after this process exits
        // (`spawn_setup`, on "Restart" / `--update`); here the verified file just stays in the cache.
        InstallKind::Setup(_) => Ok(staged.to_path_buf()),
        _ => Err("this installation is managed by something else".into()),
    }
}

/// Download, verify and install in one go; the cache copy is removed afterwards.
pub fn update(env: &Env, rel: &Release, progress: Option<&AtomicU64>) -> Result<PathBuf, String> {
    let staged = stage(env, rel, progress)?;
    let res = install(env, rel, &staged);
    if res.is_ok()
        && !matches!(env.kind, InstallKind::Setup(_))
        && let Some(dir) = staged.parent()
    {
        let _ = fs::remove_dir_all(dir);
    }
    res
}

// ───────────────────────────── applying ─────────────────────────────

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

/// Replace the AppImage at `target` with `staged`, keeping the previous one as `<target>.old`.
/// The swap is a single `rename`, so `target` always exists; the running process keeps its inode.
pub fn apply_appimage(staged: &Path, target: &Path) -> Result<PathBuf, String> {
    let dir = target.parent().ok_or("bad AppImage path")?;
    let name = target.file_name().ok_or("bad AppImage path")?.to_string_lossy().into_owned();
    let new = dir.join(format!("{name}.new"));
    let old = dir.join(format!("{name}.old"));
    let _ = fs::remove_file(&new);
    if fs::rename(staged, &new).is_err() {
        fs::copy(staged, &new).map_err(|e| format!("cannot write next to the AppImage ({}): {e}", dir.display()))?;
    }
    if let Err(e) = set_mode(&new, 0o755) {
        let _ = fs::remove_file(&new);
        return Err(e);
    }
    if target.exists() {
        let _ = fs::remove_file(&old);
        if fs::hard_link(target, &old).is_err() {
            let _ = fs::copy(target, &old);
        }
    }
    fs::rename(&new, target).map_err(|e| {
        let _ = fs::remove_file(&new);
        format!("cannot replace {}: {e}", target.display())
    })?;
    Ok(old)
}

/// Name of the version directory the running executable lives in (`<root>/<version>/bin/lumen-text`).
fn running_version_dir(exe: &Path, root: &Path) -> Option<String> {
    let rel = exe.strip_prefix(root).ok()?;
    Some(rel.components().next()?.as_os_str().to_string_lossy().into_owned())
}

/// Atomically point `<root>/current` at `<root>/<version>` (relative symlink).
fn flip_current(root: &Path, version: &str) -> Result<(), String> {
    let tmp = root.join(".current.new");
    let _ = fs::remove_file(&tmp);
    crate::paths::symlink(Path::new(version), &tmp).map_err(|e| format!("symlink: {e}"))?;
    fs::rename(&tmp, root.join("current")).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot switch the current version: {e}")
    })
}

/// Keep the newest two version directories plus `keep` (the one in use and the previous `current`).
pub fn prune_versions(root: &Path, keep: &[String]) {
    let Ok(rd) = fs::read_dir(root) else { return };
    let mut dirs: Vec<(Version, String)> = rd
        .flatten()
        .filter(|e| e.path().is_dir() && !e.path().is_symlink())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            Version::parse(&n).map(|v| (v, n))
        })
        .collect();
    dirs.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (i, (_, name)) in dirs.iter().enumerate() {
        if i >= 2 && !keep.contains(name) {
            let _ = fs::remove_dir_all(root.join(name));
        }
    }
}

/// Install the release tarball as `<root>/<version>`, flip `current` to it and prune old versions.
pub fn apply_user(staged: &Path, version: &str, root: &Path, running: Option<&str>) -> Result<(), String> {
    if Version::parse(version).is_none() {
        return Err("bad version".into());
    }
    fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let tmp = root.join(format!(".{version}.tmp"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("mkdir: {e}"))?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(staged)
        .arg("-C")
        .arg(&tmp)
        .args(["--strip-components=1", "--no-same-owner"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("tar failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let bin = tmp.join("bin/lumen-text");
    if !bin.is_file() || !tmp.join("share/lumen-text/assets/language-manifest.json").is_file() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("the archive does not look like a lumen-text release".into());
    }
    set_mode(&bin, 0o755)?;
    let previous = fs::read_link(root.join("current")).ok().map(|p| p.to_string_lossy().into_owned());
    let dir = root.join(version);
    if fs::symlink_metadata(&dir).is_ok() {
        // Same version again (repair): the running copy, if it is that one, keeps working from its open files.
        let _ = fs::remove_dir_all(&dir);
    }
    fs::rename(&tmp, &dir).map_err(|e| format!("cannot install {}: {e}", dir.display()))?;
    flip_current(root, version)?;
    let mut keep: Vec<String> = vec![version.to_string()];
    keep.extend(previous);
    keep.extend(running.map(str::to_string));
    prune_versions(root, &keep);
    Ok(())
}

// ───────────────────────────── --install-user ─────────────────────────────

const ICON_SVG: &str = include_str!("../packaging/icons/lumen-text.svg");
/// Raster icons installed next to the SVG (`hicolor/<N>x<N>/apps/lumen-text.png`).
const ICON_PNGS: [(u32, &[u8]); 8] = [
    (16, include_bytes!("../packaging/icons/png/lumen-text-16.png")),
    (24, include_bytes!("../packaging/icons/png/lumen-text-24.png")),
    (32, include_bytes!("../packaging/icons/png/lumen-text-32.png")),
    (48, include_bytes!("../packaging/icons/png/lumen-text-48.png")),
    (64, include_bytes!("../packaging/icons/png/lumen-text-64.png")),
    (128, include_bytes!("../packaging/icons/png/lumen-text-128.png")),
    (256, include_bytes!("../packaging/icons/png/lumen-text-256.png")),
    (512, include_bytes!("../packaging/icons/png/lumen-text-512.png")),
];
const DESKTOP: &str = include_str!("../lumen-text.desktop");

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type()?.is_dir() {
            // Licensed HugeIcons masks live inside the executable, never as loose files.
            if matches!(e.file_name().to_str(), Some("hugeicons" | "hugeicons_bulk")) {
                continue;
            }
            copy_tree(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// The `.desktop` entry with an absolute `Exec` (works from launchers whose PATH lacks ~/.local/bin).
pub fn desktop_entry(exec: &Path) -> String {
    let mut out = String::new();
    for line in DESKTOP.lines() {
        if line.starts_with("Exec=") {
            out.push_str(&format!("Exec={} %F\n", exec.display()));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

pub struct Installed {
    pub bin_link: PathBuf,
    pub desktop: PathBuf,
    pub version_dir: PathBuf,
}

/// Copy the running editor (binary + assets) into the user layout and register it with the desktop.
pub fn install_user(exe: &Path, assets: &Path, version: &str, home: &Path, data: &Path) -> Result<Installed, String> {
    if Version::parse(version).is_none() {
        return Err(format!("cannot install version {version:?}"));
    }
    if !assets.join("language-manifest.json").is_file() {
        return Err(format!("assets not found at {}", assets.display()));
    }
    let root = data.join("lumen-text");
    fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let tmp = root.join(format!(".{version}.tmp"));
    let _ = fs::remove_dir_all(&tmp);
    let stage = || -> Result<(), String> {
        fs::create_dir_all(tmp.join("bin")).map_err(|e| e.to_string())?;
        fs::copy(exe, tmp.join("bin/lumen-text")).map_err(|e| format!("cannot copy the executable: {e}"))?;
        set_mode(&tmp.join("bin/lumen-text"), 0o755)?;
        copy_tree(assets, &tmp.join("share/lumen-text/assets")).map_err(|e| format!("cannot copy assets: {e}"))?;
        Ok(())
    };
    if let Err(e) = stage() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let previous = fs::read_link(root.join("current")).ok().map(|p| p.to_string_lossy().into_owned());
    let dir = root.join(version);
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&tmp, &dir).map_err(|e| format!("cannot install {}: {e}", dir.display()))?;
    flip_current(&root, version)?;

    // ~/.local/bin/lumen-text -> <data>/lumen-text/current/bin/lumen-text
    let bin_dir = home.join(".local/bin");
    fs::create_dir_all(&bin_dir).map_err(|e| format!("cannot create {}: {e}", bin_dir.display()))?;
    let link = bin_dir.join("lumen-text");
    let tmp_link = bin_dir.join(".lumen-text.new");
    let _ = fs::remove_file(&tmp_link);
    crate::paths::symlink(&root.join("current/bin/lumen-text"), &tmp_link).map_err(|e| format!("symlink: {e}"))?;
    fs::rename(&tmp_link, &link).map_err(|e| format!("cannot create {}: {e}", link.display()))?;

    let desktop = register_desktop(data, &link)?;
    let mut keep = vec![version.to_string()];
    keep.extend(previous);
    prune_versions(&root, &keep);
    Ok(Installed { bin_link: link, desktop, version_dir: dir })
}

/// Write the launcher entry and the icons (SVG + PNGs) under `data` and refresh the desktop caches.
fn register_desktop(data: &Path, link: &Path) -> Result<PathBuf, String> {
    let apps = data.join("applications");
    let icons = data.join("icons/hicolor/scalable/apps");
    fs::create_dir_all(&apps)
        .and_then(|_| fs::create_dir_all(&icons))
        .map_err(|e| format!("cannot create desktop directories: {e}"))?;
    let desktop = apps.join("lumen-text.desktop");
    fs::write(&desktop, desktop_entry(link)).map_err(|e| format!("cannot write {}: {e}", desktop.display()))?;
    fs::write(icons.join("lumen-text.svg"), ICON_SVG).map_err(|e| format!("cannot write the icon: {e}"))?;
    for (n, bytes) in ICON_PNGS {
        let dir = data.join(format!("icons/hicolor/{n}x{n}/apps"));
        fs::create_dir_all(&dir)
            .and_then(|_| fs::write(dir.join("lumen-text.png"), bytes))
            .map_err(|e| format!("cannot write the {n}px icon: {e}"))?;
    }
    for (cmd, args) in [
        ("update-desktop-database", vec![apps.display().to_string()]),
        ("gtk-update-icon-cache", vec!["-f".into(), "-t".into(), data.join("icons/hicolor").display().to_string()]),
    ] {
        let _ = Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    Ok(desktop)
}

/// A user install updated by an older build keeps that build's launcher and icon. Once per version,
/// the running build rewrites them (no-op for system packages, AppImages and dev builds).
pub fn refresh_user_integration() {
    let (Some(data), Ok(exe)) = (data_home(), std::env::current_exe()) else { return };
    let root = data.join("lumen-text");
    let Ok(exe) = crate::paths::canonical(&exe) else { return };
    let Ok(root_c) = crate::paths::canonical(&root) else { return };
    if !exe.starts_with(&root_c) {
        return;
    }
    let marker = root.join(".integration");
    let version = current_version();
    if fs::read_to_string(&marker).is_ok_and(|v| v.trim() == version) {
        return;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let link = home.join(".local/bin/lumen-text");
    if register_desktop(&data, &link).is_ok() {
        let _ = fs::write(&marker, version);
    }
}

// ───────────────────────────── command line ─────────────────────────────

/// `--install-user`. Returns the process exit code.
pub fn cli_install_user() -> i32 {
    let (Some(home), Some(data)) = (std::env::var_os("HOME").map(PathBuf::from), data_home()) else {
        eprintln!("HOME is not set");
        return 1;
    };
    let exe = std::env::current_exe().ok().map(|p| crate::paths::canonical(&p).unwrap_or(p)).unwrap_or_default();
    match install_user(&exe, &crate::config::assets_dir(), current_version(), &home, &data) {
        Ok(i) => {
            println!("Installed lumen-text {} in {}", current_version(), i.version_dir.display());
            println!("  command:  {}", i.bin_link.display());
            println!("  launcher: {}", i.desktop.display());
            let bin_dir = home.join(".local/bin");
            if !std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == bin_dir)) {
                println!("Note: {} is not in PATH; the launcher entry works anyway.", bin_dir.display());
            }
            println!("Updates are now automatic (setting \"updates\"; try: lumen-text --check-update).");
            0
        }
        Err(e) => {
            eprintln!("install failed: {e}");
            1
        }
    }
}

/// `--check-update` (and `--update` with `apply`). Exit code: 0 up to date or updated, 10 update
/// available (not installed), 1 error.
pub fn cli_check(apply: bool) -> i32 {
    let env = Env::detect();
    println!("lumen-text {} ({})", env.current, env.kind.name());
    let rel = match check(&env) {
        Ok(None) => {
            println!("Up to date.");
            return 0;
        }
        Ok(Some(r)) => r,
        Err(e) => {
            eprintln!("Update check failed: {e}");
            return 1;
        }
    };
    println!("Update available: {} ({})", rel.version, rel.page);
    if !apply {
        return 10;
    }
    if !env.kind.can_self_update() || rel.asset.is_none() {
        match (&rel.command, env.kind.can_self_update()) {
            (Some(c), _) => println!("This installation is managed by your package manager. Run:\n  {c}"),
            (None, false) => println!(
                "This installation can't update itself. Download {} {}.",
                rel.page,
                if cfg!(windows) {
                    "and install lumen-text-<version>-windows-x86_64-setup.exe for automatic updates"
                } else {
                    "or run `lumen-text --install-user` from it for automatic updates"
                }
            ),
            _ => println!("This release has no file for this installation: {}", rel.page),
        }
        return 10;
    }
    match update(&env, &rel, None) {
        Ok(setup) if matches!(env.kind, InstallKind::Setup(_)) => {
            let InstallKind::Setup(dir) = &env.kind else { unreachable!() };
            match spawn_setup(&setup, dir, false) {
                Ok(()) => {
                    println!(
                        "Installing {v} in the background; Lumen {v} replaces this one in a few seconds.",
                        v = rel.version
                    );
                    0
                }
                Err(e) => {
                    eprintln!("Update failed: {e}");
                    1
                }
            }
        }
        Ok(exe) => {
            println!("Updated to {}. Start it with: {}", rel.version, exe.display());
            0
        }
        Err(e) => {
            eprintln!("Update failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("upd-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    // Fixture: signed with a throwaway key (never used for releases).
    const TEST_PUB: &str = "untrusted comment: minisign public key 88D0F3FF8CEE5682\nRWSCVu6M//PQiCeBeC3PvK1EtHMkyjO+RgwsMyOlc+7LpGdIDPm28RiF\n";
    const TEST_SIG_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIG1pbmlzaWduIHNlY3JldCBrZXkKUlVTQ1Z1Nk0vL1BRaUFGdkUyTXgzcU5uNTNQODN2T2xTOEJKWVAxUFA5TW1CSFk2aXNheHBrbWtlZVQ0SzhhTk1Wb2tSQ0h4eGZwQm1PeFoweDU5Q05qSDV5UGgyUU1jZlFJPQp0cnVzdGVkIGNvbW1lbnQ6IGx1bWVuLXRleHQgMjAyNi4xMS4xMC4xIHBheWxvYWQuYmluClhwV09Xdmd6YmEzWGZEcHdkNmF2ejBSb2I4YmVqMUFXWTZpNVR0RUhuNXZoWktiNmlFbDFmL3U4bk1IWjkwcDRzQ3Jld0tRWHoxcHJsU3hUZkJvaURnPT0K";

    #[test]
    fn calver_compares_numerically_per_component() {
        assert!(is_newer("2026.11.10.1", "2026.11.9.4"));
        assert!(is_newer("2026.11.10.2", "2026.11.10.1"));
        assert!(is_newer("2026.12.1.1", "2026.11.30.9"), "month compares as a number, not text");
        assert!(is_newer("2026.11.10.10", "2026.11.10.9"), "10 > 9 numerically");
        assert!(is_newer("2027.1.1.1", "2026.12.31.99"));
        assert!(!is_newer("2026.11.10.1", "2026.11.10.1"));
        assert!(!is_newer("2026.11.9.1", "2026.11.10.1"), "never downgrade");
        assert!(is_newer("v2026.11.10.1", "2026.11.10"), "v prefix ok");
    }

    #[test]
    fn missing_build_number_counts_as_zero() {
        assert!(is_newer("2026.11.9.1", "2026.11.9"));
        assert!(!is_newer("2026.11.9.0", "2026.11.9"));
        assert!(!is_newer("2026.11.9", "2026.11.9.0"));
        assert_eq!(Version::parse("2026.11.9"), Version::parse("2026.11.9.0"));
    }

    #[test]
    fn bad_versions_are_never_newer() {
        for bad in ["", "abc", "1.2", "1.2.3.4.5", "2026.11.x.1", "2026..9", "-1.2.3", "2026.11.10.1-beta"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
            assert!(!is_newer(bad, "2026.1.1.1"));
            assert!(!is_newer("2999.1.1.1", bad));
        }
    }

    #[test]
    fn local_builds_are_recognised_by_the_missing_build_number() {
        assert!(is_release_build("2026.11.10.1"));
        assert!(!is_release_build("2026.11.9"));
        assert!(!is_release_build("0.1.0"));
    }

    #[test]
    fn schedule_checks_after_delay_then_every_four_hours() {
        let t0 = Instant::now();
        let mut s = Schedule::new(t0, STARTUP_DELAY);
        assert!(!s.due(t0 + Duration::from_secs(19)));
        assert!(s.due(t0 + Duration::from_secs(20)));
        let t1 = t0 + Duration::from_secs(21);
        s.on_success(t1);
        assert!(!s.due(t1 + CHECK_EVERY - Duration::from_secs(1)));
        assert!(s.due(t1 + CHECK_EVERY));
    }

    #[test]
    fn failures_back_off_but_never_stop() {
        assert_eq!(backoff(1), Duration::from_secs(300));
        assert_eq!(backoff(2), Duration::from_secs(600));
        assert_eq!(backoff(3), Duration::from_secs(1200));
        assert_eq!(backoff(50), CHECK_EVERY, "capped at the normal interval");
        let t0 = Instant::now();
        let mut s = Schedule::new(t0, Duration::ZERO);
        for _ in 0..30 {
            s.on_failure(t0);
            assert!(s.next() > t0 && s.next() <= t0 + CHECK_EVERY, "always schedules another try, within 4 h");
        }
        s.on_success(t0);
        assert_eq!(s.failures(), 0);
        assert_eq!(s.next(), t0 + CHECK_EVERY);
    }

    #[test]
    fn each_version_is_announced_once() {
        let mut a = Announcer::default();
        assert!(a.should_announce("2026.11.10.1"));
        assert!(!a.should_announce("2026.11.10.1"), "same version stays quiet");
        assert!(!a.should_announce("2026.11.9.7"), "older stays quiet");
        assert!(a.should_announce("2026.11.10.2"), "a newer one is announced again");
        assert!(!a.should_announce("garbage"));
    }

    const MANIFEST: &str = r###"{
        "version": "2026.11.10.1",
        "notes": "## What's Changed\n* Add updates by @x in #1\n* Fix a thing\n\n**Full Changelog**: https://x",
        "pub_date": "2026-11-10T12:00:00Z",
        "platforms": {
            "linux-x86_64-appimage": {"url": "https://h/lumen-text-2026.11.10.1-x86_64.AppImage", "signature": "c2ln"},
            "linux-x86_64-tarball": {"url": "https://h/lumen-text-2026.11.10.1-linux-x86_64.tar.gz", "signature": "c2ln"},
            "linux-x86_64-deb": {"url": "https://h/lumen-text_2026.11.10.1-1_amd64.deb", "signature": ""},
            "linux-x86_64-rpm": {"url": "https://h/lumen-text-2026.11.10.1-1.x86_64.rpm"},
            "linux-x86_64-arch": {"url": "https://h/lumen-text-bin-2026.11.10.1-1-x86_64.pkg.tar.zst"}
        }
    }"###;

    #[test]
    fn manifest_parses_and_rejects_garbage() {
        let m = parse_manifest(MANIFEST).unwrap();
        assert_eq!(m.version, "2026.11.10.1");
        assert_eq!(m.platforms.len(), 5);
        assert_eq!(m.platforms["linux-x86_64-rpm"].signature, "", "signature is optional for informational entries");
        assert!(parse_manifest("not json").is_err());
        assert!(parse_manifest(r#"{"version": "banana"}"#).is_err());
        assert!(parse_manifest(r#"{"notes": "no version"}"#).is_err());
        assert!(parse_manifest(r#"{"version": "2026.1.1.1"}"#).unwrap().platforms.is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn evaluate_picks_the_file_for_the_install_kind() {
        let m = parse_manifest(MANIFEST).unwrap();
        let eval = |kind: &InstallKind, pkg, cur: &str| evaluate(&m, cur, kind, "x86_64", pkg, true);
        let appimage = InstallKind::AppImage(PathBuf::from("/home/u/lumen.AppImage"));
        let r = eval(&appimage, PkgFormat::Deb, "2026.11.9").unwrap();
        assert_eq!(r.version, "2026.11.10.1");
        assert_eq!(r.asset.as_ref().unwrap().name, "lumen-text-2026.11.10.1-x86_64.AppImage");
        assert_eq!(r.page, "https://github.com/castrogusttavo/lumen-text/releases/tag/v2026.11.10.1");

        let user = InstallKind::User(PathBuf::from("/home/u/.local/share/lumen-text"));
        assert_eq!(
            eval(&user, PkgFormat::Deb, "2026.11.9.3").unwrap().asset.unwrap().name,
            "lumen-text-2026.11.10.1-linux-x86_64.tar.gz"
        );

        let sys = eval(&InstallKind::System, PkgFormat::Deb, "2026.11.9.3").unwrap();
        assert!(sys.asset.is_none(), "system installs never self-modify");
        assert_eq!(
            sys.command.as_deref(),
            Some(
                "curl -fLO https://h/lumen-text_2026.11.10.1-1_amd64.deb && sudo apt install ./lumen-text_2026.11.10.1-1_amd64.deb"
            )
        );
        assert_eq!(
            eval(&InstallKind::System, PkgFormat::Arch, "2026.11.9").unwrap().command.unwrap(),
            "sudo pacman -U https://h/lumen-text-bin-2026.11.10.1-1-x86_64.pkg.tar.zst"
        );
        assert_eq!(
            eval(&InstallKind::System, PkgFormat::Rpm, "2026.11.9").unwrap().command.unwrap(),
            "sudo dnf install https://h/lumen-text-2026.11.10.1-1.x86_64.rpm"
        );
        assert!(eval(&InstallKind::System, PkgFormat::Unknown, "2026.11.9").unwrap().command.is_none());

        assert!(eval(&appimage, PkgFormat::Deb, "2026.11.10.1").is_none(), "up to date");
        assert!(eval(&appimage, PkgFormat::Deb, "2026.12.1.1").is_none(), "local build newer than the release");
        assert!(
            evaluate(&m, "2026.11.9", &appimage, "aarch64", PkgFormat::Deb, true).unwrap().asset.is_none(),
            "no aarch64 file: still announced, not installable"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn http_urls_are_ignored_unless_testing() {
        let m = parse_manifest(&MANIFEST.replace("https://h/", "http://h/")).unwrap();
        let kind = InstallKind::AppImage(PathBuf::from("/x.AppImage"));
        assert!(evaluate(&m, "2026.11.9", &kind, "x86_64", PkgFormat::Deb, true).unwrap().asset.is_none());
        assert!(evaluate(&m, "2026.11.9", &kind, "x86_64", PkgFormat::Deb, false).unwrap().asset.is_some());
    }

    #[test]
    fn install_kind_comes_from_the_executable() {
        let data = PathBuf::from("/home/u/.local/share");
        let k = |exe: &str, ai: Option<&str>| detect_kind(Path::new(exe), ai, Some(&data));
        assert_eq!(
            k("/tmp/.mount_x/usr/bin/lumen-text", Some("/home/u/Apps/L.AppImage")),
            InstallKind::AppImage("/home/u/Apps/L.AppImage".into())
        );
        assert_eq!(
            k("/home/u/.local/share/lumen-text/2026.11.10.1/bin/lumen-text", None),
            InstallKind::User("/home/u/.local/share/lumen-text".into())
        );
        assert_eq!(
            k("/home/u/.local/share/lumen-text/current/bin/lumen-text", None),
            InstallKind::User("/home/u/.local/share/lumen-text".into())
        );
        assert_eq!(k("/home/u/.local/share/lumen-text/tsgo/bin/lumen-text", None), InstallKind::Other);
        assert_eq!(k("/usr/bin/lumen-text", None), InstallKind::System);
        assert_eq!(k("/usr/local/bin/lumen-text", None), InstallKind::Other);
        assert_eq!(k("/home/u/www/lumen-text/target/release/lumen-text", None), InstallKind::Other);
        assert_eq!(k("/usr/bin/lumen-text", Some("")), InstallKind::System, "empty APPIMAGE is ignored");
    }

    #[test]
    fn distro_decides_the_package_command() {
        let f = |s: &str| pkg_format(s);
        assert_eq!(f("NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n"), PkgFormat::Deb);
        assert_eq!(f("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"), PkgFormat::Deb);
        assert_eq!(f("ID=fedora\n"), PkgFormat::Rpm);
        assert_eq!(f("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n"), PkgFormat::Rpm);
        assert_eq!(f("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n"), PkgFormat::Zypper);
        assert_eq!(f("ID=arch\n"), PkgFormat::Arch);
        assert_eq!(f("ID=manjaro\nID_LIKE=arch\n"), PkgFormat::Arch);
        assert_eq!(f("ID=endeavouros\nID_LIKE=arch\n"), PkgFormat::Arch);
        assert_eq!(f("ID=gentoo\n"), PkgFormat::Unknown);
        assert_eq!(f(""), PkgFormat::Unknown);
        assert_eq!(
            install_command(PkgFormat::Zypper, "https://h/x.rpm").unwrap(),
            "sudo zypper install https://h/x.rpm"
        );
    }

    #[test]
    fn notes_summary_skips_headings_and_chrome() {
        let m = parse_manifest(MANIFEST).unwrap();
        assert_eq!(notes_summary(&m.notes, 5, 80), vec!["Add updates by @x in #1", "Fix a thing"]);
        assert_eq!(notes_summary(&m.notes, 1, 80).len(), 1);
        assert_eq!(notes_summary("a very long line indeed", 1, 8), vec!["a very …"]);
        assert!(notes_summary("", 3, 10).is_empty());
    }

    #[test]
    fn base64_roundtrip_of_the_signature_fixture() {
        let text = String::from_utf8(base64_decode(TEST_SIG_B64).unwrap()).unwrap();
        assert!(text.starts_with("untrusted comment: signature from minisign secret key\n"));
        assert!(text.contains("trusted comment: lumen-text 2026.11.10.1 payload.bin"));
        assert!(base64_decode("a$b").is_err());
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi", "padding optional");
    }

    #[test]
    fn signatures_verify_and_tampering_is_caught() {
        let d = scratch("sig");
        let f = d.join("payload.bin");
        fs::write(&f, "lumen test payload\n").unwrap();
        let ok = expected_comment("2026.11.10.1", "payload.bin");
        verify_file(TEST_PUB, TEST_SIG_B64, &f, Some(&ok)).unwrap();
        verify_file(TEST_PUB, TEST_SIG_B64, &f, None).unwrap();
        // The raw .minisig text works too, and a bare base64 public key.
        let raw = String::from_utf8(base64_decode(TEST_SIG_B64).unwrap()).unwrap();
        verify_file(TEST_PUB.lines().nth(1).unwrap(), &raw, &f, None).unwrap();

        fs::write(&f, "lumen test payload!\n").unwrap();
        assert!(verify_file(TEST_PUB, TEST_SIG_B64, &f, None).is_err(), "modified file");
        fs::write(&f, "lumen test payload\n").unwrap();
        // Right file, but the signature says another version or name.
        assert!(
            verify_file(TEST_PUB, TEST_SIG_B64, &f, Some(&expected_comment("2026.11.11.1", "payload.bin"))).is_err()
        );
        assert!(verify_file(TEST_PUB, TEST_SIG_B64, &f, Some(&expected_comment("2026.11.10.1", "other.bin"))).is_err());
        // Wrong key (the real release key is not the test key).
        assert!(verify_file(PUBLIC_KEY, TEST_SIG_B64, &f, None).is_err());
        assert!(verify_file(TEST_PUB, "not a signature", &f, None).is_err());
        assert!(verify_file("garbage", TEST_SIG_B64, &f, None).is_err());
        assert!(verify_file(TEST_PUB, TEST_SIG_B64, &d.join("missing"), None).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn embedded_public_key_is_a_valid_minisign_key() {
        assert!(!parse_public_keys(PUBLIC_KEY).unwrap().is_empty());
    }

    #[test]
    fn several_trusted_keys_allow_rotation() {
        let d = scratch("rot");
        let f = d.join("payload.bin");
        fs::write(&f, "lumen test payload\n").unwrap();
        // New key first, old (test) key second: a signature from either verifies.
        let both = format!("{PUBLIC_KEY}{TEST_PUB}");
        verify_file(&both, TEST_SIG_B64, &f, None).unwrap();
        assert_eq!(parse_public_keys(&both).unwrap().len(), 2);
        // Without the signer's key nothing verifies.
        assert!(verify_file(PUBLIC_KEY, TEST_SIG_B64, &f, None).is_err());
        assert!(parse_public_keys("").is_err());
        assert!(parse_public_keys("untrusted comment: x\nnot-a-key\n").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn download_rejects_non_https_and_reports_missing_hosts() {
        let d = scratch("dl");
        assert!(download("http://example.invalid/x", &d.join("x"), 10, Duration::from_secs(1), true, None).is_err());
        assert!(download("file:///etc/passwd", &d.join("y"), 10, Duration::from_secs(1), false, None).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn appimage_swap_keeps_the_previous_file() {
        let d = scratch("ai");
        let target = d.join("Lumen.AppImage");
        fs::write(&target, "old").unwrap();
        let staged = d.join("staged");
        fs::write(&staged, "new").unwrap();
        let old = apply_appimage(&staged, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::read_to_string(&old).unwrap(), "old");
        assert_eq!(old.file_name().unwrap(), "Lumen.AppImage.old");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(!d.join("Lumen.AppImage.new").exists());
        // A second update replaces .old with the previous version.
        fs::write(&staged, "newer").unwrap();
        apply_appimage(&staged, &target).unwrap();
        assert_eq!(fs::read_to_string(&old).unwrap(), "new");
        let _ = fs::remove_dir_all(&d);
    }

    fn fake_release_tar(dir: &Path, name: &str, marker: &str) -> PathBuf {
        let top = dir.join(format!("pkg-{marker}")).join(format!("lumen-text-{name}-linux-x86_64"));
        fs::create_dir_all(top.join("bin")).unwrap();
        fs::create_dir_all(top.join("share/lumen-text/assets")).unwrap();
        fs::write(top.join("bin/lumen-text"), marker).unwrap();
        fs::write(top.join("share/lumen-text/assets/language-manifest.json"), "{}").unwrap();
        let tar = dir.join(format!("{name}.tar.gz"));
        let st = Command::new("tar")
            .arg("-czf")
            .arg(&tar)
            .arg("-C")
            .arg(top.parent().unwrap())
            .arg(top.file_name().unwrap())
            .status()
            .unwrap();
        assert!(st.success());
        tar
    }

    #[test]
    fn user_layout_flips_current_and_keeps_two_versions() {
        let d = scratch("user");
        let root = d.join("data/lumen-text");
        for (i, v) in ["2026.11.1.1", "2026.11.2.1", "2026.11.3.1"].iter().enumerate() {
            let tar = fake_release_tar(&d, v, &format!("bin-{i}"));
            apply_user(&tar, v, &root, None).unwrap();
            assert_eq!(fs::read_link(root.join("current")).unwrap(), Path::new(v));
            assert_eq!(fs::read_to_string(root.join("current/bin/lumen-text")).unwrap(), format!("bin-{i}"));
        }
        let mut dirs: Vec<String> =
            fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        dirs.sort();
        assert_eq!(dirs, vec!["2026.11.2.1", "2026.11.3.1", "current"], "oldest pruned, previous kept");
        // The running version's directory is never pruned, even when it is old.
        let tar = fake_release_tar(&d, "2026.11.4.1", "bin-3");
        apply_user(&tar, "2026.11.4.1", &root, Some("2026.11.2.1")).unwrap();
        assert!(
            root.join("2026.11.2.1").is_dir() && root.join("2026.11.3.1").is_dir() && root.join("2026.11.4.1").is_dir()
        );
        assert!(!root.join("2026.11.1.1").exists());
        // Garbage archives leave everything untouched.
        let bad = d.join("bad.tar.gz");
        fs::write(&bad, "not a tarball").unwrap();
        assert!(apply_user(&bad, "2026.11.5.1", &root, None).is_err());
        assert_eq!(fs::read_link(root.join("current")).unwrap(), Path::new("2026.11.4.1"));
        assert!(!root.join(".2026.11.5.1.tmp").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn install_user_creates_layout_launcher_and_icon() {
        let d = scratch("inst");
        let (home, data) = (d.join("home"), d.join("home/.local/share"));
        let exe = d.join("lumen-text");
        fs::write(&exe, "#!/bin/sh\necho hi\n").unwrap();
        let assets = d.join("assets");
        fs::create_dir_all(assets.join("themes")).unwrap();
        fs::write(assets.join("language-manifest.json"), "{}").unwrap();
        fs::write(assets.join("themes/a.json"), "{}").unwrap();
        let i = install_user(&exe, &assets, "2026.11.9", &home, &data).unwrap();
        assert_eq!(fs::read_link(&i.bin_link).unwrap(), data.join("lumen-text/current/bin/lumen-text"));
        assert!(i.bin_link.is_symlink() && i.bin_link.exists(), "symlink resolves");
        assert!(data.join("lumen-text/2026.11.9/share/lumen-text/assets/themes/a.json").is_file());
        let desktop = fs::read_to_string(&i.desktop).unwrap();
        assert!(desktop.contains(&format!("Exec={} %F", i.bin_link.display())), "{desktop}");
        assert!(desktop.contains("Icon=lumen-text") && desktop.contains("[Desktop Entry]"));
        assert!(data.join("icons/hicolor/scalable/apps/lumen-text.svg").is_file());
        assert!(data.join("icons/hicolor/256x256/apps/lumen-text.png").is_file());
        assert!(data.join("icons/hicolor/16x16/apps/lumen-text.png").is_file());
        // The layout is recognised as a user install afterwards, and re-running is fine.
        let resolved = data.join("lumen-text/2026.11.9/bin/lumen-text");
        assert_eq!(detect_kind(&resolved, None, Some(&data)), InstallKind::User(data.join("lumen-text")));
        install_user(&exe, &assets, "2026.11.9", &home, &data).unwrap();
        assert!(install_user(&exe, &d.join("nope"), "2026.11.9", &home, &data).is_err());
        assert!(install_user(&exe, &assets, "dev", &home, &data).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    fn tmpd(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lumen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn register_desktop_rewrites_stale_icons() {
        let data = std::env::temp_dir().join(format!("lumen-integ-{}", std::process::id()));
        let _ = fs::remove_dir_all(&data);
        let icon = data.join("icons/hicolor/scalable/apps");
        fs::create_dir_all(&icon).unwrap();
        fs::write(icon.join("lumen-text.svg"), "old").unwrap();
        let d = register_desktop(&data, Path::new("/home/x/.local/bin/lumen-text")).unwrap();
        assert_eq!(fs::read_to_string(&d).unwrap().matches("Exec=/home/x/.local/bin/lumen-text %F").count(), 1);
        assert_eq!(fs::read(icon.join("lumen-text.svg")).unwrap(), ICON_SVG.as_bytes());
        assert!(data.join("icons/hicolor/256x256/apps/lumen-text.png").is_file());
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn setup_install_is_recognised_by_its_uninstaller_under_the_user_profile() {
        let home = tmpd("setup-home");
        let dir = home.join("AppData/Local/Programs/Lumen");
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("lumen-text.exe");
        fs::write(&exe, b"x").unwrap();
        assert_eq!(setup_install_dir(&exe, Some(&home)), None, "portable copy: no uninstaller");
        fs::write(dir.join("unins000.exe"), b"x").unwrap();
        assert_eq!(setup_install_dir(&exe, Some(&home)), Some(dir.clone()));
        assert_eq!(
            setup_install_dir(&exe, Some(&tmpd("setup-other"))),
            None,
            "outside the profile (all users): not self-updated"
        );
        assert_eq!(setup_install_dir(&exe, None), None);
        assert_eq!(setup_install_dir(&dir.join("ghp-lsp.exe"), Some(&home)), None);
    }

    #[test]
    fn setup_kind_updates_from_the_signed_setup_entry() {
        let key = platform_key("x86_64", "setup");
        let zip = platform_key("x86_64", "zip");
        let json = format!(
            r#"{{"version":"2026.11.10.2","notes":"n","pub_date":"d","platforms":{{
              "{key}":{{"url":"https://h/lumen-text-2026.11.10.2-windows-x86_64-setup.exe","signature":"c2ln"}},
              "{zip}":{{"url":"https://h/lumen-text-2026.11.10.2-windows-x86_64.zip","signature":"c2ln"}}}}}}"#
        );
        let m = parse_manifest(&json).unwrap();
        let setup = InstallKind::Setup(PathBuf::from("C:/Users/u/AppData/Local/Programs/Lumen"));
        assert!(setup.can_self_update());
        let r = evaluate(&m, "2026.11.10.1", &setup, "x86_64", PkgFormat::Unknown, true).unwrap();
        assert_eq!(r.asset.unwrap().name, "lumen-text-2026.11.10.2-windows-x86_64-setup.exe");
        assert!(
            evaluate(&m, "2026.11.10.2", &setup, "x86_64", PkgFormat::Unknown, true).is_none(),
            "same version: nothing to do"
        );
        assert!(evaluate(&m, "2026.11.10.3", &setup, "x86_64", PkgFormat::Unknown, true).is_none(), "never downgrade");
        // A portable copy keeps announcing only.
        let r = evaluate(&m, "2026.11.10.1", &InstallKind::Other, "x86_64", PkgFormat::Unknown, true).unwrap();
        assert!(r.asset.is_none());
    }

    #[test]
    fn setup_switches_are_silent_per_user_and_target_the_install_dir() {
        let dir = Path::new("C:\\Users\\u\\AppData\\Local\\Programs\\Lumen");
        let a = setup_args(dir, false);
        for f in ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER"] {
            assert!(a.iter().any(|x| x == f), "{f}");
        }
        assert!(a.iter().any(|x| x.starts_with("/DIR=\"") && x.ends_with('"')));
        assert!(!a.iter().any(|x| x.starts_with("/RELAUNCH")));
        assert!(setup_args(dir, true).iter().any(|x| x == "/RELAUNCH=1"));
    }
}
