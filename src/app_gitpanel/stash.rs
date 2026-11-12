//! Stashes: plain functions over `git stash`, covered by tests against temporary repositories.
//! The App glue (panel rows, prompts, cards, palette) lives in `stash_ui.rs`.
//!
//! Entries are identified by their commit hash, not by `stash@{n}`: the index of an entry moves
//! whenever another stash is pushed or dropped, so it is looked up again right before acting.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashEntry {
    pub sha: String,
    /// `n` of `stash@{n}` when the list was read.
    pub index: usize,
    /// Creation time (unix seconds).
    pub ts: i64,
    /// Branch it was made on ("" when git did not say).
    pub branch: String,
    pub message: String,
}

impl StashEntry {
    pub fn name(&self) -> String {
        format!("stash@{{{}}}", self.index)
    }
}

/// `git stash list` format read by [`parse_stash_list`].
pub(super) const STASH_FORMAT: &str = "--format=%H%x1f%gd%x1f%ct%x1f%gs";

/// "On main: message" / "WIP on main: 1a2b3c4 subject" -> (branch, message).
pub fn split_stash_subject(gs: &str) -> (String, String) {
    for p in ["WIP on ", "On "] {
        if let Some(rest) = gs.strip_prefix(p)
            && let Some((b, m)) = rest.split_once(": ")
        {
            return (b.to_string(), m.to_string());
        }
    }
    (String::new(), gs.to_string())
}

pub fn parse_stash_list(out: &str) -> Vec<StashEntry> {
    out.lines()
        .enumerate()
        .filter_map(|(pos, l)| {
            let mut p = l.split('\x1f');
            let sha = p.next().filter(|h| !h.trim().is_empty())?.trim().to_string();
            let sel = p.next()?;
            let index = sel
                .rsplit_once("@{")
                .and_then(|(_, n)| n.strip_suffix('}'))
                .and_then(|n| n.parse().ok())
                .unwrap_or(pos);
            let ts = p.next()?.parse().unwrap_or(0);
            let (branch, message) = split_stash_subject(p.next().unwrap_or(""));
            Some(StashEntry { sha, index, ts, branch, message })
        })
        .collect()
}

/// Stashes, newest first.
pub fn list_stashes(top: &Path) -> Vec<StashEntry> {
    git_run(top, &["stash", "list", STASH_FORMAT]).map(|o| parse_stash_list(&o)).unwrap_or_default()
}

/// The current `stash@{n}` of the entry whose commit is `sha`.
pub fn stash_ref(top: &Path, sha: &str) -> Option<String> {
    list_stashes(top).into_iter().find(|e| e.sha == sha).map(|e| e.name())
}

/// Arguments of `git stash push`: untracked files too when `untracked`, only `paths` when given.
pub fn stash_push_args(message: &str, paths: &[String], untracked: bool) -> Vec<String> {
    let mut v = vec!["stash".to_string(), "push".to_string()];
    if untracked {
        v.push("--include-untracked".into());
    }
    let m = message.trim();
    if !m.is_empty() {
        v.extend(["-m".to_string(), m.to_string()]);
    }
    if !paths.is_empty() {
        v.push("--".into());
        v.extend(paths.iter().cloned());
    }
    v
}

/// `git stash push`; the sentence to report.
pub fn stash_push(top: &Path, message: &str, paths: &[String], untracked: bool) -> Result<String, String> {
    let out = run_owned(top, &stash_push_args(message, paths, untracked))?;
    if out.contains("No local changes to save") {
        return Err("No local changes to save".into());
    }
    let what = match paths.len() {
        0 if untracked => "all changes (untracked files included)".to_string(),
        0 => "all changes".to_string(),
        1 => paths[0].clone(),
        n => format!("{n} files"),
    };
    let m = message.trim();
    Ok(if m.is_empty() { format!("Stashed {what}") } else { format!("Stashed {what}: “{m}”") })
}

/// Paths with unresolved conflicts (unmerged index entries).
pub fn unmerged_paths(top: &Path) -> Vec<String> {
    git_run(top, &["diff", "--name-only", "--diff-filter=U", "-z"])
        .map(|o| o.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StashFail {
    /// Applied with conflicts in these files (the stash is kept, also by `pop`).
    Conflict(Vec<String>),
    Other(String),
}

/// `git stash apply` (or `pop`) of the entry `sha`.
pub fn stash_apply(top: &Path, sha: &str, pop: bool) -> Result<String, StashFail> {
    let Some(r) = stash_ref(top, sha) else {
        return Err(StashFail::Other("that stash no longer exists".into()));
    };
    if !unmerged_paths(top).is_empty() {
        return Err(StashFail::Other("resolve the conflicting files first (stage them when they are fixed)".into()));
    }
    match git_run(top, &["stash", if pop { "pop" } else { "apply" }, &r]) {
        Ok(_) => Ok(format!("{} {r}", if pop { "Popped" } else { "Applied" })),
        Err(e) => {
            let u = unmerged_paths(top);
            if u.is_empty() { Err(StashFail::Other(e)) } else { Err(StashFail::Conflict(u)) }
        }
    }
}

/// `git stash drop` of the entry `sha`.
pub fn stash_drop(top: &Path, sha: &str) -> Result<String, String> {
    let r = stash_ref(top, sha).ok_or("that stash no longer exists")?;
    git_run(top, &["stash", "drop", &r]).map(|_| format!("Dropped {r}"))
}

/// Files of a stash: tracked changes against the commit it was made on, then its untracked
/// files (code '?').
pub fn stash_files(top: &Path, sha: &str) -> Vec<CommitFile> {
    let mut v = git_run(top, &["diff", "--name-status", "-M", &format!("{sha}^1"), sha])
        .map(|o| parse_name_status(&o))
        .unwrap_or_default();
    let third = format!("{sha}^3");
    if git_run(top, &["rev-parse", "-q", "--verify", &third]).is_ok() {
        let out = git_run(top, &["ls-tree", "-r", "-z", "--name-only", &third]).unwrap_or_default();
        v.extend(out.split('\0').filter(|p| !p.is_empty()).map(|p| CommitFile {
            code: '?',
            path: p.to_string(),
            old: None,
        }));
    }
    v
}

/// (old, new) texts of a file of a stash.
pub fn stash_pair(top: &Path, sha: &str, f: &CommitFile) -> (String, String) {
    if f.code == '?' {
        return (String::new(), show(top, &format!("{sha}^3:{}", f.path)));
    }
    commit_pair(top, sha, f)
}
