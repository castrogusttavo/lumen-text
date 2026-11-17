//! External formatters (Biome, Prettier) driven through stdin/stdout, so the buffer is
//! formatted without being saved first.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// The error [`run`] returns when the tool was still running after its wait and was killed.
pub const TIMED_OUT: &str = "did not answer in time";

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Formatter {
    pub name: &'static str,
    pub bin: PathBuf,
    pub args: Vec<String>,
    /// Directory holding the config that was found: the tool runs there so it resolves the nearest
    /// config (a nested project under the opened folder must not see the outer root's).
    pub cwd: PathBuf,
}

fn find_up(start: &Path, names: &[&str]) -> Option<PathBuf> {
    start.ancestors().find(|d| names.iter().any(|n| d.join(n).exists())).map(Path::to_path_buf)
}

fn local_bin(from: &Path, name: &str) -> Option<PathBuf> {
    from.ancestors().map(|d| d.join("node_modules/.bin").join(name)).find(|p| p.exists())
}

/// The formatter this project is configured for, if its binary is installed.
pub fn detect(file: &Path) -> Option<Formatter> {
    detect_named(file, "biome").or_else(|| detect_named(file, "prettier"))
}

/// Formatter `name` ("biome" or "prettier") for `file`, when the project has its config and binary
/// (the `formatter` setting forces one of them).
pub fn detect_named(file: &Path, name: &str) -> Option<Formatter> {
    let dir = file.parent()?;
    let path = file.to_string_lossy().into_owned();
    if name == "biome"
        && let Some(root) = find_up(dir, &["biome.json", "biome.jsonc"])
        && let Some(bin) = local_bin(&root, "biome")
    {
        return Some(Formatter {
            name: "biome",
            bin,
            args: vec!["format".into(), format!("--stdin-file-path={path}")],
            cwd: root,
        });
    }
    let prettier_cfgs = [
        ".prettierrc",
        ".prettierrc.json",
        ".prettierrc.yaml",
        ".prettierrc.yml",
        ".prettierrc.js",
        ".prettierrc.cjs",
        ".prettierrc.mjs",
        "prettier.config.js",
        "prettier.config.cjs",
        "prettier.config.mjs",
        "prettier.config.ts",
    ];
    if name == "prettier"
        && let Some(root) = find_up(dir, &prettier_cfgs)
        && let Some(bin) = local_bin(&root, "prettier")
    {
        return Some(Formatter { name: "prettier", bin, args: vec![format!("--stdin-filepath={path}")], cwd: root });
    }
    None
}

/// The tool gets a process group of its own, so a wrapper script and what it started (Biome's
/// launcher runs the native binary as a child) can be stopped together.
fn own_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    // SAFETY: plain signal to the group created by `own_group` (its id is the child's pid, which
    // is still ours: the child has not been reaped yet).
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// What a finished tool left: exit status, stdout, stderr.
type Finished = (ExitStatus, Vec<u8>, Vec<u8>);

/// Feed `input` to `child` and collect its output, waiting at most `wait`: `None` when it was
/// still running then (it is killed). The pipes are read on threads, so a large buffer never
/// deadlocks against a full pipe and a process that keeps a pipe open cannot hold the caller.
fn wait_output(mut child: Child, input: Option<String>, wait: Duration) -> Option<Finished> {
    let deadline = Instant::now() + wait;
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let mut pipes = 0;
    let mut read = |pipe: Option<Box<dyn Read + Send>>, is_out: bool| {
        let Some(mut pipe) = pipe else { return };
        pipes += 1;
        let tx = tx.clone();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send((is_out, buf));
        });
    };
    read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), true);
    read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>), false);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    for _ in 0..pipes {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((true, b)) => out = b,
            Ok((false, b)) => err = b,
            Err(_) => {
                kill_tree(&mut child);
                return None;
            }
        }
    }
    // Both pipes are closed: the process is gone or about to be.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some((status, out, err)),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            _ => {
                kill_tree(&mut child);
                return None;
            }
        }
    }
}

/// Format `text`; returns the new text, or an error message (formatter output on failure,
/// [`TIMED_OUT`] when the tool was still running after `wait` and was killed).
pub fn run(f: &Formatter, text: &str, wait: Duration) -> Result<String, String> {
    let child = own_group(Command::new(&f.bin).args(&f.args).current_dir(&f.cwd))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", f.name))?;
    let Some((status, out, err)) = wait_output(child, Some(text.to_string()), wait) else {
        return Err(TIMED_OUT.to_string());
    };
    if status.success() {
        String::from_utf8(out).map_err(|e| e.to_string())
    } else {
        Err(format!("{}: {}", f.name, error_line(&String::from_utf8_lossy(&err))))
    }
}

/// Modification times of the Biome config files in the formatter's directory.
type BiomeStamp = [Option<SystemTime>; 2];

fn biome_stamp(cwd: &Path) -> BiomeStamp {
    ["biome.json", "biome.jsonc"].map(|n| std::fs::metadata(cwd.join(n)).and_then(|m| m.modified()).ok())
}

/// Answers of [`biome_ignores`] per file, each valid while the config files keep their stamp.
static IGNORED: Mutex<Option<HashMap<PathBuf, (BiomeStamp, bool)>>> = Mutex::new(None);

/// Does Biome's config leave `file` out (`files.includes`, ignore files)? Through stdin Biome hands
/// an ignored file back unchanged and says nothing, so ask it about the path itself (no `--write`:
/// nothing is touched on disk). Only asked when the stdin run changed nothing, and once per file:
/// the answer is kept until `biome.json` / `biome.jsonc` changes. Waits at most `wait` (no answer
/// counts as "not ignored" and is asked again next time).
pub fn biome_ignores(f: &Formatter, file: &Path, wait: Duration) -> bool {
    if f.name != "biome" {
        return false;
    }
    let stamp = biome_stamp(&f.cwd);
    if let Ok(g) = IGNORED.lock()
        && let Some((s, ignored)) = g.as_ref().and_then(|m| m.get(file))
        && *s == stamp
    {
        return *ignored;
    }
    let rel = file.strip_prefix(&f.cwd).unwrap_or(file);
    let Ok(child) = own_group(Command::new(&f.bin).arg("format").arg(rel).current_dir(&f.cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return false;
    };
    let Some((_, out, err)) = wait_output(child, None, wait) else { return false };
    let text = format!("{}{}", String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
    let ignored = reports_ignored(&text);
    if let Ok(mut g) = IGNORED.lock() {
        g.get_or_insert_with(HashMap::new).insert(file.to_path_buf(), (stamp, ignored));
    }
    ignored
}

/// Biome's report for a path its configuration ignores.
fn reports_ignored(report: &str) -> bool {
    report.contains("provided but ignored") || report.contains("No files were processed")
}

/// The first meaningful line of a tool's stderr: info/warning notes and decoration are skipped, an
/// explicit error line wins.
pub fn error_line(stderr: &str) -> String {
    let clean = |l: &str| l.trim().trim_start_matches(['×', '✖', 'x']).trim().to_string();
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| {
            let low = l.to_lowercase();
            !l.is_empty()
                && !l.starts_with(['ℹ', '!', '⚠', '━', '─', '│', '>', '|'])
                && !low.starts_with("info")
                && !low.starts_with("warn")
                && l.chars().any(char::is_alphanumeric)
        })
        .collect();
    let pick = lines
        .iter()
        .find(|l| l.starts_with(['×', '✖']) || l.to_lowercase().contains("error"))
        .or_else(|| lines.first());
    pick.map_or_else(|| "failed".to_string(), |l| clean(l))
}

/// Re-indent JSON (2 spaces) without touching values, key order or string contents. `None` for
/// anything that is not plain JSON (comments, trailing commas, unbalanced brackets).
pub fn format_json(text: &str, indent: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let (mut depth, mut chars) = (0usize, text.chars().peekable());
    let nl = |out: &mut String, d: usize| {
        out.push('\n');
        for _ in 0..d {
            out.push_str(indent);
        }
    };
    let mut last: Option<char> = None; // last significant char emitted
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                loop {
                    let n = chars.next()?;
                    out.push(n);
                    match n {
                        '\\' => out.push(chars.next()?),
                        '"' => break,
                        _ => {}
                    }
                }
                last = Some('"');
            }
            '{' | '[' => {
                let close = if c == '{' { '}' } else { ']' };
                while chars.peek().is_some_and(|x| x.is_whitespace()) {
                    chars.next();
                }
                if chars.peek() == Some(&close) {
                    chars.next();
                    out.push(c);
                    out.push(close);
                    last = Some(close);
                } else {
                    out.push(c);
                    depth += 1;
                    nl(&mut out, depth);
                    last = Some(c);
                }
            }
            '}' | ']' => {
                if matches!(last, Some(',')) {
                    return None;
                }
                depth = depth.checked_sub(1)?;
                nl(&mut out, depth);
                out.push(c);
                last = Some(c);
            }
            ',' => {
                if matches!(last, Some(',' | '[' | '{')) {
                    return None;
                }
                out.push(',');
                nl(&mut out, depth);
                while chars.peek().is_some_and(|x| x.is_whitespace()) {
                    chars.next();
                }
                last = Some(',');
            }
            ':' => {
                out.push_str(": ");
                while chars.peek().is_some_and(|x| x.is_whitespace()) {
                    chars.next();
                }
                last = Some(':');
            }
            '/' => return None,
            c if c.is_whitespace() => {}
            c => {
                out.push(c);
                last = Some(c);
            }
        }
    }
    if depth != 0 || out.is_empty() {
        return None;
    }
    out.push('\n');
    Some(out)
}

/// Key for "one-time message per file type": the extension, or the file name when it has none.
pub fn type_key(file: &Path) -> String {
    match file.extension() {
        Some(e) => format!(".{}", e.to_string_lossy()),
        None => file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Duration = Duration::from_secs(30);

    /// Scratch dir for tests (never under /tmp).
    pub fn test_base(tag: &str) -> PathBuf {
        let d = crate::app_unsaved::testkit::scratch_base().join(format!("fmt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detects_biome_when_installed_and_ignores_missing_binaries() {
        let root = std::env::temp_dir().join(format!("lumen-fmt-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("biome.json"), "{}").unwrap();
        let file = root.join("src/a.ts");
        assert_eq!(detect(&file), None, "config without binary is not usable");
        std::fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        std::fs::write(root.join("node_modules/.bin/biome"), "#!/bin/sh\n").unwrap();
        let f = detect(&file).unwrap();
        assert_eq!(f.name, "biome");
        assert!(f.args.iter().any(|a| a.starts_with("--stdin-file-path=")));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn run_pipes_text_through_the_tool() {
        let cwd = PathBuf::from("/");
        let f = Formatter {
            name: "tr",
            bin: PathBuf::from("tr"),
            args: vec!["a-z".into(), "A-Z".into()],
            cwd: cwd.clone(),
        };
        assert_eq!(run(&f, "hello\n", W).unwrap(), "HELLO\n");
        let big = "x".repeat(2_000_000);
        assert_eq!(run(&f, &big, W).unwrap().len(), 2_000_000, "no pipe deadlock");
        let bad = Formatter {
            name: "sh",
            bin: PathBuf::from("sh"),
            args: vec!["-c".into(), "echo boom >&2; exit 3".into()],
            cwd,
        };
        assert!(run(&bad, "", W).unwrap_err().contains("boom"));
        // stderr noise on success is not a failure
        let noisy = Formatter {
            name: "sh",
            bin: PathBuf::from("sh"),
            args: vec!["-c".into(), "echo 'i schema mismatch' >&2; echo out".into()],
            cwd: PathBuf::from("/"),
        };
        assert_eq!(run(&noisy, "", W).unwrap(), "out\n");
    }

    #[test]
    fn error_line_skips_info_and_prefers_errors() {
        let e = "ℹ Biome schema version mismatch\nsome note\n× Found a nested root configuration, but there's already a root configuration.\n";
        assert_eq!(error_line(e), "Found a nested root configuration, but there's already a root configuration.");
        assert_eq!(error_line("info: hi\nplain failure\n"), "plain failure");
        assert_eq!(error_line(""), "failed");
    }

    #[test]
    fn json_formatter_reindents_and_refuses_non_json() {
        let got = format_json("{\"a\":1,\"b\":[1,2,{\"c\":\"x, y \\\" {\"}],\"e\":{},\"f\":[ ]}", "  ").unwrap();
        let want = "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2,\n    {\n      \"c\": \"x, y \\\" {\"\n    }\n  ],\n  \"e\": {},\n  \"f\": []\n}\n";
        assert_eq!(got, want);
        assert_eq!(format_json(&got, "  ").unwrap(), got, "idempotent");
        assert_eq!(format_json("{\"a\": 1, // c\n}", "  "), None);
        assert_eq!(format_json("{\"a\": 1,}", "  "), None);
        assert_eq!(format_json("{\"a\": 1", "  "), None);
        assert_eq!(format_json("", "  "), None);
    }

    #[cfg(unix)]
    #[test]
    fn formatter_runs_in_the_directory_of_its_config() {
        use std::os::unix::fs::PermissionsExt;
        let base = test_base("cwd");
        // Outer root and a nested project both have a biome.json; the fake biome fails like the
        // real one when it runs from the outer directory and sees a nested root config.
        let inner = base.join("inner");
        std::fs::create_dir_all(inner.join("src")).unwrap();
        std::fs::create_dir_all(inner.join("node_modules/.bin")).unwrap();
        std::fs::write(base.join("biome.json"), "{}").unwrap();
        std::fs::write(inner.join("biome.json"), "{}").unwrap();
        let bin = inner.join("node_modules/.bin/biome");
        std::fs::write(
            &bin,
            "#!/bin/sh\nif [ -f inner/biome.json ]; then echo 'Found a nested root configuration' >&2; exit 1; fi\ncat\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let f = detect(&inner.join("src/a.ts")).unwrap();
        assert_eq!(f.cwd, inner);
        assert_eq!(run(&f, "ok\n", W).unwrap(), "ok\n");
        let mut outer = f.clone();
        outer.cwd = base.clone();
        assert!(run(&outer, "ok\n", W).unwrap_err().contains("nested root"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_formatter_that_hangs_is_killed_when_its_wait_expires() {
        let f = Formatter {
            name: "sh",
            bin: PathBuf::from("sh"),
            args: vec!["-c".into(), "sleep 30; cat".into()],
            cwd: PathBuf::from("/"),
        };
        let t0 = Instant::now();
        assert_eq!(run(&f, "x\n", Duration::from_millis(200)).unwrap_err(), TIMED_OUT);
        assert!(t0.elapsed() < Duration::from_secs(10), "did not wait for the tool: {:?}", t0.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn biome_is_asked_about_a_path_once_until_its_config_changes() {
        use std::os::unix::fs::PermissionsExt;
        let base = test_base("ignored-cache");
        std::fs::create_dir_all(base.join("lib")).unwrap();
        std::fs::write(base.join("biome.json"), "{}").unwrap();
        let bin = base.join("biome");
        // Every call leaves a line in `calls`; `lib/` is reported as ignored.
        let script = "#!/bin/sh\necho x >> calls\ncase \"$2\" in lib/*) echo 'These paths were provided but ignored:' ;; *) echo 'Checked 1 file' ;; esac\n";
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let f = Formatter { name: "biome", bin, args: Vec::new(), cwd: base.clone() };
        let calls = || std::fs::read_to_string(base.join("calls")).unwrap_or_default().lines().count();
        assert!(biome_ignores(&f, &base.join("lib/a.ts"), W));
        assert!(biome_ignores(&f, &base.join("lib/a.ts"), W));
        assert_eq!(calls(), 1, "the second answer came from the cache");
        assert!(!biome_ignores(&f, &base.join("src/a.ts"), W));
        assert!(!biome_ignores(&f, &base.join("src/a.ts"), W));
        assert_eq!(calls(), 2, "one question per file");
        // The config changed: ask again.
        let cfg = std::fs::File::options().append(true).open(base.join("biome.json")).unwrap();
        cfg.set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
        drop(cfg);
        assert!(biome_ignores(&f, &base.join("lib/a.ts"), W));
        assert_eq!(calls(), 3);
        let prettier = Formatter { name: "prettier", ..f.clone() };
        assert!(!biome_ignores(&prettier, &base.join("lib/a.ts"), W));
        assert_eq!(calls(), 3, "only Biome is asked");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn biome_ignored_report_is_recognized() {
        // Biome 2.4 for `biome format lib/a.ts` when files.includes leaves it out.
        let ignored = "Checked 0 files in 2ms. No fixes applied.\nformat ━━━━\n  × No files were processed in the specified paths.\n  i These paths were provided but ignored:\n  - lib/a.ts\n";
        assert!(reports_ignored(ignored));
        assert!(!reports_ignored("Checked 1 file in 3ms. No fixes applied.\n"));
        assert!(!reports_ignored("Formatter would have printed the following content:\n"));
    }

    #[test]
    fn a_named_formatter_is_found_even_when_the_other_one_would_win() {
        let root = test_base("named");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        for f in ["biome.json", ".prettierrc", "node_modules/.bin/biome", "node_modules/.bin/prettier"] {
            std::fs::write(root.join(f), "{}").unwrap();
        }
        let file = root.join("src/a.ts");
        assert_eq!(detect(&file).unwrap().name, "biome", "auto: Biome first");
        assert_eq!(detect_named(&file, "prettier").unwrap().name, "prettier");
        assert_eq!(detect_named(&file, "biome").unwrap().name, "biome");
        assert_eq!(detect_named(&file, "black"), None);
        std::fs::remove_file(root.join("node_modules/.bin/prettier")).unwrap();
        assert_eq!(detect_named(&file, "prettier"), None, "config without binary");
        std::fs::remove_dir_all(&root).ok();
    }
}
