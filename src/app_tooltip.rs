//! Hover tooltips for the buttons of the top and bottom bars.
//!
//! The state is just "which target is under the pointer and since when" ([`Tooltip`]); the
//! rect, the text and the position are resolved again on every draw, so a resize, a changed
//! label or a shortcut remapped in `keybindings.json` shows up at once. After the pointer rests
//! on a target for [`TOOLTIP_DELAY`] a small rounded card appears (above the target in the bottom
//! bar, below it in the top bar, always inside the window). Leaving the target, clicking or
//! pressing a key closes it; moving straight to a neighbouring target shows its tooltip with no
//! new delay. To give another group of buttons a tooltip: add a [`TipKey`] variant, return it
//! from [`App::tip_target`] and describe it in [`App::tip_text`].

use crate::{
    app::App,
    app_overlay::Overlay,
    app_topbar::{MenuKind, StatusBtn},
    ui::{Canvas, Rect},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub const TOOLTIP_DELAY: Duration = Duration::from_millis(450);
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
}

/// Pointer-rest state (the text is resolved while drawing).
#[derive(Clone, Debug)]
pub struct Tooltip {
    pub key: TipKey,
    pub since: Instant,
    pub shown: bool,
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

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl App {
    fn shortcut_text(&self, id: &str) -> Option<String> {
        shortcut_in(&self.keymap.entries(), id)
    }

    /// The target under the pointer, its rect and whether the card goes below it.
    pub fn tip_target(&self) -> Option<(TipKey, Rect, bool)> {
        let (x, y) = self.mouse;
        let l = &self.layout;
        if !self.overlay.is_none() || self.dragging || self.dragging_minimap || self.drag_bar.is_some() {
            return None;
        }
        if let Some((r, k)) = l.top_btns.iter().find(|(r, k)| r.contains(x, y) && *k != MenuKind::Browse) {
            return Some((TipKey::Top(*k), *r, true));
        }
        l.status_btns.iter().chain(l.status_info.iter()).find(|(r, _)| r.contains(x, y)).map(|(r, k)| (TipKey::Status(*k), *r, false))
    }

    /// The words for a target.
    pub fn tip_text(&mut self, key: TipKey) -> TipText {
        let t = |title: &str, keys: Option<String>, detail: Option<String>| TipText { title: title.to_string(), keys, detail };
        match key {
            TipKey::Top(MenuKind::Branch) => t("Trocar de branch", None, self.branch.clone()),
            TipKey::Top(_) => t("Trocar de projeto", None, Some(crate::app_topbar::tilde(&self.root))),
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
                StatusBtn::Position => t("Posição do cursor", None, self.shortcut_text("GotoLine").map(|k| format!("Ir para linha  {k}"))),
                StatusBtn::Indent => {
                    let d = self.docs.get(self.active);
                    let detail = d.map(|d| if d.ed.indent.contains('\t') { "Tabulação".to_string() } else { plural(d.ed.indent.len(), "espaço", "espaços") });
                    t("Indentação", None, detail)
                }
                StatusBtn::Language => t("Linguagem do arquivo", None, self.docs.get(self.active).map(|d| d.lang.unwrap_or("texto").to_string())),
            },
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
                *old = Some(Tooltip { key, since: Instant::now(), shown: warm, dismissed: false });
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

    /// Timer: show the card once the pointer has rested long enough.
    pub fn tooltip_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some(t) = &mut self.tooltip else { return };
        if t.shown || t.dismissed {
            return;
        }
        let due = t.since + TOOLTIP_DELAY;
        if due <= now {
            t.shown = true;
            self.request_redraw();
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
        let c = self.theme.chrome;
        let tw = self.tx.ui_width(&text.title, false);
        let kw = text.keys.as_ref().map_or(0, |k| self.tx.ui_width(k, false) + 14);
        let dw = text.detail.as_ref().map_or(0, |d| self.tx.ui_width(d, false));
        let w = (tw + kw).max(dw) + 2 * PAD_X;
        let lines = 1 + text.detail.is_some() as i32;
        let h = 2 * PAD_Y + lines * LINE + (lines - 1) * 2;
        let (x, y) = place(anchor, w, h, below, (self.size.0 as i32, self.size.1 as i32));
        let r = Rect::new(x, y, w, h);
        cv.shadow(r, 8);
        cv.card(r, 8, c.widget_bg, c.border);
        self.tx.draw_ui(cv, r.x + PAD_X, r.y + PAD_Y, &text.title, c.tab_active_fg, r, false);
        if let Some(k) = &text.keys {
            self.tx.draw_ui(cv, r.x + PAD_X + tw + 14, r.y + PAD_Y, k, c.tab_inactive_fg, r, false);
        }
        if let Some(d) = &text.detail {
            self.tx.draw_ui(cv, r.x + PAD_X, r.y + PAD_Y + LINE + 2, d, c.tab_inactive_fg, r, false);
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
}
