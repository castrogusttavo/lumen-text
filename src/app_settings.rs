//! Glue between the settings/keybindings modal (`settingsui`) and the app: opening it with the
//! current values, applying its events (config.json, keybindings.json) and routing input.

use crate::{
    app::{App, KeyIn},
    app_overlay::{COMMANDS, Overlay},
    keymap,
    settingsui::{CommandRow, Event, KeybindsPage, Modal, Page, SettingsPage, default_settings, default_values},
    ui::Rect,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use winit::event::MouseScrollDelta;

fn keybindings_path() -> Option<std::path::PathBuf> {
    crate::config::default_path().map(|p| p.with_file_name("keybindings.json"))
}

impl App {
    fn config_values(&self) -> Value {
        let c = &self.config;
        json!({
            "format_on_save": c.format_on_save, "auto_import": c.auto_import, "font_size": c.font_size,
            "lsp_narrow": c.lsp_narrow, "code_lens": c.code_lens, "inlay_hints": c.inlay_hints, "appearance": c.appearance, "theme_chrome": c.theme_chrome,
            "theme": c.theme.clone().unwrap_or_else(|| self.theme.name.clone()),
            "theme_light": c.theme_light, "theme_dark": c.theme_dark, "tsgo_source": c.tsgo_source,
            "minimap": c.minimap, "minimap_style": c.minimap_style, "word_wrap": c.word_wrap, "sticky_scroll": c.sticky_scroll, "indent_guides": c.indent_guides,
            "ui_scale": c.ui_scale,
            "breadcrumbs": c.breadcrumbs, "git_blame": c.git_blame, "ram_display": c.ram_display,
            "status_search": c.status_visible("search"), "status_problems": c.status_visible("problems"),
            "status_position": c.status_visible("position"), "status_indent": c.status_visible("indent"),
            "status_language": c.status_visible("language"), "status_terminal": c.status_visible("terminal"),
            "status_debug": c.status_visible("debug"), "status_git": c.status_visible("git"), "status_ram": c.status_visible("ram"),
            "status_pos_position": c.status_slot("position").map_or(1, |s| s + 1), "status_pos_indent": c.status_slot("indent").map_or(2, |s| s + 1),
            "status_pos_language": c.status_slot("language").map_or(3, |s| s + 1), "status_pos_terminal": c.status_slot("terminal").map_or(4, |s| s + 1),
            "status_pos_debug": c.status_slot("debug").map_or(5, |s| s + 1), "status_pos_git": c.status_slot("git").map_or(6, |s| s + 1),
            "status_pos_ram": c.status_slot("ram").map_or(7, |s| s + 1),
        })
    }

    pub fn open_settings(&mut self, page: Page) {
        let mut sp = SettingsPage::new(default_settings(), self.config_values());
        sp.set_defaults(default_values());
        let names = self.theme_names();
        for k in ["theme", "theme_light", "theme_dark"] {
            sp.set_choices(k, names.clone());
        }
        let rows: Vec<CommandRow> = COMMANDS
            .iter()
            .map(|(label, _, cmd)| CommandRow {
                id: format!("{cmd:?}"),
                label: label.to_string(),
                default: keymap::default_combo(&format!("{cmd:?}")).map(str::to_string),
            })
            .collect();
        let user = self.keymap.entries();
        let mut kb = KeybindsPage::new(rows, user);
        // Aliases (`ctrl+y` for Redo, `ctrl+k s` for Save, ...): every default row but a command's first.
        let aliases = keymap::DEFAULTS.iter().filter(|(c, id)| keymap::default_combo(id) != Some(*c)).map(|(c, id)| (c.to_string(), id.to_string()));
        kb.add_default_aliases(aliases);
        self.overlay = Overlay::Settings(Box::new(Modal::new(sp, kb, page)));
    }

    fn window_rect(&self) -> Rect {
        Rect::new(0, 0, self.size.0 as i32, self.size.1 as i32)
    }

    pub fn draw_settings(&mut self, cv: &mut crate::ui::Canvas) {
        let window = self.window_rect();
        if let Overlay::Settings(m) = &mut self.overlay {
            m.draw(cv, &mut self.tx, &self.theme, window);
        }
    }

    pub fn settings_key(&mut self, ev: &KeyIn) {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let Overlay::Settings(m) = &mut self.overlay else { return };
        let e = m.key(&ev.key, ev.text.as_deref(), ctrl, shift, alt);
        self.apply_settings_event(e);
    }

    pub fn settings_mouse(&mut self, x: f64, y: f64) {
        let Overlay::Settings(m) = &mut self.overlay else { return };
        let e = m.mouse_down(x, y);
        self.apply_settings_event(e);
    }

    pub fn settings_wheel(&mut self, delta: MouseScrollDelta) {
        let Overlay::Settings(m) = &mut self.overlay else { return };
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => -(y as f64) * 3.0 * 34.0,
            MouseScrollDelta::PixelDelta(p) => -p.y,
        };
        m.wheel(dy);
    }

    fn apply_settings_event(&mut self, e: Event) {
        for e in e.flatten() {
            match e {
                Event::Close => {
                    self.overlay = Overlay::None;
                    return;
                }
                Event::Setting { key, value } => self.apply_setting(&key, &value),
                // Re-assigning a command also switches its built-in combo off ("none").
                Event::Bind { combo, command_id } => self.edit_keybindings(|m| keymap::bind_in(m, &combo, &command_id)),
                Event::Unbind { combo } => self.edit_keybindings(|m| {
                    m.remove(&combo);
                }),
                Event::ResetAll => self.edit_keybindings(|m| m.clear()),
                Event::None | Event::Batch(_) => {}
            }
        }
        self.request_redraw();
    }

    fn edit_keybindings(&mut self, f: impl FnOnce(&mut HashMap<String, String>)) {
        let mut m = self.keymap.entries();
        f(&mut m);
        let mut obj = serde_json::Map::new();
        let mut keys: Vec<_> = m.keys().cloned().collect();
        keys.sort();
        for k in keys {
            obj.insert(k.clone(), Value::String(m[&k].clone()));
        }
        let text = serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default();
        if let Some(p) = keybindings_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&p, &text);
        }
        self.keymap = keymap::Keymap::from_json(&text);
    }

    /// Palette command "Alternar git blame": same effect as the switch in the settings.
    pub fn toggle_git_blame(&mut self) {
        let on = !self.config.git_blame;
        self.apply_setting("git_blame", &json!(on));
        self.say(if on { "git blame: ligado" } else { "git blame: desligado" });
    }

    /// Load the current theme again (same file, same name) so a change of `theme_chrome` shows
    /// right away. `LUMEN_THEME` keeps winning, as it does at startup.
    fn reload_theme(&mut self) {
        let path = std::env::var_os("LUMEN_THEME").map(std::path::PathBuf::from).unwrap_or_else(|| {
            let name = self.config.resolve_theme(if self.config.appearance == "system" { crate::config::detect_dark() } else { None });
            self.theme_path(&name)
        });
        match crate::theme::Theme::load(&path) {
            Ok(t) => {
                self.theme = t;
                for d in &mut self.docs {
                    if let Some(h) = d.hl.as_mut() {
                        h.theme_changed();
                    }
                }
            }
            Err(e) => self.say(format!("tema inválido: {e}")),
        }
    }

    fn apply_setting(&mut self, key: &str, v: &Value) {
        let b = v.as_bool();
        let s = v.as_str().map(str::to_string);
        match key {
            "format_on_save" => self.config.format_on_save = b.unwrap_or(false),
            "auto_import" => self.config.auto_import = b.unwrap_or(false),
            "lsp_narrow" => self.config.lsp_narrow = b.unwrap_or(true),
            "code_lens" => {
                self.config.code_lens = b.unwrap_or(false);
                for d in &mut self.docs {
                    d.lens_due = self.config.code_lens.then(std::time::Instant::now);
                    if !self.config.code_lens {
                        d.lenses.clear();
                    }
                }
            }
            "inlay_hints" => {
                self.config.inlay_hints = b.unwrap_or(false);
                self.refresh_inlay_hints();
            }
            "font_size" => {
                let size = self.tx.set_zoom(v.as_f64().unwrap_or(14.0) as f32);
                self.config.font_size = size;
            }
            "appearance" => {
                self.config.appearance = s.unwrap_or_else(|| "fixed".into());
                self.follow_system_theme();
            }
            "theme_chrome" => {
                self.config.theme_chrome = if s.as_deref() == Some("theme") { "theme".into() } else { "derived".into() };
                crate::theme::set_theme_chrome(self.config.own_chrome());
                self.reload_theme();
            }
            "theme" => {
                if let Some(n) = s {
                    self.config.appearance = "fixed".into();
                    self.set_theme_inner(&n, false);
                }
            }
            "theme_light" | "theme_dark" => {
                if let Some(n) = s {
                    if key == "theme_light" { self.config.theme_light = n } else { self.config.theme_dark = n }
                    self.follow_system_theme();
                }
            }
            "ui_scale" => {
                let v = v.as_f64().unwrap_or(0.0) as f32;
                // 0 = automatic; anything else is kept within the usable range.
                self.config.ui_scale = if v < 0.1 { 0.0 } else { v.clamp(0.5, 4.0) };
                self.apply_scale();
            }
            "tsgo_source" => self.config.tsgo_source = s.unwrap_or_else(|| "auto".into()),
            "minimap" => {
                self.config.minimap = b.unwrap_or(true);
                self.minimap_default = self.config.minimap;
                for d in &mut self.docs {
                    d.minimap = self.config.minimap;
                }
            }
            "minimap_style" => {
                self.config.minimap_style = if s.as_deref() == Some("normal") { "normal".into() } else { "subtle".into() };
                crate::minimap::set_style(crate::minimap::Style::from_name(&self.config.minimap_style));
            }
            "word_wrap" => {
                self.config.word_wrap = b.unwrap_or(false);
                for d in &mut self.docs {
                    d.set_word_wrap(self.config.word_wrap);
                }
                self.split_wrap_all(self.config.word_wrap);
            }
            "sticky_scroll" => {
                self.config.sticky_scroll = b.unwrap_or(true);
                for d in &mut self.docs {
                    d.sticky = self.config.sticky_scroll;
                }
            }
            "indent_guides" => {
                self.config.indent_guides = b.unwrap_or(true);
                for d in &mut self.docs {
                    d.indent_guides = self.config.indent_guides;
                }
            }
            "breadcrumbs" => self.config.breadcrumbs = b.unwrap_or(true),
            "git_blame" => {
                self.config.git_blame = b.unwrap_or(true);
                if !self.config.git_blame {
                    for d in &mut self.docs {
                        d.blame = None;
                    }
                }
            }
            "ram_display" => self.config.ram_display = if s.as_deref() == Some("short") { "short".into() } else { "long".into() },
            k if k.starts_with("status_pos_") => {
                // Stepper of an item's slot in the right-hand group: moves it there, live.
                let slot = (v.as_f64().unwrap_or(1.0).round().max(1.0) as usize) - 1;
                self.config.move_status_to(&k["status_pos_".len()..], slot);
                self.tooltip = None;
                // The other items shifted: refresh every stepper of the open modal.
                let vals = self.config_values();
                if let Overlay::Settings(m) = &mut self.overlay {
                    let mut merged = m.settings.values();
                    if let (Some(dst), Some(src)) = (merged.as_object_mut(), vals.as_object()) {
                        for (key, val) in src.iter().filter(|(key, _)| key.starts_with("status_pos_")) {
                            dst.insert(key.clone(), val.clone());
                        }
                    }
                    m.settings.set_values(merged);
                }
            }
            k if k.starts_with("status_") => {
                // One switch per bottom bar item; the mandatory one has none (and ignores `false`).
                self.config.set_status_item(&k["status_".len()..], b.unwrap_or(true));
                self.tooltip = None;
            }
            _ => return,
        }
        self.config.save();
    }
}
