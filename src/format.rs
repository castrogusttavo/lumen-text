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
    if let Some(root) = find_up(dir, &["biome.json", "biome.jsonc"]) {
        if let Some(bin) = local_bin(&root, "biome") {
            return Some(Formatter { name: "biome", bin, args: vec!["format".into(), format!("--stdin-file-path={path}")] });
        }
    }
    let prettier_cfgs = [".prettierrc", ".prettierrc.json", ".prettierrc.yaml", ".prettierrc.yml", ".prettierrc.js", ".prettierrc.cjs", ".prettierrc.mjs", "prettier.config.js", "prettier.config.cjs", "prettier.config.mjs", "prettier.config.ts"];
    if let Some(root) = find_up(dir, &prettier_cfgs) {
        if let Some(bin) = local_bin(&root, "prettier") {
            return Some(Formatter { name: "prettier", bin, args: vec![format!("--stdin-filepath={path}")] });
        }
    }
    None
}

/// Format `text`; returns the new text, or an error message (formatter output on failure).
pub fn run(f: &Formatter, cwd: &Path, text: &str) -> Result<String, String> {
    let mut child = Command::new(&f.bin)
        .args(&f.args)
        .current_dir(cwd)
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
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("falhou");
        Err(format!("{}: {first}", f.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let f = Formatter { name: "tr", bin: PathBuf::from("tr"), args: vec!["a-z".into(), "A-Z".into()] };
        assert_eq!(run(&f, Path::new("/tmp"), "hello\n").unwrap(), "HELLO\n");
        let big = "x".repeat(2_000_000);
        assert_eq!(run(&f, Path::new("/tmp"), &big).unwrap().len(), 2_000_000, "no pipe deadlock");
        let bad = Formatter { name: "sh", bin: PathBuf::from("sh"), args: vec!["-c".into(), "echo boom >&2; exit 3".into()] };
        assert!(run(&bad, Path::new("/tmp"), "").unwrap_err().contains("boom"));
    }
}
