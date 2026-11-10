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
    /// Reference/implementation counts above symbols (costs server CPU, so off by default).
    pub code_lens: bool,
    pub inlay_hints: bool,
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
    /// Width in px of the Git panel when the user dragged its edge (0 = automatic).
    pub git_panel_width: i32,
    /// Share of the editor area given to the source side of the Markdown reader (0.2 to 0.8).
    pub md_split: f32,
    /// "derived" (default: one calm flat chrome shared by every theme) or "theme" (sidebar, tabs and
    /// status bar use the theme's own colors wherever they keep the text readable).
    pub theme_chrome: String,
}

/// Bounds of [`Config::md_split`] (the reader's divider can't leave this range).
pub const MD_SPLIT_RANGE: (f32, f32) = (0.2, 0.8);

/// Bottom bar items: (id, label, required). Only the first one is mandatory (the sidebar
/// toggle in the left corner); every other item can be hidden through `status_items` or the
/// settings (`status_<id>` keys).
pub const STATUS_ITEMS: &[(&str, &str, bool)] = &[
    ("sidebar", "Alternar barra lateral", true),
    ("search", "Buscar no projeto", false),
    ("problems", "Problemas", false),
    ("terminal", "Terminal", false),
    ("debug", "Depuração", false),
    ("git", "Git", false),
    ("ram", "Memória", false),
    ("position", "Linha e coluna", false),
    ("indent", "Indentação", false),
    ("language", "Linguagem", false),
];

/// The bottom bar items of the right-hand group in their default order, left to right. They can
/// be reordered with `Config::status_order` (config.json) or the settings modal.
pub const STATUS_RIGHT: &[&str] = &["position", "indent", "language", "terminal", "debug", "git", "ram"];

/// `list` cleaned up into a permutation of [`STATUS_RIGHT`]: unknown ids and repeats are dropped
/// and the missing ones are appended in their default order.
pub fn normalize_status_order(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in list {
        if STATUS_RIGHT.contains(&id.as_str()) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    for id in STATUS_RIGHT {
        if !out.iter().any(|o| o == id) {
            out.push((*id).to_string());
        }
    }
    out
}

/// Is `id` a bottom bar item that cannot be hidden?
pub fn status_required(id: &str) -> bool {
    STATUS_ITEMS.iter().any(|(i, _, req)| *i == id && *req)
}

impl Default for Config {
    fn default() -> Self {
        Self { theme: None, format_on_save: false, auto_import: false, font_size: 14.0, lsp_narrow: true, code_lens: false, inlay_hints: false, appearance: "fixed".into(), theme_light: "github-light-default".into(), theme_dark: "github-dark-default".into(), tsgo_source: "auto".into(), minimap: true, minimap_style: "subtle".into(), word_wrap: false, sticky_scroll: true, indent_guides: true, ui_scale: 0.0, breadcrumbs: true, git_blame: true, ram_display: "long".into(), status_items: BTreeMap::new(), status_order: normalize_status_order(&[]), git_panel_width: 0, md_split: 0.5, theme_chrome: "derived".into() }
    }
}

/// Where grammars and themes live: `$LUMEN_ASSETS`, an installed copy next to the binary or
/// under `~/.local/share`, or (during development) the source tree.
pub fn assets_dir() -> PathBuf {
    let ok = |p: &Path| p.join("language-manifest.json").exists();
    if let Some(p) = std::env::var_os("LUMEN_ASSETS").map(PathBuf::from).filter(|p| ok(p)) {
        return p;
    }
    if let Some(p) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("../share/lumen-text/assets"))).filter(|p| ok(p)) {
        return p;
    }
    if let Some(p) = std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share/lumen-text/assets")).filter(|p| ok(p)) {
        return p;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

/// Desktop dark-mode preference, when it can be determined.
pub fn detect_dark() -> Option<bool> {
    if let Ok(o) = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "color-scheme"]).output() {
        if o.status.success() {
            return Some(String::from_utf8_lossy(&o.stdout).contains("dark"));
        }
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

    /// Move right-hand item `id` to slot `slot` (0 = leftmost; clamped), shifting the others.
    /// Returns false for an unknown id or when nothing changed.
    pub fn move_status_to(&mut self, id: &str, slot: usize) -> bool {
        let Some(from) = self.status_order.iter().position(|s| s == id) else { return false };
        let to = slot.min(self.status_order.len() - 1);
        if from == to {
            return false;
        }
        let item = self.status_order.remove(from);
        self.status_order.insert(to, item);
        true
    }

    /// Move `id` one step left (`dir < 0`) or right. Returns false at the edge.
    #[allow(dead_code)] // API for scripted/keyboard moves; the modal uses `move_status_to`
    pub fn move_status(&mut self, id: &str, dir: i32) -> bool {
        let Some(from) = self.status_order.iter().position(|s| s == id) else { return false };
        let to = if dir < 0 { from.checked_sub(1) } else { Some(from + 1).filter(|t| *t < self.status_order.len()) };
        to.is_some_and(|t| self.move_status_to(id, t))
    }

    /// Slot (0 = leftmost) of a right-hand item.
    pub fn status_slot(&self, id: &str) -> Option<usize> {
        self.status_order.iter().position(|s| s == id)
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
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
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
            lsp_narrow: v["lsp_narrow"].as_bool().unwrap_or(true),
            code_lens: v["code_lens"].as_bool().unwrap_or(false),
            inlay_hints: v["inlay_hints"].as_bool().unwrap_or(false),
            appearance: v["appearance"].as_str().unwrap_or("fixed").to_string(),
            theme_light: v["theme_light"].as_str().unwrap_or("github-light-default").to_string(),
            theme_dark: v["theme_dark"].as_str().unwrap_or("github-dark-default").to_string(),
            tsgo_source: v["tsgo_source"].as_str().unwrap_or("auto").to_string(),
            minimap: v["minimap"].as_bool().unwrap_or(true),
            minimap_style: if v["minimap_style"].as_str() == Some("normal") { "normal".into() } else { "subtle".into() },
            word_wrap: v["word_wrap"].as_bool().unwrap_or(false),
            sticky_scroll: v["sticky_scroll"].as_bool().unwrap_or(true),
            indent_guides: v["indent_guides"].as_bool().unwrap_or(true),
            breadcrumbs: v["breadcrumbs"].as_bool().unwrap_or(true),
            git_blame: v["git_blame"].as_bool().unwrap_or(true),
            ram_display: if v["ram_display"].as_str() == Some("short") { "short".into() } else { "long".into() },
            status_items: v["status_items"]
                .as_object()
                .map(|o| o.iter().filter(|(k, val)| !status_required(k) && STATUS_ITEMS.iter().any(|(i, _, _)| *i == k.as_str()) && val.is_boolean()).map(|(k, val)| (k.clone(), val.as_bool().unwrap_or(true))).collect())
                .unwrap_or_default(),
            status_order: normalize_status_order(&v["status_order"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default()),
            git_panel_width: v["git_panel_width"].as_i64().map_or(0, |n| n.clamp(0, 4000) as i32),
            md_split: v["md_split"].as_f64().map_or(0.5, |f| f as f32).clamp(MD_SPLIT_RANGE.0, MD_SPLIT_RANGE.1),
            theme_chrome: if v["theme_chrome"].as_str() == Some("theme") { "theme".into() } else { "derived".into() },
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
        let v = json!({"theme": self.theme, "format_on_save": self.format_on_save, "auto_import": self.auto_import, "font_size": self.font_size, "lsp_narrow": self.lsp_narrow, "code_lens": self.code_lens, "inlay_hints": self.inlay_hints, "appearance": self.appearance, "theme_light": self.theme_light, "theme_dark": self.theme_dark, "tsgo_source": self.tsgo_source, "minimap": self.minimap, "minimap_style": self.minimap_style, "word_wrap": self.word_wrap, "sticky_scroll": self.sticky_scroll, "indent_guides": self.indent_guides, "ui_scale": self.ui_scale, "breadcrumbs": self.breadcrumbs, "git_blame": self.git_blame, "ram_display": self.ram_display, "status_items": self.status_items, "status_order": self.status_order, "git_panel_width": self.git_panel_width, "md_split": self.md_split, "theme_chrome": self.theme_chrome});
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
        let c = Config { minimap: false, minimap_style: "normal".into(), md_split: 0.65, word_wrap: true, sticky_scroll: false, indent_guides: false, theme: Some("dracula".into()), format_on_save: true, auto_import: true, font_size: 17.0, lsp_narrow: false, code_lens: true, inlay_hints: true, appearance: "system".into(), theme_light: "a".into(), theme_dark: "b".into(), tsgo_source: "project".into(), ui_scale: 1.5, breadcrumbs: false, git_blame: false, ram_display: "short".into(), status_items: [("terminal".to_string(), false), ("ram".to_string(), false)].into_iter().collect(), status_order: ["git", "ram", "position", "indent", "language", "terminal", "debug"].iter().map(|s| s.to_string()).collect(), git_panel_width: 512, theme_chrome: "theme".into() };
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
        assert_eq!(normalize_status_order(&v(&["git", "bogus", "git", "sidebar", "ram"])), v(&["git", "ram", "position", "indent", "language", "terminal", "debug"]));
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
}
