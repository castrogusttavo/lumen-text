use super::*;

// ───────────────────────────── logic ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Group {
    Staged,
    Changes,
    Untracked,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Staged => "Staged",
            Group::Changes => "Unstaged",
            Group::Untracked => "Untracked",
        }
    }
}

/// One entry of `git status`. A file that is staged *and* modified again has TWO entries (one under
/// Staged, one under Unstaged), like in VS Code, so "Add all" picks up the new edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Repository-root-relative path.
    pub path: String,
    /// Previous path of a rename / copy.
    pub old: Option<String>,
    /// M A D R C T U or '?'.
    pub code: char,
    pub group: Group,
}

impl Change {
    /// Has working-tree changes that "discard" can throw away.
    pub fn discardable(&self) -> bool {
        self.group != Group::Staged
    }
}

/// State of a row's checkbox: staged (the Staged group) or not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckState {
    Off,
    On,
}

impl Change {
    pub fn check_state(&self) -> CheckState {
        if self.group == Group::Staged { CheckState::On } else { CheckState::Off }
    }
}

/// Parse `git status --porcelain=v1 -z -uall`; sorted by group (Staged, Unstaged, Untracked)
/// then path.
pub fn parse_status_z(out: &str) -> Vec<Change> {
    let mut v = Vec::new();
    let mut it = out.split('\0');
    while let Some(e) = it.next() {
        if e.len() < 4 || !e.is_char_boundary(2) || !e.is_char_boundary(3) {
            continue;
        }
        let (x, y) = (e.as_bytes()[0] as char, e.as_bytes()[1] as char);
        let path = e[3..].to_string();
        let old = (matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C')).then(|| it.next().unwrap_or("").to_string());
        let mk = |code: char, group: Group| Change { path: path.clone(), old: old.clone(), code, group };
        if x == '?' && y == '?' {
            v.push(mk('?', Group::Untracked));
        } else if x == '!' {
            continue;
        } else if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
            v.push(mk('U', Group::Changes));
        } else {
            match (x != ' ', y != ' ') {
                // Staged and modified again: listed under Staged (what the index holds) and under
                // Unstaged (the edits made since).
                (true, true) => {
                    v.push(mk(x, Group::Staged));
                    v.push(mk(y, Group::Changes));
                }
                (true, false) => v.push(mk(x, Group::Staged)),
                (false, true) => v.push(mk(y, Group::Changes)),
                (false, false) => {}
            }
        }
    }
    v.sort_by(|a, b| (a.group as u8, &a.path).cmp(&(b.group as u8, &b.path)));
    v
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Commit time (unix seconds).
    pub ts: i64,
    pub subject: String,
}

pub(super) const LOG_FORMAT: &str = "--pretty=format:%H%x1f%h%x1f%an%x1f%ct%x1f%s";

pub fn parse_log(out: &str) -> Vec<LogEntry> {
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            Some(LogEntry {
                hash: p.next().filter(|h| !h.is_empty())?.to_string(),
                short: p.next()?.to_string(),
                author: p.next()?.to_string(),
                ts: p.next()?.parse().ok()?,
                subject: p.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// A file touched by a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub code: char,
    pub path: String,
    pub old: Option<String>,
}

/// Parse `git diff --name-status -M` lines (`M\tpath`, `R100\told\tnew`).
pub fn parse_name_status(out: &str) -> Vec<CommitFile> {
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let code = p.next()?.chars().next()?;
            let a = p.next()?.to_string();
            match (code, p.next()) {
                ('R' | 'C', Some(b)) => Some(CommitFile { code, path: b.to_string(), old: Some(a) }),
                _ => Some(CommitFile { code, path: a, old: None }),
            }
        })
        .collect()
}

/// Run git in `dir`. Err = the most useful line of its output (stderr first, then stdout: "nothing
/// to commit" goes to stdout).
pub fn git_run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let pick = |b: &[u8]| {
        let s = String::from_utf8_lossy(b).into_owned();
        let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        // Hooks print a lot: the first line says what ran, the last one why it failed.
        lines
            .iter()
            .find(|l| l.starts_with("error:") || l.starts_with("fatal:"))
            .or(lines.last())
            .map(|l| l.to_string())
    };
    Err(pick(&out.stderr).or_else(|| pick(&out.stdout)).unwrap_or_else(|| "git failed".into()))
}

pub(super) fn with_paths(head: &[&str], paths: &[String]) -> Vec<String> {
    head.iter().map(|s| s.to_string()).chain(std::iter::once("--".to_string())).chain(paths.iter().cloned()).collect()
}

pub(super) fn run_owned(dir: &Path, args: &[String]) -> Result<String, String> {
    git_run(dir, &args.iter().map(String::as_str).collect::<Vec<_>>())
}

pub fn stage(top: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    run_owned(top, &with_paths(&["add"], paths)).map(|_| ())
}

pub fn unstage(top: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // `restore --staged` needs a HEAD; in a repository without commits drop from the index instead.
    run_owned(top, &with_paths(&["restore", "--staged"], paths))
        .or_else(|_| run_owned(top, &with_paths(&["rm", "--cached", "-r", "-q"], paths)))
        .map(|_| ())
}

/// Throw away the working-tree changes of one entry (untracked files are deleted).
pub fn discard(top: &Path, c: &Change) -> Result<(), String> {
    match c.group {
        Group::Untracked => std::fs::remove_file(top.join(&c.path)).map_err(|e| e.to_string()),
        // Staged + modified again: restore the working tree from the index (keeps the staged part).
        Group::Changes | Group::Staged if c.discardable() => {
            run_owned(top, &with_paths(&["checkout"], std::slice::from_ref(&c.path))).map(|_| ())
        }
        Group::Changes | Group::Staged => Err("unstage before discarding".into()),
    }
}

/// `git commit -m msg`; returns git's first output line (`[main abc1234] subject`).
pub fn commit(top: &Path, msg: &str) -> Result<String, String> {
    if msg.trim().is_empty() {
        return Err("write a commit message".into());
    }
    git_run(top, &["commit", "-m", msg.trim()]).map(|o| o.lines().next().unwrap_or("commit created").to_string())
}

/// One page of the log, newest first.
#[cfg(test)]
pub fn log_page(top: &Path, skip: usize, n: usize) -> Vec<LogEntry> {
    let (s, n) = (format!("--skip={skip}"), format!("--max-count={n}"));
    git_run(top, &["log", &s, &n, LOG_FORMAT]).map(|o| parse_log(&o)).unwrap_or_default()
}

/// Files touched by `hash` (against its first parent; root commits against the empty tree).
pub fn commit_files(top: &Path, hash: &str) -> Vec<CommitFile> {
    let parents = git_run(top, &["rev-list", "--parents", "-n", "1", hash])
        .map(|o| o.split_whitespace().count().saturating_sub(1))
        .unwrap_or(0);
    let out = if parents == 0 {
        git_run(top, &["diff-tree", "--root", "--no-commit-id", "--name-status", "-r", "-M", hash])
    } else {
        git_run(top, &["diff", "--name-status", "-M", &format!("{hash}^1"), hash])
    };
    out.map(|o| parse_name_status(&o)).unwrap_or_default()
}

pub(super) fn show(top: &Path, rev_path: &str) -> String {
    git_run(top, &["show", rev_path]).unwrap_or_default()
}

#[cfg(test)]
/// (old, new) texts of a working-tree entry: HEAD against the file on disk.
pub fn working_pair(top: &Path, c: &Change) -> (String, String) {
    let old = if c.code == '?' || (c.code == 'A' && c.group == Group::Staged && c.old.is_none()) {
        String::new()
    } else {
        show(top, &format!("HEAD:{}", c.old.as_deref().unwrap_or(&c.path)))
    };
    let new = std::fs::read(top.join(&c.path)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    (old, new)
}

/// (old, new) texts of a file in a commit.
pub fn commit_pair(top: &Path, hash: &str, f: &CommitFile) -> (String, String) {
    let old = if f.code == 'A' {
        String::new()
    } else {
        show(top, &format!("{hash}^1:{}", f.old.as_deref().unwrap_or(&f.path)))
    };
    let new = if f.code == 'D' { String::new() } else { show(top, &format!("{hash}:{}", f.path)) };
    // Root commit: no parent, `show` fails and gives "" which is the right "before".
    (old, new)
}
