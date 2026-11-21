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

/// Lines added / removed in one file (`None`: a binary file, which git reports as `-`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineStat {
    pub add: Option<u32>,
    pub del: Option<u32>,
}

/// Per-file line counts of the Staged side (`git diff --cached`) and the Unstaged side
/// (`git diff`), by repository-relative path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NumStats {
    pub staged: std::collections::HashMap<String, LineStat>,
    pub unstaged: std::collections::HashMap<String, LineStat>,
}

impl NumStats {
    /// The counts of a change (untracked files have none).
    pub fn of(&self, c: &Change) -> Option<LineStat> {
        match c.group {
            Group::Staged => self.staged.get(&c.path).copied(),
            Group::Changes => self.unstaged.get(&c.path).copied(),
            Group::Untracked => None,
        }
    }
}

/// Parse `git diff --numstat -z --no-renames`: `added<TAB>removed<TAB>path<NUL>` per file, with
/// `-` for both counts of a binary file.
pub fn parse_numstat(out: &str) -> std::collections::HashMap<String, LineStat> {
    let mut m = std::collections::HashMap::new();
    for rec in out.split('\0') {
        let mut it = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (it.next(), it.next(), it.next()) else { continue };
        if path.is_empty() {
            continue;
        }
        let num = |s: &str| s.trim().parse::<u32>().ok();
        m.insert(path.to_string(), LineStat { add: num(a), del: num(d) });
    }
    m
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
        let old = (matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C')).then(|| it.next().unwrap_or("").to_string());
        push_change(&mut v, x, y, &e[3..], old);
    }
    v.sort_by(|a, b| (a.group as u8, &a.path).cmp(&(b.group as u8, &b.path)));
    v
}

/// The entries of one status record: its `XY` pair (a space for "unchanged"), path and old path.
fn push_change(v: &mut Vec<Change>, x: char, y: char, path: &str, old: Option<String>) {
    let mk = |code: char, group: Group| Change { path: path.to_string(), old: old.clone(), code, group };
    if x == '?' && y == '?' {
        v.push(mk('?', Group::Untracked));
    } else if x == '!' {
        // Ignored: never listed.
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

/// Arguments of the one `git status` the panel runs: the changes (every untracked file) plus the
/// branch headers (upstream, ahead / behind). `--no-optional-locks`: it runs on a worker thread
/// and must not hold `index.lock` while the user stages or commits.
pub const STATUS_V2_ARGS: [&str; 7] =
    ["--no-optional-locks", "status", "--porcelain=v2", "--branch", "--ahead-behind", "-z", "-uall"];

/// What `git status --porcelain=v2 --branch -z` says: the changes (as [`parse_status_z`] lists
/// them) and where the branch stands against its upstream (as `gitsync::info` reports it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusV2 {
    pub changes: Vec<Change>,
    pub sync: SyncInfo,
}

/// `+3 -1` of the `# branch.ab` header: (ahead, behind).
fn parse_ab(s: &str) -> Option<(usize, usize)> {
    let mut it = s.split_whitespace();
    let ahead = it.next()?.strip_prefix('+')?.parse().ok()?;
    let behind = it.next()?.strip_prefix('-')?.parse().ok()?;
    Some((ahead, behind))
}

/// Parse `git status --porcelain=v2 --branch -z`. Records end with NUL; a rename / copy is
/// followed by one more field, its old path. The upstream only counts when git could compare
/// with it (`# branch.ab` is missing when the upstream branch is gone), like `gitsync::info`.
pub fn parse_status_v2(out: &str) -> StatusV2 {
    let mut v = Vec::new();
    let (mut upstream, mut ab) = (None, None);
    let mut it = out.split('\0');
    while let Some(e) = it.next() {
        if let Some(h) = e.strip_prefix("# ") {
            if let Some(u) = h.strip_prefix("branch.upstream ") {
                upstream = Some(u.trim().to_string()).filter(|u| !u.is_empty());
            } else if let Some(c) = h.strip_prefix("branch.ab ") {
                ab = parse_ab(c);
            }
            continue;
        }
        let Some((kind, rest)) = e.split_once(' ') else { continue };
        // Fields up to and including the path: `XY sub mH mI mW hH hI path` for an ordinary
        // entry, one more (the score) for a rename, two more (stages) for an unmerged one.
        let n = match kind {
            "?" => {
                push_change(&mut v, '?', '?', rest, None);
                continue;
            }
            "1" => 8,
            "2" => 9,
            "u" => 10,
            _ => continue,
        };
        // The old path follows a rename even when the record itself is malformed.
        let old = (kind == "2").then(|| it.next().unwrap_or("").to_string());
        let f: Vec<&str> = rest.splitn(n, ' ').collect();
        let xy = f[0].as_bytes();
        if f.len() < n || xy.len() != 2 {
            continue;
        }
        // v2 writes `.` where v1 writes a space.
        let flag = |b: u8| if b == b'.' { ' ' } else { b as char };
        push_change(&mut v, flag(xy[0]), flag(xy[1]), f[n - 1], old);
    }
    v.sort_by(|a, b| (a.group as u8, &a.path).cmp(&(b.group as u8, &b.path)));
    let sync = match (upstream, ab) {
        (Some(u), Some((ahead, behind))) => SyncInfo { upstream: Some(u), ahead, behind },
        _ => SyncInfo::default(),
    };
    StatusV2 { changes: v, sync }
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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("PATH", crate::lspreg::git_hook_path(dir))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let pick = |b: &[u8]| pick_error_line(&String::from_utf8_lossy(b));
    Err(pick(&out.stderr).or_else(|| pick(&out.stdout)).unwrap_or_else(|| "git failed".into()))
}

/// The most useful line of a failed git run's output. Hooks print a lot: the first line says what ran,
/// the last one why git failed. A husky hook ends with its own "husky - <hook> script failed" line,
/// which hides the tool's reason (a commitlint "✖ …" line, say), so that reason leads and husky's follows.
pub fn pick_error_line(out: &str) -> Option<String> {
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(l) = lines.iter().find(|l| l.starts_with("error:") || l.starts_with("fatal:")) {
        return Some(l.to_string());
    }
    let last = *lines.last()?;
    if last.starts_with("husky - ")
        && let Some(why) = lines.iter().find(|l| l.starts_with('✖') || l.starts_with('×') || l.starts_with("error"))
    {
        return Some(format!("{why} ({last})"));
    }
    Some(last.to_string())
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
    git_run(top, &["commit", "-m", &crate::commitmsg::prepare(top, msg)])
        .map(|o| o.lines().next().unwrap_or("commit created").to_string())
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

#[cfg(test)]
mod pick_tests {
    use super::pick_error_line;

    #[test]
    fn a_husky_failure_names_the_tool_s_reason() {
        let out = "input: long line\n✖   body's lines must not be longer than 100 characters [body-max-line-length]\n\n✖   found 1 problems\nhusky - commit-msg script failed (code 1)\n";
        assert_eq!(
            pick_error_line(out).unwrap(),
            "✖   body's lines must not be longer than 100 characters [body-max-line-length] (husky - commit-msg script failed (code 1))"
        );
        assert_eq!(pick_error_line("nothing to commit\n").unwrap(), "nothing to commit");
        assert_eq!(pick_error_line("a\nfatal: bad\nb\n").unwrap(), "fatal: bad");
        assert_eq!(pick_error_line(""), None);
    }
}
