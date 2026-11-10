//! ESLint diagnostics through the project's own `eslint` CLI (`--stdin`, JSON output): no
//! language-server install needed. Runs on a worker thread so typing never waits for it.

use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub line: usize, // 1-based
    pub col: usize,  // 1-based, UTF-16 units
    pub end_line: usize,
    pub end_col: usize,
    pub severity: u8, // 1 warning, 2 error (ESLint numbering)
    pub message: String,
    pub rule: String,
}

const CONFIGS: &[&str] = &[
    "eslint.config.js", "eslint.config.mjs", "eslint.config.cjs", "eslint.config.ts", ".eslintrc", ".eslintrc.js", ".eslintrc.cjs",
    ".eslintrc.json", ".eslintrc.yaml", ".eslintrc.yml",
];

/// (project root, eslint binary) when the project is configured for ESLint and has it installed.
pub fn detect(file: &Path) -> Option<(PathBuf, PathBuf)> {
    let dir = file.parent()?;
    let root = dir.ancestors().find(|d| CONFIGS.iter().any(|c| d.join(c).exists()))?.to_path_buf();
    let bin = root.ancestors().map(|d| d.join("node_modules/.bin/eslint")).find(|p| p.exists())?;
    Some((root, bin))
}

pub fn parse(json: &str) -> Vec<Msg> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let mut out = Vec::new();
    for file in v.as_array().into_iter().flatten() {
        for m in file["messages"].as_array().into_iter().flatten() {
            let line = m["line"].as_u64().unwrap_or(1) as usize;
            let col = m["column"].as_u64().unwrap_or(1) as usize;
            out.push(Msg {
                line,
                col,
                end_line: m["endLine"].as_u64().map_or(line, |v| v as usize),
                end_col: m["endColumn"].as_u64().map_or(col + 1, |v| v as usize),
                severity: m["severity"].as_u64().unwrap_or(1) as u8,
                message: m["message"].as_str().unwrap_or("").to_string(),
                rule: m["ruleId"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    out
}

/// Lint `text` as if it were `file`. Blocking: call from a worker thread.
pub fn run(root: &Path, bin: &Path, file: &Path, text: &str) -> Result<Vec<Msg>, String> {
    let mut child = Command::new(bin)
        .args(["--stdin", "--stdin-filename"])
        .arg(file)
        .args(["-f", "json"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().unwrap();
    let input = text.to_string();
    let w = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = w.join();
    // ESLint exits 1 when it found problems; the JSON on stdout is what matters.
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().find(|l| !l.trim().is_empty()).unwrap_or("sem saída").to_string());
    }
    Ok(parse(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_eslint_json() {
        let j = r#"[{"filePath":"/x/a.ts","messages":[
            {"ruleId":"no-unused-vars","severity":2,"message":"'x' is defined but never used.","line":3,"column":7,"endLine":3,"endColumn":8},
            {"ruleId":null,"severity":1,"message":"Parsing warning","line":1,"column":1}]}]"#;
        let m = parse(j);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].line, m[0].col, m[0].end_col, m[0].severity), (3, 7, 8, 2));
        assert_eq!(m[0].rule, "no-unused-vars");
        assert_eq!((m[1].end_line, m[1].end_col, m[1].rule.as_str()), (1, 2, ""), "missing end positions default to one column");
        assert!(parse("garbage").is_empty());
    }

    #[test]
    fn detection_needs_config_and_binary() {
        let root = std::env::temp_dir().join(format!("lumen-eslint-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/a.ts");
        assert!(detect(&file).is_none());
        std::fs::write(root.join("eslint.config.js"), "").unwrap();
        assert!(detect(&file).is_none(), "config without binary");
        std::fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        std::fs::write(root.join("node_modules/.bin/eslint"), "").unwrap();
        assert_eq!(detect(&file).unwrap().0, root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn run_reads_stdout_json_even_when_exit_status_is_failure() {
        let dir = std::env::temp_dir().join(format!("lumen-eslint-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("eslint");
        std::fs::write(&fake, "#!/bin/sh\ncat >/dev/null\necho '[{\"messages\":[{\"ruleId\":\"r\",\"severity\":2,\"message\":\"m\",\"line\":2,\"column\":3}]}]'\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let m = run(&dir, &fake, Path::new("a.ts"), "x").unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].line, m[0].col), (2, 3));
        std::fs::remove_dir_all(&dir).ok();
    }
}
