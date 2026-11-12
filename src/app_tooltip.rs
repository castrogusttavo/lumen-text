//! Hover tooltips: top and bottom bar buttons, tabs (full path, close button), sidebar rows whose
//! name is cut off, bottom panel tabs and the Git panel buttons.
//!
//! The state is just "which target is under the pointer and since when" ([`Tooltip`]); the
//! rect, the text and the position are resolved again on every draw, so a resize, a changed
//! label or a shortcut remapped in `keybindings.json` shows up at once. After the pointer rests
//! on a target for [`TOOLTIP_DELAY`] a small rounded card fades in (above the target in the bottom
//! bar, below it elsewhere, always inside the window). Leaving the target, clicking or
//! pressing a key closes it; moving straight to a neighbouring target shows its tooltip with no
//! new delay. To give another group of buttons a tooltip: add a [`TipKey`] variant, return it
//! from [`App::tip_target`] and describe it in [`App::tip_text`].

use crate::{
    app::{App, ROW_H, SidebarMode, TAB_H},
    app_ctxmenu::Act,
    app_gitpanel::{Group, Hit, Tab as GitTab},
    app_overlay::Overlay,
    app_panel::PanelTab,
    app_topbar::{MenuKind, StatusBtn},
    theme::Rgba,
    ui::{Canvas, Rect},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub const TOOLTIP_DELAY: Duration = Duration::from_millis(450);
/// Length of the fade-in.
pub const FADE: Duration = Duration::from_millis(120);
const PAD_X: i32 = 10;
const PAD_Y: i32 = 6;
const LINE: i32 = 18;
const GAP: i32 = 6;
const WIN_M: i32 = 6;
/// Widest card (long subjects and paths wrap).
const MAX_W: i32 = 460;

/// What the pointer is resting on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TipKey {
    Top(MenuKind),
    Status(StatusBtn),
    /// A document tab (index into `App::docs`) and its close button.
    Tab(usize),
    TabClose(usize),
    /// A row of the file tree (index into `Tree::rows`).
    SideRow(usize),
    PanelTab(PanelTab),
    /// A button or row of the Git panel (history rows included).
    Git(Hit),
    /// A row of the outline sidebar ("Outline"): index into the active document's symbols.
    Outline(usize),
    /// An item of the open context menu that needs a word of explanation.
    Ctx(Act),
    /// A button of the AI panel or of the Git | AI switch.
    Ai(crate::app_aipanel::AiHit),
    /// The Tests panel's target selector and the buttons a log-only target disables.
    Tests(crate::app_tests::Tip),
    /// A tab or button of the terminal strip in the bottom panel.
    Term(crate::app_terms::TermHit),
    /// The explorer's refresh button and the "Polling (NFS)" hint.
    Fs(crate::app_fswatch::FsTip),
}

/// Pointer-rest state (the text is resolved while drawing).
#[derive(Clone, Debug)]
pub struct Tooltip {
    pub key: TipKey,
    pub since: Instant,
    pub shown: bool,
    /// When the card became visible (the fade-in runs from here).
    pub shown_at: Option<Instant>,
    /// Closed by a click or a key press; stays closed until the pointer leaves the target.
    pub dismissed: bool,
}

/// What a tooltip says: a name, an optional shortcut (dim, right of the name) and an optional
/// second line.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TipText {
    pub title: String,
    pub keys: Option<String>,
    pub detail: Option<String>,
    /// More dim lines under the detail (hash of a commit, ...).
    pub more: Vec<String>,
}

/// "ctrl+shift+g" -> "Ctrl+Shift+G".
pub fn pretty_combo(combo: &str) -> String {
    combo
        .split('+')
        .map(|p| {
            let mut cs = p.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => "+".to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Shortcut shown for command `id`: the user's own binding if there is one, else the built-in
/// one (unless the user switched it off with "none").
pub fn shortcut_in(user: &HashMap<String, String>, id: &str) -> Option<String> {
    let mut mine: Vec<&String> = user.iter().filter(|(_, v)| v.as_str() == id).map(|(k, _)| k).collect();
    mine.sort();
    if let Some(c) = mine.first() {
        return Some(pretty_combo(c));
    }
    let d = crate::keymap::default_combo(id)?;
    (user.get(d).map(String::as_str) != Some("none")).then(|| pretty_combo(d))
}

/// Top-left corner of a `w` x `h` card for a target: centered on it, above (`below == false`) or
/// below it, flipped to the other side when it does not fit and clamped inside the window.
pub fn place(anchor: Rect, w: i32, h: i32, below: bool, win: (i32, i32)) -> (i32, i32) {
    let x = (anchor.x + (anchor.w - w) / 2).clamp(WIN_M, (win.0 - w - WIN_M).max(WIN_M));
    let above_y = anchor.y - GAP - h;
    let below_y = anchor.bottom() + GAP;
    let y = match below {
        true if below_y + h <= win.1 - WIN_M => below_y,
        true => above_y,
        false if above_y >= WIN_M => above_y,
        false => below_y,
    };
    (x, y.clamp(WIN_M, (win.1 - h - WIN_M).max(WIN_M)))
}

/// Git panel hit targets that get a tooltip (buttons, tabs and file rows; not the message box).
fn git_tip_wanted(h: Hit) -> bool {
    matches!(
        h,
        Hit::Refresh
            | Hit::Close
            | Hit::Tab(_)
            | Hit::Check(_)
            | Hit::Discard(_)
            | Hit::ConfirmDiscard(_)
            | Hit::Row(_)
            | Hit::CommitBtn
            | Hit::Opt
            | Hit::Fetch
            | Hit::Pull
            | Hit::Push
            | Hit::Amend
            | Hit::JobCancel
            | Hit::Commit(_)
            | Hit::Strategy(_)
            | Hit::ForceYes
    )
}

/// Opacity (0..=1) of the card `elapsed` after it appeared.
pub fn fade_alpha(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
}

fn faded(c: Rgba, a: f32) -> Rgba {
    Rgba(c.0, c.1, c.2, (c.3 as f32 * a) as u8)
}

/// Index of the tree row under a pointer at `y`, for a list whose first row starts at `top`, scrolled
/// `scroll` px, with `rows` rows of height `row_h`; `None` outside the rows.
pub fn sidebar_row_at(scroll: f64, top: i32, y: i32, row_h: i32, rows: usize) -> Option<usize> {
    if y < top || row_h <= 0 {
        return None;
    }
    let k = ((scroll.max(0.0) + (y - top) as f64) / row_h as f64).floor() as usize;
    (k < rows).then_some(k)
}

/// LSP `SymbolKind` names, for the outline tooltip.
pub fn symbol_kind_name(kind: u32) -> &'static str {
    match kind {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "Enum member",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "Type parameter",
        _ => "Symbol",
    }
}

/// The one-line explanation of a context menu action, for the ones that are not obvious.
pub fn act_hint(act: Act) -> Option<&'static str> {
    Some(match act {
        Act::Cut => "Marks for moving: paste into another folder (Esc cancels)",
        Act::Copy => "Copies so you can paste into another folder",
        Act::Paste => "Pastes here what was copied or cut",
        Act::Duplicate => "Creates a copy next to it, with “(copy)” in the name",
        Act::Trash => "Sends to the system trash; you can restore it from there",
        Act::Delete => "Deletes permanently, bypassing the trash. Cannot be undone",
        Act::CopyPath => "Copies the full path to the clipboard",
        Act::OpenSide => "Opens the file in a split pane beside this one",
        Act::CloseOthers => "Closes the other tabs; those with unsaved changes ask first",
        Act::CloseAll => "Closes all tabs except pinned ones",
        Act::TogglePin => "Pinned tabs stay on the left and survive “Close All”",
        Act::Markdown => "Shows the rendered text beside the editor",
        Act::LocalHistory => "Earlier versions of this file, kept on every save: compare and restore",
        Act::NewWindow => {
            "Opens this tab in a window of its own, unsaved changes included (or drag the tab out of the window)"
        }
        Act::CherryPick => "Applies this commit's changes on top of the current branch (git cherry-pick)",
        Act::Checkout => "Switches to the branch at this commit, or detaches HEAD there",
        Act::Revert => "Adds a commit that undoes this one (git revert)",
        Act::StashFiles => "Sets these changes aside (git stash push -- files); apply them later from the Stashes list",
        Act::BranchHere => "Creates a branch at this commit and switches to it",
        Act::TagHere => "Tags this commit (annotated when you give a message)",
        Act::UndoCommit => "Removes the last commit and keeps its changes staged (git reset --soft HEAD~1)",
        Act::Run("AiSendSelection") => "Mentions the selected lines (file#L3-9) in the connected Claude Code",
        Act::Run("ReopenClosedTab") => {
            "Reopens the most recently closed tab where it was: same pane, cursor and scroll"
        }
        Act::CopyHash | Act::Run(_) => return None,
        Act::NewFile | Act::NewFolder | Act::Rename | Act::CloseTab => return None,
    })
}

/// "2026-11-09 14:03" in local time.
pub fn local_datetime(ts: i64) -> String {
    let (y, mo, d, h, mi, _) = crate::paths::local_time(ts);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

/// Greedy word wrap of `text` into lines no wider than `max` px (`width` measures a string); a word
/// wider than the line is cut by characters. At most `max_lines` lines, the last one ellipsized.
pub fn wrap_tip(text: &str, max: i32, max_lines: usize, width: &mut dyn FnMut(&str) -> i32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let push_word = |cur: &mut String, lines: &mut Vec<String>, word: &str, width: &mut dyn FnMut(&str) -> i32| {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if width(&cand) <= max {
            *cur = cand;
            return;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(cur));
        }
        // The word alone may be too long: break it by characters.
        let mut piece = String::new();
        for ch in word.chars() {
            let mut next = piece.clone();
            next.push(ch);
            if width(&next) > max && !piece.is_empty() {
                lines.push(std::mem::take(&mut piece));
            }
            piece.push(ch);
        }
        *cur = piece;
    };
    for word in text.split_whitespace() {
        push_word(&mut cur, &mut lines, word, width);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines.max(1) {
        lines.truncate(max_lines.max(1));
        let last = lines.pop().unwrap_or_default();
        let mut l = format!("{last}…");
        while width(&l) > max && l.chars().count() > 2 {
            let mut c: Vec<char> = l.chars().collect();
            c.pop();
            c.pop();
            l = c.into_iter().collect::<String>() + "…";
        }
        lines.push(l);
    }
    lines
}

/// Top-left corner of a card beside `anchor` (to its right, else its left), vertically centered on
/// it and kept inside the window: for context menu items, whose neighbours are other items.
pub fn place_side(anchor: Rect, w: i32, h: i32, win: (i32, i32)) -> (i32, i32) {
    let right_x = anchor.right() + GAP;
    let x = if right_x + w <= win.0 - WIN_M { right_x } else { (anchor.x - GAP - w).max(WIN_M) };
    let y = (anchor.y + (anchor.h - h) / 2).clamp(WIN_M, (win.1 - h - WIN_M).max(WIN_M));
    (x, y)
}

/// Tooltip of a history row: the full subject, who and when, and the whole hash.
pub fn commit_tip(c: &crate::app_gitpanel::LogEntry, now: i64) -> TipText {
    let when = format!("{} ({})", local_datetime(c.ts), crate::git::ago((now - c.ts).max(0)));
    TipText {
        title: c.subject.clone(),
        keys: None,
        detail: Some(format!("{} · {when}", c.author)),
        more: vec![c.hash.clone()],
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl App {
    pub(crate) fn shortcut_text(&self, id: &str) -> Option<String> {
        shortcut_in(&self.keymap.entries(), id)
    }

    /// The target under the pointer, its rect and whether the card goes below it.
    pub fn tip_target(&mut self) -> Option<(TipKey, Rect, bool)> {
        let (x, y) = self.mouse;
        if self.dragging || self.dragging_minimap || self.drag_bar.is_some() {
            return None;
        }
        // An open context menu explains the items that need it.
        if let Overlay::Ctx(m) = &self.overlay {
            if m.confirm.is_some() || m.unsaved.is_some() {
                return None;
            }
            let &(r, i) = m.rects.iter().find(|(r, _)| r.contains(x, y))?;
            return match m.entries.get(i) {
                Some(crate::app_ctxmenu::Entry::Item { act, .. }) if act_hint(*act).is_some() => {
                    Some((TipKey::Ctx(*act), r, false))
                }
                _ => None,
            };
        }
        if !self.overlay.is_none() || self.tab_dragging() || self.sidebar_resizing() {
            return None;
        }
        let l = &self.layout;
        if let Some((r, k)) = l.top_btns.iter().find(|(r, k)| r.contains(x, y) && *k != MenuKind::Browse) {
            return Some((TipKey::Top(*k), *r, true));
        }
        if let Some((r, k)) = l.status_btns.iter().chain(l.status_info.iter()).find(|(r, _)| r.contains(x, y)) {
            return Some((TipKey::Status(*k), *r, false));
        }
        if let Some((tab, close, i)) = l.tabs.iter().find(|(r, _, _)| r.contains(x, y)).copied() {
            return Some(if close.contains(x, y) {
                (TipKey::TabClose(i), close, true)
            } else {
                (TipKey::Tab(i), tab, true)
            });
        }
        if let Some((r, t)) = l.panel_tabs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((TipKey::PanelTab(*t), *r, false));
        }
        if let Some((r, h)) = self.terms_hit_at(x, y) {
            return Some((TipKey::Term(h), r, false));
        }
        if let Some((r, h)) = self.aipanel_tip_target(x, y) {
            return Some((TipKey::Ai(h), r, true));
        }
        if let Some((r, k)) = self.tests_tip_at(x, y) {
            return Some((TipKey::Tests(k), r, true));
        }
        if let Some((k, r, below)) = self.fswatch_tip_at(x, y) {
            return Some((TipKey::Fs(k), r, below));
        }
        if let Some(gp) = &self.gitpanel
            && self.gitpanel_rect().contains(x, y)
        {
            return gp
                .hit_at(x, y)
                .filter(|h| git_tip_wanted(*h))
                .and_then(|h| gp.hits.iter().rev().find(|(_, k)| *k == h).map(|(r, _)| (TipKey::Git(h), *r, true)));
        }
        let side = self.layout.sidebar;
        if self.sidebar_open
            && self.sidebar_mode == SidebarMode::Outline
            && side.contains(x, y)
            && x < (side.right() - 15) as f64
        {
            let top = side.y + TAB_H;
            let n = self.docs.get(self.active).map_or(0, |d| d.symbols.len());
            let k = sidebar_row_at(self.tree.scroll, top, y as i32, ROW_H, n)?;
            let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
            let ry = top - (self.tree.scroll as i32 % ROW_H) + (k - first) as i32 * ROW_H;
            return Some((TipKey::Outline(k), Rect::new(side.x + 8, ry + 1, side.w - 17, ROW_H - 2), true));
        }
        if self.sidebar_open
            && self.sidebar_mode == SidebarMode::Files
            && side.contains(x, y)
            && x < (side.right() - 15) as f64
        {
            let top = side.y + TAB_H;
            let k = sidebar_row_at(self.tree.scroll, top, y as i32, ROW_H, self.tree.rows.len())?;
            // Only rows whose name does not fit (the tooltip is how to read the rest).
            let (name, depth) = {
                let row = &self.tree.rows[k];
                (row.name.clone(), row.depth)
            };
            // Same geometry as `draw_sidebar`: cut (drawn with "…") or scrolled out on the left.
            let text_x = side.x + 18 + depth as i32 * 14 + 38 - self.tree.hscroll as i32;
            let room = crate::app_sidebar::name_room(side, text_x, false);
            if self.tx.ui_width(&name, false) > room || text_x < side.x {
                let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
                let ry = top - (self.tree.scroll as i32 % ROW_H) + (k - first) as i32 * ROW_H;
                return Some((TipKey::SideRow(k), Rect::new(side.x + 8, ry + 1, side.w - 17, ROW_H - 2), true));
            }
        }
        None
    }

    /// The words for a target.
    pub fn tip_text(&mut self, key: TipKey) -> TipText {
        let t = |title: &str, keys: Option<String>, detail: Option<String>| TipText {
            title: title.to_string(),
            keys,
            detail,
            more: Vec::new(),
        };
        match key {
            TipKey::Ai(h) => self.aipanel_tip(h),
            TipKey::Tests(k) => self.tests_tip(k),
            TipKey::Term(h) => self.terms_tip(h),
            TipKey::Fs(k) => self.fswatch_tip(k),
            TipKey::Outline(i) => match self.docs.get(self.active).and_then(|d| d.symbols.get(i)) {
                Some(s) => t(&s.name, None, Some(format!("{} · line {}", symbol_kind_name(s.kind), s.sl + 1))),
                None => TipText::default(),
            },
            TipKey::Ctx(act) => {
                let label = match &self.overlay {
                    Overlay::Ctx(m) => m.entries.iter().find_map(|e| match e {
                        crate::app_ctxmenu::Entry::Item { act: a, label, .. } if *a == act => Some(label.to_string()),
                        _ => None,
                    }),
                    _ => None,
                };
                t(&label.unwrap_or_default(), None, act_hint(act).map(str::to_string))
            }
            TipKey::Top(MenuKind::Branch) => match &self.gitbg.worktree_main {
                Some(main) => t(
                    "Switch branch",
                    None,
                    Some(format!(
                        "{} · linked worktree of {}",
                        self.branch.clone().unwrap_or_default(),
                        crate::app_topbar::tilde(main)
                    )),
                ),
                None => t("Switch branch", None, self.branch.clone()),
            },
            TipKey::Top(_) => t("Switch project", None, Some(crate::app_topbar::tilde(&self.root))),
            TipKey::Tab(i) => match self.docs.get(i) {
                Some(d) => {
                    let path = d.path().map_or_else(|| "File not saved yet".to_string(), crate::app_topbar::tilde);
                    TipText {
                        title: d.name.clone(),
                        keys: d.buf.is_dirty().then(|| "unsaved".to_string()),
                        detail: Some(path),
                        more: Vec::new(),
                    }
                }
                None => TipText::default(),
            },
            TipKey::TabClose(i) => {
                let dirty = self.docs.get(i).is_some_and(|d| d.buf.is_dirty());
                t("Close", self.shortcut_text("CloseTab"), dirty.then(|| "Has unsaved changes".to_string()))
            }
            TipKey::SideRow(k) => match self.tree.rows.get(k) {
                Some(r) => {
                    let rel = self.rel(&r.path);
                    TipText {
                        title: r.name.clone(),
                        keys: None,
                        detail: (rel != r.name).then_some(rel),
                        more: Vec::new(),
                    }
                }
                None => TipText::default(),
            },
            TipKey::PanelTab(PanelTab::Output) => t("Output", None, Some("Results of tasks and tests".into())),
            TipKey::PanelTab(PanelTab::Terminal) => t("Terminal", self.shortcut_text("OpenTerminal"), None),
            TipKey::PanelTab(PanelTab::Tests) => {
                t("Tests", self.shortcut_text("TestAll"), Some("Run tests and browse the results".into()))
            }
            TipKey::PanelTab(PanelTab::Debug) => t("Debug", None, Some("Stack, variables and output".into())),
            TipKey::PanelTab(PanelTab::Build) => t(
                "Build & Run",
                self.shortcut_text("BuildBuild"),
                Some("Build and run the project, jump to the compiler's problems".into()),
            ),
            TipKey::Git(h) => self.git_tip_text(h),
            TipKey::Status(k) => match k {
                StatusBtn::Sidebar => t("Toggle sidebar", self.shortcut_text("ToggleSidebar"), None),
                StatusBtn::Search => t("Search in project", self.shortcut_text("ProjectSearch"), None),
                StatusBtn::Problems => {
                    let (e, w) = self.diag_counts();
                    let detail = if e + w == 0 {
                        "No problems".to_string()
                    } else {
                        format!("{} · {}", plural(e, "error", "errors"), plural(w, "warning", "warnings"))
                    };
                    t("Problems", None, Some(detail))
                }
                StatusBtn::Terminal => t("Terminal", self.shortcut_text("OpenTerminal"), None),
                StatusBtn::Debug => t("Debugger", None, Some("Stack, variables and output".into())),
                StatusBtn::Git => {
                    let n = self.git_status.len();
                    let title = if n == 0 {
                        "No changes · Git panel".to_string()
                    } else {
                        format!("{} · Git panel", plural(n, "changed file", "changed files"))
                    };
                    // Ahead / behind of the branch, from the panel or the background refresh.
                    let detail = self
                        .git_counts()
                        .filter(|i| i.upstream.is_some())
                        .map(|i| format!("{}: ↑{} ↓{}", i.upstream.unwrap_or_default(), i.ahead, i.behind));
                    TipText { title, keys: self.shortcut_text("GitPanel"), detail, more: Vec::new() }
                }
                StatusBtn::Ram => {
                    self.mem_cache.0 = Instant::now() - Duration::from_secs(10); // fresh numbers
                    let mut full = self.memory_text();
                    if let Some(per) = self.lspx_ram_detail() {
                        full = format!("{full} — {per}");
                    }
                    t("Memory in use", None, Some(full))
                }
                StatusBtn::Position => t("Go to line…", self.shortcut_text("GotoLine"), Some("Cursor position".into())),
                StatusBtn::Indent => {
                    let d = self.docs.get(self.active);
                    let now = d.map(|d| {
                        if d.ed.indent.contains('\t') {
                            "Tab".to_string()
                        } else {
                            plural(d.ed.indent.len(), "space", "spaces")
                        }
                    });
                    t("Change indentation…", None, now.map(|n| format!("Current: {n}")))
                }
                StatusBtn::Update => {
                    let (title, detail) = self.update_indicator_tip();
                    t(&title, None, detail)
                }
                StatusBtn::Build => t("Build", self.shortcut_text("BuildBuild"), Some(self.build_status_tip())),
                StatusBtn::Ai => t(
                    "AI agent",
                    self.shortcut_text("AiPanel"),
                    Some(format!(
                        "{} — click to open the AI panel",
                        self.ai.badge().map_or_else(|| "Agent".to_string(), |(b, _)| b)
                    )),
                ),
                StatusBtn::Inline => {
                    t("Inline completion", self.shortcut_text("InlineSuggest"), Some(self.inline_status_tip()))
                }
                StatusBtn::Language => self.language_tip(),
                StatusBtn::Encoding => t(
                    "Reopen or save with another encoding…",
                    None,
                    self.docs.get(self.active).map(|d| format!("Current: {}", d.buf.encoding.label())),
                ),
                StatusBtn::Project => self.projcfg_tip(),
            },
        }
    }

    fn git_tip_text(&self, h: Hit) -> TipText {
        let t = |title: &str, keys: Option<String>, detail: Option<String>| TipText {
            title: title.to_string(),
            keys,
            detail,
            more: Vec::new(),
        };
        let change = |i: usize| self.gitpanel.as_ref().and_then(|gp| gp.changes.get(i));
        match h {
            Hit::Refresh => t("Refresh", None, Some("Re-reads the git status and history".into())),
            Hit::Close => t("Close Git panel", self.shortcut_text("GitPanel"), None),
            Hit::Tab(GitTab::Changes) => t("Changes", None, Some("Modified, new and staged files".into())),
            Hit::Tab(GitTab::History) => t("History", None, Some("Recent commits".into())),
            Hit::Check(i) => match change(i) {
                Some(c) if c.group == Group::Staged => t("Unstage", None, Some(c.path.clone())),
                Some(c) => t("Stage", None, Some(c.path.clone())),
                None => TipText::default(),
            },
            Hit::Discard(i) => t("Discard changes", None, change(i).map(|c| c.path.clone())),
            Hit::ConfirmDiscard(i) => t("Click to discard for good", None, change(i).map(|c| c.path.clone())),
            Hit::Row(i) => match change(i) {
                Some(c) => TipText {
                    title: c.path.rsplit('/').next().unwrap_or(&c.path).to_string(),
                    keys: None,
                    detail: Some(c.path.clone()),
                    more: Vec::new(),
                },
                None => TipText::default(),
            },
            Hit::Commit(i) => match self.gitpanel.as_ref().and_then(|g| g.commits.get(i)) {
                Some(c) => commit_tip(c, crate::app_gitpanel::now_secs()),
                None => TipText::default(),
            },
            Hit::Strategy(st) => t(st.label(), None, Some(st.hint().to_string())),
            Hit::ForceYes => t("Force push", None, Some("git push --force-with-lease".into())),
            Hit::CommitBtn => t("Commit", Some("Ctrl+Enter".into()), Some("Commits what is staged".into())),
            Hit::Opt => t(
                "Include everything if nothing is staged",
                None,
                Some("With nothing staged, the commit takes all changes".into()),
            ),
            Hit::Fetch => t("Fetch", None, Some("Downloads news from the remote without touching your files".into())),
            Hit::Pull => {
                let n = self.gitpanel.as_ref().map_or(0, |g| g.sync.behind);
                let how = match crate::gitsync::PullStrategy::from_name(&self.config.git_pull_strategy) {
                    Some(st) => format!("Strategy: {} · right-click to change", st.label()),
                    None => "Right-click to choose merge, rebase or fast-forward only".to_string(),
                };
                TipText {
                    title: "Pull".into(),
                    keys: None,
                    detail: Some(if n > 0 {
                        format!("{n} new commit(s) on the remote")
                    } else {
                        "Fetches and integrates the remote commits".to_string()
                    }),
                    more: vec![how],
                }
            }
            Hit::Push => {
                let g = self.gitpanel.as_ref().map(|g| (g.sync.upstream.clone(), g.sync.ahead));
                t(
                    "Push",
                    None,
                    Some(match g {
                        Some((None, _)) => "Branch has no upstream: asks before publishing".to_string(),
                        Some((Some(u), n)) => format!("{n} commit(s) to push to {u}"),
                        None => String::new(),
                    }),
                )
            }
            Hit::Amend => t(
                "Amend the last commit",
                None,
                Some("Rewrites the previous commit with the message and what is staged".into()),
            ),
            Hit::JobCancel => t("Cancel the operation", None, None),
            _ => TipText::default(),
        }
    }

    /// Follow the pointer: arm, keep or drop the tooltip. Called on every pointer move.
    pub fn update_tooltip(&mut self) {
        let Some((key, _, _)) = self.tip_target() else {
            if self.tooltip.take().is_some() {
                self.request_redraw();
            }
            return;
        };
        match &mut self.tooltip {
            Some(t) if t.key == key => {}
            old => {
                // Neighbour to neighbour: no second delay, like native toolbars.
                let warm = old.as_ref().is_some_and(|t| t.shown && !t.dismissed);
                let now = Instant::now();
                // A warm card appears at full opacity at once.
                *old = Some(Tooltip {
                    key,
                    since: now,
                    shown: warm,
                    shown_at: warm.then(|| now - FADE),
                    dismissed: false,
                });
                self.request_redraw();
            }
        }
    }

    /// Click or key press: hide it until the pointer leaves the target.
    pub fn dismiss_tooltip(&mut self) {
        if let Some(t) = &mut self.tooltip
            && (!t.dismissed || t.shown)
        {
            t.dismissed = true;
            t.shown = false;
            t.shown_at = None;
            self.request_redraw();
        }
    }

    /// The pointer left the window.
    pub fn tooltip_leave(&mut self) {
        if self.tooltip.take().is_some() {
            self.request_redraw();
        }
    }

    /// Timer: show the card once the pointer has rested long enough, then drive the fade-in.
    pub fn tooltip_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some(t) = &mut self.tooltip else { return };
        if t.dismissed {
            return;
        }
        if t.shown {
            if t.shown_at.is_some_and(|s| now.saturating_duration_since(s) < FADE) {
                self.request_redraw();
                want(now + Duration::from_millis(16));
            }
            return;
        }
        let due = t.since + TOOLTIP_DELAY;
        if due <= now {
            t.shown = true;
            t.shown_at = Some(now);
            self.request_redraw();
            want(now + Duration::from_millis(16));
        } else {
            want(due);
        }
    }

    pub fn draw_tooltip(&mut self, cv: &mut Canvas) {
        let Some(t) = &self.tooltip else { return };
        if !t.shown || t.dismissed || !matches!(self.overlay, Overlay::None | Overlay::Ctx(_)) {
            return;
        }
        let key = t.key;
        // Re-resolve against the current layout; a stale target draws nothing.
        let Some((cur, anchor, below)) = self.tip_target() else { return };
        if cur != key {
            return;
        }
        let text = self.tip_text(key);
        if text.title.is_empty() {
            return;
        }
        let alpha = self.tooltip.as_ref().and_then(|t| t.shown_at).map_or(1.0, |s| fade_alpha(s.elapsed()));
        let c = self.theme.chrome;
        // Long subjects, paths and hints wrap instead of running off the window.
        let win = (self.size.0 as i32, self.size.1 as i32);
        let inner_max = (MAX_W.min(win.0 - 2 * WIN_M) - 2 * PAD_X).max(80);
        let kw = text.keys.as_ref().map_or(0, |k| self.tx.ui_width(k, false) + 14);
        let tx = &mut self.tx;
        let mut measure = |s: &str| tx.ui_width(s, false);
        let title = wrap_tip(&text.title, inner_max - kw, 3, &mut measure);
        let mut rest: Vec<String> = Vec::new();
        for d in text.detail.iter().chain(text.more.iter()) {
            rest.extend(wrap_tip(d, inner_max, 3, &mut measure));
        }
        let widest = title.iter().map(|l| measure(l)).max().unwrap_or(0) + kw;
        let widest = rest.iter().map(|l| measure(l)).fold(widest, i32::max);
        let w = widest + 2 * PAD_X;
        let lines = (title.len() + rest.len()) as i32;
        let h = 2 * PAD_Y + lines * LINE + if rest.is_empty() { 0 } else { 2 };
        let (x, y) =
            if matches!(key, TipKey::Ctx(_)) { place_side(anchor, w, h, win) } else { place(anchor, w, h, below, win) };
        let r = Rect::new(x, y, w, h);
        if alpha > 0.6 {
            cv.shadow(r, 8);
        }
        cv.card(r, 8, faded(c.widget_bg, alpha), faded(c.border, alpha));
        for (i, l) in title.iter().enumerate() {
            self.tx.draw_ui(cv, r.x + PAD_X, r.y + PAD_Y + i as i32 * LINE, l, faded(c.tab_active_fg, alpha), r, false);
        }
        if let (Some(k), Some(first)) = (&text.keys, title.first()) {
            let fw = self.tx.ui_width(first, false);
            self.tx.draw_ui(cv, r.x + PAD_X + fw + 14, r.y + PAD_Y, k, faded(c.tab_inactive_fg, alpha), r, false);
        }
        for (i, l) in rest.iter().enumerate() {
            self.tx.draw_ui(
                cv,
                r.x + PAD_X,
                r.y + PAD_Y + (title.len() + i) as i32 * LINE + 2,
                l,
                faded(c.tab_inactive_fg, alpha),
                r,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos_are_prettified() {
        assert_eq!(pretty_combo("ctrl+`"), "Ctrl+`");
        assert_eq!(pretty_combo("ctrl+b"), "Ctrl+B");
        assert_eq!(pretty_combo("ctrl+shift+g"), "Ctrl+Shift+G");
        assert_eq!(pretty_combo("alt+left"), "Alt+Left");
        assert_eq!(pretty_combo("f5"), "F5");
    }

    #[test]
    fn shortcut_follows_user_bindings_then_defaults() {
        let mut user = HashMap::new();
        assert_eq!(shortcut_in(&user, "ToggleSidebar").as_deref(), Some("Ctrl+B"));
        assert_eq!(shortcut_in(&user, "OpenTerminal").as_deref(), Some("Ctrl+`"));
        user.insert("ctrl+shift+b".to_string(), "ToggleSidebar".to_string());
        user.insert("ctrl+b".to_string(), "none".to_string());
        assert_eq!(shortcut_in(&user, "ToggleSidebar").as_deref(), Some("Ctrl+Shift+B"));
        user.clear();
        user.insert("ctrl+b".to_string(), "none".to_string());
        assert_eq!(shortcut_in(&user, "ToggleSidebar"), None, "default switched off");
        assert_eq!(shortcut_in(&user, "NoSuchCommand"), None);
    }

    #[test]
    fn card_goes_above_in_the_bottom_bar_and_below_in_the_top_bar() {
        let win = (800, 600);
        let status_btn = Rect::new(100, 575, 30, 20);
        let (x, y) = place(status_btn, 80, 30, false, win);
        assert!(y + 30 <= status_btn.y, "above the button: {y}");
        assert_eq!(x, 100 + (30 - 80) / 2);
        let top_btn = Rect::new(10, 4, 100, 28);
        let (_, y) = place(top_btn, 120, 30, true, win);
        assert!(y >= top_btn.bottom(), "below the button: {y}");
    }

    #[test]
    fn card_stays_inside_the_window_and_flips_when_it_does_not_fit() {
        let win = (400, 300);
        let (x, _) = place(Rect::new(0, 275, 20, 20), 200, 30, false, win);
        assert_eq!(x, WIN_M, "clamped to the left edge");
        let (x, _) = place(Rect::new(390, 275, 10, 20), 200, 30, false, win);
        assert_eq!(x, 400 - 200 - WIN_M, "clamped to the right edge");
        // No room above: goes below.
        let (_, y) = place(Rect::new(50, 4, 40, 20), 100, 30, false, win);
        assert!(y >= 24);
        // No room below: goes above.
        let (_, y) = place(Rect::new(50, 280, 40, 20), 100, 30, true, win);
        assert!(y + 30 <= 280);
        // Taller than the window never leaves it.
        let (_, y) = place(Rect::new(50, 10, 40, 20), 100, 500, true, win);
        assert_eq!(y, WIN_M);
    }

    #[test]
    fn memory_text_sums_editor_and_servers() {
        assert_eq!(crate::app_topbar::memory_mb("RAM 13 MB + tsgo 65 MB"), 78.0);
        assert_eq!(crate::app_topbar::memory_mb("RAM 13 MB"), 13.0);
        assert_eq!(crate::app_topbar::memory_mb(""), 0.0);
    }

    #[test]
    fn plural_forms() {
        assert_eq!(plural(1, "changed file", "changed files"), "1 changed file");
        assert_eq!(plural(3, "changed file", "changed files"), "3 changed files");
    }

    #[test]
    fn fade_runs_from_transparent_to_opaque() {
        assert_eq!(fade_alpha(Duration::ZERO), 0.0);
        assert!((fade_alpha(FADE / 2) - 0.5).abs() < 1e-3);
        assert_eq!(fade_alpha(FADE), 1.0);
        assert_eq!(fade_alpha(FADE * 10), 1.0);
        assert_eq!(faded(Rgba(1, 2, 3, 200), 0.0).3, 0);
        assert_eq!(faded(Rgba(1, 2, 3, 200), 1.0), Rgba(1, 2, 3, 200));
    }

    #[test]
    fn sidebar_rows_follow_the_scroll() {
        // Rows of 28 px starting at y = 100, scrolled by 40 px: row 1 spans y 88..116 and row 2 starts at 116.
        assert_eq!(sidebar_row_at(0.0, 100, 99, 28, 10), None, "above the list");
        assert_eq!(sidebar_row_at(0.0, 100, 100, 28, 10), Some(0));
        assert_eq!(sidebar_row_at(0.0, 100, 128, 28, 10), Some(1));
        assert_eq!(sidebar_row_at(40.0, 100, 100, 28, 10), Some(1));
        assert_eq!(sidebar_row_at(40.0, 100, 115, 28, 10), Some(1));
        assert_eq!(sidebar_row_at(40.0, 100, 116, 28, 10), Some(2));
        assert_eq!(sidebar_row_at(0.0, 100, 100 + 28 * 3, 28, 3), None, "below the last row");
    }

    #[test]
    fn git_panel_tooltips_skip_the_message_box_and_commit_rows() {
        assert!(git_tip_wanted(Hit::Refresh) && git_tip_wanted(Hit::Close) && git_tip_wanted(Hit::Discard(2)));
        assert!(!git_tip_wanted(Hit::Msg) && !git_tip_wanted(Hit::CFile(1)));
        assert!(git_tip_wanted(Hit::Commit(0)), "history rows explain themselves now");
    }

    #[test]
    fn wrapping_keeps_words_and_cuts_giant_ones() {
        let mut w = |s: &str| s.chars().count() as i32 * 10;
        assert_eq!(wrap_tip("um dois três quatro", 100, 5, &mut w), ["um dois", "três", "quatro"]);
        assert_eq!(wrap_tip("short", 100, 5, &mut w), ["short"]);
        assert_eq!(wrap_tip("abcdefghijklmnop", 50, 5, &mut w), ["abcde", "fghij", "klmno", "p"]);
        let l = wrap_tip("a b c d e f g h i j k l", 30, 2, &mut w);
        assert_eq!(l.len(), 2);
        assert!(l[1].ends_with('…') && w(&l[1]) <= 30, "{l:?}");
        assert!(wrap_tip("", 100, 3, &mut w).is_empty());
    }

    #[test]
    fn side_card_prefers_the_right_and_flips_left() {
        let win = (800, 600);
        let item = Rect::new(100, 100, 150, 28);
        let (x, y) = place_side(item, 200, 40, win);
        assert_eq!(x, item.right() + GAP);
        assert_eq!(y, 100 + (28 - 40) / 2);
        let edge = Rect::new(600, 100, 190, 28);
        let (x, _) = place_side(edge, 200, 40, win);
        assert_eq!(x, edge.x - GAP - 200, "no room on the right: flips to the left");
        let (_, y) = place_side(Rect::new(100, 590, 150, 28), 200, 40, win);
        assert!(y + 40 <= 600 - WIN_M, "kept inside the window");
    }

    #[test]
    fn history_rows_describe_subject_author_date_and_hash() {
        let c = crate::app_gitpanel::LogEntry {
            hash: "0123456789abcdef0123456789abcdef01234567".into(),
            short: "0123456".into(),
            author: "Ana".into(),
            ts: 1_700_000_000,
            subject: "Fix the shipping calculation for kit orders".into(),
        };
        let tip = commit_tip(&c, 1_700_000_000 + 7200);
        assert_eq!(tip.title, c.subject, "the whole subject, not the cut-off row text");
        let d = tip.detail.unwrap();
        assert!(d.starts_with("Ana · 2023-11-1") && (d.contains("2 hours ago")), "{d}");
        assert_eq!(tip.more, [c.hash.clone()]);
        assert!(git_tip_wanted(Hit::Commit(3)), "history rows get a tooltip now");
    }

    #[test]
    fn outline_kinds_and_context_menu_hints() {
        assert_eq!(symbol_kind_name(12), "Function");
        assert_eq!(symbol_kind_name(5), "Class");
        assert_eq!(symbol_kind_name(11), "Interface");
        assert_eq!(symbol_kind_name(999), "Symbol");
        assert!(act_hint(Act::Trash).unwrap().contains("trash"));
        assert!(act_hint(Act::Delete).unwrap().contains("Cannot be undone"));
        assert_eq!(act_hint(Act::Rename), None, "self-explanatory items get no card");
        // Every action the menus can offer is either explained or deliberately plain.
        for t in [
            crate::app_ctxmenu::Target::File("/x".into()),
            crate::app_ctxmenu::Target::Folder("/x".into()),
            crate::app_ctxmenu::Target::Tab(0),
        ] {
            let f = crate::app_ctxmenu::Flags {
                has_clip: true,
                split: true,
                md: Some(false),
                pinned: false,
                has_path: true,
                reopen: true,
            };
            for e in crate::app_ctxmenu::entries_for(&t, &f) {
                if let crate::app_ctxmenu::Entry::Item { act, label, .. } = e {
                    let plain = matches!(act, Act::NewFile | Act::NewFolder | Act::Rename | Act::CloseTab);
                    assert_eq!(act_hint(act).is_none(), plain, "{label}");
                }
            }
        }
    }
}
