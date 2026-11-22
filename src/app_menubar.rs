//! The title bar's menu bar (File, Edit, Selection, View, Go, Terminal, Run, Git, Help) and the
//! run cluster at its right end (run target, run / stop, debug).
//!
//! Nothing here is a command of its own: every menu lists entries of the command palette
//! (`COMMANDS`, grouped by the area before the colon of their name) and every button runs the
//! same command the palette entry does, so a shortcut, a tooltip or a user keymap stays true in
//! both places. A menu is a [`TopMenu`](crate::app_topbar::TopMenu) hanging from its title.
//!
//! The window keeps the system's own title bar and buttons (it is not client-side decorated), so
//! there are no drawn window controls here.

use crate::{
    anim::lerp_rgba,
    app::App,
    app_overlay::{COMMANDS, Cmd, Overlay},
    app_shell::HotSlot,
    app_tooltip::TipText,
    app_topbar::{Entry, MenuKind, TopAct},
    ds,
    ui::{Canvas, Rect, fit_ellipsis},
};

/// First hot slot of the menu titles, and of the run cluster (see `app_shell::TOP_BTNS`).
const SLOT_MENU: usize = 3;
const SLOT_ACT: usize = SLOT_MENU + MENUS.len();
/// Side padding of a menu title.
const TITLE_PAD: i32 = 9;
/// Room the badges and messages keep when the menu bar is drawn.
const KEEP_FREE: i32 = 200;
/// Width of an icon button of the run cluster.
const ICON_BTN: i32 = 30;
const TARGET_MAX: i32 = 150;

/// A menu: its title, the palette areas it lists, and the search hint.
pub const MENUS: [(&str, &[&str], &str); 9] = [
    ("File", &["File", "Workspace", "Project", "Remote", "Quit"], "Search File commands…"),
    ("Edit", &["Edit", "Search", "Code", "Fold", "Macro", "Snippets"], "Search Edit commands…"),
    ("Selection", &["Edit"], "Search Selection commands…"),
    ("View", &["View", "Appearance", "Theme", "Fonts", "Explorer", "Preferences", "Palette"], "Search View commands…"),
    ("Go", &["Go to", "Go", "Bookmark"], "Search Go commands…"),
    ("Terminal", &["Terminal"], "Search Terminal commands…"),
    ("Run", &["Build", "Debug", "Tasks", "Tests"], "Search Run commands…"),
    ("Git", &["Git", "GitHub", "Merge"], "Search Git commands…"),
    ("Help", &["Help"], "Search Help commands…"),
];

/// The area of a palette name: "Git: Push" -> "Git"; a name without a colon is its own area.
fn area_of(name: &str) -> &str {
    name.split_once(':').map_or(name, |(a, _)| a)
}

/// The words that make an Edit command a selection command.
fn is_selection_cmd(name: &str) -> bool {
    area_of(name) == "Edit" && name.to_lowercase().contains("select")
}

/// Does menu `i` list the palette entry named `name`? Edit and Selection split the "Edit" area.
pub fn menu_wants(i: usize, name: &str) -> bool {
    let Some((_, areas, _)) = MENUS.get(i) else { return false };
    let sel = is_selection_cmd(name);
    match (MENUS[i].0, sel) {
        ("Selection", s) => s,
        ("Edit", true) => false,
        _ => areas.contains(&area_of(name)),
    }
}

/// The entries of menu `i` from a command table: (name without its area, shortcut, command),
/// in table order, each command once (a name another entry already took keeps its area).
pub fn menu_items(i: usize, table: &[(&str, &str, Cmd)]) -> Vec<(String, String, Cmd)> {
    let mut out: Vec<(String, String, Cmd)> = Vec::new();
    for (name, keys, cmd) in table {
        if !menu_wants(i, name) {
            continue;
        }
        if out.iter().any(|(_, _, c)| *c == *cmd) {
            continue;
        }
        let short = name.split_once(": ").map_or(*name, |(_, rest)| rest);
        let mut label = short.to_string();
        if let Some(first) = label.get(..1) {
            label = first.to_uppercase() + &label[1..];
        }
        // Two areas can both have a "Stop": the second keeps its area to tell them apart.
        if out.iter().any(|(l, _, _)| *l == label) {
            label = name.to_string();
        }
        out.push((label, keys.to_string(), *cmd));
    }
    out
}

impl App {
    /// Entries of menu `i`: the palette's, with the shortcuts the user's keymap really has.
    pub fn menu_entries(&self, i: usize) -> Vec<Entry> {
        menu_items(i, COMMANDS)
            .into_iter()
            .map(|(label, keys, cmd)| {
                let keys = self.shortcut_text(&format!("{cmd:?}")).unwrap_or(keys);
                Entry::Cmd(label, keys, cmd)
            })
            .collect()
    }

    /// The menu titles from `x0` while they leave `KEEP_FREE` px before `limit`; returns the x
    /// after them (`x0` when the bar is too narrow, the palette still has every command).
    pub fn draw_menu_bar(&mut self, cv: &mut Canvas, x0: i32, limit: i32) -> i32 {
        let bar = self.layout.topbar;
        let c = self.theme.chrome;
        let widths: Vec<i32> = MENUS.iter().map(|m| self.tx.ui_width(m.0, false) + 2 * TITLE_PAD).collect();
        if x0 + widths.iter().sum::<i32>() + KEEP_FREE > limit {
            return x0;
        }
        let open = if let Overlay::TopMenu(m) = &self.overlay { Some(m.kind) } else { None };
        let ty = bar.y + (bar.h - 18) / 2;
        let mut x = x0 + 4;
        for (i, w) in widths.iter().enumerate() {
            let r = Rect::new(x, bar.y + 5, *w, bar.h - 10);
            let kind = MenuKind::Menu(i as u8);
            let hot = self.hot_fill(cv, HotSlot::Top(SLOT_MENU + i), r, ds::R_ROW, open == Some(kind), 0.0);
            let ink = lerp_rgba(c.tab_inactive_fg, c.tab_active_fg, hot);
            self.tx.draw_ui(cv, r.x + TITLE_PAD, ty, MENUS[i].0, ink, bar, false);
            self.layout.top_btns.push((r, kind));
            x = r.right();
        }
        x
    }

    /// Label of the chosen run target ("Run target" before one is known).
    fn run_target_label(&self) -> String {
        self.build.selected_target().map_or_else(|| "Run target".to_string(), |t| t.label.clone())
    }

    /// The run cluster, right-aligned to `right`: target picker, run / stop, debug. Returns the x
    /// where it starts (`right` when the bar is too narrow for it).
    pub fn draw_run_cluster(&mut self, cv: &mut Canvas, right: i32) -> i32 {
        let bar = self.layout.topbar;
        let c = self.theme.chrome;
        let label = self.run_target_label();
        let label = fit_ellipsis(&mut self.tx, &label, TARGET_MAX, false);
        let target_w = 10 + self.tx.ui_width(&label, false) + 8 + 12 + 10;
        let total = target_w + 2 * ICON_BTN + 4;
        if right - total < bar.x + 360 {
            return right;
        }
        let h = bar.h - 10;
        let iy = bar.y + (bar.h - 16) / 2;
        let mut x = right - ICON_BTN;

        // Debug.
        let dbg_on = self.dbg.is_some();
        let r = Rect::new(x, bar.y + 5, ICON_BTN, h);
        let hot = self.hot_fill(cv, HotSlot::Top(SLOT_ACT + 2), r, ds::R_ROW, false, 0.0);
        let base = if dbg_on { c.accent_text } else { c.tab_inactive_fg };
        let icon = if dbg_on { "ui/debug_continue" } else { "ui/debug" };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, iy, 16, lerp_rgba(base, c.tab_active_fg, hot), bar);
        self.layout.top_btns.push((r, MenuKind::Act(TopAct::Debug)));
        x -= ICON_BTN;

        // Run, or stop what runs.
        let running = self.build.job.is_some();
        let r = Rect::new(x, bar.y + 5, ICON_BTN, h);
        self.hot_fill(cv, HotSlot::Top(SLOT_ACT + 1), r, ds::R_ROW, false, 0.0);
        let (icon, tint) = if running { ("ui/stop", c.error) } else { ("ui/play_filled", c.git_added) };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, iy, 16, tint, bar);
        self.layout.top_btns.push((r, MenuKind::Act(TopAct::Run)));
        x -= 4 + target_w;

        // Target.
        let r = Rect::new(x, bar.y + 5, target_w, h);
        let hot = self.hot_fill(cv, HotSlot::Top(SLOT_ACT), r, ds::R_ROW, false, 0.45);
        let ink = lerp_rgba(c.tab_inactive_fg, c.tab_active_fg, hot);
        self.tx.draw_ui(cv, r.x + 10, bar.y + (bar.h - 18) / 2, &label, ink, bar, false);
        self.tx.icon(cv, "ui/chevron_down", r.right() - 22, bar.y + (bar.h - 12) / 2, 12, ink, bar);
        self.layout.top_btns.push((r, MenuKind::Act(TopAct::Target)));
        x
    }

    /// Tooltip of a run-cluster button: the same name and shortcut as its palette entry.
    pub fn top_act_tip(&self, a: TopAct) -> TipText {
        let t = |title: &str, id: &str, detail: Option<String>| TipText {
            title: title.to_string(),
            keys: self.shortcut_text(id),
            detail,
            more: Vec::new(),
        };
        match a {
            TopAct::Target => t("Run target", "BuildSelectTarget", Some(self.run_target_label())),
            TopAct::Run if self.build.job.is_some() => t("Stop", "BuildStop", self.build.running_label()),
            TopAct::Run => t("Run", "BuildRun", Some(self.run_target_label())),
            TopAct::Debug if self.dbg.is_some() => t("Continue debugging", "DebugStart", None),
            TopAct::Debug => t("Start debugging", "DebugStart", None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_lists_commands_once() {
        for (i, m) in MENUS.iter().enumerate() {
            let items = menu_items(i, COMMANDS);
            assert!(!items.is_empty(), "{} is empty", m.0);
            for (a, (la, _, ca)) in items.iter().enumerate() {
                for (lb, _, cb) in &items[a + 1..] {
                    assert!(la != lb && ca != cb, "{} lists {la} twice", m.0);
                }
            }
        }
    }

    fn has(i: usize, label: &str) -> bool {
        menu_items(i, COMMANDS).iter().any(|(l, _, _)| l == label)
    }

    #[test]
    fn menus_hold_what_their_names_promise() {
        assert!(has(0, "Save") && has(0, "Quit"));
        assert!(has(1, "Undo") && has(1, "In project"), "Edit holds undo and search");
        assert!(has(2, "Expand selection") && has(2, "Column select up"));
        assert!(!has(1, "Expand selection"), "the selection commands live under Selection");
        assert!(!has(2, "Undo"));
        assert!(has(3, "Toggle sidebar"));
        assert!(has(4, "Line…") && has(4, "Definition"));
        assert!(has(5, "New terminal"));
        assert!(has(6, "Run") && has(6, "Start / continue"), "Run holds build, run and debug");
        assert!(has(7, "Open panel"));
        assert!(has(8, "About Lumen"));
    }

    #[test]
    fn labels_drop_their_area_and_keep_the_shortcut() {
        let (label, keys, cmd) = &menu_items(0, COMMANDS).into_iter().find(|(l, _, _)| l == "Save").unwrap();
        assert_eq!((label.as_str(), keys.as_str()), ("Save", "Ctrl+S"));
        assert_eq!(format!("{cmd:?}"), "Save");
    }

    #[test]
    fn areas_split_on_the_first_colon() {
        assert_eq!(area_of("Go to: Line…"), "Go to");
        assert_eq!(area_of("Quit"), "Quit");
        assert!(!menu_wants(99, "File: Save"));
    }
}
