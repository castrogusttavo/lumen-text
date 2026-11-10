use super::*;

// ───────────────────────────── settings model ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    /// Fixed options; an empty slice means "filled at runtime" via [`SettingsPage::set_choices`].
    Choice(&'static [&'static str]),
    Number {
        min: f64,
        max: f64,
        step: f64,
    },
    Text,
}

#[derive(Debug, Clone)]
pub struct SettingDef {
    pub key: &'static str,
    pub section: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: SettingKind,
}

/// Keys that are listed here but NOT yet read/written by `config.rs`; the owner must wire them.
pub const PENDING_KEYS: &[&str] = &["minimap", "word_wrap", "sticky_scroll", "indent_guides"];

pub fn default_settings() -> Vec<SettingDef> {
    use SettingKind::*;
    let d = |key, section, label, help, kind| SettingDef { key, section, label, help, kind };
    vec![
        d(
            "format_on_save",
            "General",
            "Format on save",
            "Runs the language server's formatter every time a file is saved.",
            Bool,
        ),
        d(
            "updates",
            "General",
            "Updates",
            "Automatic checks every 4 hours and downloads the new version in the background; Notify only shows a notice; Off never checks (the \"Check for updates\" command still works).",
            Choice(&["auto", "notify", "off"]),
        ),
        d(
            "font_size",
            "Editor",
            "Font size",
            "Code text size in pixels (8 to 40). Ctrl + mouse wheel adjusts it temporarily.",
            Number { min: 8.0, max: 40.0, step: 1.0 },
        ),
        // --- not in config.rs yet (see PENDING_KEYS) ---
        d("minimap", "Editor", "Minimap", "Shows a thumbnail of the file at the right edge of the editor.", Bool),
        d(
            "minimap_style",
            "Editor",
            "Minimap style",
            "Subtle is narrower and low-contrast; Normal is the original look.",
            Choice(&["subtle", "normal"]),
        ),
        d(
            "word_wrap",
            "Editor",
            "Word wrap",
            "Wraps long lines at the window width instead of scrolling horizontally.",
            Bool,
        ),
        d(
            "sticky_scroll",
            "Editor",
            "Sticky scroll",
            "Keeps the enclosing scopes (function, class) visible at the top while you scroll.",
            Bool,
        ),
        d(
            "tab_width",
            "Editor",
            "Tab width",
            "How many columns a tab character takes in the editor (2, 4 or 8). Per-file indentation stays in the bottom bar.",
            Choice(&["2", "4", "8"]),
        ),
        d("indent_guides", "Editor", "Indent guides", "Draws thin vertical lines at the indentation levels.", Bool),
        d(
            "breadcrumbs",
            "Editor",
            "Breadcrumbs",
            "Shows the file path and symbols above the editor. When off, the editor takes the row's space.",
            Bool,
        ),
        // --- end of pending keys ---
        d(
            "appearance",
            "Appearance",
            "Appearance mode",
            "Fixed uses the chosen theme; System follows the desktop light/dark setting.",
            Choice(&["fixed", "system"]),
        ),
        d("theme", "Appearance", "Theme", "Color theme used in fixed mode.", Choice(&[])),
        d(
            "theme_light",
            "Appearance",
            "Light theme",
            "Theme used when the system is in light mode (appearance: system).",
            Choice(&[]),
        ),
        d(
            "theme_dark",
            "Appearance",
            "Dark theme",
            "Theme used when the system is in dark mode (appearance: system).",
            Choice(&[]),
        ),
        d(
            "theme_chrome",
            "Appearance",
            "Interface colors",
            "Derived keeps the usual calm, flat look; From theme uses the theme's own sidebar, tabs and bottom bar when the text stays legible (the rest falls back to the derived look).",
            Choice(&["derived", "theme"]),
        ),
        d(
            "ui_scale",
            "Appearance",
            "Interface scale",
            "Scale for hi-DPI screens (0.5 to 4). 0 = automatic, uses the system scale (LUMEN_SCALE is also accepted).",
            Number { min: 0.0, max: 4.0, step: 0.25 },
        ),
        d(
            "scroll_speed",
            "Appearance",
            "Scroll speed",
            "Mouse wheel speed (0.1 to 5). 1 = 3 lines per notch, like Zed. Touchpads always scroll exactly with the fingers.",
            Number { min: 0.1, max: 5.0, step: 0.1 },
        ),
        d(
            "explorer_hide_gitignored",
            "Appearance",
            "Hide git-ignored files",
            "Leaves out of the explorer what .gitignore, the global ignore file and .git/info/exclude ignore. Off shows them dimmed.",
            Bool,
        ),
        d(
            "auto_import",
            "TypeScript",
            "Auto import",
            "Automatically adds missing imports when you accept a suggestion.",
            Bool,
        ),
        d(
            "lsp_narrow",
            "TypeScript",
            "Lean mode",
            "Opt-in for low-RAM machines: the TypeScript server only sees open files and their imports (less memory, but references, rename, workspace symbols and errors in other files are incomplete). Off = the whole project, like VS Code.",
            Bool,
        ),
        d(
            "code_lens",
            "TypeScript",
            "Code lens",
            "Shows reference and implementation counts next to symbols (costs server CPU).",
            Bool,
        ),
        d(
            "code_lens_refs",
            "TypeScript",
            "Code lens: references",
            "Shows \"N references\" (needs Code lens on).",
            Bool,
        ),
        d(
            "code_lens_impls",
            "TypeScript",
            "Code lens: implementations",
            "Shows \"N implementations\" (needs Code lens on).",
            Bool,
        ),
        d(
            "inlay_hints",
            "TypeScript",
            "Inlay hints",
            "Shows inferred types and parameter names in gray inside the code (visible lines only).",
            Bool,
        ),
        d(
            "inlay_hints_params",
            "TypeScript",
            "Inlay hints: parameters",
            "Shows the parameter name before each argument (needs Inlay hints on).",
            Bool,
        ),
        d(
            "inlay_hints_types",
            "TypeScript",
            "Inlay hints: types",
            "Shows inferred types of variables, parameters and returns; double-click a type to insert the annotation, Ctrl+click goes to the definition (needs Inlay hints on).",
            Bool,
        ),
        d(
            "tsgo_source",
            "TypeScript",
            "tsgo source",
            "Automatic uses the project's tsgo if installed, otherwise the bundled one.",
            Choice(&["auto", "project", "bundled"]),
        ),
        d(
            "git_pull_strategy",
            "Git",
            "Pull strategy",
            "How Pull integrates the remote commits when the histories have diverged. \"Ask\" shows the choice on the spot; you can also change it by right-clicking Pull.",
            Choice(&["ask", "merge", "rebase", "ff-only"]),
        ),
        d(
            "git_blame",
            "Git",
            "Git blame on line",
            "Shows the author and date of the last commit on the cursor line, in gray at the end of the line. Also available as the \"Toggle git blame\" command.",
            Bool,
        ),
        // Bottom bar: one switch per item. The sidebar toggle (left corner) is mandatory and has none.
        d("status_search", "Status bar", "Search in project", "Project search button (left corner).", Bool),
        d("status_problems", "Status bar", "Problems", "Error and warning counts (left corner).", Bool),
        d("status_position", "Status bar", "Line and column", "Cursor position (Ln, Col) and number of cursors.", Bool),
        d("status_indent", "Status bar", "Indentation", "The file's spaces or tabs.", Bool),
        d("status_language", "Status bar", "Language", "Language of the active file.", Bool),
        d("status_terminal", "Status bar", "Terminal", "Button that opens the integrated terminal.", Bool),
        d("status_debug", "Status bar", "Debug", "Button that opens the debug panel.", Bool),
        d("status_git", "Status bar", "Git", "Git panel button with the changed-file count.", Bool),
        d("status_ram", "Status bar", "Memory", "Memory use of the editor and the language servers.", Bool),
        // Order of the items: a stepper per item with its position among the visible ones of its group
        // (1 = leftmost); - moves it left, + moves it right, hopping over hidden items.
        d(
            "status_pos_sidebar",
            "Status bar",
            "Order: Sidebar",
            "Position in the left group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 3.0, step: 1.0 },
        ),
        d(
            "status_pos_search",
            "Status bar",
            "Order: Search in project",
            "Position in the left group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 3.0, step: 1.0 },
        ),
        d(
            "status_pos_problems",
            "Status bar",
            "Order: Problems",
            "Position in the left group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 3.0, step: 1.0 },
        ),
        d(
            "status_pos_position",
            "Status bar",
            "Order: Line and column",
            "Position in the right group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 7.0, step: 1.0 },
        ),
        d(
            "status_pos_indent",
            "Status bar",
            "Order: Indentation",
            "Position in the right group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 7.0, step: 1.0 },
        ),
        d(
            "status_pos_language",
            "Status bar",
            "Order: Language",
            "Position in the right group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 7.0, step: 1.0 },
        ),
        d(
            "status_pos_terminal",
            "Status bar",
            "Order: Terminal",
            "Position in the right group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 7.0, step: 1.0 },
        ),
        d(
            "status_pos_debug",
            "Status bar",
            "Order: Debug",
            "Position in the right group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 7.0, step: 1.0 },
        ),
        d(
            "status_pos_git",
            "Status bar",
            "Order: Git",
            "Position in the right group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 7.0, step: 1.0 },
        ),
        d(
            "status_pos_ram",
            "Status bar",
            "Order: Memory",
            "Position in the right group (1 = leftmost); − and + move it, skipping hidden items.",
            Number { min: 1.0, max: 7.0, step: 1.0 },
        ),
        d(
            "ram_display",
            "Status bar",
            "Memory format",
            "Long shows the numbers (\"RAM 13 MB + tsgo 65 MB\"); Short shows just a chip icon, with the values in the tooltip.",
            Choice(&["long", "short"]),
        ),
    ]
}

/// Defaults matching `Config::default()` plus the pending keys. Used for the per-row reset icon
/// and as the display value of keys missing from the values map.
pub fn default_values() -> Value {
    let mut v = json!({
        "format_on_save": false, "updates": "auto", "auto_import": false, "font_size": 14, "lsp_narrow": false,
        "code_lens": false, "code_lens_refs": true, "code_lens_impls": true, "inlay_hints": false, "inlay_hints_params": true, "inlay_hints_types": true, "appearance": "fixed", "theme_chrome": "derived", "theme_light": "github-light-default",
        "theme_dark": "github-dark-default", "tsgo_source": "auto",
        "minimap": true, "minimap_style": "subtle", "word_wrap": false, "sticky_scroll": true, "indent_guides": true, "ui_scale": 0, "scroll_speed": 1,
        "breadcrumbs": true, "git_blame": true, "ram_display": "long", "tab_width": "4", "git_pull_strategy": "ask",
        "explorer_hide_gitignored": true,
    });
    // Split in two: one `json!` this long hits the macro recursion limit.
    let bar = json!({
        "status_search": true, "status_problems": true, "status_position": true, "status_indent": true, "status_language": true,
        "status_terminal": true, "status_debug": true, "status_git": true, "status_ram": true,
        "status_pos_sidebar": 1, "status_pos_search": 2, "status_pos_problems": 3,
        "status_pos_position": 1, "status_pos_indent": 2, "status_pos_language": 3, "status_pos_terminal": 4,
        "status_pos_debug": 5, "status_pos_git": 6, "status_pos_ram": 7,
    });
    if let (Some(dst), Some(src)) = (v.as_object_mut(), bar.as_object()) {
        dst.extend(src.iter().map(|(k, x)| (k.clone(), x.clone())));
    }
    v
}

pub(super) fn choice_label(s: &str) -> String {
    match s {
        "fixed" => "Fixed".into(),
        "system" => "System".into(),
        "auto" => "Automatic".into(),
        "notify" => "Notify".into(),
        "off" => "Off".into(),
        "project" => "Project".into(),
        "bundled" => "Bundled".into(),
        "long" => "Long".into(),
        "short" => "Short".into(),
        "derived" => "Derived".into(),
        "theme" => "From theme".into(),
        "ask" => "Ask".into(),
        "merge" => "Merge".into(),
        "rebase" => "Rebase".into(),
        "ff-only" => "Fast-forward only".into(),
        "subtle" => "Subtle".into(),
        "normal" => "Normal".into(),
        o => o.to_string(),
    }
}

pub(super) fn section_icon(s: &str) -> &'static str {
    match s {
        "General" => "settings",
        "Editor" => "edit",
        "Appearance" => "palette",
        "TypeScript" => "code",
        "Git" => "branch",
        "Terminal" => "terminal",
        "Status bar" => "layout",
        _ => "settings",
    }
}

pub(super) fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub(super) fn num_value(v: f64) -> Value {
    let v = (v * 1e6).round() / 1e6;
    if v.fract() == 0.0 && v.abs() < 1e15 { json!(v as i64) } else { json!(v) }
}

/// Segmented control for up to 3 options, dropdown list above that.
pub(super) fn is_segmented(n: usize) -> bool {
    (1..=3).contains(&n)
}

/// Case-insensitive AND match of every whitespace-separated term of `q` in `hay`.
pub(super) fn matches_all(hay: &str, q: &str) -> bool {
    let hay = hay.to_lowercase();
    q.to_lowercase().split_whitespace().all(|t| hay.contains(t))
}
