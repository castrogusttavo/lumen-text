//! Applying the font and editor-display settings live, the gutter click that toggles breakpoints,
//! the palette commands for them, and the master switch for language servers.

use crate::{
    app::App,
    app_overlay::Cmd,
    config::Config,
    display::{self, Scrollbar},
    fontcfg::{self, FontSpec},
    ui::TextCtx,
};
use serde_json::{Value, json};

/// The buffer font group of a config.
pub fn buffer_spec(c: &Config) -> FontSpec {
    FontSpec {
        family: c.buffer_font_family.clone(),
        size: c.buffer_font_size,
        weight: c.buffer_font_weight,
        line_height: c.buffer_line_height,
        ligatures: c.buffer_font_ligatures,
        features: c.view.buffer_font_features.clone(),
        letter_spacing: c.view.buffer_letter_spacing,
    }
}

/// The UI font group of a config.
pub fn ui_spec(c: &Config) -> FontSpec {
    FontSpec {
        family: c.ui_font_family.clone(),
        size: c.ui_font_size,
        weight: c.ui_font_weight,
        line_height: c.ui_line_height,
        ligatures: c.ui_font_ligatures,
        ..FontSpec::ui_default()
    }
}

/// Apply the saved fonts to a fresh text context (before the first frame). Returns the families
/// that are not installed (the defaults stand in for them).
pub fn startup_fonts(tx: &mut TextCtx, c: &Config) -> Vec<String> {
    let mut missing = Vec::new();
    let (b, u) = (buffer_spec(c), ui_spec(c));
    if b != FontSpec::buffer_default() && !tx.set_buffer_font(&b) {
        missing.push(b.family);
    }
    if u != FontSpec::ui_default() && !tx.set_ui_font(&u) {
        missing.push(u.family);
    }
    display::apply(c);
    missing
}

impl Config {
    /// Are language servers allowed to run? (`enable_lsp`)
    pub fn lsp_enabled(&self) -> bool {
        self.enable_lsp
    }
}

impl App {
    /// Master switch for every language server: false stops them all and keeps them from starting
    /// (no diagnostics, hover, completion or definition requests either, since those need a
    /// running server). Every place that would start or talk to a server checks this one helper.
    pub fn lsp_enabled(&self) -> bool {
        self.config.lsp_enabled()
    }

    /// Re-apply the buffer and/or UI font group after a setting changed, then refresh whatever
    /// depends on text metrics.
    pub(crate) fn refit_fonts(&mut self, buffer: bool, ui: bool) {
        let mut missing: Vec<String> = Vec::new();
        if buffer && !self.tx.set_buffer_font(&buffer_spec(&self.config)) {
            missing.push(self.config.buffer_font_family.clone());
        }
        if ui && !self.tx.set_ui_font(&ui_spec(&self.config)) {
            missing.push(self.config.ui_font_family.clone());
        }
        for d in &mut self.docs {
            d.cell_w = self.tx.cell_w;
            d.reveal = true;
        }
        self.tooltip = None;
        if !missing.is_empty() {
            self.say(format!("font not found: {} (using the default)", missing.join(", ")));
        }
        self.request_redraw();
    }

    /// Settings handled here; false when `key` is not one of them.
    pub(crate) fn apply_ext_setting(&mut self, key: &str, v: &Value) -> bool {
        let b = v.as_bool();
        let s = v.as_str().map(fontcfg::clean_family).unwrap_or_default();
        let n = v.as_f64().unwrap_or(f64::NAN);
        match key {
            "buffer_font_family" => {
                self.config.buffer_font_family = s;
                self.refit_fonts(true, false);
            }
            "buffer_font_size" => {
                let c = &mut self.config;
                c.buffer_font_size =
                    fontcfg::clean_size(n, fontcfg::BUFFER_SIZE_RANGE, fontcfg::DEFAULT_BUFFER_SIZE).round();
                self.refit_fonts(true, false);
            }
            "buffer_font_weight" => {
                self.config.buffer_font_weight = fontcfg::clean_weight(if n.is_finite() { n as i64 } else { 400 });
                self.refit_fonts(true, false);
                let w = self.config.buffer_font_weight;
                self.say(format!("buffer font weight: {w} {}", fontcfg::weight_name(w)));
            }
            "buffer_line_height" => {
                self.config.buffer_line_height = fontcfg::clean_line_height(n, fontcfg::DEFAULT_BUFFER_LH);
                self.refit_fonts(true, false);
            }
            "buffer_font_ligatures" => {
                self.config.buffer_font_ligatures = b.unwrap_or(false);
                self.refit_fonts(true, false);
            }
            "ui_font_family" => {
                self.config.ui_font_family = s;
                self.refit_fonts(false, true);
            }
            "ui_font_size" => {
                self.config.ui_font_size =
                    fontcfg::clean_size(n, fontcfg::UI_SIZE_RANGE, fontcfg::DEFAULT_UI_SIZE).round();
                self.refit_fonts(false, true);
            }
            "ui_font_weight" => {
                self.config.ui_font_weight = fontcfg::clean_weight(if n.is_finite() { n as i64 } else { 400 });
                self.refit_fonts(false, true);
                let w = self.config.ui_font_weight;
                self.say(format!("interface font weight: {w} {}", fontcfg::weight_name(w)));
            }
            "ui_line_height" => {
                self.config.ui_line_height = fontcfg::clean_line_height(n, fontcfg::DEFAULT_UI_LH);
                self.refit_fonts(false, true);
            }
            "ui_font_ligatures" => {
                self.config.ui_font_ligatures = b.unwrap_or(true);
                self.refit_fonts(false, true);
            }
            "show_line_numbers" | "show_bookmarks" | "show_breakpoints" => {
                let on = b.unwrap_or(true);
                match key {
                    "show_line_numbers" => {
                        self.config.show_line_numbers = on;
                        display::set_line_numbers(on);
                    }
                    "show_bookmarks" => {
                        self.config.show_bookmarks = on;
                        display::set_bookmarks(on);
                    }
                    _ => {
                        self.config.show_breakpoints = on;
                        display::set_breakpoints(on);
                    }
                }
                for d in &mut self.docs {
                    d.reveal = true;
                }
            }
            "scrollbar" => {
                self.config.scrollbar = crate::config::normalize_scrollbar(v.as_str());
                display::set_scrollbar(Scrollbar::from_name(&self.config.scrollbar));
                display::touch();
            }
            "bidi_paragraph" => {
                self.config.bidi_paragraph = crate::bidi::normalize(v.as_str());
                crate::bidi::set_para(crate::bidi::Para::from_name(&self.config.bidi_paragraph));
            }
            // Read by the Markdown reader on its next draw.
            "markdown_remote_images" => self.config.markdown_remote_images = b.unwrap_or(true),
            // Read when the app quits / the next backup pass runs, and when a file is opened or closed.
            "hot_exit" => self.config.hot_exit = b.unwrap_or(true),
            "persistent_undo" => self.config.persistent_undo = b.unwrap_or(true),
            "render_whitespace" => {
                self.config.render_whitespace = crate::whitespace::normalize(v.as_str());
                display::set_whitespace(crate::whitespace::Render::from_name(&self.config.render_whitespace));
            }
            // Read on the next save.
            "trim_trailing_whitespace" => self.config.trim_trailing_whitespace = b.unwrap_or(false),
            "insert_final_newline" => self.config.insert_final_newline = b.unwrap_or(false),
            // Read when a session next finishes or asks.
            "ai_notifications" => self.config.ai_notifications = b.unwrap_or(true),
            // An open diff follows on its next draw (`diff_sync_ws`).
            "diff_ignore_whitespace" => self.config.diff_ignore_whitespace = b.unwrap_or(false),
            "build_command" | "run_command" => {
                let cmd = v.as_str().unwrap_or("").trim().to_string();
                if key == "build_command" {
                    self.config.build_command = cmd;
                } else {
                    self.config.run_command = cmd;
                }
            }
            // The registry is rebuilt from it on the next tick (`lspx_sync_config`): the spell
            // checkers start or stop with the documents they apply to.
            "spell_check" => self.config.spell_check = b.unwrap_or(false),
            "enable_lsp" => {
                let on = b.unwrap_or(true);
                if on != self.config.enable_lsp {
                    self.config.enable_lsp = on;
                    if on {
                        // Servers start again lazily, the next time a document needs one.
                        for d in &mut self.docs {
                            d.lsp_open = false;
                        }
                    } else {
                        self.stop_all_lsp();
                    }
                }
            }
            _ => return self.apply_assist_setting(key, v),
        }
        true
    }

    /// Shut every language server down and forget what the documents had synced to them.
    fn stop_all_lsp(&mut self) {
        for l in &mut self.lsps {
            crate::doc::unregister_mirror(&l.root);
            l.shutdown();
        }
        self.lsps.clear();
        for d in &mut self.docs {
            d.lsp_open = false;
            d.biome_open = false;
            d.set_source_diags(false, Vec::new());
            d.set_source_diags(true, Vec::new());
            d.lenses.clear();
            d.inlay.reset();
        }
        self.completion = None;
    }

    /// Palette commands of the font and display options. True when `cmd` was one of them.
    pub fn run_display_cmd(&mut self, cmd: Cmd) -> bool {
        let c = &self.config;
        let (key, val): (&str, Value) = match cmd {
            Cmd::ToggleBookmark => return self.toggle_bookmark_cmd(),
            Cmd::NextBookmark => return self.goto_bookmark_cmd(true),
            Cmd::PrevBookmark => return self.goto_bookmark_cmd(false),
            Cmd::ChooseBufferFont => {
                self.open_font_picker("buffer_font_family", false);
                return true;
            }
            Cmd::ChooseUiFont => {
                self.open_font_picker("ui_font_family", false);
                return true;
            }
            Cmd::ToggleLineNumbers => ("show_line_numbers", json!(!c.show_line_numbers)),
            Cmd::ToggleBookmarkMarks => ("show_bookmarks", json!(!c.show_bookmarks)),
            Cmd::ToggleBreakpointMarks => ("show_breakpoints", json!(!c.show_breakpoints)),
            Cmd::ToggleLsp => ("enable_lsp", json!(!c.enable_lsp)),
            Cmd::ToggleSpellCheck => ("spell_check", json!(!c.spell_check)),
            Cmd::CycleScrollbar => {
                let next = match c.scrollbar.as_str() {
                    "show" => "auto",
                    "auto" => "never",
                    _ => "show",
                };
                ("scrollbar", json!(next))
            }
            _ => return false,
        };
        self.apply_setting(key, &val);
        let msg = match key {
            "scrollbar" => format!("scrollbars: {}", val.as_str().unwrap_or("show")),
            "enable_lsp" => if val == json!(true) { "language servers: on" } else { "language servers: off" }.into(),
            "spell_check" => if val == json!(true) {
                "spell checking: on (harper-ls / typos-lsp, when installed)"
            } else {
                "spell checking: off"
            }
            .into(),
            _ => format!(
                "{}: {}",
                key.trim_start_matches("show_").replace('_', " "),
                if val == json!(true) { "on" } else { "off" }
            ),
        };
        self.say(msg);
        true
    }

    fn toggle_bookmark_cmd(&mut self) -> bool {
        self.toggle_bookmark();
        true
    }

    fn goto_bookmark_cmd(&mut self, forward: bool) -> bool {
        self.goto_bookmark(forward);
        true
    }

    /// A click in the gutter's mark column toggles a breakpoint on that line. True when it did.
    pub fn gutter_breakpoint_click(&mut self, x: f64, y: f64) -> bool {
        if !display::show_breakpoints() {
            return false;
        }
        let Some(d) = self.docs.get(self.active) else { return false };
        let col = d.rect.x as f64..(d.rect.x + crate::display::BP_CLICK_W) as f64;
        if !col.contains(&x) || y < d.rect.y as f64 || y >= d.rect.bottom() as f64 || d.path().is_none() {
            return false;
        }
        let (p, _) = d.hit_at((d.rect.x + d.gutter_w() + 1) as f64, y);
        let line = d.buf.line_col(p).0;
        self.toggle_breakpoint_at(line);
        true
    }

    /// Note scrolling of the visible panes: the auto-hiding scrollbars show up, and the frame loop
    /// keeps running while they fade. Returns true while a fade is in progress.
    pub fn scroll_activity(&mut self) -> bool {
        let d = self.docs.get(self.active);
        let sig = (d.map_or(0, |d| d.top.to_bits() ^ d.left.to_bits().rotate_left(17))
            ^ self.tree.scroll.to_bits().rotate_left(31)
            ^ self.panel.scroll.to_bits().rotate_left(47))
            | 1;
        if display::scroll_changed(sig) {
            display::touch();
        }
        display::fading()
    }
}
