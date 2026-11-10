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
    /// A button or row of the Git panel.
    Git(Hit),
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
    matches!(h, Hit::Refresh | Hit::Close | Hit::Tab(_) | Hit::Check(_) | Hit::Discard(_) | Hit::ConfirmDiscard(_) | Hit::Row(_) | Hit::CommitBtn | Hit::Opt | Hit::Fetch | Hit::Pull | Hit::Push | Hit::Amend | Hit::JobCancel)
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

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl App {
    fn shortcut_text(&self, id: &str) -> Option<String> {
        shortcut_in(&self.keymap.entries(), id)
    }

    /// The target under the pointer, its rect and whether the card goes below it.
    pub fn tip_target(&mut self) -> Option<(TipKey, Rect, bool)> {
        let (x, y) = self.mouse;
        if !self.overlay.is_none() || self.dragging || self.dragging_minimap || self.drag_bar.is_some() {
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
            return Some(if close.contains(x, y) { (TipKey::TabClose(i), close, true) } else { (TipKey::Tab(i), tab, true) });
        }
        if let Some((r, t)) = l.panel_tabs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((TipKey::PanelTab(*t), *r, false));
        }
        if let Some(gp) = &self.gitpanel {
            if self.gitpanel_rect().contains(x, y) {
                return gp.hit_at(x, y).filter(|h| git_tip_wanted(*h)).and_then(|h| gp.hits.iter().rev().find(|(_, k)| *k == h).map(|(r, _)| (TipKey::Git(h), *r, true)));
            }
        }
        let side = self.layout.sidebar;
        if self.sidebar_open && self.sidebar_mode == SidebarMode::Files && side.contains(x, y) && x < (side.right() - 15) as f64 {
            let top = side.y + TAB_H;
            let k = sidebar_row_at(self.tree.scroll, top, y as i32, ROW_H, self.tree.rows.len())?;
            // Only rows whose name does not fit (the tooltip is how to read the rest).
            let (name, depth) = {
                let row = &self.tree.rows[k];
                (row.name.clone(), row.depth)
            };
            let text_x = side.x + 18 + depth as i32 * 14 + 38 - self.tree.hscroll as i32;
            let end = text_x + self.tx.ui_width(&name, false);
            if end > side.right() - 24 || text_x < side.x {
                let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
                let ry = top - (self.tree.scroll as i32 % ROW_H) + (k - first) as i32 * ROW_H;
                return Some((TipKey::SideRow(k), Rect::new(side.x + 8, ry + 1, side.w - 17, ROW_H - 2), true));
            }
        }
        None
    }

    /// The words for a target.
    pub fn tip_text(&mut self, key: TipKey) -> TipText {
        let t = |title: &str, keys: Option<String>, detail: Option<String>| TipText { title: title.to_string(), keys, detail };
        match key {
            TipKey::Top(MenuKind::Branch) => t("Trocar de branch", None, self.branch.clone()),
            TipKey::Top(_) => t("Trocar de projeto", None, Some(crate::app_topbar::tilde(&self.root))),
            TipKey::Tab(i) => match self.docs.get(i) {
                Some(d) => {
                    let path = d.path().map_or_else(|| "Arquivo ainda não salvo".to_string(), crate::app_topbar::tilde);
                    TipText { title: d.name.clone(), keys: d.buf.is_dirty().then(|| "não salvo".to_string()), detail: Some(path) }
                }
                None => TipText::default(),
            },
            TipKey::TabClose(i) => {
                let dirty = self.docs.get(i).is_some_and(|d| d.buf.is_dirty());
                t("Fechar", self.shortcut_text("CloseTab"), dirty.then(|| "Há alterações não salvas".to_string()))
            }
            TipKey::SideRow(k) => match self.tree.rows.get(k) {
                Some(r) => {
                    let rel = self.rel(&r.path);
                    TipText { title: r.name.clone(), keys: None, detail: (rel != r.name).then_some(rel) }
                }
                None => TipText::default(),
            },
            TipKey::PanelTab(PanelTab::Output) => t("Saída", None, Some("Resultado de tarefas e testes".into())),
            TipKey::PanelTab(PanelTab::Terminal) => t("Terminal", self.shortcut_text("OpenTerminal"), None),
            TipKey::PanelTab(PanelTab::Debug) => t("Depurar", None, Some("Pilha, variáveis e saída".into())),
            TipKey::Git(h) => self.git_tip_text(h),
            TipKey::Status(k) => match k {
                StatusBtn::Sidebar => t("Alternar sidebar", self.shortcut_text("ToggleSidebar"), None),
                StatusBtn::Search => t("Buscar no projeto", self.shortcut_text("ProjectSearch"), None),
                StatusBtn::Problems => {
                    let (e, w) = self.diag_counts();
                    let detail = if e + w == 0 { "Nenhum problema".to_string() } else { format!("{} · {}", plural(e, "erro", "erros"), plural(w, "aviso", "avisos")) };
                    t("Problemas", None, Some(detail))
                }
                StatusBtn::Terminal => t("Terminal", self.shortcut_text("OpenTerminal"), None),
                StatusBtn::Debug => t("Depuração", None, Some("Pilha, variáveis e saída".into())),
                StatusBtn::Git => {
                    let n = self.git_status.len();
                    let title = if n == 0 { "Sem alterações · Painel Git".to_string() } else { format!("{} · Painel Git", plural(n, "arquivo alterado", "arquivos alterados")) };
                    TipText { title, keys: self.shortcut_text("GitPanel"), detail: None }
                }
                StatusBtn::Ram => {
                    self.mem_cache.0 = Instant::now() - Duration::from_secs(10); // fresh numbers
                    let full = self.memory_text();
                    t("Memória em uso", None, Some(full))
                }
                StatusBtn::Position => t("Ir para linha…", self.shortcut_text("GotoLine"), Some("Posição do cursor".into())),
                StatusBtn::Indent => {
                    let d = self.docs.get(self.active);
                    let now = d.map(|d| if d.ed.indent.contains('\t') { "Tabulação".to_string() } else { plural(d.ed.indent.len(), "espaço", "espaços") });
                    t("Mudar indentação…", None, now.map(|n| format!("Atual: {n}")))
                }
                StatusBtn::Language => t("Mudar linguagem…", None, self.docs.get(self.active).map(|d| format!("Atual: {}", d.lang.unwrap_or("texto")))),
            },
        }
    }

    fn git_tip_text(&self, h: Hit) -> TipText {
        let t = |title: &str, keys: Option<String>, detail: Option<String>| TipText { title: title.to_string(), keys, detail };
        let change = |i: usize| self.gitpanel.as_ref().and_then(|gp| gp.changes.get(i));
        match h {
            Hit::Refresh => t("Atualizar", None, Some("Relê o status e o histórico do git".into())),
            Hit::Close => t("Fechar painel Git", self.shortcut_text("GitPanel"), None),
            Hit::Tab(GitTab::Changes) => t("Alterações", None, Some("Arquivos modificados, novos e no stage".into())),
            Hit::Tab(GitTab::History) => t("Histórico", None, Some("Commits recentes".into())),
            Hit::Check(i) => match change(i) {
                Some(c) if c.group == Group::Staged => t("Tirar do stage", None, Some(c.path.clone())),
                Some(c) => t("Colocar no stage", None, Some(c.path.clone())),
                None => TipText::default(),
            },
            Hit::Discard(i) => t("Descartar alterações", None, change(i).map(|c| c.path.clone())),
            Hit::ConfirmDiscard(i) => t("Clique para descartar de vez", None, change(i).map(|c| c.path.clone())),
            Hit::Row(i) => match change(i) {
                Some(c) => TipText { title: c.path.rsplit('/').next().unwrap_or(&c.path).to_string(), keys: None, detail: Some(c.path.clone()) },
                None => TipText::default(),
            },
            Hit::CommitBtn => t("Commit", Some("Ctrl+Enter".into()), Some("Grava o que está no stage".into())),
            Hit::Opt => t("Incluir tudo se nada no stage", None, Some("Sem nada no stage, o commit leva todas as alterações".into())),
            Hit::Fetch => t("Buscar (fetch)", None, Some("Baixa as novidades do remoto sem alterar seus arquivos".into())),
            Hit::Pull => {
                let n = self.gitpanel.as_ref().map_or(0, |g| g.sync.behind);
                t("Pull", None, Some(if n > 0 { format!("{n} commit(s) novos no remoto") } else { "Traz e integra os commits do remoto".to_string() }))
            }
            Hit::Push => {
                let g = self.gitpanel.as_ref().map(|g| (g.sync.upstream.clone(), g.sync.ahead));
                t("Push", None, Some(match g {
                    Some((None, _)) => "Branch sem upstream: pergunta antes de publicar".to_string(),
                    Some((Some(u), n)) => format!("{n} commit(s) a enviar para {u}"),
                    None => String::new(),
                }))
            }
            Hit::Amend => t("Emendar o último commit", None, Some("Reescreve o commit anterior com a mensagem e o que está no stage".into())),
            Hit::JobCancel => t("Cancelar a operação", None, None),
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
                *old = Some(Tooltip { key, since: now, shown: warm, shown_at: warm.then(|| now - FADE), dismissed: false });
                self.request_redraw();
            }
        }
    }

    /// Click or key press: hide it until the pointer leaves the target.
    pub fn dismiss_tooltip(&mut self) {
        if let Some(t) = &mut self.tooltip {
            if !t.dismissed || t.shown {
                t.dismissed = true;
                t.shown = false;
                t.shown_at = None;
                self.request_redraw();
            }
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
        if !t.shown || t.dismissed || !matches!(self.overlay, Overlay::None) {
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
        let tw = self.tx.ui_width(&text.title, false);
        let kw = text.keys.as_ref().map_or(0, |k| self.tx.ui_width(k, false) + 14);
        let dw = text.detail.as_ref().map_or(0, |d| self.tx.ui_width(d, false));
        let w = (tw + kw).max(dw) + 2 * PAD_X;
        let lines = 1 + text.detail.is_some() as i32;
        let h = 2 * PAD_Y + lines * LINE + (lines - 1) * 2;
        let (x, y) = place(anchor, w, h, below, (self.size.0 as i32, self.size.1 as i32));
        let r = Rect::new(x, y, w, h);
        if alpha > 0.6 {
            cv.shadow(r, 8);
        }
        cv.card(r, 8, faded(c.widget_bg, alpha), faded(c.border, alpha));
        self.tx.draw_ui(cv, r.x + PAD_X, r.y + PAD_Y, &text.title, faded(c.tab_active_fg, alpha), r, false);
        if let Some(k) = &text.keys {
            self.tx.draw_ui(cv, r.x + PAD_X + tw + 14, r.y + PAD_Y, k, faded(c.tab_inactive_fg, alpha), r, false);
        }
        if let Some(d) = &text.detail {
            self.tx.draw_ui(cv, r.x + PAD_X, r.y + PAD_Y + LINE + 2, d, faded(c.tab_inactive_fg, alpha), r, false);
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
        assert_eq!(plural(1, "arquivo alterado", "arquivos alterados"), "1 arquivo alterado");
        assert_eq!(plural(3, "arquivo alterado", "arquivos alterados"), "3 arquivos alterados");
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
        assert!(!git_tip_wanted(Hit::Msg) && !git_tip_wanted(Hit::Commit(0)) && !git_tip_wanted(Hit::CFile(1)));
    }
}
