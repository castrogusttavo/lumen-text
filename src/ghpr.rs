//! GitHub pull request awareness through the `gh` CLI, entirely optional: when `gh` is not
//! installed, not logged in, or the project has no GitHub remote, nothing is shown and nothing is
//! reported. Never a library, never a token handled here: `gh` owns the credentials.
//!
//! * [`snapshot`] (worker thread): the checked-out branch's PR (`gh pr view --json …`) and, for an
//!   open one, its review comments (`gh api repos/{owner}/{repo}/pulls/N/comments`).
//! * [`PrInfo::chip`] / [`PrInfo::tone`]: the status chip ("PR #12 · passing · approved").
//! * [`marker_lines`] / [`comments_markdown`]: gutter markers of a changed file and the popover.
//! * [`create_pr`]: "Create PR" (pushes the branch when it has no upstream).

use crate::gitblame::{md_escape, text_block};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Checks {
    pub pass: u32,
    pub fail: u32,
    pub pending: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review {
    None,
    Approved,
    ChangesRequested,
    Required,
}

/// Color family of the chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Bad,
    Neutral,
    Merged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub url: String,
    pub review: Review,
    pub checks: Checks,
    pub head: String,
}

/// `--json` fields asked of `gh pr view`.
pub const PR_FIELDS: &str = "number,title,state,isDraft,url,reviewDecision,statusCheckRollup,headRefName";

/// Count the checks of `statusCheckRollup` (check runs and legacy commit statuses).
pub fn tally(rollup: &Value) -> Checks {
    let mut c = Checks::default();
    let Some(arr) = rollup.as_array() else { return c };
    for item in arr {
        let get = |k: &str| item[k].as_str().unwrap_or("").to_ascii_uppercase();
        let (conclusion, status, state) = (get("conclusion"), get("status"), get("state"));
        if !state.is_empty() && conclusion.is_empty() && status.is_empty() {
            match state.as_str() {
                "SUCCESS" => c.pass += 1,
                "FAILURE" | "ERROR" => c.fail += 1,
                _ => c.pending += 1,
            }
        } else if status == "COMPLETED" || !conclusion.is_empty() {
            match conclusion.as_str() {
                "SUCCESS" | "NEUTRAL" | "SKIPPED" => c.pass += 1,
                "" => c.pending += 1,
                _ => c.fail += 1,
            }
        } else {
            c.pending += 1;
        }
    }
    c
}

/// Parse the answer of `gh pr view --json PR_FIELDS`.
pub fn parse_pr_view(json: &str) -> Option<PrInfo> {
    let v: Value = serde_json::from_str(json).ok()?;
    let number = v["number"].as_u64()?;
    let state = match v["state"].as_str().unwrap_or("OPEN") {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ if v["isDraft"].as_bool().unwrap_or(false) => PrState::Draft,
        _ => PrState::Open,
    };
    let review = match v["reviewDecision"].as_str().unwrap_or("") {
        "APPROVED" => Review::Approved,
        "CHANGES_REQUESTED" => Review::ChangesRequested,
        "REVIEW_REQUIRED" => Review::Required,
        _ => Review::None,
    };
    Some(PrInfo {
        number,
        title: v["title"].as_str().unwrap_or("").to_string(),
        state,
        url: v["url"].as_str().unwrap_or("").to_string(),
        review,
        checks: tally(&v["statusCheckRollup"]),
        head: v["headRefName"].as_str().unwrap_or("").to_string(),
    })
}

impl PrInfo {
    /// Text of the status chip.
    pub fn chip(&self) -> String {
        let mut parts = vec![format!("PR #{}", self.number)];
        match self.state {
            PrState::Merged => parts.push("merged".into()),
            PrState::Closed => parts.push("closed".into()),
            PrState::Draft | PrState::Open => {
                if self.state == PrState::Draft {
                    parts.push("draft".into());
                }
                let c = self.checks;
                if c.fail > 0 {
                    parts.push("failing".into());
                } else if c.pending > 0 {
                    parts.push("running".into());
                } else if c.pass > 0 {
                    parts.push("passing".into());
                }
                match self.review {
                    Review::Approved => parts.push("approved".into()),
                    Review::ChangesRequested => parts.push("changes requested".into()),
                    Review::Required | Review::None => {}
                }
            }
        }
        parts.join(" · ")
    }

    pub fn tone(&self) -> Tone {
        match self.state {
            PrState::Merged => Tone::Merged,
            PrState::Closed => Tone::Bad,
            PrState::Draft => Tone::Neutral,
            PrState::Open => {
                if self.checks.fail > 0 || self.review == Review::ChangesRequested {
                    Tone::Bad
                } else if self.checks.pending > 0 {
                    Tone::Neutral
                } else if self.checks.pass > 0 || self.review == Review::Approved {
                    Tone::Good
                } else {
                    Tone::Neutral
                }
            }
        }
    }

    /// Tooltip of the chip.
    pub fn tip(&self) -> String {
        let c = self.checks;
        let checks = if c.pass + c.fail + c.pending == 0 {
            "no checks".to_string()
        } else {
            format!("checks: {} passed, {} failed, {} running", c.pass, c.fail, c.pending)
        };
        format!("{} · {checks} · click to open on GitHub", self.title)
    }
}

/// One comment of a pull request review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: u64,
    /// Repository-relative, `/` separated.
    pub path: String,
    /// 1-based line in the PR's current head; None when the code it was made on is gone.
    pub line: Option<usize>,
    pub author: String,
    pub body: String,
    pub url: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub is_reply: bool,
}

/// Parse `gh api repos/{owner}/{repo}/pulls/N/comments`.
pub fn parse_comments(json: &str) -> Vec<ReviewComment> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    items
        .iter()
        .filter_map(|c| {
            Some(ReviewComment {
                id: c["id"].as_u64()?,
                path: c["path"].as_str()?.to_string(),
                line: c["line"].as_u64().map(|l| l as usize),
                author: c["user"]["login"].as_str().unwrap_or("someone").to_string(),
                body: c["body"].as_str().unwrap_or("").to_string(),
                url: c["html_url"].as_str().unwrap_or("").to_string(),
                date: c["created_at"].as_str().unwrap_or("").chars().take(10).collect(),
                is_reply: !c["in_reply_to_id"].is_null(),
            })
        })
        .collect()
}

/// 0-based lines of `rel` that carry a comment, ascending.
pub fn marker_lines(comments: &[ReviewComment], rel: &str) -> Vec<usize> {
    let mut v: Vec<usize> =
        comments.iter().filter(|c| c.path == rel).filter_map(|c| c.line).map(|l| l.saturating_sub(1)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Comments of the 0-based `line` of `rel`, replies included (in the order GitHub gave them).
pub fn comments_on<'a>(comments: &'a [ReviewComment], rel: &str, line: usize) -> Vec<&'a ReviewComment> {
    comments.iter().filter(|c| c.path == rel && c.line.is_some_and(|l| l.saturating_sub(1) == line)).collect()
}

const COMMENTS_SHOWN: usize = 6;

/// The popover of a marker.
pub fn comments_markdown(list: &[&ReviewComment]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for c in list.iter().take(COMMENTS_SHOWN) {
        let mut b =
            format!("**{}** · {}{}\n\n", md_escape(&c.author), c.date, if c.is_reply { " · reply" } else { "" });
        b.push_str(&text_block(&c.body, 14));
        if !c.url.is_empty() {
            b.push_str(&format!("\n\n[Open on GitHub]({})", c.url));
        }
        blocks.push(b);
    }
    if list.len() > COMMENTS_SHOWN {
        blocks.push(format!("… and {} more comments", list.len() - COMMENTS_SHOWN));
    }
    blocks.join("\n\n---\n\n")
}

// ───────────── running gh ─────────────

#[derive(Debug)]
pub enum GhErr {
    /// `gh` is not installed.
    Missing,
    Failed(String),
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Run `gh` in `dir`: never prompts, never checks for updates, no console window on Windows.
pub fn gh(dir: &Path, args: &[String]) -> Result<String, GhErr> {
    let mut cmd = Command::new("gh");
    cmd.current_dir(dir)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { GhErr::Missing } else { GhErr::Failed(e.to_string()) }
    })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("gh failed");
    Err(GhErr::Failed(line.to_string()))
}

/// What the worker found for the project.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The project folder the question was asked for.
    pub root: PathBuf,
    /// Repository root (comment paths are relative to it).
    pub top: PathBuf,
    pub branch: String,
    /// `gh` is not installed: stop asking.
    pub gh_missing: bool,
    pub info: Option<PrInfo>,
    pub comments: Vec<ReviewComment>,
}

/// The PR of the checked-out branch and its review comments. Silent on every failure.
pub fn snapshot(root: &Path) -> Snapshot {
    let mut s = Snapshot {
        root: root.to_path_buf(),
        top: root.to_path_buf(),
        branch: String::new(),
        gh_missing: false,
        info: None,
        comments: Vec::new(),
    };
    let Some(top) = crate::git::toplevel(root) else { return s };
    s.top = top.clone();
    s.branch = crate::git::branch(&top).unwrap_or_default();
    if s.branch.is_empty() || s.branch == "HEAD" {
        return s;
    }
    // Not a GitHub project: gh would only fail, do not even start it.
    if crate::app_gitpanel::github_remotes(&top, None).is_empty() {
        return s;
    }
    match gh(&top, &strs(&["pr", "view", "--json", PR_FIELDS])) {
        Err(GhErr::Missing) => {
            s.gh_missing = true;
            return s;
        }
        Err(GhErr::Failed(_)) => return s,
        Ok(out) => s.info = parse_pr_view(&out),
    }
    if let Some(i) = &s.info
        && matches!(i.state, PrState::Open | PrState::Draft)
    {
        let api = format!("repos/{{owner}}/{{repo}}/pulls/{}/comments?per_page=100", i.number);
        if let Ok(out) = gh(&top, &strs(&["api", &api])) {
            s.comments = parse_comments(&out);
        }
    }
    s
}

/// Arguments of `gh pr create`.
pub fn create_args(title: &str, body: &str, draft: bool) -> Vec<String> {
    let mut v = strs(&["pr", "create", "--title", title, "--body", body]);
    if draft {
        v.push("--draft".into());
    }
    v
}

/// "Create PR": push the branch first when it has no upstream, then `gh pr create`. Returns the
/// address of the new pull request.
pub fn create_pr(root: &Path, title: &str, draft: bool) -> Result<String, String> {
    let top = crate::git::toplevel(root).ok_or("Not a git repository")?;
    let branch = crate::git::branch(&top).filter(|b| b != "HEAD").ok_or("Check out a branch first")?;
    let git = |args: &[&str]| -> Result<String, String> { crate::app_gitpanel::git_run(&top, args) };
    if git(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_err() {
        let remote = crate::gitsync::default_remote(&top, &branch)
            .ok_or("No remote configured (git remote add origin <url>)")?;
        let out = Command::new("git")
            .arg("-C")
            .arg(&top)
            .args(["push", "--set-upstream", &remote, &branch])
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let line = err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("push failed");
            return Err(format!("git push: {line}"));
        }
    }
    let body = git(&["log", "-1", "--format=%b"]).unwrap_or_default();
    match gh(&top, &create_args(title.trim(), body.trim(), draft)) {
        Ok(out) => out
            .lines()
            .map(str::trim)
            .rfind(|l| l.starts_with("http"))
            .map(str::to_string)
            .ok_or_else(|| "Pull request created".to_string()),
        Err(GhErr::Missing) => Err("The GitHub CLI (gh) is not installed: https://cli.github.com".into()),
        Err(GhErr::Failed(e)) => Err(format!("gh pr create: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: &str = r#"{"number":12,"title":"Add thing","state":"OPEN","isDraft":false,"url":"https://github.com/o/r/pull/12",
      "reviewDecision":"APPROVED","headRefName":"feat/x",
      "statusCheckRollup":[
        {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
        {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"},
        {"__typename":"StatusContext","state":"SUCCESS"}]}"#;

    fn info(state: PrState, checks: Checks, review: Review) -> PrInfo {
        PrInfo { number: 3, title: "t".into(), state, url: "u".into(), review, checks, head: "h".into() }
    }

    #[test]
    fn a_pr_view_is_parsed() {
        let p = parse_pr_view(VIEW).unwrap();
        assert_eq!((p.number, p.title.as_str(), p.state, p.review), (12, "Add thing", PrState::Open, Review::Approved));
        assert_eq!(p.checks, Checks { pass: 3, fail: 0, pending: 0 });
        assert_eq!((p.url.as_str(), p.head.as_str()), ("https://github.com/o/r/pull/12", "feat/x"));
        assert_eq!(p.chip(), "PR #12 · passing · approved");
        assert_eq!(p.tone(), Tone::Good);
        assert!(p.tip().contains("3 passed"));
        assert!(parse_pr_view("not json").is_none());
        assert!(parse_pr_view("{}").is_none());
    }

    #[test]
    fn checks_are_counted_by_kind() {
        let v: Value = serde_json::from_str(
            r#"[{"status":"COMPLETED","conclusion":"FAILURE"},{"status":"IN_PROGRESS","conclusion":""},
                {"status":"QUEUED"},{"state":"PENDING"},{"state":"ERROR"},{"status":"COMPLETED","conclusion":"CANCELLED"}]"#,
        )
        .unwrap();
        assert_eq!(tally(&v), Checks { pass: 0, fail: 3, pending: 3 });
        assert_eq!(tally(&Value::Null), Checks::default());
    }

    #[test]
    fn the_chip_follows_state_checks_and_review() {
        let none = Checks::default();
        let (pass, fail, run) =
            (Checks { pass: 2, ..none }, Checks { fail: 1, pass: 1, pending: 0 }, Checks { pending: 1, ..none });
        let c = info;
        assert_eq!(c(PrState::Open, none, Review::None).chip(), "PR #3");
        assert_eq!(c(PrState::Draft, run, Review::Required).chip(), "PR #3 · draft · running");
        assert_eq!(c(PrState::Open, fail, Review::ChangesRequested).chip(), "PR #3 · failing · changes requested");
        assert_eq!(c(PrState::Merged, pass, Review::Approved).chip(), "PR #3 · merged");
        assert_eq!(c(PrState::Closed, none, Review::None).chip(), "PR #3 · closed");
        assert_eq!(c(PrState::Open, fail, Review::None).tone(), Tone::Bad);
        assert_eq!(c(PrState::Open, pass, Review::ChangesRequested).tone(), Tone::Bad);
        assert_eq!(c(PrState::Open, run, Review::None).tone(), Tone::Neutral);
        assert_eq!(c(PrState::Open, none, Review::Approved).tone(), Tone::Good);
        assert_eq!(c(PrState::Draft, pass, Review::None).tone(), Tone::Neutral);
        assert_eq!(c(PrState::Merged, none, Review::None).tone(), Tone::Merged);
        assert_eq!(c(PrState::Open, none, Review::None).tip(), "t · no checks · click to open on GitHub");
    }

    const COMMENTS: &str = r#"[
      {"id":1,"path":"src/a.rs","line":10,"user":{"login":"bo"},"body":"use `x_y` here","html_url":"https://github.com/o/r/pull/12#discussion_r1","created_at":"2026-11-10T12:00:00Z"},
      {"id":2,"path":"src/a.rs","line":10,"user":{"login":"ana"},"body":"done","html_url":"https://github.com/o/r/pull/12#discussion_r2","created_at":"2026-11-11T08:00:00Z","in_reply_to_id":1},
      {"id":3,"path":"src/a.rs","line":null,"user":{"login":"bo"},"body":"outdated","created_at":"2026-11-09T00:00:00Z"},
      {"id":4,"path":"src/b.rs","line":3,"user":{"login":"bo"},"body":"b","created_at":"2026-11-09T00:00:00Z"},
      {"path":"src/c.rs","line":1}]"#;

    #[test]
    fn review_comments_become_markers_and_a_popover() {
        let all = parse_comments(COMMENTS);
        assert_eq!(all.len(), 4, "the entry without an id is skipped");
        assert_eq!(all[0].date, "2026-11-10");
        assert!(!all[0].is_reply && all[1].is_reply);
        assert_eq!(all[2].line, None);
        assert_eq!(marker_lines(&all, "src/a.rs"), [9], "two comments on a line make one marker; outdated ones none");
        assert_eq!(marker_lines(&all, "src/b.rs"), [2]);
        assert!(marker_lines(&all, "src/none.rs").is_empty());
        let on = comments_on(&all, "src/a.rs", 9);
        assert_eq!(on.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 2]);
        assert!(comments_on(&all, "src/a.rs", 8).is_empty());
        let md = comments_markdown(&on);
        assert!(md.starts_with("**bo** · 2026-11-10"), "{md}");
        assert!(md.contains("use \\`x\\_y\\` here") && md.contains("**ana** · 2026-11-11 · reply"), "{md}");
        assert!(md.contains("[Open on GitHub](https://github.com/o/r/pull/12#discussion_r1)"));
        assert_eq!(md.matches("---").count(), 1);
        assert!(parse_comments("{}").is_empty() && parse_comments("").is_empty());
    }

    #[test]
    fn many_comments_are_cut() {
        let list: Vec<ReviewComment> = (0..9)
            .map(|i| ReviewComment {
                id: i,
                path: "f".into(),
                line: Some(1),
                author: "a".into(),
                body: "b".into(),
                url: String::new(),
                date: "2026-01-01".into(),
                is_reply: false,
            })
            .collect();
        let refs: Vec<&ReviewComment> = list.iter().collect();
        assert!(comments_markdown(&refs).ends_with("… and 3 more comments"));
    }

    #[test]
    fn create_arguments() {
        assert_eq!(create_args("T", "B", false), ["pr", "create", "--title", "T", "--body", "B"]);
        assert_eq!(create_args("T", "", true).last().map(String::as_str), Some("--draft"));
    }

    #[test]
    fn no_github_remote_means_no_question_for_gh() {
        let d = std::env::temp_dir().join(format!("lumen-ghpr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = |a: &[&str]| {
            Command::new("git").arg("-C").arg(&d).args(a).output().unwrap();
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "Ana"]);
        std::fs::write(d.join("f"), "x").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "one"]);
        let s = snapshot(&d);
        assert_eq!(s.branch, "main");
        assert!(s.info.is_none() && s.comments.is_empty() && !s.gh_missing, "{s:?}");
        // Not a repository at all: an empty snapshot too.
        let plain = std::env::temp_dir().join(format!("lumen-ghpr-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        let s = snapshot(&plain);
        assert!(s.info.is_none() && s.branch.is_empty());
        std::fs::remove_dir_all(&d).ok();
        std::fs::remove_dir_all(&plain).ok();
    }
}
