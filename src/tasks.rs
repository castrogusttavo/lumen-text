//! Running project tasks (npm scripts, tests, cargo) and presenting their output:
//! ANSI colors, clickable `file:line:col` links, process-group kill.

use crate::theme::Rgba;
use regex::Regex;
use std::{
    io::{BufRead, BufReader},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{Receiver, channel},
    thread,
};

pub type Segs = Vec<(String, Rgba)>;

pub(crate) const PALETTE: [Rgba; 16] = [
    Rgba(0x1e, 0x1e, 0x1e, 255),
    Rgba(0xf4, 0x47, 0x47, 255),
    Rgba(0x4e, 0xc9, 0x7a, 255),
    Rgba(0xe5, 0xc0, 0x7b, 255),
    Rgba(0x61, 0xaf, 0xef, 255),
    Rgba(0xc6, 0x78, 0xdd, 255),
    Rgba(0x56, 0xb6, 0xc2, 255),
    Rgba(0xd4, 0xd4, 0xd4, 255),
    Rgba(0x7f, 0x84, 0x8e, 255),
    Rgba(0xff, 0x6b, 0x6b, 255),
    Rgba(0x7e, 0xe7, 0x87, 255),
    Rgba(0xff, 0xd8, 0x66, 255),
    Rgba(0x82, 0xb1, 0xff, 255),
    Rgba(0xe0, 0x9b, 0xff, 255),
    Rgba(0x7d, 0xd8, 0xe8, 255),
    Rgba(0xff, 0xff, 0xff, 255),
];

/// The ANSI palette is tuned for dark backgrounds; on any surface, nudge `fg` toward black or white
/// until it reads (>= 4.5:1). Colors that already read are returned untouched.
pub fn legible(fg: Rgba, bg: Rgba) -> Rgba {
    if fg.contrast(bg) >= 4.5 { fg } else { fg.readable_on(&[bg], 4.5) }
}

pub(crate) fn xterm256(n: u32) -> Rgba {
    match n {
        0..=15 => PALETTE[n as usize],
        16..=231 => {
            let k = n - 16;
            let lvl = |v: u32| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgba(lvl(k / 36), lvl((k / 6) % 6), lvl(k % 6), 255)
        }
        _ => {
            let g = (8 + (n.min(255) - 232) * 10) as u8;
            Rgba(g, g, g, 255)
        }
    }
}

/// Turns raw output lines into colored segments, carrying SGR state across lines.
pub struct Ansi {
    fg: Option<Rgba>,
    dim: bool,
}

impl Ansi {
    pub fn new() -> Self {
        Self { fg: None, dim: false }
    }

    pub fn line(&mut self, raw: &str, default: Rgba, dim_color: Rgba) -> Segs {
        let mut out: Segs = Vec::new();
        let mut cur = String::new();
        let color = |s: &Ansi| if s.dim { dim_color } else { s.fg.unwrap_or(default) };
        let mut it = raw.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\u{1b}' && it.peek() == Some(&'[') {
                it.next();
                let mut params = String::new();
                let mut fin = '\0';
                for ch in it.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        fin = ch;
                        break;
                    }
                    params.push(ch);
                }
                if fin == 'm' {
                    if !cur.is_empty() {
                        out.push((std::mem::take(&mut cur), color(self)));
                    }
                    self.sgr(&params);
                }
                continue;
            }
            if c == '\r' || (c.is_control() && c != '\t') {
                continue;
            }
            cur.push(if c == '\t' { ' ' } else { c });
        }
        if !cur.is_empty() {
            out.push((cur, color(self)));
        }
        out
    }

    fn sgr(&mut self, params: &str) {
        let nums: Vec<u32> =
            if params.is_empty() { vec![0] } else { params.split(';').map(|p| p.parse().unwrap_or(0)).collect() };
        let mut i = 0;
        while i < nums.len() {
            match nums[i] {
                0 => {
                    self.fg = None;
                    self.dim = false;
                }
                2 => self.dim = true,
                22 => self.dim = false,
                30..=37 => self.fg = Some(PALETTE[(nums[i] - 30) as usize]),
                90..=97 => self.fg = Some(PALETTE[(nums[i] - 90 + 8) as usize]),
                39 => self.fg = None,
                38 if nums.get(i + 1) == Some(&5) => {
                    if let Some(&n) = nums.get(i + 2) {
                        self.fg = Some(xterm256(n));
                    }
                    i += 2;
                }
                38 if nums.get(i + 1) == Some(&2) => {
                    if let (Some(&r), Some(&g), Some(&b)) = (nums.get(i + 2), nums.get(i + 3), nums.get(i + 4)) {
                        self.fg = Some(Rgba(r as u8, g as u8, b as u8, 255));
                    }
                    i += 4;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: usize, // char offsets in the plain text
    pub end: usize,
    pub path: String,
    pub line: usize, // 0-based
    pub col: usize,  // 0-based
}

/// `src/a.ts:12:5`, `/abs/b.rs:3`, and tsc-style `src/a.ts(12,5)`.
pub fn find_links(text: &str) -> Vec<Link> {
    thread_local! {
        static COLON: Regex = Regex::new(r"([A-Za-z0-9_@./\\-]+\.[A-Za-z0-9]{1,6}):(\d+)(?::(\d+))?").unwrap();
        static PAREN: Regex = Regex::new(r"([A-Za-z0-9_@./\\-]+\.[A-Za-z0-9]{1,6})\((\d+),(\d+)\)").unwrap();
    }
    let mut out = Vec::new();
    let to_chars = |b: usize| text[..b].chars().count();
    for re in [&COLON, &PAREN] {
        re.with(|re| {
            for c in re.captures_iter(text) {
                let m = c.get(0).unwrap();
                let line = c[2].parse::<usize>().unwrap_or(1).saturating_sub(1);
                let col = c.get(3).and_then(|v| v.as_str().parse::<usize>().ok()).unwrap_or(1).saturating_sub(1);
                out.push(Link {
                    start: to_chars(m.start()),
                    end: to_chars(m.end()),
                    path: c[1].to_string(),
                    line,
                    col,
                });
            }
        });
    }
    out.sort_by_key(|l| l.start);
    out
}

// ───────────── project knowledge ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pm {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl Pm {
    pub fn detect(root: &Path) -> Pm {
        if root.join("pnpm-lock.yaml").exists() {
            Pm::Pnpm
        } else if root.join("yarn.lock").exists() {
            Pm::Yarn
        } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
            Pm::Bun
        } else {
            Pm::Npm
        }
    }
    pub fn run(&self, script: &str) -> String {
        match self {
            Pm::Npm => format!("npm run {script}"),
            Pm::Pnpm => format!("pnpm run {script}"),
            Pm::Yarn => format!("yarn {script}"),
            Pm::Bun => format!("bun run {script}"),
        }
    }
    pub fn exec(&self) -> &'static str {
        match self {
            Pm::Npm => "npx",
            Pm::Pnpm => "pnpm exec",
            Pm::Yarn => "yarn",
            Pm::Bun => "bunx",
        }
    }
}

/// (name, command) pairs the palette offers for this project.
pub fn project_tasks(root: &Path) -> Vec<(String, String)> {
    let mut v = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join("package.json"))
        && let Ok(j) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let pm = Pm::detect(root);
        if let Some(scripts) = j["scripts"].as_object() {
            for (k, body) in scripts {
                v.push((format!("{} {k}", pm.run("").trim()), pm.run(k)));
                let _ = body;
            }
        }
    }
    if root.join("Cargo.toml").exists() {
        for c in ["cargo build", "cargo test", "cargo run", "cargo check", "cargo clippy"] {
            v.push((c.to_string(), c.to_string()));
        }
    }
    v
}

/// Which test runner does this package use?
pub fn test_runner(root: &Path) -> Option<&'static str> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    if text.contains("\"vitest\"") {
        Some("vitest")
    } else if text.contains("\"jest\"") {
        Some("jest")
    } else {
        None
    }
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Name of the `it(...)`/`test(...)` at or above `line`, from the given lines.
pub fn test_name_at(lines: &[String], line: usize) -> Option<String> {
    thread_local! {
        static RE: Regex = Regex::new(r#"\b(?:it|test)(?:\.(?:only|skip|concurrent))?\s*\(\s*(?:'((?:[^'\\]|\\.)*)'|"((?:[^"\\]|\\.)*)"|`((?:[^`\\]|\\.)*)`)"#).unwrap();
    }
    for l in (0..=line.min(lines.len().saturating_sub(1))).rev() {
        if let Some(c) =
            RE.with(|re| re.captures(&lines[l]).map(|c| (1..=3).find_map(|i| c.get(i)).map(|m| m.as_str().to_string())))
        {
            return c;
        }
    }
    None
}

/// Line of the `it(...)`/`test(...)` call whose title is `title`.
pub fn find_test_line(lines: &[String], title: &str) -> Option<usize> {
    lines.iter().position(|l| {
        (l.contains("it(") || l.contains("test(") || l.contains("it.") || l.contains("test.")) && l.contains(title)
    })
}

// ───────────── running ─────────────

pub enum Event {
    Line(String),
    Done,
}

pub struct Running {
    pub child: Child,
    pub rx: Receiver<Event>,
    pub pid: i32,
}

pub fn spawn(cmd: &str, cwd: &Path, wake: crate::lsp::Wake) -> std::io::Result<Running> {
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .env("FORCE_COLOR", "1")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group so Stop kills the whole tree (npm -> node -> vitest ...).
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.spawn()?;
    let pid = child.id() as i32;
    let (tx, rx) = channel();
    let mut readers = Vec::new();
    for stream in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let (tx, wake) = (tx.clone(), wake.clone());
        readers.push(thread::spawn(move || {
            for line in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                if tx.send(Event::Line(String::from_utf8_lossy(&line).into_owned())).is_err() {
                    break;
                }
                wake();
            }
        }));
    }
    let waiter_tx = tx;
    let waiter_wake = wake;
    let wpid = pid;
    thread::spawn(move || {
        for r in readers {
            let _ = r.join();
        }
        // Exit status is collected by the main thread through `try_wait`; signal completion here.
        let _ = waiter_tx.send(Event::Done);
        waiter_wake();
        let _ = wpid;
    });
    Ok(Running { child, rx, pid })
}

pub fn kill_group(pid: i32) {
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
}

pub fn resolve_link_path(root: &Path, p: &str) -> Option<PathBuf> {
    let path = PathBuf::from(p);
    let cand = if path.is_absolute() { path } else { root.join(p.trim_start_matches("./")) };
    cand.is_file().then_some(cand)
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Rgba = Rgba(200, 200, 200, 255);
    const DIM: Rgba = Rgba(100, 100, 100, 255);

    #[test]
    fn ansi_colors_persist_and_reset() {
        let mut a = Ansi::new();
        let s = a.line("\u{1b}[31mFAIL\u{1b}[0m ok \u{1b}[38;2;1;2;3mrgb", D, DIM);
        assert_eq!(s[0], ("FAIL".to_string(), PALETTE[1]));
        assert_eq!(s[1], (" ok ".to_string(), D));
        assert_eq!(s[2], ("rgb".to_string(), Rgba(1, 2, 3, 255)));
        // State carries to the next line until reset.
        let s2 = a.line("still rgb\u{1b}[39m plain", D, DIM);
        assert_eq!(s2[0].1, Rgba(1, 2, 3, 255));
        assert_eq!(s2[1].1, D);
        // Unknown CSI sequences are dropped, carriage returns ignored.
        let s3 = a.line("a\u{1b}[2Kb\r", D, DIM);
        assert_eq!(s3.iter().map(|x| x.0.as_str()).collect::<String>(), "ab");
    }

    #[test]
    fn xterm256_cube_and_gray() {
        assert_eq!(xterm256(16), Rgba(0, 0, 0, 255));
        assert_eq!(xterm256(196), Rgba(255, 0, 0, 255));
        assert_eq!(xterm256(232), Rgba(8, 8, 8, 255));
    }

    #[test]
    fn links_colon_and_paren_forms() {
        let l = find_links(" ❯ src/a.test.ts:12:5 and lib/b.rs:3 and c.ts(7,9)");
        assert_eq!(l.len(), 3, "{l:?}");
        assert_eq!((l[0].path.as_str(), l[0].line, l[0].col), ("src/a.test.ts", 11, 4));
        assert_eq!((l[1].path.as_str(), l[1].line, l[1].col), ("lib/b.rs", 2, 0));
        assert_eq!((l[2].path.as_str(), l[2].line, l[2].col), ("c.ts", 6, 8));
        assert!(find_links("12:30:45 time only").is_empty(), "times are not file links");
    }

    #[test]
    fn test_name_search_goes_upwards() {
        let src: Vec<String> = [
            "describe('suite', () => {",
            "  it('does a thing', async () => {",
            "    const x = 1",
            "    expect(x).toBe(1)",
            "  })",
            "  test.skip(\"second one\", () => {})",
            "})",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(test_name_at(&src, 3).as_deref(), Some("does a thing"));
        assert_eq!(test_name_at(&src, 5).as_deref(), Some("second one"));
        assert_eq!(test_name_at(&src, 0), None);
    }

    #[test]
    fn finds_the_line_of_a_test_by_title() {
        let src: Vec<String> =
            ["describe('s', () => {", "  it('adds numbers', () => {})", "  test.skip(\"later\", () => {})"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(find_test_line(&src, "adds numbers"), Some(1));
        assert_eq!(find_test_line(&src, "later"), Some(2));
        assert_eq!(find_test_line(&src, "nope"), None);
    }

    #[test]
    fn project_detection() {
        let root = std::env::temp_dir().join(format!("lumen-tasks-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"dev":"next dev","test":"vitest"},"devDependencies":{"vitest":"1"}}"#,
        )
        .unwrap();
        assert_eq!(Pm::detect(&root), Pm::Pnpm);
        assert_eq!(test_runner(&root), Some("vitest"));
        let t = project_tasks(&root);
        assert!(t.iter().any(|(_, c)| c == "pnpm run dev"), "{t:?}");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn spawn_streams_output_and_kill_group_works() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let wake: crate::lsp::Wake = std::sync::Arc::new(move || {
            let _ = tx.send(());
        });
        let mut r = spawn("echo one; echo two >&2", Path::new("/tmp"), wake.clone()).unwrap();
        let mut lines = Vec::new();
        loop {
            match r.rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                Event::Line(l) => lines.push(l),
                Event::Done => break,
            }
        }
        lines.sort();
        assert_eq!(lines, ["one", "two"]);
        let _ = r.child.wait();
        let _ = rx;

        let mut long = spawn("sleep 30", Path::new("/tmp"), wake).unwrap();
        kill_group(long.pid);
        let st = long.child.wait().unwrap();
        assert!(!st.success());
    }
}
