//! Git integration by shelling out to `git`: no libgit2, no extra memory.
//! The gutter compares the *buffer* (not the file on disk) against HEAD.

use crate::doc::{GitKind, GitMark};
use similar::{DiffOp, TextDiff};
use std::{path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn toplevel(dir: &Path) -> Option<std::path::PathBuf> {
    let t = git(dir, &["rev-parse", "--show-toplevel"])?;
    let t = t.trim();
    (!t.is_empty()).then(|| std::path::PathBuf::from(t))
}

pub fn branch(dir: &Path) -> Option<String> {
    let b = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let b = b.trim();
    (!b.is_empty()).then(|| b.to_string())
}

/// Content of `path` at HEAD, or None when untracked / not in a repository.
pub fn head_text(path: &Path) -> Option<String> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    git(dir, &["show", &format!("HEAD:./{name}")])
}

/// `git status --porcelain` flags per path, relative to `root`.
pub fn status(root: &Path) -> Vec<(String, String)> {
    let Some(out) = git(root, &["status", "--porcelain", "-uall"]) else { return Vec::new() };
    out.lines().filter(|l| l.len() > 3).map(|l| (l[..2].to_string(), l[3..].to_string())).collect()
}

/// Short relative time ("3 days ago") from seconds ago.
pub fn ago(secs: i64) -> String {
    let (n, unit) = match secs {
        s if s < 60 => return "just now".to_string(),
        s if s < 3600 => (s / 60, if s / 60 == 1 { "minute" } else { "minutes" }),
        s if s < 86_400 => (s / 3600, if s / 3600 == 1 { "hour" } else { "hours" }),
        s if s < 86_400 * 30 => (s / 86_400, if s / 86_400 == 1 { "day" } else { "days" }),
        s if s < 86_400 * 365 => (s / (86_400 * 30), if s / (86_400 * 30) == 1 { "month" } else { "months" }),
        s => (s / (86_400 * 365), if s / (86_400 * 365) == 1 { "year" } else { "years" }),
    };
    format!("{n} {unit} ago")
}

/// "Author, 3 days ago · message" for a 0-based `line` of the committed file, if blame works.
pub fn blame_line(path: &Path, line: usize) -> Option<String> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let l = format!("{0},{0}", line + 1);
    let out = git(dir, &["blame", "-L", &l, "--porcelain", "--", &name])?;
    let field = |k: &str| out.lines().find_map(|x| x.strip_prefix(k)).map(str::to_string);
    let author = field("author ")?;
    if author == "Not Committed Yet" {
        return Some("You · uncommitted changes".to_string());
    }
    let time: i64 = field("author-time ")?.parse().ok()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let summary = field("summary ").unwrap_or_default();
    Some(format!("{author}, {} · {summary}", ago(now - time)))
}

pub fn marks(base: &str, current: &str) -> Vec<GitMark> {
    let diff = TextDiff::from_lines(base, current);
    let mut out = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => {
                out.push(GitMark { line: new_index, len: new_len, kind: GitKind::Added })
            }
            DiffOp::Replace { new_index, new_len, .. } => {
                out.push(GitMark { line: new_index, len: new_len, kind: GitKind::Modified })
            }
            DiffOp::Delete { new_index, .. } => out.push(GitMark { line: new_index, len: 0, kind: GitKind::Deleted }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_and_blame() {
        assert_eq!(ago(30), "just now");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(86_400 * 3), "3 days ago");
        assert_eq!(ago(86_400 * 400), "1 year ago");
        let dir = std::env::temp_dir().join(format!("lumen-blame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |a: &[&str]| {
            Command::new("git").arg("-C").arg(&dir).args(a).output().unwrap();
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "Ana"]);
        std::fs::write(dir.join("f.ts"), "one\ntwo\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "primeiro commit"]);
        let b = blame_line(&dir.join("f.ts"), 1).unwrap();
        assert!(b.starts_with("Ana, ") && b.ends_with("primeiro commit"), "{b}");
        std::fs::write(dir.join("f.ts"), "one\nCHANGED\n").unwrap();
        assert_eq!(blame_line(&dir.join("f.ts"), 1).unwrap(), "You · uncommitted changes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn marks_added_modified_deleted() {
        let base = "a\nb\nc\nd\n";
        let cur = "a\nB\nc\nnew\nd\n";
        let m = marks(base, cur);
        assert!(m.iter().any(|x| x.kind == GitKind::Modified && x.line == 1 && x.len == 1), "{m:?}");
        assert!(m.iter().any(|x| x.kind == GitKind::Added && x.line == 3 && x.len == 1), "{m:?}");
        let m = marks("a\nb\nc\n", "a\nc\n");
        assert!(m.iter().any(|x| x.kind == GitKind::Deleted && x.line == 1), "{m:?}");
        assert!(marks("same\n", "same\n").is_empty());
    }

    #[test]
    fn head_text_and_branch_in_a_real_repo() {
        let dir = std::env::temp_dir().join(format!("lumen-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f.ts"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "x"]);
        assert_eq!(head_text(&dir.join("f.ts")).as_deref(), Some("one\n"));
        assert_eq!(branch(&dir).as_deref(), Some("trunk"));
        assert!(head_text(&dir.join("missing.ts")).is_none());
        std::fs::write(dir.join("f.ts"), "two\n").unwrap();
        assert!(status(&dir).iter().any(|(s, p)| s.contains('M') && p == "f.ts"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
