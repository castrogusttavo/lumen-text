//! What the detail pane shows, as a flat list of items (title, badges, buttons, comments, rows...)
//! worked out from the panel's data. Plain code: the drawing measures and paints them.

use super::*;
use ghub::{Comment, CommentKind, Label};
use model::{PrStatus, plural, time_ago};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtnStyle {
    Primary,
    Plain,
    Danger,
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Btn {
    pub label: String,
    pub icon: &'static str,
    pub hit: GhHit,
    pub style: BtnStyle,
}

fn btn(label: &str, icon: &'static str, hit: GhHit, style: BtnStyle) -> Btn {
    Btn { label: label.into(), icon, hit, style }
}

/// A row of a list inside the detail (a file, a commit, a check, a job, a step).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub indent: i32,
    pub icon: &'static str,
    pub tone: Tone,
    pub title: String,
    pub sub: String,
    pub right: String,
    pub hit: GhHit,
    /// A chevron before the icon: (what it toggles, open now).
    pub fold: Option<(GhHit, bool)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum It {
    Title(String),
    Badges(Vec<(String, Tone)>),
    Dim(String),
    Space(i32),
    Buttons(Vec<Btn>),
    Confirm(Confirm),
    Tabs(Vec<(String, GhHit, bool)>),
    /// A section title with a count (hidden when 0).
    Heading(String, usize),
    Md(String),
    Chips(Vec<Label>),
    Kv(String, String),
    Comment(Comment),
    Row(Row),
    /// The diff of file `n` of the open pull request.
    Diff(usize),
    Composer(Vec<Btn>),
    Note(String, Tone),
}

/// The icon of a run / job / step / check state.
pub fn phase_icon(p: model::Phase) -> &'static str {
    use model::Phase::*;
    match p {
        Success => "ui/check",
        Failure => "ui/x_circle_filled",
        Cancelled => "ui/slash",
        Skipped => "ui/dash",
        ActionRequired => "ui/warning",
        Neutral => "ui/circle",
        Running => "ui/load_circle",
        Queued => "ui/clock",
        Unknown => "ui/circle_help",
    }
}

fn retry(msg: &str) -> Vec<It> {
    vec![
        It::Note(msg.to_string(), Tone::Error),
        It::Space(8),
        It::Buttons(vec![btn("Try again", "ui/rotate_cw", GhHit::Retry, BtnStyle::Plain)]),
    ]
}

impl GhPanel {
    /// The items of the open detail (empty without one).
    pub fn detail_items(&self, now: i64) -> Vec<It> {
        match &self.detail {
            Detail::Pr(v) => self.pr_items(v, now),
            Detail::Issue(v) => self.issue_items(v, now),
            Detail::Run(v) => self.run_items(v, now),
            Detail::None => Vec::new(),
        }
    }

    fn style(&self, s: BtnStyle) -> BtnStyle {
        if self.busy.is_some() { BtnStyle::Off } else { s }
    }

    fn activity(&self, items: &mut Vec<It>, activity: &[Comment]) {
        items.push(It::Heading("Activity".into(), activity.len()));
        if activity.is_empty() {
            items.push(It::Dim("No comments yet".into()));
        }
        for c in activity {
            items.push(It::Comment(c.clone()));
            items.push(It::Space(10));
        }
    }

    fn pr_items(&self, v: &PrView, now: i64) -> Vec<It> {
        let d = match &v.detail {
            Load::Ready(d) => d,
            Load::Failed(e) => return retry(e),
            _ => return vec![It::Note("Loading pull request…".into(), Tone::Muted)],
        };
        let p = &d.item;
        let st = PrStatus::of(p);
        let ms = model::merge_status_of(d);
        let tally = model::tally(&p.checks);
        let mut out = vec![It::Title(format!("{} #{}", p.title, p.number))];
        let mut badges = vec![(st.label().to_string(), st.tone())];
        if let Some((l, t)) = model::review_badge(&p.review) {
            badges.push((l.to_string(), t));
        }
        if matches!(st, PrStatus::Open | PrStatus::Draft) && !ms.ready {
            badges.push((ms.text.to_string(), ms.tone));
        }
        if !p.checks.is_empty() {
            badges.push((tally.text(), tally.tone()));
        }
        out.push(It::Badges(badges));
        let when = |s: &str| time_ago(s, now);
        out.push(It::Dim(match st {
            PrStatus::Merged => format!(
                "{} merged {} into {} from {} · {}",
                if d.merged_by.is_empty() { &p.author } else { &d.merged_by },
                plural(d.commits.len(), "commit"),
                p.base,
                p.head,
                when(&d.merged_at)
            ),
            _ => format!(
                "{} wants to merge {} into {} from {} · {}",
                p.author,
                plural(d.commits.len(), "commit"),
                p.base,
                p.head,
                when(&p.created)
            ),
        }));
        out.push(It::Dim(format!(
            "+{} −{} · {} changed",
            p.additions,
            p.deletions,
            plural(d.changed_files as usize, "file")
        )));
        out.push(It::Space(8));
        let mut b = Vec::new();
        match st {
            PrStatus::Draft => {
                b.push(btn("Ready for review", "ui/check", GhHit::Ask(Act::Ready), self.style(BtnStyle::Primary)))
            }
            PrStatus::Open if ms.ready => b.push(btn(
                "Merge",
                "ui/git_merge_conflict",
                GhHit::MergeMenu,
                self.style(if ms.tone == Tone::Warning { BtnStyle::Plain } else { BtnStyle::Primary }),
            )),
            PrStatus::Open => b.push(btn(ms.text, "ui/git_merge_conflict", GhHit::Noop, BtnStyle::Off)),
            _ => {}
        }
        if matches!(st, PrStatus::Open | PrStatus::Draft) {
            b.push(btn("Close", "ui/close", GhHit::Ask(Act::ClosePr), self.style(BtnStyle::Danger)));
        } else if st == PrStatus::Closed {
            b.push(btn("Reopen", "ui/rotate_cw", GhHit::Ask(Act::ReopenPr), self.style(BtnStyle::Plain)));
        }
        if st != PrStatus::Merged {
            b.push(btn("Checkout", "ui/git_branch", GhHit::Ask(Act::Checkout), self.style(BtnStyle::Plain)));
        }
        if !b.is_empty() {
            out.push(It::Buttons(b));
        }
        if let Some(c) = &self.confirm {
            out.push(It::Space(6));
            out.push(It::Confirm(c.clone()));
        }
        out.push(It::Space(10));
        out.push(It::Tabs(vec![
            ("Overview".into(), GhHit::Tab(PrTab::Overview), v.tab == PrTab::Overview),
            (format!("Files {}", d.files.len()), GhHit::Tab(PrTab::Files), v.tab == PrTab::Files),
            (format!("Commits {}", d.commits.len()), GhHit::Tab(PrTab::Commits), v.tab == PrTab::Commits),
            (format!("Checks {}", p.checks.len()), GhHit::Tab(PrTab::Checks), v.tab == PrTab::Checks),
        ]));
        out.push(It::Space(12));
        match v.tab {
            PrTab::Overview => {
                if !p.labels.is_empty() {
                    out.push(It::Chips(p.labels.clone()));
                    out.push(It::Space(8));
                }
                if !d.reviewers.is_empty() {
                    out.push(It::Kv("Reviewers".into(), d.reviewers.join(", ")));
                }
                if !d.assignees.is_empty() {
                    out.push(It::Kv("Assignees".into(), d.assignees.join(", ")));
                }
                if !d.closing.is_empty() {
                    let l: Vec<String> = d.closing.iter().map(|(n, t)| format!("#{n} {t}")).collect();
                    out.push(It::Kv("Closes".into(), l.join(", ")));
                }
                out.push(It::Space(8));
                out.push(It::Heading("Description".into(), 0));
                if d.body.trim().is_empty() {
                    out.push(It::Dim("No description provided".into()));
                } else {
                    out.push(It::Md(d.body.clone()));
                }
                out.push(It::Space(14));
                self.activity(&mut out, &d.activity);
                let open = matches!(st, PrStatus::Open | PrStatus::Draft);
                let mut cb = vec![btn("Comment", "ui/chat", GhHit::Ask(Act::CommentPr), self.style(BtnStyle::Primary))];
                if open {
                    cb.push(btn("Approve", "ui/check", GhHit::Ask(Act::Approve), self.style(BtnStyle::Plain)));
                    cb.push(btn(
                        "Request changes",
                        "ui/warning",
                        GhHit::Ask(Act::RequestChanges),
                        self.style(BtnStyle::Danger),
                    ));
                }
                out.push(It::Composer(cb));
            }
            PrTab::Files => {
                if d.files.is_empty() {
                    out.push(It::Dim("No file changes".into()));
                }
                for (i, f) in d.files.iter().enumerate() {
                    let (icon, tone) = match f.change.as_str() {
                        "ADDED" => ("ui/plus", Tone::Success),
                        "DELETED" => ("ui/minus", Tone::Error),
                        _ => ("ui/file_diff", Tone::Warning),
                    };
                    let open = v.file == Some(i);
                    out.push(It::Row(Row {
                        indent: 0,
                        icon,
                        tone,
                        title: f.path.clone(),
                        sub: String::new(),
                        right: format!("+{} −{}", f.additions, f.deletions),
                        hit: GhHit::File(i),
                        fold: Some((GhHit::File(i), open)),
                    }));
                    if open {
                        match &v.diff {
                            Load::Ready(_) => out.push(It::Diff(i)),
                            Load::Failed(e) => out.push(It::Note(e.clone(), Tone::Error)),
                            _ => out.push(It::Note("Loading diff…".into(), Tone::Muted)),
                        }
                        out.push(It::Space(6));
                    }
                }
            }
            PrTab::Commits => {
                if d.commits.is_empty() {
                    out.push(It::Dim("No commits".into()));
                }
                for (i, c) in d.commits.iter().enumerate() {
                    out.push(It::Row(Row {
                        indent: 0,
                        icon: "ui/git_commit",
                        tone: Tone::Muted,
                        title: c.headline.clone(),
                        sub: format!("{} · {}", model::short_sha(&c.oid), c.author),
                        right: when(&c.date),
                        hit: GhHit::Commit(i),
                        fold: None,
                    }));
                }
            }
            PrTab::Checks => {
                if p.checks.is_empty() {
                    out.push(It::Dim("No checks reported".into()));
                }
                for (i, c) in p.checks.iter().enumerate() {
                    let s = model::check_state(c);
                    let sub =
                        if c.workflow.is_empty() { s.label.clone() } else { format!("{} · {}", c.workflow, s.label) };
                    out.push(It::Row(Row {
                        indent: 0,
                        icon: phase_icon(s.phase),
                        tone: s.tone,
                        title: c.name.clone(),
                        sub,
                        right: String::new(),
                        hit: GhHit::Check(i),
                        fold: None,
                    }));
                }
            }
        }
        out
    }

    fn issue_items(&self, v: &IssueView, now: i64) -> Vec<It> {
        let d = match &v.detail {
            Load::Ready(d) => d,
            Load::Failed(e) => return retry(e),
            _ => return vec![It::Note("Loading issue…".into(), Tone::Muted)],
        };
        let i = &d.item;
        let open = i.state == "OPEN";
        let tone = if open {
            Tone::Success
        } else if i.reason == "NOT_PLANNED" {
            Tone::Muted
        } else {
            Tone::Merged
        };
        let word = match (open, i.reason.as_str()) {
            (true, _) => "Open",
            (false, "NOT_PLANNED") => "Closed as not planned",
            _ => "Closed",
        };
        let mut out = vec![It::Title(format!("{} #{}", i.title, i.number)), It::Badges(vec![(word.to_string(), tone)])];
        out.push(It::Dim(format!(
            "{} opened {} · {}",
            i.author,
            time_ago(&i.created, now),
            plural(d.activity.len(), "comment")
        )));
        out.push(It::Space(8));
        let mut b = Vec::new();
        if open {
            b.push(btn("Close issue", "ui/check", GhHit::Ask(Act::CloseIssue), self.style(BtnStyle::Plain)));
            b.push(btn(
                "Close as not planned",
                "ui/slash",
                GhHit::Ask(Act::CloseIssueNotPlanned),
                self.style(BtnStyle::Plain),
            ));
        } else {
            b.push(btn("Reopen", "ui/rotate_cw", GhHit::Ask(Act::ReopenIssue), self.style(BtnStyle::Plain)));
        }
        out.push(It::Buttons(b));
        if let Some(c) = &self.confirm {
            out.push(It::Space(6));
            out.push(It::Confirm(c.clone()));
        }
        out.push(It::Space(12));
        if !i.labels.is_empty() {
            out.push(It::Chips(i.labels.clone()));
            out.push(It::Space(8));
        }
        if !i.assignees.is_empty() {
            out.push(It::Kv("Assignees".into(), i.assignees.join(", ")));
        }
        if !d.milestone.is_empty() {
            out.push(It::Kv("Milestone".into(), d.milestone.clone()));
        }
        out.push(It::Space(8));
        out.push(It::Heading("Description".into(), 0));
        if d.body.trim().is_empty() {
            out.push(It::Dim("No description provided".into()));
        } else {
            out.push(It::Md(d.body.clone()));
        }
        out.push(It::Space(14));
        self.activity(&mut out, &d.activity);
        out.push(It::Composer(vec![btn(
            "Comment",
            "ui/chat",
            GhHit::Ask(Act::CommentIssue),
            self.style(BtnStyle::Primary),
        )]));
        out
    }

    fn run_items(&self, v: &RunView, now: i64) -> Vec<It> {
        let d = match &v.detail {
            Load::Ready(d) => d,
            Load::Failed(e) => return retry(e),
            _ => return vec![It::Note("Loading run…".into(), Tone::Muted)],
        };
        let r = &d.item;
        let s = model::run_state(&r.status, &r.conclusion);
        let mut badges = vec![(s.label.clone(), s.tone)];
        if r.attempt > 1 {
            badges.push((format!("Attempt {}", r.attempt), Tone::Warning));
        }
        if !r.event.is_empty() {
            badges.push((r.event.clone(), Tone::Muted));
        }
        let mut out = vec![It::Title(r.title.clone()), It::Badges(badges)];
        let took = model::elapsed(
            if r.started.is_empty() { &r.created } else { &r.started },
            &r.updated,
            s.active,
            now_secs(),
        );
        let mut line = format!("#{} · {} · {}", r.number, r.branch, model::short_sha(&r.sha));
        line.push_str(&format!(
            " · started {}",
            time_ago(if r.started.is_empty() { &r.created } else { &r.started }, now)
        ));
        if let Some(t) = took {
            line.push_str(&format!(" · {t}"));
        }
        out.push(It::Dim(line));
        out.push(It::Space(8));
        let mut b = Vec::new();
        if s.active {
            b.push(btn("Cancel run", "ui/stop", GhHit::Ask(Act::CancelRun), self.style(BtnStyle::Danger)));
        } else {
            b.push(btn("Re-run all jobs", "ui/rerun", GhHit::Ask(Act::Rerun), self.style(BtnStyle::Plain)));
            if s.failed {
                b.push(btn(
                    "Re-run failed jobs",
                    "ui/rerun",
                    GhHit::Ask(Act::RerunFailed),
                    self.style(BtnStyle::Primary),
                ));
            }
        }
        out.push(It::Buttons(b));
        if let Some(c) = &self.confirm {
            out.push(It::Space(6));
            out.push(It::Confirm(c.clone()));
        }
        out.push(It::Space(14));
        out.push(It::Heading("Jobs".into(), d.jobs.len()));
        if d.jobs.is_empty() {
            out.push(It::Dim("No jobs reported for this run yet".into()));
        }
        for (i, j) in d.jobs.iter().enumerate() {
            let js = model::run_state(&j.status, &j.conclusion);
            let folded = v.folded.contains(&i);
            let done = j
                .steps
                .iter()
                .filter(|st| model::run_state(&st.status, &st.conclusion).phase == model::Phase::Success)
                .count();
            out.push(It::Row(Row {
                indent: 0,
                icon: phase_icon(js.phase),
                tone: js.tone,
                title: j.name.clone(),
                sub: if j.steps.is_empty() {
                    js.label.clone()
                } else {
                    format!("{} · {}/{} steps", js.label, done, j.steps.len())
                },
                right: model::elapsed(&j.started, &j.completed, js.active, now_secs()).unwrap_or_default(),
                hit: GhHit::Job(i),
                fold: (!j.steps.is_empty()).then_some((GhHit::JobFold(i), !folded)),
            }));
            if !folded {
                for st in &j.steps {
                    let ss = model::run_state(&st.status, &st.conclusion);
                    out.push(It::Row(Row {
                        indent: 28,
                        icon: phase_icon(ss.phase),
                        tone: ss.tone,
                        title: st.name.clone(),
                        sub: String::new(),
                        right: model::elapsed(&st.started, &st.completed, ss.active, now_secs()).unwrap_or_default(),
                        hit: GhHit::Job(i),
                        fold: None,
                    }));
                }
            }
        }
        out
    }
}

/// The question a change asks before it runs (None: it runs at once).
pub fn confirm_for(act: &Act, number: u64) -> Option<Confirm> {
    let c = |text: String, yes: &str, danger: bool| Some(Confirm { text, yes: yes.into(), act: act.clone(), danger });
    match act {
        Act::Merge(how) => {
            let (what, yes) = match *how {
                "squash" => ("Squash and merge", "Squash and merge"),
                "rebase" => ("Rebase and merge", "Rebase and merge"),
                _ => ("Create a merge commit for", "Merge"),
            };
            c(format!("{what} pull request #{number}? The branch is kept."), yes, false)
        }
        Act::ClosePr => c(format!("Close pull request #{number} without merging it?"), "Close pull request", true),
        Act::CloseIssue => c(format!("Close issue #{number}?"), "Close issue", false),
        Act::CloseIssueNotPlanned => c(format!("Close issue #{number} as not planned?"), "Close as not planned", false),
        Act::Rerun => c("Re-run every job of this run?".into(), "Re-run all jobs", false),
        Act::RerunFailed => c("Re-run the failed jobs of this run?".into(), "Re-run failed jobs", false),
        Act::CancelRun => c("Cancel this run?".into(), "Cancel run", true),
        Act::MarkAllRead => c("Mark every notification as read?".into(), "Mark all read", false),
        _ => None,
    }
}

/// Past-tense label of a change, shown while it runs and when it fails.
pub fn act_label(act: &Act) -> &'static str {
    match act {
        Act::Merge(_) => "Merging…",
        Act::Approve => "Approving…",
        Act::RequestChanges => "Requesting changes…",
        Act::CommentPr | Act::CommentIssue => "Commenting…",
        Act::ClosePr | Act::CloseIssue | Act::CloseIssueNotPlanned => "Closing…",
        Act::ReopenPr | Act::ReopenIssue => "Reopening…",
        Act::Ready => "Marking ready…",
        Act::Checkout => "Checking out…",
        Act::Rerun | Act::RerunFailed => "Re-running…",
        Act::CancelRun => "Cancelling…",
        Act::MarkRead(_) | Act::MarkAllRead => "Marking read…",
    }
}

/// The reviewer's note kind of a comment, for the badge on its card.
pub fn review_word(c: &Comment) -> Option<(&'static str, Tone)> {
    if c.kind != CommentKind::Review {
        return None;
    }
    Some(match c.state.as_str() {
        "APPROVED" => ("approved", Tone::Success),
        "CHANGES_REQUESTED" => ("requested changes", Tone::Error),
        "DISMISSED" => ("dismissed", Tone::Muted),
        _ => ("reviewed", Tone::Muted),
    })
}
