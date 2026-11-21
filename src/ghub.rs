//! GitHub through the `gh` CLI, the data side of the GitHub page (`app_ghpanel`): models, the
//! JSON the CLI prints turned into them, and the calls. Nothing here draws and nothing keeps a
//! token: `gh` owns the credentials, every call runs on a worker thread and never prompts.
//!
//! The shapes follow what Athas' github panel shows: pull requests (list with author, state,
//! checks, labels; detail with description, activity, files, commits, checks), issues, workflow
//! runs (jobs, steps, logs) and notifications.

use crate::ghpr::{GhErr, gh};
use serde_json::Value;
use std::path::Path;

// ───────────── models ─────────────

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Label {
    pub name: String,
    /// Hex color without `#` ("d73a4a"), as GitHub stores it.
    pub color: String,
}

/// One check of a pull request: a check run or a legacy commit status.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Check {
    pub name: String,
    pub workflow: String,
    /// Lowercase `status` ("completed", "in_progress", "queued") of a check run; for a commit
    /// status the `state` ("success", "pending"...) goes to `conclusion` and this is "completed".
    pub status: String,
    pub conclusion: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PrItem {
    pub number: u64,
    pub title: String,
    /// "OPEN", "CLOSED" or "MERGED".
    pub state: String,
    pub draft: bool,
    pub author: String,
    pub head: String,
    pub base: String,
    pub created: String,
    pub updated: String,
    pub additions: u64,
    pub deletions: u64,
    /// "APPROVED", "CHANGES_REQUESTED", "REVIEW_REQUIRED" or empty.
    pub review: String,
    pub labels: Vec<Label>,
    pub url: String,
    pub checks: Vec<Check>,
    pub mergeable: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CommentKind {
    #[default]
    Comment,
    /// A review: the text may be empty (an approval without a note).
    Review,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub when: String,
    pub kind: CommentKind,
    /// "APPROVED", "CHANGES_REQUESTED", "COMMENTED", "DISMISSED" for a review.
    pub state: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// "ADDED", "MODIFIED", "DELETED", "RENAMED"...
    pub change: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PrCommit {
    pub oid: String,
    pub headline: String,
    pub author: String,
    pub date: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PrDetail {
    pub item: PrItem,
    pub body: String,
    /// `mergeStateStatus` ("CLEAN", "BLOCKED", "BEHIND", "DIRTY", "UNSTABLE", "HAS_HOOKS").
    pub merge_state: String,
    pub merged_at: String,
    pub merged_by: String,
    pub changed_files: u64,
    pub reviewers: Vec<String>,
    pub assignees: Vec<String>,
    /// Comments and reviews, oldest first.
    pub activity: Vec<Comment>,
    pub files: Vec<PrFile>,
    pub commits: Vec<PrCommit>,
    /// Issues the pull request closes: (number, title).
    pub closing: Vec<(u64, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct IssueItem {
    pub number: u64,
    pub title: String,
    /// "OPEN" or "CLOSED".
    pub state: String,
    pub reason: String,
    pub author: String,
    pub created: String,
    pub updated: String,
    pub labels: Vec<Label>,
    pub assignees: Vec<String>,
    pub comments: usize,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct IssueDetail {
    pub item: IssueItem,
    pub body: String,
    pub milestone: String,
    pub activity: Vec<Comment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RunItem {
    pub id: u64,
    pub title: String,
    pub workflow: String,
    pub status: String,
    pub conclusion: String,
    pub event: String,
    pub branch: String,
    pub sha: String,
    pub created: String,
    pub updated: String,
    pub started: String,
    pub attempt: u64,
    pub number: u64,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Step {
    pub name: String,
    pub number: u64,
    pub status: String,
    pub conclusion: String,
    pub started: String,
    pub completed: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub started: String,
    pub completed: String,
    pub url: String,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RunDetail {
    pub item: RunItem,
    pub jobs: Vec<Job>,
}

/// One line of a job's log.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct LogLine {
    pub step: String,
    /// "HH:MM:SS" of the runner's timestamp (empty when the line had none).
    pub time: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Notif {
    pub id: String,
    pub unread: bool,
    pub reason: String,
    pub updated: String,
    pub title: String,
    /// "PullRequest", "Issue", "CheckSuite", "Release", "Discussion"...
    pub kind: String,
    /// "owner/repo".
    pub repo: String,
    /// The API address of the subject (empty for a check suite).
    pub api_url: String,
    /// Number of the pull request / issue the subject is, when it is one.
    pub number: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RepoInfo {
    pub slug: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Add,
    Del,
    Ctx,
    Hunk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DiffFile {
    pub path: String,
    pub lines: Vec<DiffLine>,
    pub binary: bool,
}

// ───────────── parsing ─────────────

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn n(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

fn login(v: &Value) -> String {
    v["login"].as_str().unwrap_or("").to_string()
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v[k].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn labels(v: &Value) -> Vec<Label> {
    arr(v, "labels").iter().map(|l| Label { name: s(l, "name"), color: s(l, "color") }).collect()
}

fn logins(v: &Value, k: &str) -> Vec<String> {
    arr(v, k).iter().map(login).filter(|l| !l.is_empty()).collect()
}

/// The checks of `statusCheckRollup` (check runs and commit statuses).
pub fn parse_checks(rollup: &Value) -> Vec<Check> {
    let Some(a) = rollup.as_array() else { return Vec::new() };
    a.iter()
        .map(|c| {
            if c["__typename"].as_str() == Some("StatusContext") || c["context"].is_string() {
                Check {
                    name: s(c, "context"),
                    workflow: String::new(),
                    status: "completed".into(),
                    conclusion: s(c, "state").to_lowercase(),
                    url: s(c, "targetUrl"),
                }
            } else {
                Check {
                    name: s(c, "name"),
                    workflow: s(c, "workflowName"),
                    status: s(c, "status").to_lowercase(),
                    conclusion: s(c, "conclusion").to_lowercase(),
                    url: s(c, "detailsUrl"),
                }
            }
        })
        .collect()
}

fn pr_item(v: &Value) -> PrItem {
    PrItem {
        number: n(v, "number"),
        title: s(v, "title"),
        state: s(v, "state").to_uppercase(),
        draft: v["isDraft"].as_bool().unwrap_or(false),
        author: login(&v["author"]),
        head: s(v, "headRefName"),
        base: s(v, "baseRefName"),
        created: s(v, "createdAt"),
        updated: s(v, "updatedAt"),
        additions: n(v, "additions"),
        deletions: n(v, "deletions"),
        review: s(v, "reviewDecision"),
        labels: labels(v),
        url: s(v, "url"),
        checks: parse_checks(&v["statusCheckRollup"]),
        mergeable: s(v, "mergeable"),
    }
}

pub fn parse_prs(json: &str) -> Vec<PrItem> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    v.as_array().map_or_else(Vec::new, |a| a.iter().map(pr_item).filter(|p| p.number > 0).collect())
}

/// Comments and reviews of a pull request / issue, oldest first. A review with neither text nor
/// a decision worth showing ("COMMENTED" without a body) is dropped.
pub fn parse_activity(v: &Value) -> Vec<Comment> {
    let mut out: Vec<Comment> = arr(v, "comments")
        .iter()
        .map(|c| Comment {
            author: login(&c["author"]),
            body: s(c, "body"),
            when: s(c, "createdAt"),
            kind: CommentKind::Comment,
            state: String::new(),
            url: s(c, "url"),
        })
        .collect();
    for r in arr(v, "reviews") {
        let c = Comment {
            author: login(&r["author"]),
            body: s(r, "body"),
            when: s(r, "submittedAt"),
            kind: CommentKind::Review,
            state: s(r, "state"),
            url: String::new(),
        };
        if c.body.trim().is_empty() && c.state == "COMMENTED" {
            continue;
        }
        out.push(c);
    }
    out.sort_by(|a, b| a.when.cmp(&b.when));
    out
}

pub fn parse_pr_detail(json: &str) -> Option<PrDetail> {
    let v: Value = serde_json::from_str(json).ok()?;
    let item = pr_item(&v);
    if item.number == 0 {
        return None;
    }
    let mut reviewers = Vec::new();
    for r in arr(&v, "reviewRequests") {
        let name = if r["login"].is_string() { s(r, "login") } else { s(r, "name") };
        if !name.is_empty() {
            reviewers.push(name);
        }
    }
    Some(PrDetail {
        body: s(&v, "body"),
        merge_state: s(&v, "mergeStateStatus"),
        merged_at: s(&v, "mergedAt"),
        merged_by: login(&v["mergedBy"]),
        changed_files: n(&v, "changedFiles"),
        reviewers,
        assignees: logins(&v, "assignees"),
        activity: parse_activity(&v),
        files: arr(&v, "files")
            .iter()
            .map(|f| PrFile {
                path: s(f, "path"),
                additions: n(f, "additions"),
                deletions: n(f, "deletions"),
                change: s(f, "changeType"),
            })
            .collect(),
        commits: arr(&v, "commits")
            .iter()
            .map(|c| PrCommit {
                oid: s(c, "oid"),
                headline: s(c, "messageHeadline"),
                author: c["authors"][0]["login"]
                    .as_str()
                    .or_else(|| c["authors"][0]["name"].as_str())
                    .unwrap_or("")
                    .to_string(),
                date: s(c, "committedDate"),
            })
            .collect(),
        closing: arr(&v, "closingIssuesReferences").iter().map(|i| (n(i, "number"), s(i, "title"))).collect(),
        item,
    })
}

fn issue_item(v: &Value) -> IssueItem {
    IssueItem {
        number: n(v, "number"),
        title: s(v, "title"),
        state: s(v, "state").to_uppercase(),
        reason: s(v, "stateReason").to_uppercase(),
        author: login(&v["author"]),
        created: s(v, "createdAt"),
        updated: s(v, "updatedAt"),
        labels: labels(v),
        assignees: logins(v, "assignees"),
        comments: v["comments"].as_array().map_or_else(|| n(v, "comments") as usize, Vec::len),
        url: s(v, "url"),
    }
}

pub fn parse_issues(json: &str) -> Vec<IssueItem> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    v.as_array().map_or_else(Vec::new, |a| a.iter().map(issue_item).filter(|i| i.number > 0).collect())
}

pub fn parse_issue_detail(json: &str) -> Option<IssueDetail> {
    let v: Value = serde_json::from_str(json).ok()?;
    let item = issue_item(&v);
    if item.number == 0 {
        return None;
    }
    Some(IssueDetail {
        body: s(&v, "body"),
        milestone: s(&v["milestone"], "title"),
        activity: parse_activity(&v),
        item,
    })
}

fn run_item(v: &Value) -> RunItem {
    let title = [s(v, "displayTitle"), s(v, "name"), s(v, "workflowName")]
        .into_iter()
        .find(|t| !t.is_empty())
        .unwrap_or_default();
    RunItem {
        id: n(v, "databaseId"),
        title,
        workflow: s(v, "workflowName"),
        status: s(v, "status").to_lowercase(),
        conclusion: s(v, "conclusion").to_lowercase(),
        event: s(v, "event"),
        branch: s(v, "headBranch"),
        sha: s(v, "headSha"),
        created: s(v, "createdAt"),
        updated: s(v, "updatedAt"),
        started: s(v, "startedAt"),
        attempt: n(v, "attempt"),
        number: n(v, "number"),
        url: s(v, "url"),
    }
}

pub fn parse_runs(json: &str) -> Vec<RunItem> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    v.as_array().map_or_else(Vec::new, |a| a.iter().map(run_item).filter(|r| r.id > 0).collect())
}

pub fn parse_run_detail(json: &str) -> Option<RunDetail> {
    let v: Value = serde_json::from_str(json).ok()?;
    let item = run_item(&v);
    if item.id == 0 {
        return None;
    }
    let jobs = arr(&v, "jobs")
        .iter()
        .map(|j| Job {
            id: n(j, "databaseId"),
            name: s(j, "name"),
            status: s(j, "status").to_lowercase(),
            conclusion: s(j, "conclusion").to_lowercase(),
            started: s(j, "startedAt"),
            completed: s(j, "completedAt"),
            url: s(j, "url"),
            steps: arr(j, "steps")
                .iter()
                .map(|st| Step {
                    name: s(st, "name"),
                    number: n(st, "number"),
                    status: s(st, "status").to_lowercase(),
                    conclusion: s(st, "conclusion").to_lowercase(),
                    started: s(st, "startedAt"),
                    completed: s(st, "completedAt"),
                })
                .collect(),
        })
        .collect();
    Some(RunDetail { item, jobs })
}

/// `gh run view --log` prints `job<TAB>step<TAB>timestamp text`; a line without tabs is kept as
/// plain text. The byte order mark GitHub puts on the first line goes.
pub fn parse_log(text: &str) -> Vec<LogLine> {
    text.lines()
        .map(|l| {
            let l = l.trim_start_matches('\u{feff}');
            let mut parts = l.splitn(3, '\t');
            let (_job, step, rest) = (parts.next(), parts.next(), parts.next());
            let (Some(step), Some(rest)) = (step, rest) else {
                return LogLine { step: String::new(), time: String::new(), text: l.to_string() };
            };
            let rest = rest.trim_start_matches('\u{feff}');
            let (time, body) = match rest.split_once(' ') {
                Some((t, b)) if t.len() >= 20 && t.as_bytes().get(10) == Some(&b'T') => {
                    (t.get(11..19).unwrap_or("").to_string(), b.to_string())
                }
                _ => (String::new(), rest.to_string()),
            };
            LogLine { step: step.to_string(), time, text: body }
        })
        .collect()
}

/// Number the API address of a pull request / issue ends with: `.../pulls/12`, `.../issues/7`.
pub fn subject_number(api_url: &str) -> Option<u64> {
    let mut it = api_url.rsplit('/');
    let last = it.next()?;
    let kind = it.next()?;
    if kind == "pulls" || kind == "issues" { last.parse().ok() } else { None }
}

pub fn parse_notifications(json: &str) -> Vec<Notif> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let Some(a) = v.as_array() else { return Vec::new() };
    a.iter()
        .map(|x| {
            let api_url = s(&x["subject"], "url");
            Notif {
                id: s(x, "id"),
                unread: x["unread"].as_bool().unwrap_or(false),
                reason: s(x, "reason"),
                updated: s(x, "updated_at"),
                title: s(&x["subject"], "title"),
                kind: s(&x["subject"], "type"),
                repo: s(&x["repository"], "full_name"),
                number: subject_number(&api_url),
                api_url,
            }
        })
        .collect()
}

pub fn parse_repo(json: &str) -> Option<RepoInfo> {
    let v: Value = serde_json::from_str(json).ok()?;
    let slug = s(&v, "nameWithOwner");
    (!slug.is_empty()).then(|| RepoInfo { slug, url: s(&v, "url") })
}

/// Split a unified diff (`gh pr diff`) into files with their hunk lines.
pub fn parse_diff(text: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut in_hunk = false;
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            // "a/old b/new": the new name is what the page shows.
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b).to_string();
            files.push(DiffFile { path, ..Default::default() });
            in_hunk = false;
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if l.starts_with("Binary files ") || l.starts_with("GIT binary patch") {
            f.binary = true;
        } else if l.starts_with("@@") {
            in_hunk = true;
            f.lines.push(DiffLine { kind: DiffKind::Hunk, text: l.to_string() });
        } else if in_hunk {
            let (kind, body) = match l.chars().next() {
                Some('+') => (DiffKind::Add, &l[1..]),
                Some('-') => (DiffKind::Del, &l[1..]),
                Some('\\') => continue,
                Some(' ') => (DiffKind::Ctx, &l[1..]),
                _ => (DiffKind::Ctx, l),
            };
            f.lines.push(DiffLine { kind, text: body.to_string() });
        }
    }
    files
}

// ───────────── filters (what the toolbar asks for) ─────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PrFilter {
    #[default]
    Open,
    Mine,
    ReviewRequests,
    Merged,
    Closed,
}

impl PrFilter {
    pub const ALL: [PrFilter; 5] =
        [PrFilter::Open, PrFilter::Mine, PrFilter::ReviewRequests, PrFilter::Merged, PrFilter::Closed];

    pub fn label(self) -> &'static str {
        match self {
            PrFilter::Open => "Open PRs",
            PrFilter::Mine => "My PRs",
            PrFilter::ReviewRequests => "Review requests",
            PrFilter::Merged => "Merged",
            PrFilter::Closed => "Closed",
        }
    }

    /// Arguments of `gh pr list` after `pr list`.
    pub fn args(self) -> Vec<&'static str> {
        match self {
            PrFilter::Open => vec!["--state", "open"],
            PrFilter::Mine => vec!["--state", "open", "--author", "@me"],
            PrFilter::ReviewRequests => vec!["--state", "open", "--search", "review-requested:@me"],
            PrFilter::Merged => vec!["--state", "merged"],
            PrFilter::Closed => vec!["--state", "closed"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum IssueFilter {
    #[default]
    Open,
    Closed,
    All,
    Assigned,
}

impl IssueFilter {
    pub const ALL: [IssueFilter; 4] = [IssueFilter::Open, IssueFilter::Closed, IssueFilter::All, IssueFilter::Assigned];

    pub fn label(self) -> &'static str {
        match self {
            IssueFilter::Open => "Open issues",
            IssueFilter::Closed => "Closed issues",
            IssueFilter::All => "All issues",
            IssueFilter::Assigned => "Assigned to me",
        }
    }

    pub fn args(self) -> Vec<&'static str> {
        match self {
            IssueFilter::Open => vec!["--state", "open"],
            IssueFilter::Closed => vec!["--state", "closed"],
            IssueFilter::All => vec!["--state", "all"],
            IssueFilter::Assigned => vec!["--state", "open", "--assignee", "@me"],
        }
    }
}

pub const PR_LIST_FIELDS: &str = "number,title,state,isDraft,author,headRefName,baseRefName,createdAt,updatedAt,additions,deletions,reviewDecision,labels,url,statusCheckRollup,mergeable";
pub const PR_DETAIL_FIELDS: &str = "number,title,body,state,isDraft,author,headRefName,baseRefName,createdAt,updatedAt,mergedAt,mergedBy,additions,deletions,changedFiles,reviewDecision,mergeStateStatus,mergeable,labels,assignees,reviewRequests,reviews,comments,commits,files,url,closingIssuesReferences,statusCheckRollup";
pub const ISSUE_LIST_FIELDS: &str =
    "number,title,state,stateReason,author,labels,updatedAt,createdAt,url,comments,assignees";
pub const ISSUE_DETAIL_FIELDS: &str =
    "number,title,body,state,stateReason,author,labels,assignees,milestone,comments,createdAt,updatedAt,url";
pub const RUN_LIST_FIELDS: &str = "databaseId,displayTitle,name,workflowName,status,conclusion,event,headBranch,headSha,createdAt,updatedAt,startedAt,attempt,number,url";
pub const RUN_DETAIL_FIELDS: &str = "databaseId,displayTitle,name,workflowName,status,conclusion,event,headBranch,headSha,createdAt,updatedAt,startedAt,attempt,number,url,jobs";

// ───────────── calls ─────────────

fn call(dir: &Path, args: &[&str]) -> Result<String, GhErr> {
    let v: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    gh(dir, &v)
}

/// A readable reason for a failed call (the CLI's last line, or "not installed").
pub fn err_text(e: GhErr) -> String {
    match e {
        GhErr::Missing => "The GitHub CLI (gh) is not installed".into(),
        GhErr::Failed(m) => m,
    }
}

fn get<T>(dir: &Path, args: &[&str], parse: impl FnOnce(&str) -> T) -> Result<T, String> {
    call(dir, args).map(|o| parse(&o)).map_err(err_text)
}

pub fn repo_info(dir: &Path) -> Result<RepoInfo, String> {
    let out = call(dir, &["repo", "view", "--json", "nameWithOwner,url"]).map_err(err_text)?;
    parse_repo(&out).ok_or_else(|| "This folder has no GitHub repository".to_string())
}

pub fn list_prs(dir: &Path, f: PrFilter) -> Result<Vec<PrItem>, String> {
    let mut a = vec!["pr", "list", "--limit", "60", "--json", PR_LIST_FIELDS];
    a.extend(f.args());
    get(dir, &a, parse_prs)
}

pub fn pr_detail(dir: &Path, number: u64) -> Result<PrDetail, String> {
    let num = number.to_string();
    let out = call(dir, &["pr", "view", &num, "--json", PR_DETAIL_FIELDS]).map_err(err_text)?;
    parse_pr_detail(&out).ok_or_else(|| format!("Could not read pull request #{number}"))
}

pub fn pr_diff(dir: &Path, number: u64) -> Result<Vec<DiffFile>, String> {
    let num = number.to_string();
    get(dir, &["pr", "diff", &num, "--color", "never"], parse_diff)
}

pub fn list_issues(dir: &Path, f: IssueFilter) -> Result<Vec<IssueItem>, String> {
    let mut a = vec!["issue", "list", "--limit", "60", "--json", ISSUE_LIST_FIELDS];
    a.extend(f.args());
    get(dir, &a, parse_issues)
}

pub fn issue_detail(dir: &Path, number: u64) -> Result<IssueDetail, String> {
    let num = number.to_string();
    let out = call(dir, &["issue", "view", &num, "--json", ISSUE_DETAIL_FIELDS]).map_err(err_text)?;
    parse_issue_detail(&out).ok_or_else(|| format!("Could not read issue #{number}"))
}

pub fn list_runs(dir: &Path) -> Result<Vec<RunItem>, String> {
    get(dir, &["run", "list", "--limit", "50", "--json", RUN_LIST_FIELDS], parse_runs)
}

pub fn run_detail(dir: &Path, id: u64) -> Result<RunDetail, String> {
    let id = id.to_string();
    let out = call(dir, &["run", "view", &id, "--json", RUN_DETAIL_FIELDS]).map_err(err_text)?;
    parse_run_detail(&out).ok_or_else(|| "Could not read the run".to_string())
}

/// Lines of a job's log: only the failed steps with `failed`.
pub fn job_log(dir: &Path, run: u64, job: u64, failed: bool) -> Result<Vec<LogLine>, String> {
    let (run, job) = (run.to_string(), job.to_string());
    let mut a = vec!["run", "view", &run, "--job", &job];
    a.push(if failed { "--log-failed" } else { "--log" });
    get(dir, &a, parse_log)
}

pub fn notifications(dir: &Path) -> Result<Vec<Notif>, String> {
    get(dir, &["api", "notifications?per_page=40"], parse_notifications)
}

/// A mutation (merge, review, comment, close, re-run...): the CLI's output is the answer.
pub fn act(dir: &Path, args: &[String]) -> Result<String, String> {
    gh(dir, args).map(|o| o.trim().to_string()).map_err(err_text)
}

/// Arguments of the mutations the page offers.
pub mod cmd {
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// `how`: "merge", "squash" or "rebase".
    pub fn merge(number: u64, how: &str) -> Vec<String> {
        v(&["pr", "merge", &number.to_string(), &format!("--{how}"), "--delete-branch=false"])
    }

    pub fn review(number: u64, kind: &str, body: &str) -> Vec<String> {
        let mut a = v(&["pr", "review", &number.to_string(), &format!("--{kind}")]);
        if !body.trim().is_empty() {
            a.extend(v(&["--body", body]));
        }
        a
    }

    pub fn comment(what: &str, number: u64, body: &str) -> Vec<String> {
        v(&[what, "comment", &number.to_string(), "--body", body])
    }

    pub fn state(what: &str, verb: &str, number: u64) -> Vec<String> {
        v(&[what, verb, &number.to_string()])
    }

    pub fn checkout(number: u64) -> Vec<String> {
        v(&["pr", "checkout", &number.to_string()])
    }

    pub fn ready(number: u64) -> Vec<String> {
        v(&["pr", "ready", &number.to_string()])
    }

    pub fn rerun(id: u64, failed: bool) -> Vec<String> {
        let mut a = v(&["run", "rerun", &id.to_string()]);
        if failed {
            a.push("--failed".into());
        }
        a
    }

    pub fn cancel(id: u64) -> Vec<String> {
        v(&["run", "cancel", &id.to_string()])
    }

    pub fn mark_read(id: &str) -> Vec<String> {
        v(&["api", "-X", "PATCH", &format!("notifications/threads/{id}")])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR_LIST: &str = r#"[{"additions":12,"author":{"login":"ana","is_bot":false},"baseRefName":"main","createdAt":"2026-11-21T18:31:15Z","deletions":3,"headRefName":"feat/x","isDraft":true,"labels":[{"name":"bug","color":"d73a4a"}],"number":7,"reviewDecision":"APPROVED","state":"OPEN","title":"Add x","updatedAt":"2026-11-21T19:00:00Z","url":"https://github.com/o/r/pull/7","mergeable":"MERGEABLE","statusCheckRollup":[{"__typename":"CheckRun","name":"Test","workflowName":"CI","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://x/1"},{"__typename":"StatusContext","context":"ci/old","state":"PENDING","targetUrl":"https://x/2"}]},{"number":0}]"#;

    #[test]
    fn pr_list_parses_authors_labels_and_checks() {
        let v = parse_prs(PR_LIST);
        assert_eq!(v.len(), 1, "a row without a number is dropped");
        let p = &v[0];
        assert_eq!((p.number, p.author.as_str(), p.state.as_str()), (7, "ana", "OPEN"));
        assert!(p.draft);
        assert_eq!(p.labels, vec![Label { name: "bug".into(), color: "d73a4a".into() }]);
        assert_eq!(p.checks.len(), 2);
        assert_eq!((p.checks[0].status.as_str(), p.checks[0].conclusion.as_str()), ("completed", "success"));
        assert_eq!((p.checks[1].name.as_str(), p.checks[1].conclusion.as_str()), ("ci/old", "pending"));
        assert_eq!(p.review, "APPROVED");
    }

    #[test]
    fn garbage_json_is_an_empty_list() {
        assert!(parse_prs("not json").is_empty());
        assert!(parse_issues("{}").is_empty());
        assert!(parse_runs("").is_empty());
        assert!(parse_pr_detail("[]").is_none());
    }

    #[test]
    fn pr_detail_merges_comments_and_reviews_by_time() {
        let j = r#"{"number":3,"title":"T","state":"OPEN","body":"hello","author":{"login":"a"},
        "mergeStateStatus":"CLEAN","changedFiles":2,
        "reviewRequests":[{"login":"bob"},{"name":"core-team"}],
        "assignees":[{"login":"carl"}],
        "comments":[{"author":{"login":"x"},"body":"late","createdAt":"2026-01-03T00:00:00Z","url":"u"}],
        "reviews":[{"author":{"login":"y"},"body":"","state":"APPROVED","submittedAt":"2026-01-01T00:00:00Z"},
                   {"author":{"login":"z"},"body":"","state":"COMMENTED","submittedAt":"2026-01-02T00:00:00Z"}],
        "files":[{"path":"a.rs","additions":4,"deletions":1,"changeType":"MODIFIED"}],
        "commits":[{"oid":"abcdef0123","messageHeadline":"fix","authors":[{"login":"a","name":"A"}],"committedDate":"2026-01-01T00:00:00Z"}],
        "closingIssuesReferences":[{"number":9,"title":"bug"}]}"#;
        let d = parse_pr_detail(j).unwrap();
        assert_eq!(d.reviewers, vec!["bob", "core-team"]);
        assert_eq!(d.activity.len(), 2, "an empty COMMENTED review is noise");
        assert_eq!(d.activity[0].kind, CommentKind::Review);
        assert_eq!(d.activity[0].state, "APPROVED");
        assert_eq!(d.activity[1].body, "late");
        assert_eq!(d.files[0].path, "a.rs");
        assert_eq!(d.commits[0].author, "a");
        assert_eq!(d.closing, vec![(9, "bug".to_string())]);
        assert_eq!(d.merge_state, "CLEAN");
    }

    #[test]
    fn issues_and_runs_parse() {
        let i = parse_issues(
            r#"[{"number":5,"title":"Bug","state":"CLOSED","stateReason":"NOT_PLANNED","author":{"login":"q"},"comments":[{},{}],"labels":[],"assignees":[{"login":"w"}]}]"#,
        );
        assert_eq!((i[0].comments, i[0].reason.as_str()), (2, "NOT_PLANNED"));
        assert_eq!(i[0].assignees, vec!["w"]);
        let r = parse_runs(
            r#"[{"databaseId":99,"displayTitle":"","name":"CI","workflowName":"CI","status":"completed","conclusion":"failure","event":"push","headBranch":"main","headSha":"abc","attempt":2,"number":12}]"#,
        );
        assert_eq!(r[0].title, "CI", "an empty display title falls back to the name");
        assert_eq!((r[0].id, r[0].attempt, r[0].conclusion.as_str()), (99, 2, "failure"));
    }

    #[test]
    fn run_detail_has_jobs_and_steps() {
        let d = parse_run_detail(
            r#"{"databaseId":1,"status":"in_progress","jobs":[{"databaseId":10,"name":"build","status":"in_progress","conclusion":"","steps":[{"name":"Set up job","number":1,"status":"completed","conclusion":"success"}]}]}"#,
        )
        .unwrap();
        assert_eq!(d.jobs[0].name, "build");
        assert_eq!(d.jobs[0].steps[0].conclusion, "success");
    }

    #[test]
    fn log_lines_split_step_time_and_text() {
        let l = parse_log(
            "build\tSet up job\t\u{feff}2026-11-21T18:34:55.9481225Z Current runner version\nbuild\tUNKNOWN STEP\t2026-11-21T18:34:56.0000000Z ##[group]Image\nplain line",
        );
        assert_eq!(l[0].step, "Set up job");
        assert_eq!(l[0].time, "18:34:55");
        assert_eq!(l[0].text, "Current runner version");
        assert_eq!(l[1].text, "##[group]Image");
        assert_eq!((l[2].step.as_str(), l[2].text.as_str()), ("", "plain line"));
    }

    #[test]
    fn notifications_find_the_subject_number() {
        let v = parse_notifications(
            r#"[{"id":"1","unread":true,"reason":"review_requested","updated_at":"2026-01-01T00:00:00Z","subject":{"title":"Fix","url":"https://api.github.com/repos/o/r/pulls/12","type":"PullRequest"},"repository":{"full_name":"o/r"}},
                {"id":"2","unread":false,"reason":"ci_activity","subject":{"title":"ci failed","url":null,"type":"CheckSuite"},"repository":{"full_name":"o/r"}}]"#,
        );
        assert_eq!(v[0].number, Some(12));
        assert_eq!(v[0].repo, "o/r");
        assert!(v[0].unread);
        assert_eq!(v[1].number, None);
        assert_eq!(subject_number("https://api.github.com/repos/o/r/issues/7"), Some(7));
        assert_eq!(subject_number("https://api.github.com/repos/o/r/releases/7"), None);
    }

    #[test]
    fn diff_splits_files_and_hunks() {
        let d = parse_diff(
            "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n\\ No newline at end of file\ndiff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n",
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].path, "src/a.rs");
        let kinds: Vec<DiffKind> = d[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![DiffKind::Hunk, DiffKind::Ctx, DiffKind::Del, DiffKind::Add]);
        assert!(d[1].binary && d[1].lines.is_empty());
    }

    #[test]
    fn repo_info_needs_a_slug() {
        assert_eq!(
            parse_repo(r#"{"nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
            Some(RepoInfo { slug: "o/r".into(), url: "https://github.com/o/r".into() })
        );
        assert_eq!(parse_repo("{}"), None);
    }

    #[test]
    fn filters_map_to_cli_arguments() {
        assert_eq!(PrFilter::Mine.args(), vec!["--state", "open", "--author", "@me"]);
        assert!(PrFilter::ReviewRequests.args().contains(&"review-requested:@me"));
        assert_eq!(IssueFilter::All.args(), vec!["--state", "all"]);
    }

    #[test]
    fn mutation_arguments() {
        assert_eq!(cmd::merge(4, "squash"), ["pr", "merge", "4", "--squash", "--delete-branch=false"]);
        assert_eq!(cmd::review(4, "approve", "  "), ["pr", "review", "4", "--approve"]);
        assert_eq!(cmd::review(4, "request-changes", "no"), ["pr", "review", "4", "--request-changes", "--body", "no"]);
        assert_eq!(cmd::rerun(8, true), ["run", "rerun", "8", "--failed"]);
        assert_eq!(cmd::comment("issue", 2, "hi"), ["issue", "comment", "2", "--body", "hi"]);
    }
}
