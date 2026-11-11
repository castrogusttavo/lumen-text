//! Top bar: the project name and the current git branch, each opening a dropdown.
//!
//! * project menu: search field, the 3 most recent projects, "Open folder…" (an in-app folder
//!   browser). Typing searches every known project (recents plus the folders next to the current one).
//! * branch menu: search field and every local and remote branch as
//!   `name | author, 3 days ago, last commit subject…`; Enter checks the branch out.
//!
//! The menus are an [`Overlay::TopMenu`]; input, drawing and hit-testing live here.

use crate::{
    app::{App, KeyIn},
    app_overlay::Overlay,
    field::TextField,
    fuzzy::fuzzy,
    git,
    scrollreg::Reg,
    tree::Tree,
    ui::{Canvas, Rect},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
};

pub const TOP_H: i32 = 38;
const ROW: i32 = 30;
const INPUT: i32 = 42;
const MAX_ROWS: usize = 10;
const RECENT_SHOWN: usize = 3;
const RECENT_KEPT: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKind {
    Project,
    Branch,
    /// Folder browser behind "Open folder…".
    Browse,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub author: String,
    /// Commit time, unix seconds.
    pub when: i64,
    pub subject: String,
}

#[derive(Clone, Debug)]
pub enum Entry {
    /// A project folder; the flag tells whether it comes from the recents list.
    Project(PathBuf, bool),
    OpenProject,
    Branch(Branch),
    /// "New branch…" at the top of the branch menu (prefilled with what was typed).
    NewBranch,
    /// Open the folder being browsed as the project.
    UseDir(PathBuf),
    Up(PathBuf),
    Dir(PathBuf),
    /// "Worktrees…" in the branch menu (how many the repository has).
    Worktrees(usize),
}

pub struct TopMenu {
    pub kind: MenuKind,
    /// The button it hangs from.
    pub anchor: Rect,
    pub field: TextField,
    pub entries: Vec<Entry>,
    /// Indices into `entries` (best first) that match the field.
    pub filtered: Vec<usize>,
    pub sel: usize,
    pub scroll: usize,
    /// Folder being browsed (`Browse` only).
    pub dir: PathBuf,
    /// Rows drawn last frame, for the mouse.
    pub rows: Vec<(Rect, usize)>,
    /// Delete buttons of local branches drawn last frame: (button, branch).
    pub trash: Vec<(Rect, String)>,
    pub now: i64,
    /// Branch menu: branches checked out in another worktree, and its folder.
    pub worktrees: Vec<(String, PathBuf)>,
}

// ───────────── pure helpers ─────────────

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// `git for-each-ref` output (see [`list_branches`]) -> branches: local ones first, then remote
/// ones, each group newest commit first (the order git gave). `origin/HEAD` is dropped.
pub fn parse_branches(out: &str) -> Vec<Branch> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in out.lines() {
        let mut it = line.splitn(5, '\t');
        let (Some(refname), Some(head), Some(author), Some(when), Some(subject)) =
            (it.next(), it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let (name, is_remote) = if let Some(n) = refname.strip_prefix("refs/heads/") {
            (n, false)
        } else if let Some(n) = refname.strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue;
            }
            (n, true)
        } else {
            continue;
        };
        let b = Branch {
            name: name.to_string(),
            remote: is_remote,
            current: head.trim() == "*",
            author: author.to_string(),
            when: when.trim().parse().unwrap_or(0),
            subject: subject.to_string(),
        };
        if is_remote { remote.push(b) } else { local.push(b) }
    }
    local.extend(remote);
    local
}

pub fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&out.stderr);
        Err(e.lines().find(|l| !l.trim().is_empty()).unwrap_or("failed").to_string())
    }
}

pub fn list_branches(dir: &Path) -> Vec<Branch> {
    let fmt = "--format=%(refname)%09%(HEAD)%09%(authorname)%09%(committerdate:unix)%09%(subject)";
    run_git(dir, &["for-each-ref", "--sort=-committerdate", fmt, "refs/heads", "refs/remotes"])
        .map(|o| parse_branches(&o))
        .unwrap_or_default()
}

/// `list` with `p` moved to the front (no duplicates), at most `max` entries.
pub fn push_recent(list: &mut Vec<PathBuf>, p: &Path, max: usize) {
    list.retain(|x| x != p);
    list.insert(0, p.to_path_buf());
    list.truncate(max);
}

fn recents_file() -> Option<PathBuf> {
    let base = crate::paths::state_home()?;
    Some(base.join("lumen-text").join("recent-projects.json"))
}

pub fn load_recents() -> Vec<PathBuf> {
    let Some(text) = recents_file().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    v.as_array().into_iter().flatten().filter_map(|s| s.as_str()).map(PathBuf::from).filter(|p| p.is_dir()).collect()
}

pub fn save_recents(list: &[PathBuf]) {
    let Some(p) = recents_file() else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let arr: Vec<String> = list.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let _ = std::fs::write(p, serde_json::to_string(&arr).unwrap_or_default());
}

pub fn remember_project(root: &Path) {
    let mut r = load_recents();
    push_recent(&mut r, root, RECENT_KEPT);
    save_recents(&r);
}

pub(crate) fn project_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
}

pub(crate) fn tilde(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// Sub-folders of `dir` (no hidden ones, no `node_modules`), sorted by name.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir())))
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            !n.starts_with('.') && n != "node_modules" && n != "target"
        })
        .map(|e| e.path())
        .collect();
    v.sort_by_key(|p| project_name(p).to_lowercase());
    v
}

impl TopMenu {
    pub fn new(kind: MenuKind, anchor: Rect, entries: Vec<Entry>, dir: PathBuf) -> Self {
        let mut m = TopMenu {
            kind,
            anchor,
            field: TextField::default(),
            entries,
            filtered: Vec::new(),
            sel: 0,
            scroll: 0,
            dir,
            rows: Vec::new(),
            trash: Vec::new(),
            now: now_secs(),
            worktrees: Vec::new(),
        };
        m.refilter();
        m
    }

    /// Recompute `filtered` from the field.
    pub fn refilter(&mut self) {
        let q = self.field.text.trim().to_lowercase();
        let searching = !q.is_empty();
        let mut scored: Vec<(i32, usize)> = Vec::new();
        let (mut lead, mut trail): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
        let mut shown_recent = 0;
        for (i, e) in self.entries.iter().enumerate() {
            let order = 1000 - i as i32; // keeps the natural order without a query
            let by_name = |name: &str, extra: &[&str]| -> Option<i32> {
                if let Some((s, _)) = fuzzy(&q, name) {
                    Some(s + 500)
                } else if extra.iter().any(|t| t.to_lowercase().contains(&q)) {
                    Some(0)
                } else {
                    None
                }
            };
            match e {
                Entry::UseDir(_) | Entry::Up(_) | Entry::NewBranch | Entry::Worktrees(_) => lead.push(i),
                Entry::OpenProject => trail.push(i),
                Entry::Project(p, recent) => {
                    if !searching {
                        if *recent && shown_recent < RECENT_SHOWN {
                            shown_recent += 1;
                            scored.push((order, i));
                        }
                    } else if let Some(s) = by_name(&project_name(p), &[&p.to_string_lossy()]) {
                        scored.push((s, i));
                    }
                }
                Entry::Branch(b) => {
                    if !searching {
                        scored.push((order, i));
                    } else if let Some(s) = by_name(&b.name, &[&b.author, &b.subject]) {
                        scored.push((s, i));
                    }
                }
                Entry::Dir(p) => {
                    if !searching {
                        scored.push((order, i));
                    } else if let Some(s) = by_name(&project_name(p), &[]) {
                        scored.push((s, i));
                    }
                }
            }
        }
        if searching {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        let first_match = if searching && !scored.is_empty() { lead.len() } else { 0 };
        self.filtered = lead.into_iter().chain(scored.into_iter().map(|s| s.1)).chain(trail).collect();
        self.sel = first_match;
        self.scroll = 0;
    }

    pub fn move_sel(&mut self, d: isize) {
        let n = self.filtered.len() as isize;
        if n == 0 {
            return;
        }
        self.sel = (self.sel as isize + d).rem_euclid(n) as usize;
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + MAX_ROWS {
            self.scroll = self.sel + 1 - MAX_ROWS;
        }
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.filtered.get(self.sel).map(|&i| &self.entries[i])
    }
}

fn fit(tx: &mut crate::ui::TextCtx, text: &str, max: i32) -> String {
    if max <= 0 {
        return String::new();
    }
    let text: String = text.chars().take(240).collect();
    if tx.ui_width(&text, false) <= max {
        return text;
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand: String = chars.iter().collect::<String>() + "…";
        if tx.ui_width(&cand, false) <= max {
            return cand;
        }
    }
    "…".to_string()
}

impl App {
    // ───────────── bar ─────────────

    pub fn draw_topbar(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let bar = self.layout.topbar;
        cv.fill(bar, c.sidebar_bg);
        cv.fill(Rect::new(bar.x, bar.bottom() - 1, bar.w, 1), c.border);
        self.layout.top_btns.clear();
        let open = if let Overlay::TopMenu(m) = &self.overlay { Some(m.kind) } else { None };
        let mut x = bar.x + 10;
        let ty = bar.y + (bar.h - 18) / 2;
        let iy = bar.y + (bar.h - 16) / 2;

        let name = project_name(&self.root);
        let nw = self.tx.ui_width(&name, true);
        let w = 12 + 16 + 8 + nw + 8 + 12 + 10;
        let r = Rect::new(x, bar.y + 4, w, bar.h - 9);
        if matches!(open, Some(MenuKind::Project | MenuKind::Browse)) {
            cv.fill_round(r, 6, c.list_active_bg);
        }
        self.tx.icon(cv, "ui/folder", r.x + 10, iy, 16, c.tab_inactive_fg, bar);
        self.tx.draw_ui(cv, r.x + 10 + 16 + 8, ty, &name, c.tab_active_fg, bar, true);
        self.tx.icon(cv, "ui/chevron_down", r.right() - 22, bar.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, bar);
        self.layout.top_btns.push((r, MenuKind::Project));
        x = r.right() + 6;

        if let Some(b) = self.branch.clone() {
            let bw = self.tx.ui_width(&b, false);
            let w = 12 + 14 + 8 + bw + 8 + 12 + 10;
            let r = Rect::new(x, bar.y + 4, w, bar.h - 9);
            if open == Some(MenuKind::Branch) {
                cv.fill_round(r, 6, c.list_active_bg);
            }
            // A linked worktree shows the worktree icon (the tooltip names its main worktree).
            let (icon, icol) = if self.gitbg.worktree_main.is_some() {
                ("ui/git_worktree", c.accent_text)
            } else {
                ("ui/git_branch", c.tab_inactive_fg)
            };
            self.tx.icon(cv, icon, r.x + 10, bar.y + (bar.h - 14) / 2, 14, icol, bar);
            self.tx.draw_ui(cv, r.x + 10 + 14 + 8, ty, &b, c.tab_inactive_fg, bar, false);
            self.tx.icon(cv, "ui/chevron_down", r.right() - 22, bar.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, bar);
            self.layout.top_btns.push((r, MenuKind::Branch));
        }
    }

    /// Mouse press on the bar. Returns true when it was inside the bar.
    pub fn topbar_click(&mut self, x: f64, y: f64) -> bool {
        if !self.layout.topbar.contains(x, y) {
            return false;
        }
        if let Some((r, kind)) = self.layout.top_btns.iter().find(|(r, _)| r.contains(x, y)).copied() {
            self.open_topmenu(kind, r);
        }
        true
    }

    /// Palette entry points: open the menu hanging from its button.
    pub fn open_topmenu_cmd(&mut self, kind: MenuKind) {
        let r = self.layout.top_btns.iter().find(|(_, k)| *k == kind).map(|(r, _)| *r).unwrap_or(Rect::new(
            10,
            4,
            140,
            TOP_H - 9,
        ));
        self.open_topmenu(kind, r);
    }

    // ───────────── menus ─────────────

    pub fn open_topmenu(&mut self, kind: MenuKind, anchor: Rect) {
        match kind {
            MenuKind::Project => {
                let recents: Vec<PathBuf> = load_recents().into_iter().filter(|p| *p != self.root).collect();
                let mut entries: Vec<Entry> = recents.iter().cloned().map(|p| Entry::Project(p, true)).collect();
                if let Some(parent) = self.root.parent() {
                    for p in subdirs(parent) {
                        if p != self.root && !recents.contains(&p) {
                            entries.push(Entry::Project(p, false));
                        }
                    }
                }
                entries.push(Entry::OpenProject);
                self.overlay = Overlay::TopMenu(Box::new(TopMenu::new(kind, anchor, entries, self.root.clone())));
            }
            MenuKind::Branch => {
                let branches = list_branches(&self.root);
                if branches.is_empty() {
                    return self.say("no branches (is the folder a git repository?)");
                }
                let wts = crate::worktree::list(&self.root);
                let entries = [Entry::NewBranch, Entry::Worktrees(wts.len())]
                    .into_iter()
                    .chain(branches.into_iter().map(Entry::Branch))
                    .collect();
                let mut m = TopMenu::new(kind, anchor, entries, self.root.clone());
                let cur = crate::worktree::containing(&wts, &self.root);
                m.worktrees = wts
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| Some(*i) != cur)
                    .filter_map(|(_, w)| Some((w.branch.clone()?, w.path.clone())))
                    .collect();
                self.overlay = Overlay::TopMenu(Box::new(m));
            }
            MenuKind::Browse => {
                let start = self.root.parent().map_or_else(|| self.root.clone(), Path::to_path_buf);
                self.overlay = Overlay::TopMenu(Box::new(browse_menu(anchor, start)));
            }
        }
    }

    fn menu_rect(&self, m: &TopMenu, shown: usize) -> Rect {
        let w = self.size.0 as i32;
        let width = if m.kind == MenuKind::Branch { (w - 24).clamp(320, 760) } else { 460.min(w - 16) };
        let x = m.anchor.x.min(w - width - 8).max(8);
        let head = if m.kind == MenuKind::Browse { 26 } else { 0 };
        let list_h = if shown == 0 { 40 } else { shown as i32 * ROW + 10 };
        Rect::new(x, m.anchor.bottom() + 4, width, INPUT + head + list_h)
    }

    pub fn draw_topmenu(&mut self, cv: &mut Canvas) {
        let mut m = match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::TopMenu(m) => m,
            o => {
                self.overlay = o;
                return;
            }
        };
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        let shown = m.filtered.len().min(MAX_ROWS);
        let r = self.menu_rect(&m, shown);
        cv.shadow(r, 12);
        cv.card(r, 12, c.widget_bg, c.border);
        // Search field.
        self.tx.icon(cv, "hugeicons/search", r.x + 16, r.y + (INPUT - 16) / 2, 16, c.accent, r);
        let hint = match m.kind {
            MenuKind::Project => "Search projects…",
            MenuKind::Branch => "Search branches…",
            MenuKind::Browse => "Search folders…",
        };
        let (txt, col) = if m.field.text.is_empty() { (hint, dim) } else { (m.field.text.as_str(), fg) };
        if m.field.sel_all {
            let w = self.tx.ui_width(&m.field.text, false);
            cv.fill_round_clip(Rect::new(r.x + 40, r.y + (INPUT - 24) / 2, w + 6, 24), 5, c.accent.with_alpha(0x55), r);
        }
        self.tx.draw_ui(cv, r.x + 42, r.y + (INPUT - 18) / 2, txt, col, r, false);
        let prefix: String = m.field.text.chars().take(m.field.cursor).collect();
        let cx = r.x + 42 + self.tx.ui_width(&prefix, false);
        cv.fill_clip(Rect::new(cx, r.y + (INPUT - 20) / 2, 1, 20), fg, r);
        cv.fill(Rect::new(r.x + 1, r.y + INPUT - 1, r.w - 2, 1), c.border);
        let mut top = r.y + INPUT;
        if m.kind == MenuKind::Browse {
            let path = fit(&mut self.tx, &tilde(&m.dir), r.w - 32);
            self.tx.draw_ui(cv, r.x + 16, top + 4, &path, dim, r, false);
            top += 26;
        }

        let mut rows = Vec::new();
        let mut trash = Vec::new();
        // Mid-glide: rows shift up by a fraction of a row, one more shows at the bottom, all clipped.
        let off = self.region_off(Reg::TopMenu, ROW);
        let lclip = Rect::new(r.x, top, r.w, MAX_ROWS as i32 * ROW + 10);
        for (k, &ei) in m.filtered.iter().enumerate().skip(m.scroll).take(MAX_ROWS + (off > 0) as usize) {
            let y = top + 5 + (k - m.scroll) as i32 * ROW - off;
            let row_full = Rect::new(r.x + 6, y, r.w - 12, ROW);
            let rr = row_full.intersect(&lclip);
            if k == m.sel {
                cv.fill_round_clip(row_full, 8, c.list_active_bg, lclip);
            }
            let ly = y + (ROW - 18) / 2;
            let iy = y + (ROW - 16) / 2;
            match &m.entries[ei] {
                Entry::Project(p, recent) => {
                    self.tx.icon(cv, if *recent { "ui/clock" } else { "ui/folder" }, rr.x + 12, iy, 16, dim, rr);
                    let name = fit(&mut self.tx, &project_name(p), rr.w * 2 / 5);
                    self.tx.draw_ui(cv, rr.x + 38, ly, &name, fg, rr, false);
                    let used = self.tx.ui_width(&name, false);
                    let room = rr.w - 38 - used - 30;
                    let path = fit(&mut self.tx, &tilde(p), room);
                    self.tx.draw_ui(cv, rr.x + 38 + used + 14, ly, &path, dim, rr, false);
                }
                Entry::OpenProject => {
                    self.tx.icon(cv, "ui/folder_open", rr.x + 12, iy, 16, c.accent, rr);
                    self.tx.draw_ui(cv, rr.x + 38, ly, "Open folder…", c.accent_text, rr, false);
                }
                Entry::NewBranch => {
                    self.tx.icon(cv, "ui/git_branch_plus", rr.x + 12, iy, 16, c.accent, rr);
                    let q = m.field.text.trim();
                    let label =
                        if q.is_empty() { "New branch…".to_string() } else { format!("Create branch “{q}”…") };
                    let label = fit(&mut self.tx, &label, rr.w - 60);
                    self.tx.draw_ui(cv, rr.x + 38, ly, &label, c.accent_text, rr, false);
                }
                Entry::UseDir(p) => {
                    self.tx.icon(cv, "ui/check", rr.x + 12, iy, 16, c.accent, rr);
                    let label = format!("Open “{}” as a project", project_name(p));
                    let label = fit(&mut self.tx, &label, rr.w - 60);
                    self.tx.draw_ui(cv, rr.x + 38, ly, &label, c.accent_text, rr, false);
                }
                Entry::Up(_) => {
                    self.tx.icon(cv, "ui/arrow_up", rr.x + 12, iy, 16, dim, rr);
                    self.tx.draw_ui(cv, rr.x + 38, ly, "..", fg, rr, false);
                }
                Entry::Dir(p) => {
                    self.tx.icon(cv, "ui/folder", rr.x + 12, iy, 16, dim, rr);
                    let name = fit(&mut self.tx, &project_name(p), rr.w - 60);
                    self.tx.draw_ui(cv, rr.x + 38, ly, &name, fg, rr, false);
                }
                Entry::Worktrees(n) => {
                    self.tx.icon(cv, "ui/git_worktree", rr.x + 12, iy, 16, c.accent, rr);
                    let label = if *n > 1 { format!("Worktrees… ({n})") } else { "Worktrees…".to_string() };
                    self.tx.draw_ui(cv, rr.x + 38, ly, &label, c.accent_text, rr, false);
                }
                Entry::Branch(b) => {
                    // Checked out in another worktree: choosing it offers to open that worktree.
                    let held = if b.remote { None } else { m.worktrees.iter().find(|(n, _)| *n == b.name) };
                    let icon = match (b.remote, held.is_some()) {
                        (true, _) => "ui/public",
                        (false, true) => "ui/git_worktree",
                        (false, false) => "ui/git_branch",
                    };
                    self.tx.icon(cv, icon, rr.x + 12, iy, 16, dim, rr);
                    let name = fit(&mut self.tx, &b.name, rr.w * 2 / 5);
                    let name_col = if b.current { c.accent_text } else { fg };
                    self.tx.draw_ui(cv, rr.x + 38, ly, &name, name_col, rr, b.current);
                    let used = self.tx.ui_width(&name, b.current);
                    let dx = rr.x + 38 + used + 12;
                    // Local branches other than the current one get a delete button on the
                    // highlighted row (it asks before deleting).
                    let deletable = !b.remote && !b.current && held.is_none();
                    let end = rr.right() - if b.current || (deletable && k == m.sel) { 40 } else { 12 };
                    self.tx.draw_ui(cv, dx, ly, "|", dim.with_alpha(0x70), rr, false);
                    let mut info = format!("{}, {}, {}", b.author, git::ago((m.now - b.when).max(0)), b.subject);
                    if let Some((_, p)) = held {
                        info = format!("worktree {} · {info}", tilde(p));
                    }
                    let info = fit(&mut self.tx, &info, end - (dx + 16));
                    self.tx.draw_ui(cv, dx + 16, ly, &info, dim, rr, false);
                    if b.current {
                        self.tx.icon(cv, "ui/check", rr.right() - 28, iy, 16, c.accent, rr);
                    } else if deletable && k == m.sel {
                        let tr = Rect::new(rr.right() - 34, y + (ROW - 26) / 2, 26, 26);
                        let (mx, my) = self.mouse;
                        let hot = tr.contains(mx, my);
                        if hot {
                            cv.fill_round_clip(tr, 8, c.error.with_alpha(0x2a), lclip);
                        }
                        self.tx.icon(cv, "ui/trash", tr.x + 5, tr.y + 5, 16, if hot { c.error } else { dim }, rr);
                        trash.push((tr.intersect(&lclip), b.name.clone()));
                    }
                }
            }
            rows.push((rr, k));
        }
        if m.filtered.is_empty() {
            self.tx.draw_ui(cv, r.x + 16, top + 10, "No results", dim, r, false);
        }
        m.rows = rows;
        m.trash = trash;
        let (total, pos) = (m.filtered.len() as f64, m.scroll as f64);
        self.overlay = Overlay::TopMenu(m);
        if total > MAX_ROWS as f64 {
            let area = Rect::new(r.x, top, r.w, MAX_ROWS as i32 * ROW + 10);
            self.region_bar(cv, Reg::TopMenu, area, total, MAX_ROWS as f64, pos, ROW as f64, true);
        }
    }

    pub fn topmenu_mouse(&mut self, x: f64, y: f64) -> bool {
        let Overlay::TopMenu(m) = &mut self.overlay else { return false };
        if let Some((_, name)) = m.trash.iter().find(|(r, _)| r.contains(x, y)) {
            let name = name.clone();
            self.overlay = Overlay::None;
            self.git_ask_delete_branch(&name);
            return true;
        }
        if let Some((_, k)) = m.rows.iter().find(|(r, _)| r.contains(x, y)) {
            m.sel = *k;
            self.topmenu_accept();
            return true;
        }
        let shown = m.filtered.len().min(MAX_ROWS);
        let (anchor, kind) = (m.anchor, m.kind);
        let r = {
            let Overlay::TopMenu(m) = &self.overlay else { return true };
            self.menu_rect(m, shown)
        };
        let _ = (anchor, kind);
        if !r.contains(x, y) {
            self.overlay = Overlay::None;
        }
        true
    }

    pub fn topmenu_wheel(&mut self, delta: MouseScrollDelta) {
        let Overlay::TopMenu(m) = &mut self.overlay else { return };
        let d = crate::scroller::wheel_rows_int(delta, ROW as f64);
        let max = m.filtered.len().saturating_sub(MAX_ROWS) as isize;
        m.scroll = (m.scroll as isize + d).clamp(0, max) as usize;
    }

    pub fn topmenu_key(&mut self, ev: &KeyIn) {
        let ctrl = self.mods.control_key();
        let alt = self.mods.alt_key();
        let paste = (ctrl && matches!(&ev.key, Key::Character(c) if c.to_lowercase() == "v")).then(|| self.clip.get());
        let Overlay::TopMenu(m) = &mut self.overlay else { return };
        let mut changed = false;
        if ctrl && matches!(&ev.key, Key::Character(c) if c.eq_ignore_ascii_case("a")) {
            m.field.select_all();
            return self.request_redraw();
        }
        match &ev.key {
            Key::Named(NamedKey::Escape) => {
                self.overlay = Overlay::None;
                return;
            }
            Key::Named(NamedKey::Enter) => return self.topmenu_accept(),
            Key::Named(NamedKey::ArrowDown) => m.move_sel(1),
            Key::Named(NamedKey::ArrowUp) => m.move_sel(-1),
            Key::Named(NamedKey::PageDown) => m.move_sel(MAX_ROWS as isize),
            Key::Named(NamedKey::PageUp) => m.move_sel(-(MAX_ROWS as isize)),
            Key::Named(NamedKey::ArrowLeft) => m.field.left(),
            Key::Named(NamedKey::ArrowRight) => m.field.right(),
            Key::Named(NamedKey::Home) => m.field.home(),
            Key::Named(NamedKey::End) => m.field.end(),
            Key::Named(NamedKey::Backspace) => {
                if m.kind == MenuKind::Browse && m.field.text.is_empty() {
                    if let Some(parent) = m.dir.parent().map(Path::to_path_buf) {
                        let anchor = m.anchor;
                        self.overlay = Overlay::TopMenu(Box::new(browse_menu(anchor, parent)));
                    }
                    return;
                }
                changed = if ctrl { m.field.delete_word() } else { m.field.backspace() };
            }
            Key::Named(NamedKey::Delete) => changed = m.field.delete(),
            _ => {
                if let Some(t) = paste {
                    m.field.insert(&t.replace(['\n', '\r'], " "));
                    changed = true;
                } else if let (false, false, Some(t)) = (ctrl, alt, ev.text.as_deref())
                    && !t.is_empty()
                    && !t.chars().any(char::is_control)
                {
                    m.field.insert(t);
                    changed = true;
                }
            }
        }
        if changed {
            m.refilter();
        }
    }

    fn topmenu_accept(&mut self) {
        let Overlay::TopMenu(m) = std::mem::replace(&mut self.overlay, Overlay::None) else { return };
        let Some(entry) = m.selected().cloned() else {
            self.overlay = Overlay::TopMenu(m);
            return;
        };
        match entry {
            Entry::Project(p, _) | Entry::UseDir(p) => self.switch_project(p),
            Entry::OpenProject => self.open_topmenu(MenuKind::Browse, m.anchor),
            Entry::Up(p) | Entry::Dir(p) => self.overlay = Overlay::TopMenu(Box::new(browse_menu(m.anchor, p))),
            Entry::Branch(b) => match m.worktrees.iter().find(|(n, _)| !b.remote && *n == b.name) {
                // git refuses to check out a branch another worktree holds: offer that worktree.
                Some((_, p)) => {
                    let pick = crate::app_worktree::WtPick::Menu(p.clone());
                    self.git_pick(crate::app_gitpanel::GitPick::Worktree(pick));
                }
                None => self.checkout_branch(b, &m.entries),
            },
            Entry::NewBranch => self.git_new_branch(None, m.field.text.trim()),
            Entry::Worktrees(_) => self.worktrees_open(),
        }
    }

    // ───────────── actions ─────────────

    /// Open `path` as the project. With unsaved tabs or a running debug session it asks first
    /// (Save all / Discard / Cancel) instead of refusing.
    pub fn switch_project(&mut self, path: PathBuf) {
        let path = crate::paths::canonical(&path).unwrap_or(path);
        if !path.is_dir() {
            return self.say(format!("{} is not a folder", path.display()));
        }
        if path == self.root {
            return self.say("that project is already open");
        }
        let names: Vec<String> = self.docs.iter().filter(|d| d.buf.is_dirty()).map(|d| d.name.clone()).collect();
        if !names.is_empty() || self.dbg.is_some() {
            return self.ask_unsaved(crate::app_unsaved::Pending::SwitchProject(path), names, self.dbg.is_some());
        }
        self.switch_project_now(path);
    }

    /// Close everything of the current project and open `path` as the new one (no questions).
    pub fn switch_project_now(&mut self, path: PathBuf) {
        self.save_session();
        remember_project(&self.root);
        while !self.docs.is_empty() {
            self.close_tab(0, true);
        }
        // Tabs of the old project are not "recently closed" in the new one.
        self.closed_tabs.clear();
        self.lsps.clear();
        self.lsp_warned = false;
        self.diff = None;
        self.find = None;
        self.completion = None;
        self.hover = None;
        self.nav_back.clear();
        self.nav_fwd.clear();
        self.file_index = None;
        self.tests.reset();
        self.build.reset();
        self.blame_for = None;
        self.eslint_busy.clear();
        self.git_status.clear();
        self.root = path;
        crate::instance::set_root(&self.root);
        // The new project's `.lumen/` settings replace the old project's.
        self.projcfg_init();
        self.tree = Tree::for_config(self.root.clone(), &self.config);
        self.active = 0;
        self.refresh_git_status();
        self.watcher = crate::watcher::Watcher::new(self.wake()).ok();
        self.watch_dirs();
        remember_project(&self.root);
        self.restore_session();
        self.restore_backups();
        let name = project_name(&self.root);
        self.say(format!("project: {name}"));
        self.request_redraw();
    }

    fn checkout_branch(&mut self, b: Branch, all: &[Entry]) {
        if b.current {
            return self.say(format!("already on {}", b.name));
        }
        // A remote branch whose local twin exists just switches to the twin.
        let twin = if b.remote { b.name.split_once('/').map(|x| x.1.to_string()) } else { None };
        let has_twin = twin
            .as_ref()
            .is_some_and(|t| all.iter().any(|e| matches!(e, Entry::Branch(x) if !x.remote && &x.name == t)));
        let (res, shown) = if b.remote && !has_twin {
            (run_git(&self.root, &["checkout", "--track", &b.name]), b.name.clone())
        } else {
            let n = if has_twin { twin.unwrap_or_default() } else { b.name.clone() };
            (run_git(&self.root, &["checkout", &n]), n)
        };
        match res {
            Ok(_) => {
                self.refresh_git_status();
                for d in &mut self.docs {
                    if let Some(p) = d.path().map(Path::to_path_buf) {
                        d.git_base = git::head_text(&p);
                        d.git_stale = true;
                    }
                }
                self.last_disk_check = std::time::Instant::now() - std::time::Duration::from_secs(10);
                self.say(format!("branch: {shown}"));
            }
            Err(e) => self.say(format!("git checkout: {e}")),
        }
    }
}

/// Width of the short memory readout: chip icon, gap and the level dot.
const RAM_ICON_W: i32 = 14 + 4 + 5;

/// Sum of every "N MB" in the memory text (editor + language servers), in MB.
pub fn memory_mb(text: &str) -> f64 {
    let mut total = 0.0;
    let mut prev: Option<f64> = None;
    for tok in text.split_whitespace() {
        if tok == "MB" {
            total += prev.take().unwrap_or(0.0);
        } else {
            prev = tok.parse::<f64>().ok();
        }
    }
    total
}

/// Buttons of the bottom bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusBtn {
    Sidebar,
    Search,
    Problems,
    Terminal,
    Debug,
    Git,
    /// Memory read-out (tooltip only).
    Ram,
    /// Cursor position, indentation and language: clickable (go to line, pickers).
    Position,
    Indent,
    Language,
    /// The file's encoding: picker to reopen or save it in another one.
    Encoding,
    /// An update is ready or available (left group, only then).
    Update,
    /// A build is running, or the last one failed (left group, only then).
    Build,
    /// The agent of the hidden AI panel finished or wants an answer (left group, only then).
    Ai,
    /// Project settings (`.lumen/config.json`) are in effect (left group, only then).
    Project,
    /// Inline completions (left group, while the setting is on): on / off / signed out / loading.
    Inline,
}

/// Is it drawn as an icon button (as opposed to a text read-out)?
fn is_status_button(k: StatusBtn) -> bool {
    matches!(k, StatusBtn::Terminal | StatusBtn::Debug | StatusBtn::Git)
}

/// Space between two neighbouring right-hand items: the buttons carry their own padding, the
/// read-outs are separated by a wider gap, the memory figure sits a bit closer to the buttons.
fn status_gap(a: StatusBtn, b: StatusBtn) -> i32 {
    match (is_status_button(a), is_status_button(b)) {
        (true, true) => 0,
        _ if (a == StatusBtn::Ram && is_status_button(b)) || (b == StatusBtn::Ram && is_status_button(a)) => 12,
        _ => 20,
    }
}

impl App {
    /// One button of the bottom bar at `x`; returns the x after it.
    fn status_btn(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        icon: &str,
        label: &str,
        tint: crate::theme::Rgba,
        active: bool,
        kind: StatusBtn,
    ) -> i32 {
        let c = self.theme.chrome;
        let r = self.layout.status;
        let lw = if label.is_empty() { 0 } else { self.tx.ui_width(label, false) + 6 };
        let w = 8 + 14 + lw + 8;
        let b = Rect::new(x, r.y + 3, w, r.h - 6);
        if active {
            cv.fill_round(b, 6, c.list_active_bg);
        }
        self.tx.icon(cv, icon, b.x + 8, r.y + (r.h - 14) / 2, 14, tint, r);
        if !label.is_empty() {
            self.tx.draw_ui(cv, b.x + 8 + 14 + 6, r.y + (r.h - 18) / 2, label, tint, r, false);
        }
        self.layout.status_btns.push((b, kind));
        b.right() + 2
    }

    fn status_btn_width(&mut self, label: &str) -> i32 {
        8 + 14 + if label.is_empty() { 0 } else { self.tx.ui_width(label, false) + 6 } + 8 + 2
    }

    /// Bottom bar. Left corner: sidebar toggle (mandatory), project search, project diagnostics
    /// (and the transient message). Right corner: cursor position / indentation / language,
    /// terminal, debug, git, then the memory readout. Every item but the sidebar toggle can be
    /// hidden with `config.status_items`.
    pub fn draw_status(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.status;
        cv.fill(r, c.status_bg);
        cv.fill(Rect::new(r.x, r.y, r.w, 1), c.border);
        self.layout.status_btns.clear();
        self.layout.status_info.clear();
        let y = r.y + (r.h - 18) / 2;
        let dim = c.tab_inactive_fg;
        let vis = |id: &str| self.config.status_visible(id);
        let (v_search, v_problems, v_term, v_dbg, v_git, v_ram) =
            (vis("search"), vis("problems"), vis("terminal"), vis("debug"), vis("git"), vis("ram"));
        let (v_pos, v_indent, v_lang) = (vis("position"), vis("indent"), vis("language"));
        let v_enc = vis("encoding");

        // Left group, in `config.status_left_order` (the sidebar toggle is always shown).
        let mut x = r.x + 6;
        for id in self.config.status_left_order.clone() {
            match id.as_str() {
                "sidebar" => {
                    let side_icon = if self.sidebar_open {
                        "ui/threads_sidebar_left_open"
                    } else {
                        "ui/threads_sidebar_left_closed"
                    };
                    x = self.status_btn(cv, x, side_icon, "", dim, self.sidebar_open, StatusBtn::Sidebar);
                }
                "search" if v_search => {
                    x = self.status_btn(cv, x, "hugeicons/search", "", dim, false, StatusBtn::Search);
                }
                "problems" if v_problems => {
                    let (e, w) = self.diag_counts();
                    if e == 0 && w == 0 {
                        // Nothing to report: a single calm check instead of two zero counters.
                        x = self.status_btn(cv, x, "ui/check", "", dim, false, StatusBtn::Problems);
                        x += 8;
                    } else {
                        x = self.status_btn(
                            cv,
                            x,
                            "ui/x_circle",
                            &e.to_string(),
                            if e > 0 { c.error } else { dim },
                            false,
                            StatusBtn::Problems,
                        );
                        // The warning count is part of the same button (same hit target).
                        let warn_label = w.to_string();
                        let ww = self.tx.ui_width(&warn_label, false);
                        self.tx.icon(
                            cv,
                            "ui/warning",
                            x,
                            r.y + (r.h - 14) / 2,
                            14,
                            if w > 0 { c.warning } else { dim },
                            r,
                        );
                        self.tx.draw_ui(cv, x + 20, y, &warn_label, if w > 0 { c.warning } else { dim }, r, false);
                        if let Some((b, _)) = self.layout.status_btns.last_mut() {
                            b.w = (x + 20 + ww + 8) - b.x;
                        }
                        x += 20 + ww + 16;
                    }
                }
                _ => {}
            }
        }
        if !self.lsp_enabled() {
            let w = self.tx.ui_width("LSP off", false);
            self.tx.draw_ui(cv, x + 2, y, "LSP off", dim, Rect::new(x, r.y, w + 8, r.h), false);
            x += w + 14;
        }
        // A chord waits for its second stroke: say so (Esc gives up).
        // So does "Jump to…" while it waits for characters or a label.
        if let Some(label) = self.chord.as_deref().map(crate::keymap::chord_label).or_else(|| self.jump_status()) {
            let lw = self.tx.ui_width(&label, true);
            let pill = Rect::new(x + 2, r.y + 4, lw + 18, r.h - 8);
            cv.fill_round(pill, (pill.h / 2).min(10), c.accent.with_alpha(0x38));
            self.tx.draw_ui(cv, pill.x + 9, y, &label, c.accent_text, pill, true);
            let hint = "Esc cancels";
            let hw = self.tx.ui_width(hint, false);
            self.tx.draw_ui(cv, pill.right() + 8, y, hint, dim, Rect::new(pill.right(), r.y, hw + 16, r.h), false);
            x = pill.right() + 8 + hw + 14;
        }
        x = self.draw_macro_status(cv, x);
        if self.upd.indicator().is_some() {
            let tint = if self.upd.ready() { c.accent_text } else { dim };
            x = self.status_btn(cv, x, "ui/download", "", tint, self.upd.card.is_some(), StatusBtn::Update);
        }
        x = self.draw_build_status(cv, x);
        x = self.draw_ai_status(cv, x);
        x = self.draw_fswatch_status(cv, x);
        x = self.draw_projcfg_status(cv, x);
        x = self.draw_img_status(cv, x);
        x = self.draw_inline_status(cv, x);
        x = self.draw_cov_status(cv, x);

        // Right group, laid out from the right edge in `config.status_order`: buttons, read-outs
        // (cursor position, indentation, language: clickable) and the memory figure, in any order.
        let changed = self.git_status.len();
        let git_label = if changed > 0 { changed.to_string() } else { String::new() };
        let short = self.config.ram_display == "short";
        let memtxt = if v_ram { self.memory_text() } else { String::new() };
        // An image tab has no caret, indentation, language or encoding to show (its read-out is on the left).
        let doc_texts: Option<(String, String, String)> =
            self.docs.get(self.active).filter(|d| !d.showing_image()).map(|d| {
                let (l, col) = d.buf.line_col(d.ed.head());
                let vc = crate::editor::visual_col(&d.buf.line(l), col, d.ed.tab_w) + 1;
                let sel =
                    if d.ed.extra.is_empty() { String::new() } else { format!("{} cursores   ", d.ed.extra.len() + 1) };
                (
                    format!("{sel}Ln {}, Col {}", l + 1, vc),
                    if d.ed.indent.contains('\t') {
                        "Tab".to_string()
                    } else {
                        format!("Spaces: {}", d.ed.indent.len())
                    },
                    d.lang.unwrap_or("text").to_string(),
                )
            });
        let enc_text: Option<String> =
            self.docs.get(self.active).filter(|d| !d.showing_image()).map(|d| d.buf.encoding.short());
        let mut items: Vec<(StatusBtn, i32)> = Vec::new();
        for id in self.config.status_order.clone() {
            let item = match id.as_str() {
                "position" if v_pos => doc_texts.as_ref().map(|t| (StatusBtn::Position, self.tx.ui_width(&t.0, false))),
                "indent" if v_indent => doc_texts.as_ref().map(|t| (StatusBtn::Indent, self.tx.ui_width(&t.1, false))),
                "language" if v_lang => {
                    doc_texts.as_ref().map(|t| (StatusBtn::Language, self.tx.ui_width(&t.2, false)))
                }
                "encoding" if v_enc => enc_text.as_ref().map(|t| (StatusBtn::Encoding, self.tx.ui_width(t, false))),
                "terminal" if v_term => Some((StatusBtn::Terminal, self.status_btn_width(""))),
                "debug" if v_dbg => Some((StatusBtn::Debug, self.status_btn_width(""))),
                "git" if v_git => Some((StatusBtn::Git, self.status_btn_width(&git_label))),
                "ram" if v_ram => {
                    Some((StatusBtn::Ram, if short { RAM_ICON_W } else { self.tx.ui_width(&memtxt, false) }))
                }
                _ => None,
            };
            items.extend(item);
        }
        let kinds: Vec<StatusBtn> = items.iter().map(|i| i.0).collect();
        let total: i32 = items.iter().map(|i| i.1).sum::<i32>()
            + (1..kinds.len()).map(|k| status_gap(kinds[k - 1], kinds[k])).sum::<i32>();
        let mut right_min = r.right() - 10;
        if let Some(&last) = kinds.last() {
            let mut ix = r.right() - if is_status_button(last) { 10 } else { 16 } - total;
            right_min = ix;
            let term_on = self.panel.open && self.panel.tab == crate::app_panel::PanelTab::Terminal;
            let dbg_on = self.panel.open && self.panel.tab == crate::app_panel::PanelTab::Debug;
            for (k, &(kind, w)) in items.iter().enumerate() {
                match kind {
                    StatusBtn::Terminal => {
                        self.status_btn(
                            cv,
                            ix,
                            "ui/terminal",
                            "",
                            if term_on { c.accent_text } else { dim },
                            term_on,
                            kind,
                        );
                    }
                    StatusBtn::Debug => {
                        self.status_btn(
                            cv,
                            ix,
                            "ui/debug",
                            "",
                            if dbg_on || self.dbg.is_some() { c.accent_text } else { dim },
                            dbg_on,
                            kind,
                        );
                    }
                    StatusBtn::Git => {
                        let git_on = self.gitpanel.is_some();
                        self.status_btn(
                            cv,
                            ix,
                            "ui/git_graph",
                            &git_label,
                            if changed > 0 || git_on { c.accent_text } else { dim },
                            git_on,
                            kind,
                        );
                    }
                    StatusBtn::Ram => {
                        if short {
                            self.tx.icon(cv, "hugeicons/cpu", ix, r.y + (r.h - 14) / 2, 14, dim, r);
                            let mb = memory_mb(&memtxt);
                            let dot = if mb < 400.0 {
                                c.git_added
                            } else if mb < 900.0 {
                                c.warning
                            } else {
                                c.error
                            };
                            cv.fill_round(Rect::new(ix + 14 + 4, r.y + (r.h - 5) / 2, 5, 5), 2, dot.with_alpha(0xc8));
                        } else {
                            self.tx.draw_ui(cv, ix, y, &memtxt, dim, r, false);
                        }
                        self.layout.status_info.push((Rect::new(ix - 4, r.y + 3, w + 8, r.h - 6), kind));
                    }
                    StatusBtn::Position | StatusBtn::Indent | StatusBtn::Language | StatusBtn::Encoding => {
                        let t = match kind {
                            StatusBtn::Encoding => enc_text.as_ref(),
                            _ => doc_texts.as_ref().map(|t| match kind {
                                StatusBtn::Position => &t.0,
                                StatusBtn::Indent => &t.1,
                                _ => &t.2,
                            }),
                        };
                        let b = Rect::new(ix - 4, r.y + 3, w + 8, r.h - 6);
                        if self.overlay.is_none() && b.contains(self.mouse.0, self.mouse.1) {
                            cv.fill_round(b, 6, c.list_hover_bg);
                        }
                        if let Some(t) = t {
                            self.tx.draw_ui(cv, ix, y, t, dim, r, false);
                        }
                        self.layout.status_btns.push((b, kind));
                    }
                    StatusBtn::Sidebar
                    | StatusBtn::Search
                    | StatusBtn::Problems
                    | StatusBtn::Update
                    | StatusBtn::Build
                    | StatusBtn::Ai
                    | StatusBtn::Project
                    | StatusBtn::Inline => {}
                }
                ix += w + kinds.get(k + 1).map_or(0, |&n| status_gap(kind, n));
            }
        }

        // A git command left running by a closed panel: spinner first, then the message.
        let x = x + self.draw_git_bg(cv, x, r, right_min - 16 - x);
        // Transient message gets whatever room is left between the corners, ellipsized.
        if let Some((m, t)) = &self.status
            && t.elapsed() < std::time::Duration::from_secs(6)
        {
            let room = right_min - 16 - x;
            let msg = fit(&mut self.tx, m, room);
            if room > 40 {
                self.tx.draw_ui(cv, x, y, &msg, c.status_fg, Rect::new(x, r.y, room.max(0), r.h), false);
            }
        }
    }

    /// (errors, warnings) over every open document.
    pub fn diag_counts(&self) -> (usize, usize) {
        let (mut e, mut w) = (0usize, 0usize);
        for d in &self.docs {
            for g in &d.diags {
                match g.severity {
                    1 => e += 1,
                    2 => w += 1,
                    _ => {}
                }
            }
        }
        (e, w)
    }

    /// Mouse press on a bottom bar button.
    pub fn status_click(&mut self, x: f64, y: f64) -> bool {
        if !self.layout.status.contains(x, y) {
            return false;
        }
        let Some((_, kind)) = self.layout.status_btns.iter().find(|(r, _)| r.contains(x, y)).copied() else {
            return true;
        };
        use crate::app_panel::PanelTab;
        match kind {
            StatusBtn::Sidebar => {
                self.sidebar_open = !self.sidebar_open;
                if !self.sidebar_open {
                    self.focus = crate::app::Focus::Editor;
                }
            }
            StatusBtn::Search => self.open_project_search(),
            StatusBtn::Problems => self.open_problems(),
            StatusBtn::Terminal => {
                if self.panel.open && self.panel.tab == PanelTab::Terminal {
                    self.toggle_panel();
                } else {
                    self.open_terminal();
                }
            }
            StatusBtn::Debug => {
                if self.panel.open && self.panel.tab == PanelTab::Debug {
                    self.toggle_panel();
                } else {
                    self.panel.open = true;
                    self.panel.tab = PanelTab::Debug;
                }
            }
            StatusBtn::Git => self.git_button(),
            StatusBtn::Position => self.open_goto_line(),
            StatusBtn::Indent => self.open_indent_picker(),
            StatusBtn::Language => self.open_language_picker(),
            StatusBtn::Encoding => self.open_encoding_menu(),
            StatusBtn::Update => self.update_indicator_click(),
            StatusBtn::Build => self.build_toggle_panel(),
            StatusBtn::Ai => self.ai_badge_click(),
            StatusBtn::Project => self.open_project_settings(),
            StatusBtn::Inline => self.inline_status_click(),
            StatusBtn::Ram => {}
        }
        true
    }
}

// ───────────── indentation / language of the current document ─────────────

/// Entries of the indentation picker: (label, spec).
pub const INDENT_CHOICES: &[(&str, &str)] =
    &[("Spaces: 2", "2"), ("Spaces: 4", "4"), ("Spaces: 8", "8"), ("Tab", "tab")];

/// "2" / "4" / "8" -> that many spaces, "tab" -> a tab; anything else -> `None`.
pub fn indent_from_spec(spec: &str) -> Option<String> {
    match spec {
        "tab" => Some("\t".to_string()),
        n => n.parse::<usize>().ok().filter(|n| (1..=8).contains(n)).map(|n| " ".repeat(n)),
    }
}

/// The spec an indent string corresponds to (inverse of [`indent_from_spec`]).
pub fn indent_spec(indent: &str) -> String {
    if indent.contains('\t') { "tab".to_string() } else { indent.len().to_string() }
}

/// Line comment prefix for a language id ("" = none: toggling comments does nothing).
pub fn comment_for_lang(lang: Option<&str>) -> &'static str {
    match lang {
        Some(
            "python" | "shellscript" | "toml" | "yaml" | "ruby" | "dockerfile" | "makefile" | "perl" | "r"
            | "powershell" | "properties" | "ini" | "nix" | "elixir" | "julia",
        ) => "#",
        Some("lua" | "sql" | "haskell" | "ada" | "plsql") => "--",
        Some("css" | "scss" | "less" | "html" | "markdown" | "json" | "xml" | "jsonc") => "",
        _ => "//",
    }
}

/// (id, display name) of every language with a grammar, sorted by name.
pub fn language_choices(assets: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(assets.join("language-manifest.json")) else { return Vec::new() };
    let Ok(m) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let mut v: Vec<(String, String)> = m["languages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let id = l["id"].as_str()?;
            Some((id.to_string(), l["displayName"].as_str().unwrap_or(id).to_string()))
        })
        .collect();
    v.sort_by_key(|(_, name)| name.to_lowercase());
    v
}

/// `Doc::lang` wants a `&'static str`: ids picked at runtime are interned (leaked once each; there
/// are only a few hundred possible ones).
fn static_lang(id: &str) -> &'static str {
    static SEEN: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(s) = seen.iter().find(|s| **s == id) {
        return s;
    }
    let s: &'static str = Box::leak(id.to_string().into_boxed_str());
    seen.push(s);
    s
}

/// What a choice of the indentation picker does. They travel in `Action::SetIndent(String)`:
/// `"4"` / `"tab"` change the setting, `"reindent:4"` also converts the file, `"tabw:8"` sets the
/// width of a tab character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndentAction {
    Set(String),
    Reindent(String),
    TabWidth(usize),
}

pub fn parse_indent_action(spec: &str) -> Option<IndentAction> {
    if let Some(rest) = spec.strip_prefix("reindent:") {
        return indent_from_spec(rest).map(|_| IndentAction::Reindent(rest.to_string()));
    }
    if let Some(rest) = spec.strip_prefix("tabw:") {
        return rest
            .parse::<usize>()
            .ok()
            .filter(|n| crate::config::TAB_WIDTHS.contains(n))
            .map(IndentAction::TabWidth);
    }
    indent_from_spec(spec).map(|_| IndentAction::Set(spec.to_string()))
}

/// The indentation unit a file actually uses: tabs when most indented lines start with one, else
/// the most frequent increase in leading spaces from one line to the next (2, 3, 4 or 8), so a
/// block comment's stray one-space lines do not fool it. `fallback` when the text is not indented.
pub fn detect_unit(text: &str, fallback: &str) -> String {
    let (mut tabs, mut spaced) = (0usize, 0usize);
    let mut deltas = [0usize; 9];
    let mut prev = 0usize;
    for line in text.lines().take(4000) {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            tabs += 1;
            continue;
        }
        let n = line.chars().take_while(|c| *c == ' ').count();
        if n > 0 {
            spaced += 1;
        }
        if n > prev && n - prev <= 8 {
            deltas[n - prev] += 1;
        }
        prev = n;
    }
    if tabs > 0 && tabs >= spaced {
        return "\t".into();
    }
    // Ties go to the smaller step; a lone 1-space increase is noise unless it is all there is.
    let best = (2..=8).filter(|&d| deltas[d] > 0).max_by_key(|&d| (deltas[d], std::cmp::Reverse(d)));
    match best {
        Some(d) => " ".repeat(d),
        None => fallback.to_string(),
    }
}

/// Convert the leading whitespace of every non-blank line from indentation unit `from` to `to`.
/// A tab counts up to the next multiple of `tab_w`; leftover columns that do not fill a level stay
/// as spaces. Returns the new text and how many lines changed.
pub fn reindent(text: &str, from: &str, to: &str, tab_w: usize) -> (String, usize) {
    let tab_w = tab_w.max(1);
    let unit_w = if from.contains('\t') { tab_w } else { from.chars().count().max(1) };
    let mut out = String::with_capacity(text.len());
    let mut changed = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let lead_len = body.len() - body.trim_start_matches([' ', '\t']).len();
        if lead_len == 0 || body.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        let mut w = 0;
        for ch in body[..lead_len].chars() {
            w += if ch == '\t' { tab_w - w % tab_w } else { 1 };
        }
        let new_lead = format!("{}{}", to.repeat(w / unit_w), " ".repeat(w % unit_w));
        if new_lead != body[..lead_len] {
            changed += 1;
        }
        out.push_str(&new_lead);
        out.push_str(&line[lead_len..]);
    }
    (out, changed)
}

/// What the user picked for a document in the bottom bar pickers; kept for the session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocPick {
    /// `Some(None)` = plain text was picked.
    pub lang: Option<Option<String>>,
    /// Indentation spec ("2", "4", "8", "tab").
    pub indent: Option<String>,
    pub tab_w: Option<usize>,
}

impl DocPick {
    /// Add this pick's fields to a session entry (`lang_pick` is "" for plain text).
    pub fn write_json(&self, v: &mut serde_json::Value) {
        let Some(o) = v.as_object_mut() else { return };
        if let Some(l) = &self.lang {
            o.insert("lang_pick".into(), serde_json::Value::String(l.clone().unwrap_or_default()));
        }
        if let Some(i) = &self.indent {
            o.insert("indent".into(), serde_json::Value::String(i.clone()));
        }
        if let Some(w) = self.tab_w {
            o.insert("tab_w".into(), serde_json::json!(w));
        }
    }

    pub fn from_json(v: &serde_json::Value) -> DocPick {
        DocPick {
            lang: v["lang_pick"].as_str().map(|s| if s.is_empty() { None } else { Some(s.to_string()) }),
            indent: v["indent"].as_str().filter(|s| indent_from_spec(s).is_some()).map(str::to_string),
            tab_w: v["tab_w"].as_u64().map(|n| n as usize).filter(|n| crate::config::TAB_WIDTHS.contains(n)),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == DocPick::default()
    }
}

impl App {
    /// Session entry of a document: add what the pickers chose.
    pub fn picks_to_json(&self, path: &Path, v: &mut serde_json::Value) {
        if let Some(p) = self.picks.get(path) {
            p.write_json(v);
        }
    }

    /// Re-apply the picks stored in a session entry to document `i`.
    pub fn apply_picks_json(&mut self, i: usize, entry: &serde_json::Value) {
        let pick = DocPick::from_json(entry);
        if pick.is_empty() || i >= self.docs.len() {
            return;
        }
        if let Some(l) = pick.lang.clone() {
            self.set_doc_language_for(i, l, false);
        }
        if let Some(ind) = pick.indent.as_deref().and_then(indent_from_spec) {
            self.docs[i].ed.indent = ind;
        }
        if let Some(w) = pick.tab_w {
            self.docs[i].ed.tab_w = w;
        }
        if let Some(path) = self.docs[i].path().map(Path::to_path_buf) {
            self.picks.insert(path, pick);
        }
    }

    fn pick_entry(&mut self, i: usize) -> Option<&mut DocPick> {
        let path = self.docs.get(i)?.path()?.to_path_buf();
        Some(self.picks.entry(path).or_default())
    }

    /// New documents start with the tab width of the settings (and the setting changed: follow it
    /// in the open documents the user has not given a width of their own).
    pub fn set_default_tab_width(&mut self, n: usize) {
        crate::editor::set_default_tab_w(n);
        for i in 0..self.docs.len() {
            let own = self.docs[i].path().and_then(|p| self.picks.get(p)).is_some_and(|p| p.tab_w.is_some())
                || self.docs[i].fileconf.tab_width.is_some();
            if !own {
                // A `languages.<id>.tab_width` override keeps its own width.
                let lang = self.docs[i].lang;
                let w = if self.config.lang_override(lang, "tab_width").is_some() {
                    self.config.tab_width_for(lang)
                } else {
                    n
                };
                self.docs[i].ed.tab_w = w;
            }
        }
        self.request_redraw();
    }

    /// Bottom bar "Spaces: 4": choose how the current document indents, convert the file to a
    /// setting, or set the width of a tab character.
    pub fn open_indent_picker(&mut self) {
        use crate::app_overlay::{Action, Item};
        let Some(d) = self.docs.get(self.active) else { return };
        let cur = indent_spec(&d.ed.indent);
        let mut items: Vec<Item> = INDENT_CHOICES
            .iter()
            .map(|(label, spec)| Item {
                label: label.to_string(),
                detail: if *spec == cur { "current".into() } else { "only changes the setting".into() },
                action: Action::SetIndent(spec.to_string()),
            })
            .collect();
        items.extend(INDENT_CHOICES.iter().map(|(label, spec)| Item {
            label: format!("Re-indent file to: {label}"),
            detail: "converts the indentation of every line".into(),
            action: Action::SetIndent(format!("reindent:{spec}")),
        }));
        items.extend(crate::config::TAB_WIDTHS.iter().map(|w| Item {
            label: format!("Tab width: {w}"),
            detail: if d.ed.tab_w == *w { "current".into() } else { "columns per tab".into() },
            action: Action::SetIndent(format!("tabw:{w}")),
        }));
        let sel = INDENT_CHOICES.iter().position(|(_, s)| *s == cur).unwrap_or(0);
        self.overlay = Overlay::Picker(crate::app_overlay::choice_picker("File indentation", items, sel));
    }

    /// Bottom bar language name: choose the language (and so the highlighting) of the document.
    pub fn open_language_picker(&mut self) {
        use crate::app_overlay::{Action, Item};
        let Some(d) = self.docs.get(self.active) else { return };
        let cur = d.lang;
        let mut items = vec![Item {
            label: "Plain text".into(),
            detail: if cur.is_none() { "current".into() } else { String::new() },
            action: Action::SetLang(None),
        }];
        let mut sel = 0;
        for (id, name) in language_choices(&self.assets) {
            if Some(id.as_str()) == cur {
                sel = items.len();
            }
            let detail = if Some(id.as_str()) == cur { format!("{id} · atual") } else { id.clone() };
            items.push(Item { label: name, detail, action: Action::SetLang(Some(id)) });
        }
        self.overlay = Overlay::Picker(crate::app_overlay::choice_picker("File language (type to filter)", items, sel));
    }

    /// Apply a choice of the indentation picker to the active document.
    pub fn set_doc_indent(&mut self, spec: &str) {
        let i = self.active;
        let Some(action) = parse_indent_action(spec) else { return };
        if self.docs.get(i).is_none() {
            return;
        }
        match action {
            IndentAction::Set(s) => {
                let Some(indent) = indent_from_spec(&s) else { return };
                self.docs[i].ed.indent = indent;
                if let Some(p) = self.pick_entry(i) {
                    p.indent = Some(s.clone());
                }
                self.say(format!(
                    "indentation: {}",
                    if s == "tab" { "tabs".to_string() } else { format!("{s} spaces") }
                ));
            }
            IndentAction::Reindent(s) => {
                let Some(to) = indent_from_spec(&s) else { return };
                let d = &mut self.docs[i];
                let text = d.buf.to_string();
                let from = detect_unit(&text, &d.ed.indent);
                let (new, lines) = reindent(&text, &from, &to, d.ed.tab_w);
                if lines > 0 {
                    d.replace_text(&new);
                }
                d.ed.indent = to;
                if let Some(p) = self.pick_entry(i) {
                    p.indent = Some(s.clone());
                }
                let what = if s == "tab" { "tabs".to_string() } else { format!("{s} spaces") };
                self.say(if lines == 0 {
                    format!("indentation: {what} (the file already used it)")
                } else {
                    format!("re-indented to {what}: {lines} lines")
                });
            }
            IndentAction::TabWidth(w) => {
                self.docs[i].ed.tab_w = w;
                if let Some(p) = self.pick_entry(i) {
                    p.tab_w = Some(w);
                }
                self.say(format!("tab width: {w}"));
            }
        }
    }

    /// Switch the active document to language `id` (`None` = plain text); see [`Self::set_doc_language_for`].
    pub fn set_doc_language(&mut self, id: Option<String>) {
        let i = self.active;
        self.set_doc_language_for(i, id, true);
    }

    /// Switch document `i` to language `id`: new grammar, comment prefix and JSX handling, and the
    /// language servers are told (`didClose` + `didOpen` with the new `languageId`; a server the new
    /// language needs is started). `remember` stores the choice for the session.
    pub fn set_doc_language_for(&mut self, i: usize, id: Option<String>, remember: bool) {
        if self.docs.get(i).is_none() {
            return;
        }
        let lang = id.as_deref().map(static_lang);
        let mut note = None;
        let hl = lang.and_then(|l| {
            let tk = crate::textmate::Registry::new(&self.assets)
                .ok()
                .and_then(|reg| reg.scope_for_language(l))
                .and_then(|scope| {
                    if let Some(t) = self.tokenizers.get(&scope) {
                        return Some(t.clone());
                    }
                    let t = crate::highlight::make_tokenizer(&self.assets, &scope)?;
                    self.tokenizers.insert(scope, t.clone());
                    Some(t)
                });
            if tk.is_none() {
                note = Some(format!("no grammar for {l}"));
            }
            tk.map(crate::highlight::Highlighter::new)
        });
        let d = &mut self.docs[i];
        let old = d.lang;
        d.lang = lang;
        d.hl = hl;
        d.ed.comment = comment_for_lang(lang).to_string();
        d.ed.jsx = matches!(lang, Some("tsx" | "jsx" | "html" | "vue" | "svelte"));
        d.sem.clear();
        self.lsp_language_changed(i, old, lang);
        if remember {
            if let Some(p) = self.pick_entry(i) {
                p.lang = Some(id.clone());
            }
            self.say(note.unwrap_or_else(|| format!("language: {}", lang.unwrap_or("plain text"))));
        }
    }
}

/// Folder browser menu for `dir`.
pub fn browse_menu(anchor: Rect, dir: PathBuf) -> TopMenu {
    let mut entries = vec![Entry::UseDir(dir.clone())];
    if let Some(parent) = dir.parent() {
        entries.push(Entry::Up(parent.to_path_buf()));
    }
    entries.extend(subdirs(&dir).into_iter().map(Entry::Dir));
    TopMenu::new(MenuKind::Browse, anchor, entries, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "refs/heads/main\t*\tAna\t1700000000\tfix: algo\n\
                       refs/heads/feat/x\t \tBeto\t1699990000\tfeat: x\twith tab\n\
                       refs/remotes/origin/HEAD\t \tAna\t1700000000\tfix: algo\n\
                       refs/remotes/origin/main\t \tAna\t1700000000\tfix: algo\n\
                       refs/remotes/origin/dev\t \tCa\t1699000000\tdev work\n\
                       lixo sem tabs\n";

    #[test]
    fn parses_local_then_remote_and_skips_head() {
        let b = parse_branches(OUT);
        let names: Vec<&str> = b.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "feat/x", "origin/main", "origin/dev"]);
        assert!(b[0].current && !b[0].remote);
        assert!(!b[1].current && b[1].subject == "feat: x\twith tab");
        assert!(b[2].remote && b[3].remote);
        assert_eq!(b[1].author, "Beto");
        assert_eq!(b[0].when, 1_700_000_000);
    }

    #[test]
    fn recents_move_to_front_without_duplicates_and_are_capped() {
        let mut l: Vec<PathBuf> = ["/a", "/b", "/c"].iter().map(PathBuf::from).collect();
        push_recent(&mut l, Path::new("/c"), 3);
        assert_eq!(l, [PathBuf::from("/c"), "/a".into(), "/b".into()]);
        push_recent(&mut l, Path::new("/d"), 3);
        assert_eq!(l, [PathBuf::from("/d"), "/c".into(), "/a".into()]);
    }

    fn proj(n: &str, recent: bool) -> Entry {
        Entry::Project(PathBuf::from(format!("/work/{n}")), recent)
    }

    #[test]
    fn project_menu_shows_three_recents_and_open_without_search() {
        let mut e: Vec<Entry> = (0..5).map(|i| proj(&format!("r{i}"), true)).collect();
        e.push(proj("sibling", false));
        e.push(Entry::OpenProject);
        let m = TopMenu::new(MenuKind::Project, Rect::new(0, 0, 10, 10), e, "/work".into());
        assert_eq!(m.filtered, [0, 1, 2, 6], "3 recents, then Open folder…");
    }

    #[test]
    fn project_search_covers_siblings_and_keeps_open_row_last() {
        let mut e: Vec<Entry> = (0..5).map(|i| proj(&format!("r{i}"), true)).collect();
        e.push(proj("sibling-app", false));
        e.push(Entry::OpenProject);
        let mut m = TopMenu::new(MenuKind::Project, Rect::new(0, 0, 10, 10), e, "/work".into());
        m.field.insert("sibl");
        m.refilter();
        assert_eq!(m.filtered, [5, 6]);
        assert_eq!(m.sel, 0);
        m.field = TextField::with("zzz");
        m.refilter();
        assert_eq!(m.filtered, [6], "only the open row survives");
    }

    fn br(name: &str, author: &str, subject: &str, remote: bool) -> Entry {
        Entry::Branch(Branch {
            name: name.into(),
            remote,
            current: false,
            author: author.into(),
            when: 0,
            subject: subject.into(),
        })
    }

    #[test]
    fn branch_search_matches_name_author_and_subject_and_wraps_selection() {
        let e = vec![
            br("main", "Ana", "init", false),
            br("feat/login", "Beto", "add form", false),
            br("origin/feat/login", "Beto", "add form", true),
        ];
        let mut m = TopMenu::new(MenuKind::Branch, Rect::new(0, 0, 10, 10), e, "/w".into());
        assert_eq!(m.filtered, [0, 1, 2]);
        m.field = TextField::with("login");
        m.refilter();
        assert_eq!(m.filtered.len(), 2);
        m.field = TextField::with("beto");
        m.refilter();
        assert_eq!(m.filtered, [1, 2], "author search");
        m.move_sel(-1);
        assert_eq!(m.sel, 1, "selection wraps");
        m.move_sel(1);
        assert_eq!(m.sel, 0);
    }

    #[test]
    fn browse_menu_lists_up_and_subfolders() {
        let base = std::env::temp_dir().join(format!("lumen-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("b-proj")).unwrap();
        std::fs::create_dir_all(base.join("a-proj")).unwrap();
        std::fs::create_dir_all(base.join(".hidden")).unwrap();
        std::fs::create_dir_all(base.join("node_modules")).unwrap();
        std::fs::write(base.join("file.txt"), "x").unwrap();
        let m = browse_menu(Rect::new(0, 0, 10, 10), base.clone());
        let labels: Vec<String> = m
            .filtered
            .iter()
            .map(|&i| match &m.entries[i] {
                Entry::UseDir(_) => "use".to_string(),
                Entry::Up(_) => "up".to_string(),
                Entry::Dir(p) => project_name(p),
                _ => "?".to_string(),
            })
            .collect();
        assert_eq!(labels, ["use", "up", "a-proj", "b-proj"]);
        let mut m = m;
        m.field = TextField::with("b-pr");
        m.refilter();
        assert_eq!(m.sel, 2, "searching selects the first match, not the 'use this folder' row");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn list_branches_reads_a_real_repo() {
        let base = std::env::temp_dir().join(format!("lumen-branches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let g = |args: &[&str]| run_git(&base, args).unwrap_or_default();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "Tester"]);
        std::fs::write(base.join("a.txt"), "1").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "first commit"]);
        g(&["branch", "feature/x"]);
        let b = list_branches(&base);
        let names: Vec<&str> = b.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main") && names.contains(&"feature/x"), "{names:?}");
        let main = b.iter().find(|b| b.name == "main").unwrap();
        assert!(main.current && main.author == "Tester" && main.subject == "first commit");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn indent_specs_roundtrip() {
        assert_eq!(indent_from_spec("2").as_deref(), Some("  "));
        assert_eq!(indent_from_spec("8").as_deref(), Some("        "));
        assert_eq!(indent_from_spec("tab").as_deref(), Some("\t"));
        assert_eq!(indent_from_spec("0"), None);
        assert_eq!(indent_from_spec("x"), None);
        for (_, spec) in INDENT_CHOICES {
            assert_eq!(indent_spec(&indent_from_spec(spec).unwrap()), *spec);
        }
    }

    #[test]
    fn comment_prefix_follows_the_chosen_language() {
        assert_eq!(comment_for_lang(Some("python")), "#");
        assert_eq!(comment_for_lang(Some("sql")), "--");
        assert_eq!(comment_for_lang(Some("css")), "");
        assert_eq!(comment_for_lang(Some("rust")), "//");
        assert_eq!(comment_for_lang(None), "//");
    }

    #[test]
    fn language_list_comes_from_the_manifest_sorted_by_name() {
        let v = language_choices(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"));
        assert!(v.len() > 50, "{}", v.len());
        assert!(v.iter().any(|(id, name)| id == "typescript" && name == "TypeScript"));
        assert!(v.windows(2).all(|w| w[0].1.to_lowercase() <= w[1].1.to_lowercase()));
        assert!(language_choices(Path::new("/nonexistent")).is_empty());
    }

    #[test]
    fn interned_language_ids_are_stable() {
        let a = static_lang("some-lang");
        let b = static_lang("some-lang");
        assert!(std::ptr::eq(a, b));
        assert_eq!(a, "some-lang");
    }

    #[test]
    fn right_group_gaps() {
        assert_eq!(status_gap(StatusBtn::Terminal, StatusBtn::Git), 0, "buttons carry their own padding");
        assert_eq!(status_gap(StatusBtn::Position, StatusBtn::Indent), 20);
        assert_eq!(status_gap(StatusBtn::Git, StatusBtn::Ram), 12);
        assert_eq!(status_gap(StatusBtn::Ram, StatusBtn::Language), 20);
        assert_eq!(status_gap(StatusBtn::Language, StatusBtn::Terminal), 20);
    }

    #[test]
    fn indent_picker_actions_parse() {
        assert_eq!(parse_indent_action("4"), Some(IndentAction::Set("4".into())));
        assert_eq!(parse_indent_action("tab"), Some(IndentAction::Set("tab".into())));
        assert_eq!(parse_indent_action("reindent:2"), Some(IndentAction::Reindent("2".into())));
        assert_eq!(parse_indent_action("reindent:tab"), Some(IndentAction::Reindent("tab".into())));
        assert_eq!(parse_indent_action("tabw:8"), Some(IndentAction::TabWidth(8)));
        assert_eq!(parse_indent_action("tabw:3"), None, "only 2, 4 and 8");
        assert_eq!(parse_indent_action("reindent:99"), None);
        assert_eq!(parse_indent_action("lixo"), None);
    }

    #[test]
    fn the_files_own_indentation_unit_is_detected() {
        let two = "function f() {\n  if (a) {\n    b();\n  }\n  /**\n   * doc\n   */\n  c();\n}\n";
        assert_eq!(detect_unit(two, "    "), "  ", "the one-space comment lines do not count");
        let four = "class A {\n    m() {\n        x();\n    }\n    n() {\n        y();\n    }\n}\n";
        assert_eq!(detect_unit(four, "  "), "    ");
        assert_eq!(detect_unit("a\n\tb\n\t\tc\n\td\n", "  "), "\t");
        assert_eq!(detect_unit("a\nb\nc\n", "   "), "   ", "nothing indented: keep the fallback");
    }

    #[test]
    fn reindenting_converts_leading_whitespace_only() {
        let src = "if (a) {\n  b(\"  keep  \");\n\n  if (c) {\n    d();\n  }\n}\n";
        let (out, n) = reindent(src, "  ", "    ", 4);
        assert_eq!(out, "if (a) {\n    b(\"  keep  \");\n\n    if (c) {\n        d();\n    }\n}\n");
        assert_eq!(n, 4, "the blank line and the unindented ones are untouched");
        // Spaces to tabs and back.
        let (tabs, _) = reindent(&out, "    ", "\t", 4);
        assert_eq!(tabs, "if (a) {\n\tb(\"  keep  \");\n\n\tif (c) {\n\t\td();\n\t}\n}\n");
        let (back, _) = reindent(&tabs, "\t", "  ", 4);
        assert_eq!(back, src);
        // A tab counts to the next multiple of the tab width; leftover columns stay spaces.
        let (odd, _) = reindent("   x\n", "  ", "\t", 4);
        assert_eq!(odd, "\t x\n");
        let (w8, _) = reindent("\tx\n", "\t", "    ", 8);
        assert_eq!(w8, "    x\n", "one tab level becomes one level of the target unit, whatever the tab width");
    }

    #[test]
    fn reindent_keeps_line_endings_and_reports_zero_when_nothing_changes() {
        let (out, n) = reindent("a\r\n  b\r\n", "  ", "    ", 4);
        assert_eq!(out, "a\r\n    b\r\n");
        assert_eq!(n, 1);
        let (same, n) = reindent("a\n    b\n", "    ", "    ", 4);
        assert_eq!((same.as_str(), n), ("a\n    b\n", 0));
        let (nolast, _) = reindent("  a", "  ", "\t", 4);
        assert_eq!(nolast, "\ta", "no trailing newline needed");
    }

    #[test]
    fn picks_roundtrip_through_the_session_entry() {
        let pick = DocPick { lang: Some(Some("python".into())), indent: Some("tab".into()), tab_w: Some(8) };
        let mut v = serde_json::json!({"path": "/x", "line": 1});
        pick.write_json(&mut v);
        assert_eq!(DocPick::from_json(&v), pick);
        // Plain text is a pick of its own ("" in the file), distinct from no pick at all.
        let plain = DocPick { lang: Some(None), ..Default::default() };
        let mut v = serde_json::json!({});
        plain.write_json(&mut v);
        assert_eq!(v["lang_pick"], "");
        assert_eq!(DocPick::from_json(&v), plain);
        assert!(DocPick::from_json(&serde_json::json!({"path": "/x"})).is_empty());
        // Garbage in an old or edited session is ignored.
        let bad = serde_json::json!({"indent": "banana", "tab_w": 3});
        assert!(DocPick::from_json(&bad).is_empty());
    }
}
