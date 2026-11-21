//! Quick settings: a button at the right end of the tab bar that opens the options a developer
//! reaches for while a file is open (wrap, minimap, line numbers, whitespace, format, zoom, zen)
//! as one list, with the current state beside each, without going through the settings page.

use crate::{
    app::App,
    app_overlay::{Action, Cmd, Item, Overlay},
    ui::{Canvas, Rect},
};

/// Width reserved at the right end of the tab bar.
pub const QUICK_W: i32 = 36;

fn on_off(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}

impl App {
    /// Draw the button over the right end of the tab bar (tabs end `QUICK_W` before it).
    pub fn draw_quick(&mut self, cv: &mut Canvas) {
        self.layout.quick = Rect::default();
        let bar = self.layout.tabbar;
        if bar.h <= 0 || bar.w < QUICK_W * 3 {
            return;
        }
        let c = self.theme.chrome;
        let b = Rect::new(bar.right() - QUICK_W, bar.y, QUICK_W, bar.h);
        cv.fill(b, c.tabbar_bg);
        let hot = b.contains(self.mouse.0, self.mouse.1);
        let btn = Rect::new(b.x + 4, b.y + (b.h - 26) / 2, 26, 26);
        if hot {
            cv.fill_round(btn, 7, c.list_hover_bg);
        }
        let tint = if hot { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.icon(cv, "ui/ellipsis", btn.x + 5, btn.y + 5, 16, tint, b);
        self.layout.quick = b;
    }

    /// Mouse press on the button; true when it was taken.
    pub fn quick_click(&mut self, x: f64, y: f64) -> bool {
        if self.layout.quick.w <= 0 || !self.layout.quick.contains(x, y) {
            return false;
        }
        self.open_quick_settings();
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
