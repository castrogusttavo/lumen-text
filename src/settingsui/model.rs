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
    let mut defs = vec![
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
        // Info bar: readouts at the right of the breadcrumb row and the memory read-out in the top bar.
        d("status_problems", "Info bar", "Problems", "Error and warning counts (breadcrumb row).", Bool),
        d("status_position", "Info bar", "Line and column", "Cursor position (Ln, Col) and number of cursors.", Bool),
        d("status_indent", "Info bar", "Indentation", "The file's spaces or tabs.", Bool),
        d("status_language", "Info bar", "Language", "Language of the active file.", Bool),
        d(
            "status_encoding",
            "Info bar",
            "Encoding",
            "Encoding of the active file (UTF-8, UTF-16, Windows-1252…); click to reopen or save it in another one.",
            Bool,
        ),
        d("status_ram", "Info bar", "Memory", "Memory read-out at the top right (editor and language servers).", Bool),
        // Order of the items: a stepper per item with its position among the visible ones of its group
        // (1 = leftmost); - moves it left, + moves it right, hopping over hidden items.
        d(
            "status_pos_position",
            "Info bar",
            "Order: Line and column",
            "Position among the readouts at the right of the breadcrumb row (1 = leftmost); − and + move it, skipping hidden ones.",
            Number { min: 1.0, max: 4.0, step: 1.0 },
        ),
        d(
            "status_pos_indent",
            "Info bar",
            "Order: Indentation",
            "Position among the readouts at the right of the breadcrumb row (1 = leftmost); − and + move it, skipping hidden ones.",
            Number { min: 1.0, max: 4.0, step: 1.0 },
        ),
        d(
            "status_pos_encoding",
            "Info bar",
            "Order: Encoding",
            "Position among the readouts at the right of the breadcrumb row (1 = leftmost); − and + move it, skipping hidden ones.",
            Number { min: 1.0, max: 4.0, step: 1.0 },
        ),
        d(
            "status_pos_language",
            "Info bar",
            "Order: Language",
            "Position among the readouts at the right of the breadcrumb row (1 = leftmost); − and + move it, skipping hidden ones.",
            Number { min: 1.0, max: 4.0, step: 1.0 },
        ),
        d(
            "ram_display",
            "Info bar",
            "Memory format",
            "Long shows the numbers (\"RAM 13 MB + tsgo 65 MB\"); Short shows just a chip icon, with the values in the tooltip.",
            Choice(&["long", "short"]),
        ),
    ];
    defs.extend(crate::settings_ext::defs());
    defs.extend(crate::app_view::defs());
    defs
}

/// Defaults matching `Config::default()` plus the pending keys. Used for the per-row reset icon
/// and as the display value of keys missing from the values map.
pub fn default_values() -> Value {
    let mut v = json!({
        "format_on_save": false, "updates": "auto", "auto_import": false, "lsp_narrow": false,
        "code_lens": false, "code_lens_refs": true, "code_lens_impls": true, "inlay_hints": false, "inlay_hints_params": true, "inlay_hints_types": true, "appearance": "fixed", "theme_chrome": "derived", "theme_light": "github-light-default",
        "theme_dark": "github-dark-default", "tsgo_source": "auto",
        "minimap": true, "minimap_style": "subtle", "word_wrap": false, "sticky_scroll": true, "indent_guides": true, "ui_scale": 0, "scroll_speed": 1,
        "breadcrumbs": true, "git_blame": true, "ram_display": "long", "tab_width": "4", "git_pull_strategy": "ask",
        "explorer_hide_gitignored": true,
    });
    // Split in two: one `json!` this long hits the macro recursion limit.
    let bar = json!({
        "status_problems": true, "status_position": true, "status_indent": true, "status_language": true, "status_encoding": true,
        "status_ram": true,
        "status_pos_position": 1, "status_pos_indent": 2, "status_pos_encoding": 3, "status_pos_language": 4,
    });
    if let (Some(dst), Some(src)) = (v.as_object_mut(), bar.as_object()) {
        dst.extend(src.iter().map(|(k, x)| (k.clone(), x.clone())));
    }
    if let (Some(dst), Some(src)) = (v.as_object_mut(), crate::settings_ext::defaults().as_object()) {
        dst.extend(src.iter().map(|(k, x)| (k.clone(), x.clone())));
    }
    if let (Some(dst), Some(src)) = (v.as_object_mut(), crate::settings_ext::defaults().as_object()) {
        dst.extend(src.iter().map(|(k, x)| (k.clone(), x.clone())));
    }
    if let (Some(dst), Some(src)) = (v.as_object_mut(), crate::app_view::defaults().as_object()) {
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
        "none" => "None".into(),
        "boundary" => "Boundary".into(),
        "trailing" => "Trailing".into(),
        "all" => "All".into(),
        "normal" => "Normal".into(),
        "current_line" => "Current line".into(),
        "name" | "type" | "modified" | "mixed" => crate::app_view::sort_label(s).into(),
        o => o.to_string(),
    }
}

pub(super) fn section_icon(s: &str) -> &'static str {
    match s {
        "General" => "settings",
        "Editor" => "edit",
        "Display" | "Editor display" => "eye",
        "Fonts" => "font",
        "Appearance" => "palette",
        "Files" => "folder",
        "Languages" | "TypeScript" | "Language servers" => "code",
        "Git" => "branch",
        "Build" => "tool_hammer",
        "Terminal" => "terminal",
        "Info bar" => "layout",
        "AI" => "sparkle",
        _ => "settings",
    }
}

/// One-line description shown under the page title.
pub(super) fn section_desc(s: &str) -> &'static str {
    match s {
        "General" => "Saving, updates and the memory budget.",
        "Editor" => "How text is edited: indentation, whitespace, keymap and writing aids.",
        "Display" => "What the editor window shows: gutter, minimap, guides, tabs and diagnostics.",
        "Appearance" => "Theme, light/dark mode, interface scale and scrolling.",
        "Fonts" => "Typeface, size and spacing for the code editor and for the interface.",
        "Files" => "The file explorer and what is kept between sessions.",
        "Languages" => "Language servers, TypeScript intelligence and code lenses.",
        "Git" => "Pulling, blame and diffs.",
        "Build" => "Commands behind Build and Run, and the debugger.",
        "Info bar" => "Which readouts the breadcrumb row and the top bar show, and in what order.",
        "AI" => "Notifications and the prompts used by the AI actions.",
        _ => "",
    }
}

/// Position of a page in the left navigation (unknown pages go last, in definition order).
pub(super) fn section_rank(s: &str) -> usize {
    const ORDER: &[&str] = &[
        "General",
        "Editor",
        "Display",
        "Appearance",
        "Fonts",
        "Files",
        "Languages",
        "Git",
        "Build",
        "Info bar",
        "AI",
    ];
    ORDER.iter().position(|o| *o == s).unwrap_or(ORDER.len())
}

/// The page (left-nav entry) a setting is listed on. The row definitions live in several modules
/// with older, finer-grained section names; this regroups them into a few clear pages.
pub(super) fn page_of(key: &str, section: &'static str) -> &'static str {
    match key {
        "mem_budget_mb" => return "General",
        "word_wrap"
        | "tab_width"
        | "trim_trailing_whitespace"
        | "insert_final_newline"
        | "vim_mode"
        | "keymap_preset"
        | "spell_check"
        | "inline_completion"
        | "accessibility"
        | "bidi_paragraph" => {
            return "Editor";
        }
        "minimap" | "minimap_style" | "sticky_scroll" | "indent_guides" | "breadcrumbs" => return "Display",
        "explorer_hide_gitignored" | "explorer_sort" | "explorer_folders_first" | "hot_exit" | "persistent_undo" => {
            return "Files";
        }
        "enable_lsp" => return "Languages",
        _ => {}
    }
    match section {
        "Editor display" => "Display",
        "TypeScript" | "Language servers" => "Languages",
        o => o,
    }
}

/// Sub-heading of a row inside its page, with the order of the headings (rows keep their
/// definition order within one heading). Empty = no heading.
pub(super) fn row_group(key: &str) -> (u8, &'static str) {
    if key.starts_with("status_pos_") {
        return (1, "Order");
    }
    if key.starts_with("status_") || key == "ram_display" {
        return (0, "Visible items");
    }
    if key.starts_with("buffer_") {
        return (0, "Code editor");
    }
    if key.starts_with("ui_font") || key == "ui_line_height" {
        return (1, "Interface");
    }
    if key.starts_with("language_servers.") {
        return (0, "Language servers");
    }
    if key.starts_with("ai_prompt_") {
        return (1, "Prompts");
    }
    if key.starts_with("debug_") {
        return (1, "Debugger");
    }
    match key {
        "format_on_save" => (0, "Saving"),
        "updates" => (1, "Updates"),
        "mem_budget_mb" => (2, "Performance"),
        "tab_width" | "word_wrap" => (0, "Text"),
        "trim_trailing_whitespace" | "insert_final_newline" => (1, "Whitespace"),
        "spell_check" | "inline_completion" => (2, "Writing aids"),
        "vim_mode" | "keymap_preset" => (3, "Keys"),
        "accessibility" | "bidi_paragraph" => (4, "Accessibility"),
        "show_line_numbers" | "show_bookmarks" | "show_breakpoints" => (0, "Gutter"),
        "minimap" | "minimap_style" | "breadcrumbs" | "sticky_scroll" | "scrollbar" => (1, "Navigation"),
        "indent_guides" | "rulers" | "render_whitespace" => (2, "Guides"),
        "inline_diagnostics" | "problems_scope" => (3, "Diagnostics"),
        "show_tabs" | "show_status_bar" | "zen_max_width" => (4, "Window"),
        "markdown_remote_images" => (5, "Markdown"),
        "appearance" | "theme" | "theme_light" | "theme_dark" | "theme_chrome" => (0, "Theme"),
        "ui_scale" | "scroll_speed" => (1, "Interface"),
        "explorer_hide_gitignored" | "explorer_sort" | "explorer_folders_first" => (0, "Explorer"),
        "hot_exit" | "persistent_undo" => (1, "Recovery"),
        "enable_lsp" => (0, "Language servers"),
        "auto_import" | "lsp_narrow" | "code_lens" | "code_lens_refs" | "code_lens_impls" | "inlay_hints"
        | "inlay_hints_params" | "inlay_hints_types" | "tsgo_source" => (1, "TypeScript"),
        "git_pull_strategy" => (0, "Sync"),
        "git_blame" | "diff_ignore_whitespace" => (1, "Display"),
        "build_command" | "run_command" => (0, "Build and run"),
        "ai_notifications" => (0, "Notifications"),
        _ => (0, ""),
    }
}

/// Equal values; numbers compare as numbers (14 and 14.0 are the same setting).
pub(super) fn same_value(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if a.is_number() && b.is_number() => (x - y).abs() < 1e-6,
        _ => a == b,
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
