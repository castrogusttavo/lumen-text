//! User settings, stored as JSON in `$XDG_CONFIG_HOME/lumen-text/config.json`.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: Option<String>,
    pub format_on_save: bool,
    pub auto_import: bool,
    pub font_size: f32,
    pub lsp_narrow: bool,
    /// Start big projects in lean mode and fall back to it when the full-project server gets too large.
    pub lsp_auto_lean: bool,
    /// Reference/implementation counts above symbols (costs server CPU, so off by default).
    pub code_lens: bool,
    /// Which code lens items to show when `code_lens` is on.
    pub code_lens_refs: bool,
    pub code_lens_impls: bool,
    pub inlay_hints: bool,
    /// Inlay hints: parameter names / inferred types (both on by default; need `inlay_hints`).
    pub inlay_hints_params: bool,
    pub inlay_hints_types: bool,
    /// "fixed" keeps `theme`; "system" follows the desktop light/dark preference.
    pub appearance: String,
    pub theme_light: String,
    pub theme_dark: String,
    /// Which tsgo to run: "auto" (project's own if installed, else bundled), "project" or "bundled".
    pub tsgo_source: String,
    pub minimap: bool,
    /// "subtle" (default: narrow and faint) or "normal"; see `minimap::Style`.
    pub minimap_style: String,
    pub word_wrap: bool,
    pub sticky_scroll: bool,
    pub indent_guides: bool,
    /// UI scale override for hi-DPI (0 = automatic: the window's scale factor).
    pub ui_scale: f32,
    /// Mouse-wheel speed multiplier (1 = 3 rows per notch; touchpads are never scaled).
    pub scroll_speed: f32,
    /// Breadcrumb line above the editor (file path > symbols).
    pub breadcrumbs: bool,
    /// Inline git blame text after the cursor line.
    pub git_blame: bool,
    /// Memory readout in the bottom bar: "long" (numbers) or "short" (a small chip icon).
    pub ram_display: String,
    /// Bottom bar items the user switched on/off (id -> visible); missing = visible.
    /// See [`STATUS_ITEMS`]; the required items can never be hidden.
    pub status_items: BTreeMap<String, bool>,
    /// Order of the right-hand bottom bar items, left to right (see [`STATUS_RIGHT`]); always a
    /// permutation of it (see [`normalize_status_order`]).
    pub status_order: Vec<String>,
    /// Order of the left-hand bottom bar items, left to right (see [`STATUS_LEFT`]); always a
    /// permutation of it (see [`normalize_left_order`]).
    pub status_left_order: Vec<String>,
    /// How `Pull` integrates remote commits: "merge", "rebase", "ff-only", or "" = not chosen yet
    /// (git's own `pull.rebase` / `pull.ff` if set, else fast-forward only; a diverged pull asks).
    pub git_pull_strategy: String,
    /// Columns a tab character spans in the editor (2, 4 or 8).
    pub tab_width: usize,
    /// Width in px of the Git panel when the user dragged its edge (0 = automatic).
    pub git_panel_width: i32,
    /// Share of the editor area given to the source side of the Markdown reader (0.2 to 0.8).
    pub md_split: f32,
    /// "derived" (default: one calm flat chrome shared by every theme) or "theme" (sidebar, tabs and
    /// status bar use the theme's own colors wherever they keep the text readable).
    pub theme_chrome: String,
    /// Updates: "auto" (check, download and install in the background), "notify" (only announce) or "off".
    pub updates: String,
    /// Tests panel: a command of your own (e.g. `make test`) and the JUnit XML file it writes
    /// (relative to the project) so its results show up as a tree. Empty = auto-detect the runner.
    pub test_command: String,
    pub test_report: String,
    /// Re-run the last tests whenever a file is saved.
    pub test_watch: bool,
}

/// `updates` setting values; anything else falls back to "auto".
pub fn normalize_updates(v: Option<&str>) -> String {
    match v {
        Some(m @ ("auto" | "notify" | "off")) => m.to_string(),
        _ => "auto".to_string(),
    }
}

/// Bounds of [`Config::md_split`] (the reader's divider can't leave this range).
pub const MD_SPLIT_RANGE: (f32, f32) = (0.2, 0.8);

/// Bottom bar items: (id, label, required). Only the first one is mandatory (the sidebar
/// toggle in the left corner); every other item can be hidden through `status_items` or the
/// settings (`status_<id>` keys).
pub const STATUS_ITEMS: &[(&str, &str, bool)] = &[
    ("sidebar", "Toggle sidebar", true),
    ("search", "Search in project", false),
    ("problems", "Problems", false),
    ("terminal", "Terminal", false),
    ("debug", "Debug", false),
    ("git", "Git", false),
    ("ram", "Memory", false),
    ("position", "Line and column", false),
    ("indent", "Indentation", false),
    ("language", "Language", false),
];

/// The bottom bar items of the right-hand group in their default order, left to right. They can
/// be reordered with `Config::status_order` (config.json) or the settings modal.
pub const STATUS_RIGHT: &[&str] = &["position", "indent", "language", "terminal", "debug", "git", "ram"];

/// The left-hand group in its default order (sidebar toggle, project search, problems).
pub const STATUS_LEFT: &[&str] = &["sidebar", "search", "problems"];

/// Pull strategies a user can remember in `git_pull_strategy`.
pub const PULL_STRATEGIES: &[&str] = &["merge", "rebase", "ff-only"];

/// Tab widths offered by the bottom bar picker and the settings.
pub const TAB_WIDTHS: &[usize] = &[2, 4, 8];

fn normalize_order(list: &[String], all: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in list {
        if all.contains(&id.as_str()) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    for id in all {
        if !out.iter().any(|o| o == id) {
            out.push((*id).to_string());
        }
    }
    out
}

/// `list` cleaned up into a permutation of [`STATUS_RIGHT`]: unknown ids and repeats are dropped
/// and the missing ones are appended in their default order.
pub fn normalize_status_order(list: &[String]) -> Vec<String> {
    normalize_order(list, STATUS_RIGHT)
}

/// Same for the left-hand group ([`STATUS_LEFT`]).
pub fn normalize_left_order(list: &[String]) -> Vec<String> {
    normalize_order(list, STATUS_LEFT)
}

/// `s` if it names a pull strategy, else "" (not chosen).
pub fn clean_pull_strategy(s: &str) -> String {
    if PULL_STRATEGIES.contains(&s) { s.to_string() } else { String::new() }
}

/// `n` if it is an offered tab width, else the default (4).
pub fn clean_tab_width(n: usize) -> usize {
    if TAB_WIDTHS.contains(&n) { n } else { 4 }
}

/// Is `id` a bottom bar item that cannot be hidden?
pub fn status_required(id: &str) -> bool {
    STATUS_ITEMS.iter().any(|(i, _, req)| *i == id && *req)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: None,
            format_on_save: false,
            auto_import: false,
            font_size: 14.0,
            lsp_narrow: false,
            lsp_auto_lean: true,
            code_lens: false,
            code_lens_refs: true,
            code_lens_impls: true,
            inlay_hints: false,
            inlay_hints_params: true,
            inlay_hints_types: true,
            appearance: "fixed".into(),
            theme_light: "github-light-default".into(),
            theme_dark: "github-dark-default".into(),
            tsgo_source: "auto".into(),
            minimap: true,
            minimap_style: "subtle".into(),
            word_wrap: false,
            sticky_scroll: true,
            indent_guides: true,
            ui_scale: 0.0,
            scroll_speed: 1.0,
            breadcrumbs: true,
            git_blame: true,
            ram_display: "long".into(),
            status_items: BTreeMap::new(),
            status_order: normalize_status_order(&[]),
            status_left_order: normalize_left_order(&[]),
            git_pull_strategy: String::new(),
            tab_width: 4,
            git_panel_width: 0,
            md_split: 0.5,
            theme_chrome: "derived".into(),
            updates: "auto".into(),
            test_command: String::new(),
            test_report: String::new(),
            test_watch: false,
        }
    }
}

/// Where grammars and themes live: `$LUMEN_ASSETS`, an installed copy next to the binary or
/// under `~/.local/share`, or (during development) the source tree.
pub fn assets_dir() -> PathBuf {
    let ok = |p: &Path| p.join("language-manifest.json").exists();
    if let Some(p) = std::env::var_os("LUMEN_ASSETS").map(PathBuf::from).filter(|p| ok(p)) {
        return p;
    }
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("../share/lumen-text/assets")))
        .filter(|p| ok(p))
    {
        return p;
    }
    if let Some(p) =
        std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share/lumen-text/assets")).filter(|p| ok(p))
    {
        return p;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// Desktop dark-mode preference, when it can be determined.
pub fn detect_dark() -> Option<bool> {
    if let Ok(o) =
        std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "color-scheme"]).output()
        && o.status.success()
    {
        return Some(String::from_utf8_lossy(&o.stdout).contains("dark"));
    }
    std::env::var("GTK_THEME").ok().map(|t| t.to_lowercase().contains("dark"))
}

impl Config {
    /// Is the bottom bar item `id` shown? Required items always are; unknown or unset ids too.
    pub fn status_visible(&self, id: &str) -> bool {
        status_required(id) || self.status_items.get(id).copied().unwrap_or(true)
    }

    /// Show/hide a bottom bar item. Required items ignore `false`; showing an item stores no entry.
    pub fn set_status_item(&mut self, id: &str, on: bool) {
        if status_required(id) || !STATUS_ITEMS.iter().any(|(i, _, _)| *i == id) {
            return;
        }
        if on {
            self.status_items.remove(id);
        } else {
            self.status_items.insert(id.to_string(), false);
        }
    }

    fn group_of(&mut self, id: &str) -> Option<&mut Vec<String>> {
        if self.status_order.iter().any(|s| s == id) {
            Some(&mut self.status_order)
        } else if self.status_left_order.iter().any(|s| s == id) {
            Some(&mut self.status_left_order)
        } else {
            None
        }
    }

    /// Move bottom bar item `id` (either group) to slot `slot` (0 = leftmost; clamped), shifting
    /// the others. Returns false for an unknown id or when nothing changed.
    pub fn move_status_to(&mut self, id: &str, slot: usize) -> bool {
        let Some(list) = self.group_of(id) else { return false };
        let Some(from) = list.iter().position(|s| s == id) else { return false };
        let to = slot.min(list.len() - 1);
        if from == to {
            return false;
        }
        let item = list.remove(from);
        list.insert(to, item);
        true
    }

    /// Move `id` one step left (`dir < 0`) or right. Returns false at the edge.
    #[allow(dead_code)] // API for scripted/keyboard moves; the modal uses `step_status`
    pub fn move_status(&mut self, id: &str, dir: i32) -> bool {
        let Some(list) = self.group_of(id) else { return false };
        let Some(from) = list.iter().position(|s| s == id) else { return false };
        let to = if dir < 0 { from.checked_sub(1) } else { Some(from + 1).filter(|t| *t < list.len()) };
        to.is_some_and(|t| self.move_status_to(id, t))
    }

    /// Move `id` past the next *visible* neighbour in direction `dir` (hidden items in between are
    /// hopped over, so the bar always changes). A hidden `id` just moves one slot. False when there
    /// is no visible item that way (nothing would change on screen).
    pub fn step_status(&mut self, id: &str, dir: i32) -> bool {
        let hidden_self = !self.status_visible(id);
        let Some(list) = self.group_of(id).map(|l| l.clone()) else { return false };
        let Some(from) = list.iter().position(|s| s == id) else { return false };
        let step = if dir < 0 { -1i32 } else { 1 };
        let mut t = from as i32 + step;
        if !hidden_self {
            while t >= 0 && (t as usize) < list.len() && !self.status_visible(&list[t as usize]) {
                t += step;
            }
        }
        if t < 0 || t as usize >= list.len() {
            return false;
        }
        self.move_status_to(id, t as usize)
    }

    #[cfg(test)]
    /// Slot (0 = leftmost, hidden items count) of a bottom bar item in its group.
    pub fn status_slot(&self, id: &str) -> Option<usize> {
        self.status_order.iter().position(|s| s == id).or_else(|| self.status_left_order.iter().position(|s| s == id))
    }

    /// Position (1 = leftmost) among the *visible* items of the group for a visible item; the raw
    /// slot + 1 for a hidden one. This is what the settings steppers show.
    pub fn status_rank(&self, id: &str) -> usize {
        let list =
            if self.status_left_order.iter().any(|s| s == id) { &self.status_left_order } else { &self.status_order };
        let Some(slot) = list.iter().position(|s| s == id) else { return 1 };
        if !self.status_visible(id) {
            return slot + 1;
        }
        list[..slot].iter().filter(|s| self.status_visible(s)).count() + 1
    }

    /// Does the sidebar / tab bar / status bar use the theme's own colors?
    pub fn own_chrome(&self) -> bool {
        self.theme_chrome == "theme"
    }

    /// Theme name to use right now.
    pub fn resolve_theme(&self, dark: Option<bool>) -> String {
        if self.appearance == "system" {
            return if dark.unwrap_or(false) { self.theme_dark.clone() } else { self.theme_light.clone() };
        }
        self.theme.clone().unwrap_or_else(|| self.theme_light.clone())
    }
}

pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("lumen-text").join("config.json"))
}

impl Config {
    pub fn load_from(path: &Path) -> Config {
        let Ok(text) = std::fs::read_to_string(path) else { return Config::default() };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return Config::default() };
        Config {
            theme: v["theme"].as_str().map(str::to_string),
            format_on_save: v["format_on_save"].as_bool().unwrap_or(false),
            auto_import: v["auto_import"].as_bool().unwrap_or(false),
            // Lean mode was the default before v2: an old stored `true` is not a deliberate choice.
            lsp_narrow: v["lsp_lean_v"].as_u64() == Some(2) && v["lsp_narrow"].as_bool().unwrap_or(false),
            lsp_auto_lean: v["lsp_auto_lean"].as_bool().unwrap_or(true),
            code_lens: v["code_lens"].as_bool().unwrap_or(false),
            code_lens_refs: v["code_lens_refs"].as_bool().unwrap_or(true),
            code_lens_impls: v["code_lens_impls"].as_bool().unwrap_or(true),
            inlay_hints: v["inlay_hints"].as_bool().unwrap_or(false),
            inlay_hints_params: v["inlay_hints_params"].as_bool().unwrap_or(true),
            inlay_hints_types: v["inlay_hints_types"].as_bool().unwrap_or(true),
            appearance: v["appearance"].as_str().unwrap_or("fixed").to_string(),
            theme_light: v["theme_light"].as_str().unwrap_or("github-light-default").to_string(),
            theme_dark: v["theme_dark"].as_str().unwrap_or("github-dark-default").to_string(),
            tsgo_source: v["tsgo_source"].as_str().unwrap_or("auto").to_string(),
            minimap: v["minimap"].as_bool().unwrap_or(true),
            minimap_style: if v["minimap_style"].as_str() == Some("normal") {
                "normal".into()
            } else {
                "subtle".into()
            },
            word_wrap: v["word_wrap"].as_bool().unwrap_or(false),
            sticky_scroll: v["sticky_scroll"].as_bool().unwrap_or(true),
            indent_guides: v["indent_guides"].as_bool().unwrap_or(true),
            breadcrumbs: v["breadcrumbs"].as_bool().unwrap_or(true),
            git_blame: v["git_blame"].as_bool().unwrap_or(true),
            ram_display: if v["ram_display"].as_str() == Some("short") { "short".into() } else { "long".into() },
            status_items: v["status_items"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(k, val)| {
                            !status_required(k)
                                && STATUS_ITEMS.iter().any(|(i, _, _)| *i == k.as_str())
                                && val.is_boolean()
                        })
                        .map(|(k, val)| (k.clone(), val.as_bool().unwrap_or(true)))
                        .collect()
                })
                .unwrap_or_default(),
            status_order: normalize_status_order(
                &v["status_order"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>())
                    .unwrap_or_default(),
            ),
            status_left_order: normalize_left_order(
                &v["status_left_order"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>())
                    .unwrap_or_default(),
            ),
            git_pull_strategy: clean_pull_strategy(v["git_pull_strategy"].as_str().unwrap_or("")),
            tab_width: clean_tab_width(v["tab_width"].as_u64().unwrap_or(4) as usize),
            git_panel_width: v["git_panel_width"].as_i64().map_or(0, |n| n.clamp(0, 4000) as i32),
            md_split: v["md_split"].as_f64().map_or(0.5, |f| f as f32).clamp(MD_SPLIT_RANGE.0, MD_SPLIT_RANGE.1),
            theme_chrome: if v["theme_chrome"].as_str() == Some("theme") { "theme".into() } else { "derived".into() },
            updates: normalize_updates(v["updates"].as_str()),
            test_command: v["test_command"].as_str().unwrap_or("").trim().to_string(),
            test_report: v["test_report"].as_str().unwrap_or("").trim().to_string(),
            test_watch: v["test_watch"].as_bool().unwrap_or(false),
            scroll_speed: v["scroll_speed"].as_f64().map_or(1.0, |f| f as f32).clamp(0.1, 5.0),
            ui_scale: v["ui_scale"].as_f64().map_or(0.0, |f| f as f32).clamp(0.0, 4.0),
            font_size: v["font_size"].as_f64().map_or(14.0, |f| f as f32).clamp(8.0, 40.0),
        }
    }

    pub fn load() -> Config {
        default_path().map(|p| Config::load_from(&p)).unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let v = json!({"theme": self.theme, "format_on_save": self.format_on_save, "auto_import": self.auto_import, "font_size": self.font_size, "lsp_narrow": self.lsp_narrow, "lsp_lean_v": 2, "lsp_auto_lean": self.lsp_auto_lean, "code_lens": self.code_lens, "code_lens_refs": self.code_lens_refs, "code_lens_impls": self.code_lens_impls, "inlay_hints": self.inlay_hints, "inlay_hints_params": self.inlay_hints_params, "inlay_hints_types": self.inlay_hints_types, "appearance": self.appearance, "theme_light": self.theme_light, "theme_dark": self.theme_dark, "tsgo_source": self.tsgo_source, "minimap": self.minimap, "minimap_style": self.minimap_style, "word_wrap": self.word_wrap, "sticky_scroll": self.sticky_scroll, "indent_guides": self.indent_guides, "ui_scale": self.ui_scale, "scroll_speed": self.scroll_speed, "breadcrumbs": self.breadcrumbs, "git_blame": self.git_blame, "ram_display": self.ram_display, "status_items": self.status_items, "status_order": self.status_order, "status_left_order": self.status_left_order, "git_pull_strategy": self.git_pull_strategy, "tab_width": self.tab_width, "git_panel_width": self.git_panel_width, "md_split": self.md_split, "theme_chrome": self.theme_chrome, "updates": self.updates, "test_command": self.test_command, "test_report": self.test_report, "test_watch": self.test_watch});
        std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    pub fn save(&self) {
        if let Some(p) = default_path() {
            let _ = self.save_to(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_resolution_follows_appearance() {
        let mut c = Config::default();
        assert_eq!(c.resolve_theme(Some(true)), "github-light-default", "fixed + no explicit theme -> light");
        c.theme = Some("dracula".into());
        assert_eq!(c.resolve_theme(None), "dracula");
        c.appearance = "system".into();
        assert_eq!(c.resolve_theme(Some(true)), "github-dark-default");
        assert_eq!(c.resolve_theme(Some(false)), "github-light-default");
        assert_eq!(c.resolve_theme(None), "github-light-default");
    }

    #[test]
    fn roundtrip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("lumen-cfg-{}", std::process::id()));
        let p = dir.join("sub/config.json");
        assert_eq!(Config::load_from(&p), Config::default(), "missing file -> defaults");
        let c = Config {
            minimap: false,
            minimap_style: "normal".into(),
            md_split: 0.65,
            word_wrap: true,
            sticky_scroll: false,
            indent_guides: false,
            theme: Some("dracula".into()),
            format_on_save: true,
            auto_import: true,
            font_size: 17.0,
            lsp_narrow: false,
            lsp_auto_lean: true,
            code_lens: true,
            code_lens_refs: false,
            code_lens_impls: true,
            inlay_hints: true,
            inlay_hints_params: false,
            inlay_hints_types: false,
            appearance: "system".into(),
            theme_light: "a".into(),
            theme_dark: "b".into(),
            tsgo_source: "project".into(),
            ui_scale: 1.5,
            scroll_speed: 2.0,
            breadcrumbs: false,
            git_blame: false,
            ram_display: "short".into(),
            status_items: [("terminal".to_string(), false), ("ram".to_string(), false)].into_iter().collect(),
            status_order: ["git", "ram", "position", "indent", "language", "terminal", "debug"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            status_left_order: ["problems", "sidebar", "search"].iter().map(|s| s.to_string()).collect(),
            git_pull_strategy: "rebase".into(),
            tab_width: 8,
            git_panel_width: 512,
            theme_chrome: "theme".into(),
            updates: "notify".into(),
            test_command: "make test".into(),
            test_report: "out/junit.xml".into(),
            test_watch: true,
        };
        c.save_to(&p).unwrap();
        assert_eq!(Config::load_from(&p), c);
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(Config::load_from(&p), Config::default(), "corrupt file -> defaults");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn minimap_style_and_md_split_defaults_and_bounds() {
        let c = Config::default();
        assert!(c.minimap, "the minimap stays on by default");
        assert_eq!((c.minimap_style.as_str(), c.md_split), ("subtle", 0.5));
        let dir = std::env::temp_dir().join(format!("lumen-cfg-mm-{}", std::process::id()));
        let p = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, r#"{"minimap_style": "loud", "md_split": 3.0}"#).unwrap();
        let c = Config::load_from(&p);
        assert_eq!(c.minimap_style, "subtle", "unknown style falls back");
        assert_eq!(c.md_split, MD_SPLIT_RANGE.1, "out-of-range ratio is clamped");
        std::fs::write(&p, r#"{"minimap_style": "normal", "md_split": 0.01}"#).unwrap();
        let c = Config::load_from(&p);
        assert_eq!((c.minimap_style.as_str(), c.md_split), ("normal", MD_SPLIT_RANGE.0));
        std::fs::write(&p, r#"{"md_split": "x"}"#).unwrap();
        assert_eq!(Config::load_from(&p).md_split, 0.5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn updates_setting_defaults_to_auto_and_rejects_unknown_values() {
        assert_eq!(Config::default().updates, "auto");
        let dir = std::env::temp_dir().join(format!("lumen-cfg-upd-{}", std::process::id()));
        let p = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        for (raw, want) in [
            (r#"{"updates": "off"}"#, "off"),
            (r#"{"updates": "notify"}"#, "notify"),
            (r#"{"updates": "sometimes"}"#, "auto"),
            (r#"{"updates": 3}"#, "auto"),
            ("{}", "auto"),
        ] {
            std::fs::write(&p, raw).unwrap();
            assert_eq!(Config::load_from(&p).updates, want, "{raw}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn status_items_defaults_and_required_rule() {
        let mut c = Config::default();
        assert!(c.breadcrumbs && c.git_blame && c.ram_display == "long" && c.status_items.is_empty());
        for (id, _, _) in STATUS_ITEMS {
            assert!(c.status_visible(id), "{id} visible by default");
        }
        assert!(status_required("sidebar") && !status_required("git"));
        assert_eq!(STATUS_ITEMS.iter().filter(|(_, _, r)| *r).count(), 1, "only the sidebar toggle is mandatory");
        c.set_status_item("sidebar", false);
        assert!(c.status_visible("sidebar") && c.status_items.is_empty(), "the sidebar button cannot be hidden");
        c.set_status_item("git", false);
        c.set_status_item("bogus", false);
        assert!(!c.status_visible("git") && c.status_visible("terminal") && c.status_visible("bogus"));
        assert_eq!(c.status_items.len(), 1);
        c.set_status_item("git", true);
        assert!(c.status_items.is_empty(), "showing again drops the entry");
    }

    #[test]
    fn status_items_load_ignores_required_unknown_and_bad_values() {
        let dir = std::env::temp_dir().join(format!("lumen-cfg-st-{}", std::process::id()));
        let p = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, r#"{"status_items": {"sidebar": false, "git": false, "ram": "no", "nope": false, "debug": true}, "ram_display": "weird", "breadcrumbs": false, "git_blame": false}"#).unwrap();
        let c = Config::load_from(&p);
        assert!(c.status_visible("sidebar"), "mandatory item stays visible even if the file says false");
        assert!(!c.status_visible("git"));
        assert!(c.status_visible("ram") && c.status_visible("debug"));
        assert_eq!(c.status_items.len(), 2, "git=false and debug=true are kept: {:?}", c.status_items);
        assert_eq!(c.ram_display, "long", "unknown value falls back");
        assert!(!c.breadcrumbs && !c.git_blame);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn status_order_is_always_a_permutation_of_the_right_items() {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(normalize_status_order(&[]), v(STATUS_RIGHT));
        assert_eq!(
            normalize_status_order(&v(&["git", "bogus", "git", "sidebar", "ram"])),
            v(&["git", "ram", "position", "indent", "language", "terminal", "debug"])
        );
        assert_eq!(Config::default().status_order, v(STATUS_RIGHT));
    }

    #[test]
    fn moving_status_items() {
        let mut c = Config::default();
        assert!(!c.move_status("position", -1), "already leftmost");
        assert!(c.move_status("position", 1));
        assert_eq!(c.status_order[..3], ["indent", "position", "language"]);
        assert!(c.move_status_to("ram", 0));
        assert_eq!(c.status_slot("ram"), Some(0));
        assert!(c.move_status_to("ram", 99), "clamped to the last slot");
        assert_eq!(c.status_slot("ram"), Some(STATUS_RIGHT.len() - 1));
        assert!(!c.move_status("ram", 1), "already rightmost");
        assert!(!c.move_status_to("nope", 0));
        assert_eq!(normalize_status_order(&c.status_order), c.status_order);
    }

    #[test]
    fn status_order_loads_cleaned_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("lumen-cfg-order-{}", std::process::id()));
        let p = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, r#"{"status_order": ["git", "nope", "language", "git"]}"#).unwrap();
        let mut c = Config::load_from(&p);
        assert_eq!(c.status_order[..3], ["git", "language", "position"]);
        assert_eq!(c.status_order.len(), STATUS_RIGHT.len());
        c.move_status("git", 1);
        c.save_to(&p).unwrap();
        assert_eq!(Config::load_from(&p).status_order, c.status_order);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn left_group_is_reorderable_too() {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut c = Config::default();
        assert_eq!(c.status_left_order, v(STATUS_LEFT));
        assert_eq!(
            normalize_left_order(&v(&["problems", "x", "problems", "ram"])),
            v(&["problems", "sidebar", "search"])
        );
        assert!(c.move_status("search", -1));
        assert_eq!(c.status_left_order, v(&["search", "sidebar", "problems"]));
        assert!(c.move_status_to("sidebar", 2));
        assert_eq!(c.status_left_order, v(&["search", "problems", "sidebar"]));
        assert_eq!(c.status_slot("sidebar"), Some(2));
        assert_eq!(c.status_order, v(STATUS_RIGHT), "the right group is untouched");
    }

    #[test]
    fn stepping_skips_hidden_items_so_the_bar_always_changes() {
        let mut c = Config::default();
        // position, indent, language, terminal, debug, git, ram: hide indent and language.
        c.set_status_item("indent", false);
        c.set_status_item("language", false);
        assert_eq!(c.status_rank("position"), 1);
        assert_eq!(c.status_rank("terminal"), 2, "hidden items do not count");
        assert!(c.step_status("position", 1), "hops over the two hidden items and the terminal button");
        assert_eq!(c.status_order[..5], ["indent", "language", "terminal", "position", "debug"]);
        assert_eq!(c.status_rank("position"), 2);
        assert!(c.step_status("position", -1));
        assert_eq!(c.status_rank("position"), 1);
        // Nothing visible to the left of the first visible item: no change (the bar would not move).
        c.move_status_to("position", 0);
        c.move_status_to("terminal", 0);
        assert!(!c.step_status("terminal", -1));
        assert_eq!(c.status_rank("terminal"), 1);
        // Last visible item with only hidden ones after it.
        let mut d = Config::default();
        d.set_status_item("ram", false);
        d.move_status_to("git", 6);
        assert_eq!(d.status_order.last().map(String::as_str), Some("git"));
        d.move_status_to("ram", 6);
        assert!(!d.step_status("git", 1), "only the hidden RAM item is to its right");
        // A hidden item itself moves one slot at a time.
        let slot = d.status_slot("ram").unwrap();
        assert!(d.step_status("ram", -1));
        assert_eq!(d.status_slot("ram"), Some(slot - 1));
    }

    #[test]
    fn pull_strategy_and_tab_width_are_cleaned_on_load() {
        assert_eq!(clean_pull_strategy("rebase"), "rebase");
        assert_eq!(clean_pull_strategy("squash"), "");
        assert_eq!((clean_tab_width(2), clean_tab_width(3), clean_tab_width(8)), (2, 4, 8));
        let dir = std::env::temp_dir().join(format!("lumen-cfg-pull-{}", std::process::id()));
        let p = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, r#"{"git_pull_strategy": "merge", "tab_width": 2, "status_left_order": ["problems"]}"#)
            .unwrap();
        let c = Config::load_from(&p);
        assert_eq!((c.git_pull_strategy.as_str(), c.tab_width), ("merge", 2));
        assert_eq!(c.status_left_order, ["problems", "sidebar", "search"]);
        std::fs::write(&p, r#"{"git_pull_strategy": "nope", "tab_width": 5}"#).unwrap();
        let c = Config::load_from(&p);
        assert_eq!((c.git_pull_strategy.as_str(), c.tab_width), ("", 4));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lean_mode_is_opt_in_and_an_old_stored_default_is_ignored() {
        assert!(!Config::default().lsp_narrow);
        let dir = std::env::temp_dir().join(format!("lumen-cfg-lean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("config.json");
        std::fs::write(&f, r#"{"lsp_narrow": true}"#).unwrap();
        assert!(!Config::load_from(&f).lsp_narrow, "pre-v2 files stored lean mode as the default");
        std::fs::write(&f, r#"{"lsp_narrow": true, "lsp_lean_v": 2}"#).unwrap();
        assert!(Config::load_from(&f).lsp_narrow, "a deliberate opt-in survives");
        std::fs::remove_dir_all(&dir).ok();
    }
}
