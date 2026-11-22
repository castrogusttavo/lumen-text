use super::*;

// ───────────────────────────── state ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Changes,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Tab(Tab),
    Refresh,
    Close,
    Check(usize),
    Discard(usize),
    ConfirmDiscard(usize),
    Row(usize),
    Section(Group),
    /// A group header (or its chevron): collapse / expand its files.
    Toggle(Group),
    Msg,
    Opt,
    CommitBtn,
    Commit(usize),
    CFile(usize),
    Fetch,
    Pull,
    Push,
    /// "Amend last commit" checkbox.
    Amend,
    /// "Publish" in the set-upstream prompt.
    PublishYes,
    PublishNo,
    /// Cancel the running fetch / pull / push.
    JobCancel,
    /// The result / error line under the branch row (click dismisses it).
    Note,
    /// "Force push" / "Cancel" in the force-push card.
    ForceYes,
    ForceNo,
    /// A pull strategy button of the chooser card, and its "Cancel".
    Strategy(PullStrategy),
    StrategyNo,
    /// Button `n` of the confirmation card (`GitPanel::card`).
    Ask(usize),
    /// "Undo last commit" next to the Amend checkbox.
    UndoLast,
    /// "Stash…" in the Stashes header.
    StashNew,
    /// Body of a stash row (expands its files), and its Apply / Pop / Drop buttons.
    Stash(usize),
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
    /// A file of the expanded stash.
    SFile(usize),
    /// Open the merge editor on conflicted file `i`.
    Merge(usize),
    /// A folder row of a group (the change index of its first file): fold / unfold it.
    Folder(Group, usize),
    /// Discard every unstaged / untracked change of the group (asks first).
    DiscardAll(Group),
    /// A dropdown button of the header, the sync button or the Commit button.
    Menu(GpMenu),
    /// Item `n` of the open dropdown.
    MenuItem(usize),
    /// The branch / repository chip: opens the branch switcher.
    Branches,
    /// The sparkle button of the composer: write a message from the diff.
    Generate,
    /// "Commit & push" of the Commit dropdown.
    CommitPush,
    /// "Pull strategy..." of the sync dropdown.
    PullChoose,
    /// The "Filter changes" box and its clear button.
    Filter,
    FilterClear,
    /// "Initialize repository" of a folder that is not one yet.
    Init,
}

/// The dropdowns of the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GpMenu {
    /// The "..." button of the header.
    Actions,
    /// The chevron of the sync button (fetch / pull / push).
    Sync,
    /// The chevron of the Commit button (amend, commit & push, ...).
    Commit,
}

/// One entry of a dropdown.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub icon: &'static str,
    /// What a click does (handled like the same hit on the panel itself).
    pub hit: Hit,
    /// Drawn with a tick box (None: no box).
    pub checked: Option<bool>,
    /// Right-aligned dim text (a count).
    pub trailing: String,
}

impl MenuItem {
    fn new(label: impl Into<String>, icon: &'static str, hit: Hit) -> Self {
        MenuItem { label: label.into(), icon, hit, checked: None, trailing: String::new() }
    }
}

/// What the sync button next to the branch does now, and how it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPlan {
    pub hit: Hit,
    pub icon: &'static str,
    /// Commits to pull / push (0 for a fetch or a first publish).
    pub count: usize,
    pub title: String,
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 { format!("{n} {what}") } else { format!("{n} {what}s") }
}

/// The one remote operation worth suggesting: pull when the remote is ahead, push when the branch
/// is, publish when it has no upstream, otherwise fetch.
pub fn sync_plan(ahead: usize, behind: usize, has_upstream: bool) -> SyncPlan {
    if !has_upstream {
        return SyncPlan { hit: Hit::Push, icon: "ui/arrow_up", count: 0, title: "Publish branch".into() };
    }
    if behind > 0 {
        return SyncPlan {
            hit: Hit::Pull,
            icon: "ui/arrow_down",
            count: behind,
            title: format!("Pull {}", plural(behind, "commit")),
        };
    }
    if ahead > 0 {
        return SyncPlan {
            hit: Hit::Push,
            icon: "ui/arrow_up",
            count: ahead,
            title: format!("Push {}", plural(ahead, "commit")),
        };
    }
    SyncPlan { hit: Hit::Fetch, icon: "ui/rotate_cw", count: 0, title: "Fetch from remote".into() }
}

/// Why the force-push card is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForceWhy {
    /// A push was rejected as non-fast-forward and nothing new is waiting on the remote.
    Rejected,
    /// The last commit was amended after it had been published.
    Amended,
}

impl ForceWhy {
    pub fn text(self) -> &'static str {
        match self {
            ForceWhy::Rejected => {
                "Force the push? The remote has a different version of this branch (you rewrote commits that were already published). --force-with-lease only overwrites if nobody else has pushed anything since your last fetch."
            }
            ForceWhy::Amended => {
                "The amended commit was already published. Sending it requires a force push. --force-with-lease only overwrites if the remote is still as it was at your last fetch."
            }
        }
    }
}

/// Text of the pull strategy chooser card.
pub const STRATEGY_TEXT: &str = "The branches have diverged. How should the remote commits be integrated? Your choice is saved (config git_pull_strategy).";

/// Pack button widths into right-aligned rows no wider than `avail`: returns the number of
/// buttons on each row, filled from the left, `gap` px apart.
pub fn button_rows(widths: &[i32], avail: i32, gap: i32) -> Vec<usize> {
    let mut rows: Vec<usize> = Vec::new();
    let mut used = 0;
    for (i, &w) in widths.iter().enumerate() {
        if i == 0 || used + gap + w > avail {
            rows.push(1);
            used = w;
        } else {
            *rows.last_mut().unwrap() += 1;
            used += gap + w;
        }
    }
    rows
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Header(Group),
    File(usize),
    Commit(usize),
    CFile(usize),
    /// A folder of a group (the change index of its first file): folds its files.
    Folder(Group, usize),
    /// "Working tree clean" line above the stashes of a clean tree.
    NoChanges,
    /// "Stashes" header (with the "Stash…" button).
    StashHead,
    Stash(usize),
    /// A file of the expanded stash.
    SFile(usize),
}

pub(super) const HEAD_H: i32 = 40;
/// The "Filter changes" box above the lists.
pub(super) const FILTER_H: i32 = 36;
pub(super) const BRANCH_H: i32 = 34;
/// One line of the result / progress strip under the branch row.
pub(super) const STRIP_LINE: i32 = 17;
/// Height of the "publish this branch?" prompt.
pub(super) const PROMPT_H: i32 = 58;
pub(super) const FILE_H: i32 = 28;
pub(super) const FOLDER_H: i32 = 26;
pub(super) const SEC_H: i32 = 30;
pub(super) const COMMIT_H: i32 = 44;
pub(super) const STASH_H: i32 = 42;
pub(super) const CFILE_H: i32 = 28;
/// Height of the composer's button row (sparkle, scope, Commit) under the message.
pub(super) const COMPOSER_ROW_H: i32 = 34;
/// Composer around the message box: gap above, the button row, gap below.
pub(super) const COMMIT_CHROME_H: i32 = 6 + COMPOSER_ROW_H + 8;
/// Margin between the panel edge and the composer box.
pub(super) const COMPOSER_MARGIN: i32 = 8;
/// Smallest row height of the message box.
pub(super) const LINE: i32 = 18;
/// Message box padding (horizontal / vertical).
pub(super) const MSG_PAD_X: i32 = 10;
pub(super) const MSG_PAD_Y: i32 = 8;
/// The message box shows 3 to 10 visual lines, then scrolls.
pub(super) const MSG_MIN_LINES: i32 = 3;
pub(super) const MSG_MAX_LINES: i32 = 10;
/// Room kept for the changes list above when the message box grows.
pub(super) const MSG_LIST_ROOM: i32 = 3 * FILE_H + SEC_H;

pub struct GitPanel {
    pub root: PathBuf,
    pub top: Option<PathBuf>,
    pub tab: Tab,
    pub changes: Vec<Change>,
    /// Commit message: soft-wrapped, scrolls vertically, real selection, undo.
    pub msg: TextArea,
    pub msg_focus: bool,
    /// "Filter changes" box of the Changes tab: only the files whose path contains the text show.
    pub filter: TextField,
    pub filter_focus: bool,
    /// Last press in the message box (when, where, click count) for double / triple clicks.
    pub msg_click: (Instant, (f64, f64), u32),
    /// Last autoscroll step while dragging a selection out of the message box.
    pub msg_auto: Option<Instant>,
    /// Keyboard focus is inside the panel.
    pub focused: bool,
    pub include_all: bool,
    pub notice: Option<(String, bool, Instant)>,
    pub confirm: Option<usize>,
    pub cursor: usize,
    pub scroll: [f64; 2],
    pub max_scroll: [f64; 2],
    pub commits: Vec<LogEntry>,
    pub log_done: bool,
    pub open_commit: Option<usize>,
    pub files: Vec<CommitFile>,
    pub open_file: Option<usize>,
    pub hits: Vec<(Rect, Hit)>,
    pub hover: Option<Hit>,
    /// Message box as drawn (its height follows the text).
    pub msg_box: Rect,
    pub sig: u64,
    pub hist_sig: u64,
    pub closed: bool,
    /// The left edge is being dragged.
    /// Ahead / behind of the branch against its upstream.
    pub sync: SyncInfo,
    /// Fetch / pull / push running on a worker thread.
    pub job: Option<Job>,
    /// Last result of a remote operation: (text, ok, when). Errors stay until dismissed.
    pub sync_note: Option<(String, bool, Instant)>,
    /// Lines the note was wrapped into at the last draw (the strip grows with it).
    pub note_lines: i32,
    /// "Publish branch on remote?" prompt: (remote, branch).
    pub publish: Option<(String, String)>,
    /// "Amend" is checked: the message box holds the last commit's message.
    pub amend: bool,
    /// What was in the message box before Amend replaced it.
    pub amend_draft: String,
    /// Last time the progress counter forced a redraw.
    pub spun: Instant,
    /// The force-push card, when it is showing.
    pub force: Option<ForceWhy>,
    /// The pull strategy chooser card is showing.
    pub strategy_ask: bool,
    /// Height of the card above, measured at the last draw (it depends on the width).
    pub prompt_h: i32,
    /// Confirmation card (delete branch / tag, release, undo a published commit…).
    pub ask: Option<Ask>,
    /// Merge / rebase / cherry-pick in progress, as of the last refresh.
    pub state: RepoState,
    /// HEAD has a parent, so "Undo last commit" makes sense.
    pub can_undo: bool,
    /// Worker streaming the history (None when nothing is loading).
    pub loader: Option<HistLoader>,
    /// Wakes the event loop when a page of history arrives (None in tests).
    pub wake: Option<crate::lsp::Wake>,
    /// Lanes of the loaded commits (parallel to `commits`).
    pub graph: Graph,
    /// Commit index -> its branch / tag / HEAD labels.
    pub chips: std::collections::HashMap<usize, Vec<Chip>>,
    /// Lane colours and the background they were made for.
    pub lane_palette: Option<(Rgba, [Rgba; LANE_COLORS])>,
    /// Stashes, newest first (re-read with the status).
    pub stashes: Vec<StashEntry>,
    /// Expanded stash (its files are listed under it) and its files.
    pub open_stash: Option<usize>,
    pub stash_files: Vec<CommitFile>,
    pub open_sfile: Option<usize>,
    /// Files selected with Ctrl / Shift + click (repository-relative paths): what "Stash" takes.
    pub selected: std::collections::BTreeSet<String>,
    /// Change index a Shift + click extends the selection from.
    pub sel_anchor: Option<usize>,
    /// Action asked for while the panel was taken out of the App (run right after it is back).
    pub pending: Option<GitAct>,
    /// Background refresh (see `refresh_bg`): answers tagged with a generation.
    pub refresh_ch: (std::sync::mpsc::Sender<(u64, PanelData)>, std::sync::mpsc::Receiver<(u64, PanelData)>),
    pub refresh_gen: u64,
    pub refresh_busy: bool,
    pub refresh_again: bool,
    /// Groups whose files are folded away under their header.
    pub collapsed: std::collections::HashSet<Group>,
    /// Folders (group, directory) whose files are folded away under their row.
    pub collapsed_dirs: std::collections::HashSet<(Group, String)>,
    /// The open dropdown.
    pub menu: Option<GpMenu>,
    /// Line counts per file; `None` until the first read arrives (nothing is drawn meanwhile).
    pub stats: Option<NumStats>,
}

/// What one refresh of the panel reads from git (on any thread; applied on the UI thread).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelData {
    /// The folder it was read for.
    pub root: PathBuf,
    pub top: Option<PathBuf>,
    pub changes: Vec<Change>,
    pub sync: SyncInfo,
    pub state: RepoState,
    pub can_undo: bool,
    pub stashes: Vec<StashEntry>,
    pub stats: NumStats,
}

/// Run the panel's git commands for `root`: the repository top, one `git status` (changes,
/// upstream, ahead / behind), the operation in progress, whether HEAD has a parent, the stashes.
pub fn read_panel(root: &Path) -> PanelData {
    let top = git::toplevel(root);
    let Some(t) = top.as_deref() else {
        return PanelData {
            root: root.to_path_buf(),
            top: None,
            changes: Vec::new(),
            sync: SyncInfo::default(),
            state: RepoState::Clean,
            can_undo: false,
            stashes: Vec::new(),
            stats: NumStats::default(),
        };
    };
    let st = git_run(t, &STATUS_V2_ARGS).map(|o| parse_status_v2(&o)).unwrap_or_default();
    PanelData {
        root: root.to_path_buf(),
        changes: st.changes,
        sync: st.sync,
        state: repo_state(t),
        can_undo: git_run(t, &["rev-parse", "--verify", "-q", "HEAD~1"]).is_ok(),
        stashes: list_stashes(t),
        stats: NumStats {
            unstaged: git_run(t, &["diff", "--numstat", "-z", "--no-renames"])
                .map(|o| parse_numstat(&o))
                .unwrap_or_default(),
            staged: git_run(t, &["diff", "--cached", "--numstat", "-z", "--no-renames"])
                .map(|o| parse_numstat(&o))
                .unwrap_or_default(),
        },
        top,
    }
}

impl GitPanel {
    pub fn new(root: &Path) -> Self {
        let mut p = GitPanel {
            root: root.to_path_buf(),
            top: None,
            tab: Tab::Changes,
            changes: Vec::new(),
            msg: TextArea::default(),
            msg_focus: true,
            filter: TextField::default(),
            filter_focus: false,
            msg_click: (Instant::now(), (f64::NAN, f64::NAN), 0),
            msg_auto: None,
            focused: true,
            include_all: true,
            notice: None,
            confirm: None,
            cursor: 0,
            scroll: [0.0; 2],
            max_scroll: [0.0; 2],
            commits: Vec::new(),
            log_done: false,
            open_commit: None,
            files: Vec::new(),
            open_file: None,
            hits: Vec::new(),
            hover: None,
            msg_box: Rect::default(),
            sig: 0,
            hist_sig: 0,
            closed: false,
            sync: SyncInfo::default(),
            job: None,
            sync_note: None,
            note_lines: 1,
            publish: None,
            amend: false,
            amend_draft: String::new(),
            spun: Instant::now(),
            force: None,
            strategy_ask: false,
            prompt_h: PROMPT_H,
            ask: None,
            state: RepoState::Clean,
            can_undo: false,
            loader: None,
            wake: None,
            graph: Graph::default(),
            chips: std::collections::HashMap::new(),
            lane_palette: None,
            stashes: Vec::new(),
            open_stash: None,
            stash_files: Vec::new(),
            open_sfile: None,
            selected: std::collections::BTreeSet::new(),
            sel_anchor: None,
            pending: None,
            refresh_ch: std::sync::mpsc::channel(),
            refresh_gen: 0,
            refresh_busy: false,
            refresh_again: false,
            collapsed: std::collections::HashSet::new(),
            collapsed_dirs: std::collections::HashSet::new(),
            menu: None,
            stats: None,
        };
        p.refresh();
        p
    }

    /// Re-read everything now, on this thread (after an action of the user, and in tests).
    pub fn refresh(&mut self) {
        // A background answer computed before this one must not overwrite it.
        self.refresh_gen += 1;
        let data = read_panel(&self.root);
        self.apply(data);
    }

    /// `refresh` on a worker thread, for the automatic refreshes (the status, the index or the
    /// stashes moved): the panel keeps showing what it has until the answer arrives (see
    /// `pump_refresh`). Requests made while one is running are coalesced into a single follow-up.
    pub fn refresh_bg(&mut self) {
        let Some(wake) = self.wake.clone().filter(|_| !cfg!(test)) else { return self.refresh() };
        if self.refresh_busy {
            self.refresh_again = true;
            return;
        }
        self.refresh_busy = true;
        self.refresh_gen += 1;
        let (generation, root, tx) = (self.refresh_gen, self.root.clone(), self.refresh_ch.0.clone());
        std::thread::spawn(move || {
            let _ = tx.send((generation, read_panel(&root)));
            wake();
        });
    }

    /// Apply finished background refreshes; returns whether the panel changed. An answer older
    /// than the last refresh asked for, or read for another folder, is dropped.
    pub fn pump_refresh(&mut self) -> bool {
        let (mut got, mut changed) = (false, false);
        while let Ok((generation, data)) = self.refresh_ch.1.try_recv() {
            got = true;
            self.refresh_busy = false;
            if generation == self.refresh_gen && data.root == self.root {
                self.apply(data);
                changed = true;
            }
        }
        if got && std::mem::take(&mut self.refresh_again) {
            self.refresh_bg();
        }
        changed
    }

    fn apply(&mut self, data: PanelData) {
        // Stashes: the expanded one stays expanded while it exists (its index may have moved).
        let open_sha = self.open_stash.and_then(|i| self.stashes.get(i)).map(|e| e.sha.clone());
        self.top = data.top;
        self.changes = data.changes;
        self.sync = data.sync;
        (self.state, self.can_undo) = (data.state, data.can_undo);
        self.stashes = data.stashes;
        self.stats = Some(data.stats);
        let still = open_sha.and_then(|s| self.stashes.iter().position(|e| e.sha == s));
        if still.is_none() {
            self.stash_files.clear();
            self.open_sfile = None;
        }
        self.open_stash = still;
        // The selection keeps the files that are still changed.
        let changed: std::collections::HashSet<&str> = self.changes.iter().map(|c| c.path.as_str()).collect();
        self.selected.retain(|p| changed.contains(p.as_str()));
        if self.sel_anchor.is_some_and(|a| a >= self.changes.len()) {
            self.sel_anchor = None;
        }
        self.confirm = None;
        let n = self.row_count();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    pub(super) fn note_visible(&self) -> bool {
        self.sync_note.as_ref().is_some_and(|(_, ok, t)| !*ok || t.elapsed().as_secs() < 8)
    }

    /// Height of the strip under the branch row: the set-upstream prompt, the progress of a
    /// running command, or the last result.
    pub fn strip_h(&self) -> i32 {
        if self.publish.is_some() {
            PROMPT_H
        } else if self.force.is_some() || self.strategy_ask || self.card().is_some() {
            self.prompt_h.max(PROMPT_H)
        } else if self.job.is_some() {
            STRIP_LINE + 10
        } else if self.note_visible() {
            self.note_lines.max(1) * STRIP_LINE + 10
        } else {
            0
        }
    }

    /// Buttons of the force-push card (`force`) or the strategy chooser: (label, hit, warning color).
    pub fn gp_card_buttons_for(force: bool) -> Vec<(&'static str, Hit, bool)> {
        if force {
            return vec![("Force push", Hit::ForceYes, true), ("Cancel", Hit::ForceNo, false)];
        }
        let mut v: Vec<(&'static str, Hit, bool)> = [PullStrategy::Merge, PullStrategy::Rebase, PullStrategy::FfOnly]
            .into_iter()
            .map(|st| {
                (
                    match st {
                        PullStrategy::Merge => "Merge",
                        PullStrategy::Rebase => "Rebase",
                        PullStrategy::FfOnly => "FF only",
                    },
                    Hit::Strategy(st),
                    false,
                )
            })
            .collect();
        v.push(("Cancel", Hit::StrategyNo, false));
        v
    }

    /// Everything above the list: tabs, branch row and strip.
    pub fn chrome_h(&self) -> i32 {
        HEAD_H + BRANCH_H + self.strip_h()
    }

    /// Height of the message box: as drawn last, the 3-line minimum before the first frame.
    pub fn msg_h(&self) -> i32 {
        self.msg_box.h.max(MSG_MIN_LINES * LINE + 2 * MSG_PAD_Y)
    }

    /// Does file `i` pass the "Filter changes" box (case-insensitive part of its path)?
    pub fn filter_ok(&self, i: usize) -> bool {
        let q = self.filter.text.trim().to_lowercase();
        q.is_empty() || self.changes.get(i).is_some_and(|c| c.path.to_lowercase().contains(&q))
    }

    /// Height of the filter box above the lists: only when there is something to filter.
    pub fn filter_h(&self) -> i32 {
        if self.tab == Tab::Changes && (!self.changes.is_empty() || !self.filter.text.is_empty()) {
            FILTER_H
        } else {
            0
        }
    }

    /// Height of the commit area pinned to the bottom of the Changes tab.
    pub fn commit_area_h(&self) -> i32 {
        COMMIT_CHROME_H + self.msg_h()
    }

    /// Fold / unfold one folder of a group.
    pub fn toggle_dir(&mut self, g: Group, dir: &str) {
        let key = (g, dir.to_string());
        if !self.collapsed_dirs.remove(&key) {
            self.collapsed_dirs.insert(key);
        }
        let n = self.row_count();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    /// What a commit would take, for the composer: (label, added, removed) lines. The staged
    /// files when there are any, otherwise everything when "include everything" is on.
    pub fn composer_scope(&self) -> (String, u32, u32) {
        let staged = self.staged();
        let (label, groups): (String, Vec<Group>) = if staged > 0 {
            (format!("{staged} staged"), vec![Group::Staged])
        } else if self.include_all && !self.changes.is_empty() {
            (format!("All {} changes", self.changes.len()), vec![Group::Changes, Group::Untracked])
        } else {
            ("Nothing staged".to_string(), Vec::new())
        };
        let (mut add, mut del) = (0, 0);
        if let Some(st) = &self.stats {
            for c in self.changes.iter().filter(|c| groups.contains(&c.group)) {
                if let Some(s) = st.of(c) {
                    add += s.add.unwrap_or(0);
                    del += s.del.unwrap_or(0);
                }
            }
        }
        (label, add, del)
    }

    /// Tooltip title of the Fetch / Pull / Push button: what the sync button says when it is the
    /// suggested step ("Pull 2 commits"), `plain` otherwise.
    pub fn sync_title(&self, hit: Hit, plain: &str) -> String {
        let plan = sync_plan(self.sync.ahead, self.sync.behind, self.sync.upstream.is_some());
        if plan.hit == hit { plan.title } else { plain.to_string() }
    }

    /// Fill the message box with a suggestion written from the files in scope.
    pub fn generate_message(&mut self) {
        let staged = self.staged() > 0;
        let scope: Vec<Change> = self
            .changes
            .iter()
            .filter(|c| if staged { c.group == Group::Staged } else { c.group != Group::Staged })
            .cloned()
            .collect();
        if scope.is_empty() {
            return self.say("Nothing to describe: no changes", false);
        }
        let text = suggest_message(&scope);
        self.msg.set_text(&text);
        self.msg_focus = true;
    }

    /// Entries of dropdown `m`.
    pub fn menu_items(&self, m: GpMenu) -> Vec<MenuItem> {
        let mut v = Vec::new();
        let (ahead, behind, up) = (self.sync.ahead, self.sync.behind, self.sync.upstream.is_some());
        match m {
            GpMenu::Actions => {
                v.push(MenuItem::new("Refresh", "ui/rotate_cw", Hit::Refresh));
                if !self.changes.is_empty() {
                    v.push(MenuItem::new("Stash changes...", "ui/archive", Hit::StashNew));
                }
                if self.can_undo && self.state == RepoState::Clean {
                    v.push(MenuItem::new("Undo last commit", "ui/undo", Hit::UndoLast));
                }
            }
            GpMenu::Sync => {
                v.push(MenuItem::new("Fetch", "ui/cloud_download", Hit::Fetch));
                let mut pull = MenuItem::new("Pull", "ui/arrow_down", Hit::Pull);
                if behind > 0 {
                    pull.trailing = behind.to_string();
                }
                let mut push = MenuItem::new(if up { "Push" } else { "Publish branch" }, "ui/arrow_up", Hit::Push);
                if up && ahead > 0 {
                    push.trailing = ahead.to_string();
                }
                v.extend([pull, push, MenuItem::new("Pull strategy...", "ui/git_branch", Hit::PullChoose)]);
            }
            GpMenu::Commit => {
                v.push(MenuItem::new("Commit & push", "ui/arrow_up", Hit::CommitPush));
                let mut am = MenuItem::new("Amend last commit", "ui/git_commit", Hit::Amend);
                am.checked = Some(self.amend);
                let mut all = MenuItem::new("Include everything if nothing is staged", "ui/plus", Hit::Opt);
                all.checked = Some(self.include_all);
                v.extend([am, all]);
                if self.can_undo && !self.amend && self.state == RepoState::Clean {
                    v.push(MenuItem::new("Undo last commit", "ui/undo", Hit::UndoLast));
                }
            }
        }
        v
    }

    /// Top-left corner of the message text (view coordinates of the text area).
    pub fn msg_origin(&self) -> (f64, f64) {
        ((self.msg_box.x + MSG_PAD_X) as f64, (self.msg_box.y + MSG_PAD_Y) as f64)
    }

    /// Click count of a press in the message box at (x, y): 1, 2 (word), 3 (line), then 1 again.
    pub fn msg_click_count(&mut self, x: f64, y: f64) -> u32 {
        let (t, (px, py), n) = self.msg_click;
        let near = (x - px).abs() < 5.0 && (y - py).abs() < 5.0;
        let n = if near && t.elapsed() < Duration::from_millis(400) { n % 3 + 1 } else { 1 };
        self.msg_click = (Instant::now(), (x, y), n);
        n
    }

    pub fn reset_history(&mut self) {
        // Dropping the loader ends its worker (and the git process behind it).
        self.loader = None;
        self.commits = Vec::new();
        self.graph.clear();
        self.chips.clear();
        self.log_done = false;
        self.open_commit = None;
        self.files.clear();
        self.open_file = None;
        self.scroll[1] = 0.0;
        if self.tab == Tab::History {
            self.cursor = 0;
            self.load_more();
        }
    }

    /// Fold / unfold a group's files; the cursor stays on a row that exists.
    pub fn toggle_group(&mut self, g: Group) {
        if !self.collapsed.remove(&g) {
            self.collapsed.insert(g);
        }
        let n = self.row_count();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    pub fn staged(&self) -> usize {
        self.changes.iter().filter(|c| c.group == Group::Staged).count()
    }

    pub fn group_paths(&self, g: Group) -> Vec<String> {
        let mut v = Vec::new();
        for c in self.changes.iter().filter(|c| c.group == g) {
            v.push(c.path.clone());
            if g == Group::Staged {
                v.extend(c.old.clone());
            }
        }
        v
    }

    /// Flat list of what the active tab shows.
    pub fn rows(&self) -> Vec<Row> {
        let mut v = Vec::new();
        match self.tab {
            Tab::Changes => {
                for g in [Group::Staged, Group::Changes, Group::Untracked] {
                    let idx: Vec<usize> =
                        (0..self.changes.len()).filter(|&i| self.changes[i].group == g && self.filter_ok(i)).collect();
                    if idx.is_empty() {
                        continue;
                    }
                    v.push(Row::Header(g));
                    if !self.collapsed.contains(&g) {
                        for (dir, files) in group_by_dir(&self.changes, &idx) {
                            if dir.is_empty() {
                                v.extend(files.into_iter().map(Row::File));
                                continue;
                            }
                            v.push(Row::Folder(g, files[0]));
                            if !self.collapsed_dirs.contains(&(g, dir)) {
                                v.extend(files.into_iter().map(Row::File));
                            }
                        }
                    }
                }
                // Stashes after the changes; the header also offers "Stash…" when there is
                // something to stash.
                if !self.stashes.is_empty() {
                    if self.changes.is_empty() {
                        v.push(Row::NoChanges);
                    }
                    v.push(Row::StashHead);
                    for i in 0..self.stashes.len() {
                        v.push(Row::Stash(i));
                        if self.open_stash == Some(i) {
                            v.extend((0..self.stash_files.len()).map(Row::SFile));
                        }
                    }
                } else if !self.changes.is_empty() {
                    v.push(Row::StashHead);
                }
            }
            Tab::History => v.extend((0..self.hist_len()).filter_map(|k| self.hist_row(k))),
        }
        v
    }

    pub fn row_h(r: Row) -> i32 {
        match r {
            Row::Header(_) | Row::StashHead => SEC_H,
            Row::Folder(..) => FOLDER_H,
            Row::File(_) | Row::NoChanges => FILE_H,
            Row::Commit(_) => COMMIT_H,
            Row::CFile(_) | Row::SFile(_) => CFILE_H,
            Row::Stash(_) => STASH_H,
        }
    }

    /// Rows of the active tab (the History tab counts them without listing them).
    pub fn row_count(&self) -> usize {
        match self.tab {
            Tab::Changes => self.rows().len(),
            Tab::History => self.hist_len(),
        }
    }

    pub fn row_at(&self, k: usize) -> Option<Row> {
        match self.tab {
            Tab::Changes => self.rows().get(k).copied(),
            Tab::History => self.hist_row(k),
        }
    }

    /// Index of row `r` in the active tab.
    pub fn index_of(&self, r: Row) -> Option<usize> {
        match self.tab {
            Tab::Changes => self.rows().iter().position(|x| *x == r),
            Tab::History => self.hist_index(r),
        }
    }

    /// Top of row `k` in content coordinates of the active tab.
    pub fn row_top(&self, k: usize) -> i32 {
        match self.tab {
            Tab::Changes => self.rows().iter().take(k).map(|r| Self::row_h(*r)).sum(),
            Tab::History => self.hist_top(k),
        }
    }

    pub fn content_h(&self) -> i32 {
        match self.tab {
            Tab::Changes => self.rows().into_iter().map(Self::row_h).sum::<i32>() + 16,
            Tab::History => self.hist_top(self.hist_len()) + 16,
        }
    }

    pub fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    pub fn say(&mut self, msg: impl Into<String>, ok: bool) {
        self.notice = Some((msg.into(), ok, Instant::now()));
    }

    /// Move the keyboard cursor over rows, skipping section headers.
    pub fn move_cursor(&mut self, d: isize) {
        let n = self.row_count() as isize;
        let mut i = self.cursor as isize;
        loop {
            i += d;
            if i < 0 || i >= n {
                return;
            }
            if !matches!(
                self.row_at(i as usize),
                Some(Row::Header(_) | Row::Folder(..) | Row::StashHead | Row::NoChanges) | None
            ) {
                self.cursor = i as usize;
                return;
            }
        }
    }

    /// Scroll the active tab so that rows `first..=last` are visible in a viewport of height
    /// `view`: the last row's bottom wins when the span fits, otherwise the first row's top.
    pub fn reveal_span(&mut self, first: usize, last: usize, view: i32) {
        let k = if self.tab == Tab::Changes { 0 } else { 1 };
        let top = self.row_top(first);
        let bottom = self.row_top(last + 1);
        let (s, v) = (self.scroll[k], view as f64);
        if bottom - top > view || (top as f64) < s {
            self.scroll[k] = top as f64;
        } else if bottom as f64 > s + v {
            self.scroll[k] = (bottom - view) as f64;
        }
    }

    /// Scroll so that row `idx` is inside a viewport of height `view`.
    pub fn reveal(&mut self, view: i32) {
        let k = if self.tab == Tab::Changes { 0 } else { 1 };
        let top = self.row_top(self.cursor);
        let h = self.row_at(self.cursor).map_or(0, Self::row_h);
        if (top as f64) < self.scroll[k] {
            self.scroll[k] = top as f64;
        } else if (top + h) as f64 > self.scroll[k] + view as f64 {
            self.scroll[k] = (top + h - view) as f64;
        }
    }
}

/// Short Portuguese status word for tooltips-less UIs: only the letter is drawn.
pub fn code_color(c: &crate::theme::Chrome, code: char) -> Rgba {
    match code {
        'A' | '?' => c.git_added,
        'D' => c.git_deleted,
        'U' => c.error,
        'R' | 'C' => c.info,
        _ => c.git_modified,
    }
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub(super) fn mtime(p: &Path) -> u128 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

/// Longest prefix of `text` (plus an ellipsis) that fits `max` px.
pub(crate) fn fit(tx: &mut TextCtx, text: &str, max: i32, bold: bool) -> String {
    if max <= 0 {
        return String::new();
    }
    if tx.ui_width(text, bold) <= max {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand: String = chars[..mid].iter().collect::<String>() + "…";
        if tx.ui_width(&cand, bold) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>() + "…"
}

/// Greedy word wrap of `text` into at most `max` lines of `width` px; the last one gets an
/// ellipsis when the text does not fit.
pub(super) fn wrap_lines(tx: &mut TextCtx, text: &str, width: i32, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if cur.is_empty() || tx.ui_width(&cand, false) <= width {
            cur = cand;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max {
        lines.truncate(max);
        let last = lines.pop().unwrap_or_default();
        lines.push(fit(tx, &format!("{last}…"), width, false));
    }
    // A single word wider than the line is cut with an ellipsis.
    lines.into_iter().map(|l| fit(tx, &l, width, false)).collect()
}

pub(super) fn split_name(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[i + 1..], &path[..i]),
        None => (path, ""),
    }
}

/// Narrowest width the panel clamp allows.
pub(super) const GP_MIN_W: i32 = 280;
/// Narrowest the panel is while the GitHub tab shows (list and detail need the room).
pub(super) const GH_MIN_W: i32 = 460;
/// Widest the automatic GitHub width gets.
pub(super) const GH_AUTO_MAX_W: i32 = 820;
/// Half-width of the grab band around the panel's left edge.
pub(super) const GP_GRAB: i32 = 4;

/// Panel width for a requested `want` px (a dragged value) inside a window `win` px wide:
/// at least `GP_MIN_W` (less in tiny windows) and at most 60% of the window.
pub fn clamp_panel_width(want: i32, win: i32) -> i32 {
    let lo = GP_MIN_W.min(win / 2);
    want.clamp(lo, (win * 6 / 10).max(lo))
}

/// Automatic width: 30% of the window, 340..440 px.
pub(super) fn auto_panel_width(win: i32) -> i32 {
    ((win as f32 * 0.30) as i32).clamp(340, 440).min(win / 2)
}

/// Keys that only change modifier state; they never mean anything on their own.
pub(super) fn is_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Control
                | NamedKey::Shift
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    )
}
