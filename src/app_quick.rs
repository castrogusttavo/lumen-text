//! Editor strip: three buttons at the right end of the tab bar: find in file, all settings, and
//! quick settings (the options a developer reaches for while a file is open: wrap, minimap, line
//! numbers, whitespace, format, zoom, zen) as one list with the current state beside each.

use crate::{
    app::App,
    app_overlay::{Action, Cmd, Item, Overlay},
    ui::{Canvas, Rect},
};

/// One button of the strip.
const BTN: i32 = 28;
const PAD: i32 = 4;
/// Buttons of the strip, left to right: find in file, all settings, quick editor settings.
const ICONS: [&str; 3] = ["hugeicons/search", "ui/option", "ui/settings"];
/// Width reserved at the right end of the tab bar.
pub const QUICK_W: i32 = BTN * 3 + PAD * 2;

fn on_off(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}

/// Tooltip of each button: (title, command id of its shortcut, detail).
pub const TIPS: [(&str, Option<&str>, &str); 3] = [
    ("Find in file", Some("Find"), "Search the open file"),
    ("All settings", Some("OpenSettings"), "Every option, grouped in pages"),
    ("Editor settings", None, "Wrap, minimap, line numbers, whitespace, zoom, zen"),
];

/// Index of the strip button under `(x, y)` and its rect.
pub fn button_at(strip: Rect, x: f64, y: f64) -> Option<(usize, Rect)> {
    if strip.w <= 0 {
        return None;
    }
    (0..ICONS.len()).map(|i| (i, button_rect(strip, i))).find(|(_, r)| r.contains(x, y))
}

fn button_rect(strip: Rect, i: usize) -> Rect {
    Rect::new(strip.x + PAD + i as i32 * BTN, strip.y + (strip.h - BTN + 2) / 2, BTN - 2, BTN - 2)
}

impl App {
    /// Draw the strip over the right end of the tab bar (tabs end `QUICK_W` before it).
    pub fn draw_quick(&mut self, cv: &mut Canvas) {
        self.layout.quick = Rect::default();
        let bar = self.layout.tabbar;
        if bar.h <= 0 || bar.w < QUICK_W * 3 {
            return;
        }
        let c = self.theme.chrome;
        let strip = Rect::new(bar.right() - QUICK_W, bar.y, QUICK_W, bar.h);
        cv.fill(strip, c.tabbar_bg);
        for (i, icon) in ICONS.iter().enumerate() {
            let btn = button_rect(strip, i);
            let hot = btn.contains(self.mouse.0, self.mouse.1);
            if hot {
                cv.fill_round(btn, 7, c.list_hover_bg);
            }
            let tint = if hot { c.tab_active_fg } else { c.tab_inactive_fg };
            self.tx.icon(cv, icon, btn.x + (btn.w - 16) / 2, btn.y + (btn.h - 16) / 2, 16, tint, strip);
        }
        self.layout.quick = strip;
    }

    /// Mouse press on the strip; true when it was taken.
    pub fn quick_click(&mut self, x: f64, y: f64) -> bool {
        let strip = self.layout.quick;
        if strip.w <= 0 || !strip.contains(x, y) {
            return false;
        }
        match (0..ICONS.len()).find(|&i| button_rect(strip, i).inset(-1).contains(x, y)) {
            Some(0) => self.open_find(false),
            Some(1) => self.open_settings(crate::settingsui::Page::Settings),
            Some(_) => self.open_quick_settings(),
            None => {}
        }
        self.request_redraw();
        true
    }

    /// The list: each row runs the command that flips the setting.
    pub fn open_quick_settings(&mut self) {
        let ws = crate::whitespace::Render::from_name(&self.config.render_whitespace) != crate::whitespace::Render::Off;
        let row =
            |label: &str, detail: String, cmd: Cmd| Item { label: label.to_string(), detail, action: Action::Cmd(cmd) };
        let items = vec![
            row("Word wrap", on_off(self.config.word_wrap), Cmd::ToggleWordWrap),
            row("Minimap", on_off(self.config.minimap), Cmd::ToggleMinimap),
            row("Line numbers", on_off(self.config.show_line_numbers), Cmd::ToggleLineNumbers),
            row("Show whitespace", on_off(ws), Cmd::ToggleRenderWhitespace),
            row("Format on save", on_off(self.config.format_on_save), Cmd::ToggleFormatOnSave),
            row("Format document", "Shift+Alt+F".into(), Cmd::Format),
            row("Zoom in", format!("{}px", self.config.buffer_font_size), Cmd::ZoomIn),
            row("Zoom out", String::new(), Cmd::ZoomOut),
            row("Zen mode", String::new(), Cmd::ToggleZen),
            row("All settings…", "Ctrl+,".into(), Cmd::OpenSettings),
        ];
        self.overlay = Overlay::Picker(crate::app_overlay::choice_picker("Editor quick settings", items, 0));
        self.request_redraw();
    }
}
