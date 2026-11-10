//! System clipboard and primary selection through external tools, with an in-process fallback:
//! `wl-copy`/`wl-paste` on Wayland, `xclip` or `xsel` on X11. Shelling out keeps us free of
//! windowing-protocol dependencies and works on compositors without data-control. macOS uses
//! `pbcopy`/`pbpaste`; Windows talks to the clipboard API through `arboard` (text only there).

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// MIME type most file managers and toolkits read and write for copied files.
pub const URI_LIST: &str = "text/uri-list";
/// GNOME Files / Nemo / Caja / Thunar flavor: `copy` or `cut` on the first line, then the URIs.
pub const GNOME_FILES: &str = "x-special/gnome-copied-files";

/// Files copied (or cut) in a program, as they travel through the system clipboard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileList {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

/// `file://` URI of an absolute path (everything but unreserved characters and `/` is %-encoded).
pub fn path_to_uri(p: &Path) -> String {
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(p.as_os_str());
    #[cfg(not(unix))]
    let bytes = p.to_string_lossy().replace('\\', "/").into_bytes();
    #[cfg(not(unix))]
    let bytes = [&b"/"[..], &bytes[..]].concat();
    let mut out = String::from("file://");
    for &b in bytes.iter() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Local path of a `file://` URI (`file:///x`, `file://localhost/x`); None for any other scheme.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.trim().strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    #[cfg(unix)]
    return Some(PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(out)));
    #[cfg(not(unix))]
    {
        let text = String::from_utf8_lossy(&out).into_owned();
        let text = text.strip_prefix('/').filter(|t| t.as_bytes().get(1) == Some(&b':')).unwrap_or(&text);
        Some(PathBuf::from(text.replace('/', "\\")))
    }
}

/// Body of a clipboard offer of `mime` for these files.
pub fn encode_files(mime: &str, files: &FileList) -> String {
    let uris: Vec<String> = files.paths.iter().map(|p| path_to_uri(p)).collect();
    if mime == GNOME_FILES {
        format!("{}\n{}", if files.cut { "cut" } else { "copy" }, uris.join("\n"))
    } else {
        uris.iter().map(|u| format!("{u}\r\n")).collect()
    }
}

/// Files in clipboard text of type `mime` (`x-special/gnome-copied-files` or `text/uri-list`);
/// only `file://` entries count, comments (`#`) are skipped. None when there is no file at all.
pub fn parse_files(mime: &str, text: &str) -> Option<FileList> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut cut = false;
    let mut first = lines.next();
    if mime == GNOME_FILES {
        match first {
            Some("cut") => cut = true,
            Some("copy") => {}
            _ => return None,
        }
        first = lines.next();
    }
    let paths: Vec<PathBuf> =
        first.into_iter().chain(lines).filter(|l| !l.starts_with('#')).filter_map(uri_to_path).collect();
    (!paths.is_empty()).then_some(FileList { paths, cut })
}

/// Which flavor to offer: GNOME-family file managers want `x-special/gnome-copied-files` (it is the
/// only one that carries "cut"); everything else reads `text/uri-list`. A tool such as `wl-copy`
/// or `xclip` serves a single MIME type per invocation, so one of the two is chosen
/// (`LUMEN_FILE_CLIP=gnome|uri` overrides the guess made from `XDG_CURRENT_DESKTOP`).
pub fn offer_mime(desktop: &str, forced: Option<&str>) -> &'static str {
    match forced {
        Some("gnome") => return GNOME_FILES,
        Some("uri") => return URI_LIST,
        _ => {}
    }
    let d = desktop.to_ascii_lowercase();
    if ["gnome", "unity", "pantheon", "budgie", "cinnamon", "mate", "xfce", "x-cinnamon"]
        .iter()
        .any(|n| d.split(':').any(|p| p == *n))
    {
        GNOME_FILES
    } else {
        URI_LIST
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    Wayland,
    Xclip,
    Xsel,
    /// macOS `pbcopy` / `pbpaste`.
    #[cfg(target_os = "macos")]
    Pasteboard,
    /// Windows clipboard API (through `arboard`).
    #[cfg(windows)]
    Native,
    None,
}

fn has(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

#[cfg(target_os = "macos")]
fn detect() -> Backend {
    Backend::Pasteboard
}

#[cfg(windows)]
fn detect() -> Backend {
    Backend::Native
}

#[cfg(not(any(target_os = "macos", windows)))]
fn detect() -> Backend {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && has("wl-copy") && has("wl-paste") {
        Backend::Wayland
    } else if has("xclip") {
        Backend::Xclip
    } else if has("xsel") {
        Backend::Xsel
    } else if has("wl-copy") && has("wl-paste") {
        Backend::Wayland
    } else {
        Backend::None
    }
}

pub struct Clipboard {
    backend: Backend,
    internal: String,
    internal_primary: String,
    /// `PATH` the tools are looked up in (tests point it at fake tools).
    tool_path: Option<std::ffi::OsString>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self { backend: detect(), internal: String::new(), internal_primary: String::new(), tool_path: None }
    }
}

/// (program, args) that read or write one of the two selections.
fn command(b: Backend, primary: bool, write: bool) -> Option<(&'static str, Vec<&'static str>)> {
    Some(match (b, write) {
        (Backend::Wayland, true) => ("wl-copy", if primary { vec!["--primary"] } else { vec![] }),
        (Backend::Wayland, false) => {
            ("wl-paste", if primary { vec!["--primary", "--no-newline"] } else { vec!["--no-newline"] })
        }
        (Backend::Xclip, true) => ("xclip", vec!["-selection", if primary { "primary" } else { "clipboard" }, "-i"]),
        (Backend::Xclip, false) => ("xclip", vec!["-selection", if primary { "primary" } else { "clipboard" }, "-o"]),
        (Backend::Xsel, true) => ("xsel", vec![if primary { "-ip" } else { "-ib" }]),
        (Backend::Xsel, false) => ("xsel", vec![if primary { "-op" } else { "-ob" }]),
        #[cfg(target_os = "macos")]
        (Backend::Pasteboard, true) if !primary => ("pbcopy", vec![]),
        #[cfg(target_os = "macos")]
        (Backend::Pasteboard, false) if !primary => ("pbpaste", vec![]),
        _ => return None,
    })
}

/// (program, args) that write the clipboard as MIME type `mime`; None when the tool cannot.
fn typed_write(b: Backend, mime: &str) -> Option<(&'static str, Vec<String>)> {
    Some(match b {
        Backend::Wayland => ("wl-copy", vec!["--type".into(), mime.into()]),
        Backend::Xclip => {
            ("xclip", vec!["-selection".into(), "clipboard".into(), "-t".into(), mime.into(), "-i".into()])
        }
        _ => return None,
    })
}

/// (program, args) that read the clipboard as MIME type `mime` (`None` = list the offered types).
fn typed_read(b: Backend, mime: Option<&str>) -> Option<(&'static str, Vec<String>)> {
    Some(match (b, mime) {
        (Backend::Wayland, Some(m)) => ("wl-paste", vec!["--no-newline".into(), "--type".into(), m.into()]),
        (Backend::Wayland, None) => ("wl-paste", vec!["--list-types".into()]),
        (Backend::Xclip, Some(m)) => {
            ("xclip", vec!["-selection".into(), "clipboard".into(), "-t".into(), m.into(), "-o".into()])
        }
        (Backend::Xclip, None) => {
            ("xclip", vec!["-selection".into(), "clipboard".into(), "-t".into(), "TARGETS".into(), "-o".into()])
        }
        _ => return None,
    })
}

impl Clipboard {
    /// No tools at all: everything stays in this process (tests must never touch the real clipboard).
    #[cfg(test)]
    pub fn in_process() -> Self {
        Self { backend: Backend::None, internal: String::new(), internal_primary: String::new(), tool_path: None }
    }

    /// Wayland backend whose `wl-copy` / `wl-paste` are shell scripts in `dir` that keep the
    /// clipboard in `dir/mime` (offered types) and `dir/data` (the bytes), like a real run would
    /// see them. Tests inspect or rewrite those files to play "another program".
    #[cfg(all(test, unix))]
    pub fn with_fake_tools(dir: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let d = dir.display();
        script("wl-copy", &format!("echo \"$2\" > {d}/mime; cat > {d}/data"));
        script("wl-paste", &format!("case \"$1\" in --list-types) cat {d}/mime;; *) cat {d}/data;; esac"));
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        Clipboard {
            backend: Backend::Wayland,
            internal: String::new(),
            internal_primary: String::new(),
            tool_path: Some(path.into()),
        }
    }

    fn tool(&self, prog: &str) -> Command {
        let mut c = Command::new(prog);
        if let Some(p) = &self.tool_path {
            c.env("PATH", p);
        }
        c
    }

    fn write(&self, primary: bool, text: &str) {
        #[cfg(windows)]
        if self.backend == Backend::Native {
            if !primary && let Ok(mut c) = arboard::Clipboard::new() {
                let _ = c.set_text(text.to_string());
            }
            return;
        }
        let Some((prog, args)) = command(self.backend, primary, true) else { return };
        let Ok(mut child) =
            self.tool(prog).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // Tools that serve the selection fork into the background; reap off-thread.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }

    fn read(&self, primary: bool) -> Option<String> {
        #[cfg(windows)]
        if self.backend == Backend::Native {
            return if primary { None } else { arboard::Clipboard::new().ok()?.get_text().ok() };
        }
        let (prog, args) = command(self.backend, primary, false)?;
        let out = self.tool(prog).args(args).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten()
    }

    pub fn set(&mut self, text: &str) {
        self.internal = text.to_string();
        self.write(false, text);
    }

    pub fn get(&mut self) -> String {
        self.read(false).unwrap_or_else(|| self.internal.clone())
    }

    /// Selecting text copies it to the primary selection (middle-click pastes it).
    pub fn set_primary(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.internal_primary = text.to_string();
        self.write(true, text);
    }

    pub fn get_primary(&mut self) -> String {
        self.read(true).unwrap_or_else(|| self.internal_primary.clone())
    }

    /// Put copied/cut files on the system clipboard so other programs can paste them.
    /// Returns false when the backend cannot serve a typed offer (no tool, or `xsel`).
    pub fn set_files(&mut self, files: &FileList) -> bool {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let mime = offer_mime(&desktop, std::env::var("LUMEN_FILE_CLIP").ok().as_deref());
        let Some((prog, args)) = typed_write(self.backend, mime) else { return false };
        if files.paths.is_empty() {
            return false;
        }
        let Ok(mut child) =
            self.tool(prog).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(encode_files(mime, files).as_bytes());
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        true
    }

    /// Files another program (or this one) copied to the system clipboard.
    pub fn get_files(&mut self) -> Option<FileList> {
        let this = &*self;
        let run = |(prog, args): (&str, Vec<String>)| -> Option<String> {
            let out = this.tool(prog).args(args).stderr(Stdio::null()).output().ok()?;
            out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten()
        };
        let types = run(typed_read(self.backend, None)?)?;
        let has = |m: &str| types.lines().any(|t| t.trim() == m);
        // The GNOME flavor knows about "cut"; prefer it when both are offered.
        for mime in [GNOME_FILES, URI_LIST] {
            if has(mime)
                && let Some(list) = run(typed_read(self.backend, Some(mime))?).and_then(|t| parse_files(mime, &t))
            {
                return Some(list);
            }
        }
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn command_table_covers_each_backend() {
        assert_eq!(command(Backend::Wayland, true, true), Some(("wl-copy", vec!["--primary"])));
        assert_eq!(command(Backend::Wayland, false, false), Some(("wl-paste", vec!["--no-newline"])));
        assert_eq!(command(Backend::Xclip, true, false), Some(("xclip", vec!["-selection", "primary", "-o"])));
        assert_eq!(command(Backend::Xsel, false, true), Some(("xsel", vec!["-ib"])));
        assert_eq!(command(Backend::None, false, true), None);
    }

    #[test]
    fn uris_round_trip_with_odd_characters() {
        for p in ["/home/a/b.txt", "/home/a/with space/é ü#%?.ts", "/x/a+b (cópia).md"] {
            let uri = path_to_uri(Path::new(p));
            assert!(
                uri.starts_with("file:///") && !uri.contains(' ') && !uri.contains('#') && !uri[7..].contains('?'),
                "{uri}"
            );
            assert_eq!(uri_to_path(&uri), Some(PathBuf::from(p)), "{uri}");
        }
        assert_eq!(path_to_uri(Path::new("/a b")), "file:///a%20b");
        assert_eq!(uri_to_path("file://localhost/tmp/x"), Some(PathBuf::from("/tmp/x")));
        assert_eq!(uri_to_path("https://example.com/x"), None);
        assert_eq!(uri_to_path("file://otherhost/x"), None, "remote hosts are not local files");
        assert_eq!(uri_to_path("file:///a%2"), Some(PathBuf::from("/a%2")), "a truncated escape stays literal");
    }

    #[test]
    fn file_lists_encode_and_parse_both_flavors() {
        let files = FileList { paths: vec!["/p/a b.ts".into(), "/p/dir".into()], cut: false };
        assert_eq!(encode_files(URI_LIST, &files), "file:///p/a%20b.ts\r\nfile:///p/dir\r\n");
        assert_eq!(encode_files(GNOME_FILES, &files), "copy\nfile:///p/a%20b.ts\nfile:///p/dir");
        let cut = FileList { cut: true, ..files.clone() };
        assert!(encode_files(GNOME_FILES, &cut).starts_with("cut\n"));
        assert_eq!(parse_files(URI_LIST, &encode_files(URI_LIST, &files)), Some(files.clone()));
        assert_eq!(parse_files(GNOME_FILES, &encode_files(GNOME_FILES, &files)), Some(files.clone()));
        assert_eq!(parse_files(GNOME_FILES, &encode_files(GNOME_FILES, &cut)), Some(cut));
        // Comments, blank lines and non-file URIs (a browser's link) are ignored.
        assert_eq!(
            parse_files(URI_LIST, "# comment\r\nfile:///x/y\r\nhttps://a.b/c\r\n\r\n").map(|f| f.paths),
            Some(vec![PathBuf::from("/x/y")])
        );
        assert_eq!(parse_files(URI_LIST, "https://a.b/c\r\n"), None);
        assert_eq!(parse_files(URI_LIST, "just text"), None);
        assert_eq!(parse_files(GNOME_FILES, "file:///x\n"), None, "the GNOME flavor needs its copy/cut line");
    }

    #[test]
    fn flavor_follows_the_desktop() {
        assert_eq!(offer_mime("GNOME", None), GNOME_FILES);
        assert_eq!(offer_mime("ubuntu:GNOME", None), GNOME_FILES);
        assert_eq!(offer_mime("X-Cinnamon", None), GNOME_FILES);
        assert_eq!(offer_mime("KDE", None), URI_LIST);
        assert_eq!(offer_mime("Hyprland", None), URI_LIST);
        assert_eq!(offer_mime("", None), URI_LIST);
        assert_eq!(offer_mime("GNOME", Some("uri")), URI_LIST);
        assert_eq!(offer_mime("KDE", Some("gnome")), GNOME_FILES);
    }

    #[test]
    fn typed_commands_per_backend() {
        assert_eq!(
            typed_write(Backend::Wayland, URI_LIST),
            Some(("wl-copy", vec!["--type".to_string(), URI_LIST.to_string()]))
        );
        assert_eq!(typed_write(Backend::Xclip, GNOME_FILES).map(|c| c.0), Some("xclip"));
        assert_eq!(typed_write(Backend::Xsel, URI_LIST), None);
        assert_eq!(typed_read(Backend::Wayland, None), Some(("wl-paste", vec!["--list-types".to_string()])));
        assert!(typed_read(Backend::Xclip, None).unwrap().1.contains(&"TARGETS".to_string()));
        assert_eq!(typed_read(Backend::None, Some(URI_LIST)), None);
        let mut c = Clipboard::in_process();
        assert!(!c.set_files(&FileList { paths: vec!["/x".into()], cut: false }));
        assert_eq!(c.get_files(), None);
    }

    /// `set_files` / `get_files` against fake `wl-copy` / `wl-paste` scripts that keep the clipboard
    /// in a directory: the program, arguments and bytes are what a real run would use.
    #[test]
    fn files_round_trip_through_the_clipboard_tools() {
        let dir = std::env::temp_dir().join(format!("lumen-fakeclip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut c = Clipboard::with_fake_tools(&dir);
        let files = FileList { paths: vec!["/p/a b.ts".into(), "/p/\u{e9}.md".into()], cut: true };
        assert!(c.set_files(&files));
        // `wl-copy` is a background process in tests too: wait for the bytes to land.
        for _ in 0..100 {
            if std::fs::read_to_string(dir.join("data")).is_ok_and(|s| !s.is_empty()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mime = std::fs::read_to_string(dir.join("mime")).unwrap().trim().to_string();
        assert!(mime == URI_LIST || mime == GNOME_FILES, "{mime}");
        let got = c.get_files().expect("files come back");
        assert_eq!(got.paths, files.paths);
        assert_eq!(got.cut, mime == GNOME_FILES, "only the GNOME flavor carries cut");
        // Another program copies plain text: no files.
        std::fs::write(dir.join("mime"), "text/plain\n").unwrap();
        std::fs::write(dir.join("data"), "hello").unwrap();
        assert_eq!(c.get_files(), None);
        // A file manager offering both flavors: the GNOME one wins (it knows about cut).
        std::fs::write(dir.join("mime"), "text/uri-list\nx-special/gnome-copied-files\n").unwrap();
        std::fs::write(dir.join("data"), "cut\nfile:///tmp/zz").unwrap();
        assert_eq!(c.get_files(), Some(FileList { paths: vec!["/tmp/zz".into()], cut: true }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_the_in_process_copy_without_tools() {
        let mut c = Clipboard::in_process();
        c.set("abc");
        assert_eq!(c.get(), "abc");
        c.set_primary("sel");
        assert_eq!(c.get_primary(), "sel");
        c.set_primary("");
        assert_eq!(c.get_primary(), "sel", "empty selections do not clobber the primary");
    }
}
