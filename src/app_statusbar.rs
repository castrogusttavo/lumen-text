//! The status bar: a slim strip under the cards, on the canvas, that keeps the state of the work
//! in sight and one click from its detail.
//!
//! Left: the branch (opens the branch picker), how far it is from its upstream (opens the Git
//! panel), the error and warning counts (open Problems) and the language server (a spinner while
//! it starts or indexes). Right: the Vim mode, the selection, line and column, indentation,
//! encoding, line ending and language of the active file; each one that has a command opens it.
//!
//! The geometry is `layout.status` (a zero-height rect while the bar is hidden by zen mode or the
//! View switch); hit rects go to `layout.status_btns` (clickable) and `layout.status_info`
//! (tooltip only), exactly like the other badges, so cursors and tooltips need nothing new. The
//! chip layout and what to drop on a narrow window are pure functions with tests.

use crate::{
    anim::lerp_rgba,
    app::{App, Focus},
    app_overlay::{Cmd, Overlay},
    app_shell::HotSlot,
    app_tooltip::TipText,
    app_topbar::{MenuKind, StatusBtn},
    ds, motion,
    theme::Rgba,
    ui::{Canvas, Rect, fit_ellipsis},
};

/// Height of the bar.
pub const STATUS_H: i32 = 22;
/// Margin at the window's edges.
const EDGE: i32 = 12;
/// Padding inside a chip, gap between chips, gap between the parts of a chip.
const PAD: i32 = 6;
const CHIP_GAP: i32 = 2;
const IN_GAP: i32 = 4;
const ICON_PX: i32 = 12;
const BRANCH_MAX: i32 = 180;
const LSP_MAX: i32 = 170;
/// Room kept between the left group and the right one.
const MID_GAP: i32 = 16;

// ───────────── pure model ─────────────

/// "3 selected", "2 cursors", "2 cursors, 14 selected"; `None` when there is nothing to say.
pub fn selection_label(chars: usize, cursors: usize) -> Option<String> {
    match (chars, cursors > 1) {
        (0, false) => None,
        (n, false) => Some(format!("{n} selected")),
        (0, true) => Some(format!("{cursors} cursors")),
        (n, true) => Some(format!("{cursors} cursors, {n} selected")),
    }
}

/// What the language servers are doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspState {
    /// Switched off in the settings.
    Off,
    /// No server runs (nothing to say).
    None,
    /// At least one is still starting.
    Starting,
    /// A server reports progress (indexing, loading): its text.
    Busy(String),
    /// Every server is ready: their name, or how many.
    Ready(String),
}

/// One server as the bar sees it: ready, name, the text of its running progress.
pub type Server<'a> = (bool, &'a str, Option<&'a str>);

pub fn lsp_state(enabled: bool, servers: &[Server]) -> LspState {
    if !enabled {
        return LspState::Off;
    }
    if let Some(t) = servers.iter().find_map(|s| s.2) {
        return LspState::Busy(t.to_string());
    }
    if servers.iter().any(|s| !s.0) {
        return LspState::Starting;
    }
    match servers {
        [] => LspState::None,
        [one] => LspState::Ready(one.1.to_string()),
        many => LspState::Ready(format!("{} servers", many.len())),
    }
}

/// Which chips survive in `room` px: the lowest `rank` goes first until the rest fits. `widths`
/// include the chips' own padding; `gap` is the space between two.
pub fn keep_mask(widths: &[i32], ranks: &[u8], room: i32, gap: i32) -> Vec<bool> {
    let mut keep = vec![true; widths.len()];
    let used = |keep: &[bool]| {
        let n = keep.iter().filter(|k| **k).count() as i32;
        widths.iter().zip(keep).filter(|(_, k)| **k).map(|(w, _)| *w).sum::<i32>() + (n - 1).max(0) * gap
    };
    while used(&keep) > room {
        let Some(drop) = (0..keep.len()).filter(|i| keep[*i]).min_by_key(|i| ranks[*i]) else { break };
        keep[drop] = false;
    }
    keep
}

// ───────────── drawing ─────────────

/// One piece of a chip. A `None` tint is the muted text color, which brightens under the pointer.
enum Part {
    Icon(&'static str, Option<Rgba>),
    Text(String, Option<Rgba>),
    Dot(Rgba),
    Spin(Rgba),
}

struct Chip {
    kind: StatusBtn,
    parts: Vec<Part>,
    /// Has a command (otherwise a read-out with a tooltip).
    click: bool,
    /// Shown as pressed (its menu is open).
    on: bool,
}

impl App {
    /// Height of the bar now: none in zen mode or when the View switch hides it.
    pub fn status_h(&self) -> i32 {
        if self.zen.active() || !self.config.view.show_status_bar { 0 } else { STATUS_H }
    }

    fn part_w(&mut self, p: &Part) -> i32 {
        match p {
            Part::Icon(..) | Part::Spin(_) => ICON_PX,
            Part::Dot(_) => 8,
            Part::Text(t, _) => self.tx.ui_width(t, false),
        }
    }

    fn chip_w(&mut self, chip: &Chip) -> i32 {
        let inner: i32 = chip.parts.iter().map(|p| self.part_w(p)).sum();
        2 * PAD + inner + (chip.parts.len() as i32 - 1).max(0) * IN_GAP
    }

    /// Paint `chip` into `b` and register its hit rect; true while it animates.
    fn paint_chip(&mut self, cv: &mut Canvas, b: Rect, chip: &Chip) -> bool {
        let c = self.theme.chrome;
        let r = self.layout.status;
        let hot = if chip.click {
            self.hot_fill(cv, HotSlot::Chip(chip.kind as usize), b, ds::R_CTRL, chip.on, 0.0)
        } else {
            0.0
        };
        let muted = lerp_rgba(c.tab_inactive_fg, c.tab_active_fg, hot);
        let bg = crate::app_shell::canvas_color(&self.theme);
        let mut busy = false;
        let mut x = b.x + PAD;
        for p in &chip.parts {
            let w = self.part_w(p);
            match p {
                Part::Icon(name, tint) => {
                    self.tx.icon(cv, name, x, r.y + (r.h - ICON_PX) / 2, ICON_PX, tint.unwrap_or(muted), r);
                }
                Part::Text(t, tint) => {
                    self.tx.draw_ui(cv, x, r.y + (r.h - 18) / 2, t, tint.unwrap_or(muted), b, false);
                }
                Part::Dot(col) => cv.fill_round(Rect::new(x, r.y + (r.h - 6) / 2, 6, 6), 3, *col),
                Part::Spin(col) => {
                    let (cx, cy) = (x + ICON_PX / 2, r.y + r.h / 2);
                    if motion::reduced() {
                        cv.fill_round(Rect::new(cx - 3, cy - 3, 6, 6), 3, *col);
                    } else {
                        crate::app_tests::spinner_t(cv, cx, cy, 5, *col, bg, crate::ui_kit::shimmer::clock(), r);
                        busy = true;
                    }
                }
            }
            x += w + IN_GAP;
        }
        if chip.click {
            self.layout.status_btns.push((b, chip.kind));
        } else {
            self.layout.status_info.push((b, chip.kind));
        }
        busy
    }

    /// Branch, sync, problems and language server.
    fn left_chips(&mut self) -> Vec<Chip> {
        let c = self.theme.chrome;
        let mut out = Vec::new();
        if let Some(b) = self.branch.clone() {
            let name = fit_ellipsis(&mut self.tx, &b, BRANCH_MAX, false);
            let linked = self.gitbg.worktree_main.is_some();
            let icon = if linked { "ui/git_worktree" } else { "ui/git_branch" };
            let on = matches!(&self.overlay, Overlay::TopMenu(m) if m.kind == MenuKind::Branch);
            out.push(Chip {
                kind: StatusBtn::Branch,
                parts: vec![Part::Icon(icon, linked.then_some(c.accent_text)), Part::Text(name, None)],
                click: true,
                on,
            });
            if let Some(i) = self.git_counts().filter(|i| i.upstream.is_some()) {
                let mut parts = Vec::new();
                if i.ahead > 0 {
                    parts.push(Part::Icon("ui/arrow_up", Some(c.accent_text)));
                    parts.push(Part::Text(i.ahead.to_string(), Some(c.accent_text)));
                }
                if i.behind > 0 {
                    parts.push(Part::Icon("ui/arrow_down", Some(c.warning)));
                    parts.push(Part::Text(i.behind.to_string(), Some(c.warning)));
                }
                if parts.is_empty() {
                    parts.push(Part::Icon("ui/check", None));
                }
                out.push(Chip { kind: StatusBtn::Sync, parts, click: true, on: self.gitpanel.is_some() });
            }
        }
        if self.config.status_visible("problems") {
            let (e, w) = self.diag_counts();
            let parts = if e == 0 && w == 0 {
                vec![Part::Icon("ui/check", None)]
            } else {
                let (ec, wc) = (if e > 0 { Some(c.error) } else { None }, if w > 0 { Some(c.warning) } else { None });
                vec![
                    Part::Icon("ui/x_circle", ec),
                    Part::Text(e.to_string(), ec),
                    Part::Icon("ui/warning", wc),
                    Part::Text(w.to_string(), wc),
                ]
            };
            out.push(Chip { kind: StatusBtn::Problems, parts, click: true, on: false });
        }
        if !crate::remote::is_remote(&self.root) {
            let servers: Vec<Server> = self
                .lsps
                .iter()
                .map(|l| (l.ready, l.name.as_str(), l.progress.values().next().map(String::as_str)))
                .collect();
            let state = lsp_state(self.lsp_enabled(), &servers);
            let chip = |parts| Chip { kind: StatusBtn::Lsp, parts, click: true, on: false };
            let text = |tx: &mut crate::ui::TextCtx, t: &str| fit_ellipsis(tx, t, LSP_MAX, false);
            match state {
                LspState::None => {}
                LspState::Off => out.push(chip(vec![Part::Text("LSP off".into(), None)])),
                LspState::Starting => {
                    out.push(chip(vec![Part::Spin(c.accent_text), Part::Text("Starting…".into(), None)]));
                }
                LspState::Busy(t) => {
                    let t = text(&mut self.tx, &t);
                    out.push(chip(vec![Part::Spin(c.accent_text), Part::Text(t, None)]));
                }
                LspState::Ready(n) => {
                    let n = text(&mut self.tx, &n);
                    out.push(chip(vec![Part::Dot(c.git_added), Part::Text(n, None)]));
                }
            }
        }
        out
    }

    /// Read-outs of the active file in display order, each with the rank that keeps it longest.
    fn right_chips(&self) -> Vec<(Chip, u8)> {
        let Some(d) = self.docs.get(self.active).filter(|d| {
            !d.showing_image() && d.sql.is_none() && d.mb.is_none() && !d.api.as_ref().is_some_and(|v| v.preview)
        }) else {
            return Vec::new();
        };
        let (l, col) = d.buf.line_col(d.ed.head());
        let vc = crate::editor::visual_col(&d.buf.line(l), col, d.ed.tab_w) + 1;
        let cursors = d.ed.extra.len() + 1;
        let chars: usize = std::iter::once(&d.ed.sel).chain(d.ed.extra.iter()).map(|s| s.anchor.abs_diff(s.head)).sum();
        let indent =
            if d.ed.indent.contains('\t') { "Tab".to_string() } else { format!("Spaces: {}", d.ed.indent.len()) };
        let eol = if d.buf.eol() == "\r\n" { "CRLF" } else { "LF" };
        let mut named: Vec<(&'static str, StatusBtn, String, u8)> = vec![
            ("position", StatusBtn::Position, format!("Ln {}, Col {}", l + 1, vc), 6),
            ("indent", StatusBtn::Indent, indent, 4),
            ("language", StatusBtn::Language, d.lang.unwrap_or("text").to_string(), 5),
            ("encoding", StatusBtn::Encoding, d.buf.encoding.short(), 3),
        ];
        named.retain(|(id, ..)| self.config.status_visible(id));
        named.sort_by_key(|(id, ..)| self.config.status_order.iter().position(|s| s == *id).unwrap_or(usize::MAX));
        let mut out: Vec<(Chip, u8)> = Vec::new();
        if let Some(t) = selection_label(chars, cursors) {
            out.push((
                Chip { kind: StatusBtn::Selection, parts: vec![Part::Text(t, None)], click: false, on: false },
                1,
            ));
        }
        for (_, kind, text, rank) in named {
            let at = out.len();
            out.push((Chip { kind, parts: vec![Part::Text(text, None)], click: true, on: false }, rank));
            // The line ending reads right after the encoding.
            if kind == StatusBtn::Encoding {
                let chip =
                    Chip { kind: StatusBtn::Eol, parts: vec![Part::Text(eol.into(), None)], click: false, on: false };
                out.insert(at + 1, (chip, 2));
            }
        }
        out
    }

    /// Paint the bar (call after the title bar, which resets the hit rects). True while a
    /// spinner turns.
    pub fn draw_statusbar(&mut self, cv: &mut Canvas) -> bool {
        let r = self.layout.status;
        if r.h <= 0 {
            return false;
        }
        cv.fill(r, crate::app_shell::canvas_color(&self.theme));
        let mut busy = false;
        let mut x = r.x + EDGE;
        for chip in self.left_chips() {
            let w = self.chip_w(&chip);
            busy |= self.paint_chip(cv, Rect::new(x, r.y + 2, w, r.h - 4), &chip);
            x += w + CHIP_GAP;
        }

        // Right group: fit what the window leaves, dropping the least useful first.
        let right = r.right() - EDGE;
        let vim = self.vim_label();
        let vim_w = vim.as_ref().map_or(0, |l| self.tx.ui_width(l, true) + 18 + 10);
        let chips = self.right_chips();
        let widths: Vec<i32> = chips.iter().map(|(c, _)| self.chip_w(c)).collect();
        let ranks: Vec<u8> = chips.iter().map(|(_, k)| *k).collect();
        let room = right - (x + MID_GAP) - vim_w;
        let keep = keep_mask(&widths, &ranks, room, CHIP_GAP);
        let total: i32 = widths.iter().zip(&keep).filter(|(_, k)| **k).map(|(w, _)| *w + CHIP_GAP).sum::<i32>();
        let mut rx = right - total + CHIP_GAP;
        if vim.is_some() && room >= 0 {
            self.draw_vim_status(cv, rx - vim_w);
        }
        for ((chip, _), (w, keep)) in chips.iter().zip(widths.iter().zip(&keep)) {
            if !keep {
                continue;
            }
            busy |= self.paint_chip(cv, Rect::new(rx, r.y + 2, *w, r.h - 4), chip);
            rx += w + CHIP_GAP;
        }
        busy
    }

    /// The Vim status text while the pill shows (modal editing on and the editor has the focus).
    fn vim_label(&self) -> Option<String> {
        if self.focus != Focus::Editor {
            return None;
        }
        self.vim.as_ref().map(|v| v.status())
    }

    /// A press on the language server chip: its log, or switch the servers on when they are off.
    pub fn statusbar_lsp_click(&mut self) {
        if self.lsp_enabled() || crate::remote::is_remote(&self.root) {
            self.run_cmd(Cmd::ShowLspLog);
        } else {
            self.run_cmd(Cmd::ToggleLsp);
        }
    }

    /// Tooltip of the chips only the status bar has.
    pub fn statusbar_tip(&self, k: StatusBtn) -> TipText {
        let t = |title: &str, keys: Option<String>, detail: Option<String>| TipText {
            title: title.to_string(),
            keys,
            detail,
            more: Vec::new(),
        };
        match k {
            StatusBtn::Branch => {
                let branch = self.branch.clone().unwrap_or_default();
                let detail = match &self.gitbg.worktree_main {
                    Some(main) => format!("{branch} · linked worktree of {}", crate::app_topbar::tilde(main)),
                    None => branch,
                };
                t("Switch branch", self.shortcut_text("BranchMenu"), Some(detail))
            }
            StatusBtn::Sync => {
                let detail = match self.git_counts().filter(|i| i.upstream.is_some()) {
                    Some(i) if i.ahead + i.behind == 0 => {
                        format!("{}: up to date", i.upstream.unwrap_or_default())
                    }
                    Some(i) => format!(
                        "{}: {} ahead, {} behind — click to open the Git panel",
                        i.upstream.unwrap_or_default(),
                        i.ahead,
                        i.behind
                    ),
                    None => "No upstream".to_string(),
                };
                t("Sync with upstream", self.shortcut_text("GitPanel"), Some(detail))
            }
            StatusBtn::Lsp => {
                let servers: Vec<Server> = self
                    .lsps
                    .iter()
                    .map(|l| (l.ready, l.name.as_str(), l.progress.values().next().map(String::as_str)))
                    .collect();
                let (title, detail) = match lsp_state(self.lsp_enabled(), &servers) {
                    LspState::Off => ("Language servers are off", "Click to turn them on".to_string()),
                    LspState::None => ("Language server", "None running".to_string()),
                    LspState::Starting => ("Language server", "Starting… — click for its log".to_string()),
                    LspState::Busy(p) => ("Language server", format!("{p} — click for its log")),
                    LspState::Ready(n) => ("Language server", format!("{n} ready — click for its log")),
                };
                t(title, None, Some(detail))
            }
            StatusBtn::Eol => {
                let crlf = self.docs.get(self.active).is_some_and(|d| d.buf.eol() == "\r\n");
                t("Line endings", None, Some(if crlf { "CRLF (Windows)" } else { "LF (Unix)" }.to_string()))
            }
            StatusBtn::Selection => {
                let d = self.docs.get(self.active);
                let cursors = d.map_or(1, |d| d.ed.extra.len() + 1);
                let chars: usize = d.map_or(0, |d| {
                    std::iter::once(&d.ed.sel).chain(d.ed.extra.iter()).map(|s| s.anchor.abs_diff(s.head)).sum()
                });
                t("Selection", None, Some(format!("{chars} characters, {cursors} cursor(s)")))
            }
            _ => TipText::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        app_unsaved::testkit::{lock, test_app, tmp},
        ui::Canvas,
    };

    fn frame(app: &mut App) {
        let (w, h) = (1400, 900);
        let mut px = vec![0u32; (w * h) as usize];
        let mut cv = Canvas { px: &mut px, w, h };
        app.size = (w as u32, h as u32);
        app.render_into(&mut cv, 0.016);
    }

    #[test]
    fn the_bar_sits_under_the_cards_and_carries_the_read_outs() {
        let _g = lock();
        let dir = tmp("statusbar-layout");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("one\ntwo\n");
        frame(&mut app);
        let l = &app.layout;
        assert_eq!((l.status.y, l.status.h, l.status.bottom()), (900 - STATUS_H, STATUS_H, 900));
        assert_eq!(l.card_outer.bottom(), l.status.y - ds::GAP, "cards float above the bar");
        assert_eq!(l.rail.bottom(), l.status.y, "the rail stops at the bar");
        for kind in [StatusBtn::Problems, StatusBtn::Position, StatusBtn::Indent, StatusBtn::Language] {
            let (r, _) = l.status_btns.iter().find(|(_, k)| *k == kind).unwrap_or_else(|| panic!("{kind:?} missing"));
            assert!(r.y >= l.status.y && r.bottom() <= l.status.bottom(), "{kind:?} inside the bar: {r:?}");
        }
        assert!(l.status_info.iter().any(|(_, k)| *k == StatusBtn::Eol), "the line ending reads out");
        // Problems and the cursor position keep their old actions.
        let pos = l.status_btns.iter().find(|(_, k)| *k == StatusBtn::Position).unwrap().0;
        assert!(app.status_click(pos.x as f64 + 3.0, pos.y as f64 + 3.0));
        assert!(!app.overlay.is_none(), "Go to line opens");
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hiding_the_bar_gives_its_row_back() {
        let _g = lock();
        let dir = tmp("statusbar-hide");
        let mut app = test_app(&dir);
        frame(&mut app);
        let shown = app.layout.card_outer;
        app.config.view.show_status_bar = false;
        frame(&mut app);
        assert_eq!(app.layout.status.h, 0);
        assert!(app.layout.status_btns.iter().all(|(_, k)| *k != StatusBtn::Position), "no stale chips");
        assert_eq!(app.layout.card_outer.h, shown.h + STATUS_H);
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_title_bar_has_menus_and_a_run_cluster() {
        use crate::app_topbar::{MenuKind, TopAct};
        let _g = lock();
        let dir = tmp("statusbar-menus");
        let mut app = test_app(&dir);
        frame(&mut app);
        let btns = app.layout.top_btns.clone();
        for i in 0..crate::app_menubar::MENUS.len() as u8 {
            assert!(btns.iter().any(|(_, k)| *k == MenuKind::Menu(i)), "menu {i} missing");
        }
        for a in [TopAct::Target, TopAct::Run, TopAct::Debug] {
            assert!(btns.iter().any(|(_, k)| *k == MenuKind::Act(a)), "{a:?} missing");
        }
        // No two buttons overlap, and all stay inside the bar.
        for (i, (a, _)) in btns.iter().enumerate() {
            assert!(a.x >= 0 && a.right() <= 1400 && a.bottom() <= app.layout.topbar.bottom());
            for (b, _) in &btns[i + 1..] {
                assert!(a.intersect(b).w <= 0 || a.intersect(b).h <= 0, "{a:?} overlaps {b:?}");
            }
        }
        // A menu opens with its commands, and a click on another title moves to that menu.
        let (r, _) = btns.iter().find(|(_, k)| *k == MenuKind::Menu(0)).unwrap();
        assert!(app.topbar_click(r.x as f64 + 2.0, r.y as f64 + 2.0));
        let Overlay::TopMenu(m) = &app.overlay else { panic!("File menu did not open") };
        assert!(m.entries.iter().any(|e| matches!(e, crate::app_topbar::Entry::Cmd(l, _, Cmd::Save) if l == "Save")));
        frame(&mut app);
        let (r2, _) = btns.iter().find(|(_, k)| *k == MenuKind::Menu(3)).unwrap();
        assert!(app.overlay_mouse(r2.x as f64 + 2.0, r2.y as f64 + 2.0));
        assert!(matches!(&app.overlay, Overlay::TopMenu(m) if m.kind == MenuKind::Menu(3)));
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn selection_text_counts_characters_and_cursors() {
        assert_eq!(selection_label(0, 1), None);
        assert_eq!(selection_label(12, 1).as_deref(), Some("12 selected"));
        assert_eq!(selection_label(0, 3).as_deref(), Some("3 cursors"));
        assert_eq!(selection_label(5, 2).as_deref(), Some("2 cursors, 5 selected"));
    }

    #[test]
    fn lsp_state_follows_servers_and_the_switch() {
        assert_eq!(lsp_state(false, &[(true, "a", None)]), LspState::Off);
        assert_eq!(lsp_state(true, &[]), LspState::None);
        assert_eq!(lsp_state(true, &[(false, "a", None)]), LspState::Starting);
        assert_eq!(lsp_state(true, &[(true, "a", Some("Indexing 3/9"))]), LspState::Busy("Indexing 3/9".into()));
        assert_eq!(lsp_state(true, &[(true, "rust-analyzer", None)]), LspState::Ready("rust-analyzer".into()));
        assert_eq!(lsp_state(true, &[(true, "a", None), (true, "b", None)]), LspState::Ready("2 servers".into()));
        // Progress wins over a server that is still starting.
        assert!(matches!(lsp_state(true, &[(false, "a", None), (true, "b", Some("x"))]), LspState::Busy(_)));
    }

    #[test]
    fn narrow_windows_drop_the_least_useful_chip_first() {
        // selection(1) position(6) indent(4) encoding(3) eol(2) language(5)
        let widths = [60, 70, 70, 50, 30, 60];
        let ranks = [1, 6, 4, 3, 2, 5];
        let all: i32 = widths.iter().sum::<i32>() + 5 * 2;
        assert_eq!(keep_mask(&widths, &ranks, all, 2), vec![true; 6], "all fit");
        let k = keep_mask(&widths, &ranks, all - 1, 2);
        assert_eq!(k, vec![false, true, true, true, true, true], "the selection goes first");
        let k = keep_mask(&widths, &ranks, 150, 2);
        assert!(k[1], "the position outlives everything");
        assert!(!k[0] && !k[4], "selection and line ending are gone");
        let used: i32 = widths.iter().zip(&k).filter(|(_, k)| **k).map(|(w, _)| *w).sum::<i32>()
            + (k.iter().filter(|k| **k).count() as i32 - 1) * 2;
        assert!(used <= 150);
        assert_eq!(keep_mask(&widths, &ranks, 0, 2), vec![false; 6]);
    }
}
