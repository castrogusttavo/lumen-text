//! Top bar: the project name and the current git branch, each opening a dropdown.
//!
//! * project menu: search field, the 3 most recent projects, "Abrir pasta…" (an in-app folder
//!   browser). Typing searches every known project (recents plus the folders next to the current one).
//! * branch menu: search field and every local and remote branch as
//!   `name | author, há 3 dias, last commit subject…`; Enter checks the branch out.
//!
//! The menus are an [`Overlay::TopMenu`]; input, drawing and hit-testing live here.

use crate::{
    app::{App, KeyIn},
    app_overlay::Overlay,
    field::TextField,
    fuzzy::fuzzy,
    git,
    tree::Tree,
    scrollreg::Reg,
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
    /// Folder browser behind "Abrir pasta…".
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
    /// Open the folder being browsed as the project.
    UseDir(PathBuf),
    Up(PathBuf),
    Dir(PathBuf),
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
    pub now: i64,
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
        let (Some(refname), Some(head), Some(author), Some(when), Some(subject)) = (it.next(), it.next(), it.next(), it.next(), it.next()) else { continue };
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
        let b = Branch { name: name.to_string(), remote: is_remote, current: head.trim() == "*", author: author.to_string(), when: when.trim().parse().unwrap_or(0), subject: subject.to_string() };
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
        Err(e.lines().find(|l| !l.trim().is_empty()).unwrap_or("falhou").to_string())
    }
}

pub fn list_branches(dir: &Path) -> Vec<Branch> {
    let fmt = "--format=%(refname)%09%(HEAD)%09%(authorname)%09%(committerdate:unix)%09%(subject)";
    run_git(dir, &["for-each-ref", "--sort=-committerdate", fmt, "refs/heads", "refs/remotes"]).map(|o| parse_branches(&o)).unwrap_or_default()
}

/// `list` with `p` moved to the front (no duplicates), at most `max` entries.
pub fn push_recent(list: &mut Vec<PathBuf>, p: &Path, max: usize) {
    list.retain(|x| x != p);
    list.insert(0, p.to_path_buf());
    list.truncate(max);
}

fn recents_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
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
        let mut m = TopMenu { kind, anchor, field: TextField::default(), entries, filtered: Vec::new(), sel: 0, scroll: 0, dir, rows: Vec::new(), now: now_secs() };
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
                Entry::UseDir(_) | Entry::Up(_) => lead.push(i),
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
            self.tx.icon(cv, "ui/git_branch", r.x + 10, bar.y + (bar.h - 14) / 2, 14, c.tab_inactive_fg, bar);
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
        let r = self.layout.top_btns.iter().find(|(_, k)| *k == kind).map(|(r, _)| *r).unwrap_or(Rect::new(10, 4, 140, TOP_H - 9));
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
                    return self.say("sem branches (a pasta não é um repositório git?)");
                }
                let entries = branches.into_iter().map(Entry::Branch).collect();
                self.overlay = Overlay::TopMenu(Box::new(TopMenu::new(kind, anchor, entries, self.root.clone())));
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
            MenuKind::Project => "Buscar projeto…",
            MenuKind::Branch => "Buscar branch…",
            MenuKind::Browse => "Buscar pasta…",
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
        for (k, &ei) in m.filtered.iter().enumerate().skip(m.scroll).take(MAX_ROWS) {
            let y = top + 5 + (k - m.scroll) as i32 * ROW;
            let rr = Rect::new(r.x + 6, y, r.w - 12, ROW);
            if k == m.sel {
                cv.fill_round(rr, 8, c.list_active_bg);
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
                    self.tx.draw_ui(cv, rr.x + 38, ly, "Abrir pasta…", c.accent_text, rr, false);
                }
                Entry::UseDir(p) => {
                    self.tx.icon(cv, "ui/check", rr.x + 12, iy, 16, c.accent, rr);
                    let label = format!("Abrir “{}” como projeto", project_name(p));
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
                Entry::Branch(b) => {
                    self.tx.icon(cv, if b.remote { "ui/public" } else { "ui/git_branch" }, rr.x + 12, iy, 16, dim, rr);
                    let name = fit(&mut self.tx, &b.name, rr.w * 2 / 5);
                    let name_col = if b.current { c.accent_text } else { fg };
                    self.tx.draw_ui(cv, rr.x + 38, ly, &name, name_col, rr, b.current);
                    let used = self.tx.ui_width(&name, b.current);
                    let dx = rr.x + 38 + used + 12;
                    let end = rr.right() - if b.current { 34 } else { 12 };
                    self.tx.draw_ui(cv, dx, ly, "|", dim.with_alpha(0x70), rr, false);
                    let info = format!("{}, {}, {}", b.author, git::ago((m.now - b.when).max(0)), b.subject);
                    let info = fit(&mut self.tx, &info, end - (dx + 16));
                    self.tx.draw_ui(cv, dx + 16, ly, &info, dim, rr, false);
                    if b.current {
                        self.tx.icon(cv, "ui/check", rr.right() - 28, iy, 16, c.accent, rr);
                    }
                }
            }
            rows.push((rr, k));
        }
        if m.filtered.is_empty() {
            self.tx.draw_ui(cv, r.x + 16, top + 10, "Sem resultados", dim, r, false);
        }
        m.rows = rows;
        let (total, pos) = (m.filtered.len() as f64, m.scroll as f64);
        self.overlay = Overlay::TopMenu(m);
        if total > MAX_ROWS as f64 {
            let area = Rect::new(r.x, top, r.w, MAX_ROWS as i32 * ROW + 10);
            self.region_bar(cv, Reg::TopMenu, area, total, MAX_ROWS as f64, pos, ROW as f64, true);
        }
    }

    pub fn topmenu_mouse(&mut self, x: f64, y: f64) -> bool {
        let Overlay::TopMenu(m) = &mut self.overlay else { return false };
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
        let d = match delta {
            MouseScrollDelta::LineDelta(_, y) => -(y as isize) * 3,
            MouseScrollDelta::PixelDelta(p) => -(p.y / ROW as f64) as isize,
        };
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
                } else if let (false, false, Some(t)) = (ctrl, alt, ev.text.as_deref()) {
                    if !t.is_empty() && !t.chars().any(char::is_control) {
                        m.field.insert(t);
                        changed = true;
                    }
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
            Entry::Branch(b) => self.checkout_branch(b, &m.entries),
        }
    }

    // ───────────── actions ─────────────

    /// Close everything of the current project and open `path` as the new one.
    pub fn switch_project(&mut self, path: PathBuf) {
        let path = path.canonicalize().unwrap_or(path);
        if !path.is_dir() {
            return self.say(format!("{} não é uma pasta", path.display()));
        }
        if path == self.root {
            return self.say("esse já é o projeto aberto");
        }
        if let Some(d) = self.docs.iter().find(|d| d.buf.is_dirty()) {
            let n = d.name.clone();
            return self.say(format!("{n} tem alterações não salvas — salve ou feche antes de trocar de projeto"));
        }
        if self.dbg.is_some() {
            return self.say("pare a depuração antes de trocar de projeto");
        }
        self.save_session();
        remember_project(&self.root);
        while !self.docs.is_empty() {
            self.close_tab(0, true);
        }
        self.lsps.clear();
        self.lsp_warned = false;
        self.diff = None;
        self.find = None;
        self.completion = None;
        self.hover = None;
        self.nav_back.clear();
        self.nav_fwd.clear();
        self.file_index = None;
        self.test_json = None;
        self.blame_for = None;
        self.eslint_busy.clear();
        self.git_status.clear();
        self.root = path;
        self.tree = Tree::new(self.root.clone());
        self.active = 0;
        self.refresh_git_status();
        self.watcher = crate::watcher::Watcher::new(self.wake()).ok();
        self.watch_dirs();
        remember_project(&self.root);
        self.restore_session();
        let name = project_name(&self.root);
        self.say(format!("projeto: {name}"));
        self.request_redraw();
    }

    fn checkout_branch(&mut self, b: Branch, all: &[Entry]) {
        if b.current {
            return self.say(format!("já está em {}", b.name));
        }
        // A remote branch whose local twin exists just switches to the twin.
        let twin = if b.remote { b.name.split_once('/').map(|x| x.1.to_string()) } else { None };
        let has_twin = twin.as_ref().is_some_and(|t| all.iter().any(|e| matches!(e, Entry::Branch(x) if !x.remote && &x.name == t)));
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
    /// Read-outs (not clickable): they only exist for tooltips.
    Ram,
    Position,
    Indent,
    Language,
}

impl App {
    /// One button of the bottom bar at `x`; returns the x after it.
    fn status_btn(&mut self, cv: &mut Canvas, x: i32, icon: &str, label: &str, tint: crate::theme::Rgba, active: bool, kind: StatusBtn) -> i32 {
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
        cv.fill(Rect::new(r.x, r.y, r.w, 1), c.border);
        self.layout.status_btns.clear();
        self.layout.status_info.clear();
        let y = r.y + (r.h - 18) / 2;
        let dim = c.tab_inactive_fg;
        let vis = |id: &str| self.config.status_visible(id);
        let (v_search, v_problems, v_term, v_dbg, v_git, v_ram) = (vis("search"), vis("problems"), vis("terminal"), vis("debug"), vis("git"), vis("ram"));
        let (v_pos, v_indent, v_lang) = (vis("position"), vis("indent"), vis("language"));

        // Left group.
        let mut x = r.x + 6;
        let side_icon = if self.sidebar_open { "ui/threads_sidebar_left_open" } else { "ui/threads_sidebar_left_closed" };
        x = self.status_btn(cv, x, side_icon, "", dim, self.sidebar_open, StatusBtn::Sidebar);
        if v_search {
            x = self.status_btn(cv, x, "hugeicons/search", "", dim, false, StatusBtn::Search);
        }
        if v_problems {
            let (e, w) = self.diag_counts();
            if e == 0 && w == 0 {
                // Nothing to report: a single calm check instead of two zero counters.
                x = self.status_btn(cv, x, "ui/check", "", dim, false, StatusBtn::Problems);
                x += 8;
            } else {
                x = self.status_btn(cv, x, "ui/x_circle", &e.to_string(), if e > 0 { c.error } else { dim }, false, StatusBtn::Problems);
                // The warning count is part of the same button (same hit target).
                let warn_label = w.to_string();
                let ww = self.tx.ui_width(&warn_label, false);
                self.tx.icon(cv, "ui/warning", x, r.y + (r.h - 14) / 2, 14, if w > 0 { c.warning } else { dim }, r);
                self.tx.draw_ui(cv, x + 20, y, &warn_label, if w > 0 { c.warning } else { dim }, r, false);
                if let Some((b, _)) = self.layout.status_btns.last_mut() {
                    b.w = (x + 20 + ww + 8) - b.x;
                }
                x += 20 + ww + 16;
            }
        }

        // Right group, laid out from the right edge: memory, then the buttons, then the info.
        let mut right_min = r.right() - 10;
        let short = self.config.ram_display == "short";
        let mut ram_x = r.right() - 16;
        if v_ram {
            let memtxt = self.memory_text();
            let mw = if short { RAM_ICON_W } else { self.tx.ui_width(&memtxt, false) };
            ram_x = r.right() - 16 - mw;
            if short {
                self.tx.icon(cv, "hugeicons/cpu", ram_x, r.y + (r.h - 14) / 2, 14, dim, r);
                let mb = memory_mb(&memtxt);
                let dot = if mb < 400.0 { c.git_added } else if mb < 900.0 { c.warning } else { c.error };
                cv.fill_round(Rect::new(ram_x + 14 + 4, r.y + (r.h - 5) / 2, 5, 5), 2, dot.with_alpha(0xc8));
            } else {
                self.tx.draw_ui(cv, ram_x, y, &memtxt, dim, r, false);
            }
            self.layout.status_info.push((Rect::new(ram_x - 4, r.y + 3, mw + 8, r.h - 6), StatusBtn::Ram));
            right_min = ram_x - 12;
        }
        let changed = self.git_status.len();
        let git_label = if changed > 0 { changed.to_string() } else { String::new() };
        let mut group_w = 0;
        for (on, label) in [(v_term, ""), (v_dbg, ""), (v_git, git_label.as_str())] {
            if on {
                group_w += self.status_btn_width(label);
            }
        }
        if group_w > 0 {
            let mut bx = right_min - group_w;
            let group_x = bx;
            let term_on = self.panel.open && self.panel.tab == crate::app_panel::PanelTab::Terminal;
            let dbg_on = self.panel.open && self.panel.tab == crate::app_panel::PanelTab::Debug;
            if v_term {
                bx = self.status_btn(cv, bx, "ui/terminal", "", if term_on { c.accent_text } else { dim }, term_on, StatusBtn::Terminal);
            }
            if v_dbg {
                bx = self.status_btn(cv, bx, "ui/debug", "", if dbg_on || self.dbg.is_some() { c.accent_text } else { dim }, dbg_on, StatusBtn::Debug);
            }
            if v_git {
                let git_on = self.gitpanel.is_some();
                let _ = self.status_btn(cv, bx, "ui/git_graph", &git_label, if changed > 0 || git_on { c.accent_text } else { dim }, git_on, StatusBtn::Git);
            }
            right_min = group_x;
        }

        // Cursor position, indentation and language, just left of the right group.
        let mut pieces: Vec<(StatusBtn, String)> = Vec::new();
        if let Some(d) = self.docs.get(self.active) {
            let (l, col) = d.buf.line_col(d.ed.head());
            let vc = crate::editor::visual_col(&d.buf.line(l), col, d.ed.tab_w) + 1;
            let sel = if d.ed.extra.is_empty() { String::new() } else { format!("{} cursores   ", d.ed.extra.len() + 1) };
            if v_pos {
                pieces.push((StatusBtn::Position, format!("{sel}Ln {}, Col {}", l + 1, vc)));
            }
            if v_indent {
                pieces.push((StatusBtn::Indent, if d.ed.indent.contains('\t') { "Tab".to_string() } else { format!("Espaços: {}", d.ed.indent.len()) }));
            }
            if v_lang {
                pieces.push((StatusBtn::Language, d.lang.unwrap_or("texto").to_string()));
            }
        }
        if !pieces.is_empty() {
            const GAP: i32 = 20;
            let widths: Vec<i32> = pieces.iter().map(|(_, t)| self.tx.ui_width(t, false)).collect();
            let total = widths.iter().sum::<i32>() + GAP * (pieces.len() as i32 - 1);
            let edge = if right_min < r.right() - 10 { right_min - 20 } else { r.right() - 16 };
            let mut ix = edge - total;
            right_min = ix;
            for ((kind, text), w) in pieces.iter().zip(&widths) {
                self.tx.draw_ui(cv, ix, y, text, dim, r, false);
                self.layout.status_info.push((Rect::new(ix - 4, r.y + 3, w + 8, r.h - 6), *kind));
                ix += w + GAP;
            }
        }

        // Transient message gets whatever room is left between the corners, ellipsized.
        if let Some((m, t)) = &self.status {
            if t.elapsed() < std::time::Duration::from_secs(6) {
                let room = right_min - 16 - x;
                let msg = fit(&mut self.tx, m, room);
                if room > 40 {
                    self.tx.draw_ui(cv, x, y, &msg, c.status_fg, Rect::new(x, r.y, room.max(0), r.h), false);
                }
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
        let Some((_, kind)) = self.layout.status_btns.iter().find(|(r, _)| r.contains(x, y)).copied() else { return true };
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
            StatusBtn::Ram | StatusBtn::Position | StatusBtn::Indent | StatusBtn::Language => {}
        }
        true
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
        assert_eq!(m.filtered, [0, 1, 2, 6], "3 recents, then Abrir pasta…");
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
        Entry::Branch(Branch { name: name.into(), remote, current: false, author: author.into(), when: 0, subject: subject.into() })
    }

    #[test]
    fn branch_search_matches_name_author_and_subject_and_wraps_selection() {
        let e = vec![br("main", "Ana", "init", false), br("feat/login", "Beto", "add form", false), br("origin/feat/login", "Beto", "add form", true)];
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
        g(&["commit", "-q", "-m", "primeiro commit"]);
        g(&["branch", "feature/x"]);
        let b = list_branches(&base);
        let names: Vec<&str> = b.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main") && names.contains(&"feature/x"), "{names:?}");
        let main = b.iter().find(|b| b.name == "main").unwrap();
        assert!(main.current && main.author == "Tester" && main.subject == "primeiro commit");
        let _ = std::fs::remove_dir_all(&base);
    }
}
