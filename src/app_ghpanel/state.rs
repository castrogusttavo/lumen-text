use super::*;
use ghub::{DiffFile, IssueDetail, IssueItem, LogLine, Notif, PrDetail, PrItem, RepoInfo, RunDetail, RunItem};

/// A list that was not asked for in this long is asked for again when the page shows it.
pub const LIST_TTL: Duration = Duration::from_secs(60);
/// A run that is still going is read again this often while it is open.
pub const RUN_POLL: Duration = Duration::from_secs(6);

// ───────────── what the page is looking at ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Section {
    Prs,
    Issues,
    Actions,
    Inbox,
}

impl Section {
    pub const ALL: [Section; 4] = [Section::Prs, Section::Issues, Section::Actions, Section::Inbox];

    pub fn title(self) -> &'static str {
        match self {
            Section::Prs => "PRs",
            Section::Issues => "Issues",
            Section::Actions => "Actions",
            Section::Inbox => "Inbox",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Section::Prs => "pull requests",
            Section::Issues => "issues",
            Section::Actions => "workflow runs",
            Section::Inbox => "notifications",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Section::Prs => "ui/pull_request",
            Section::Issues => "ui/info",
            Section::Actions => "ui/bolt_filled",
            Section::Inbox => "ui/bell",
        }
    }
}

/// What the repository check found: the page shows a message for everything but `Ready`.
#[derive(Clone, Debug, PartialEq)]
pub enum Env {
    Checking,
    Ready(RepoInfo),
    /// `gh` is not installed.
    NoGh,
    /// `gh` is installed but not logged in to github.com.
    NoAuth,
    /// The folder has no GitHub remote (or is not a repository).
    NoRepo,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct List<T> {
    pub items: Option<Vec<T>>,
    pub loading: bool,
    pub error: Option<String>,
    pub at: Option<Instant>,
}

impl<T> List<T> {
    pub fn new() -> Self {
        List { items: None, loading: false, error: None, at: None }
    }

    pub fn fresh(&self) -> bool {
        self.at.is_some_and(|t| t.elapsed() < LIST_TTL)
    }

    pub fn len(&self) -> usize {
        self.items.as_ref().map_or(0, Vec::len)
    }
}

#[derive(Clone, Debug, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrTab {
    Overview,
    Files,
    Commits,
    Checks,
}

pub struct PrView {
    pub number: u64,
    pub detail: Load<PrDetail>,
    pub tab: PrTab,
    pub diff: Load<Vec<DiffFile>>,
    /// The file of the Files tab whose diff is open.
    pub file: Option<usize>,
}

pub struct IssueView {
    pub number: u64,
    pub detail: Load<IssueDetail>,
}

pub struct RunView {
    pub id: u64,
    pub detail: Load<RunDetail>,
    /// The job whose log is shown (index into the run's jobs).
    pub job: Option<usize>,
    pub log: Load<Vec<LogLine>>,
    pub log_failed_only: bool,
    pub log_filter: TextField,
    pub log_filter_focus: bool,
    /// Jobs whose steps are folded.
    pub folded: HashSet<usize>,
    pub polled: Instant,
}

pub enum Detail {
    None,
    Pr(PrView),
    Issue(IssueView),
    Run(RunView),
}

// ───────────── what a click means ─────────────

/// A change the page can make on GitHub (every one runs `gh` on a worker).
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// "merge", "squash" or "rebase".
    Merge(&'static str),
    Approve,
    RequestChanges,
    CommentPr,
    ClosePr,
    ReopenPr,
    Ready,
    Checkout,
    CommentIssue,
    CloseIssue,
    CloseIssueNotPlanned,
    ReopenIssue,
    Rerun,
    RerunFailed,
    CancelRun,
    MarkRead(String),
    MarkAllRead,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GhHit {
    Section(Section),
    Refresh,
    RepoMenu,
    FilterMenu,
    MergeMenu,
    /// "+": start a new pull request / issue on GitHub.
    New,
    Search,
    SearchClear,
    Group(usize),
    Row(usize),
    Back,
    OpenWeb,
    CopyLink,
    Tab(PrTab),
    File(usize),
    Commit(usize),
    Check(usize),
    Job(usize),
    JobFold(usize),
    Ask(Act),
    Yes,
    No,
    Composer,
    LogBack,
    LogFailed,
    LogFilter,
    LogFilterClear,
    LogCopy,
    MenuItem(usize),
    /// The padding of an open dropdown (a click only closes it).
    MenuPad,
    /// A web address of the page (repository, profile...), opened in the browser.
    Link(String),
    Retry,
    Noop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKind {
    Repo,
    Filter,
    Merge,
}

/// The question a mutation asks first.
#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    pub text: String,
    pub yes: String,
    pub act: Act,
    pub danger: bool,
}

/// One entry of a dropdown.
#[derive(Clone, Debug, PartialEq)]
pub struct MItem {
    pub label: String,
    pub icon: &'static str,
    pub hit: GhHit,
    pub checked: Option<bool>,
}

// ───────────── worker messages ─────────────

/// A message is moved once, from the worker to the page: boxing the big payloads would only add noise.
#[allow(clippy::large_enum_variant)]
pub enum Msg {
    Env(Env),
    Prs(PrFilter, Result<Vec<PrItem>, String>),
    Issues(IssueFilter, Result<Vec<IssueItem>, String>),
    Runs(Result<Vec<RunItem>, String>),
    Inbox(Result<Vec<Notif>, String>),
    PrDetail(u64, Result<PrDetail, String>),
    PrDiff(u64, Result<Vec<DiffFile>, String>),
    IssueDetail(u64, Result<IssueDetail, String>),
    RunDetail(u64, Result<RunDetail, String>),
    Log {
        run: u64,
        job: u64,
        failed: bool,
        lines: Result<Vec<LogLine>, String>,
    },
    /// A mutation finished: what it was, the CLI's answer and what to refresh.
    Done {
        label: String,
        result: Result<String, String>,
        act: Act,
    },
}

// ───────────── the panel ─────────────

pub struct GhPanel {
    pub open: bool,
    pub focused: bool,
    pub root: PathBuf,
    pub env: Env,
    pub section: Section,
    pub pr_filter: PrFilter,
    pub issue_filter: IssueFilter,
    pub run_filter: RunFilter,
    pub prs: HashMap<PrFilter, List<PrItem>>,
    pub issues: HashMap<IssueFilter, List<IssueItem>>,
    pub runs: List<RunItem>,
    pub inbox: List<Notif>,
    /// Details seen before: shown at once, refreshed behind.
    pub pr_cache: HashMap<u64, PrDetail>,
    pub issue_cache: HashMap<u64, IssueDetail>,
    pub run_cache: HashMap<u64, RunDetail>,
    pub detail: Detail,
    pub query: TextField,
    pub query_focus: bool,
    pub collapsed: HashSet<String>,
    /// Group keys of the list as last drawn (`GhHit::Group(i)` is an index into it).
    pub group_keys: Vec<String>,
    /// List, detail and log scroll (pixels; the log's is in rows).
    pub scroll: [f64; 3],
    pub hover: Option<GhHit>,
    pub hits: Vec<(Rect, GhHit)>,
    pub menu: Option<(MenuKind, Rect)>,
    pub confirm: Option<Confirm>,
    pub composer: TextArea,
    pub composer_box: Rect,
    pub composer_focus: bool,
    /// What is running ("Merging…"); one change at a time.
    pub busy: Option<String>,
    /// The answer of the last change, shown under the header: (text, ok, when).
    pub note: Option<(String, bool, Instant)>,
    pub wrap_cache: HashMap<(u64, i32), std::sync::Arc<Vec<super::md::MdLine>>>,
    /// The visible lines of the open log: (log length, filter, failed-only flag) → indices.
    pub log_view: (usize, String, Vec<(usize, bool)>),
    pub listed: Instant,
    /// The row the keyboard is on (an index into the section's source list).
    pub cursor: Option<usize>,
    /// The environment check is running.
    pub env_sent: bool,
    /// Height of the list as last drawn (for keeping the cursor in view).
    pub list_h: i32,
    pub tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    pub wake: Option<crate::lsp::Wake>,
}

impl GhPanel {
    pub fn new(root: &Path) -> Self {
        let (tx, rx) = channel();
        GhPanel {
            open: false,
            focused: false,
            root: root.to_path_buf(),
            env: Env::Checking,
            section: Section::Prs,
            pr_filter: PrFilter::default(),
            issue_filter: IssueFilter::default(),
            run_filter: RunFilter::default(),
            prs: HashMap::new(),
            issues: HashMap::new(),
            runs: List::new(),
            inbox: List::new(),
            pr_cache: HashMap::new(),
            issue_cache: HashMap::new(),
            run_cache: HashMap::new(),
            detail: Detail::None,
            query: TextField::default(),
            query_focus: false,
            collapsed: HashSet::new(),
            group_keys: Vec::new(),
            scroll: [0.0; 3],
            hover: None,
            hits: Vec::new(),
            menu: None,
            confirm: None,
            composer: TextArea::default(),
            composer_box: Rect::default(),
            composer_focus: false,
            busy: None,
            note: None,
            wrap_cache: HashMap::new(),
            log_view: (usize::MAX, String::new(), Vec::new()),
            listed: Instant::now(),
            cursor: None,
            env_sent: false,
            list_h: 400,
            tx,
            rx,
            wake: None,
        }
    }

    pub fn hit_at(&self, x: f64, y: f64) -> Option<GhHit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone())
    }

    pub fn say(&mut self, text: impl Into<String>, ok: bool) {
        self.note = Some((text.into(), ok, Instant::now()));
    }

    /// Run `f` on a thread and deliver its message to the page (waking the event loop).
    pub fn spawn(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let wake = self.wake.clone().filter(|_| !cfg!(test));
        std::thread::spawn(move || {
            let m = f();
            let _ = tx.send(m);
            if let Some(w) = wake {
                w();
            }
        });
    }

    pub fn ready(&self) -> Option<&RepoInfo> {
        match &self.env {
            Env::Ready(i) => Some(i),
            _ => None,
        }
    }

    pub fn web_url(&self) -> Option<String> {
        self.ready().map(|i| i.url.clone()).filter(|u| !u.is_empty())
    }

    // ───────────── asking ─────────────

    /// Find out whether `gh` can be used here (off the UI thread).
    pub fn check_env(&mut self) {
        self.env = Env::Checking;
        self.env_sent = true;
        let root = self.root.clone();
        self.spawn(move || Msg::Env(detect(&root)));
    }

    pub fn pr_list(&self) -> Option<&List<PrItem>> {
        self.prs.get(&self.pr_filter)
    }

    pub fn issue_list(&self) -> Option<&List<IssueItem>> {
        self.issues.get(&self.issue_filter)
    }

    /// Ask for the list of the current section (and filter) unless it is fresh; `force` asks anyway.
    pub fn load_section(&mut self, force: bool) {
        if self.ready().is_none() {
            return;
        }
        let root = self.root.clone();
        match self.section {
            Section::Prs => {
                let f = self.pr_filter;
                let l = self.prs.entry(f).or_default();
                if l.loading || (!force && l.fresh()) {
                    return;
                }
                l.loading = true;
                self.spawn(move || Msg::Prs(f, ghub::list_prs(&root, f)));
            }
            Section::Issues => {
                let f = self.issue_filter;
                let l = self.issues.entry(f).or_default();
                if l.loading || (!force && l.fresh()) {
                    return;
                }
                l.loading = true;
                self.spawn(move || Msg::Issues(f, ghub::list_issues(&root, f)));
            }
            Section::Actions => {
                if self.runs.loading || (!force && self.runs.fresh()) {
                    return;
                }
                self.runs.loading = true;
                self.spawn(move || Msg::Runs(ghub::list_runs(&root)));
            }
            Section::Inbox => {
                if self.inbox.loading || (!force && self.inbox.fresh()) {
                    return;
                }
                self.inbox.loading = true;
                self.spawn(move || Msg::Inbox(ghub::notifications(&root)));
            }
        }
        self.listed = Instant::now();
    }

    pub fn loading_now(&self) -> bool {
        match self.section {
            Section::Prs => self.pr_list().is_some_and(|l| l.loading),
            Section::Issues => self.issue_list().is_some_and(|l| l.loading),
            Section::Actions => self.runs.loading,
            Section::Inbox => self.inbox.loading,
        }
    }

    pub fn open_pr(&mut self, number: u64) {
        let cached = self.pr_cache.get(&number).cloned();
        self.detail = Detail::Pr(PrView {
            number,
            detail: cached.map_or(Load::Loading, Load::Ready),
            tab: PrTab::Overview,
            diff: Load::Idle,
            file: None,
        });
        self.reset_detail_view();
        let root = self.root.clone();
        self.spawn(move || Msg::PrDetail(number, ghub::pr_detail(&root, number)));
    }

    /// The Files tab needs the diff of the whole pull request: asked once.
    pub fn load_pr_diff(&mut self) {
        let Detail::Pr(v) = &mut self.detail else { return };
        if !matches!(v.diff, Load::Idle | Load::Failed(_)) {
            return;
        }
        v.diff = Load::Loading;
        let (root, number) = (self.root.clone(), v.number);
        self.spawn(move || Msg::PrDiff(number, ghub::pr_diff(&root, number)));
    }

    pub fn open_issue(&mut self, number: u64) {
        let cached = self.issue_cache.get(&number).cloned();
        self.detail = Detail::Issue(IssueView { number, detail: cached.map_or(Load::Loading, Load::Ready) });
        self.reset_detail_view();
        let root = self.root.clone();
        self.spawn(move || Msg::IssueDetail(number, ghub::issue_detail(&root, number)));
    }

    pub fn open_run(&mut self, id: u64) {
        let cached = self.run_cache.get(&id).cloned();
        self.detail = Detail::Run(RunView {
            id,
            detail: cached.map_or(Load::Loading, Load::Ready),
            job: None,
            log: Load::Idle,
            log_failed_only: false,
            log_filter: TextField::default(),
            log_filter_focus: false,
            folded: HashSet::new(),
            polled: Instant::now(),
        });
        self.reset_detail_view();
        self.fetch_run(id);
    }

    pub fn fetch_run(&self, id: u64) {
        let root = self.root.clone();
        self.spawn(move || Msg::RunDetail(id, ghub::run_detail(&root, id)));
    }

    /// Load the log of job `idx` of the open run.
    pub fn open_log(&mut self, idx: usize) {
        let Detail::Run(v) = &mut self.detail else { return };
        let Some(job) = v.detail.ready().and_then(|d| d.jobs.get(idx)).map(|j| j.id) else { return };
        v.job = Some(idx);
        v.log = Load::Loading;
        v.log_filter = TextField::default();
        v.log_filter_focus = false;
        self.scroll[2] = 0.0;
        let (root, run, failed) = (self.root.clone(), v.id, v.log_failed_only);
        self.spawn(move || Msg::Log { run, job, failed, lines: ghub::job_log(&root, run, job, failed) });
    }

    pub fn reset_detail_view(&mut self) {
        self.scroll[1] = 0.0;
        self.scroll[2] = 0.0;
        self.confirm = None;
        self.menu = None;
        self.composer = TextArea::default();
        self.composer_focus = false;
    }

    /// Re-read the open detail (after a change, or on Refresh).
    pub fn reload_detail(&mut self) {
        let root = self.root.clone();
        match &self.detail {
            Detail::Pr(v) => {
                let n = v.number;
                let want_diff = v.tab == PrTab::Files;
                self.spawn(move || Msg::PrDetail(n, ghub::pr_detail(&root, n)));
                if want_diff && let Detail::Pr(v) = &mut self.detail {
                    v.diff = Load::Idle;
                    self.load_pr_diff();
                }
            }
            Detail::Issue(v) => {
                let n = v.number;
                self.spawn(move || Msg::IssueDetail(n, ghub::issue_detail(&root, n)));
            }
            Detail::Run(v) => {
                let id = v.id;
                self.fetch_run(id);
            }
            Detail::None => {}
        }
    }

    /// Start a change: one at a time. The result arrives as `Msg::Done`.
    pub fn run_act(&mut self, act: Act, label: &str) {
        if self.busy.is_some() {
            return;
        }
        let Some(args) = self.act_args(&act) else { return };
        self.busy = Some(label.to_string());
        let root = self.root.clone();
        let label = label.to_string();
        self.spawn(move || {
            let result = ghub::act(&root, &args);
            Msg::Done { label, result, act }
        });
    }

    /// The `gh` arguments of a change, from what the page has open.
    pub fn act_args(&self, act: &Act) -> Option<Vec<String>> {
        use ghub::cmd;
        let body = self.composer.text().trim().to_string();
        let pr = match &self.detail {
            Detail::Pr(v) => Some(v.number),
            _ => None,
        };
        let issue = match &self.detail {
            Detail::Issue(v) => Some(v.number),
            _ => None,
        };
        let run = match &self.detail {
            Detail::Run(v) => Some(v.id),
            _ => None,
        };
        Some(match act {
            Act::Merge(how) => cmd::merge(pr?, how),
            Act::Approve => cmd::review(pr?, "approve", &body),
            Act::RequestChanges => cmd::review(pr?, "request-changes", &body),
            Act::CommentPr => cmd::comment("pr", pr?, &body),
            Act::ClosePr => cmd::state("pr", "close", pr?),
            Act::ReopenPr => cmd::state("pr", "reopen", pr?),
            Act::Ready => cmd::ready(pr?),
            Act::Checkout => cmd::checkout(pr?),
            Act::CommentIssue => cmd::comment("issue", issue?, &body),
            Act::CloseIssue => cmd::state("issue", "close", issue?),
            Act::CloseIssueNotPlanned => {
                let mut a = cmd::state("issue", "close", issue?);
                a.extend(["--reason".into(), "not planned".into()]);
                a
            }
            Act::ReopenIssue => cmd::state("issue", "reopen", issue?),
            Act::Rerun => cmd::rerun(run?, false),
            Act::RerunFailed => cmd::rerun(run?, true),
            Act::CancelRun => cmd::cancel(run?),
            Act::MarkRead(id) => cmd::mark_read(id),
            Act::MarkAllRead => {
                ["api", "-X", "PUT", "notifications", "-F", "read=true"].iter().map(|s| s.to_string()).collect()
            }
        })
    }

    // ───────────── listening ─────────────

    /// Apply what the workers sent; true when something changed. `Done` messages are returned to
    /// the caller (they may touch the repository: a checkout moves HEAD).
    pub fn pump(&mut self) -> (bool, Vec<(String, Result<String, String>, Act)>) {
        let mut changed = false;
        let mut done = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            changed = true;
            match m {
                Msg::Env(e) => {
                    self.env_sent = false;
                    let ready = matches!(e, Env::Ready(_));
                    self.env = e;
                    if ready {
                        self.load_section(true);
                    }
                }
                Msg::Prs(f, r) => store(self.prs.entry(f).or_default(), r),
                Msg::Issues(f, r) => store(self.issues.entry(f).or_default(), r),
                Msg::Runs(r) => store(&mut self.runs, r),
                Msg::Inbox(r) => store(&mut self.inbox, r),
                Msg::PrDetail(n, r) => match r {
                    Ok(d) => {
                        self.pr_cache.insert(n, d.clone());
                        if let Detail::Pr(v) = &mut self.detail
                            && v.number == n
                        {
                            v.detail = Load::Ready(d);
                        }
                    }
                    Err(e) => {
                        if let Detail::Pr(v) = &mut self.detail
                            && v.number == n
                            && v.detail.ready().is_none()
                        {
                            v.detail = Load::Failed(e);
                        }
                    }
                },
                Msg::PrDiff(n, r) => {
                    if let Detail::Pr(v) = &mut self.detail
                        && v.number == n
                    {
                        v.diff = match r {
                            Ok(d) => Load::Ready(d),
                            Err(e) => Load::Failed(e),
                        };
                    }
                }
                Msg::IssueDetail(n, r) => match r {
                    Ok(d) => {
                        self.issue_cache.insert(n, d.clone());
                        if let Detail::Issue(v) = &mut self.detail
                            && v.number == n
                        {
                            v.detail = Load::Ready(d);
                        }
                    }
                    Err(e) => {
                        if let Detail::Issue(v) = &mut self.detail
                            && v.number == n
                            && v.detail.ready().is_none()
                        {
                            v.detail = Load::Failed(e);
                        }
                    }
                },
                Msg::RunDetail(id, r) => match r {
                    Ok(d) => {
                        self.run_cache.insert(id, d.clone());
                        if let Detail::Run(v) = &mut self.detail
                            && v.id == id
                        {
                            v.detail = Load::Ready(d);
                        }
                    }
                    Err(e) => {
                        if let Detail::Run(v) = &mut self.detail
                            && v.id == id
                            && v.detail.ready().is_none()
                        {
                            v.detail = Load::Failed(e);
                        }
                    }
                },
                Msg::Log { run, job, failed, lines } => {
                    if let Detail::Run(v) = &mut self.detail
                        && v.id == run
                        && v.log_failed_only == failed
                        && v.job.and_then(|j| v.detail.ready().and_then(|d| d.jobs.get(j))).is_some_and(|j| j.id == job)
                    {
                        v.log = match lines {
                            Ok(l) => Load::Ready(l),
                            Err(e) => Load::Failed(e),
                        };
                    }
                }
                Msg::Done { label, result, act } => {
                    if !matches!(act, Act::MarkRead(_)) {
                        self.busy = None;
                    }
                    done.push((label, result, act));
                }
            }
        }
        (changed, done)
    }

    /// The change finished: say how it went and re-read what it touched.
    pub fn after_act(&mut self, label: &str, result: &Result<String, String>, act: &Act) {
        // Marking one notification read already showed on the list: nothing more to do.
        if matches!(act, Act::MarkRead(_)) && result.is_ok() {
            return;
        }
        match result {
            Ok(out) => {
                let line = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                self.say(
                    if line.is_empty() || line.len() > 120 { format!("{label}: done") } else { line.to_string() },
                    true,
                );
                if matches!(act, Act::CommentPr | Act::CommentIssue | Act::Approve | Act::RequestChanges) {
                    self.composer = TextArea::default();
                }
                self.reload_detail();
                match act {
                    Act::MarkRead(_) | Act::MarkAllRead => {
                        self.inbox.at = None;
                        self.section_reload(Section::Inbox);
                    }
                    Act::Rerun | Act::RerunFailed | Act::CancelRun => self.section_reload(Section::Actions),
                    Act::CommentPr
                    | Act::CommentIssue
                    | Act::CloseIssue
                    | Act::CloseIssueNotPlanned
                    | Act::ReopenIssue => self.section_reload(self.section),
                    _ => self.section_reload(Section::Prs),
                }
            }
            Err(e) => self.say(format!("{label}: {e}"), false),
        }
    }

    /// Ask for a section's list again (when it is the one shown; otherwise just mark it stale).
    fn section_reload(&mut self, s: Section) {
        match s {
            Section::Prs => self.prs.values_mut().for_each(|l| l.at = None),
            Section::Issues => self.issues.values_mut().for_each(|l| l.at = None),
            Section::Actions => self.runs.at = None,
            Section::Inbox => self.inbox.at = None,
        }
        if self.section == s {
            self.load_section(true);
        }
    }

    /// (line count, index in the diff) of the diff of file `i` of the open pull request.
    pub fn diff_lines(&self, i: usize) -> Option<(usize, usize)> {
        let Detail::Pr(v) = &self.detail else { return None };
        let path = &v.detail.ready()?.files.get(i)?.path;
        let files = v.diff.ready()?;
        let k = files.iter().position(|f| &f.path == path)?;
        Some((files[k].lines.len(), k))
    }

    /// Re-read the list and the open detail.
    pub fn refresh(&mut self) {
        if self.ready().is_none() {
            self.check_env();
            return;
        }
        self.load_section(true);
        self.reload_detail();
    }

    /// The list is old enough to ask again (a list with a run going is read more often).
    pub fn needs_refresh(&self) -> bool {
        let age = |at: Option<Instant>| at.map(|t| t.elapsed());
        match self.section {
            Section::Prs => age(self.pr_list().and_then(|l| l.at)).is_some_and(|a| a > Duration::from_secs(90)),
            Section::Issues => age(self.issue_list().and_then(|l| l.at)).is_some_and(|a| a > Duration::from_secs(90)),
            Section::Actions => {
                let busy = self
                    .runs
                    .items
                    .as_ref()
                    .is_some_and(|v| v.iter().any(|r| model::run_state(&r.status, &r.conclusion).active));
                age(self.runs.at).is_some_and(|a| a > Duration::from_secs(if busy { 12 } else { 90 }))
            }
            Section::Inbox => age(self.inbox.at).is_some_and(|a| a > Duration::from_secs(60)),
        }
    }

    /// Choose entry `n` of the filter menu of the current section.
    pub fn apply_filter(&mut self, n: usize) {
        match self.section {
            Section::Prs => self.pr_filter = PrFilter::ALL[n.min(PrFilter::ALL.len() - 1)],
            Section::Issues => self.issue_filter = IssueFilter::ALL[n.min(IssueFilter::ALL.len() - 1)],
            Section::Actions => self.run_filter = RunFilter::ALL[n.min(RunFilter::ALL.len() - 1)],
            Section::Inbox => return,
        }
        self.scroll[0] = 0.0;
        self.cursor = None;
        self.load_section(false);
    }

    /// Number (pull request, issue) or id (run) of the open detail; 0 without one.
    pub fn detail_number(&self) -> u64 {
        match &self.detail {
            Detail::Pr(v) => v.number,
            Detail::Issue(v) => v.number,
            Detail::Run(v) => v.id,
            Detail::None => 0,
        }
    }

    /// The text of the open log as shown (filter applied), for the clipboard.
    pub fn log_text(&self) -> String {
        let Detail::Run(v) = &self.detail else { return String::new() };
        let Some(lines) = v.log.ready() else { return String::new() };
        let q = v.log_filter.text.trim().to_lowercase();
        super::draw_log::view_rows(lines, &q)
            .into_iter()
            .filter(|&(_, header)| !header)
            .map(|(i, _)| super::draw_log::classify(&lines[i].text).1)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A notification was opened: it is no longer unread on the list.
    pub fn mark_read_local(&mut self, i: usize) {
        if let Some(n) = self.inbox.items.as_mut().and_then(|v| v.get_mut(i)) {
            n.unread = false;
        }
    }

    /// Tell GitHub, without blocking the page.
    pub fn spawn_mark_read(&self, id: &str) {
        let (root, id) = (self.root.clone(), id.to_string());
        self.spawn(move || {
            let result = ghub::act(&root, &ghub::cmd::mark_read(&id));
            Msg::Done { label: "Mark read".into(), result, act: Act::MarkRead(id) }
        });
    }

    /// Scroll the list so the keyboard row is on screen.
    pub fn reveal_cursor(&mut self) {
        use super::draw::{GROUP_H, ROW_H};
        let Some(cur) = self.cursor else { return };
        let force = !self.query_text().is_empty();
        let mut y = 0;
        for gr in self.groups() {
            y += GROUP_H;
            if !force && self.collapsed.contains(&format!("{:?}:{}", self.section, gr.id)) {
                continue;
            }
            for &i in &gr.items {
                if i == cur {
                    let (top, bottom) = (y as f64, (y + ROW_H) as f64);
                    let view = self.list_h as f64;
                    if top < self.scroll[0] {
                        self.scroll[0] = top - GROUP_H as f64;
                    } else if bottom > self.scroll[0] + view {
                        self.scroll[0] = bottom - view + 8.0;
                    }
                    self.scroll[0] = self.scroll[0].max(0.0);
                    return;
                }
                y += ROW_H;
            }
        }
    }

    // ───────────── what the list shows ─────────────

    /// The query, trimmed and lowercased.
    pub fn query_text(&self) -> String {
        self.query.text.trim().to_lowercase()
    }

    pub fn pr_groups(&self) -> Vec<model::Group> {
        let (Some(items), q) = (self.pr_list().and_then(|l| l.items.as_ref()), self.query_text()) else {
            return Vec::new();
        };
        let shown: Vec<usize> = (0..items.len()).filter(|&i| model::pr_matches(&items[i], &q)).collect();
        model::group_prs(items, &shown, self.pr_filter.label())
    }

    pub fn issue_groups(&self) -> Vec<model::Group> {
        let (Some(items), q) = (self.issue_list().and_then(|l| l.items.as_ref()), self.query_text()) else {
            return Vec::new();
        };
        let shown: Vec<usize> = (0..items.len()).filter(|&i| model::issue_matches(&items[i], &q)).collect();
        model::group_issues(items, &shown)
    }

    pub fn run_groups(&self) -> Vec<model::Group> {
        let (Some(items), q) = (self.runs.items.as_ref(), self.query_text()) else { return Vec::new() };
        let shown: Vec<usize> = (0..items.len())
            .filter(|&i| self.run_filter.matches(&items[i]) && model::run_matches(&items[i], &q))
            .collect();
        model::group_runs(items, &shown, now_secs())
    }

    /// Notifications: unread first, then the rest; the query matches title, repository, reason.
    pub fn inbox_groups(&self) -> Vec<model::Group> {
        let (Some(items), q) = (self.inbox.items.as_ref(), self.query_text()) else { return Vec::new() };
        let ok = |n: &Notif| {
            q.is_empty()
                || n.title.to_lowercase().contains(&q)
                || n.repo.to_lowercase().contains(&q)
                || n.reason.to_lowercase().contains(&q)
        };
        let (unread, read): (Vec<usize>, Vec<usize>) =
            (0..items.len()).filter(|&i| ok(&items[i])).partition(|&i| items[i].unread);
        let mut out = Vec::new();
        for (id, title, v) in [("unread", "Unread", unread), ("read", "Earlier", read)] {
            if !v.is_empty() {
                out.push(model::Group { id: id.into(), title: title.into(), items: v });
            }
        }
        out
    }

    pub fn groups(&self) -> Vec<model::Group> {
        match self.section {
            Section::Prs => self.pr_groups(),
            Section::Issues => self.issue_groups(),
            Section::Actions => self.run_groups(),
            Section::Inbox => self.inbox_groups(),
        }
    }

    /// Source indices in the order the list shows them (folded groups left out).
    pub fn shown_order(&self) -> Vec<usize> {
        let force = !self.query_text().is_empty();
        let key = |g: &model::Group| format!("{:?}:{}", self.section, g.id);
        self.groups()
            .iter()
            .filter(|g| force || !self.collapsed.contains(&key(g)))
            .flat_map(|g| g.items.clone())
            .collect()
    }

    /// Number / id of the item open in the detail, as the list knows it.
    pub fn selected_row(&self) -> Option<usize> {
        match (&self.detail, self.section) {
            (Detail::Pr(v), Section::Prs) => self.pr_list()?.items.as_ref()?.iter().position(|p| p.number == v.number),
            (Detail::Issue(v), Section::Issues) => {
                self.issue_list()?.items.as_ref()?.iter().position(|i| i.number == v.number)
            }
            (Detail::Run(v), Section::Actions) => self.runs.items.as_ref()?.iter().position(|r| r.id == v.id),
            _ => None,
        }
    }

    /// Open the item `i` of the current list in the detail.
    pub fn open_row(&mut self, i: usize) {
        match self.section {
            Section::Prs => {
                if let Some(n) = self.pr_list().and_then(|l| l.items.as_ref()).and_then(|v| v.get(i)).map(|p| p.number)
                {
                    self.open_pr(n);
                }
            }
            Section::Issues => {
                if let Some(n) =
                    self.issue_list().and_then(|l| l.items.as_ref()).and_then(|v| v.get(i)).map(|p| p.number)
                {
                    self.open_issue(n);
                }
            }
            Section::Actions => {
                if let Some(id) = self.runs.items.as_ref().and_then(|v| v.get(i)).map(|r| r.id) {
                    self.open_run(id);
                }
            }
            Section::Inbox => {}
        }
    }

    pub fn set_section(&mut self, s: Section) {
        if self.section != s {
            self.section = s;
            self.detail = Detail::None;
            self.query = TextField::default();
            self.query_focus = false;
            self.scroll[0] = 0.0;
            self.reset_detail_view();
        }
        self.load_section(false);
    }

    /// The address of what the detail shows, for "Open on GitHub" and "Copy link".
    pub fn detail_url(&self) -> Option<String> {
        match &self.detail {
            Detail::Pr(v) => v
                .detail
                .ready()
                .map(|d| d.item.url.clone())
                .or_else(|| self.web_url().map(|u| format!("{u}/pull/{}", v.number))),
            Detail::Issue(v) => v
                .detail
                .ready()
                .map(|d| d.item.url.clone())
                .or_else(|| self.web_url().map(|u| format!("{u}/issues/{}", v.number))),
            Detail::Run(v) => v
                .detail
                .ready()
                .map(|d| d.item.url.clone())
                .or_else(|| self.web_url().map(|u| format!("{u}/actions/runs/{}", v.id))),
            Detail::None => None,
        }
    }

    /// Whether the running run needs polling now.
    pub fn run_active(&self) -> bool {
        match &self.detail {
            Detail::Run(v) => {
                v.detail.ready().is_some_and(|d| model::run_state(&d.item.status, &d.item.conclusion).active)
            }
            _ => false,
        }
    }
}

fn store<T>(l: &mut List<T>, r: Result<Vec<T>, String>) {
    l.loading = false;
    l.at = Some(Instant::now());
    match r {
        Ok(v) => {
            l.items = Some(v);
            l.error = None;
        }
        Err(e) => l.error = Some(e),
    }
}

/// Is `gh` installed, logged in, and does this folder have a GitHub repository?
pub fn detect(root: &Path) -> Env {
    use crate::app_gitpanel::{GhStatus, gh_status, github_remotes};
    match gh_status("gh") {
        GhStatus::Missing => return Env::NoGh,
        GhStatus::NoAuth => return Env::NoAuth,
        GhStatus::Ready => {}
    }
    let Some(top) = crate::git::toplevel(root) else { return Env::NoRepo };
    if github_remotes(&top, None).is_empty() {
        return Env::NoRepo;
    }
    match ghub::repo_info(root) {
        Ok(i) => Env::Ready(i),
        Err(e) => Env::Failed(e),
    }
}

impl App {
    pub fn gh_open(&self) -> bool {
        self.gh.as_ref().is_some_and(|g| g.open)
    }
}
