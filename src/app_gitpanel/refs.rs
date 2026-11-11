//! Branches, tags, cherry-pick, "undo last commit" and GitHub releases: plain functions over
//! `git` (and `gh` for the availability check), covered by tests against temporary repositories.
//! The App glue (palette, prompts, confirmation cards) lives in `actions.rs`.

use super::*;

/// What the repository is in the middle of (from the marker files in the git directory).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoState {
    Clean,
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl RepoState {
    pub fn busy_text(self) -> &'static str {
        match self {
            RepoState::Clean => "",
            RepoState::Merge => "a merge is in progress: finish it (commit) or abort it first",
            RepoState::Rebase => "a rebase is in progress: finish it or abort it first",
            RepoState::CherryPick => "a cherry-pick is in progress: continue or abort it first",
            RepoState::Revert => "a revert is in progress: finish it or abort it first",
        }
    }
}

/// The repository's git directory (`.git`, or the per-worktree one), absolute.
pub fn git_dir(top: &Path) -> Option<PathBuf> {
    let d = git_run(top, &["rev-parse", "--git-dir"]).ok()?;
    let d = PathBuf::from(d.trim());
    Some(if d.is_absolute() { d } else { top.join(d) })
}

pub fn repo_state(top: &Path) -> RepoState {
    let Some(d) = git_dir(top) else { return RepoState::Clean };
    if d.join("rebase-merge").exists() || d.join("rebase-apply").exists() {
        RepoState::Rebase
    } else if d.join("CHERRY_PICK_HEAD").exists() {
        RepoState::CherryPick
    } else if d.join("MERGE_HEAD").exists() {
        RepoState::Merge
    } else if d.join("REVERT_HEAD").exists() {
        RepoState::Revert
    } else {
        RepoState::Clean
    }
}

fn exists(top: &Path, refname: &str) -> bool {
    git_run(top, &["show-ref", "--verify", "--quiet", refname]).is_ok()
}

/// Checked-out branch; None on a detached HEAD.
pub fn current_branch(top: &Path) -> Option<String> {
    git_run(top, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ───────────── branches ─────────────

/// A valid name for a new branch (trimmed), or why not.
pub fn check_branch_name(top: &Path, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name for the branch".into());
    }
    if name.starts_with('-') || git_run(top, &["check-ref-format", "--branch", name]).is_err() {
        return Err(format!("“{name}” is not a valid branch name"));
    }
    if exists(top, &format!("refs/heads/{name}")) {
        return Err(format!("A branch named “{name}” already exists"));
    }
    Ok(name.to_string())
}

/// `git switch -c <name> [<from>]`: create the branch (at HEAD, or at the commit `from`) and check
/// it out. Local changes travel along.
pub fn create_branch(top: &Path, name: &str, from: Option<&str>) -> Result<String, String> {
    let name = check_branch_name(top, name)?;
    let mut args = vec!["switch", "-c", name.as_str()];
    args.extend(from);
    git_run(top, &args)?;
    Ok(match from {
        Some(c) => format!("Created and switched to “{name}” (from {})", short(c)),
        None => format!("Created and switched to “{name}”"),
    })
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(7)]
}

/// Local branches, most recently committed first.
pub fn local_branches(top: &Path) -> Vec<String> {
    git_run(top, &["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)", "refs/heads"])
        .map(|o| o.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// The upstream a local branch tracks: (remote, branch name on the remote). None when it has none
/// (or tracks another local branch).
pub fn upstream_of(top: &Path, branch: &str) -> Option<(String, String)> {
    let get = |k: &str| git_run(top, &["config", "--get", &format!("branch.{branch}.{k}")]).ok();
    let remote = get("remote")?.trim().to_string();
    let merge = get("merge")?.trim().to_string();
    let name = merge.strip_prefix("refs/heads/")?.to_string();
    (remote != "." && !remote.is_empty()).then_some((remote, name))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteFail {
    /// `git branch -d` refused: the branch has commits that are not merged anywhere.
    Unmerged,
    Other(String),
}

/// `git branch -d` (or `-D` when `force`). The checked-out branch is never deleted.
pub fn delete_branch(top: &Path, name: &str, force: bool) -> Result<String, DeleteFail> {
    if current_branch(top).as_deref() == Some(name) {
        return Err(DeleteFail::Other(format!("“{name}” is checked out: switch to another branch first")));
    }
    match git_run(top, &["branch", if force { "-D" } else { "-d" }, "--", name]) {
        Ok(_) => Ok(format!("Branch “{name}” deleted")),
        Err(e) if e.contains("not fully merged") => Err(DeleteFail::Unmerged),
        Err(e) => Err(DeleteFail::Other(e)),
    }
}

// ───────────── tags ─────────────

/// A valid name for a new tag (trimmed), or why not.
pub fn check_tag_name(top: &Path, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name for the tag".into());
    }
    if name.starts_with('-') || git_run(top, &["check-ref-format", &format!("refs/tags/{name}")]).is_err() {
        return Err(format!("“{name}” is not a valid tag name"));
    }
    if exists(top, &format!("refs/tags/{name}")) {
        return Err(format!("A tag named “{name}” already exists"));
    }
    Ok(name.to_string())
}

/// Arguments of `git tag`: annotated (`-a -m`) when there is a message, lightweight otherwise.
pub fn tag_args(name: &str, message: Option<&str>, at: Option<&str>) -> Vec<String> {
    let mut v = vec!["tag".to_string()];
    if let Some(m) = message.map(str::trim).filter(|m| !m.is_empty()) {
        v.extend(["-a".into(), "-m".into(), m.to_string()]);
    }
    v.push(name.to_string());
    v.extend(at.map(str::to_string));
    v
}

/// Create a tag on HEAD (or on the commit `at`).
pub fn create_tag(top: &Path, name: &str, message: Option<&str>, at: Option<&str>) -> Result<String, String> {
    let name = check_tag_name(top, name)?;
    run_owned(top, &tag_args(&name, message, at))?;
    let what = if message.is_some_and(|m| !m.trim().is_empty()) { "Annotated tag" } else { "Tag" };
    Ok(match at {
        Some(c) => format!("{what} “{name}” created on {}", short(c)),
        None => format!("{what} “{name}” created"),
    })
}

/// Tags, newest first: (name, subject of the commit / annotation).
pub fn list_tags(top: &Path) -> Vec<(String, String)> {
    git_run(top, &["tag", "--sort=-creatordate", "--format=%(refname:short)%09%(subject)"])
        .map(|o| {
            o.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let (n, s) = l.split_once('\t').unwrap_or((l, ""));
                    (n.to_string(), s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn delete_tag(top: &Path, name: &str) -> Result<String, String> {
    git_run(top, &["tag", "-d", name]).map(|_| format!("Tag “{name}” deleted"))
}

/// Parse `for-each-ref --format=%(objectname)%09%(*objectname)%09%(refname:short) refs/tags`:
/// commit hash -> its tags (annotated tags are peeled to the commit they point at).
pub fn parse_tag_map(out: &str) -> std::collections::HashMap<String, Vec<String>> {
    let mut m: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for l in out.lines() {
        let mut it = l.split('\t');
        let (Some(obj), Some(peeled), Some(name)) = (it.next(), it.next(), it.next()) else { continue };
        let commit = if peeled.trim().is_empty() { obj } else { peeled };
        if !commit.trim().is_empty() && !name.is_empty() {
            m.entry(commit.trim().to_string()).or_default().push(name.to_string());
        }
    }
    m
}

pub fn tag_map(top: &Path) -> std::collections::HashMap<String, Vec<String>> {
    let fmt = "--format=%(objectname)%09%(*objectname)%09%(refname:short)";
    git_run(top, &["for-each-ref", fmt, "refs/tags"]).map(|o| parse_tag_map(&o)).unwrap_or_default()
}

/// A next version from the newest tag that looks like one (`v1.2.3` -> `v1.2.4`); `v0.1.0` when
/// there is none.
pub fn suggest_next_tag(tags: &[String]) -> String {
    for t in tags {
        let (pre, rest) = if let Some(r) = t.strip_prefix('v') { ("v", r) } else { ("", t.as_str()) };
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() == 3
            && let (Ok(a), Ok(b), Ok(c)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>(), parts[2].parse::<u64>())
        {
            return format!("{pre}{a}.{b}.{}", c + 1);
        }
    }
    "v0.1.0".into()
}

// ───────────── cherry-pick ─────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CherryFail {
    /// Stopped on conflicting files: the repository is now in the cherry-pick state.
    Conflict,
    Other(String),
}

/// `git cherry-pick <hash>` onto the checked-out branch.
pub fn cherry_pick(top: &Path, hash: &str) -> Result<String, CherryFail> {
    let st = repo_state(top);
    if st != RepoState::Clean {
        return Err(CherryFail::Other(format!("Cannot cherry-pick: {}", st.busy_text())));
    }
    let parents = git_run(top, &["rev-list", "--parents", "-n", "1", hash])
        .map(|o| o.split_whitespace().count().saturating_sub(1))
        .map_err(CherryFail::Other)?;
    if parents > 1 {
        return Err(CherryFail::Other("Merge commits cannot be cherry-picked from here".into()));
    }
    if git_run(top, &["merge-base", "--is-ancestor", hash, "HEAD"]).is_ok() {
        return Err(CherryFail::Other(format!("{} is already part of the current branch", short(hash))));
    }
    match git_run(top, &["cherry-pick", hash]) {
        Ok(o) => Ok(o.lines().next().map_or_else(|| "Cherry-pick done".into(), |l| l.trim().to_string())),
        Err(_) if repo_state(top) == RepoState::CherryPick => Err(CherryFail::Conflict),
        Err(e) => Err(CherryFail::Other(e)),
    }
}

/// After resolving and staging the conflicts: commit with the original message (no editor).
pub fn cherry_continue(top: &Path) -> Result<String, String> {
    git_run(top, &["-c", "core.editor=true", "cherry-pick", "--continue"]).map(|_| "Cherry-pick completed".into())
}

pub fn cherry_abort(top: &Path) -> Result<String, String> {
    git_run(top, &["cherry-pick", "--abort"]).map(|_| "Cherry-pick aborted".into())
}

/// Commits that the checked-out branch does not have (other branches, remotes, tags), newest first.
pub fn other_commits(top: &Path, n: usize) -> Vec<LogEntry> {
    let n = format!("--max-count={n}");
    git_run(top, &["log", "--no-merges", &n, LOG_FORMAT, "--exclude=refs/stash", "--all", "--not", "HEAD"])
        .map(|o| parse_log(&o))
        .unwrap_or_default()
}

// ───────────── undo last commit ─────────────

/// Is the last commit already on the upstream (undoing it would then need a force push)?
pub fn head_published(top: &Path) -> bool {
    git_run(top, &["rev-parse", "--verify", "-q", "@{u}"]).is_ok()
        && git_run(top, &["merge-base", "--is-ancestor", "HEAD", "@{u}"]).is_ok()
}

/// Why the last commit cannot be undone right now, if it cannot.
pub fn undo_blocker(top: &Path) -> Option<String> {
    let st = repo_state(top);
    if st != RepoState::Clean {
        return Some(format!("Cannot undo the last commit: {}", st.busy_text()));
    }
    if git_run(top, &["rev-parse", "--verify", "-q", "HEAD"]).is_err() {
        return Some("There is no commit to undo".into());
    }
    if git_run(top, &["rev-parse", "--verify", "-q", "HEAD~1"]).is_err() {
        return Some("The first commit of the repository cannot be undone".into());
    }
    None
}

/// `git reset --soft HEAD~1`: the last commit goes away, its changes stay staged. Returns its
/// message (it goes back into the commit box). Only ever the last commit.
pub fn undo_last_commit(top: &Path) -> Result<String, String> {
    if let Some(why) = undo_blocker(top) {
        return Err(why);
    }
    let msg = git_run(top, &["log", "-1", "--format=%B", "HEAD"])?.trim_end().to_string();
    git_run(top, &["reset", "--soft", "HEAD~1"])?;
    Ok(msg)
}

// ───────────── GitHub releases (gh) ─────────────

/// `owner/repo` of a GitHub remote address (https, ssh or scp-like); None for other hosts.
pub fn github_slug(url: &str) -> Option<String> {
    let u = url.trim();
    let rest =
        ["https://github.com/", "http://github.com/", "ssh://git@github.com/", "git@github.com:", "git://github.com/"]
            .iter()
            .find_map(|p| u.strip_prefix(*p))
            .or_else(|| {
                // https://user@github.com/owner/repo
                let i = u.find("@github.com/")?;
                if u.starts_with("https://") { Some(&u[i + "@github.com/".len()..]) } else { None }
            })?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut it = rest.split('/');
    let (owner, repo) = (it.next()?, it.next()?);
    (!owner.is_empty() && !repo.is_empty() && it.next().is_none()).then(|| format!("{owner}/{repo}"))
}

/// Remotes pointing at GitHub: (remote, owner/repo). `prefer` (the push remote) comes first.
pub fn github_remotes(top: &Path, prefer: Option<&str>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = git_run(top, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let url = git_run(top, &["remote", "get-url", r]).ok()?;
            github_slug(&url).map(|s| (r.to_string(), s))
        })
        .collect();
    if let Some(p) = prefer
        && let Some(i) = v.iter().position(|(r, _)| r == p)
    {
        let e = v.remove(i);
        v.insert(0, e);
    }
    v
}

/// Can a release be made with the `gh` program?
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhStatus {
    Ready,
    /// `gh` is not installed (not on PATH).
    Missing,
    /// Installed but not logged in to github.com.
    NoAuth,
}

impl GhStatus {
    pub fn text(&self) -> &'static str {
        match self {
            GhStatus::Ready => "",
            GhStatus::Missing => {
                "Creating releases needs the GitHub CLI (gh): install it from https://cli.github.com and run “gh auth login”"
            }
            GhStatus::NoAuth => "The GitHub CLI is not logged in: run “gh auth login” in a terminal, then try again",
        }
    }
}

/// Check `program` (normally "gh") without touching the network: `--version` says it is
/// installed; `auth token` succeeds only with stored credentials (its output is discarded).
pub fn gh_status(program: &str) -> GhStatus {
    let run = |args: &[&str]| {
        Command::new(program)
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .stdin(std::process::Stdio::null())
            .output()
    };
    match run(&["--version"]) {
        Ok(o) if o.status.success() => {}
        _ => return GhStatus::Missing,
    }
    match run(&["auth", "token", "--hostname", "github.com"]) {
        Ok(o) if o.status.success() => GhStatus::Ready,
        _ => GhStatus::NoAuth,
    }
}

/// The confirmation card's text for a release: exactly what will be published, where.
pub fn release_summary(p: &gitsync::ReleasePlan, new_tag_at: Option<&str>) -> String {
    let tag = match new_tag_at {
        Some(c) => format!("new tag “{}” on {} (created now and pushed to {})", p.tag, short(c), p.remote),
        None => format!("tag “{}” (pushed to {} if it is not there yet)", p.tag, p.remote),
    };
    let notes = match p.notes.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => {
            let n: String = n.chars().take(120).collect();
            format!("notes: “{n}”")
        }
        None => "notes generated by GitHub".into(),
    };
    format!(
        "Publish a GitHub release on github.com/{}? Title “{}”, {tag}, {notes}. Runs gh release create; it is visible to everyone who can see the repository.",
        p.slug, p.title
    )
}
