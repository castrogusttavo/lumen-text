//! Settings rows for the font and editor-display options, kept apart from the main list so the
//! model in `settingsui` stays small: [`defs`] are appended to `default_settings`, [`defaults`]
//! to `default_values`, and [`values`] to the app's current values.

use crate::{config::Config, fontcfg, settingsui::SettingDef, settingsui::SettingKind};
use serde_json::{Value, json};

pub fn defs() -> Vec<SettingDef> {
    use SettingKind::*;
    let d = |key, section, label, help, kind| SettingDef { key, section, label, help, kind };
    vec![
        d(
            "buffer_font_family",
            "Fonts",
            "Buffer font",
            "Family of the code editor and everything monospace (terminal, output, diffs, code blocks). Click to search the installed fonts or type a name; a missing family falls back to the system monospace.",
            Text,
        ),
        d(
            "buffer_font_size",
            "Fonts",
            "Buffer font size",
            "Code text size in pixels (8 to 40). Ctrl + mouse wheel and Ctrl +/- adjust it too.",
            Number { min: 8.0, max: 40.0, step: 1.0 },
        ),
        d(
            "buffer_font_weight",
            "Fonts",
            "Buffer font weight",
            "100 Thin, 200 Extra Light, 300 Light, 400 Regular, 500 Medium, 600 Semi Bold, 700 Bold, 800 Extra Bold, 900 Black. The nearest weight the family has is used.",
            Number { min: 100.0, max: 900.0, step: 100.0 },
        ),
        d(
            "buffer_line_height",
            "Fonts",
            "Buffer line height",
            "Row height as a multiple of the font size (1.0 to 2.5). In config.json a number above 4 means pixels.",
            Number { min: 1.0, max: 2.5, step: 0.05 },
        ),
        d(
            "buffer_font_ligatures",
            "Fonts",
            "Buffer ligatures",
            "Joins sequences such as => and != into one glyph when the font has them. Each ligature keeps the width of its characters and the caret still moves one character at a time.",
            Bool,
        ),
        d(
            "ui_font_family",
            "Fonts",
            "Interface font",
            "Family of the sidebar, tabs, menus, dialogs, status bar and hover text. Click to search the installed fonts or type a name.",
            Text,
        ),
        d(
            "ui_font_size",
            "Fonts",
            "Interface font size",
            "Interface text size in pixels (10 to 20).",
            Number { min: 10.0, max: 20.0, step: 1.0 },
        ),
        d(
            "ui_font_weight",
            "Fonts",
            "Interface font weight",
            "100 to 900; the nearest weight the family has is used. Bold interface text stays heavier than this.",
            Number { min: 100.0, max: 900.0, step: 100.0 },
        ),
        d(
            "ui_line_height",
            "Fonts",
            "Interface line height",
            "Line spacing of multi-line interface text such as hover cards, as a multiple of the font size (1.0 to 2.5). Single-line labels keep their rows.",
            Number { min: 1.0, max: 2.5, step: 0.05 },
        ),
        d(
            "ui_font_ligatures",
            "Fonts",
            "Interface ligatures",
            "Uses the interface font's standard and contextual ligatures.",
            Bool,
        ),
        d(
            "show_line_numbers",
            "Editor display",
            "Line numbers",
            "Shows the line numbers in the gutter. Without them the gutter shrinks to the marks and the fold arrows.",
            Bool,
        ),
        d(
            "show_bookmarks",
            "Editor display",
            "Bookmarks",
            "Shows bookmark marks in the gutter. Ctrl+F2 toggles a bookmark, Ctrl+Alt+F2 and Ctrl+Alt+Shift+F2 jump to the next and previous one.",
            Bool,
        ),
        d(
            "show_breakpoints",
            "Editor display",
            "Breakpoints",
            "Shows breakpoint dots in the gutter and lets you click the gutter to toggle one. Off hides them and the click; debugging still stops at them.",
            Bool,
        ),
        d(
            "scrollbar",
            "Editor display",
            "Scrollbars",
            "Show keeps the thin bars visible; Auto fades them in while you scroll or point at them; Never hides them (the wheel and keys still scroll).",
            Choice(&["show", "auto", "never"]),
        ),
        d(
            "enable_lsp",
            "Editor display",
            "Language servers",
            "Master switch: off stops every language server (no diagnostics, hover, completion or go to definition) and the status bar says so. On starts them again when needed.",
            Bool,
        ),
    ]
}

pub fn defaults() -> Value {
    json!({
        "buffer_font_family": "", "buffer_font_size": 14, "buffer_font_weight": 400,
        "buffer_line_height": fontcfg::DEFAULT_BUFFER_LH, "buffer_font_ligatures": false,
        "ui_font_family": "", "ui_font_size": 13, "ui_font_weight": 400,
        "ui_line_height": fontcfg::DEFAULT_UI_LH, "ui_font_ligatures": true,
        "show_line_numbers": true, "show_bookmarks": true, "show_breakpoints": true,
        "scrollbar": "show", "enable_lsp": true,
    })
}

pub fn values(c: &Config) -> Value {
    json!({
        "buffer_font_family": c.buffer_font_family, "buffer_font_size": c.buffer_font_size,
        "buffer_font_weight": c.buffer_font_weight, "buffer_line_height": c.buffer_line_height,
        "buffer_font_ligatures": c.buffer_font_ligatures,
        "ui_font_family": c.ui_font_family, "ui_font_size": c.ui_font_size, "ui_font_weight": c.ui_font_weight,
        "ui_line_height": c.ui_line_height, "ui_font_ligatures": c.ui_font_ligatures,
        "show_line_numbers": c.show_line_numbers, "show_bookmarks": c.show_bookmarks,
        "show_breakpoints": c.show_breakpoints, "scrollbar": c.scrollbar, "enable_lsp": c.enable_lsp,
    })
}

/// The settings key whose row opens the font picker instead of an inline text field.
pub fn picker_key(key: &str) -> Option<&'static str> {
    match key {
        "buffer_font_family" => Some("buffer_font_family"),
        "ui_font_family" => Some("ui_font_family"),
        _ => None,
    }
}
