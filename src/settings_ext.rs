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
            "Never (the default) hides them (the wheel, touchpad and keys still scroll); Show keeps the thin bars visible; Auto fades them in while you scroll or point at them.",
            Choice(&["show", "auto", "never"]),
        ),
        d(
            "enable_lsp",
            "Editor display",
            "Language servers",
            "Master switch: off stops every language server (no diagnostics, hover, completion or go to definition) and the status bar says so. On starts them again when needed.",
            Bool,
        ),
        d(
            "spell_check",
            "Editor display",
            "Spell checking",
            "Underlines spelling and grammar slips in comments, strings, Markdown, plain text and commit messages with a dotted info line (never an error), through the companion servers harper-ls and typos-lsp. They start only when installed; Ctrl+. offers the fixes and \"add to dictionary\".",
            Bool,
        ),
        d(
            "inline_completion",
            "Editor display",
            "Inline completions",
            "Faded suggestions at the cursor from language servers that offer them (LSP inline completion), such as GitHub Copilot's copilot-language-server once you install it (npm i -g @github/copilot-language-server) and run \"Copilot: Sign in\". Tab accepts, Ctrl+Right accepts a word, Esc dismisses, Alt+] / Alt+[ show other suggestions, Alt+\\ asks now. Lumen never handles API keys.",
            Bool,
        ),
        d(
            "markdown_remote_images",
            "Editor display",
            "Markdown remote images",
            "Lets the Markdown reader download http(s) images (badges, screenshots) into a local cache. Off keeps a placeholder card instead, so opening a README contacts no server.",
            Bool,
        ),
        d(
            "render_whitespace",
            "Editor display",
            "Render whitespace",
            "Draws spaces as faint dots and tabs as arrows. Boundary shows all but single spaces between words, Trailing only the whitespace at line ends. The palette command \"Toggle render whitespace\" switches it on and off.",
            Choice(&["none", "boundary", "trailing", "all"]),
        ),
        d(
            "trim_trailing_whitespace",
            "Editor",
            "Trim trailing whitespace on save",
            "Removes spaces and tabs at the end of lines when saving, as one undo step. Markdown files are left alone (two trailing spaces are a line break there); an .editorconfig can decide per file.",
            Bool,
        ),
        d(
            "insert_final_newline",
            "Editor",
            "Insert final newline on save",
            "Ends the file with a line break when saving, if it has none. An .editorconfig can decide per file.",
            Bool,
        ),
        d(
            "rulers",
            "Editor display",
            "Rulers",
            "Columns where thin vertical guide lines are drawn, separated by commas (for example 80, 120). Empty: none. A project's .editorconfig max_line_length adds one.",
            Text,
        ),
        d(
            "inline_diagnostics",
            "Editor display",
            "Inline diagnostics",
            "Shows the most severe problem of a line after its code, faded and cut to the room left: Current line (the default) only on the caret's line, All on every line, Off never. Never drawn over the text.",
            Choice(crate::assist::InlineDiags::NAMES),
        ),
        d(
            "problems_scope",
            "Editor display",
            "Problems scope",
            "What the Problems panel lists: open_files (the diagnostics of the open documents) or project (also what the language servers report for files that are not open, asked with workspace pull diagnostics where supported). Alt+W in the panel switches it.",
            Choice(&["open_files", "project"]),
        ),
        d(
            "build_command",
            "Build",
            "Build command",
            "A build command of your own, run by the shell in the project folder (Build panel, Ctrl+Shift+B). It is listed first among the targets and its output is read for compiler problems. Empty: only what is detected (Cargo, npm / pnpm / yarn / bun, Maven, Gradle, .NET, Go, CMake, Make).",
            Text,
        ),
        d(
            "run_command",
            "Build",
            "Run command",
            "A run command of your own for the Build panel's Run (Ctrl+F5), run by the shell in the project folder.",
            Text,
        ),
        d(
            "hot_exit",
            "Files",
            "Hot exit",
            "Quitting keeps unsaved changes (untitled tabs too) and reopens them next time instead of asking to save. Unsaved changes are also backed up a few seconds after each edit, so a crash loses almost nothing. Off asks before quitting, as before.",
            Bool,
        ),
        d(
            "persistent_undo",
            "Files",
            "Persistent undo",
            "Keeps each file's undo history after it is closed (and across restarts), as long as the file was not changed elsewhere. Up to 2 MB per file; the oldest histories are dropped first.",
            Bool,
        ),
        d(
            "ai_notifications",
            "AI",
            "Desktop notifications",
            "Shows a system notification when an agent session is waiting for you or done while Lumen is in the background (once per turn).",
            Bool,
        ),
        d(
            "diff_ignore_whitespace",
            "Git",
            "Diff: ignore whitespace",
            "Side-by-side diffs ignore changes in spaces, tabs and line endings (git diff -w): such lines do not count as changed and are not highlighted. Staging a hunk or lines still stages their real text. Also a toggle in the diff header (key W).",
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
        "scrollbar": "never", "enable_lsp": true, "build_command": "", "run_command": "",
        "markdown_remote_images": true, "hot_exit": true, "persistent_undo": true, "spell_check": false,
        "render_whitespace": "none", "trim_trailing_whitespace": false, "insert_final_newline": false,
        "ai_notifications": true,
        "diff_ignore_whitespace": false,
        "rulers": "", "inline_diagnostics": "current_line",
        "problems_scope": "open_files",
        "inline_completion": false,
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
        "build_command": c.build_command, "run_command": c.run_command,
        "markdown_remote_images": c.markdown_remote_images, "spell_check": c.spell_check,
        "hot_exit": c.hot_exit, "persistent_undo": c.persistent_undo,
        "render_whitespace": c.render_whitespace, "trim_trailing_whitespace": c.trim_trailing_whitespace,
        "insert_final_newline": c.insert_final_newline,
        "ai_notifications": c.ai_notifications,
        "diff_ignore_whitespace": c.diff_ignore_whitespace,
        "rulers": crate::assist::rulers_text(&c.rulers), "inline_diagnostics": c.inline_diagnostics,
        "problems_scope": c.problems_scope,
        "inline_completion": c.inline_completion,
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
