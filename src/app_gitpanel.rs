//! Git panel docked on the right: two tabs.
//!   * "Alterações": changed files grouped as Staged / Mudanças / Não rastreados (checkbox =
//!     stage/unstage, click = side-by-side diff against HEAD, trash = discard with a second click
//!     to confirm) and a multi-line commit message box (Ctrl+Enter commits).
//!   * "Histórico": `git log` loaded a page at a time while scrolling; a click on a commit
//!     expands its files and shows the first file's diff.
//!
//! The first half of the file is plain logic (parsers, git calls, message editing) covered by
//! unit tests against real temporary repositories; the second half is the App glue and drawing.

use crate::{
    app::{App, KeyIn},
    app_gitx::DiffView,
    field::TextField,
    git,
    gitsync::{self, Fail, FailKind, Job, Op, SyncInfo},
    gitx::{self, Side},
    theme::Rgba,
    ui::{Canvas, Rect, TextCtx},
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

// ───────────────────────────── logic ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Group {
    Staged,
    Changes,
    Untracked,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Staged => "Staged",
            Group::Changes => "Mudanças",
            Group::Untracked => "Não rastreados",
        }
    }
}

/// One entry of `git status`. A file that is staged *and* modified again is ONE entry in the
/// Staged group whose `also` holds the working-tree state (so the panel draws a single row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Repository-root-relative path.
    pub path: String,
    /// Previous path of a rename / copy.
    pub old: Option<String>,
    /// M A D R C T U or '?'.
    pub code: char,
    pub group: Group,
    /// Staged entries only: the file also has unstaged changes (their M/D code).
    pub also: Option<char>,
}

impl Change {
    /// Has working-tree changes that "discard" can throw away.
    pub fn discardable(&self) -> bool {
        self.group != Group::Staged || self.also.is_some()
    }
}

/// State of a row's checkbox: nothing staged, everything staged, or staged with later edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckState {
    Off,
    On,
    Partial,
}

impl Change {
    pub fn check_state(&self) -> CheckState {
        match (self.group, self.also) {
            (Group::Staged, None) => CheckState::On,
            (Group::Staged, Some(_)) => CheckState::Partial,
            _ => CheckState::Off,
        }
    }
}

/// Parse `git status --porcelain=v1 -z -uall`; sorted by group (Staged, Mudanças, Não rastreados)
/// then path.
pub fn parse_status_z(out: &str) -> Vec<Change> {
    let mut v = Vec::new();
    let mut it = out.split('\0');
    while let Some(e) = it.next() {
        if e.len() < 4 || !e.is_char_boundary(2) || !e.is_char_boundary(3) {
            continue;
        }
        let (x, y) = (e.as_bytes()[0] as char, e.as_bytes()[1] as char);
        let path = e[3..].to_string();
        let old = (matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C')).then(|| it.next().unwrap_or("").to_string());
        let mk = |code: char, group: Group| Change { path: path.clone(), old: old.clone(), code, group, also: None };
        if x == '?' && y == '?' {
            v.push(mk('?', Group::Untracked));
        } else if x == '!' {
            continue;
        } else if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
            v.push(mk('U', Group::Changes));
        } else {
            match (x != ' ', y != ' ') {
                // Staged and modified again: a single row carrying both states.
                (true, true) => v.push(Change { also: Some(y), ..mk(x, Group::Staged) }),
                (true, false) => v.push(mk(x, Group::Staged)),
                (false, true) => v.push(mk(y, Group::Changes)),
                (false, false) => {}
            }
        }
    }
    v.sort_by(|a, b| (a.group as u8, &a.path).cmp(&(b.group as u8, &b.path)));
    v
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Commit time (unix seconds).
    pub ts: i64,
    pub subject: String,
}

const LOG_FORMAT: &str = "--pretty=format:%H%x1f%h%x1f%an%x1f%ct%x1f%s";

pub fn parse_log(out: &str) -> Vec<LogEntry> {
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            Some(LogEntry {
                hash: p.next().filter(|h| !h.is_empty())?.to_string(),
                short: p.next()?.to_string(),
                author: p.next()?.to_string(),
                ts: p.next()?.parse().ok()?,
                subject: p.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// A file touched by a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub code: char,
    pub path: String,
    pub old: Option<String>,
}

/// Parse `git diff --name-status -M` lines (`M\tpath`, `R100\told\tnew`).
pub fn parse_name_status(out: &str) -> Vec<CommitFile> {
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let code = p.next()?.chars().next()?;
            let a = p.next()?.to_string();
            match (code, p.next()) {
                ('R' | 'C', Some(b)) => Some(CommitFile { code, path: b.to_string(), old: Some(a) }),
                _ => Some(CommitFile { code, path: a, old: None }),
            }
        })
        .collect()
}

/// Run git in `dir`. Err = the most useful line of its output (stderr first, then stdout: "nothing
/// to commit" goes to stdout).
pub fn git_run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let pick = |b: &[u8]| {
        let s = String::from_utf8_lossy(b).into_owned();
        let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        // Hooks print a lot: the first line says what ran, the last one why it failed.
        lines.iter().find(|l| l.starts_with("error:") || l.starts_with("fatal:")).or(lines.last()).map(|l| l.to_string())
    };
    Err(pick(&out.stderr).or_else(|| pick(&out.stdout)).unwrap_or_else(|| "git falhou".into()))
}

fn with_paths(head: &[&str], paths: &[String]) -> Vec<String> {
    head.iter().map(|s| s.to_string()).chain(std::iter::once("--".to_string())).chain(paths.iter().cloned()).collect()
}

fn run_owned(dir: &Path, args: &[String]) -> Result<String, String> {
    git_run(dir, &args.iter().map(String::as_str).collect::<Vec<_>>())
}

pub fn stage(top: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    run_owned(top, &with_paths(&["add"], paths)).map(|_| ())
}

pub fn unstage(top: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // `restore --staged` needs a HEAD; in a repository without commits drop from the index instead.
    run_owned(top, &with_paths(&["restore", "--staged"], paths))
        .or_else(|_| run_owned(top, &with_paths(&["rm", "--cached", "-r", "-q"], paths)))
        .map(|_| ())
}

/// Throw away the working-tree changes of one entry (untracked files are deleted).
pub fn discard(top: &Path, c: &Change) -> Result<(), String> {
    match c.group {
        Group::Untracked => std::fs::remove_file(top.join(&c.path)).map_err(|e| e.to_string()),
        // Staged + modified again: restore the working tree from the index (keeps the staged part).
        Group::Changes | Group::Staged if c.discardable() => run_owned(top, &with_paths(&["checkout"], std::slice::from_ref(&c.path))).map(|_| ()),
        Group::Changes | Group::Staged => Err("tire do stage antes de descartar".into()),
    }
}

/// `git commit -m msg`; returns git's first output line (`[main abc1234] subject`).
pub fn commit(top: &Path, msg: &str) -> Result<String, String> {
    if msg.trim().is_empty() {
        return Err("escreva uma mensagem de commit".into());
    }
    git_run(top, &["commit", "-m", msg.trim()]).map(|o| o.lines().next().unwrap_or("commit criado").to_string())
}

/// One page of the log, newest first.
pub fn log_page(top: &Path, skip: usize, n: usize) -> Vec<LogEntry> {
    let (s, n) = (format!("--skip={skip}"), format!("--max-count={n}"));
    git_run(top, &["log", &s, &n, LOG_FORMAT]).map(|o| parse_log(&o)).unwrap_or_default()
}

/// Files touched by `hash` (against its first parent; root commits against the empty tree).
pub fn commit_files(top: &Path, hash: &str) -> Vec<CommitFile> {
    let parents = git_run(top, &["rev-list", "--parents", "-n", "1", hash]).map(|o| o.split_whitespace().count().saturating_sub(1)).unwrap_or(0);
    let out = if parents == 0 {
        git_run(top, &["diff-tree", "--root", "--no-commit-id", "--name-status", "-r", "-M", hash])
    } else {
        git_run(top, &["diff", "--name-status", "-M", &format!("{hash}^1"), hash])
    };
    out.map(|o| parse_name_status(&o)).unwrap_or_default()
}

fn show(top: &Path, rev_path: &str) -> String {
    git_run(top, &["show", rev_path]).unwrap_or_default()
}

/// (old, new) texts of a working-tree entry: HEAD against the file on disk.
pub fn working_pair(top: &Path, c: &Change) -> (String, String) {
    let old = if c.code == '?' || (c.code == 'A' && c.group == Group::Staged && c.old.is_none()) {
        String::new()
    } else {
        show(top, &format!("HEAD:{}", c.old.as_deref().unwrap_or(&c.path)))
    };
    let new = std::fs::read(top.join(&c.path)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    (old, new)
}

/// (old, new) texts of a file in a commit.
pub fn commit_pair(top: &Path, hash: &str, f: &CommitFile) -> (String, String) {
    let old = if f.code == 'A' { String::new() } else { show(top, &format!("{hash}^1:{}", f.old.as_deref().unwrap_or(&f.path))) };
    let new = if f.code == 'D' { String::new() } else { show(top, &format!("{hash}:{}", f.path)) };
    // Root commit: no parent, `show` fails and gives "" which is the right "before".
    (old, new)
}

// ── multi-line editing on top of the single-line TextField ──

fn byte_at(f: &TextField, ci: usize) -> usize {
    f.text.char_indices().nth(ci).map_or(f.text.len(), |(b, _)| b)
}

/// Insert text keeping line breaks (CRLF / CR become LF, tabs four spaces, other controls dropped).
pub fn ml_insert(f: &mut TextField, s: &str) {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let s: String = s.chars().flat_map(|c| if c == '\t' { vec![' '; 4] } else { vec![c] }).filter(|c| *c == '\n' || !c.is_control()).collect();
    let b = byte_at(f, f.cursor);
    f.text.insert_str(b, &s);
    f.cursor += s.chars().count();
}

/// (line, column) of the cursor, both 0-based, in chars.
pub fn ml_pos(f: &TextField) -> (usize, usize) {
    let before: String = f.text.chars().take(f.cursor).collect();
    let line = before.matches('\n').count();
    (line, before.rsplit('\n').next().map_or(0, |l| l.chars().count()))
}

fn line_lens(f: &TextField) -> Vec<usize> {
    f.text.split('\n').map(|l| l.chars().count()).collect()
}

/// Char index of (line, col), col clamped to the line.
pub fn ml_index(f: &TextField, line: usize, col: usize) -> usize {
    let lens = line_lens(f);
    let line = line.min(lens.len() - 1);
    lens[..line].iter().map(|n| n + 1).sum::<usize>() + col.min(lens[line])
}

pub fn ml_vertical(f: &mut TextField, dir: isize) {
    let (l, c) = ml_pos(f);
    let nl = (l as isize + dir).clamp(0, line_lens(f).len() as isize - 1) as usize;
    f.cursor = if nl == l { if dir < 0 { 0 } else { f.len() } } else { ml_index(f, nl, c) };
}

pub fn ml_home(f: &mut TextField) {
    let (l, _) = ml_pos(f);
    f.cursor = ml_index(f, l, 0);
}

pub fn ml_end(f: &mut TextField) {
    let (l, _) = ml_pos(f);
    f.cursor = ml_index(f, l, usize::MAX);
}

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
    /// "Emendar o último commit" checkbox.
    Amend,
    /// "Publicar" in the set-upstream prompt.
    PublishYes,
    PublishNo,
    /// Cancel the running fetch / pull / push.
    JobCancel,
    /// The result / error line under the branch row (click dismisses it).
    Note,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Header(Group),
    File(usize),
    Commit(usize),
    CFile(usize),
}

const LOG_PAGE: usize = 60;
const HEAD_H: i32 = 44;
const BRANCH_H: i32 = 36;
/// One line of the result / progress strip under the branch row.
const STRIP_LINE: i32 = 17;
/// Height of the "publish this branch?" prompt.
const PROMPT_H: i32 = 58;
const FILE_H: i32 = 30;
const SEC_H: i32 = 34;
const COMMIT_H: i32 = 52;
const CFILE_H: i32 = 28;
const MSG_H: i32 = 92;
const AMEND_H: i32 = 28;
const COMMIT_AREA_H: i32 = 12 + 22 + MSG_H + 8 + AMEND_H + 34 + 12;
const LINE: i32 = 18;

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
        };
        p.refresh();
        p
    }

    pub fn refresh(&mut self) {
        self.top = git::toplevel(&self.root);
        self.changes = match &self.top {
            Some(t) => git_run(t, &["status", "--porcelain=v1", "-z", "-uall"]).map(|o| parse_status_z(&o)).unwrap_or_default(),
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

    fn note_visible(&self) -> bool {
        self.sync_note.as_ref().is_some_and(|(_, ok, t)| !*ok || t.elapsed().as_secs() < 8)
    }

    /// Height of the strip under the branch row: the set-upstream prompt, the progress of a
    /// running command, or the last result.
    pub fn strip_h(&self) -> i32 {
        if self.publish.is_some() {
            PROMPT_H
        } else if self.job.is_some() {
            STRIP_LINE + 10
        } else if self.note_visible() {
            self.note_lines.max(1) * STRIP_LINE + 10
        } else {
            0
        }
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

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn mtime(p: &Path) -> u128 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

/// Longest prefix of `text` (plus an ellipsis) that fits `max` px.
fn fit(tx: &mut TextCtx, text: &str, max: i32, bold: bool) -> String {
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
fn wrap_lines(tx: &mut TextCtx, text: &str, width: i32, max: usize) -> Vec<String> {
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

fn split_name(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[i + 1..], &path[..i]),
        None => (path, ""),
    }
}

/// Narrowest the panel may be dragged to.
const GP_MIN_W: i32 = 280;
/// Half-width of the grab band around the panel's left edge.
const GP_GRAB: i32 = 4;

/// Panel width for a requested `want` px (a dragged value) inside a window `win` px wide:
/// at least `GP_MIN_W` (less in tiny windows) and at most 60% of the window.
pub fn clamp_panel_width(want: i32, win: i32) -> i32 {
    let lo = GP_MIN_W.min(win / 2);
    want.clamp(lo, (win * 6 / 10).max(lo))
}

/// Automatic width: 30% of the window, 340..440 px.
fn auto_panel_width(win: i32) -> i32 {
    ((win as f32 * 0.30) as i32).clamp(340, 440).min(win / 2)
}

/// Keys that only change modifier state; they never mean anything on their own.
fn is_modifier_key(key: &Key) -> bool {
    matches!(key, Key::Named(NamedKey::Control | NamedKey::Shift | NamedKey::Alt | NamedKey::AltGraph | NamedKey::Super | NamedKey::Meta | NamedKey::Hyper | NamedKey::CapsLock | NamedKey::NumLock | NamedKey::ScrollLock | NamedKey::Fn | NamedKey::FnLock | NamedKey::Symbol | NamedKey::SymbolLock))
}

/// Text color that reads on top of `bg`.
fn on(bg: Rgba) -> Rgba {
    if bg.luminance() > 0.6 { Rgba(0x10, 0x10, 0x10, 0xff) } else { Rgba(0xff, 0xff, 0xff, 0xff) }
}

// ───────────────────────────── App glue ─────────────────────────────

impl App {
    /// Width the panel takes when open (also valid while the panel state is taken out of `self`).
    fn gp_width(&self) -> i32 {
        let w = self.size.0 as i32;
        match self.config.git_panel_width {
            0 => auto_panel_width(w),
            n => clamp_panel_width(n, w),
        }
    }

    /// Is (x, y) on the draggable left edge of the open panel?
    fn gp_on_edge(&self, x: f64, y: f64) -> bool {
        let p = self.gitpanel_rect();
        self.gitpanel.is_some() && Rect::new(p.x - GP_GRAB, p.y, 2 * GP_GRAB, p.h).contains(x, y)
    }

    /// Mouse button released: ends an edge drag and stores the width in `config.json`.
    pub fn gitpanel_release(&mut self) {
        if self.gitpanel.as_mut().is_some_and(|gp| std::mem::take(&mut gp.resizing)) {
            self.config.save();
            self.request_redraw();
        }
    }

    /// Width taken from the editor area: 0 when the panel is closed.
    pub fn gitpanel_w(&self) -> i32 {
        if self.gitpanel.is_none() { 0 } else { self.gp_width() }
    }

    /// Panel rectangle (as if open).
    pub fn gitpanel_rect(&self) -> Rect {
        let w = self.gp_width();
        let (ww, hh) = (self.size.0 as i32, self.size.1 as i32);
        Rect::new(ww - w, crate::app::TOP_H, w, (hh - crate::app::TOP_H - crate::app::STATUS_H).max(0))
    }

    /// Ctrl+Shift+G / palette: open, or focus, or close when it already has the focus.
    pub fn toggle_git_panel(&mut self) {
        match self.gitpanel.as_mut() {
            None => self.open_git_panel(),
            Some(gp) if gp.focused => self.close_git_panel(),
            Some(gp) => gp.focused = true,
        }
    }

    /// Bottom bar button: plain open / close.
    pub fn git_button(&mut self) {
        if self.gitpanel.is_some() {
            self.close_git_panel();
        } else {
            self.open_git_panel();
        }
    }

    pub fn open_git_panel(&mut self) {
        self.refresh_git_status();
        self.gitpanel = Some(Box::new(GitPanel::new(&self.root)));
    }

    pub fn close_git_panel(&mut self) {
        self.gitpanel = None;
        self.focus = crate::app::Focus::Editor;
    }

    fn sync_docs_with_head(&mut self) {
        for d in &mut self.docs {
            if let Some(p) = d.path().map(Path::to_path_buf) {
                d.git_base = git::head_text(&p);
                d.git_stale = true;
            }
        }
    }

    /// Re-read status / history when the index, HEAD or the working tree moved under us.
    fn gp_sync(&mut self, gp: &mut GitPanel) {
        let mut h = DefaultHasher::new();
        let mut acc = 0u64;
        for (p, b) in &self.git_status {
            let mut e = DefaultHasher::new();
            (p, b).hash(&mut e);
            acc ^= e.finish();
        }
        acc.hash(&mut h);
        self.root.hash(&mut h);
        let gitdir = gp.top.clone().unwrap_or_else(|| self.root.clone()).join(".git");
        mtime(&gitdir.join("index")).hash(&mut h);
        let sig = h.finish();
        let hist = mtime(&gitdir.join("logs").join("HEAD")) as u64 ^ (mtime(&gitdir.join("HEAD")) as u64).rotate_left(7);
        if gp.root != self.root {
            gp.root = self.root.clone();
            gp.reset_history();
        }
        if sig != gp.sig {
            gp.sig = sig;
            gp.refresh();
        }
        if hist != gp.hist_sig {
            gp.hist_sig = hist;
            gp.reset_history();
        }
    }

    // ───────────── actions ─────────────

    fn gp_after_change(&mut self, gp: &mut GitPanel) {
        self.refresh_git_status();
        gp.refresh();
    }

    fn gp_toggle_stage(&mut self, gp: &mut GitPanel, i: usize) {
        let (Some(top), Some(c)) = (gp.top.clone(), gp.changes.get(i).cloned()) else { return };
        let mut paths = vec![c.path.clone()];
        if c.check_state() == CheckState::On {
            paths.extend(c.old.clone());
        }
        // Fully staged -> unstage; unstaged or staged-with-later-edits -> stage everything.
        let r = if c.check_state() == CheckState::On { unstage(&top, &paths) } else { stage(&top, &paths) };
        if let Err(e) = r {
            gp.say(format!("git: {e}"), false);
        }
        self.gp_after_change(gp);
    }

    fn gp_section(&mut self, gp: &mut GitPanel, g: Group) {
        let Some(top) = gp.top.clone() else { return };
        let paths = gp.group_paths(g);
        let r = if g == Group::Staged { unstage(&top, &paths) } else { stage(&top, &paths) };
        if let Err(e) = r {
            gp.say(format!("git: {e}"), false);
        }
        self.gp_after_change(gp);
    }

    fn gp_discard(&mut self, gp: &mut GitPanel, i: usize) {
        let (Some(top), Some(c)) = (gp.top.clone(), gp.changes.get(i).cloned()) else { return };
        match discard(&top, &c) {
            Ok(()) => {
                let abs = top.join(&c.path);
                if let Some(k) = self.docs.iter().position(|d| d.path() == Some(abs.as_path())) {
                    let _ = self.docs[k].reload_from_disk();
                    self.docs[k].git_stale = true;
                }
                gp.say(format!("{} descartado", c.path), true);
                self.say(format!("{} descartado", c.path));
            }
            Err(e) => gp.say(format!("descartar: {e}"), false),
        }
        self.diff = None;
        self.gp_after_change(gp);
    }

    /// Check / uncheck "Amend": checking loads the last commit's message into the box (the draft
    /// is kept and comes back when unchecking).
    fn gp_toggle_amend(&mut self, gp: &mut GitPanel) {
        let Some(top) = gp.top.clone() else { return };
        if gp.amend {
            gp.amend = false;
            gp.msg = TextField::with(&std::mem::take(&mut gp.amend_draft));
        } else {
            match gitsync::last_message(&top) {
                Some(m) => {
                    gp.amend_draft = std::mem::take(&mut gp.msg.text);
                    gp.msg = TextField::with(&m);
                    gp.amend = true;
                    gp.msg_focus = true;
                }
                None => return gp.say("Nenhum commit para emendar", false),
            }
        }
        gp.msg_top = 0;
    }

    fn gp_amend(&mut self, gp: &mut GitPanel, top: &Path) {
        // The commit being rewritten is already on the upstream when nothing is ahead of it.
        let published = gp.sync.upstream.is_some() && gp.sync.ahead == 0;
        match gitsync::commit_amend(top, &gp.msg.text) {
            Ok(line) => {
                gp.amend = false;
                gp.msg = TextField::with(&std::mem::take(&mut gp.amend_draft));
                gp.msg_top = 0;
                let extra = if published { " (já publicado: o push exige force)" } else { "" };
                gp.say(format!("Commit emendado{extra}"), true);
                self.say(format!("amend: {line}"));
                self.sync_docs_with_head();
                self.diff = None;
                gp.reset_history();
            }
            Err(e) => {
                gp.say(format!("git commit --amend: {e}"), false);
                self.say(format!("git commit --amend: {e}"));
            }
        }
        self.gp_after_change(gp);
    }

    fn gp_commit(&mut self, gp: &mut GitPanel) {
        let Some(top) = gp.top.clone() else { return };
        if gp.msg.text.trim().is_empty() {
            gp.say("Escreva uma mensagem de commit", false);
            gp.msg_focus = true;
            return;
        }
        if gp.amend {
            return self.gp_amend(gp, &top);
        }
        if gp.staged() == 0 {
            if gp.changes.is_empty() {
                return gp.say("Nada para commitar", false);
            }
            if !gp.include_all {
                return gp.say("Nada no stage: marque arquivos ou ative \"incluir tudo\"", false);
            }
            if let Err(e) = git_run(&top, &["add", "-A"]) {
                return gp.say(format!("git add: {e}"), false);
            }
        }
        match commit(&top, &gp.msg.text) {
            Ok(line) => {
                gp.msg = TextField::default();
                gp.msg_top = 0;
                gp.say(line.clone(), true);
                self.say(format!("commit: {line}"));
                self.sync_docs_with_head();
                self.diff = None;
                gp.reset_history();
            }
            Err(e) => {
                gp.say(format!("git commit: {e}"), false);
                self.say(format!("git commit: {e}"));
            }
        }
        self.gp_after_change(gp);
    }

    // ───────────── remote: fetch / pull / push ─────────────

    fn gp_start(&mut self, gp: &mut GitPanel, op: Op) {
        let Some(top) = gp.top.clone() else { return };
        if gp.job.is_some() {
            return gp.say("Aguarde: já há uma operação em andamento", false);
        }
        gp.sync_note = None;
        gp.publish = None;
        gp.job = Some(gitsync::spawn(&top, op, self.wake()));
        gp.spun = Instant::now();
    }

    fn gp_pull(&mut self, gp: &mut GitPanel) {
        if gp.top.is_some() && gp.sync.upstream.is_none() {
            let b = self.branch.clone().unwrap_or_default();
            return self.gp_note(gp, format!("O branch “{b}” não tem upstream: use Push para publicá-lo antes"), false);
        }
        self.gp_start(gp, Op::Pull);
    }

    fn gp_push(&mut self, gp: &mut GitPanel) {
        if gp.sync.upstream.is_none() {
            return self.gp_ask_publish(gp);
        }
        self.gp_start(gp, Op::Push);
    }

    /// Branch without upstream: ask before publishing it (`push --set-upstream`).
    fn gp_ask_publish(&mut self, gp: &mut GitPanel) {
        let Some(top) = gp.top.clone() else { return };
        let Some(branch) = self.branch.clone().filter(|b| b != "HEAD") else {
            return self.gp_note(gp, "HEAD solto (sem branch): crie um branch antes de enviar".into(), false);
        };
        match gitsync::default_remote(&top, &branch) {
            Some(remote) => {
                gp.sync_note = None;
                gp.publish = Some((remote, branch));
            }
            None => self.gp_note(gp, "Nenhum remote configurado (git remote add origin <url>)".into(), false),
        }
    }

    fn gp_note(&mut self, gp: &mut GitPanel, text: String, ok: bool) {
        self.say(&text);
        gp.sync_note = Some((text, ok, Instant::now()));
    }

    fn gp_job_done(&mut self, gp: &mut GitPanel, op: Op, res: Result<String, Fail>) {
        match res {
            Ok(msg) => self.gp_note(gp, msg, true),
            Err(f) if f.kind == FailKind::NoUpstream && op == Op::Push => return self.gp_ask_publish(gp),
            Err(f) => self.gp_note(gp, format!("{}: {}", op.label(), f.text), false),
        }
        // A pull moves HEAD and files; everything the panel shows may have changed.
        self.sync_docs_with_head();
        self.refresh_tree();
        gp.reset_history();
        self.gp_after_change(gp);
    }

    /// Per-event hook (`App::tick`): collect the finished command and keep the elapsed counter alive.
    pub fn gitpanel_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some(mut gp) = self.gitpanel.take() else { return };
        if let Some(job) = gp.job.as_mut() {
            let before = job.progress.len();
            match job.poll() {
                Some(res) => {
                    let op = job.op.clone();
                    gp.job = None;
                    self.gp_job_done(&mut gp, op, res);
                    self.request_redraw();
                }
                None => {
                    let due = gp.spun + Duration::from_millis(500);
                    if job.progress.len() != before || due <= now {
                        gp.spun = now;
                        self.request_redraw();
                    }
                    want(gp.spun + Duration::from_millis(500));
                }
            }
        }
        if gp.closed {
            self.focus = crate::app::Focus::Editor;
        } else {
            self.gitpanel = Some(gp);
        }
    }

    fn gp_open_diff_change(&mut self, gp: &mut GitPanel, i: usize) {
        let (Some(top), Some(c)) = (gp.top.clone(), gp.changes.get(i).cloned()) else { return };
        let big = std::fs::metadata(top.join(&c.path)).map_or(false, |m| m.len() > 3_000_000);
        if big {
            return gp.say("Arquivo grande demais para o diff lado a lado", false);
        }
        let name = split_name(&c.path).0.to_string();
        // Fully staged files open on the staged side (their hunks can be unstaged); the rest on the
        // not-staged side (their hunks can be staged). A staged + modified file offers both.
        let side = if c.group == Group::Staged && c.also.is_none() { Side::Staged } else { Side::Unstaged };
        self.diff = DiffView::staging_auto(&top, &c.path, c.old.as_deref(), side, &name);
        if self.diff.is_none() {
            gp.say("Sem diferenças de conteúdo para mostrar", false);
        }
    }

    fn gp_open_diff_commit(&mut self, gp: &mut GitPanel, j: usize) {
        let (Some(top), Some(ci)) = (gp.top.clone(), gp.open_commit) else { return };
        let (Some(f), Some(c)) = (gp.files.get(j).cloned(), gp.commits.get(ci).cloned()) else { return };
        let (old, new) = commit_pair(&top, &c.hash, &f);
        let rows = gitx::side_by_side(&old, &new);
        let name = split_name(&f.path).0.to_string();
        self.diff = Some(DiffView::plain(format!("{name} — {}  {}", c.short, c.subject), format!("{}^", c.short), c.short.clone(), rows));
        gp.open_file = Some(j);
    }

    fn gp_toggle_commit(&mut self, gp: &mut GitPanel, i: usize) {
        if gp.open_commit == Some(i) {
            gp.open_commit = None;
            gp.files.clear();
            gp.open_file = None;
            return;
        }
        let (Some(top), Some(c)) = (gp.top.clone(), gp.commits.get(i).cloned()) else { return };
        gp.files = commit_files(&top, &c.hash);
        gp.open_commit = Some(i);
        gp.open_file = None;
        // Another commit may have been collapsed above this one: re-find it and bring the commit
        // together with its files into view.
        if let Some(k) = gp.rows().iter().position(|r| *r == Row::Commit(i)) {
            gp.cursor = k;
            let view = self.gitpanel_rect().h - gp.chrome_h();
            gp.reveal_span(k, k + gp.files.len(), view);
        }
        if !gp.files.is_empty() {
            self.gp_open_diff_commit(gp, 0);
        } else {
            self.diff = None;
        }
    }

    /// Activate a row (Enter / click on the body of a row).
    fn gp_activate(&mut self, gp: &mut GitPanel, r: Row) {
        match r {
            Row::File(i) => self.gp_open_diff_change(gp, i),
            Row::Commit(i) => self.gp_toggle_commit(gp, i),
            Row::CFile(j) => self.gp_open_diff_commit(gp, j),
            Row::Header(_) => {}
        }
    }

    // ───────────── input ─────────────

    pub fn gitpanel_key(&mut self, ev: &KeyIn) -> bool {
        let Some(mut gp) = self.gitpanel.take() else { return false };
        let consumed = if gp.focused { self.gp_key(&mut gp, ev) } else { false };
        if gp.closed {
            self.focus = crate::app::Focus::Editor;
        } else {
            self.gitpanel = Some(gp);
        }
        consumed
    }

    /// A key the panel does not use: hand the focus back and report "not consumed" so the
    /// global shortcut handler sees it.
    fn gp_pass(gp: &mut GitPanel) -> bool {
        gp.focused = false;
        gp.msg_focus = false;
        false
    }

    fn gp_key(&mut self, gp: &mut GitPanel, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let key = &ev.key;
        let entered = matches!(key, Key::Named(NamedKey::Enter));
        if entered && ctrl && gp.tab == Tab::Changes {
            self.gp_commit(gp);
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if gp.confirm.take().is_some() || gp.publish.take().is_some() {
            } else if gp.msg_focus && gp.tab == Tab::Changes && self.diff.is_none() {
                gp.msg_focus = false;
            } else if self.diff.is_some() {
                self.diff = None;
            } else {
                gp.closed = true;
            }
            return true;
        }
        if is_modifier_key(key) {
            return true;
        }
        // Anything bound to a command (Ctrl+P, Ctrl+Shift+G, ...) keeps working.
        let bound = (ctrl || alt) && self.keymap.lookup(ctrl, shift, alt, key).is_some();
        if bound {
            return Self::gp_pass(gp);
        }
        if gp.msg_focus && gp.tab == Tab::Changes {
            let f = &mut gp.msg;
            match key {
                Key::Named(NamedKey::Enter) => ml_insert(f, "\n"),
                Key::Named(NamedKey::Backspace) => {
                    if ctrl {
                        f.delete_word();
                    } else {
                        f.backspace();
                    }
                }
                Key::Named(NamedKey::Delete) => {
                    f.delete();
                }
                Key::Named(NamedKey::ArrowLeft) => f.left(),
                Key::Named(NamedKey::ArrowRight) => f.right(),
                Key::Named(NamedKey::ArrowUp) => ml_vertical(f, -1),
                Key::Named(NamedKey::ArrowDown) => ml_vertical(f, 1),
                Key::Named(NamedKey::Home) => ml_home(f),
                Key::Named(NamedKey::End) => ml_end(f),
                Key::Named(NamedKey::Tab) => gp.msg_focus = false,
                Key::Character(c) if ctrl && !alt && c.to_lowercase() == "v" => {
                    let t = self.clip.get();
                    ml_insert(&mut gp.msg, &t);
                }
                // Any other Ctrl/Alt combo has no meaning here: let the global shortcut run.
                _ if ctrl || alt => return Self::gp_pass(gp),
                _ => {
                    if let Some(t) = &ev.text {
                        ml_insert(f, t);
                    }
                }
            }
            return true;
        }
        // List mode. Ctrl/Alt + a character key means nothing here (the arrows, Enter, Space,
        // Delete and Tab below keep working with modifiers): let the global shortcut run.
        if (ctrl || alt) && matches!(key, Key::Character(_)) {
            return Self::gp_pass(gp);
        }
        let rows = gp.rows();
        match key {
            Key::Named(NamedKey::ArrowDown) => gp.move_cursor(1),
            Key::Named(NamedKey::ArrowUp) => gp.move_cursor(-1),
            Key::Named(NamedKey::ArrowLeft) => gp.tab = Tab::Changes,
            Key::Named(NamedKey::ArrowRight) => {
                gp.tab = Tab::History;
                if gp.commits.is_empty() {
                    gp.load_more();
                }
            }
            Key::Named(NamedKey::Tab) => {
                if gp.tab == Tab::Changes {
                    gp.msg_focus = true;
                } else {
                    gp.tab = Tab::Changes;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(r) = rows.get(gp.cursor).copied() {
                    self.gp_activate(gp, r);
                }
            }
            Key::Named(NamedKey::Space) => match rows.get(gp.cursor).copied() {
                Some(Row::File(i)) => self.gp_toggle_stage(gp, i),
                Some(Row::Header(g)) => self.gp_section(gp, g),
                Some(r) => self.gp_activate(gp, r),
                None => {}
            },
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if let Some(Row::File(i)) = rows.get(gp.cursor).copied() {
                    if gp.changes[i].discardable() {
                        gp.confirm = Some(i);
                    }
                }
            }
            Key::Named(NamedKey::PageUp | NamedKey::PageDown | NamedKey::Home | NamedKey::End) if self.diff.is_some() => {
                self.diff_key(key);
            }
            Key::Character(c) if matches!(c.as_str(), "n" | "p" | "s" | "q") && self.diff.is_some() => {
                self.diff_key(key);
            }
            Key::Character(c) if c.as_str() == "y" && gp.confirm.is_some() => {
                if let Some(i) = gp.confirm.take() {
                    self.gp_discard(gp, i);
                }
            }
            Key::Character(c) if c.as_str() == "r" => {
                gp.refresh();
                gp.reset_history();
            }
            _ if ctrl || alt => return Self::gp_pass(gp),
            _ => {}
        }
        let view = self.gitpanel_rect().h - gp.chrome_h() - if gp.tab == Tab::Changes { COMMIT_AREA_H } else { 0 };
        gp.reveal(view);
        true
    }

    pub fn gitpanel_mouse(&mut self, x: f64, y: f64) -> bool {
        // A click on a "stage this hunk" button of the open diff.
        if self.diff_click(x, y) {
            return true;
        }
        if self.gp_on_edge(x, y) {
            if let Some(gp) = self.gitpanel.as_mut() {
                gp.resizing = true;
                gp.focused = true;
            }
            return true;
        }
        let Some(mut gp) = self.gitpanel.take() else { return false };
        let p = self.gitpanel_rect();
        let inside = p.contains(x, y);
        if !inside {
            gp.focused = false;
            gp.msg_focus = false;
            gp.confirm = None;
        } else {
            gp.focused = true;
            self.focus = crate::app::Focus::Editor;
            let hit = gp.hit_at(x, y);
            let keep_confirm = matches!(hit, Some(Hit::ConfirmDiscard(_)));
            if !keep_confirm {
                gp.confirm = None;
            }
            if !matches!(hit, Some(Hit::Msg)) {
                gp.msg_focus = false;
            }
            let rows = gp.rows();
            match hit {
                Some(Hit::Tab(t)) => {
                    gp.tab = t;
                    gp.cursor = 0;
                    if t == Tab::History && gp.commits.is_empty() {
                        gp.load_more();
                    }
                }
                Some(Hit::Refresh) => {
                    gp.refresh();
                    gp.reset_history();
                    self.refresh_git_status();
                }
                Some(Hit::Close) => gp.closed = true,
                Some(Hit::Check(i)) => self.gp_toggle_stage(&mut gp, i),
                Some(Hit::Discard(i)) => gp.confirm = Some(i),
                Some(Hit::ConfirmDiscard(i)) => self.gp_discard(&mut gp, i),
                Some(Hit::Row(i)) => {
                    if let Some(k) = rows.iter().position(|r| *r == Row::File(i)) {
                        gp.cursor = k;
                    }
                    self.gp_open_diff_change(&mut gp, i);
                }
                Some(Hit::Section(g)) => self.gp_section(&mut gp, g),
                Some(Hit::Msg) => {
                    gp.msg_focus = true;
                    let b = gp.msg_box;
                    let line = gp.msg_top + ((y as i32 - b.y - 8).max(0) / LINE) as usize;
                    let text_line = gp.msg.text.split('\n').nth(line.min(gp.msg.text.split('\n').count() - 1)).unwrap_or("").to_string();
                    let target = x as i32 - (b.x + 12) + gp.msg_hscroll;
                    let mut col = 0;
                    for n in 1..=text_line.chars().count() {
                        let pre: String = text_line.chars().take(n).collect();
                        let w = self.tx.ui_width(&pre, false);
                        let prev: String = text_line.chars().take(n - 1).collect();
                        let wp = self.tx.ui_width(&prev, false);
                        if target >= (w + wp) / 2 {
                            col = n;
                        } else {
                            break;
                        }
                    }
                    gp.msg.cursor = ml_index(&gp.msg, line, col);
                }
                Some(Hit::Opt) => gp.include_all = !gp.include_all,
                Some(Hit::Amend) => self.gp_toggle_amend(&mut gp),
                Some(Hit::Fetch) => self.gp_start(&mut gp, Op::Fetch),
                Some(Hit::Pull) => self.gp_pull(&mut gp),
                Some(Hit::Push) => self.gp_push(&mut gp),
                Some(Hit::PublishYes) => {
                    if let Some((remote, branch)) = gp.publish.take() {
                        self.gp_start(&mut gp, Op::Publish { remote, branch });
                    }
                }
                Some(Hit::PublishNo) => gp.publish = None,
                Some(Hit::JobCancel) => {
                    if let Some(j) = gp.job.as_mut() {
                        j.cancel();
                    }
                }
                Some(Hit::Note) => gp.sync_note = None,
                Some(Hit::CommitBtn) => self.gp_commit(&mut gp),
                Some(Hit::Commit(i)) => {
                    if let Some(k) = rows.iter().position(|r| *r == Row::Commit(i)) {
                        gp.cursor = k;
                    }
                    self.gp_toggle_commit(&mut gp, i);
                }
                Some(Hit::CFile(j)) => {
                    if let Some(k) = rows.iter().position(|r| *r == Row::CFile(j)) {
                        gp.cursor = k;
                    }
                    self.gp_open_diff_commit(&mut gp, j);
                }
                None => {}
            }
        }
        if gp.closed {
            self.focus = crate::app::Focus::Editor;
        } else {
            self.gitpanel = Some(gp);
        }
        inside
    }

    pub fn gitpanel_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let p = self.gitpanel_rect();
        let Some(gp) = self.gitpanel.as_mut() else { return false };
        if !p.contains(x, y) {
            return false;
        }
        let px = match delta {
            MouseScrollDelta::LineDelta(_, dy) => -(dy as f64) * 3.0 * FILE_H as f64,
            MouseScrollDelta::PixelDelta(d) => -d.y,
        };
        let k = if gp.tab == Tab::Changes { 0 } else { 1 };
        gp.scroll[k] = (gp.scroll[k] + px).clamp(0.0, gp.max_scroll[k].max(if k == 1 && !gp.log_done { f64::MAX } else { 0.0 }));
        true
    }

    /// Pointer moved: true when the panel needs a redraw (hover changed).
    pub fn gitpanel_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let p = self.gitpanel_rect();
        let win = self.size.0 as i32;
        let Some(gp) = self.gitpanel.as_mut() else { return false };
        if gp.resizing {
            // The panel is docked on the right, so its width is the distance to the window edge.
            let w = clamp_panel_width(win - x as i32, win);
            if self.config.git_panel_width != w {
                self.config.git_panel_width = w;
                return true;
            }
            return false;
        }
        let h = if p.contains(x, y) { gp.hit_at(x, y) } else { None };
        let changed = h != gp.hover;
        gp.hover = h;
        changed
    }

    /// Cursor over the panel: hand on clickable things, I-beam in the message box.
    pub fn gitpanel_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let gp = self.gitpanel.as_ref()?;
        if gp.resizing || self.gp_on_edge(x, y) {
            return Some(CursorIcon::ColResize);
        }
        if !self.gitpanel_rect().contains(x, y) {
            return None;
        }
        Some(match gp.hit_at(x, y) {
            Some(Hit::Msg) => CursorIcon::Text,
            Some(_) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        })
    }

    // ───────────── drawing ─────────────

    fn hv(&self, r: Rect) -> bool {
        self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1)
    }

    fn gp_icon_btn(&mut self, cv: &mut Canvas, gp: &mut GitPanel, r: Rect, icon: &str, hit: Hit) {
        let c = self.theme.chrome;
        if self.hv(r) {
            cv.fill_round(r, 8, c.tab_active_fg.with_alpha(0x1c));
        }
        let tint = if self.hv(r) { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + (r.h - 16) / 2, 16, tint, r);
        gp.hits.push((r, hit));
    }

    /// Small text button (pill) with hover; returns its width.
    fn gp_text_btn(&mut self, cv: &mut Canvas, gp: &mut GitPanel, right: i32, y: i32, label: &str, hit: Hit, tint: Rgba) -> i32 {
        let w = self.tx.ui_width(label, false) + 18;
        let r = Rect::new(right - w, y, w, 24);
        let hov = self.hv(r);
        if hov {
            cv.fill_round(r, 8, tint.with_alpha(0x26));
        }
        self.tx.draw_ui(cv, r.x + 9, r.y + 3, label, tint, r, false);
        gp.hits.push((r, hit));
        w
    }

    pub fn draw_gitpanel(&mut self, cv: &mut Canvas) {
        let Some(mut gp) = self.gitpanel.take() else { return };
        self.gp_sync(&mut gp);
        gp.hits.clear();
        let p = self.gitpanel_rect();
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(p, c.sidebar_bg);
        cv.fill(Rect::new(p.x, p.y, 1, p.h), c.border);
        // Resize handle: the left edge lights up under the pointer and while dragging.
        let (mx, my) = (self.mouse.0, self.mouse.1);
        if gp.resizing || (self.overlay.is_none() && Rect::new(p.x - GP_GRAB, p.y, 2 * GP_GRAB, p.h).contains(mx, my)) {
            cv.fill(Rect::new(p.x - 1, p.y, 3, p.h), c.accent.with_alpha(if gp.resizing { 0xc0 } else { 0x70 }));
        }

        // Header: tabs on the left, refresh / close on the right.
        let mut x = p.x + 12;
        for (tab, label) in [(Tab::Changes, "Alterações"), (Tab::History, "Histórico")] {
            let n = gp.changes.len();
            let text = if tab == Tab::Changes && n > 0 { format!("{label}  {n}") } else { label.to_string() };
            let active = gp.tab == tab;
            let w = self.tx.ui_width(&text, active) + 24;
            let r = Rect::new(x, p.y + 8, w, 28);
            if active {
                cv.fill_round(r, 8, strong.with_alpha(0x1e));
            } else if self.hv(r) {
                cv.fill_round(r, 8, strong.with_alpha(0x12));
            }
            let col = if active || self.hv(r) { strong } else { dim };
            self.tx.draw_ui(cv, r.x + 12, r.y + 5, &text, col, r, active);
            gp.hits.push((r, Hit::Tab(tab)));
            x += w + 4;
        }
        self.gp_icon_btn(cv, &mut gp, Rect::new(p.right() - 12 - 28, p.y + 8, 28, 28), "ui/close", Hit::Close);
        self.gp_icon_btn(cv, &mut gp, Rect::new(p.right() - 12 - 28 - 32, p.y + 8, 28, 28), "ui/rotate_cw", Hit::Refresh);

        // Branch line: name on the left, fetch / pull / push on the right.
        let by = p.y + HEAD_H;
        let mut right = p.right() - 10;
        if gp.top.is_some() {
            let busy = gp.job.is_some();
            let (ahead, behind, has_up) = (gp.sync.ahead, gp.sync.behind, gp.sync.upstream.is_some());
            let count = |n: usize| if n > 0 { format!(" {n}") } else { String::new() };
            let push = format!("Push{}", if has_up { count(ahead) } else { String::new() });
            right -= self.gp_sync_btn(cv, &mut gp, right, by + 5, "ui/arrow_up", &push, Hit::Push, !busy && (ahead > 0 || !has_up)) + 4;
            let pull = format!("Pull{}", count(behind));
            right -= self.gp_sync_btn(cv, &mut gp, right, by + 5, "ui/arrow_down", &pull, Hit::Pull, !busy && behind > 0) + 4;
            right -= self.gp_sync_btn(cv, &mut gp, right, by + 5, "ui/cloud_download", "", Hit::Fetch, !busy) + 4;
        }
        self.tx.icon(cv, "ui/git_branch", p.x + 16, by + 10, 14, dim, p);
        let branch = self.branch.clone().unwrap_or_else(|| "sem branch".into());
        let branch = fit(&mut self.tx, &branch, right - (p.x + 36) - 4, false);
        self.tx.draw_ui(cv, p.x + 36, by + 8, &branch, dim, p, false);

        // Strip: set-upstream prompt, progress of the running command, or the last result.
        let strip_h = gp.strip_h();
        if strip_h > 0 && gp.top.is_some() {
            self.gp_draw_strip(cv, &mut gp, Rect::new(p.x + 1, by + BRANCH_H, p.w - 1, strip_h));
        }

        let commit_h = if gp.tab == Tab::Changes && gp.top.is_some() { COMMIT_AREA_H } else { 0 };
        let body = Rect::new(p.x + 1, by + BRANCH_H + strip_h, p.w - 1, (p.h - HEAD_H - BRANCH_H - strip_h - commit_h).max(0));
        if gp.top.is_none() {
            let msg = "Esta pasta não é um repositório git";
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, p.x + (p.w - w) / 2, body.y + 40, msg, dim, body, false);
        } else if gp.tab == Tab::Changes {
            self.gp_draw_changes(cv, &mut gp, body);
            self.gp_draw_commit_area(cv, &mut gp, Rect::new(p.x + 1, body.bottom(), p.w - 1, commit_h));
        } else {
            self.gp_draw_history(cv, &mut gp, body);
        }
        gp.hover = if p.contains(self.mouse.0, self.mouse.1) { gp.hit_at(self.mouse.0, self.mouse.1) } else { None };
        self.gitpanel = Some(gp);
    }

    /// Pill with an icon and an optional label (fetch / pull / push); returns its width.
    fn gp_sync_btn(&mut self, cv: &mut Canvas, gp: &mut GitPanel, right: i32, y: i32, icon: &str, label: &str, hit: Hit, active: bool) -> i32 {
        let c = self.theme.chrome;
        let lw = if label.is_empty() { 0 } else { self.tx.ui_width(label, false) + 5 };
        let w = 12 + 14 + lw + if label.is_empty() { 0 } else { 6 };
        let r = Rect::new(right - w, y, w, 26);
        let hov = self.hv(r);
        if hov {
            cv.fill_round(r, 8, c.tab_active_fg.with_alpha(0x1c));
        }
        let tint = if active { c.accent_text } else if hov { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.icon(cv, icon, r.x + 6, r.y + 6, 14, tint, r);
        if !label.is_empty() {
            self.tx.draw_ui(cv, r.x + 6 + 14 + 5, r.y + 4, label, tint, r, false);
        }
        gp.hits.push((r, hit));
        w
    }

    fn gp_draw_strip(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(Rect::new(area.x, area.y, area.w, area.h), c.input_bg.with_alpha(0x80).over(c.sidebar_bg));
        if let Some((remote, branch)) = gp.publish.clone() {
            let q = format!("Publicar “{branch}” em {remote} e passar a acompanhá-lo (upstream)?");
            let lines = wrap_lines(&mut self.tx, &q, area.w - 32, 2);
            for (i, l) in lines.iter().enumerate() {
                self.tx.draw_ui(cv, area.x + 16, area.y + 6 + i as i32 * STRIP_LINE, l, strong, area, false);
            }
            let y = area.bottom() - 30;
            let w = self.gp_text_btn(cv, gp, area.right() - 10, y, "Publicar", Hit::PublishYes, c.accent_text);
            self.gp_text_btn(cv, gp, area.right() - 10 - w - 4, y, "Cancelar", Hit::PublishNo, dim);
            return;
        }
        if let Some(job) = &gp.job {
            let secs = job.started.elapsed().as_secs();
            let head = format!("{}… {secs}s", job.op.busy());
            let hw = self.tx.ui_width(&head, true);
            self.tx.draw_ui(cv, area.x + 16, area.y + 5, &head, strong, area, true);
            let cancel_w = self.tx.ui_width("Cancelar", false) + 18;
            let room = area.w - 32 - hw - cancel_w - 16;
            let prog = fit(&mut self.tx, &job.progress.clone(), room, false);
            self.tx.draw_ui(cv, area.x + 16 + hw + 10, area.y + 5, &prog, dim, area, false);
            self.gp_text_btn(cv, gp, area.right() - 8, area.y + 2, "Cancelar", Hit::JobCancel, c.error);
            return;
        }
        if let Some((text, ok, _)) = gp.sync_note.clone() {
            let lines = wrap_lines(&mut self.tx, &text, area.w - 32, 3);
            gp.note_lines = lines.len() as i32;
            let col = if ok { c.git_added } else { c.error };
            for (i, l) in lines.iter().enumerate() {
                self.tx.draw_ui(cv, area.x + 16, area.y + 5 + i as i32 * STRIP_LINE, l, col, area, false);
            }
            gp.hits.push((area, Hit::Note));
        }
    }

    fn gp_checkbox(&mut self, cv: &mut Canvas, r: Rect, state: CheckState, hov: bool) {
        let c = self.theme.chrome;
        if state != CheckState::Off {
            cv.fill_round(r, 5, if hov { c.accent.shift(22) } else { c.accent });
            if state == CheckState::On {
                self.tx.icon(cv, "ui/check", r.x + 2, r.y + 2, r.w - 4, c.on_accent, r);
            } else {
                // Staged, but edited again afterwards: a dash instead of the check.
                cv.fill_round(Rect::new(r.x + 4, r.y + r.h / 2 - 1, r.w - 8, 2), 1, c.on_accent);
            }
        } else {
            if hov {
                cv.fill_round(r, 5, c.tab_active_fg.with_alpha(0x1c));
            }
            cv.ring(r, 5, c.tab_inactive_fg.with_alpha(if hov { 0xb0 } else { 0x70 }));
        }
    }

    fn gp_draw_changes(&mut self, cv: &mut Canvas, gp: &mut GitPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let rows = gp.rows();
        if rows.is_empty() {
            let msg = "Nenhuma alteração";
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, body.x + (body.w - w) / 2, body.y + 40, msg, dim, body, false);
            return;
        }
        let total: i32 = rows.iter().map(|r| GitPanel::row_h(*r)).sum::<i32>() + 16;
        gp.max_scroll[0] = (total - body.h).max(0) as f64;
        gp.scroll[0] = gp.scroll[0].clamp(0.0, gp.max_scroll[0]);
        let mut y = body.y - gp.scroll[0] as i32;
        for (k, row) in rows.iter().enumerate() {
            let h = GitPanel::row_h(*row);
            if y + h < body.y || y > body.bottom() {
                y += h;
                continue;
            }
            let clip = body;
            let vis = Rect::new(body.x, y.max(body.y), body.w, (y + h).min(body.bottom()) - y.max(body.y));
            match *row {
                Row::Header(g) => {
                    let n = gp.changes.iter().filter(|c| c.group == g).count();
                    self.tx.draw_ui(cv, body.x + 16, y + 9, &format!("{}  {n}", g.title()), dim, clip, true);
                    let label = if g == Group::Staged { "Remover todos" } else { "Adicionar todos" };
                    let before = gp.hits.len();
                    self.gp_text_btn(cv, gp, body.right() - 10, y + 5, label, Hit::Section(g), c.accent_text);
                    if let Some(last) = gp.hits.get_mut(before) {
                        last.0 = last.0.intersect(&vis);
                    }
                }
                Row::File(i) => {
                    let ch = gp.changes[i].clone();
                    let r = Rect::new(body.x + 6, y, body.w - 12, h);
                    let hov = self.hv(r) && self.hv(vis);
                    let cur = gp.focused && !gp.msg_focus && gp.cursor == k;
                    if cur {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x1e), clip);
                    } else if hov {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x12), clip);
                    }
                    let cb = Rect::new(r.x + 8, y + 7, 16, 16);
                    let cbhit = Rect::new(r.x, y, 34, h);
                    self.gp_checkbox(cv, cb, ch.check_state(), self.hv(cbhit) && self.hv(vis));
                    let (name, dir) = split_name(&ch.path);
                    let icon = self.tx.icons.file_icon(name);
                    self.tx.icon(cv, &icon, r.x + 34, y + 7, 16, dim, clip);
                    let reserved = (if ch.discardable() { 66 } else { 36 }) + if ch.also.is_some() { 16 } else { 0 };
                    let avail = r.w - 58 - reserved;
                    let nm = fit(&mut self.tx, name, avail, false);
                    let nw = self.tx.ui_width(&nm, false);
                    let deleted = ch.code == 'D';
                    self.tx.draw_ui(cv, r.x + 58, y + 6, &nm, if deleted { dim } else { strong }, clip, false);
                    if !dir.is_empty() && avail - nw > 40 {
                        let d = fit(&mut self.tx, dir, avail - nw - 10, false);
                        self.tx.draw_ui_sized(cv, r.x + 58 + nw + 8, y + 8, &d, dim, clip, false, 12.0);
                    }
                    // Rightmost the working-tree state; a staged+modified file also shows the staged
                    // letter just before it.
                    let (state_code, staged_code) = match ch.also {
                        Some(w) => (w, Some(ch.code)),
                        None => (ch.code, None),
                    };
                    let letter = state_code.to_string();
                    let lw = self.tx.ui_width(&letter, true);
                    let lx = r.right() - 14 - lw / 2 - 4;
                    self.tx.draw_ui(cv, lx, y + 6, &letter, code_color(&c, state_code), clip, true);
                    if let Some(sc) = staged_code {
                        let sl = sc.to_string();
                        let sw = self.tx.ui_width(&sl, true);
                        self.tx.draw_ui(cv, lx - sw - 6, y + 6, &sl, code_color(&c, sc), clip, true);
                    }
                    gp.hits.push((Rect::new(r.x + 34, vis.y, r.w - 34, vis.h).intersect(&vis), Hit::Row(i)));
                    gp.hits.push((cbhit.intersect(&vis), Hit::Check(i)));
                    if ch.discardable() {
                        // Make room for the second state letter of a staged+modified row.
                        let dx = if ch.also.is_some() { 16 } else { 0 };
                        if gp.confirm == Some(i) {
                            let label = "Descartar?";
                            let w = self.tx.ui_width(label, false) + 16;
                            let br = Rect::new(r.right() - 34 - dx - w, y + 3, w, 24);
                            let bh = self.hv(br);
                            cv.fill_round_clip(br, 8, c.error.with_alpha(if bh { 0x55 } else { 0x33 }), clip);
                            self.tx.draw_ui(cv, br.x + 8, br.y + 3, label, c.error, br.intersect(&vis), false);
                            gp.hits.push((br.intersect(&vis), Hit::ConfirmDiscard(i)));
                        } else {
                            let br = Rect::new(r.right() - 58 - dx, y + 3, 24, 24);
                            if hov || cur {
                                let bh = self.hv(br);
                                if bh {
                                    cv.fill_round_clip(br, 8, strong.with_alpha(0x22), clip);
                                }
                                self.tx.icon(cv, "ui/undo", br.x + 4, br.y + 4, 16, if bh { strong } else { dim }, vis);
                            }
                            gp.hits.push((br.intersect(&vis), Hit::Discard(i)));
                        }
                    }
                }
                _ => {}
            }
            y += h;
        }
        let (pos, visible) = (gp.scroll[0], body.h as f64);
        self.region_bar(cv, crate::scrollreg::Reg::GitChanges, body, total as f64, visible, pos, 1.0, false);
    }

    /// Message box + options + Commit button, pinned to the bottom of the panel.
    fn gp_draw_commit_area(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(Rect::new(area.x, area.y, area.w, 1), c.border);
        let mut y = area.y + 10;
        // Notice (success / error), fades after a while.
        if let Some((m, ok, t)) = gp.notice.clone() {
            if t.elapsed().as_secs() < 12 {
                let txt = fit(&mut self.tx, &m, area.w - 32, false);
                self.tx.draw_ui(cv, area.x + 16, y + 1, &txt, if ok { c.git_added } else { c.error }, area, false);
            }
        }
        y += 22;
        // Message box.
        let b = Rect::new(area.x + 12, y, area.w - 24, MSG_H);
        gp.msg_box = b;
        cv.fill_round(b, 10, c.input_bg);
        if gp.msg_focus {
            cv.ring(b, 10, c.accent.with_alpha(0xa0));
        } else if self.hv(b) {
            cv.ring(b, 10, strong.with_alpha(0x30));
        }
        gp.hits.push((b, Hit::Msg));
        let inner = Rect::new(b.x + 4, b.y + 4, b.w - 8, b.h - 8);
        let vis_lines = ((MSG_H - 16) / LINE) as usize;
        let lines: Vec<&str> = gp.msg.text.split('\n').collect();
        let (cl, cc) = ml_pos(&gp.msg);
        if cl < gp.msg_top {
            gp.msg_top = cl;
        } else if cl >= gp.msg_top + vis_lines {
            gp.msg_top = cl + 1 - vis_lines;
        }
        let cur_line = lines.get(cl).copied().unwrap_or("");
        let prefix: String = cur_line.chars().take(cc).collect();
        let cx = self.tx.ui_width(&prefix, false);
        let room = inner.w - 16;
        gp.msg_hscroll = (cx - room).max(0);
        if gp.msg.text.is_empty() {
            self.tx.draw_ui(cv, b.x + 12, b.y + 8, "Mensagem do commit (Ctrl+Enter para commitar)", dim, inner, false);
        }
        for (k, l) in lines.iter().enumerate().skip(gp.msg_top).take(vis_lines) {
            let ly = b.y + 8 + (k - gp.msg_top) as i32 * LINE;
            self.tx.draw_ui(cv, b.x + 12 - gp.msg_hscroll, ly, l, strong, inner, false);
        }
        if gp.msg_focus && gp.focused {
            let ly = b.y + 8 + (cl - gp.msg_top) as i32 * LINE;
            cv.fill_clip(Rect::new(b.x + 12 + cx - gp.msg_hscroll, ly + 1, 1, LINE - 2), strong, inner);
        }
        y += MSG_H + 8;

        // Amend checkbox.
        let am = Rect::new(area.x + 12, y, area.w - 24, AMEND_H);
        let ah = self.hv(am);
        if ah {
            cv.fill_round(am, 8, strong.with_alpha(0x12));
        }
        self.gp_checkbox(cv, Rect::new(am.x + 6, y + 6, 16, 16), if gp.amend { CheckState::On } else { CheckState::Off }, ah);
        let txt = fit(&mut self.tx, "Emendar o último commit (amend)", am.w - 36, false);
        self.tx.draw_ui(cv, am.x + 30, y + 5, &txt, if ah || gp.amend { strong } else { dim }, am, false);
        gp.hits.push((am, Hit::Amend));
        y += AMEND_H;

        // Option + Commit button.
        let staged = gp.staged();
        let ready = !gp.msg.text.trim().is_empty() && (gp.amend || staged > 0 || (gp.include_all && !gp.changes.is_empty()));
        let label = if gp.amend { "Amend" } else { "Commit" };
        let bw = self.tx.ui_width(label, true) + 36;
        let btn = Rect::new(area.right() - 12 - bw, y, bw, 34);
        let bh = self.hv(btn);
        let fill = if ready { if bh { c.accent.shift(22) } else { c.accent } } else if bh { c.accent.with_alpha(0x70) } else { c.accent.with_alpha(0x48) };
        cv.fill_round(btn, 10, fill);
        let text_col = if ready { c.on_accent } else { strong.with_alpha(0xc0) };
        self.tx.draw_ui(cv, btn.x + 18, btn.y + 8, label, text_col, btn, true);
        gp.hits.push((btn, Hit::CommitBtn));

        let opt = Rect::new(area.x + 12, y, btn.x - area.x - 20, 34);
        let oh = self.hv(opt);
        if oh {
            cv.fill_round(opt, 8, strong.with_alpha(0x12));
        }
        self.gp_checkbox(cv, Rect::new(opt.x + 6, y + 9, 16, 16), if gp.include_all { CheckState::On } else { CheckState::Off }, oh);
        let txt = "Incluir tudo se nada no stage".to_string();
        let txt = fit(&mut self.tx, &txt, opt.w - 36, false);
        self.tx.draw_ui(cv, opt.x + 30, y + 8, &txt, if oh { strong } else { dim }, opt, false);
        gp.hits.push((opt, Hit::Opt));
    }

    fn gp_draw_history(&mut self, cv: &mut Canvas, gp: &mut GitPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        // Incremental loading: keep a few screens of rows ahead of the scroll position.
        let mut guard = 0;
        while !gp.log_done && guard < 4 && (gp.scroll[1] as i32 + body.h + 600) >= gp.content_h() {
            gp.load_more();
            guard += 1;
        }
        if gp.commits.is_empty() {
            let msg = "Sem commits";
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, body.x + (body.w - w) / 2, body.y + 40, msg, dim, body, false);
            return;
        }
        let total = gp.content_h() + 28;
        gp.max_scroll[1] = (total - body.h).max(0) as f64;
        gp.scroll[1] = gp.scroll[1].clamp(0.0, gp.max_scroll[1]);
        let rows = gp.rows();
        let now = now_secs();
        let mut y = body.y - gp.scroll[1] as i32;
        for (k, row) in rows.iter().enumerate() {
            let h = GitPanel::row_h(*row);
            if y + h < body.y || y > body.bottom() {
                y += h;
                continue;
            }
            let vis = Rect::new(body.x, y.max(body.y), body.w, (y + h).min(body.bottom()) - y.max(body.y));
            let r = Rect::new(body.x + 6, y + 1, body.w - 12, h - 2);
            let hov = self.hv(r) && self.hv(vis);
            let cur = gp.focused && gp.cursor == k;
            match *row {
                Row::Commit(i) => {
                    let e = &gp.commits[i];
                    let open = gp.open_commit == Some(i);
                    if open {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x1a), body);
                    } else if hov || cur {
                        cv.fill_round_clip(r, 8, strong.with_alpha(if cur { 0x1e } else { 0x12 }), body);
                    }
                    self.tx.icon(cv, if open { "ui/chevron_down" } else { "ui/chevron_right" }, r.x + 8, y + 17, 16, dim, vis);
                    let subj = if e.subject.is_empty() { "(sem assunto)".to_string() } else { e.subject.clone() };
                    let subj = fit(&mut self.tx, &subj, r.w - 44, open);
                    self.tx.draw_ui(cv, r.x + 32, y + 7, &subj, strong, vis, open);
                    let meta = format!("{}  ·  {}  ·  {}", e.short, e.author, git::ago((now - e.ts).max(0)));
                    let meta = fit(&mut self.tx, &meta, r.w - 44, false);
                    self.tx.draw_ui_sized(cv, r.x + 32, y + 28, &meta, dim, vis, false, 12.0);
                    gp.hits.push((r.intersect(&vis), Hit::Commit(i)));
                }
                Row::CFile(j) => {
                    let f = gp.files[j].clone();
                    let sel = gp.open_file == Some(j);
                    if sel {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x1e), body);
                    } else if hov || cur {
                        cv.fill_round_clip(r, 8, strong.with_alpha(if cur { 0x1e } else { 0x12 }), body);
                    }
                    let letter = f.code.to_string();
                    self.tx.draw_ui(cv, r.x + 34, y + 5, &letter, code_color(&c, f.code), vis, true);
                    let (name, dir) = split_name(&f.path);
                    let avail = r.w - 76;
                    let nm = fit(&mut self.tx, name, avail, false);
                    let nw = self.tx.ui_width(&nm, false);
                    self.tx.draw_ui(cv, r.x + 56, y + 5, &nm, strong, vis, false);
                    if !dir.is_empty() && avail - nw > 40 {
                        let d = fit(&mut self.tx, dir, avail - nw - 10, false);
                        self.tx.draw_ui_sized(cv, r.x + 56 + nw + 8, y + 7, &d, dim, vis, false, 12.0);
                    }
                    gp.hits.push((r.intersect(&vis), Hit::CFile(j)));
                }
                _ => {}
            }
            y += h;
        }
        let msg = if gp.log_done { format!("{} commits", gp.commits.len()) } else { "Carregando…".to_string() };
        let w = self.tx.ui_width(&msg, false);
        self.tx.draw_ui(cv, body.x + (body.w - w) / 2, y + 4, &msg, dim, body, false);
        let (pos, visible) = (gp.scroll[1], body.h as f64);
        self.region_bar(cv, crate::scrollreg::Reg::GitLog, body, total as f64, visible, pos, 1.0, false);
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-gp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "Ana"], &["config", "commit.gpgsign", "false"]] {
            git_run(&dir, a).unwrap();
        }
        dir
    }

    #[test]
    fn status_z_groups_and_renames() {
        let out = "M  staged.ts\0 M work.ts\0MM both.ts\0?? new.ts\0R  new name.ts\0old name.ts\0A  añadido.ts\0 D gone.ts\0UU conflict.ts\0";
        let v = parse_status_z(out);
        let find = |p: &str, g: Group| v.iter().find(|c| c.path == p && c.group == g).cloned();
        assert_eq!(find("staged.ts", Group::Staged).unwrap().code, 'M');
        assert_eq!(find("work.ts", Group::Changes).unwrap().code, 'M');
        let both = find("both.ts", Group::Staged).unwrap();
        assert_eq!(both.also, Some('M'), "staged and modified again = one entry carrying both states");
        assert!(find("both.ts", Group::Changes).is_none());
        assert_eq!(find("new.ts", Group::Untracked).unwrap().code, '?');
        let r = find("new name.ts", Group::Staged).unwrap();
        assert_eq!((r.code, r.old.as_deref()), ('R', Some("old name.ts")));
        assert_eq!(find("añadido.ts", Group::Staged).unwrap().code, 'A');
        assert_eq!(find("gone.ts", Group::Changes).unwrap().code, 'D');
        assert_eq!(find("conflict.ts", Group::Changes).unwrap().code, 'U');
        // Sorted by group, then path.
        let order: Vec<Group> = v.iter().map(|c| c.group).collect();
        assert!(order.windows(2).all(|w| (w[0] as u8) <= (w[1] as u8)));
        assert!(parse_status_z("").is_empty());
    }

    #[test]
    fn log_and_name_status_parsing() {
        let out = "aaaa\x1faaa\x1fAna\x1f1700000000\x1ffix: a | b\nbbbb\x1fbbb\x1fBeto\x1f1699990000\x1f\nlixo sem separador";
        let v = parse_log(out);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].short.as_str(), v[0].author.as_str(), v[0].ts, v[0].subject.as_str()), ("aaa", "Ana", 1700000000, "fix: a | b"));
        assert_eq!(v[1].subject, "");
        let f = parse_name_status("M\ta.ts\nA\tdir/b.ts\nD\tc.ts\nR087\told.ts\tnew.ts\n");
        assert_eq!(f.len(), 4);
        assert_eq!((f[1].code, f[1].path.as_str()), ('A', "dir/b.ts"));
        assert_eq!((f[3].code, f[3].path.as_str(), f[3].old.as_deref()), ('R', "new.ts", Some("old.ts")));
    }

    #[test]
    fn message_editing_is_multiline() {
        let mut f = TextField::default();
        ml_insert(&mut f, "titulo");
        ml_insert(&mut f, "\n\ncorpo ç\r\nfim\tx");
        assert_eq!(f.text, "titulo\n\ncorpo ç\nfim    x");
        assert_eq!(ml_pos(&f), (3, 8));
        ml_vertical(&mut f, -1);
        assert_eq!(ml_pos(&f), (2, 7), "clamped to the shorter line");
        ml_vertical(&mut f, -1);
        assert_eq!(ml_pos(&f), (1, 0), "short line clamps the column");
        ml_vertical(&mut f, -1);
        assert_eq!(ml_pos(&f), (0, 0));
        ml_vertical(&mut f, -1);
        assert_eq!(f.cursor, 0, "up on the first line goes home");
        ml_end(&mut f);
        assert_eq!(ml_pos(&f), (0, 6));
        ml_vertical(&mut f, 1);
        ml_home(&mut f);
        assert_eq!(ml_pos(&f), (1, 0));
        assert!(f.backspace(), "backspace joins lines");
        assert_eq!(f.text, "titulo\ncorpo ç\nfim    x");
        assert_eq!(ml_index(&f, 9, 99), f.len(), "clamped to the end");
    }

    #[test]
    fn stage_unstage_commit_discard_in_a_real_repo() {
        let dir = tmp_repo("flow");
        std::fs::write(dir.join("a.ts"), "one\n").unwrap();
        stage(&dir, &["a.ts".into()]).unwrap();
        commit(&dir, "primeiro").unwrap();
        assert!(commit(&dir, "   ").is_err());
        assert_eq!(commit(&dir, "vazio").unwrap_err().is_empty(), false, "nothing to commit explains itself");

        std::fs::write(dir.join("a.ts"), "two\n").unwrap();
        std::fs::write(dir.join("n.ts"), "new\n").unwrap();
        let st = |d: &Path| parse_status_z(&git_run(d, &["status", "--porcelain=v1", "-z", "-uall"]).unwrap());
        let v = st(&dir);
        assert!(v.iter().any(|c| c.path == "a.ts" && c.group == Group::Changes));
        assert!(v.iter().any(|c| c.path == "n.ts" && c.group == Group::Untracked));

        stage(&dir, &["a.ts".into(), "n.ts".into()]).unwrap();
        let v = st(&dir);
        assert!(v.iter().all(|c| c.group == Group::Staged), "{v:?}");
        let (old, new) = working_pair(&dir, v.iter().find(|c| c.path == "a.ts").unwrap());
        assert_eq!((old.as_str(), new.as_str()), ("one\n", "two\n"));
        let (old, new) = working_pair(&dir, v.iter().find(|c| c.path == "n.ts").unwrap());
        assert_eq!((old.as_str(), new.as_str()), ("", "new\n"));

        unstage(&dir, &["n.ts".into()]).unwrap();
        let v = st(&dir);
        assert!(v.iter().any(|c| c.path == "n.ts" && c.group == Group::Untracked));
        assert_eq!(discard(&dir, v.iter().find(|c| c.group == Group::Staged).unwrap()).is_err(), true);
        discard(&dir, v.iter().find(|c| c.path == "n.ts").unwrap()).unwrap();
        assert!(!dir.join("n.ts").exists());

        let line = commit(&dir, "segundo\n\ncorpo do commit").unwrap();
        assert!(line.contains("segundo"), "{line}");
        assert!(st(&dir).is_empty());
        let log = log_page(&dir, 0, 10);
        assert_eq!(log.iter().map(|l| l.subject.as_str()).collect::<Vec<_>>(), ["segundo", "primeiro"]);

        std::fs::write(dir.join("a.ts"), "three\n").unwrap();
        let v = st(&dir);
        discard(&dir, &v[0]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), "two\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unstage_in_a_repo_without_commits() {
        let dir = tmp_repo("empty");
        std::fs::write(dir.join("a.ts"), "x\n").unwrap();
        stage(&dir, &["a.ts".into()]).unwrap();
        assert_eq!(parse_status_z(&git_run(&dir, &["status", "--porcelain=v1", "-z"]).unwrap())[0].group, Group::Staged);
        unstage(&dir, &["a.ts".into()]).unwrap();
        assert_eq!(parse_status_z(&git_run(&dir, &["status", "--porcelain=v1", "-z"]).unwrap())[0].group, Group::Untracked);
        assert!(log_page(&dir, 0, 5).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn log_pages_and_commit_files() {
        let dir = tmp_repo("log");
        for i in 0..7 {
            std::fs::write(dir.join("f.ts"), format!("v{i}\n")).unwrap();
            if i == 3 {
                std::fs::write(dir.join("old.ts"), "keep\nme\nplease\nthis\nis\nbig\nenough\n").unwrap();
            }
            git_run(&dir, &["add", "-A"]).unwrap();
            commit(&dir, &format!("c{i}")).unwrap();
        }
        let p1 = log_page(&dir, 0, 3);
        let p2 = log_page(&dir, 3, 3);
        let p3 = log_page(&dir, 6, 3);
        let p4 = log_page(&dir, 9, 3);
        assert_eq!((p1.len(), p2.len(), p3.len(), p4.len()), (3, 3, 1, 0));
        assert_eq!(p1[0].subject, "c6");
        assert_eq!(p3[0].subject, "c0");
        assert_eq!(p1[0].author, "Ana");
        assert!(p1[0].ts > 1_600_000_000);
        // Root commit lists its files against the empty tree.
        let f = commit_files(&dir, &p3[0].hash);
        assert_eq!(f, vec![CommitFile { code: 'A', path: "f.ts".into(), old: None }]);
        let (old, new) = commit_pair(&dir, &p3[0].hash, &f[0]);
        assert_eq!((old.as_str(), new.as_str()), ("", "v0\n"));
        // Regular commit: before / after.
        let f = commit_files(&dir, &p1[0].hash);
        let (old, new) = commit_pair(&dir, &p1[0].hash, &f[0]);
        assert_eq!((old.as_str(), new.as_str()), ("v5\n", "v6\n"));
        // Rename is followed.
        git_run(&dir, &["mv", "old.ts", "renamed.ts"]).unwrap();
        commit(&dir, "rename").unwrap();
        let head = &log_page(&dir, 0, 1)[0];
        let f = commit_files(&dir, &head.hash);
        assert_eq!((f[0].code, f[0].path.as_str(), f[0].old.as_deref()), ('R', "renamed.ts", Some("old.ts")));
        let (old, new) = commit_pair(&dir, &head.hash, &f[0]);
        assert_eq!(old, new);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn panel_rows_cursor_and_state() {
        let dir = tmp_repo("state");
        std::fs::write(dir.join("a.ts"), "1\n").unwrap();
        git_run(&dir, &["add", "-A"]).unwrap();
        commit(&dir, "init").unwrap();
        std::fs::write(dir.join("a.ts"), "2\n").unwrap();
        std::fs::write(dir.join("b.ts"), "x\n").unwrap();
        git_run(&dir, &["add", "b.ts"]).unwrap();
        std::fs::write(dir.join("c.ts"), "x\n").unwrap();
        let mut gp = GitPanel::new(&dir);
        assert!(gp.top.is_some());
        let rows = gp.rows();
        assert_eq!(rows.len(), 6, "3 headers + 3 files: {rows:?}");
        assert!(matches!(rows[0], Row::Header(Group::Staged)));
        assert_eq!(gp.staged(), 1);
        assert_eq!(gp.group_paths(Group::Untracked), vec!["c.ts".to_string()]);
        // The cursor skips headers.
        gp.cursor = 1;
        gp.move_cursor(1);
        assert_eq!(gp.cursor, 3);
        gp.move_cursor(-1);
        assert_eq!(gp.cursor, 1);
        gp.move_cursor(-1);
        assert_eq!(gp.cursor, 1, "does not land on a header");
        // History: loads in pages and tracks the end.
        gp.tab = Tab::History;
        gp.load_more();
        assert_eq!(gp.commits.len(), 1);
        assert!(gp.log_done, "a short page is the last one");
        gp.open_commit = Some(0);
        gp.files = commit_files(gp.top.as_ref().unwrap(), &gp.commits[0].hash);
        assert_eq!(gp.rows(), vec![Row::Commit(0), Row::CFile(0)]);
        gp.reset_history();
        assert_eq!(gp.rows().len(), 1, "reset reloads the first page and closes the expansion");
        // Not a repository.
        let plain = std::env::temp_dir().join(format!("lumen-gp-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        let g2 = GitPanel::new(&plain);
        assert!(g2.top.is_none() && g2.rows().is_empty());
        std::fs::remove_dir_all(&plain).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fit_helpers() {
        assert_eq!(split_name("a/b/c.ts"), ("c.ts", "a/b"));
        assert_eq!(split_name("c.ts"), ("c.ts", ""));
    }

    #[test]
    fn staged_and_modified_is_one_row_with_both_states() {
        let v = parse_status_z("MM both.ts\0AD gone.ts\0M  staged.ts\0 M work.ts\0");
        assert_eq!(v.len(), 4, "one entry per file: {v:?}");
        let both = v.iter().find(|c| c.path == "both.ts").unwrap();
        assert_eq!((both.group, both.code, both.also), (Group::Staged, 'M', Some('M')));
        assert_eq!(both.check_state(), CheckState::Partial);
        assert!(both.discardable());
        let gone = v.iter().find(|c| c.path == "gone.ts").unwrap();
        assert_eq!((gone.code, gone.also), ('A', Some('D')));
        let staged = v.iter().find(|c| c.path == "staged.ts").unwrap();
        assert_eq!((staged.check_state(), staged.discardable()), (CheckState::On, false));
        let work = v.iter().find(|c| c.path == "work.ts").unwrap();
        assert_eq!((work.check_state(), work.discardable()), (CheckState::Off, true));
    }

    #[test]
    fn staged_plus_modified_in_a_real_repo() {
        let dir = tmp_repo("both");
        std::fs::write(dir.join("a.ts"), "1\n").unwrap();
        git_run(&dir, &["add", "-A"]).unwrap();
        commit(&dir, "init").unwrap();
        std::fs::write(dir.join("a.ts"), "2\n").unwrap();
        git_run(&dir, &["add", "a.ts"]).unwrap();
        std::fs::write(dir.join("a.ts"), "3\n").unwrap();
        let mut gp = GitPanel::new(&dir);
        assert_eq!(gp.changes.len(), 1);
        assert_eq!(gp.rows(), vec![Row::Header(Group::Staged), Row::File(0)], "a single row, no entry under Mudanças");
        assert_eq!(gp.changes[0].check_state(), CheckState::Partial);
        // The diff against HEAD shows the on-disk text.
        let (old, new) = working_pair(&dir, &gp.changes[0]);
        assert_eq!((old.as_str(), new.as_str()), ("1\n", "3\n"));
        // Discard throws away the unstaged edit only; the staged version survives.
        discard(&dir, &gp.changes[0].clone()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), "2\n");
        gp.refresh();
        assert_eq!(gp.changes[0].check_state(), CheckState::On);
        assert!(discard(&dir, &gp.changes[0].clone()).is_err(), "fully staged: nothing to discard");
        // Staging a partially staged file stages the rest.
        std::fs::write(dir.join("a.ts"), "4\n").unwrap();
        gp.refresh();
        stage(&dir, &["a.ts".into()]).unwrap();
        gp.refresh();
        assert_eq!(gp.changes[0].check_state(), CheckState::On);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn panel_width_clamps() {
        assert_eq!(clamp_panel_width(500, 1400), 500);
        assert_eq!(clamp_panel_width(100, 1400), 280, "not narrower than the minimum");
        assert_eq!(clamp_panel_width(5000, 1400), 840, "at most 60% of the window");
        assert_eq!(clamp_panel_width(300, 400), 240, "tiny window: 60% of it");
        assert_eq!(clamp_panel_width(10, 400), 200, "tiny window: the minimum shrinks to half");
        assert!(clamp_panel_width(0, 100) >= 1);
        assert_eq!(auto_panel_width(1000), 340);
        assert_eq!(auto_panel_width(2000), 440);
    }

    #[test]
    fn expanded_commit_files_are_scrolled_into_view() {
        let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gp-no-such-dir"));
        gp.tab = Tab::History;
        gp.commits = (0..20).map(|i| LogEntry { hash: format!("h{i}"), short: format!("s{i}"), author: "a".into(), ts: 0, subject: "x".into() }).collect();
        gp.files = (0..4).map(|i| CommitFile { code: 'M', path: format!("f{i}.ts"), old: None }).collect();
        gp.open_commit = Some(10);
        let k = gp.rows().iter().position(|r| *r == Row::Commit(10)).unwrap();
        assert_eq!(k, 10);
        // Viewport 300px, scrolled to the top: commit 10 starts at 520.
        gp.reveal_span(k, k + 4, 300);
        let (top, bottom) = (10 * COMMIT_H, 11 * COMMIT_H + 4 * CFILE_H);
        assert_eq!(gp.scroll[1], (bottom - 300) as f64, "the last file's bottom edge is visible");
        assert!(gp.scroll[1] <= top as f64, "and the commit row itself too");
        // Already visible: untouched.
        let s = gp.scroll[1];
        gp.reveal_span(k, k + 4, 300);
        assert_eq!(gp.scroll[1], s);
        // Scrolled past it: back up to the commit row.
        gp.scroll[1] = 2000.0;
        gp.reveal_span(k, k + 4, 300);
        assert_eq!(gp.scroll[1], top as f64);
        // A span taller than the viewport shows the commit row first.
        gp.scroll[1] = 0.0;
        gp.reveal_span(k, k + 4, 100);
        assert_eq!(gp.scroll[1], top as f64);
    }

    #[test]
    fn strip_and_chrome_heights_follow_the_state() {
        let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gp-no-such-dir"));
        assert_eq!(gp.strip_h(), 0);
        assert_eq!(gp.chrome_h(), HEAD_H + BRANCH_H);
        gp.sync_note = Some(("boom".into(), false, Instant::now()));
        gp.note_lines = 2;
        assert_eq!(gp.strip_h(), 2 * STRIP_LINE + 10, "errors stay and grow the strip with their lines");
        gp.sync_note = Some(("ok".into(), true, Instant::now() - Duration::from_secs(30)));
        assert_eq!(gp.strip_h(), 0, "old successes fade out");
        gp.publish = Some(("origin".into(), "main".into()));
        assert_eq!(gp.strip_h(), PROMPT_H);
        assert_eq!(gp.chrome_h(), HEAD_H + BRANCH_H + PROMPT_H);
    }

    #[test]
    fn panel_knows_ahead_and_behind_of_a_real_clone() {
        let base = tmp_repo("syncpanel");
        let (remote, work) = (base.join("remote.git"), base.join("work"));
        git_run(&base, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]).unwrap();
        git_run(&base, &["init", "-q", "-b", "main", work.to_str().unwrap()]).unwrap();
        for a in [&["config", "user.email", "t@t"][..], &["config", "user.name", "T"], &["config", "commit.gpgsign", "false"]] {
            git_run(&work, a).unwrap();
        }
        git_run(&work, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
        std::fs::write(work.join("a.txt"), "1\n").unwrap();
        git_run(&work, &["add", "."]).unwrap();
        git_run(&work, &["commit", "-q", "-m", "one"]).unwrap();
        let gp = GitPanel::new(&work);
        assert_eq!(gp.sync, SyncInfo::default(), "no upstream: counts are hidden and Push asks to publish");
        git_run(&work, &["push", "-q", "-u", "origin", "main"]).unwrap();
        std::fs::write(work.join("a.txt"), "2\n").unwrap();
        git_run(&work, &["commit", "-qam", "two"]).unwrap();
        let gp = GitPanel::new(&work);
        assert_eq!((gp.sync.upstream.as_deref(), gp.sync.ahead, gp.sync.behind), (Some("origin/main"), 1, 0));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn modifier_keys_are_recognised() {
        assert!(is_modifier_key(&Key::Named(NamedKey::Control)));
        assert!(is_modifier_key(&Key::Named(NamedKey::Shift)));
        assert!(!is_modifier_key(&Key::Named(NamedKey::Enter)));
        assert!(!is_modifier_key(&Key::Character("a".into())));
    }
}
