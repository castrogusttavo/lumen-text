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
}

pub(super) const LOG_PAGE: usize = 60;
pub(super) const HEAD_H: i32 = 44;
pub(super) const BRANCH_H: i32 = 36;
/// One line of the result / progress strip under the branch row.
pub(super) const STRIP_LINE: i32 = 17;
/// Height of the "publish this branch?" prompt.
pub(super) const PROMPT_H: i32 = 58;
pub(super) const FILE_H: i32 = 30;
pub(super) const SEC_H: i32 = 34;
pub(super) const COMMIT_H: i32 = 52;
pub(super) const CFILE_H: i32 = 28;
pub(super) const MSG_H: i32 = 92;
pub(super) const AMEND_H: i32 = 28;
pub(super) const COMMIT_AREA_H: i32 = 12 + 22 + MSG_H + 8 + AMEND_H + 34 + 12;
pub(super) const LINE: i32 = 18;

pub struct GitPanel {
    pub root: PathBuf,
    pub top: Option<PathBuf>,
    pub tab: Tab,
    pub changes: Vec<Change>,
    pub msg: TextField,
    pub msg_top: usize,
    pub msg_focus: bool,
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
    pub msg_box: Rect,
    pub msg_hscroll: i32,
    pub sig: u64,
    pub hist_sig: u64,
    pub closed: bool,
    /// The left edge is being dragged.
    pub resizing: bool,
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
}

impl GitPanel {
    pub fn new(root: &Path) -> Self {
        let mut p = GitPanel {
            root: root.to_path_buf(),
            top: None,
            tab: Tab::Changes,
            changes: Vec::new(),
            msg: TextField::default(),
            msg_top: 0,
            msg_focus: true,
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
            msg_hscroll: 0,
            sig: 0,
            hist_sig: 0,
            closed: false,
            resizing: false,
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
        };
        p.refresh();
        p
    }

    pub fn refresh(&mut self) {
        self.top = git::toplevel(&self.root);
        self.changes = match &self.top {
            Some(t) => {
                git_run(t, &["status", "--porcelain=v1", "-z", "-uall"]).map(|o| parse_status_z(&o)).unwrap_or_default()
            }
            None => Vec::new(),
        };
        self.sync = match &self.top {
            Some(t) => gitsync::info(t),
            None => SyncInfo::default(),
        };
        self.confirm = None;
        let n = self.rows().len();
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
        } else if self.force.is_some() || self.strategy_ask {
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

    pub fn reset_history(&mut self) {
        self.commits.clear();
        self.log_done = false;
        self.open_commit = None;
        self.files.clear();
        self.open_file = None;
        self.scroll[1] = 0.0;
        if self.tab == Tab::History {
            self.load_more();
        }
    }

    pub fn load_more(&mut self) {
        let Some(top) = self.top.clone() else {
            self.log_done = true;
            return;
        };
        if self.log_done {
            return;
        }
        let page = log_page(&top, self.commits.len(), LOG_PAGE);
        if page.len() < LOG_PAGE {
            self.log_done = true;
        }
        self.commits.extend(page);
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
                    let idx: Vec<usize> = (0..self.changes.len()).filter(|&i| self.changes[i].group == g).collect();
                    if idx.is_empty() {
                        continue;
                    }
                    v.push(Row::Header(g));
                    v.extend(idx.into_iter().map(Row::File));
                }
            }
            Tab::History => {
                for i in 0..self.commits.len() {
                    v.push(Row::Commit(i));
                    if self.open_commit == Some(i) {
                        v.extend((0..self.files.len()).map(Row::CFile));
                    }
                }
            }
        }
        v
    }

    pub fn row_h(r: Row) -> i32 {
        match r {
            Row::Header(_) => SEC_H,
            Row::File(_) => FILE_H,
            Row::Commit(_) => COMMIT_H,
            Row::CFile(_) => CFILE_H,
        }
    }

    pub fn content_h(&self) -> i32 {
        self.rows().into_iter().map(Self::row_h).sum::<i32>() + 16
    }

    pub fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    pub fn say(&mut self, msg: impl Into<String>, ok: bool) {
        self.notice = Some((msg.into(), ok, Instant::now()));
    }

    /// Move the keyboard cursor over rows, skipping section headers.
    pub fn move_cursor(&mut self, d: isize) {
        let rows = self.rows();
        let mut i = self.cursor as isize;
        loop {
            i += d;
            if i < 0 || i >= rows.len() as isize {
                return;
            }
            if !matches!(rows[i as usize], Row::Header(_)) {
                self.cursor = i as usize;
                return;
            }
        }
    }

    /// Scroll the active tab so that rows `first..=last` are visible in a viewport of height
    /// `view`: the last row's bottom wins when the span fits, otherwise the first row's top.
    pub fn reveal_span(&mut self, first: usize, last: usize, view: i32) {
        let rows = self.rows();
        let k = if self.tab == Tab::Changes { 0 } else { 1 };
        let top: i32 = rows.iter().take(first).map(|r| Self::row_h(*r)).sum();
        let bottom: i32 = rows.iter().take(last + 1).map(|r| Self::row_h(*r)).sum();
        let (s, v) = (self.scroll[k], view as f64);
        if bottom - top > view || (top as f64) < s {
            self.scroll[k] = top as f64;
        } else if bottom as f64 > s + v {
            self.scroll[k] = (bottom - view) as f64;
        }
    }

    /// Scroll so that row `idx` is inside a viewport of height `view`.
    pub fn reveal(&mut self, view: i32) {
        let rows = self.rows();
        let k = if self.tab == Tab::Changes { 0 } else { 1 };
        let top: i32 = rows.iter().take(self.cursor).map(|r| Self::row_h(*r)).sum();
        let h = rows.get(self.cursor).map_or(0, |r| Self::row_h(*r));
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

/// Narrowest the panel may be dragged to.
pub(super) const GP_MIN_W: i32 = 280;
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
