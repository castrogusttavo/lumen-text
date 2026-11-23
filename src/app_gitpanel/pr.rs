//! Pull requests for the branch: what the branch holds on top of its base (commits and files), the
//! pull request that exists for it (state, checks, review comments, through the `gh` CLI) and
//! creating one. Plain code with no App: the parsers are unit-tested, the runners are called from
//! worker threads (`gh` and git may take a second or two).

use super::git_run;
use crate::{textarea::TextArea, ui::Rect};
use serde_json::Value;
use std::{path::Path, process::Command, time::Instant};

/// One commit the branch has and its base has not, with the files it changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchCommit {
    pub hash: String,
    pub subject: String,
    pub files: Vec<BranchFile>,
}

/// One file the branch changes against its base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchFile {
    pub path: String,
    pub add: u32,
    pub del: u32,
    /// `M`, `A`, `D`, `R`...
    pub code: char,
}

/// The branch on top of its base: what a pull request from it would hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchInfo {
    pub branch: String,
    /// The branch it would merge into (`main`).
    pub base: String,
    pub commits: Vec<BranchCommit>,
}

impl BranchInfo {
    /// Lines added and removed by the whole branch.
    pub fn totals(&self) -> (u32, u32) {
        self.commits.iter().flat_map(|c| c.files.iter()).fold((0, 0), |(a, d), f| (a + f.add, d + f.del))
    }
}

/// The conclusion of one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Pass,
    Fail,
    Pending,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: RunState,
    /// Seconds it ran (0 when unknown or still running).
    pub secs: u64,
}

/// A comment of the conversation: a review comment on a line of a file, or a plain one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    pub bot: bool,
    pub body: String,
    /// `AddressForm.tsx:42` for a review comment.
    pub place: String,
    /// Seconds since the epoch.
    pub at: u64,
    /// Replies in the same thread, oldest first.
    pub replies: Vec<Comment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    /// `OPEN`, `MERGED`, `CLOSED`.
    pub state: String,
    pub title: String,
    pub url: String,
    pub draft: bool,
    pub checks: Vec<Check>,
    pub comments: Vec<Comment>,
}

impl PrInfo {
    pub fn passing(&self) -> usize {
        self.checks.iter().filter(|c| c.state == RunState::Pass).count()
    }

    pub fn failing(&self) -> usize {
        self.checks.iter().filter(|c| c.state == RunState::Fail).count()
    }

    pub fn pending(&self) -> usize {
        self.checks.iter().filter(|c| c.state == RunState::Pending).count()
    }
}

/// The pull request of the branch as the Checks tab shows it.
#[derive(Default)]
pub struct PrDetail {
    pub info: Option<PrInfo>,
    pub err: Option<String>,
    pub loading: bool,
    /// Asked at least once (the answer may be "no pull request").
    pub loaded: bool,
    pub at: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormField {
    Title,
    Body,
}

/// The new pull request form.
pub struct PrForm {
    pub title: TextArea,
    pub body: TextArea,
    pub base: String,
    pub draft: bool,
    pub focus: FormField,
    pub creating: bool,
    pub error: Option<String>,
    /// The boxes as drawn last (for the caret and the wheel).
    pub title_box: Rect,
    pub body_box: Rect,
}

/// What the workers report back.
pub enum PrMsg {
    Branch(BranchInfo),
    Detail(Result<Option<PrInfo>, String>),
    Created(Result<String, String>),
}

// ───────────── time ─────────────

/// Seconds since the epoch of an ISO-8601 UTC time (`2026-11-23T11:03:42Z`); 0 when it does not parse.
pub fn parse_time(s: &str) -> u64 {
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|t| t.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) =
        (n(0..4), n(5..7), n(8..10), n(11..13), n(14..16), n(17..19))
    else {
        return 0;
    };
    // Days from civil (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    (days * 86400 + h * 3600 + mi * 60 + se).max(0) as u64
}

/// Seconds since the epoch, now.
pub fn clock_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Seconds since the first call (the clock of the spinner).
pub fn spin_clock() -> f32 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// `now`, `5m`, `2h`, `3d`, `6w` for an age in seconds.
pub fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        86400..=604799 => format!("{}d", secs / 86400),
        _ => format!("{}w", secs / 604800),
    }
}

/// `38s`, `1m 12s`, `2m 41s`, `1h 5m`.
pub fn duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

// ───────────── the branch ─────────────

/// The branch a pull request from `branch` would target: the remote's default branch (`origin/HEAD`),
/// else `main` / `master` when one exists.
pub fn base_branch(top: &Path, branch: &str) -> String {
    if let Ok(o) = git_run(top, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        let b = o.trim().trim_start_matches("origin/").to_string();
        if !b.is_empty() && b != branch {
            return b;
        }
    }
    for cand in ["main", "master"] {
        if cand != branch && git_run(top, &["rev-parse", "--verify", "-q", cand]).is_ok() {
            return cand.to_string();
        }
    }
    "main".to_string()
}

/// `git log --format=%x1e%h%x1f%s --numstat` joined with the same log with `--name-status`: the
/// commits (newest first) with their files, counts and status letters.
pub fn parse_commits(numstat: &str, name_status: &str) -> Vec<BranchCommit> {
    let head = |rec: &str| -> Option<(String, String, String)> {
        let (first, rest) = rec.split_once('\n').unwrap_or((rec, ""));
        let (h, subj) = first.split_once('\x1f')?;
        Some((h.trim().to_string(), subj.trim().to_string(), rest.to_string()))
    };
    let mut codes: std::collections::HashMap<(String, String), char> = std::collections::HashMap::new();
    for rec in name_status.split('\x1e').skip(1) {
        let Some((h, _, rest)) = head(rec) else { continue };
        for l in rest.lines() {
            let mut it = l.split('\t');
            let (Some(c), Some(first)) = (it.next(), it.next()) else { continue };
            let path = it.next_back().unwrap_or(first);
            codes.insert((h.clone(), path.to_string()), c.chars().next().unwrap_or('M'));
        }
    }
    numstat
        .split('\x1e')
        .skip(1)
        .filter_map(|rec| {
            let (hash, subject, rest) = head(rec)?;
            let files = rest
                .lines()
                .filter_map(|l| {
                    let mut it = l.split('\t');
                    let (a, d, p) = (it.next()?, it.next()?, it.next()?);
                    let path = p.rsplit(" => ").next().unwrap_or(p).trim_end_matches('}').to_string();
                    Some(BranchFile {
                        code: codes.get(&(hash.clone(), path.clone())).copied().unwrap_or('M'),
                        add: a.parse().unwrap_or(0),
                        del: d.parse().unwrap_or(0),
                        path,
                    })
                })
                .collect();
            Some(BranchCommit { hash, subject, files })
        })
        .collect()
}

/// What the branch holds on top of its base.
pub fn read_branch(top: &Path) -> BranchInfo {
    let branch = git_run(top, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_default();
    let base = base_branch(top, &branch);
    // Prefer the remote's view of the base, else the local branch.
    let base_ref = if git_run(top, &["rev-parse", "--verify", "-q", &format!("origin/{base}")]).is_ok() {
        format!("origin/{base}")
    } else {
        base.clone()
    };
    let range = format!("{base_ref}..HEAD");
    let num = git_run(top, &["log", "--format=%x1e%h%x1f%s", "--numstat", &range]).unwrap_or_default();
    let ns = git_run(top, &["log", "--format=%x1e%h%x1f%s", "--name-status", &range]).unwrap_or_default();
    BranchInfo { branch, base, commits: parse_commits(&num, &ns) }
}

/// A title and a description for a pull request made of `commits` (newest first, as `git log`).
pub fn pr_defaults(info: &BranchInfo) -> (String, String) {
    match info.commits.as_slice() {
        [] => (humanize(&info.branch), String::new()),
        [one] => (one.subject.clone(), String::new()),
        many => {
            let title = many.last().map(|c| c.subject.clone()).unwrap_or_default();
            let body = many.iter().rev().map(|c| format!("- {}", c.subject)).collect::<Vec<_>>().join("\n");
            (title, body)
        }
    }
}

/// `checkout-flow-v2` as `Checkout flow v2`.
pub fn humanize(branch: &str) -> String {
    let s = branch.rsplit('/').next().unwrap_or(branch).replace(['-', '_'], " ");
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

// ───────────── gh ─────────────

fn gh(top: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(crate::ghpr::gh_exe()).current_dir(top).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { "gh not found".to_string() } else { e.to_string() }
    })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("gh failed").to_string())
}

fn check_state(status: &str, conclusion: &str) -> RunState {
    match (status, conclusion) {
        ("COMPLETED", "SUCCESS") | ("", "SUCCESS") => RunState::Pass,
        ("COMPLETED", "SKIPPED" | "NEUTRAL") | ("", "SKIPPED" | "NEUTRAL") => RunState::Skipped,
        ("COMPLETED", "") | ("", "") => RunState::Pending,
        ("COMPLETED", _) | ("", "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED") => RunState::Fail,
        _ => RunState::Pending,
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The `statusCheckRollup` of `gh pr view --json`: check runs and commit statuses.
pub fn parse_checks(v: &Value) -> Vec<Check> {
    let Some(a) = v.as_array() else { return Vec::new() };
    let all: Vec<Check> = parse_runs(a);
    // A re-run leaves the earlier run in the list: the last one of each name is the current one.
    let mut out: Vec<Check> = Vec::new();
    for c in all {
        match out.iter_mut().find(|o| o.name == c.name) {
            Some(o) => *o = c,
            None => out.push(c),
        }
    }
    out
}

fn parse_runs(a: &[Value]) -> Vec<Check> {
    a.iter()
        .map(|c| {
            let name = if c.get("name").is_some() { str_of(c, "name") } else { str_of(c, "context") };
            let (status, conclusion) = if c.get("state").is_some() {
                ("COMPLETED", str_of(c, "state"))
            } else {
                (str_of(c, "status"), str_of(c, "conclusion"))
            };
            let (s, e) = (parse_time(str_of(c, "startedAt")), parse_time(str_of(c, "completedAt")));
            Check {
                name: name.to_string(),
                state: check_state(status, conclusion),
                secs: if e > s && s > 0 { e - s } else { 0 },
            }
        })
        .collect()
}

fn is_bot(login: &str, kind: &str) -> bool {
    kind.eq_ignore_ascii_case("bot") || login.ends_with("[bot]") || login.ends_with("-bot") || login == "github-actions"
}

fn clean_login(l: &str) -> String {
    l.trim_end_matches("[bot]").to_string()
}

/// The conversation comments of `gh pr view --json comments`.
pub fn parse_issue_comments(v: &Value) -> Vec<Comment> {
    let Some(a) = v.as_array() else { return Vec::new() };
    a.iter()
        .map(|c| {
            let login = c.get("author").map(|a| str_of(a, "login")).unwrap_or("");
            Comment {
                author: clean_login(login),
                bot: is_bot(login, ""),
                body: str_of(c, "body").trim().to_string(),
                place: String::new(),
                at: parse_time(str_of(c, "createdAt")),
                replies: Vec::new(),
            }
        })
        .collect()
}

/// The review comments (on lines of files) from the REST API: threads, replies under their parent.
pub fn parse_review_comments(v: &Value) -> Vec<Comment> {
    let Some(a) = v.as_array() else { return Vec::new() };
    let mut roots: Vec<(u64, Comment)> = Vec::new();
    let mut replies: Vec<(u64, Comment)> = Vec::new();
    for c in a {
        let u = c.get("user");
        let login = u.map(|u| str_of(u, "login")).unwrap_or("");
        let kind = u.map(|u| str_of(u, "type")).unwrap_or("");
        let line = c.get("line").and_then(Value::as_u64).or_else(|| c.get("original_line").and_then(Value::as_u64));
        let file = str_of(c, "path").rsplit('/').next().unwrap_or("");
        let place = match line {
            Some(l) if !file.is_empty() => format!("{file}:{l}"),
            _ => file.to_string(),
        };
        let cm = Comment {
            author: clean_login(login),
            bot: is_bot(login, kind),
            body: str_of(c, "body").trim().to_string(),
            place,
            at: parse_time(str_of(c, "created_at")),
            replies: Vec::new(),
        };
        match c.get("in_reply_to_id").and_then(Value::as_u64) {
            Some(parent) => replies.push((parent, cm)),
            None => roots.push((c.get("id").and_then(Value::as_u64).unwrap_or(0), cm)),
        }
    }
    for (parent, r) in replies {
        if let Some((_, root)) = roots.iter_mut().find(|(id, _)| *id == parent) {
            root.replies.push(r);
        }
    }
    roots.into_iter().map(|(_, c)| c).collect()
}

/// Build the pull request from `gh pr view --json` (and the review comments of the REST API).
pub fn pr_from_json(v: &Value, review: Option<&Value>) -> PrInfo {
    let mut comments = v.get("comments").map(parse_issue_comments).unwrap_or_default();
    if let Some(r) = review {
        comments.extend(parse_review_comments(r));
    }
    comments.sort_by_key(|c| c.at);
    PrInfo {
        number: v.get("number").and_then(Value::as_u64).unwrap_or(0),
        state: str_of(v, "state").to_string(),
        title: str_of(v, "title").to_string(),
        url: str_of(v, "url").to_string(),
        draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        checks: v.get("statusCheckRollup").map(parse_checks).unwrap_or_default(),
        comments,
    }
}

/// The pull request of the current branch with its checks and comments; `Ok(None)` when there is none.
pub fn read_pr(top: &Path) -> Result<Option<PrInfo>, String> {
    // Debug builds can read a saved answer (`LUMEN_PR_FIXTURE=file.json`, with the review comments
    // under `reviewComments`) instead of asking GitHub: for screenshots and manual checks.
    #[cfg(debug_assertions)]
    if let Ok(path) = std::env::var("LUMEN_PR_FIXTURE")
        && let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(v) = serde_json::from_str::<Value>(&text)
    {
        return Ok(Some(pr_from_json(&v, v.get("reviewComments"))));
    }
    let fields = "number,state,title,url,isDraft,statusCheckRollup,comments";
    let out = match gh(top, &["pr", "view", "--json", fields]) {
        Ok(o) => o,
        Err(e) if e.contains("no pull requests found") || e.contains("no open pull requests") => return Ok(None),
        Err(e) => return Err(e),
    };
    let v: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let number = v.get("number").and_then(Value::as_u64).unwrap_or(0);
    // Review comments (inline, with threads): a second call; a failure only leaves them out.
    let review = gh(top, &["api", &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments")])
        .ok()
        .and_then(|o| serde_json::from_str::<Value>(&o).ok());
    Ok(Some(pr_from_json(&v, review.as_ref())))
}

/// Create the pull request (publishing the branch first when it has no upstream); its URL.
pub fn create_pr(top: &Path, title: &str, body: &str, base: &str, draft: bool) -> Result<String, String> {
    if git_run(top, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_err() {
        git_run(top, &["push", "-u", "origin", "HEAD"])?;
    }
    let mut args = vec!["pr", "create", "--title", title, "--body", body, "--base", base];
    if draft {
        args.push("--draft");
    }
    let out = gh(top, &args)?;
    Ok(out.lines().rev().map(str::trim).find(|l| l.starts_with("http")).unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn times_and_durations_read_like_a_review_tool() {
        assert_eq!(parse_time("1970-01-02T00:00:10Z"), 86410);
        assert_eq!(parse_time("2026-11-23T11:03:42Z") - parse_time("2026-11-23T11:00:00Z"), 222);
        assert_eq!(parse_time("garbage"), 0);
        assert_eq!(
            (ago(5), ago(125), ago(7300), ago(3 * 86400)),
            ("now".into(), "2m".into(), "2h".into(), "3d".into())
        );
        assert_eq!((duration(38), duration(72), duration(161)), ("38s".into(), "1m 12s".into(), "2m 41s".into()));
    }

    #[test]
    fn checks_are_read_from_runs_and_statuses() {
        let v = json!([
            {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS",
             "startedAt":"2026-11-23T11:00:00Z","completedAt":"2026-11-23T11:00:38Z"},
            {"__typename":"CheckRun","name":"e2e","status":"IN_PROGRESS","conclusion":""},
            {"__typename":"CheckRun","name":"unit","status":"COMPLETED","conclusion":"FAILURE"},
            {"__typename":"StatusContext","context":"ci/legacy","state":"SUCCESS"}
        ]);
        let c = parse_checks(&v);
        assert_eq!(c.len(), 4);
        assert_eq!((c[0].name.as_str(), c[0].state, c[0].secs), ("lint", RunState::Pass, 38));
        assert_eq!(c[1].state, RunState::Pending);
        assert_eq!(c[2].state, RunState::Fail);
        assert_eq!((c[3].name.as_str(), c[3].state), ("ci/legacy", RunState::Pass));
        let pr = PrInfo { checks: c, ..Default::default() };
        assert_eq!((pr.passing(), pr.failing(), pr.pending()), (2, 1, 1));
    }

    #[test]
    fn review_comments_group_replies_under_their_thread() {
        let v = json!([
            {"id":1,"user":{"login":"coderabbitai[bot]","type":"Bot"},"body":"**Potential issue**","path":"src/AddressForm.tsx","line":42,"created_at":"2026-11-23T11:00:00Z"},
            {"id":2,"in_reply_to_id":1,"user":{"login":"alex-dev","type":"User"},"body":"Fixing.","path":"src/AddressForm.tsx","line":42,"created_at":"2026-11-23T11:05:00Z"}
        ]);
        let c = parse_review_comments(&v);
        assert_eq!(c.len(), 1);
        assert!(c[0].bot && c[0].author == "coderabbitai" && c[0].place == "AddressForm.tsx:42");
        assert_eq!(c[0].replies.len(), 1);
        assert!(!c[0].replies[0].bot);
    }

    #[test]
    fn branch_commits_join_counts_and_status_letters() {
        let num = "\x1ea41c9e2\x1fValidate postal codes\n\n14\t3\tsrc/a.ts\n22\t0\ttests/b.ts\n\x1e0b0b0b0\x1fOlder\n\n1\t1\tREADME.md\n";
        let st = "\x1ea41c9e2\x1fValidate postal codes\n\nM\tsrc/a.ts\nA\ttests/b.ts\n\x1e0b0b0b0\x1fOlder\n\nM\tREADME.md\n";
        let c = parse_commits(num, st);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].hash.as_str(), c[0].subject.as_str()), ("a41c9e2", "Validate postal codes"));
        assert_eq!(c[0].files[0], BranchFile { path: "src/a.ts".into(), add: 14, del: 3, code: 'M' });
        assert_eq!((c[0].files[1].code, c[0].files[1].add), ('A', 22));
        let info = BranchInfo { commits: c, ..Default::default() };
        assert_eq!(info.totals(), (37, 4));
    }

    #[test]
    fn defaults_follow_the_commits() {
        let one = BranchInfo {
            branch: "checkout-flow-v2".into(),
            base: "main".into(),
            commits: vec![BranchCommit { hash: "a".into(), subject: "Fix it".into(), files: vec![] }],
        };
        assert_eq!(pr_defaults(&one), ("Fix it".to_string(), String::new()));
        let none = BranchInfo { commits: vec![], ..one.clone() };
        assert_eq!(pr_defaults(&none).0, "Checkout flow v2");
        let two = BranchInfo {
            commits: vec![
                BranchCommit { hash: "b".into(), subject: "Second".into(), files: vec![] },
                BranchCommit { hash: "a".into(), subject: "First".into(), files: vec![] },
            ],
            ..one
        };
        let (t, b) = pr_defaults(&two);
        assert_eq!((t.as_str(), b.as_str()), ("First", "- First\n- Second"));
    }
}
