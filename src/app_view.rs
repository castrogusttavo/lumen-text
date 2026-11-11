//! View options "your way": zen mode (`Ctrl+K Z`: every bar and panel hidden, the text centered
//! in a column of `zen_max_width` columns, `Esc Esc` or the same chord brings the layout back
//! exactly), the tab bar / status bar switches, the explorer order (name, type, modified, mixed,
//! folders first) and the buffer font features / letter spacing settings.
//!
//! The layout code asks [`App::chrome_heights`] and [`App::zen_card`]; the palette commands land
//! in [`App::run_view_cmd`] and the settings rows in [`App::apply_view_setting`].

use crate::{
    app::{App, BREAD_H, Focus, STATUS_H, TAB_H, TOP_H},
    app_overlay::Cmd,
    config::{EXPLORER_SORTS, ZEN_WIDTH_RANGE, normalize_explorer_sort},
    fontcfg,
    settingsui::{SettingDef, SettingKind},
    tree::SortMode,
    ui::Rect,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use winit::keyboard::{Key, NamedKey};

/// Two presses of Esc this close together leave zen mode.
const ESC_ESC: Duration = Duration::from_millis(500);
/// Room right of the text column in zen mode (scrollbar, the caret at the end of a full line).
const ZEN_PAD: i32 = 24;

/// What zen mode hid: put back as it was when zen mode ends.
#[derive(Clone, PartialEq)]
pub struct ZenSaved {
    pub sidebar_open: bool,
    pub panel_open: bool,
    pub ai_open: bool,
    pub ai_focused: bool,
    /// The Git panel was open, on this tab (it is closed while in zen mode and reopened after).
    pub git: Option<crate::app_gitpanel::Tab>,
    pub focus: Focus,
}

#[derive(Default)]
pub struct Zen {
    /// `Some` while zen mode is on.
    saved: Option<ZenSaved>,
    /// When the last plain Esc was pressed in zen mode.
    last_esc: Option<Instant>,
}

impl Zen {
    pub fn active(&self) -> bool {
        self.saved.is_some()
    }
}

/// The centered text column of zen mode inside `card`: the gutter plus `cols` cells (and a little
/// room on the right), never wider than `card`.
pub fn zen_column(card: Rect, gutter: i32, cols: usize, cell_w: f32) -> Rect {
    let want = gutter + (cols as f32 * cell_w).ceil() as i32 + ZEN_PAD;
    if want >= card.w {
        return card;
    }
    Rect::new(card.x + (card.w - want) / 2, card.y, want, card.h)
}

impl App {
    /// Heights of the top bar, tab bar, breadcrumbs and status bar for the current layout: zero
    /// for what zen mode or the View switches hide.
    pub fn chrome_heights(&self) -> (i32, i32, i32, i32) {
        let zen = self.zen.active();
        let v = &self.config.view;
        (
            if zen { 0 } else { TOP_H },
            if zen || !v.show_tabs { 0 } else { TAB_H },
            if zen || !self.config.breadcrumbs { 0 } else { BREAD_H },
            if zen || !v.show_status_bar { 0 } else { STATUS_H },
        )
    }

    /// The editor card: in zen mode (and without a split) a column centered in `card`.
    pub fn zen_card(&self, card: Rect) -> Rect {
        if !self.zen.active() || self.split.is_some() {
            return card;
        }
        let gutter = self.docs.get(self.active).map_or(0, |d| d.gutter_w());
        zen_column(card, gutter, self.config.view.zen_max_width, self.tx.cell_w)
    }

    /// Zen mode draws no minimap: switch them off for one frame, returning the switches.
    pub fn zen_hide_minimaps(&mut self) -> Vec<bool> {
        if !self.zen.active() {
            return Vec::new();
        }
        self.docs.iter_mut().map(|d| std::mem::replace(&mut d.minimap, false)).collect()
    }

    pub fn zen_restore_minimaps(&mut self, saved: Vec<bool>) {
        for (d, m) in self.docs.iter_mut().zip(saved) {
            d.minimap = m;
        }
    }

    pub fn toggle_zen(&mut self) {
        if self.zen.active() { self.zen_exit() } else { self.zen_enter() }
    }

    /// Hide the sidebar, the panels and every bar; the text goes to the middle.
    pub fn zen_enter(&mut self) {
        if self.zen.active() {
            return;
        }
        let git = self.gitpanel.as_ref().map(|gp| gp.tab);
        self.zen.saved = Some(ZenSaved {
            sidebar_open: self.sidebar_open,
            panel_open: self.panel.open,
            ai_open: self.ai.open,
            ai_focused: self.ai.focused,
            git,
            focus: self.focus,
        });
        self.zen.last_esc = None;
        self.sidebar_open = false;
        self.panel.open = false;
        if git.is_some() {
            // Closing detaches a running Git command; reopening shows it again.
            self.close_git_panel();
        }
        if self.ai.open {
            self.ai_hide(); // the agent keeps running
        }
        self.focus = Focus::Editor;
        self.hover = None;
        self.completion = None;
        for d in &mut self.docs {
            d.reveal = true;
        }
        self.toast("Zen mode: Esc Esc or Ctrl+K Z leaves it", true);
    }

    /// Put the layout back exactly as it was before zen mode.
    pub fn zen_exit(&mut self) {
        let Some(s) = self.zen.saved.take() else { return };
        self.zen.last_esc = None;
        self.sidebar_open = s.sidebar_open;
        self.panel.open = s.panel_open;
        match s.git {
            Some(tab) => {
                if self.gitpanel.is_none() {
                    self.open_git_panel();
                }
                if let Some(gp) = self.gitpanel.as_mut() {
                    gp.tab = tab;
                }
            }
            None if self.gitpanel.is_some() => self.close_git_panel(),
            None => {}
        }
        if s.ai_open {
            self.ai.open = true;
            self.ai.focused = s.ai_focused;
        } else if self.ai.open {
            self.ai_hide();
        }
        self.focus = s.focus;
        for d in &mut self.docs {
            d.reveal = true;
        }
    }

    /// The zen layout to restore, while zen mode is on.
    #[cfg(test)]
    pub fn zen_saved(&self) -> Option<&ZenSaved> {
        self.zen.saved.as_ref()
    }

    /// Keys in zen mode: a second plain Esc soon after the first leaves it (true = consumed).
    /// The first Esc still does its usual job (closing the find bar, dropping extra cursors).
    pub fn zen_escape(&mut self, key: &Key) -> bool {
        if !self.zen.active() {
            return false;
        }
        let plain = !self.mods.control_key() && !self.mods.shift_key() && !self.mods.alt_key();
        match key {
            Key::Named(NamedKey::Escape) if plain => {
                let now = Instant::now();
                if self.zen.last_esc.is_some_and(|t| now.duration_since(t) < ESC_ESC) {
                    self.zen_exit();
                    return true;
                }
                self.zen.last_esc = Some(now);
            }
            Key::Named(NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::Super) => {}
            _ => self.zen.last_esc = None,
        }
        false
    }

    /// Palette commands of this module; false for any other command.
    pub fn run_view_cmd(&mut self, c: Cmd) -> bool {
        match c {
            Cmd::ToggleZen => self.toggle_zen(),
            Cmd::ToggleTabBar => {
                let on = !self.config.view.show_tabs;
                self.apply_setting("show_tabs", &json!(on));
                self.say(if on { "tab bar: on" } else { "tab bar: off" });
            }
            Cmd::ToggleStatusBar => {
                let on = !self.config.view.show_status_bar;
                self.apply_setting("show_status_bar", &json!(on));
                if on {
                    self.say("status bar: on");
                } else {
                    self.toast("Status bar hidden: View: Toggle status bar shows it again", true);
                }
            }
            Cmd::ExplorerSortName => self.apply_setting("explorer_sort", &json!("name")),
            Cmd::ExplorerSortType => self.apply_setting("explorer_sort", &json!("type")),
            Cmd::ExplorerSortModified => self.apply_setting("explorer_sort", &json!("modified")),
            Cmd::ExplorerSortMixed => self.apply_setting("explorer_sort", &json!("mixed")),
            Cmd::ToggleFoldersFirst => {
                let on = !self.config.view.explorer_folders_first;
                self.apply_setting("explorer_folders_first", &json!(on));
                self.say(if on { "explorer: folders first" } else { "explorer: folders among files" });
            }
            _ => return false,
        }
        true
    }

    /// The explorer re-reads its folders in the configured order.
    fn apply_explorer_sort(&mut self) {
        let v = &self.config.view;
        let (mode, first) = (SortMode::from_name(&v.explorer_sort), v.explorer_folders_first);
        self.tree.set_sort(mode, first);
    }

    /// Settings handled here (the caller saves config.json); false when `key` is not one of them.
    pub(crate) fn apply_view_setting(&mut self, key: &str, v: &Value) -> bool {
        let b = v.as_bool();
        match key {
            "show_tabs" => self.config.view.show_tabs = b.unwrap_or(true),
            "show_status_bar" => self.config.view.show_status_bar = b.unwrap_or(true),
            "zen_max_width" => {
                let n = v.as_f64().filter(|n| n.is_finite()).unwrap_or(100.0).round().max(0.0) as usize;
                self.config.view.zen_max_width = n.clamp(ZEN_WIDTH_RANGE.0, ZEN_WIDTH_RANGE.1);
            }
            "explorer_sort" => {
                self.config.view.explorer_sort = normalize_explorer_sort(v.as_str());
                self.apply_explorer_sort();
                let s = self.config.view.explorer_sort.clone();
                self.say(format!("explorer: sorted by {}", sort_label(&s).to_lowercase()));
            }
            "explorer_folders_first" => {
                self.config.view.explorer_folders_first = b.unwrap_or(true);
                self.apply_explorer_sort();
            }
            "buffer_font_features" => {
                self.config.view.buffer_font_features = match v {
                    Value::String(s) => fontcfg::parse_features_text(s),
                    o => fontcfg::clean_features(o),
                };
                self.refit_fonts(true, false);
            }
            "buffer_letter_spacing" => {
                self.config.view.buffer_letter_spacing = fontcfg::clean_letter_spacing(v.as_f64().unwrap_or(0.0));
                self.refit_fonts(true, false);
            }
            _ => return false,
        }
        true
    }
}

/// Menu / message name of an explorer order.
pub fn sort_label(s: &str) -> &'static str {
    match s {
        "type" => "Type",
        "modified" => "Date modified",
        "mixed" => "Mixed",
        _ => "Name",
    }
}

// ───────────────────────────── settings rows ─────────────────────────────

pub fn defs() -> Vec<SettingDef> {
    use SettingKind::*;
    let d = |key, section, label, help, kind| SettingDef { key, section, label, help, kind };
    vec![
        d(
            "show_tabs",
            "Editor display",
            "Tab bar",
            "Shows the open files as tabs above the editor. Off gives the editor that row; Ctrl+Tab and Ctrl+P still switch files.",
            Bool,
        ),
        d(
            "show_status_bar",
            "Editor display",
            "Status bar",
            "Shows the bottom bar (position, language, Git, problems...). Off gives the editor that row.",
            Bool,
        ),
        d(
            "zen_max_width",
            "Editor display",
            "Zen mode width",
            "Width of the centered text in zen mode (Ctrl+K Z), in columns (40 to 400). Zen mode hides the bars, the sidebar and the panels; Esc Esc leaves it.",
            Number { min: ZEN_WIDTH_RANGE.0 as f64, max: ZEN_WIDTH_RANGE.1 as f64, step: 10.0 },
        ),
        d(
            "explorer_sort",
            "Appearance",
            "Explorer order",
            "Name (numbers by value: file2 before file10), Type (by extension), Date modified (newest first) or Mixed (folders among the files). Also in the right-click menu of the explorer's header.",
            Choice(EXPLORER_SORTS),
        ),
        d(
            "explorer_folders_first",
            "Appearance",
            "Folders first",
            "Lists the folders above the files in the explorer (not with the Mixed order).",
            Bool,
        ),
        d(
            "buffer_font_features",
            "Fonts",
            "Buffer font features",
            "OpenType features of the buffer font, separated by spaces: zero (slashed zero), ss01 to ss20 (stylistic sets), cv01 to cv99 (character variants)... -name switches one off, name=2 picks an alternate. The font must have them. Applies to the code, terminal and diffs.",
            Text,
        ),
        d(
            "buffer_letter_spacing",
            "Fonts",
            "Buffer letter spacing",
            "Extra space between characters in pixels (-1 to 3). Columns, the caret and box drawing stay aligned.",
            Number {
                min: fontcfg::LETTER_SPACING_RANGE.0 as f64,
                max: fontcfg::LETTER_SPACING_RANGE.1 as f64,
                step: 0.1,
            },
        ),
    ]
}

pub fn defaults() -> Value {
    json!({
        "show_tabs": true, "show_status_bar": true, "zen_max_width": 100,
        "explorer_sort": "name", "explorer_folders_first": true,
        "buffer_font_features": "", "buffer_letter_spacing": 0.0,
    })
}

pub fn values(c: &crate::config::Config) -> Value {
    let v = &c.view;
    json!({
        "show_tabs": v.show_tabs, "show_status_bar": v.show_status_bar, "zen_max_width": v.zen_max_width,
        "explorer_sort": v.explorer_sort, "explorer_folders_first": v.explorer_folders_first,
        "buffer_font_features": fontcfg::features_text(&v.buffer_font_features),
        "buffer_letter_spacing": v.buffer_letter_spacing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    fn frame(app: &mut App) {
        let (w, h) = (1400, 900);
        let mut px = vec![0u32; (w * h) as usize];
        let mut cv = crate::ui::Canvas { px: &mut px, w, h };
        app.size = (w as u32, h as u32);
        app.render_into(&mut cv, 0.016);
    }

    #[test]
    fn zen_column_is_centered_and_never_wider_than_the_card() {
        let card = Rect::new(100, 40, 1200, 800);
        let c = zen_column(card, 50, 100, 8.0);
        assert_eq!(c.w, 50 + 800 + ZEN_PAD);
        assert_eq!(c.x - card.x, card.right() - c.right(), "centered");
        assert_eq!((c.y, c.h), (card.y, card.h));
        assert_eq!(zen_column(card, 50, 400, 8.4), card, "too wide: the whole card");
    }

    #[test]
    fn zen_layout_round_trip() {
        let _g = lock();
        let dir = tmp("zen");
        let mut app = test_app(&dir);
        // A layout to come back to: sidebar shut, bottom panel open, focus in the explorer.
        app.sidebar_open = false;
        app.panel.open = true;
        app.focus = Focus::Sidebar;
        frame(&mut app);
        let before = (app.layout.topbar, app.layout.tabbar, app.layout.status, app.layout.panel, app.layout.editor);
        assert!(before.0.h > 0 && before.1.h > 0 && before.2.h > 0 && before.3.h > 0);
        let minimap = app.docs[0].minimap;

        app.run_bound("ToggleZen");
        assert!(app.zen.active());
        let saved = app.zen_saved().expect("saved layout");
        assert!(!saved.sidebar_open && saved.panel_open && saved.focus == Focus::Sidebar && saved.git.is_none());
        frame(&mut app);
        let l = &app.layout;
        assert_eq!((l.topbar.h, l.tabbar.h, l.bread.h, l.status.h, l.panel.h), (0, 0, 0, 0, 0), "chrome hidden");
        assert!(!app.sidebar_open && !app.panel.open && app.focus == Focus::Editor);
        assert!(l.tabs.is_empty() && l.status_btns.is_empty() && l.top_btns.is_empty(), "no stale hit rects");
        let ed = l.editor;
        assert!(ed.w < 1400 && ed.x > 0, "a centered column: {ed:?}");
        assert!((ed.x - (1400 - ed.right())).abs() <= 1, "centered: {ed:?}");
        assert_eq!((ed.y, ed.h), (0, 900), "full height");
        assert!(app.docs[0].minimap_layout.is_none(), "no minimap in zen mode");
        assert_eq!(app.docs[0].minimap, minimap, "the document keeps its own minimap switch");

        // Esc then Esc leaves it and the old layout comes back exactly.
        let esc = crate::app::KeyIn { key: Key::Named(NamedKey::Escape), text: None };
        app.handle_key(&esc);
        assert!(app.zen.active(), "one Esc is not enough");
        app.handle_key(&esc);
        assert!(!app.zen.active());
        assert!(!app.sidebar_open && app.panel.open && app.focus == Focus::Sidebar);
        frame(&mut app);
        let after = (app.layout.topbar, app.layout.tabbar, app.layout.status, app.layout.panel, app.layout.editor);
        assert_eq!(after, before, "the layout is restored exactly");

        // The chord toggles both ways too; an Esc, another key, then Esc does not leave.
        app.run_bound("ToggleZen");
        app.handle_key(&esc);
        app.handle_key(&crate::app::KeyIn { key: Key::Named(NamedKey::ArrowDown), text: None });
        app.handle_key(&esc);
        assert!(app.zen.active());
        app.run_bound("ToggleZen");
        assert!(!app.zen.active() && app.panel.open);
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tab_and_status_bar_switches() {
        let _g = lock();
        let dir = tmp("viewbars");
        let mut app = test_app(&dir);
        frame(&mut app);
        let ed = app.layout.editor;
        app.run_bound("ToggleTabBar");
        app.run_bound("ToggleStatusBar");
        assert!(!app.config.view.show_tabs && !app.config.view.show_status_bar);
        frame(&mut app);
        assert_eq!((app.layout.tabbar.h, app.layout.status.h), (0, 0));
        assert!(app.layout.topbar.h > 0, "the top bar stays");
        assert_eq!(app.layout.editor.h, ed.h + TAB_H + STATUS_H, "the editor takes both rows");
        assert!(app.layout.tabs.is_empty() && app.layout.status_btns.is_empty());
        // Saved like any setting.
        let saved = crate::config::Config::load_from(&dir.join(".config/lumen-text/config.json"));
        assert!(!saved.view.show_tabs && !saved.view.show_status_bar);
        app.run_bound("ToggleTabBar");
        app.run_bound("ToggleStatusBar");
        frame(&mut app);
        assert_eq!(app.layout.editor, ed);
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explorer_sort_commands_and_header_menu() {
        use crate::app_ctxmenu::{Act, Entry, Flags, Target, entries_for};
        let _g = lock();
        let dir = tmp("viewsort");
        std::fs::create_dir_all(dir.join("zdir")).unwrap();
        for f in ["f10.txt", "f2.txt", "b.rs"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let mut app = test_app(&dir);
        app.tree.rebuild();
        // Only the entries made here (the test's own config folder and seed file aside).
        let names = |app: &App| {
            app.tree
                .rows
                .iter()
                .map(|r| r.name.clone())
                .filter(|n| !n.starts_with('.') && n != "seed.txt")
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&app), ["zdir", "b.rs", "f2.txt", "f10.txt"]);
        app.run_bound("ExplorerSortMixed");
        assert_eq!(app.config.view.explorer_sort, "mixed");
        assert_eq!(names(&app).last().map(String::as_str), Some("zdir"), "folders among files");
        app.run_bound("ExplorerSortName");
        app.run_bound("ToggleFoldersFirst");
        assert!(!app.config.view.explorer_folders_first);
        assert_eq!(names(&app).last().map(String::as_str), Some("zdir"));
        app.run_bound("ToggleFoldersFirst");
        assert_eq!(names(&app)[0], "zdir");
        // The header's menu marks the current order and runs the commands.
        let menu = entries_for(&Target::ExplorerHeader(SortMode::Type, true), &Flags::default());
        let items: Vec<(Act, &str)> = menu
            .iter()
            .filter_map(|e| match e {
                Entry::Item { act, icon, .. } => Some((*act, *icon)),
                Entry::Sep => None,
            })
            .collect();
        assert_eq!(items.len(), 5);
        assert_eq!(items[1], (Act::Run("ExplorerSortType"), "ui/check"));
        assert_eq!(items[0].1, "");
        assert_eq!(items[4], (Act::Run("ToggleFoldersFirst"), "ui/check"));
        for (act, _) in items {
            let Act::Run(id) = act else { panic!("{act:?}") };
            assert!(Cmd::from_id(id).is_some(), "{id}");
        }
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn font_feature_and_spacing_settings_apply_and_widen_the_cell() {
        let _g = lock();
        let dir = tmp("viewfont");
        let mut app = test_app(&dir);
        let cell = app.tx.cell_w;
        app.apply_setting("buffer_font_features", &json!("zero ss01 -cv02"));
        let f = &app.config.view.buffer_font_features;
        assert_eq!((f.get("zero"), f.get("ss01"), f.get("cv02")), (Some(&1), Some(&1), Some(&0)));
        assert!((app.tx.cell_w - cell).abs() < 0.01, "features keep the monospace cell");
        app.apply_setting("buffer_letter_spacing", &json!(2.0));
        assert_eq!(app.config.view.buffer_letter_spacing, 2.0);
        assert!(
            (app.tx.cell_w - (cell + 2.0)).abs() < 0.2,
            "the spacing is part of the cell: {cell} -> {}",
            app.tx.cell_w
        );
        assert_eq!(app.docs[0].cell_w, app.tx.cell_w, "documents follow (cursor x, columns)");
        assert_eq!(values(&app.config)["buffer_font_features"], json!("-cv02 ss01 zero"));
        app.apply_setting("buffer_letter_spacing", &json!(-9));
        assert_eq!(app.config.view.buffer_letter_spacing, -1.0);
        app.apply_setting("buffer_letter_spacing", &json!(0));
        assert!((app.tx.cell_w - cell).abs() < 0.01);
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_rows_have_defaults_and_values() {
        let d = defaults();
        let v = values(&crate::config::Config::default());
        for def in defs() {
            assert!(d.get(def.key).is_some(), "{} has no default", def.key);
            assert_eq!(d[def.key], v[def.key], "{}: the default config shows the default", def.key);
        }
    }
}
