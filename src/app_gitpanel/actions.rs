//! App glue for branches, tags, cherry-pick, "undo last commit" and releases: palette commands,
//! the text prompts (`PromptKind::Git`), the pickers (`Action::Git`), the commit context menu and
//! the confirmation cards shown in the panel's strip. The git work itself is in `refs.rs`.

use super::*;
use crate::app_overlay::{Action, Item, Overlay, Prompt, PromptKind, choice_picker};
use gitsync::ReleasePlan;

/// Tone of a card button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Danger,
    Quiet,
}

/// One button of a confirmation card.
#[derive(Clone, Debug, PartialEq)]
pub struct AskBtn {
    pub label: String,
    pub act: GitAct,
    pub tone: Tone,
}

/// A confirmation card in the panel's strip (delete branch / tag, undo a published commit,
/// publish a release, cherry-pick in progress…).
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub text: String,
    /// Destructive or outward-facing: drawn with the red edge.
    pub warn: bool,
    pub buttons: Vec<AskBtn>,
}

fn btn(label: impl Into<String>, act: GitAct, tone: Tone) -> AskBtn {
    AskBtn { label: label.into(), act, tone }
}

fn cancel() -> AskBtn {
    btn("Cancel", GitAct::Dismiss, Tone::Quiet)
}

/// What a card button does.
#[derive(Clone, Debug, PartialEq)]
pub enum GitAct {
    Dismiss,
    /// `git branch -d` (`-D` when `force`), then `push <remote> --delete <branch>` when `remote`
    /// (remote, branch name there) is set.
    DeleteBranch {
        name: String,
        force: bool,
        remote: Option<(String, String)>,
    },
    /// `git tag -d`, then the remote tag too when `remote` is set.
    DeleteTag {
        name: String,
        remote: Option<String>,
    },
    /// Push one tag (None = all tags) to the default remote.
    PushTags(Option<String>),
    /// Undo the last commit (already confirmed).
    UndoCommit,
    CherryContinue,
    /// Ask before aborting the cherry-pick in progress.
    CherryAbortAsk,
    CherryAbort,
    /// Create the tag first when `new_tag_at` names a commit, then push it and run `gh release create`.
    Release {
        plan: ReleasePlan,
        new_tag_at: Option<String>,
    },
}

/// Text prompts of the git features (`PromptKind::Git`).
pub enum GitAsk {
    /// Name of a new branch, created at HEAD or at the commit `from`.
    NewBranch {
        from: Option<String>,
    },
    /// Name of a new tag, on HEAD or on the commit `at`.
    NewTag {
        at: Option<String>,
    },
    /// Optional message of the tag (annotated when given).
    TagMessage {
        name: String,
        at: Option<String>,
    },
    /// Name of the new tag a release is made for.
    ReleaseNewTag,
    ReleaseTitle {
        tag: String,
        new_tag_at: Option<String>,
    },
    ReleaseNotes {
        tag: String,
        title: String,
        new_tag_at: Option<String>,
    },
}

/// Picker choices of the git features (`Action::Git`).
#[derive(Clone, Debug, PartialEq)]
pub enum GitPick {
    DeleteBranch(String),
    DeleteTag(String),
    /// Push one tag, or all of them (None).
    PushTag(Option<String>),
    /// Release for an existing tag, or for a new one (None).
    ReleaseTag(Option<String>),
    CherryPick(String),
}

/// A commit row of the History tab, as the context menu sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitRef {
    pub hash: String,
    /// It is the checked-out commit (only it can be undone).
    pub head: bool,
}

/// The card shown while a cherry-pick waits for its conflicts to be resolved.
pub fn cherry_card() -> Ask {
    Ask {
        text: "Cherry-pick in progress: resolve the conflicting files and stage them, then Continue. Abort puts the branch back where it was.".into(),
        warn: false,
        buttons: vec![
            btn("Continue", GitAct::CherryContinue, Tone::Accent),
            btn("Abort…", GitAct::CherryAbortAsk, Tone::Danger),
        ],
    }
}

/// Card asking before a branch is deleted; `remote` is its upstream (remote, name there).
pub fn delete_branch_card(name: &str, remote: Option<(String, String)>) -> Ask {
    let mut buttons =
        vec![btn("Delete", GitAct::DeleteBranch { name: name.into(), force: false, remote: None }, Tone::Danger)];
    if let Some((r, b)) = remote {
        buttons.push(btn(
            format!("Delete + {r}/{b}"),
            GitAct::DeleteBranch { name: name.into(), force: false, remote: Some((r, b)) },
            Tone::Danger,
        ));
    }
    buttons.push(cancel());
    Ask {
        text: format!(
            "Delete the local branch “{name}”? git branch -d refuses when it has commits that are not merged. The remote branch is only deleted with the second button."
        ),
        warn: true,
        buttons,
    }
}

/// Second card after `git branch -d` refused: force (`-D`) or keep it.
pub fn unmerged_card(name: &str, remote: Option<(String, String)>) -> Ask {
    Ask {
        text: format!(
            "“{name}” has commits that are not merged into the current branch or its upstream. Force deleting it (git branch -D) drops them; only the reflog could bring them back."
        ),
        warn: true,
        buttons: vec![
            btn("Force delete", GitAct::DeleteBranch { name: name.into(), force: true, remote }, Tone::Danger),
            cancel(),
        ],
    }
}

/// Card asking before a tag is deleted (here only, or here and on `remote`).
pub fn delete_tag_card(name: &str, remote: Option<&str>) -> Ask {
    let mut buttons = vec![btn("Delete", GitAct::DeleteTag { name: name.into(), remote: None }, Tone::Danger)];
    if let Some(r) = remote {
        buttons.push(btn(
            format!("Delete + on {r}"),
            GitAct::DeleteTag { name: name.into(), remote: Some(r.to_string()) },
            Tone::Danger,
        ));
    }
    buttons.push(cancel());
    Ask {
        text: format!("Delete the tag “{name}”? The copy on the remote is only deleted with the second button."),
        warn: true,
        buttons,
    }
}

/// Warning before undoing a commit that is already on the upstream.
pub fn undo_published_card(upstream: &str) -> Ask {
    Ask {
        text: format!(
            "The last commit is already on {upstream}. Undoing it keeps its changes staged, but publishing afterwards needs a force push, which rewrites the remote branch for everyone."
        ),
        warn: true,
        buttons: vec![btn("Undo commit", GitAct::UndoCommit, Tone::Danger), cancel()],
    }
}

/// The release confirmation: everything that will be published.
pub fn release_card(plan: ReleasePlan, new_tag_at: Option<String>) -> Ask {
    Ask {
        text: release_summary(&plan, new_tag_at.as_deref()),
        warn: true,
        buttons: vec![btn("Publish release", GitAct::Release { plan, new_tag_at }, Tone::Accent), cancel()],
    }
}

impl GitPanel {
    /// The card to show: an explicit question, else the cherry-pick-in-progress card.
    pub fn card(&self) -> Option<Ask> {
        self.ask.clone().or_else(|| (self.state == RepoState::CherryPick && self.job.is_none()).then(cherry_card))
    }
}

impl App {
    /// Repository of the project, or a message saying there is none.
    fn git_top(&mut self) -> Option<PathBuf> {
        let top = git::toplevel(&self.root);
        if top.is_none() {
            self.say("This folder is not a git repository");
        }
        top
    }

    /// Report a result: in the panel's strip when it is open, else in a toast (always in the status bar).
    pub(crate) fn git_report(&mut self, text: impl Into<String>, ok: bool) {
        let text = text.into();
        self.say(&text);
        match self.gitpanel.as_mut() {
            Some(gp) => gp.sync_note = Some((text, ok, Instant::now())),
            None => self.toast(text, ok),
        }
        self.request_redraw();
    }

    /// HEAD, refs or files moved: refresh everything that shows them.
    fn git_refs_changed(&mut self, files_too: bool) {
        self.refresh_git_status();
        self.sync_docs_with_head();
        if files_too {
            self.refresh_tree();
            // Open documents are compared with the disk at the next tick.
            self.last_disk_check = Instant::now() - Duration::from_secs(60);
        }
        if let Some(gp) = self.gitpanel.as_mut() {
            gp.refresh();
            gp.reset_history();
        }
        self.request_redraw();
    }

    /// Show a card in the panel (opening the panel when it is closed).
    pub(crate) fn git_ask(&mut self, ask: Ask) {
        if self.gitpanel.is_none() {
            self.open_git_panel();
        }
        if let Some(gp) = self.gitpanel.as_mut() {
            gp.publish = None;
            gp.force = None;
            gp.strategy_ask = false;
            gp.sync_note = None;
            gp.ask = Some(ask);
            gp.focused = true;
        }
        self.request_redraw();
    }

    /// Run a remote command on the panel's worker (opening the panel to show its progress).
    fn git_start_job(&mut self, op: Op) {
        if self.gitpanel.is_none() {
            self.open_git_panel();
        }
        if let Some(mut gp) = self.gitpanel.take() {
            gp.ask = None;
            self.gp_start(&mut gp, op);
            self.gitpanel = Some(gp);
        }
        self.request_redraw();
    }

    fn git_prompt(&mut self, ask: GitAsk, title: impl Into<String>, text: &str) {
        self.overlay = Overlay::Prompt(Prompt::new(PromptKind::Git(ask), title.into(), TextField::with(text)));
        self.request_redraw();
    }

    fn git_default_remote(&self, top: &Path) -> Option<String> {
        let branch = refs_current(top).unwrap_or_default();
        gitsync::default_remote(top, &branch)
    }

    // ───────────── palette commands ─────────────

    /// Prompt for a new branch name (`from` = a commit of the History tab, else HEAD).
    pub fn git_new_branch(&mut self, from: Option<String>, text: &str) {
        if self.git_top().is_none() {
            return;
        }
        let title = match &from {
            Some(c) => format!("New branch from {} (created and checked out)", &c[..c.len().min(7)]),
            None => "New branch from the current HEAD (created and checked out)".to_string(),
        };
        self.git_prompt(GitAsk::NewBranch { from }, title, text);
    }

    pub fn git_delete_branch_pick(&mut self) {
        let Some(top) = self.git_top() else { return };
        let cur = refs_current(&top);
        let items: Vec<Item> = local_branches(&top)
            .into_iter()
            .filter(|b| Some(b) != cur.as_ref())
            .map(|b| Item {
                detail: upstream_of(&top, &b).map(|(r, n)| format!("tracks {r}/{n}")).unwrap_or_default(),
                label: b.clone(),
                action: Action::Git(GitPick::DeleteBranch(b)),
            })
            .collect();
        if items.is_empty() {
            return self.say("No other local branch to delete");
        }
        self.overlay = Overlay::Picker(choice_picker("Delete branch…", items, 0));
    }

    /// The card asking before `name` is deleted.
    pub fn git_ask_delete_branch(&mut self, name: &str) {
        let Some(top) = self.git_top() else { return };
        if refs_current(&top).as_deref() == Some(name) {
            return self.git_report(format!("“{name}” is checked out: switch to another branch first"), false);
        }
        let remote = upstream_of(&top, name);
        self.git_ask(delete_branch_card(name, remote));
    }

    pub fn git_new_tag(&mut self, at: Option<String>) {
        if self.git_top().is_none() {
            return;
        }
        let title = match &at {
            Some(c) => format!("New tag on {}", &c[..c.len().min(7)]),
            None => "New tag on the current HEAD".to_string(),
        };
        self.git_prompt(GitAsk::NewTag { at }, title, "");
    }

    fn tag_items(top: &Path, pick: impl Fn(String) -> GitPick) -> Vec<Item> {
        list_tags(top)
            .into_iter()
            .map(|(name, subject)| Item { label: name.clone(), detail: subject, action: Action::Git(pick(name)) })
            .collect()
    }

    pub fn git_delete_tag_pick(&mut self) {
        let Some(top) = self.git_top() else { return };
        let items = Self::tag_items(&top, GitPick::DeleteTag);
        if items.is_empty() {
            return self.say("The repository has no tags");
        }
        self.overlay = Overlay::Picker(choice_picker("Delete tag…", items, 0));
    }

    pub fn git_push_tags_pick(&mut self) {
        let Some(top) = self.git_top() else { return };
        let Some(remote) = self.git_default_remote(&top) else {
            return self.git_report("No remote configured (git remote add origin <url>)", false);
        };
        let mut items = vec![Item {
            label: "All tags".into(),
            detail: format!("git push {remote} --tags"),
            action: Action::Git(GitPick::PushTag(None)),
        }];
        items.extend(Self::tag_items(&top, |t| GitPick::PushTag(Some(t))));
        if items.len() == 1 {
            return self.say("The repository has no tags");
        }
        self.overlay = Overlay::Picker(choice_picker(&format!("Push tags to {remote}…"), items, 0));
    }

    /// Check that `gh` can publish to a GitHub remote; the (remote, owner/repo) to use.
    fn git_release_target(&mut self, top: &Path) -> Option<(String, String)> {
        let prefer = self.git_default_remote(top);
        let Some(target) = github_remotes(top, prefer.as_deref()).into_iter().next() else {
            self.git_report("Releases need a GitHub remote: none of the remotes points at github.com", false);
            return None;
        };
        match gh_status("gh") {
            GhStatus::Ready => Some(target),
            st => {
                self.git_report(st.text(), false);
                None
            }
        }
    }

    pub fn git_release_start(&mut self) {
        let Some(top) = self.git_top() else { return };
        let Some((remote, slug)) = self.git_release_target(&top) else { return };
        let mut items = vec![Item {
            label: "New tag…".into(),
            detail: "create a tag on HEAD for this release".into(),
            action: Action::Git(GitPick::ReleaseTag(None)),
        }];
        items.extend(Self::tag_items(&top, |t| GitPick::ReleaseTag(Some(t))));
        let title = format!("Release on github.com/{slug} (tag pushed to {remote}) — choose the tag…");
        self.overlay = Overlay::Picker(choice_picker(&title, items, 0));
    }

    pub fn git_cherry_pick_pick(&mut self) {
        let Some(top) = self.git_top() else { return };
        let now = now_secs();
        let items: Vec<Item> = other_commits(&top, 300)
            .into_iter()
            .map(|e| Item {
                label: if e.subject.is_empty() { "(no subject)".into() } else { e.subject.clone() },
                detail: format!("{}  {}, {}", e.short, e.author, git::ago((now - e.ts).max(0))),
                action: Action::Git(GitPick::CherryPick(e.hash)),
            })
            .collect();
        if items.is_empty() {
            return self.say("Every commit of the other branches is already in the current branch");
        }
        self.overlay = Overlay::Picker(choice_picker("Cherry-pick a commit from another branch…", items, 0));
    }

    pub fn git_cherry_continue(&mut self) {
        self.git_act(GitAct::CherryContinue);
    }

    pub fn git_cherry_abort(&mut self) {
        let Some(top) = self.git_top() else { return };
        if repo_state(&top) != RepoState::CherryPick {
            return self.say("No cherry-pick in progress");
        }
        self.git_act(GitAct::CherryAbortAsk);
    }

    /// Undo the last commit: refuses on the first commit and in the middle of a merge / rebase /
    /// cherry-pick; asks first when the commit is already on the upstream.
    pub fn git_undo_commit(&mut self) {
        let Some(top) = self.git_top() else { return };
        if let Some(why) = undo_blocker(&top) {
            return self.git_report(why, false);
        }
        if head_published(&top) {
            let up = gitsync::info(&top).upstream.unwrap_or_else(|| "the upstream".into());
            return self.git_ask(undo_published_card(&up));
        }
        self.git_act(GitAct::UndoCommit);
    }

    // ───────────── prompts, pickers, context menu ─────────────

    /// Enter in a `PromptKind::Git` prompt.
    pub fn git_prompt_answer(&mut self, ask: GitAsk, text: String) {
        let Some(top) = self.git_top() else { return };
        match ask {
            GitAsk::NewBranch { from } => {
                if let Err(e) = check_branch_name(&top, &text) {
                    return self.git_prompt(GitAsk::NewBranch { from }, e, text.trim());
                }
                match create_branch(&top, &text, from.as_deref()) {
                    Ok(m) => {
                        self.git_refs_changed(from.is_some());
                        self.git_report(m, true);
                    }
                    Err(e) => self.git_report(format!("git switch -c: {e}"), false),
                }
            }
            GitAsk::NewTag { at } => match check_tag_name(&top, &text) {
                Err(e) => self.git_prompt(GitAsk::NewTag { at }, e, text.trim()),
                Ok(name) => {
                    let title = format!("Message for “{name}” (empty = lightweight tag, Enter to create)");
                    self.git_prompt(GitAsk::TagMessage { name, at }, title, "");
                }
            },
            GitAsk::TagMessage { name, at } => match create_tag(&top, &name, Some(text.as_str()), at.as_deref()) {
                Ok(m) => {
                    self.git_refs_changed(false);
                    // With the panel open, offer to publish it right away.
                    let remote = self.git_default_remote(&top);
                    match (self.gitpanel.is_some(), remote) {
                        (true, Some(r)) => self.git_ask(Ask {
                            text: format!("{m}. Push it to {r}?"),
                            warn: false,
                            buttons: vec![
                                btn("Push tag", GitAct::PushTags(Some(name)), Tone::Accent),
                                btn("Not now", GitAct::Dismiss, Tone::Quiet),
                            ],
                        }),
                        _ => self.git_report(m, true),
                    }
                }
                Err(e) => self.git_report(format!("git tag: {e}"), false),
            },
            GitAsk::ReleaseNewTag => match check_tag_name(&top, &text) {
                Err(e) => self.git_prompt(GitAsk::ReleaseNewTag, e, text.trim()),
                Ok(tag) => {
                    let head = git_run(&top, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string()).ok();
                    let Some(head) = head else { return self.git_report("The repository has no commits yet", false) };
                    let title = format!("Release title (Enter keeps “{tag}”)");
                    let t = tag.clone();
                    self.git_prompt(GitAsk::ReleaseTitle { tag, new_tag_at: Some(head) }, title, &t);
                }
            },
            GitAsk::ReleaseTitle { tag, new_tag_at } => {
                let title = if text.trim().is_empty() { tag.clone() } else { text.trim().to_string() };
                self.git_prompt(
                    GitAsk::ReleaseNotes { tag, title, new_tag_at },
                    "Release notes (one line; leave empty to let GitHub generate them)",
                    "",
                );
            }
            GitAsk::ReleaseNotes { tag, title, new_tag_at } => {
                let Some((remote, slug)) = self.git_release_target(&top) else { return };
                let notes = Some(text.trim().to_string()).filter(|n| !n.is_empty());
                let plan = ReleasePlan { remote, slug, tag, title, notes };
                self.git_ask(release_card(plan, new_tag_at));
            }
        }
    }

    /// A picker row of the git features was chosen.
    pub fn git_pick(&mut self, p: GitPick) {
        let Some(top) = self.git_top() else { return };
        match p {
            GitPick::DeleteBranch(name) => self.git_ask_delete_branch(&name),
            GitPick::DeleteTag(name) => {
                let remote = self.git_default_remote(&top);
                self.git_ask(delete_tag_card(&name, remote.as_deref()));
            }
            GitPick::PushTag(t) => self.git_act(GitAct::PushTags(t)),
            GitPick::ReleaseTag(None) => {
                let names: Vec<String> = list_tags(&top).into_iter().map(|(n, _)| n).collect();
                let s = suggest_next_tag(&names);
                self.git_prompt(GitAsk::ReleaseNewTag, "Tag for the release (created on HEAD)", &s);
            }
            GitPick::ReleaseTag(Some(tag)) => {
                let title = format!("Release title (Enter keeps “{tag}”)");
                let t = tag.clone();
                self.git_prompt(GitAsk::ReleaseTitle { tag, new_tag_at: None }, title, &t);
            }
            GitPick::CherryPick(hash) => self.git_cherry_pick(&hash),
        }
    }

    pub fn git_cherry_pick(&mut self, hash: &str) {
        let Some(top) = self.git_top() else { return };
        let short = &hash[..hash.len().min(7)];
        match cherry_pick(&top, hash) {
            Ok(line) => {
                self.git_refs_changed(true);
                self.git_report(format!("Cherry-picked {short}: {line}"), true);
            }
            Err(CherryFail::Conflict) => {
                self.git_refs_changed(true);
                if self.gitpanel.is_none() {
                    self.open_git_panel();
                }
                self.git_report(
                    format!("Cherry-pick of {short} stopped on conflicts: resolve the marked files, stage them and Continue (or Abort)"),
                    false,
                );
            }
            Err(CherryFail::Other(e)) => self.git_report(format!("Cherry-pick: {e}"), false),
        }
    }

    /// Right click on a History row.
    pub(crate) fn git_open_commit_menu(&mut self, c: CommitRef, at: (i32, i32)) {
        self.open_ctxmenu(crate::app_ctxmenu::Target::Commit(c), at);
    }

    /// An item of the commit context menu.
    pub(crate) fn git_commit_menu_act(&mut self, act: crate::app_ctxmenu::Act, c: CommitRef) {
        use crate::app_ctxmenu::Act;
        match act {
            Act::CherryPick => self.git_cherry_pick(&c.hash),
            Act::CopyHash => {
                self.clip.set(&c.hash);
                self.say(format!("Copied {}", c.hash));
            }
            Act::BranchHere => self.git_new_branch(Some(c.hash), ""),
            Act::TagHere => self.git_new_tag(Some(c.hash)),
            Act::UndoCommit => self.git_undo_commit(),
            _ => {}
        }
    }

    // ───────────── card buttons ─────────────

    /// Run what a card button (or a command) asked for.
    pub fn git_act(&mut self, act: GitAct) {
        let Some(top) = self.git_top() else { return };
        if let Some(gp) = self.gitpanel.as_mut() {
            gp.ask = None;
        }
        match act {
            GitAct::Dismiss => {}
            GitAct::DeleteBranch { name, force, remote } => match delete_branch(&top, &name, force) {
                Ok(m) => {
                    self.git_refs_changed(false);
                    match remote {
                        Some((r, b)) => self.git_start_job(Op::DeleteRemote { remote: r, refname: format!("refs/heads/{b}") }),
                        None => self.git_report(m, true),
                    }
                }
                Err(DeleteFail::Unmerged) => self.git_ask(unmerged_card(&name, remote)),
                Err(DeleteFail::Other(e)) => self.git_report(format!("git branch -d: {e}"), false),
            },
            GitAct::DeleteTag { name, remote } => match delete_tag(&top, &name) {
                Ok(m) => {
                    self.git_refs_changed(false);
                    match remote {
                        Some(r) => self.git_start_job(Op::DeleteRemote { remote: r, refname: format!("refs/tags/{name}") }),
                        None => self.git_report(m, true),
                    }
                }
                Err(e) => self.git_report(format!("git tag -d: {e}"), false),
            },
            GitAct::PushTags(tag) => match self.git_default_remote(&top) {
                Some(remote) => self.git_start_job(Op::PushTags { remote, tag }),
                None => self.git_report("No remote configured (git remote add origin <url>)", false),
            },
            GitAct::UndoCommit => match undo_last_commit(&top) {
                Ok(msg) => {
                    if self.gitpanel.is_none() {
                        self.open_git_panel();
                    }
                    if let Some(gp) = self.gitpanel.as_mut() {
                        gp.tab = Tab::Changes;
                        gp.amend = false;
                        gp.amend_draft.clear();
                        gp.msg = TextField::with(&msg);
                        gp.msg_top = 0;
                        gp.msg_focus = true;
                        gp.focused = true;
                    }
                    self.git_refs_changed(false);
                    self.git_report("Last commit undone: its changes are staged and its message is back in the box", true);
                }
                Err(e) => self.git_report(e, false),
            },
            GitAct::CherryContinue => match cherry_continue(&top) {
                Ok(m) => {
                    self.git_refs_changed(true);
                    self.git_report(m, true);
                }
                Err(e) => {
                    self.git_refs_changed(false);
                    self.git_report(format!("Cherry-pick --continue: {e}"), false);
                }
            },
            GitAct::CherryAbortAsk => self.git_ask(Ask {
                text: "Abort the cherry-pick? The conflict resolutions made so far are thrown away and the branch goes back to where it was.".into(),
                warn: true,
                buttons: vec![
                    btn("Abort cherry-pick", GitAct::CherryAbort, Tone::Danger),
                    btn("Keep resolving", GitAct::Dismiss, Tone::Quiet),
                ],
            }),
            GitAct::CherryAbort => match cherry_abort(&top) {
                Ok(m) => {
                    self.git_refs_changed(true);
                    self.git_report(m, true);
                }
                Err(e) => self.git_report(format!("Cherry-pick --abort: {e}"), false),
            },
            GitAct::Release { plan, new_tag_at } => {
                if let Some(at) = new_tag_at.as_deref() {
                    if let Err(e) = create_tag(&top, &plan.tag, Some(plan.title.as_str()), Some(at)) {
                        return self.git_report(format!("git tag: {e}"), false);
                    }
                    self.git_refs_changed(false);
                }
                self.git_start_job(Op::Release(plan));
            }
        }
    }
}

/// Checked-out branch of `top` (None when detached).
fn refs_current(top: &Path) -> Option<String> {
    current_branch(top)
}
