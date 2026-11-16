//! Commit messages that pass the repository's commit linter without the user re-typing them.
//!
//! A project with commitlint (conventional commits) rejects any body line over 100 characters, and
//! the IDE's commit box keeps a long sentence on one line. So when the repository has a commitlint
//! config, plain body paragraphs that have a line over the limit are re-wrapped. Lists, indented or
//! quoted text, code fences and footers are never touched; neither is the header.

use std::path::Path;

/// The limit commitlint's conventional preset puts on body and footer lines.
const LIMIT: usize = 100;
/// Where the re-wrapped lines break (the width people write bodies at).
const WIDTH: usize = 80;

const CONFIGS: &[&str] = &[
    "commitlint.config.js",
    "commitlint.config.cjs",
    "commitlint.config.mjs",
    "commitlint.config.ts",
    "commitlint.config.json",
    ".commitlintrc",
    ".commitlintrc.js",
    ".commitlintrc.cjs",
    ".commitlintrc.mjs",
    ".commitlintrc.ts",
    ".commitlintrc.json",
    ".commitlintrc.yaml",
    ".commitlintrc.yml",
];

fn has_commitlint(top: &Path) -> bool {
    CONFIGS.iter().any(|c| top.join(c).is_file())
        || std::fs::read_to_string(top.join("package.json")).is_ok_and(|p| p.contains("\"commitlint\""))
}

/// The message to commit: trimmed, and re-wrapped when the repository lints commit messages.
pub fn prepare(top: &Path, msg: &str) -> String {
    let msg = msg.trim();
    if has_commitlint(top) { wrap_body(msg, LIMIT, WIDTH) } else { msg.to_string() }
}

fn is_plain(line: &str) -> bool {
    let t = line.trim_start();
    if t.len() != line.len() || t.is_empty() {
        return false;
    }
    let list = t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ");
    let numbered = t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    // `Key: value` / `Key #12` trailers (Co-Authored-By, Refs, BREAKING CHANGE) stay as written.
    let footer = t
        .split_once(": ")
        .or_else(|| t.split_once(" #"))
        .is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ' '));
    !(list || numbered || footer || t.starts_with("```") || t.starts_with('>') || t.starts_with('#'))
}

/// Re-wrap the body paragraphs of `msg` that have a line longer than `limit` to `width`.
pub fn wrap_body(msg: &str, limit: usize, width: usize) -> String {
    let lines: Vec<&str> = msg.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut fenced = false;
    let mut i = 0;
    // The header is the first line; the body starts after it.
    if let Some(h) = lines.first() {
        out.push(h.to_string());
        i = 1;
    }
    while i < lines.len() {
        if lines[i].trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if fenced || lines[i].trim().is_empty() {
            out.push(lines[i].to_string());
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].trim_start().starts_with("```") {
            i += 1;
        }
        let para = &lines[start..i];
        let long = para.iter().any(|l| l.chars().count() > limit);
        if long && para.iter().all(|l| is_plain(l)) {
            out.extend(wrap_words(&para.join(" "), width));
        } else {
            out.extend(para.iter().map(|l| l.to_string()));
        }
    }
    out.join("\n")
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for w in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: &str = "The updates route imported upsertLink from @platejs/link by mistake, an editor auto-import. The update factories test indexed the module namespace dynamically, which noDynamicNamespaceImportAccess rejects; it now references each factory.";

    #[test]
    fn a_long_body_line_is_wrapped_under_the_limit() {
        let msg = format!("fix(updates): drop a stray import\n\n{LONG}");
        let out = wrap_body(&msg, 100, 80);
        assert!(out.lines().all(|l| l.chars().count() <= 80), "{out}");
        assert_eq!(out.lines().next(), Some("fix(updates): drop a stray import"));
        assert_eq!(
            out.split_whitespace().skip(5).collect::<Vec<_>>().join(" "),
            msg.split_whitespace().skip(5).collect::<Vec<_>>().join(" ")
        );
    }

    #[test]
    fn short_lists_code_and_footers_are_left_alone() {
        let msg = format!(
            "feat: x\n\nshort line\nkept as is\n\n- {LONG}\n\n    {LONG}\n\n```\n{LONG}\n```\n\nCo-Authored-By: A <a@b.c>"
        );
        assert_eq!(wrap_body(&msg, 100, 80), msg);
        assert_eq!(wrap_body("fix: a very long header only", 5, 3), "fix: a very long header only");
    }

    #[test]
    fn only_repositories_with_commitlint_are_rewrapped() {
        let d = std::env::temp_dir().join(format!("lumen-commitmsg-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let msg = format!("fix: a\n\n{LONG}\n");
        assert_eq!(prepare(&d, &msg), msg.trim());
        std::fs::write(d.join("commitlint.config.js"), "module.exports={}").unwrap();
        assert!(prepare(&d, &msg).lines().all(|l| l.chars().count() <= 80));
        std::fs::remove_dir_all(&d).ok();
    }
}
