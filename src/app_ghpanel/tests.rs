use super::*;
use crate::ghub::{IssueItem, Notif, PrItem, RepoInfo, RunItem};
use draw_log::{LogClass, classify, strip_ansi, view_rows};
use items::{It, confirm_for};
use state::{Act, Detail, Env, GhHit, GhPanel, Load, Msg, PrTab, Section};

fn panel() -> GhPanel {
    let mut g = GhPanel::new(Path::new("/tmp/lumen-gh-test"));
    g.env = Env::Ready(RepoInfo { slug: "o/r".into(), url: "https://github.com/o/r".into() });
    g
}

fn pr(n: u64, title: &str, state: &str, draft: bool) -> PrItem {
    PrItem { number: n, title: title.into(), state: state.into(), draft, author: "ana".into(), ..Default::default() }
}

fn seed_prs(g: &mut GhPanel) {
    let l = g.prs.entry(g.pr_filter).or_default();
    l.items = Some(vec![
        pr(1, "Add search", "OPEN", false),
        pr(2, "WIP parser", "OPEN", true),
        pr(3, "Fix crash", "OPEN", false),
    ]);
    l.at = Some(Instant::now());
}

const PR_JSON: &str = r###"{"number":3,"title":"Fix crash","state":"OPEN","isDraft":false,"body":"## Why\n- it crashed\n<!-- hidden -->","author":{"login":"ana"},
"headRefName":"fix/crash","baseRefName":"main","createdAt":"2026-11-21T10:00:00Z","mergeStateStatus":"CLEAN","mergeable":"MERGEABLE","reviewDecision":"APPROVED",
"additions":5,"deletions":2,"changedFiles":1,"labels":[{"name":"bug","color":"d73a4a"}],"reviewRequests":[{"login":"bob"}],
"comments":[{"author":{"login":"bob"},"body":"looks good","createdAt":"2026-11-21T11:00:00Z"}],"reviews":[],
"files":[{"path":"src/a.rs","additions":5,"deletions":2,"changeType":"MODIFIED"}],
"commits":[{"oid":"0123456789abcdef","messageHeadline":"fix it","authors":[{"login":"ana"}],"committedDate":"2026-11-21T10:00:00Z"}],
"statusCheckRollup":[{"__typename":"CheckRun","name":"Test","workflowName":"CI","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"u"}],
"url":"https://github.com/o/r/pull/3"}"###;

fn open_pr_detail(g: &mut GhPanel, tab: PrTab) {
    let d = ghub::parse_pr_detail(PR_JSON).unwrap();
    g.detail = Detail::Pr(state::PrView { number: 3, detail: Load::Ready(d), tab, diff: Load::Idle, file: None });
}

fn labels(its: &[It]) -> Vec<String> {
    let mut out = Vec::new();
    for it in its {
        if let It::Buttons(b) | It::Composer(b) = it {
            out.extend(b.iter().map(|b| b.label.clone()));
        }
    }
    out
}

#[test]
fn list_groups_open_then_drafts_and_the_query_filters() {
    let mut g = panel();
    seed_prs(&mut g);
    let groups = g.groups();
    assert_eq!(groups.len(), 2);
    assert_eq!((groups[0].title.as_str(), groups[0].items.clone()), ("Open PRs", vec![0, 2]));
    assert_eq!((groups[1].title.as_str(), groups[1].items.clone()), ("Drafts", vec![1]));
    assert_eq!(g.shown_order(), vec![0, 2, 1]);
    g.query = crate::field::TextField::with("  CRASH ");
    assert_eq!(g.shown_order(), vec![2]);
    g.query = crate::field::TextField::with("#2");
    assert_eq!(g.shown_order(), vec![1]);
    // A folded group hides its rows, unless a query is typed.
    g.query = Default::default();
    g.collapsed.insert("Prs:drafts".into());
    assert_eq!(g.shown_order(), vec![0, 2]);
    g.query = crate::field::TextField::with("wip");
    assert_eq!(g.shown_order(), vec![1], "a query opens every group");
}

#[test]
fn filters_change_the_list_that_is_shown() {
    let mut g = panel();
    seed_prs(&mut g);
    g.apply_filter(1); // My PRs: nothing cached for it yet
    assert_eq!(g.pr_filter, ghub::PrFilter::Mine);
    assert!(g.groups().is_empty(), "its own list, not the open one");
    g.apply_filter(0);
    assert_eq!(g.groups().len(), 2, "the open list comes back from the cache");
    g.section = Section::Actions;
    g.runs.items = Some(vec![
        RunItem {
            id: 1,
            status: "completed".into(),
            conclusion: "success".into(),
            created: "2026-11-21T10:00:00Z".into(),
            ..Default::default()
        },
        RunItem {
            id: 2,
            status: "completed".into(),
            conclusion: "failure".into(),
            created: "2026-11-21T09:00:00Z".into(),
            ..Default::default()
        },
    ]);
    g.apply_filter(3); // Failed
    assert_eq!(g.run_filter, RunFilter::Failed);
    assert_eq!(g.shown_order(), vec![1]);
}

#[test]
fn issues_and_inbox_group() {
    let mut g = panel();
    g.section = Section::Issues;
    let l = g.issues.entry(g.issue_filter).or_default();
    l.items = Some(vec![
        IssueItem { number: 1, title: "a".into(), state: "OPEN".into(), ..Default::default() },
        IssueItem { number: 2, title: "b".into(), state: "CLOSED".into(), ..Default::default() },
    ]);
    assert_eq!(g.groups().iter().map(|x| x.title.as_str()).collect::<Vec<_>>(), vec!["Open", "Closed"]);
    g.section = Section::Inbox;
    g.inbox.items = Some(vec![
        Notif { id: "1".into(), unread: false, title: "old".into(), ..Default::default() },
        Notif { id: "2".into(), unread: true, title: "new".into(), repo: "o/r".into(), ..Default::default() },
    ]);
    let gr = g.groups();
    assert_eq!((gr[0].title.as_str(), gr[0].items.clone()), ("Unread", vec![1]));
    g.mark_read_local(1);
    assert_eq!(g.groups()[0].title, "Earlier", "an opened notification is read");
}

#[test]
fn pump_applies_worker_results_and_caches_details() {
    let mut g = panel();
    g.tx.send(Msg::Prs(ghub::PrFilter::Open, Ok(vec![pr(9, "Nine", "OPEN", false)]))).unwrap();
    g.tx.send(Msg::Runs(Err("boom".into()))).unwrap();
    let (changed, done) = g.pump();
    assert!(changed && done.is_empty());
    assert_eq!(g.pr_list().unwrap().len(), 1);
    assert!(!g.pr_list().unwrap().loading);
    assert_eq!(g.runs.error.as_deref(), Some("boom"));
    // The detail of a pull request that is not open any more is cached but not shown.
    g.detail = Detail::Pr(state::PrView {
        number: 3,
        detail: Load::Loading,
        tab: PrTab::Overview,
        diff: Load::Idle,
        file: None,
    });
    g.tx.send(Msg::PrDetail(8, Ok(ghub::parse_pr_detail(&PR_JSON.replace("\"number\":3", "\"number\":8")).unwrap())))
        .unwrap();
    g.pump();
    assert!(g.pr_cache.contains_key(&8));
    assert!(matches!(&g.detail, Detail::Pr(v) if v.detail.ready().is_none()));
    g.tx.send(Msg::PrDetail(3, Ok(ghub::parse_pr_detail(PR_JSON).unwrap()))).unwrap();
    g.pump();
    assert!(matches!(&g.detail, Detail::Pr(v) if v.detail.ready().is_some()));
    // Opening it again shows the cached copy at once.
    g.open_pr(3);
    assert!(matches!(&g.detail, Detail::Pr(v) if v.detail.ready().is_some()));
    // A failed refresh keeps what is shown; a failed first load says why.
    g.tx.send(Msg::PrDetail(3, Err("offline".into()))).unwrap();
    g.pump();
    assert!(matches!(&g.detail, Detail::Pr(v) if v.detail.ready().is_some()));
    g.detail = Detail::Pr(state::PrView {
        number: 4,
        detail: Load::Loading,
        tab: PrTab::Overview,
        diff: Load::Idle,
        file: None,
    });
    g.tx.send(Msg::PrDetail(4, Err("offline".into()))).unwrap();
    g.pump();
    assert!(matches!(&g.detail, Detail::Pr(v) if matches!(&v.detail, Load::Failed(e) if e == "offline")));
}

#[test]
fn a_change_finishing_frees_the_page_and_reports() {
    let mut g = panel();
    g.busy = Some("Merging…".into());
    g.tx.send(Msg::Done {
        label: "Merging…".into(),
        result: Ok("Merged pull request #3\n".into()),
        act: Act::Merge("squash"),
    })
    .unwrap();
    let (_, done) = g.pump();
    assert!(g.busy.is_none());
    assert_eq!(done.len(), 1);
    let (label, result, act) = &done[0];
    g.after_act(label, result, act);
    assert_eq!(g.note.as_ref().map(|n| (n.0.as_str(), n.1)), Some(("Merged pull request #3", true)));
    g.after_act("Merging…", &Err("not mergeable".into()), &Act::Merge("merge"));
    assert_eq!(g.note.as_ref().map(|n| (n.0.as_str(), n.1)), Some(("Merging…: not mergeable", false)));
    // Marking a notification read does not free a change that is running.
    g.busy = Some("Closing…".into());
    g.tx.send(Msg::Done { label: "Mark read".into(), result: Ok(String::new()), act: Act::MarkRead("1".into()) })
        .unwrap();
    g.pump();
    assert!(g.busy.is_some());
}

#[test]
fn changes_run_one_at_a_time_with_the_right_arguments() {
    let mut g = panel();
    open_pr_detail(&mut g, PrTab::Overview);
    g.composer.set_text("Looks good to me");
    assert_eq!(g.act_args(&Act::Approve).unwrap(), ["pr", "review", "3", "--approve", "--body", "Looks good to me"]);
    assert_eq!(g.act_args(&Act::CommentPr).unwrap(), ["pr", "comment", "3", "--body", "Looks good to me"]);
    assert_eq!(g.act_args(&Act::Merge("squash")).unwrap()[..4], ["pr", "merge", "3", "--squash"]);
    assert_eq!(g.act_args(&Act::Checkout).unwrap(), ["pr", "checkout", "3"]);
    assert!(g.act_args(&Act::Rerun).is_none(), "a run action needs a run open");
    g.detail = Detail::None;
    assert!(g.act_args(&Act::ClosePr).is_none());
    g.busy = Some("x".into());
    g.run_act(Act::MarkAllRead, "Marking read…");
    assert_eq!(g.busy.as_deref(), Some("x"), "one change at a time");
    assert_eq!(g.act_args(&Act::CloseIssueNotPlanned), None, "no issue open");
    g.detail = Detail::Issue(state::IssueView { number: 5, detail: Load::Loading });
    assert_eq!(g.act_args(&Act::CloseIssueNotPlanned).unwrap(), ["issue", "close", "5", "--reason", "not planned"]);
}

#[test]
fn what_asks_first() {
    assert!(confirm_for(&Act::Merge("squash"), 3).unwrap().text.starts_with("Squash and merge pull request #3"));
    assert!(confirm_for(&Act::ClosePr, 3).unwrap().danger);
    assert!(confirm_for(&Act::CancelRun, 3).unwrap().danger);
    assert!(confirm_for(&Act::Approve, 3).is_none());
    assert!(confirm_for(&Act::Checkout, 3).is_none());
    assert!(confirm_for(&Act::CommentPr, 3).is_none());
}

#[test]
fn pull_request_overview_items() {
    let mut g = panel();
    open_pr_detail(&mut g, PrTab::Overview);
    let its = g.detail_items(crate::ghub_model::parse_iso("2026-11-21T12:00:00Z").unwrap());
    assert!(matches!(&its[0], It::Title(t) if t == "Fix crash #3"));
    let badges = its.iter().find_map(|i| if let It::Badges(b) = i { Some(b.clone()) } else { None }).unwrap();
    let words: Vec<&str> = badges.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(words, vec!["Open", "Approved", "1/1 checks passed"]);
    assert!(
        its.iter()
            .any(|i| matches!(i, It::Dim(t) if t.starts_with("ana wants to merge 1 commit into main from fix/crash")))
    );
    let l = labels(&its);
    assert!(
        l.contains(&"Merge".to_string()) && l.contains(&"Close".to_string()) && l.contains(&"Checkout".to_string())
    );
    assert!(
        l.contains(&"Comment".to_string())
            && l.contains(&"Approve".to_string())
            && l.contains(&"Request changes".to_string())
    );
    assert!(its.iter().any(|i| matches!(i, It::Kv(k, v) if k == "Reviewers" && v == "bob")));
    assert!(its.iter().any(|i| matches!(i, It::Comment(c) if c.author == "bob")));
    assert!(its.iter().any(|i| matches!(i, It::Tabs(t) if t[1].0 == "Files 1")));
    // While something runs, every button is off.
    g.busy = Some("Merging…".into());
    let its = g.detail_items(0);
    assert!(its.iter().all(|i| match i {
        It::Buttons(b) | It::Composer(b) => b.iter().all(|b| b.style == items::BtnStyle::Off),
        _ => true,
    }));
}

#[test]
fn closed_and_merged_pull_requests_offer_less() {
    let mut g = panel();
    let json = PR_JSON.replace("\"state\":\"OPEN\"", "\"state\":\"CLOSED\"");
    g.detail = Detail::Pr(state::PrView {
        number: 3,
        detail: Load::Ready(ghub::parse_pr_detail(&json).unwrap()),
        tab: PrTab::Overview,
        diff: Load::Idle,
        file: None,
    });
    let l = labels(&g.detail_items(0));
    assert!(
        l.contains(&"Reopen".to_string()) && !l.contains(&"Merge".to_string()) && !l.contains(&"Approve".to_string())
    );
    let json = PR_JSON.replace("\"state\":\"OPEN\"", "\"state\":\"MERGED\"");
    g.detail = Detail::Pr(state::PrView {
        number: 3,
        detail: Load::Ready(ghub::parse_pr_detail(&json).unwrap()),
        tab: PrTab::Overview,
        diff: Load::Idle,
        file: None,
    });
    let l = labels(&g.detail_items(0));
    assert!(!l.contains(&"Checkout".to_string()) && !l.contains(&"Close".to_string()));
}

#[test]
fn blocked_pull_requests_say_why_instead_of_merge() {
    let mut g = panel();
    let json = PR_JSON.replace("\"mergeStateStatus\":\"CLEAN\"", "\"mergeStateStatus\":\"BEHIND\"");
    g.detail = Detail::Pr(state::PrView {
        number: 3,
        detail: Load::Ready(ghub::parse_pr_detail(&json).unwrap()),
        tab: PrTab::Overview,
        diff: Load::Idle,
        file: None,
    });
    let l = labels(&g.detail_items(0));
    assert!(l.contains(&"Behind base branch".to_string()) && !l.contains(&"Merge".to_string()));
}

#[test]
fn files_commits_and_checks_tabs() {
    let mut g = panel();
    open_pr_detail(&mut g, PrTab::Files);
    let its = g.detail_items(0);
    let row = its.iter().find_map(|i| if let It::Row(r) = i { Some(r.clone()) } else { None }).unwrap();
    assert_eq!((row.title.as_str(), row.right.as_str(), row.hit.clone()), ("src/a.rs", "+5 −2", GhHit::File(0)));
    // The diff of an opened file shows once it arrived.
    if let Detail::Pr(v) = &mut g.detail {
        v.file = Some(0);
        v.diff = Load::Loading;
    }
    assert!(g.detail_items(0).iter().any(|i| matches!(i, It::Note(t, _) if t == "Loading diff…")));
    if let Detail::Pr(v) = &mut g.detail {
        v.diff = Load::Ready(ghub::parse_diff("diff --git a/src/a.rs b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n"));
    }
    assert!(g.detail_items(0).iter().any(|i| matches!(i, It::Diff(0))));
    assert_eq!(g.diff_lines(0), Some((3, 0)));
    if let Detail::Pr(v) = &mut g.detail {
        v.tab = PrTab::Commits;
    }
    let its = g.detail_items(0);
    assert!(its.iter().any(|i| matches!(i, It::Row(r) if r.sub == "0123456 · ana" && r.title == "fix it")));
    if let Detail::Pr(v) = &mut g.detail {
        v.tab = PrTab::Checks;
    }
    let its = g.detail_items(0);
    assert!(
        its.iter().any(
            |i| matches!(i, It::Row(r) if r.title == "Test" && r.sub == "CI · Success" && r.hit == GhHit::Check(0))
        )
    );
}

#[test]
fn issue_items_follow_the_state() {
    let mut g = panel();
    let d = ghub::parse_issue_detail(
        r#"{"number":5,"title":"Bug","state":"CLOSED","stateReason":"NOT_PLANNED","author":{"login":"q"},"body":"","comments":[],"labels":[{"name":"x","color":"fff"}],"assignees":[{"login":"w"}],"milestone":{"title":"v1"}}"#,
    )
    .unwrap();
    g.detail = Detail::Issue(state::IssueView { number: 5, detail: Load::Ready(d) });
    let its = g.detail_items(0);
    assert!(its.iter().any(|i| matches!(i, It::Badges(b) if b[0].0 == "Closed as not planned")));
    let l = labels(&its);
    assert!(l.contains(&"Reopen".to_string()) && !l.contains(&"Close issue".to_string()));
    assert!(its.iter().any(|i| matches!(i, It::Dim(t) if t == "No description provided")));
    assert!(its.iter().any(|i| matches!(i, It::Kv(k, v) if k == "Milestone" && v == "v1")));
}

fn run_detail(status: &str, conclusion: &str) -> state::RunView {
    let j = format!(
        r#"{{"databaseId":77,"displayTitle":"CI","status":"{status}","conclusion":"{conclusion}","number":12,"headBranch":"main","headSha":"0123456789","event":"push","attempt":2,
        "jobs":[{{"databaseId":10,"name":"build","status":"completed","conclusion":"failure","steps":[{{"name":"Set up","number":1,"status":"completed","conclusion":"success"}},{{"name":"Test","number":2,"status":"completed","conclusion":"failure"}}]}}]}}"#
    );
    state::RunView {
        id: 77,
        detail: Load::Ready(ghub::parse_run_detail(&j).unwrap()),
        job: None,
        log: Load::Idle,
        log_failed_only: false,
        log_filter: Default::default(),
        log_filter_focus: false,
        folded: Default::default(),
        polled: Instant::now(),
    }
}

#[test]
fn run_items_list_jobs_and_steps_and_offer_reruns() {
    let mut g = panel();
    g.detail = Detail::Run(run_detail("completed", "failure"));
    let its = g.detail_items(0);
    let l = labels(&its);
    assert!(l.contains(&"Re-run all jobs".to_string()) && l.contains(&"Re-run failed jobs".to_string()));
    assert!(!l.contains(&"Cancel run".to_string()));
    let rows: Vec<_> = its.iter().filter_map(|i| if let It::Row(r) = i { Some(r.clone()) } else { None }).collect();
    assert_eq!(rows.len(), 3, "the job and its two steps");
    assert_eq!(rows[0].sub, "Failed · 1/2 steps");
    assert_eq!(rows[0].fold, Some((GhHit::JobFold(0), true)));
    assert_eq!(rows[2].hit, GhHit::Job(0), "a step opens the job's log");
    // Folding a job hides its steps.
    if let Detail::Run(v) = &mut g.detail {
        v.folded.insert(0);
    }
    assert_eq!(g.detail_items(0).iter().filter(|i| matches!(i, It::Row(_))).count(), 1);
    // A run that is going can be cancelled, not re-run; and it is polled.
    g.detail = Detail::Run(run_detail("in_progress", ""));
    let l = labels(&g.detail_items(0));
    assert!(l.contains(&"Cancel run".to_string()) && !l.contains(&"Re-run all jobs".to_string()));
    assert!(g.run_active());
}

#[test]
fn failed_loads_offer_a_retry() {
    let mut g = panel();
    g.detail = Detail::Issue(state::IssueView { number: 5, detail: Load::Failed("HTTP 404".into()) });
    let its = g.detail_items(0);
    assert!(matches!(&its[0], It::Note(t, _) if t == "HTTP 404"));
    assert!(labels(&its).contains(&"Try again".to_string()));
}

#[test]
fn log_lines_are_classified_and_filtered() {
    assert_eq!(strip_ansi("\u{1b}[31;1mred\u{1b}[0m text"), "red text");
    assert_eq!(
        classify("##[error]Process completed with exit code 1."),
        (LogClass::Error, "Process completed with exit code 1.".into())
    );
    assert_eq!(classify("##[warning]careful").0, LogClass::Warning);
    assert_eq!(classify("##[endgroup]").0, LogClass::EndGroup);
    assert_eq!(classify("error[E0308]: mismatched types").0, LogClass::Error);
    assert_eq!(classify("plain").0, LogClass::Normal);
    let line = |step: &str, text: &str| ghub::LogLine { step: step.into(), time: String::new(), text: text.into() };
    let lines = vec![
        line("Build", "##[group]Compile"),
        line("Build", "compiling a"),
        line("Build", "##[endgroup]"),
        line("Test", "ok"),
        line("Test", "FAILED b"),
    ];
    // Unfiltered: a header before each step, the end-of-group line dropped.
    assert_eq!(view_rows(&lines, ""), vec![(0, true), (0, false), (1, false), (3, true), (3, false), (4, false)]);
    // Filtered: matching lines only, no headers.
    assert_eq!(view_rows(&lines, "failed"), vec![(4, false)]);
    assert!(view_rows(&lines, "zzz").is_empty());
}

#[test]
fn the_copied_log_is_what_is_shown() {
    let mut g = panel();
    let mut v = run_detail("completed", "failure");
    v.job = Some(0);
    v.log = Load::Ready(vec![
        ghub::LogLine { step: "a".into(), time: String::new(), text: "one".into() },
        ghub::LogLine { step: "a".into(), time: String::new(), text: "##[error]two".into() },
    ]);
    g.detail = Detail::Run(v);
    assert_eq!(g.log_text(), "one\ntwo");
    if let Detail::Run(v) = &mut g.detail {
        v.log_filter = crate::field::TextField::with("TW");
    }
    assert_eq!(g.log_text(), "two");
}

#[test]
fn sections_and_geometry() {
    assert_eq!(Section::ALL.len(), 4);
    assert_eq!(draw::list_width(1000), 360);
    assert_eq!(draw::list_width(760), 300, "never narrower than 300");
    assert_eq!(draw::list_width(2000), 420, "nor wider than 420");
    assert_eq!(draw::list_width(500), 500, "a narrow page is all list (or all detail)");
    let mut g = panel();
    g.set_section(Section::Issues);
    assert_eq!(g.section, Section::Issues);
    g.query = crate::field::TextField::with("x");
    g.set_section(Section::Actions);
    assert!(g.query.text.is_empty(), "the query belongs to a section");
}

#[test]
fn the_cursor_stays_in_view() {
    let mut g = panel();
    seed_prs(&mut g);
    g.list_h = 120;
    g.cursor = Some(1);
    g.reveal_cursor();
    // Group "Open" (28) + 2 rows (104) + group "Drafts" (28) + row: the draft row is the 3rd item.
    assert!(g.scroll[0] > 0.0, "scrolled down to the draft row");
    g.cursor = Some(0);
    g.reveal_cursor();
    assert!(g.scroll[0] <= 28.0);
}

mod app {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    fn frame(app: &mut App, w: i32, h: i32) {
        let mut px = vec![0u32; (w * h) as usize];
        app.size = (w as u32, h as u32);
        app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
    }

    fn open_page(app: &mut App, dir: &Path) {
        let mut g = Box::new(panel());
        g.root = app.git_root();
        let _ = dir;
        seed_prs(&mut g);
        g.open = true;
        g.focused = true;
        app.gh = Some(g);
    }

    #[test]
    fn the_page_draws_its_list_and_answers_the_pointer() {
        let _g = lock();
        let dir = tmp("ghpage");
        let mut app = test_app(&dir);
        open_page(&mut app, &dir);
        frame(&mut app, 1200, 800);
        let slot = app.slot_rect();
        let page = app.gh_rect();
        assert_eq!(page.y, slot.y + crate::app_aipanel::SLOT_H, "under the Git | GitHub | AI switch");
        assert_eq!(page.right(), app.size.0 as i32, "docked on the right, like Git and AI");
        assert!(app.gh_open() && app.gitpanel_w() >= 460, "wide enough for the list and the detail");
        assert_eq!(app.layout.editor.right(), slot.x, "the editor makes room for it");
        let g = app.gh.as_ref().unwrap();
        for s in Section::ALL {
            assert!(g.hits.iter().any(|(_, h)| *h == GhHit::Section(s)), "tab {s:?}");
        }
        assert!(g.hits.iter().any(|(_, h)| *h == GhHit::Row(2)), "a row for each pull request");
        assert!(g.hits.iter().any(|(_, h)| *h == GhHit::Search) && g.hits.iter().any(|(_, h)| *h == GhHit::FilterMenu));
        // The pointer: a hand on rows and tabs, an I-beam on the filter box.
        let at =
            |h: GhHit| g.hits.iter().find(|(_, x)| *x == h).map(|(r, _)| (r.x as f64 + 5.0, r.y as f64 + 5.0)).unwrap();
        let (rx, ry) = at(GhHit::Row(0));
        let (sx, sy) = at(GhHit::Search);
        let (tx, ty) = at(GhHit::Section(Section::Issues));
        assert_eq!(app.ghpanel_cursor(rx, ry), Some(CursorIcon::Pointer));
        assert_eq!(app.ghpanel_cursor(sx, sy), Some(CursorIcon::Text));
        assert_eq!(app.ghpanel_cursor(tx, ty), Some(CursorIcon::Pointer));
        assert_eq!(app.ghpanel_cursor(1.0, 1.0), None, "outside the page the pointer is not ours");
        // The detail pane is empty until something is picked.
        assert!(matches!(app.gh.as_ref().unwrap().detail, Detail::None));
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_click_opens_the_pull_request_and_back_closes_it() {
        let _g = lock();
        let dir = tmp("ghclick");
        let mut app = test_app(&dir);
        open_page(&mut app, &dir);
        frame(&mut app, 1200, 1000);
        let (x, y) = {
            let g = app.gh.as_ref().unwrap();
            let r = g.hits.iter().find(|(_, h)| *h == GhHit::Row(2)).map(|(r, _)| *r).unwrap();
            (r.x as f64 + 20.0, r.y as f64 + 10.0)
        };
        assert!(app.ghpanel_mouse(x, y, 0));
        assert!(matches!(&app.gh.as_ref().unwrap().detail, Detail::Pr(v) if v.number == 3));
        // Give it the data the worker would bring, then draw the detail.
        let g = app.gh.as_mut().unwrap();
        g.detail = Detail::Pr(state::PrView {
            number: 3,
            detail: Load::Ready(ghub::parse_pr_detail(PR_JSON).unwrap()),
            tab: PrTab::Overview,
            diff: Load::Idle,
            file: None,
        });
        frame(&mut app, 1200, 1000);
        let g = app.gh.as_ref().unwrap();
        for h in [GhHit::OpenWeb, GhHit::CopyLink, GhHit::MergeMenu, GhHit::Tab(PrTab::Files), GhHit::Composer] {
            assert!(g.hits.iter().any(|(_, x)| *x == h), "{h:?} is drawn in the detail");
        }
        assert!(g.hits.iter().any(|(_, x)| *x == GhHit::Ask(Act::Approve)));
        assert!(g.composer_box.w > 0, "the comment box has a place");
        let (bx, by) = g
            .hits
            .iter()
            .find(|(_, h)| *h == GhHit::Composer)
            .map(|(r, _)| (r.x as f64 + 8.0, r.y as f64 + 8.0))
            .unwrap();
        assert_eq!(app.ghpanel_cursor(bx, by), Some(CursorIcon::Text));
        // Escape from the list goes back to "nothing open".
        app.gh.as_mut().unwrap().composer_focus = false;
        app.handle_key(&KeyIn { key: Key::Named(NamedKey::Escape), text: None });
        assert!(matches!(app.gh.as_ref().unwrap().detail, Detail::None));
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_narrow_window_shows_one_pane_with_a_back_button() {
        let _g = lock();
        let dir = tmp("ghnarrow");
        let mut app = test_app(&dir);
        open_page(&mut app, &dir);
        frame(&mut app, 700, 700);
        {
            let g = app.gh.as_ref().unwrap();
            assert!(g.hits.iter().any(|(_, h)| *h == GhHit::Row(0)), "the list shows");
            assert!(!g.hits.iter().any(|(_, h)| *h == GhHit::OpenWeb));
        }
        let g = app.gh.as_mut().unwrap();
        g.detail = Detail::Pr(state::PrView {
            number: 1,
            detail: Load::Loading,
            tab: PrTab::Overview,
            diff: Load::Idle,
            file: None,
        });
        frame(&mut app, 700, 700);
        let g = app.gh.as_ref().unwrap();
        assert!(g.hits.iter().any(|(_, h)| *h == GhHit::Back), "the detail has a Back button");
        assert!(!g.hits.iter().any(|(_, h)| *h == GhHit::Row(0)), "and hides the list");
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_gh_the_page_explains_and_offers_the_way_out() {
        let _g = lock();
        let dir = tmp("ghnogh");
        let mut app = test_app(&dir);
        open_page(&mut app, &dir);
        for (env, hit) in [
            (Env::NoGh, GhHit::Link("https://cli.github.com".into())),
            (Env::NoAuth, GhHit::Retry),
            (Env::NoRepo, GhHit::Retry),
            (Env::Failed("rate limited".into()), GhHit::Retry),
        ] {
            app.gh.as_mut().unwrap().env = env.clone();
            frame(&mut app, 1200, 800);
            let g = app.gh.as_ref().unwrap();
            assert!(g.hits.iter().any(|(_, h)| *h == hit), "{env:?} offers {hit:?}");
            assert!(!g.hits.iter().any(|(_, h)| matches!(h, GhHit::Row(_))), "no list without a repository");
        }
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pages_share_the_slot() {
        let _g = lock();
        let dir = tmp("ghslot");
        let mut app = test_app(&dir);
        open_page(&mut app, &dir);
        app.open_git_panel();
        assert!(!app.gh_open() && app.gitpanel.is_some(), "Git steps the GitHub page aside");
        open_page(&mut app, &dir);
        app.ai_show();
        assert!(!app.gh_open() && app.ai.open, "and so does the AI panel");
        app.ai_hide();
        open_page(&mut app, &dir);
        app.gh_hide();
        assert!(!app.gh_open());
        assert!(app.gh.is_some(), "its cache stays for the next time");
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typing_in_the_filter_narrows_the_list() {
        let _g = lock();
        let dir = tmp("ghfilter");
        let mut app = test_app(&dir);
        open_page(&mut app, &dir);
        frame(&mut app, 1200, 800);
        let (x, y) = {
            let g = app.gh.as_ref().unwrap();
            let r = g.hits.iter().find(|(_, h)| *h == GhHit::Search).map(|(r, _)| *r).unwrap();
            (r.x as f64 + 40.0, r.y as f64 + 8.0)
        };
        assert!(app.ghpanel_mouse(x, y, 0));
        assert!(app.gh.as_ref().unwrap().query_focus);
        for c in "crash".chars() {
            let t = c.to_string();
            app.handle_key(&KeyIn { key: Key::Character(t.clone().into()), text: Some(t) });
        }
        assert_eq!(app.gh.as_ref().unwrap().query.text, "crash");
        assert_eq!(app.gh.as_ref().unwrap().shown_order(), vec![2]);
        frame(&mut app, 1200, 800);
        assert!(!app.gh.as_ref().unwrap().hits.iter().any(|(_, h)| *h == GhHit::Row(0)), "the other rows are gone");
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_rail_and_commands_reach_the_page() {
        use crate::app_rail::RailBtn;
        assert!(RailBtn::TOP.contains(&RailBtn::GitHub));
        assert_eq!(RailBtn::GitHub.title(), "GitHub");
        assert!(crate::app_overlay::Cmd::from_id("GithubPanel").is_some(), "bound in keybindings.json by name");
        assert!(crate::app_overlay::COMMANDS.iter().any(|(n, _, _)| *n == "GitHub: Open panel"));
        let k = crate::keymap::Keymap::default();
        assert_eq!(k.lookup(true, false, true, &Key::Character("g".into())), Some("GithubPanel"));
    }
}
