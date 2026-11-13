//! App glue of the interactive rebase (see `gitrebase`): the plan is edited in pickers (one row per
//! commit, oldest first; Enter on a row chooses pick / reword / squash / fixup / drop or moves it),
//! "Start rebase" runs it on `gitsync`'s worker thread, and a stop on conflicts shows the usual
//! Continue / Abort card of the panel and opens the merge editor on the first conflicted file.
//!
//! The plan lives in `GitBg::rebase` while it is being edited; nothing is written to disk until
//! "Start rebase".

use super::*;
use crate::{
    app_gitext::BgMsg,
    app_overlay::{Action, Item, Overlay, choice_picker},
    gitrebase::{self, RebasePlan, TodoAction},
};

/// Choices of the rebase pickers (`GitPick::Rebase`).
#[derive(Clone, Debug, PartialEq)]
pub enum RebasePick {
    /// A plan of the last N commits.
    Last(usize),
    /// Choose the branch to rebase onto.
    OntoMenu,
    /// A plan of the commits that are not in this branch, to be replayed onto it.
    Onto(String),
    /// Choose what happens to commit `i` of the plan.
    Edit(usize),
    Set(usize, TodoAction),
    /// Move commit `i` by -1 (older) or +1 (newer).
    Move(usize, i32),
    /// Prompt for the new subject of commit `i`.
    Reword(usize),
    /// Show the plan again, the cursor on row `i`.
    Plan(usize),
    Start,
    Cancel,
    /// Back to the plan being edited.
    Resume,
}

/// Card buttons of a rebase in progress (`GitAct::Rebase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebaseAct {
    Continue,
    AbortAsk,
    Abort,
    /// Open the merge editor on the first conflicted file.
    OpenMerge,
}

/// The card shown while a rebase waits for its conflicts to be resolved.
pub fn rebase_card() -> Ask {
    Ask {
        text: "Rebase in progress: resolve the conflicting files and stage them, then Continue. Abort puts the branch back where it was.".into(),
        warn: false,
        buttons: vec![
            btn("Continue", GitAct::Rebase(RebaseAct::Continue), Tone::Accent),
            btn("Abort…", GitAct::Rebase(RebaseAct::AbortAsk), Tone::Danger),
        ],
    }
}

fn pick(p: RebasePick) -> Action {
    Action::Git(GitPick::Rebase(p))
}

fn item(label: impl Into<String>, detail: impl Into<String>, p: RebasePick) -> Item {
    Item { label: label.into(), detail: detail.into(), action: pick(p) }
}

/// Row of the plan: `reword  1a2b3c4  subject`.
pub fn step_label(s: &gitrebase::Step) -> String {
    format!("{:<6}  {}  {}", s.action.word(), s.short(), s.shown_subject())
}

/// What the plan does to the history, one line.
pub fn plan_summary(p: &RebasePlan) -> String {
    let kept = p.steps.iter().filter(|s| s.action != TodoAction::Drop).count();
    let target = match &p.upstream {
        Some(u) => format!("onto {}", u.chars().take(12).collect::<String>()),
        None => "from the first commit".to_string(),
    };
    format!("{kept} of {} commits kept, {target}", p.steps.len())
}

impl App {
    /// "Git: Interactive rebase…": how many commits, or onto which branch.
    pub fn rebase_menu(&mut self) {
        if self.git_top().is_none() {
            return;
        }
        let mut items: Vec<Item> = Vec::new();
        if let Some(p) = &self.gitbg.rebase {
            items.push(item("Continue editing the plan", plan_summary(p), RebasePick::Resume));
        }
        for n in [3usize, 5, 10, 20] {
            items.push(item(
                format!("Last {n} commits"),
                "reorder, reword, squash, fixup or drop them",
                RebasePick::Last(n),
            ));
        }
        items.push(item("Onto a branch…", "replay this branch's commits on top of another", RebasePick::OntoMenu));
        self.overlay = Overlay::Picker(choice_picker("Interactive rebase", items, 0));
        self.request_redraw();
    }

    /// The commit menu's "Interactive Rebase from Here…": that commit and everything after it.
    pub fn rebase_from(&mut self, hash: String) {
        let Some(top) = self.git_top() else { return };
        self.say("Reading the commits…");
        self.git_bg(move || BgMsg::Rebase(gitrebase::plan_from(&top, &hash).map(Box::new)));
    }

    fn rebase_last(&mut self, n: usize) {
        let Some(top) = self.git_top() else { return };
        self.say("Reading the commits…");
        self.git_bg(move || BgMsg::Rebase(gitrebase::plan_last(&top, n).map(Box::new)));
    }

    fn rebase_onto(&mut self, branch: String) {
        let Some(top) = self.git_top() else { return };
        self.say("Reading the commits…");
        self.git_bg(move || BgMsg::Rebase(gitrebase::plan_onto(&top, &branch).map(Box::new)));
    }

    /// A plan arrived from the worker: start editing it.
    pub(crate) fn rebase_begin(&mut self, plan: RebasePlan) {
        self.gitbg.rebase = Some(plan);
        self.rebase_show(0);
    }

    /// The plan as a picker: a row per commit, then "Start rebase" and "Cancel".
    fn rebase_show(&mut self, sel: usize) {
        let Some(plan) = self.gitbg.rebase.as_ref() else { return };
        let mut items: Vec<Item> = plan
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| item(step_label(s), s.action.hint(), RebasePick::Edit(i)))
            .collect();
        items.push(item("Start rebase", plan_summary(plan), RebasePick::Start));
        items.push(item("Cancel", "forget the plan, nothing is changed", RebasePick::Cancel));
        self.overlay =
            Overlay::Picker(choice_picker("Interactive rebase · oldest commit first · Enter edits a row", items, sel));
        self.request_redraw();
    }

    /// What to do with commit `i`.
    fn rebase_edit(&mut self, i: usize) {
        let Some(plan) = self.gitbg.rebase.as_ref() else { return };
        let Some(s) = plan.steps.get(i) else { return self.rebase_show(0) };
        let mut items: Vec<Item> = TodoAction::ALL
            .iter()
            .map(|a| {
                let label = if *a == s.action { format!("{}  (now)", a.word()) } else { a.word().to_string() };
                let p = if *a == TodoAction::Reword { RebasePick::Reword(i) } else { RebasePick::Set(i, *a) };
                item(label, a.hint(), p)
            })
            .collect();
        if i > 0 {
            items.push(item("Move up", "make it older", RebasePick::Move(i, -1)));
        }
        if i + 1 < plan.steps.len() {
            items.push(item("Move down", "make it newer", RebasePick::Move(i, 1)));
        }
        items.push(item("Back", "", RebasePick::Plan(i)));
        let cur = TodoAction::ALL.iter().position(|a| *a == s.action).unwrap_or(0);
        let title = format!("{}  {}", s.short(), s.shown_subject());
        self.overlay = Overlay::Picker(choice_picker(&title, items, cur));
        self.request_redraw();
    }

    /// A row of the rebase pickers was chosen.
    pub(super) fn rebase_pick(&mut self, p: RebasePick) {
        match p {
            RebasePick::Last(n) => self.rebase_last(n),
            RebasePick::OntoMenu => {
                let Some(top) = self.git_top() else { return };
                let current = current_branch(&top);
                let items: Vec<Item> = local_branches(&top)
                    .into_iter()
                    .filter(|b| Some(b) != current.as_ref())
                    .map(|b| item(b.clone(), "replay the commits of this branch on top of it", RebasePick::Onto(b)))
                    .collect();
                if items.is_empty() {
                    return self.git_report("There is no other branch to rebase onto", false);
                }
                self.overlay = Overlay::Picker(choice_picker("Rebase onto…", items, 0));
                self.request_redraw();
            }
            RebasePick::Onto(b) => self.rebase_onto(b),
            RebasePick::Edit(i) => self.rebase_edit(i),
            RebasePick::Set(i, a) => {
                if let Some(s) = self.gitbg.rebase.as_mut().and_then(|p| p.steps.get_mut(i)) {
                    s.action = a;
                }
                self.rebase_show(i);
            }
            RebasePick::Move(i, d) => {
                let j = i as i64 + d as i64;
                let mut at = i;
                if let Some(p) = self.gitbg.rebase.as_mut()
                    && j >= 0
                    && (j as usize) < p.steps.len()
                {
                    p.steps.swap(i, j as usize);
                    at = j as usize;
                }
                self.rebase_show(at);
            }
            RebasePick::Reword(i) => {
                let Some(s) = self.gitbg.rebase.as_ref().and_then(|p| p.steps.get(i)) else { return };
                let text = s.shown_subject().to_string();
                let title = format!("New subject for {} (the rest of the message is kept)", s.short());
                self.git_prompt(GitAsk::RebaseReword(i), title, &text);
            }
            RebasePick::Plan(i) => self.rebase_show(i),
            RebasePick::Resume => self.rebase_show(0),
            RebasePick::Start => self.rebase_start(),
            RebasePick::Cancel => {
                self.gitbg.rebase = None;
                self.say("Rebase plan discarded");
            }
        }
    }

    /// The new subject of commit `i` was typed.
    pub(super) fn rebase_reworded(&mut self, i: usize, text: String) {
        let text = text.trim().to_string();
        if let Some(s) = self.gitbg.rebase.as_mut().and_then(|p| p.steps.get_mut(i))
            && !text.is_empty()
        {
            s.action = TodoAction::Reword;
            s.new_subject = Some(text);
        }
        self.rebase_show(i);
    }

    /// "Start rebase": write the todo and run it on the worker.
    fn rebase_start(&mut self) {
        let Some(top) = self.git_top() else { return };
        let Some(plan) = self.gitbg.rebase.clone() else { return };
        let st = repo_state(&top);
        if st != RepoState::Clean {
            self.git_report(format!("Cannot rebase: {}", st.busy_text()), false);
            return;
        }
        if let Err(e) = plan.prepare() {
            self.git_report(e, false);
            return self.rebase_show(0);
        }
        self.gitbg.rebase = None;
        self.git_start_job(Op::Rebase(plan));
    }

    /// Card buttons and palette commands of a rebase in progress.
    pub(crate) fn rebase_act(&mut self, a: RebaseAct) {
        match a {
            RebaseAct::Continue => self.git_start_job(Op::RebaseContinue),
            RebaseAct::AbortAsk => self.git_ask(Ask {
                text: "Abort the rebase? The commits go back to how they were before it started; the conflict resolutions made so far are thrown away.".into(),
                warn: true,
                buttons: vec![
                    btn("Abort rebase", GitAct::Rebase(RebaseAct::Abort), Tone::Danger),
                    btn("Keep resolving", GitAct::Dismiss, Tone::Quiet),
                ],
            }),
            RebaseAct::Abort => self.git_start_job(Op::RebaseAbort),
            RebaseAct::OpenMerge => {
                let Some(top) = git::toplevel(&self.root) else { return };
                let first = git_run(&top, &["diff", "--name-only", "--diff-filter=U"])
                    .ok()
                    .and_then(|o| o.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string));
                if let Some(rel) = first {
                    self.merge_open(&top.join(rel));
                }
            }
        }
    }

    /// A rebase job ended (from the panel or detached): forget the generated files, or hand a
    /// stop on conflicts over to the merge editor.
    pub(crate) fn rebase_job_done(&mut self, op: &Op, res: &Result<String, Fail>) {
        if !matches!(op, Op::Rebase(_) | Op::RebaseContinue | Op::RebaseAbort) {
            return;
        }
        let Some(top) = git::toplevel(&self.root) else { return };
        if repo_state(&top) == RepoState::Rebase {
            if res.is_err() && !matches!(op, Op::RebaseAbort) {
                self.gitbg.rebase_merge = true;
                self.request_redraw();
            }
        } else {
            gitrebase::cleanup(&top);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitrebase::Step;

    fn step(a: TodoAction, h: &str, s: &str) -> Step {
        Step { action: a, hash: h.into(), subject: s.into(), body: String::new(), new_subject: None }
    }

    #[test]
    fn rows_and_summary_describe_the_plan() {
        let mut p = RebasePlan {
            upstream: Some("0123456789abcdef".into()),
            steps: vec![step(TodoAction::Pick, "aaaaaaaaaa", "one"), step(TodoAction::Drop, "bbbbbbbbbb", "two")],
            dir: PathBuf::new(),
        };
        assert_eq!(step_label(&p.steps[0]), "pick    aaaaaaa  one");
        assert_eq!(step_label(&p.steps[1]), "drop    bbbbbbb  two");
        assert_eq!(plan_summary(&p), "1 of 2 commits kept, onto 0123456789ab");
        p.steps[0].action = TodoAction::Reword;
        p.steps[0].new_subject = Some("uno".into());
        assert_eq!(step_label(&p.steps[0]), "reword  aaaaaaa  uno");
        p.upstream = None;
        assert!(plan_summary(&p).ends_with("from the first commit"));
    }

    #[test]
    fn the_rebase_card_offers_continue_and_abort() {
        let c = rebase_card();
        assert_eq!(c.buttons[0].act, GitAct::Rebase(RebaseAct::Continue));
        assert_eq!(c.buttons[1].act, GitAct::Rebase(RebaseAct::AbortAsk));
        assert!(!c.warn);
    }

    use crate::{
        app_overlay::Overlay,
        app_unsaved::testkit::{lock, test_app, tmp},
    };

    fn repo(tag: &str) -> PathBuf {
        let d = tmp(tag);
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "Ana"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git_run(&d, a).unwrap();
        }
        std::fs::write(d.join(".gitignore"), "seed.txt\n.config/\n.state/\n").unwrap();
        d
    }

    fn commit(d: &Path, file: &str, text: &str, msg: &str) {
        std::fs::write(d.join(file), text).unwrap();
        git_run(d, &["add", "-A"]).unwrap();
        git_run(d, &["commit", "-q", "-m", msg]).unwrap();
    }

    fn wait_plan(app: &mut App) {
        for _ in 0..250 {
            app.git_ext_tick(Instant::now(), &mut |_| {});
            if app.gitbg.rebase.is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the plan never arrived");
    }

    fn wait_job(app: &mut App) {
        for _ in 0..500 {
            app.gitpanel_tick(Instant::now(), &mut |_| {});
            if app.gitpanel.as_ref().is_some_and(|g| g.job.is_none()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the rebase never finished");
    }

    fn subjects(d: &Path) -> Vec<String> {
        git_run(d, &["log", "--format=%s"]).unwrap().lines().map(str::to_string).collect()
    }

    #[test]
    fn the_plan_is_edited_in_pickers_and_runs_on_the_worker() {
        let _g = lock();
        let d = repo("rebase-app");
        commit(&d, "a", "1", "c1");
        commit(&d, "b", "2", "c2");
        commit(&d, "c", "3", "c3");
        let mut app = test_app(&d);
        app.size = (1200, 800);

        app.rebase_menu();
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.title == "Interactive rebase"));
        app.rebase_pick(RebasePick::Last(2));
        wait_plan(&mut app);
        let subjects_of = |app: &App| -> Vec<String> {
            app.gitbg.rebase.as_ref().unwrap().steps.iter().map(|s| s.subject.clone()).collect()
        };
        assert_eq!(subjects_of(&app), ["c2", "c3"]);
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.title.starts_with("Interactive rebase · oldest")));

        // Edit one row: the action picker lists the choices; drop c2, reword c3.
        app.rebase_pick(RebasePick::Edit(0));
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.title.contains("c2")));
        app.rebase_pick(RebasePick::Set(0, TodoAction::Drop));
        app.rebase_reworded(1, "  c3 renamed ".into());
        let plan = app.gitbg.rebase.clone().unwrap();
        assert_eq!(plan.steps[0].action, TodoAction::Drop);
        assert_eq!(
            (plan.steps[1].action, plan.steps[1].new_subject.as_deref()),
            (TodoAction::Reword, Some("c3 renamed"))
        );
        // The menu offers to continue editing it.
        app.rebase_menu();
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.items[0].label == "Continue editing the plan"));
        // Moving a row keeps the cursor on it.
        app.rebase_pick(RebasePick::Move(1, -1));
        assert_eq!(subjects_of(&app), ["c3", "c2"]);
        app.rebase_pick(RebasePick::Move(0, 1));
        assert_eq!(subjects_of(&app), ["c2", "c3"]);

        app.rebase_pick(RebasePick::Start);
        assert!(app.gitbg.rebase.is_none());
        wait_job(&mut app);
        assert_eq!(subjects(&d), ["c3 renamed", "c1"]);
        assert_eq!(repo_state(&d), RepoState::Clean);
        assert!(!d.join(".git/lumen-rebase").exists(), "the generated files are cleaned up");
        assert!(!d.join("b").exists(), "the dropped commit is gone");
        assert!(app.gitpanel.as_ref().unwrap().card().is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_plan_that_cannot_run_is_refused_and_cancel_forgets_it() {
        let _g = lock();
        let d = repo("rebase-bad");
        commit(&d, "a", "1", "c1");
        commit(&d, "b", "2", "c2");
        let mut app = test_app(&d);
        app.size = (1200, 800);
        app.rebase_pick(RebasePick::Last(5));
        wait_plan(&mut app);
        app.rebase_pick(RebasePick::Set(0, TodoAction::Squash));
        app.rebase_pick(RebasePick::Start);
        assert!(app.gitbg.rebase.is_some(), "the plan stays for the user to fix");
        assert!(matches!(&app.overlay, Overlay::Picker(p) if p.title.starts_with("Interactive rebase")));
        assert!(app.gitpanel.is_none(), "nothing was started");
        app.rebase_pick(RebasePick::Cancel);
        assert!(app.gitbg.rebase.is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_conflict_offers_continue_or_abort_and_opens_the_merge_editor() {
        let _g = lock();
        let d = repo("rebase-conflict");
        commit(&d, "x", "a1\n", "c1");
        commit(&d, "x", "a2\n", "c2");
        commit(&d, "x", "a3\n", "c3");
        let before = git_run(&d, &["rev-parse", "HEAD"]).unwrap();
        let mut app = test_app(&d);
        app.size = (1200, 800);
        app.rebase_pick(RebasePick::Last(2));
        wait_plan(&mut app);
        app.rebase_pick(RebasePick::Move(0, 1));
        app.rebase_pick(RebasePick::Start);
        wait_job(&mut app);

        assert_eq!(repo_state(&d), RepoState::Rebase);
        {
            let gp = app.gitpanel.as_ref().unwrap();
            assert_eq!(gp.state, RepoState::Rebase);
            assert_eq!(gp.card(), Some(rebase_card()), "Continue / Abort");
            assert!(gp.sync_note.as_ref().is_some_and(|(t, ok, _)| !*ok && t.contains("conflicts")));
        }
        // The merge editor opens on the conflicted file at the next tick.
        assert!(app.gitbg.rebase_merge);
        app.git_ext_tick(Instant::now(), &mut |_| {});
        assert!(!app.gitbg.rebase_merge);
        assert!(app.merge.as_ref().is_some_and(|m| m.name == "x"), "the merge editor is on x");

        // Abort (confirmed) puts the branch back.
        app.git_act(GitAct::Rebase(RebaseAct::AbortAsk));
        let ask = app.gitpanel.as_ref().unwrap().ask.clone().expect("the abort question");
        assert_eq!(ask.buttons[0].act, GitAct::Rebase(RebaseAct::Abort));
        app.git_act(ask.buttons[0].act.clone());
        wait_job(&mut app);
        assert_eq!(repo_state(&d), RepoState::Clean);
        assert_eq!(git_run(&d, &["rev-parse", "HEAD"]).unwrap(), before);
        assert!(!d.join(".git/lumen-rebase").exists());
        std::fs::remove_dir_all(&d).ok();
    }
}
