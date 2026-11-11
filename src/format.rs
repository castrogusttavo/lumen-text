//! External formatters (Biome, Prettier) driven through stdin/stdout, so the buffer is
//! formatted without being saved first.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

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
    let dir = file.parent()?;
    let path = file.to_string_lossy().into_owned();
    if let Some(root) = find_up(dir, &["biome.json", "biome.jsonc"])
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
    if let Some(root) = find_up(dir, &prettier_cfgs)
        && let Some(bin) = local_bin(&root, "prettier")
    {
        return Some(Formatter { name: "prettier", bin, args: vec![format!("--stdin-filepath={path}")], cwd: root });
    }
    None
}

/// Format `text`; returns the new text, or an error message (formatter output on failure).
pub fn run(f: &Formatter, text: &str) -> Result<String, String> {
    let mut child = Command::new(&f.bin)
        .args(&f.args)
        .current_dir(&f.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", f.name))?;
    let mut stdin = child.stdin.take().unwrap();
    let input = text.to_string();
    // Write on a thread: large buffers would deadlock against a full stdout pipe otherwise.
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if out.status.success() {
        String::from_utf8(out.stdout).map_err(|e| e.to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{}: {}", f.name, error_line(&err)))
    }
}

/// Does Biome's config leave `file` out (`files.includes`, ignore files)? Through stdin Biome hands
/// an ignored file back unchanged and says nothing, so ask it about the path itself (no `--write`:
/// nothing is touched on disk). Only asked when the stdin run changed nothing.
pub fn biome_ignores(f: &Formatter, file: &Path) -> bool {
    if f.name != "biome" {
        return false;
    }
    let rel = file.strip_prefix(&f.cwd).unwrap_or(file);
    let Ok(out) = Command::new(&f.bin)
        .arg("format")
        .arg(rel)
        .current_dir(&f.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    else {
        return false;
    };
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    reports_ignored(&text)
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
        assert_eq!(run(&f, "hello\n").unwrap(), "HELLO\n");
        let big = "x".repeat(2_000_000);
        assert_eq!(run(&f, &big).unwrap().len(), 2_000_000, "no pipe deadlock");
        let bad = Formatter {
            name: "sh",
            bin: PathBuf::from("sh"),
            args: vec!["-c".into(), "echo boom >&2; exit 3".into()],
            cwd,
        };
        assert!(run(&bad, "").unwrap_err().contains("boom"));
        // stderr noise on success is not a failure
        let noisy = Formatter {
            name: "sh",
            bin: PathBuf::from("sh"),
            args: vec!["-c".into(), "echo 'i schema mismatch' >&2; echo out".into()],
            cwd: PathBuf::from("/"),
        };
        assert_eq!(run(&noisy, "").unwrap(), "out\n");
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
        assert_eq!(run(&f, "ok\n").unwrap(), "ok\n");
        let mut outer = f.clone();
        outer.cwd = base.clone();
        assert!(run(&outer, "ok\n").unwrap_err().contains("nested root"));
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
}
