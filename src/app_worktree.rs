//! App glue for git worktrees (the git work is in `worktree.rs`):
//!
//! * "Git: Worktrees…": every worktree with its branch, folder and state (current, main, modified,
//!   locked, missing); a row opens its actions: open here (the window switches project, each
//!   project keeps its own session), open in a new window (`lumen-text --new-window <path>`),
//!   remove, copy the path.
//! * "Git: New worktree…": a new branch or an existing one (local, or remote = new tracking
//!   branch), then its folder (default `<repo>/../<repo>-worktrees/<branch>`, or under
//!   `git_worktree_dir`); `git worktree add` runs on the git worker (`gitsync`) with the usual
//!   progress, toast and error reporting, and offers to open the result.
//! * Remove asks first (a card); a dirty worktree is refused by git, and a second card offers the
//!   forced remove. "Git: Prune worktrees" forgets worktrees whose folder is gone.
//! * The branch menu lists "Worktrees…" and marks branches checked out in another worktree; the
//!   branch chip shows a worktree icon when the project is a linked worktree.
//! * [`App::ai_session_in_worktree`] is the hook the AI panel uses to start an agent session in a
//!   worktree ([`App::worktree_pick_for_ai`] opens the picker for it).

use crate::{
    app::App,
    app_gitpanel::{Ask, AskBtn, GitAct, GitAsk, GitPanel, GitPick, Tone, check_branch_name},
    app_overlay::{Action, Item, Overlay, Prompt, PromptKind, choice_picker},
    app_topbar::tilde,
    field::TextField,
    gitsync::{self, Fail, FailKind, Op},
    worktree::{self, Worktree},
};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

/// Picker rows of the worktree features (`GitPick::Worktree`).
#[derive(Clone, Debug, PartialEq)]
pub enum WtPick {
    /// A worktree row: show its actions.
    Menu(PathBuf),
    /// Switch this window's project to it.
    Open(PathBuf),
    OpenWindow(PathBuf),
    Remove(PathBuf),
    CopyPath(PathBuf),
    /// "New worktree…": choose its branch.
    New,
    /// A new branch (asks its name), created from HEAD.
    NewBranch,
    /// An existing local branch.
    Branch(String),
    /// A remote branch (`origin/x`): a new local branch tracking it.
    Remote(String),
    Prune,
    /// "Stay here" after a worktree was created.
    Stay,
    /// Start an AI agent session in it (see [`App::ai_session_in_worktree`]).
    AiSession(PathBuf),
}

/// Text prompts of the worktree features (`GitAsk::Worktree`).
pub enum WtAsk {
    /// Name of the new branch of a new worktree.
    BranchName,
    /// Folder of the new worktree.
    Path { branch: String, create: bool, base: Option<String> },
}

/// Card buttons of the worktree features (`GitAct::Worktree`).
#[derive(Clone, Debug, PartialEq)]
pub enum WtAct {
    Remove { path: PathBuf, force: bool, locked: bool },
}

fn pick(p: WtPick) -> Action {
    Action::Git(GitPick::Worktree(p))
}

fn item(label: impl Into<String>, detail: impl Into<String>, p: WtPick) -> Item {
    Item { label: label.into(), detail: detail.into(), action: pick(p) }
}

fn cancel() -> AskBtn {
    AskBtn { label: "Cancel".into(), act: GitAct::Dismiss, tone: Tone::Quiet }
}

/// Detail of a worktree row: folder, then what is special about it.
pub fn row_detail(w: &Worktree, current: bool, dirty: bool) -> String {
    let mut d = tilde(&w.path);
    for (on, word) in [
        (current, "current"),
        (w.main, "main"),
        (dirty, "modified"),
        (w.locked, "locked"),
        (w.prunable, "missing (prune)"),
    ] {
        if on {
            d.push_str(" · ");
            d.push_str(word);
        }
    }
    d
}

/// The card asking before a worktree is removed.
pub fn remove_card(w: &Worktree, dirty: bool) -> Ask {
    let (name, path) = (w.name(), tilde(&w.path));
    let mut text = format!("Remove the worktree “{name}” at {path}? Its folder is deleted; the branch is kept.");
    if w.locked {
        text.push_str(" It is locked (git worktree lock): removing it needs a forced remove, which also deletes uncommitted changes in it.");
    } else if dirty {
        text.push_str(" It has uncommitted changes: git refuses to remove it unless you force it next.");
    }
    let act = GitAct::Worktree(WtAct::Remove { path: w.path.clone(), force: w.locked, locked: w.locked });
    let label = if w.locked { "Force remove" } else { "Remove" };
    Ask { text, warn: true, buttons: vec![AskBtn { label: label.into(), act, tone: Tone::Danger }, cancel()] }
}

/// Second card after git refused to remove a dirty worktree.
pub fn force_remove_card(path: &Path, locked: bool) -> Ask {
    Ask {
        text: format!(
            "{} has modified or untracked files. Force removing it deletes them for good (they are in no commit).",
            tilde(path)
        ),
        warn: true,
        buttons: vec![
            AskBtn {
                label: "Force remove".into(),
                act: GitAct::Worktree(WtAct::Remove { path: path.to_path_buf(), force: true, locked }),
                tone: Tone::Danger,
            },
            cancel(),
        ],
    }
}

impl App {
    /// Worktrees of the project's repository (main first), or a message when there is none.
    fn wt_list(&mut self) -> Option<Vec<Worktree>> {
        let l = worktree::list(&self.root);
        if l.is_empty() {
            self.say("This folder is not a git repository");
            return None;
        }
        Some(l)
    }

    /// The main worktree of `list` (where new ones are created next to, and git runs from).
    fn wt_main(list: &[Worktree]) -> Option<PathBuf> {
        list.first().map(|w| w.path.clone())
    }

    /// "Git: Worktrees…".
    pub fn worktrees_open(&mut self) {
        let Some(list) = self.wt_list() else { return };
        let dirty = worktree::dirty_flags(&list);
        let cur = worktree::containing(&list, &self.root);
        let mut items: Vec<Item> = list
            .iter()
            .enumerate()
            .map(|(i, w)| item(w.name(), row_detail(w, cur == Some(i), dirty[i]), WtPick::Menu(w.path.clone())))
            .collect();
        items.push(item(
            "New worktree…",
            "git worktree add: another branch checked out in its own folder",
            WtPick::New,
        ));
        if list.iter().any(|w| w.prunable) {
            items.push(item("Prune worktrees", "forget the worktrees whose folder is gone", WtPick::Prune));
        }
        let repo = list[0].path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let title = format!("Worktrees of {repo} ({})", list.len());
        self.overlay = Overlay::Picker(choice_picker(&title, items, cur.unwrap_or(0)));
        self.request_redraw();
    }

    /// "Git: New worktree…": choose the branch.
    pub fn worktree_new(&mut self) {
        let Some(list) = self.wt_list() else { return };
        let branches = crate::app_topbar::list_branches(&self.root);
        let mut items = vec![item("New branch…", "created from the current HEAD", WtPick::NewBranch)];
        let local: Vec<&str> = branches.iter().filter(|b| !b.remote).map(|b| b.name.as_str()).collect();
        for b in &branches {
            if b.remote {
                let short = b.name.split_once('/').map_or(b.name.as_str(), |x| x.1);
                if !local.contains(&short) {
                    items.push(item(
                        &b.name,
                        format!("remote: new local branch “{short}” tracking it"),
                        WtPick::Remote(b.name.clone()),
                    ));
                }
            } else if worktree::holding(&list, &b.name).is_none() {
                items.push(item(&b.name, "existing branch", WtPick::Branch(b.name.clone())));
            }
        }
        self.overlay = Overlay::Picker(choice_picker("New worktree — choose its branch…", items, 0));
        self.request_redraw();
    }

    /// "Git: Remove worktree…": the worktrees that can go.
    pub fn worktree_remove_pick(&mut self) {
        let Some(list) = self.wt_list() else { return };
        let cur = worktree::containing(&list, &self.root);
        let items: Vec<Item> = list
            .iter()
            .enumerate()
            .filter(|(i, w)| !w.main && Some(*i) != cur)
            .map(|(_, w)| item(w.name(), row_detail(w, false, false), WtPick::Remove(w.path.clone())))
            .collect();
        if items.is_empty() {
            return self.say("No other worktree to remove");
        }
        self.overlay = Overlay::Picker(choice_picker("Remove worktree…", items, 0));
        self.request_redraw();
    }

    /// "Git: Prune worktrees".
    pub fn worktree_prune(&mut self) {
        let Some(list) = self.wt_list() else { return };
        let Some(main) = Self::wt_main(&list) else { return };
        self.wt_start(&main, Op::WorktreePrune);
    }

    /// API for the AI panel: a picker of the worktrees; the chosen one goes to
    /// [`App::ai_session_in_worktree`].
    #[allow(dead_code)] // called by the AI panel's "AI: New session in worktree…" (v2/ai-sessions)
    pub fn worktree_pick_for_ai(&mut self) {
        let Some(list) = self.wt_list() else { return };
        let cur = worktree::containing(&list, &self.root);
        let items: Vec<Item> = list
            .iter()
            .enumerate()
            .filter(|(_, w)| !w.bare && !w.prunable)
            .map(|(i, w)| item(w.name(), row_detail(w, cur == Some(i), false), WtPick::AiSession(w.path.clone())))
            .collect();
        self.overlay = Overlay::Picker(choice_picker("AI: New session in worktree…", items, cur.unwrap_or(0)));
        self.request_redraw();
    }

    /// API for the AI panel: start an agent session whose working directory is the worktree
    /// `path`. Until the AI panel runs several sessions (each with its own cwd), the single
    /// session can only run in the project itself.
    // INTEGRATION POINT (v2/ai-sessions): replace the body with the call that opens a new session
    // with `cwd = path`, and add the palette entry "AI: New session in worktree…" calling
    // `worktree_pick_for_ai`.
    pub fn ai_session_in_worktree(&mut self, path: &Path) {
        if worktree::same_path(path, &self.root) {
            return self.ai_show();
        }
        self.say(format!(
            "AI sessions in another worktree need multiple AI sessions; open {} first (Git: Worktrees…)",
            tilde(path)
        ));
    }

    /// Open `path` in another window (a new `lumen-text` process).
    pub fn open_in_new_window(&mut self, path: &Path) {
        if std::env::var_os("LUMEN_HEADLESS").is_some() {
            return self.say(format!("new window: {}", tilde(path)));
        }
        match crate::instance::spawn_window(path) {
            Ok(()) => self.say(format!("opening {} in a new window", tilde(path))),
            Err(e) => self.git_report(format!("Could not start a new window: {e}"), false),
        }
    }

    /// A row of a worktree picker was chosen.
    pub(crate) fn worktree_pick(&mut self, top: &Path, p: WtPick) {
        match p {
            WtPick::Menu(path) => self.worktree_menu(&path),
            WtPick::Open(path) => {
                if !path.is_dir() {
                    return self
                        .git_report(format!("{} does not exist (Prune worktrees forgets it)", tilde(&path)), false);
                }
                self.switch_project(path);
            }
            WtPick::OpenWindow(path) => self.open_in_new_window(&path),
            WtPick::CopyPath(path) => {
                let s = path.to_string_lossy().into_owned();
                self.clip.set(&s);
                self.say(format!("Copied {s}"));
            }
            WtPick::Remove(path) => self.worktree_ask_remove(&path),
            WtPick::New => self.worktree_new(),
            WtPick::NewBranch => {
                self.wt_prompt(WtAsk::BranchName, "New worktree: name of the new branch (created from HEAD)", "")
            }
            WtPick::Branch(b) => self.wt_ask_path(top, b, false, None),
            WtPick::Remote(r) => {
                let local = r.split_once('/').map_or(r.as_str(), |x| x.1).to_string();
                self.wt_ask_path(top, local, true, Some(r));
            }
            WtPick::Prune => self.worktree_prune(),
            WtPick::Stay => {}
            WtPick::AiSession(path) => self.ai_session_in_worktree(&path),
        }
    }

    /// The actions of one worktree.
    fn worktree_menu(&mut self, path: &Path) {
        let Some(list) = self.wt_list() else { return };
        let Some(i) = list.iter().position(|w| w.path == path) else {
            return self.say("That worktree is gone");
        };
        let w = &list[i];
        let current = worktree::containing(&list, &self.root) == Some(i);
        let p = w.path.clone();
        let mut items = Vec::new();
        if w.prunable {
            items.push(item("Prune worktrees", "its folder is gone: forget it", WtPick::Prune));
        } else if !current && !w.bare {
            items.push(item(
                "Open here",
                "switch this window to it (each project keeps its open files)",
                WtPick::Open(p.clone()),
            ));
            items.push(item("Open in new window", "", WtPick::OpenWindow(p.clone())));
        }
        if !w.main && !current && !w.prunable {
            items.push(item("Remove…", "git worktree remove (asks first)", WtPick::Remove(p.clone())));
        }
        items.push(item("Copy path", tilde(&p), WtPick::CopyPath(p)));
        let title = format!("{} — {}", w.name(), tilde(&w.path));
        self.overlay = Overlay::Picker(choice_picker(&title, items, 0));
        self.request_redraw();
    }

    fn worktree_ask_remove(&mut self, path: &Path) {
        let Some(list) = self.wt_list() else { return };
        let Some(i) = list.iter().position(|w| w.path == path) else {
            return self.say("That worktree is gone");
        };
        let w = &list[i];
        if w.main {
            return self.git_report("The main worktree cannot be removed", false);
        }
        if worktree::containing(&list, &self.root) == Some(i) {
            return self.git_report("This worktree is the open project: switch to another one first", false);
        }
        let dirty = worktree::is_dirty(&w.path);
        let card = remove_card(w, dirty);
        self.git_ask(card);
    }

    fn wt_prompt(&mut self, ask: WtAsk, title: impl Into<String>, text: &str) {
        self.overlay =
            Overlay::Prompt(Prompt::new(PromptKind::Git(GitAsk::Worktree(ask)), title.into(), TextField::with(text)));
        self.request_redraw();
    }

    /// Ask for the folder of the new worktree, prefilled with the default location.
    fn wt_ask_path(&mut self, top: &Path, branch: String, create: bool, base: Option<String>) {
        let main = worktree::list(top).first().map_or_else(|| top.to_path_buf(), |w| w.path.clone());
        let def = worktree::default_path(&main, &branch, &self.config.git_worktree_dir);
        let title = format!("Folder of the worktree “{branch}” (Enter creates it)");
        self.wt_prompt(WtAsk::Path { branch, create, base }, title, &def.to_string_lossy());
    }

    /// Enter in a worktree prompt.
    pub(crate) fn worktree_answer(&mut self, top: &Path, ask: WtAsk, text: String) {
        match ask {
            WtAsk::BranchName => match check_branch_name(top, &text) {
                Err(e) => self.wt_prompt(WtAsk::BranchName, e, text.trim()),
                Ok(name) => self.wt_ask_path(top, name, true, None),
            },
            WtAsk::Path { branch, create, base } => {
                let main = worktree::list(top).first().map_or_else(|| top.to_path_buf(), |w| w.path.clone());
                if text.trim().is_empty() {
                    return self.wt_prompt(WtAsk::Path { branch, create, base }, "Type the folder of the worktree", "");
                }
                let path = worktree::resolve_typed(&main, &text);
                let taken = path.is_file() || std::fs::read_dir(&path).is_ok_and(|mut d| d.next().is_some());
                if taken {
                    let title = format!("{} already exists: choose another folder", tilde(&path));
                    return self.wt_prompt(WtAsk::Path { branch, create, base }, title, text.trim());
                }
                self.wt_start(&main, Op::WorktreeAdd { path, branch, create, base });
            }
        }
    }

    /// A card button.
    pub(crate) fn worktree_act(&mut self, top: &Path, a: WtAct) {
        match a {
            WtAct::Remove { path, force, locked } => {
                let main = worktree::list(top).first().map_or_else(|| top.to_path_buf(), |w| w.path.clone());
                self.wt_start(&main, Op::WorktreeRemove { path, force, locked });
            }
        }
    }

    /// Run a worktree command on the git worker: on the panel's when it is open (its strip shows
    /// the progress), else in the background (bottom bar spinner, then a toast).
    fn wt_start(&mut self, dir: &Path, op: Op) {
        let wake = self.wake();
        let busy = match self.gitpanel.as_mut() {
            Some(gp) if gp.job.is_none() => {
                gp.sync_note = None;
                gp.job = Some(gitsync::spawn(dir, op, wake));
                gp.spun = Instant::now();
                false
            }
            Some(_) => true,
            None if self.gitbg.detached.is_none() => {
                self.gitbg.detached = Some(gitsync::spawn(dir, op, wake));
                false
            }
            None => true,
        };
        if busy {
            return self.git_report("Wait: a git operation is already in progress", false);
        }
        self.request_redraw();
    }

    /// A git command finished (from the panel's worker when `gp` is given, else the background
    /// one): offer to open a new worktree, offer the forced remove after a refusal.
    pub(crate) fn worktree_job_done(&mut self, op: &Op, res: &Result<String, Fail>, gp: Option<&mut GitPanel>) {
        match (op, res) {
            (Op::WorktreeAdd { path, branch, .. }, Ok(_)) => {
                if self.overlay.is_none() {
                    let items = vec![
                        item("Open here", "switch this window to it", WtPick::Open(path.clone())),
                        item("Open in new window", "", WtPick::OpenWindow(path.clone())),
                        item("Stay here", "", WtPick::Stay),
                    ];
                    let title = format!("Worktree “{branch}” is ready at {}", tilde(path));
                    self.overlay = Overlay::Picker(choice_picker(&title, items, 0));
                }
            }
            (Op::WorktreeRemove { path, force: false, locked }, Err(f)) if f.kind == FailKind::Dirty => {
                let card = force_remove_card(path, *locked);
                match gp {
                    Some(gp) => {
                        gp.sync_note = None;
                        gp.ask = Some(card);
                    }
                    None => self.git_ask(card),
                }
            }
            _ => return,
        }
        self.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app};
    use crate::worktree::test_support::{repo, run};

    #[test]
    fn row_details_and_cards() {
        let w =
            Worktree { path: "/nowhere/x".into(), branch: Some("feat".into()), locked: true, ..Worktree::default() };
        assert_eq!(row_detail(&w, true, true), "/nowhere/x · current · modified · locked");
        let c = remove_card(&w, false);
        assert!(c.warn && c.text.contains("locked"));
        assert_eq!(
            c.buttons[0].act,
            GitAct::Worktree(WtAct::Remove { path: "/nowhere/x".into(), force: true, locked: true })
        );
        let w = Worktree { locked: false, ..w };
        let c = remove_card(&w, true);
        assert!(c.text.contains("uncommitted"), "{}", c.text);
        assert_eq!(c.buttons[0].label, "Remove");
        assert_eq!(
            c.buttons[0].act,
            GitAct::Worktree(WtAct::Remove { path: "/nowhere/x".into(), force: false, locked: false })
        );
        let f = force_remove_card(Path::new("/nowhere/x"), false);
        assert_eq!(f.buttons[0].label, "Force remove");
        assert_eq!(
            f.buttons[0].act,
            GitAct::Worktree(WtAct::Remove { path: "/nowhere/x".into(), force: true, locked: false })
        );
    }

    /// Drive the background job to its end (with a deadline), as the event loop would.
    fn finish_job(app: &mut App) {
        let t0 = Instant::now();
        while app.gitbg.detached.is_some() {
            assert!(t0.elapsed().as_secs() < 20, "git worktree job did not finish");
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.git_bg_tick(Instant::now(), &mut |_| {});
        }
    }

    fn picker_labels(app: &App) -> Vec<String> {
        match &app.overlay {
            Overlay::Picker(p) => p.items.iter().map(|i| i.label.clone()).collect(),
            _ => Vec::new(),
        }
    }

    fn choose(app: &mut App, label: &str) {
        let Overlay::Picker(p) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("no picker") };
        let it = p.items.iter().find(|i| i.label == label).unwrap_or_else(|| panic!("no row {label}"));
        let Action::Git(g) = it.action.clone() else { panic!("not a git row") };
        app.git_pick(g);
    }

    #[test]
    fn create_open_and_remove_a_worktree_from_the_app() {
        let _g = lock();
        let r = repo("app");
        let mut app = test_app(&r);
        // Session and recent projects of the switch below stay in the scratch folder.
        unsafe { std::env::set_var("XDG_STATE_HOME", r.parent().unwrap().join("state")) };
        app.refresh_git_status();
        assert!(app.gitbg.worktree_main.is_none(), "the main worktree is not linked");

        // New worktree on a new branch: name, then the prefilled default folder.
        app.worktree_new();
        assert_eq!(picker_labels(&app).first().map(String::as_str), Some("New branch…"));
        choose(&mut app, "New branch…");
        let Overlay::Prompt(p) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("no prompt") };
        let PromptKind::Git(ask) = p.kind else { panic!("not a git prompt") };
        app.git_prompt_answer(ask, "feat/wt".into());
        let Overlay::Prompt(p) = std::mem::replace(&mut app.overlay, Overlay::None) else { panic!("no path prompt") };
        let expected = r.parent().unwrap().join("repo-worktrees").join("feat-wt");
        assert_eq!(p.field.text, expected.to_string_lossy());
        let PromptKind::Git(ask) = p.kind else { panic!("not a git prompt") };
        app.git_prompt_answer(ask, p.field.text.clone());
        assert!(app.gitbg.detached.is_some(), "runs on the git worker");
        finish_job(&mut app);
        assert!(expected.join("a.txt").is_file(), "worktree checked out");
        assert_eq!(picker_labels(&app), ["Open here", "Open in new window", "Stay here"]);

        // The list shows both, the current one first selected.
        app.overlay = Overlay::None;
        app.worktrees_open();
        let labels = picker_labels(&app);
        assert_eq!(&labels[..3], ["main", "feat/wt", "New worktree…"], "{labels:?}");

        // Open it here: the project switches and the branch chip knows it is a linked worktree.
        choose(&mut app, "feat/wt");
        assert_eq!(picker_labels(&app)[..2], ["Open here", "Open in new window"]);
        choose(&mut app, "Open here");
        assert!(worktree::same_path(&app.root, &expected), "{:?}", app.root);
        assert_eq!(app.branch.as_deref(), Some("feat/wt"));
        assert!(app.gitbg.worktree_main.as_deref().is_some_and(|m| worktree::same_path(m, &r)));

        // It cannot remove itself; back in the main worktree, a dirty one is refused, then forced.
        app.worktree_ask_remove(&worktree::list(&r)[1].path);
        assert!(app.gitpanel.as_ref().is_none_or(|g| g.ask.is_none()), "no card for the open project");
        app.switch_project(r.clone());
        assert!(worktree::same_path(&app.root, &r));
        std::fs::write(expected.join("scratch.txt"), "x").unwrap();
        let wt_path = worktree::list(&r)[1].path.clone();
        app.worktree_ask_remove(&wt_path);
        let card = app.gitpanel.as_ref().and_then(|g| g.ask.clone()).expect("remove card");
        assert!(card.text.contains("uncommitted"), "{}", card.text);
        app.git_act(card.buttons[0].act.clone());
        // The panel is open (the card lives there): the job runs on its worker.
        let t0 = Instant::now();
        while app.gitpanel.as_ref().is_some_and(|g| g.job.is_some()) {
            assert!(t0.elapsed().as_secs() < 20, "remove did not finish");
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.gitpanel_tick(Instant::now(), &mut |_| {});
        }
        let card = app.gitpanel.as_ref().and_then(|g| g.ask.clone()).expect("force card after the refusal");
        assert_eq!(card.buttons[0].label, "Force remove");
        assert!(expected.exists(), "nothing removed without the force");
        app.git_act(card.buttons[0].act.clone());
        let t0 = Instant::now();
        while app.gitpanel.as_ref().is_some_and(|g| g.job.is_some()) {
            assert!(t0.elapsed().as_secs() < 20, "forced remove did not finish");
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.gitpanel_tick(Instant::now(), &mut |_| {});
        }
        assert!(!expected.exists(), "forced remove deletes the folder");
        assert_eq!(worktree::list(&r).len(), 1);
        // The branch is kept.
        assert!(run(&r, &["rev-parse", "--verify", "refs/heads/feat/wt"]).status.success());
        drop(app);
        let _ = std::fs::remove_dir_all(r.parent().unwrap());
    }

    #[test]
    fn branches_in_use_are_not_offered_and_taken_folders_are_refused() {
        let _g = lock();
        let r = repo("pick");
        run(&r, &["branch", "dev"]);
        let mut app = test_app(&r);
        app.worktree_new();
        let labels = picker_labels(&app);
        assert!(labels.contains(&"dev".to_string()), "{labels:?}");
        assert!(!labels.contains(&"main".to_string()), "main is checked out here: {labels:?}");
        choose(&mut app, "dev");
        let Overlay::Prompt(p) = &app.overlay else { panic!("no path prompt") };
        assert!(p.field.text.ends_with("repo-worktrees/dev"), "{}", p.field.text);
        // An existing folder is refused.
        let taken = r.parent().unwrap().join("taken");
        std::fs::create_dir_all(taken.join("x")).unwrap();
        let Overlay::Prompt(p) = std::mem::replace(&mut app.overlay, Overlay::None) else { unreachable!() };
        let PromptKind::Git(ask) = p.kind else { panic!() };
        app.git_prompt_answer(ask, taken.to_string_lossy().into_owned());
        let Overlay::Prompt(p) = &app.overlay else { panic!("asked again") };
        assert!(p.title.contains("already exists"), "{}", p.title);
        assert!(app.gitbg.detached.is_none());
        drop(app);
        let _ = std::fs::remove_dir_all(r.parent().unwrap());
    }
}
