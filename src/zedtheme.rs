//! Zed theme -> VS Code theme conversion, custom-theme template and user theme directories.
//!
//! `Theme::load` (src/theme.rs) understands VS Code style JSON (`colors` + `tokenColors`).
//! This module converts Zed theme families into that format and manages the
//! `~/.config/lumen-text/themes` directory.
//!
//! # Color mapping (Zed `style` key -> VS Code `colors` key)
//! Several Zed keys may feed one VS Code key; the first one that is present and not fully
//! transparent wins (see `COLOR_MAP`, which lists candidates in priority order).
//!
//! | VS Code key                         | Zed keys (priority order)                              |
//! |-------------------------------------|--------------------------------------------------------|
//! | editor.background                   | editor.background, background                          |
//! | editor.foreground                   | editor.foreground, text                                |
//! | editor.lineHighlightBackground      | editor.active_line.background                          |
//! | editorLineNumber.foreground         | editor.line_number                                     |
//! | editorLineNumber.activeForeground   | editor.active_line_number                              |
//! | editor.selectionBackground          | players[0].selection                                   |
//! | editorCursor.foreground             | players[0].cursor                                      |
//! | sideBar.background                  | panel.background, surface.background                   |
//! | sideBar.foreground                  | text                                                   |
//! | editorGroupHeader.tabsBackground    | tab_bar.background                                     |
//! | tab.activeBackground                | tab.active_background                                  |
//! | tab.inactiveBackground              | tab.inactive_background                                |
//! | tab.activeForeground                | text                                                   |
//! | tab.inactiveForeground              | text.muted, text.disabled                              |
//! | statusBar.background                | status_bar.background                                  |
//! | statusBar.foreground                | text                                                   |
//! | panel.border                        | border                                                 |
//! | editorWidget.background             | elevated_surface.background                            |
//! | list.hoverBackground                | element.hover                                          |
//! | list.activeSelectionBackground      | element.selected                                       |
//! | input.background                    | element.background, surface.background                 |
//! | focusBorder                         | border.focused                                         |
//! | editorError.foreground              | error, error.background                                |
//! | editorWarning.foreground            | warning, warning.background                            |
//! | editorInfo.foreground               | info, info.background                                  |
//! | editorGutter.added/modified/deleted | created / modified / deleted                           |
//! | editor.findMatchHighlightBackground | search.match_background                                |
//! | editor.findMatchBackground          | search.match_background                                |
//!
//! Colors keep their `#rrggbb` / `#rrggbbaa` form. Fully transparent colors (alpha 00) are
//! skipped so that `Theme::load` derives a sensible fallback instead of a see-through UI.
//! Unknown keys are ignored.
//!
//! # Syntax mapping
//! See `SYNTAX_MAP`: each Zed `syntax` key expands to a list of TextMate scopes. Generic
//! entries come first so that more specific ones (e.g. `function.method`) can override.
#![allow(dead_code)]

use serde_json::{Map, Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Tolerant JSON pre-pass
// ---------------------------------------------------------------------------

/// Remove `//` and `/* */` comments (outside strings) and trailing commas before `}` / `]`.
fn clean_json(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i = (i + 2).min(chars.len());
                out.push(' ');
            }
            ',' => {
                // Look ahead past whitespace and comments for a closing bracket.
                let mut j = i + 1;
                loop {
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'/') {
                        while j < chars.len() && chars[j] != '\n' {
                            j += 1;
                        }
                    } else if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'*') {
                        j += 2;
                        while j < chars.len() && !(chars[j] == '*' && chars.get(j + 1) == Some(&'/')) {
                            j += 1;
                        }
                        j = (j + 2).min(chars.len());
                    } else {
                        break;
                    }
                }
                if !matches!(chars.get(j), Some('}') | Some(']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Mapping tables
// ---------------------------------------------------------------------------

/// (VS Code key, Zed style keys in priority order).
const COLOR_MAP: &[(&str, &[&str])] = &[
    ("editor.background", &["editor.background", "background"]),
    ("editor.foreground", &["editor.foreground", "text"]),
    ("editor.lineHighlightBackground", &["editor.active_line.background"]),
    ("editorLineNumber.foreground", &["editor.line_number"]),
    ("editorLineNumber.activeForeground", &["editor.active_line_number"]),
    ("sideBar.background", &["panel.background", "surface.background"]),
    ("sideBar.foreground", &["text"]),
    ("editorGroupHeader.tabsBackground", &["tab_bar.background"]),
    ("tab.activeBackground", &["tab.active_background"]),
    ("tab.inactiveBackground", &["tab.inactive_background"]),
    ("tab.activeForeground", &["text"]),
    ("tab.inactiveForeground", &["text.muted", "text.disabled"]),
    ("statusBar.background", &["status_bar.background"]),
    ("statusBar.foreground", &["text"]),
    ("panel.border", &["border"]),
    ("editorWidget.background", &["elevated_surface.background"]),
    ("list.hoverBackground", &["element.hover"]),
    ("list.activeSelectionBackground", &["element.selected"]),
    ("input.background", &["element.background", "surface.background"]),
    ("focusBorder", &["border.focused"]),
    ("editorError.foreground", &["error", "error.background"]),
    ("editorWarning.foreground", &["warning", "warning.background"]),
    ("editorInfo.foreground", &["info", "info.background"]),
    ("editorGutter.addedBackground", &["created", "created.background"]),
    ("editorGutter.modifiedBackground", &["modified", "modified.background"]),
    ("editorGutter.deletedBackground", &["deleted", "deleted.background"]),
    ("editor.findMatchHighlightBackground", &["search.match_background"]),
    ("editor.findMatchBackground", &["search.match_background"]),
    ("editorRuler.foreground", &["editor.wrap_guide"]),
    ("editor.wordHighlightBackground", &["editor.document_highlight.read_background"]),
    ("editor.wordHighlightStrongBackground", &["editor.document_highlight.write_background"]),
];

/// (Zed syntax key, TextMate scopes). Ordered generic -> specific.
const SYNTAX_MAP: &[(&str, &[&str])] = &[
    ("variable", &["variable", "variable.other", "variable.parameter", "meta.definition.variable"]),
    ("variable.special", &["variable.language", "variable.other.constant", "support.variable"]),
    (
        "property",
        &[
            "variable.other.property",
            "support.variable.property",
            "meta.object-literal.key",
            "support.type.property-name",
            "entity.name.tag.yaml",
        ],
    ),
    ("attribute", &["entity.other.attribute-name", "meta.attribute", "entity.other.inherited-class"]),
    ("punctuation", &["punctuation", "meta.brace", "meta.delimiter"]),
    (
        "punctuation.bracket",
        &[
            "punctuation.definition.block",
            "punctuation.definition.parameters",
            "punctuation.section",
            "meta.brace.round",
            "meta.brace.square",
        ],
    ),
    ("punctuation.delimiter", &["punctuation.separator", "punctuation.terminator", "punctuation.accessor"]),
    ("punctuation.special", &["punctuation.definition.template-expression", "punctuation.definition.interpolation"]),
    (
        "punctuation.list_marker",
        &["punctuation.definition.list.begin.markdown", "markup.list punctuation.definition.list"],
    ),
    ("operator", &["keyword.operator"]),
    ("keyword", &["keyword", "storage", "storage.type", "storage.modifier", "keyword.control"]),
    ("number", &["constant.numeric"]),
    ("boolean", &["constant.language.boolean", "constant.language"]),
    ("constant", &["constant", "constant.other", "variable.other.constant", "support.constant"]),
    ("string", &["string", "string.quoted"]),
    ("string.escape", &["constant.character.escape"]),
    ("string.regex", &["string.regexp"]),
    ("string.special", &["string.template", "string.other"]),
    ("string.special.symbol", &["constant.other.symbol"]),
    ("comment", &["comment"]),
    ("comment.doc", &["comment.block.documentation", "comment.line.documentation", "string.quoted.docstring"]),
    ("function", &["entity.name.function", "support.function", "meta.function-call"]),
    ("function.method", &["entity.name.function.member", "meta.method.declaration entity.name.function"]),
    ("function.definition", &["meta.definition.function entity.name.function", "meta.function entity.name.function"]),
    ("function.special", &["support.function.builtin", "entity.name.function.macro"]),
    ("type", &["entity.name.type", "support.type", "support.class", "entity.name.class", "storage.type.primitive"]),
    ("constructor", &["entity.name.class", "support.class", "meta.class entity.name.type"]),
    ("enum", &["entity.name.type.enum", "variable.other.enummember"]),
    ("variant", &["variable.other.enummember"]),
    ("interface", &["entity.name.type.interface"]),
    ("namespace", &["entity.name.namespace", "entity.name.module"]),
    ("tag", &["entity.name.tag"]),
    ("label", &["entity.name.label", "punctuation.definition.label"]),
    ("preproc", &["meta.preprocessor", "keyword.control.directive", "keyword.other.preprocessor"]),
    ("embedded", &["meta.embedded", "source.groovy.embedded"]),
    ("title", &["markup.heading", "entity.name.section", "markup.heading entity.name"]),
    ("emphasis", &["markup.italic"]),
    ("emphasis.strong", &["markup.bold"]),
    ("link_text", &["markup.underline.link.text", "string.other.link"]),
    ("link_uri", &["markup.underline.link", "string.other.link.uri"]),
    ("text.literal", &["markup.inline.raw", "markup.fenced_code", "markup.raw"]),
    ("hint", &["markup.quote"]),
    ("primary", &["source"]),
];

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

fn is_color(s: &str) -> bool {
    s.starts_with('#') && matches!(s.len(), 4 | 7 | 9) && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// True when the color has an explicit alpha of zero.
fn fully_transparent(s: &str) -> bool {
    s.len() == 9 && s.ends_with("00")
}

fn usable_color<'a>(style: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    let s = style.get(key)?.as_str()?;
    (is_color(s) && !fully_transparent(s)).then_some(s)
}

fn convert_theme(t: &Value) -> Result<(String, String), String> {
    let name = t.get("name").and_then(Value::as_str).ok_or("theme without a name")?.to_string();
    let appearance = t.get("appearance").and_then(Value::as_str).unwrap_or("dark");
    let kind = if appearance.eq_ignore_ascii_case("light") { "light" } else { "dark" };
    let empty = Map::new();
    let style = t.get("style").and_then(Value::as_object).unwrap_or(&empty);

    let mut colors = Map::new();
    for (vs, zeds) in COLOR_MAP {
        if let Some(c) = zeds.iter().find_map(|z| usable_color(style, z)) {
            colors.insert((*vs).to_string(), json!(c));
        }
    }
    let player = style.get("players").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_object);
    if let Some(p) = player {
        if let Some(c) = usable_color(p, "selection") {
            colors.insert("editor.selectionBackground".into(), json!(c));
        }
        if let Some(c) = usable_color(p, "cursor") {
            colors.insert("editorCursor.foreground".into(), json!(c));
        }
    }

    let mut rules = Vec::new();
    if let Some(syntax) = style.get("syntax").and_then(Value::as_object) {
        for (key, scopes) in SYNTAX_MAP {
            let Some(e) = syntax.get(*key).and_then(Value::as_object) else { continue };
            let mut settings = Map::new();
            if let Some(c) = e.get("color").and_then(Value::as_str).filter(|c| is_color(c)) {
                settings.insert("foreground".into(), json!(c));
            }
            let mut fs = Vec::new();
            if e.get("font_style").and_then(Value::as_str) == Some("italic") {
                fs.push("italic");
            }
            if e.get("font_weight").and_then(Value::as_f64).is_some_and(|w| w >= 600.0) {
                fs.push("bold");
            }
            if !fs.is_empty() {
                settings.insert("fontStyle".into(), json!(fs.join(" ")));
            }
            if settings.is_empty() {
                continue;
            }
            rules.push(json!({ "scope": scopes, "settings": settings }));
        }
    }

    let out = json!({ "name": name, "type": kind, "colors": colors, "tokenColors": rules });
    Ok((name, serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?))
}

/// Convert a Zed theme family (`{"name","author","themes":[...]}`) to VS Code theme JSON texts.
/// Returns `(theme name, VS Code JSON)` per theme. Comments and trailing commas are tolerated.
pub fn convert(zed_json: &str) -> Result<Vec<(String, String)>, String> {
    let v: Value = serde_json::from_str(&clean_json(zed_json)).map_err(|e| format!("invalid Zed theme JSON: {e}"))?;
    let themes = v.get("themes").and_then(Value::as_array).ok_or("not a Zed theme family: missing \"themes\" array")?;
    let mut out = Vec::new();
    for t in themes {
        out.push(convert_theme(t)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Template
// ---------------------------------------------------------------------------

fn template_value(name: &str) -> Value {
    json!({
        "_comment": [
            "Custom lumen-text theme (VS Code theme format). JSON has no comments, so notes live here.",
            "Edit any value; use #rrggbb or #rrggbbaa. Missing color keys fall back to values derived from editor.background/foreground.",
            "'type' is 'dark' or 'light'. 'tokenColors' rules: 'scope' is a TextMate scope (or list); a later rule wins over an earlier one of equal specificity, longer scopes beat shorter.",
            "'fontStyle' may combine: italic, bold, underline.",
            "Save the file in the user theme dir (~/.config/lumen-text/themes) and pick it from the theme list."
        ],
        "name": name,
        "type": "dark",
        "colors": {
            "editor.background": "#1e1e1e",
            "editor.foreground": "#d4d4d4",
            "editor.selectionBackground": "#264f78",
            "editorCursor.foreground": "#aeafad",
            "editor.lineHighlightBackground": "#ffffff12",
            "editorLineNumber.foreground": "#858585",
            "editorWhitespace.foreground": "#3b3b3b",
            "editorLineNumber.activeForeground": "#c6c6c6",
            "sideBar.background": "#181818",
            "sideBar.foreground": "#cccccc",
            "editorGroupHeader.tabsBackground": "#181818",
            "tab.activeBackground": "#1e1e1e",
            "tab.inactiveBackground": "#181818",
            "tab.activeForeground": "#ffffff",
            "tab.inactiveForeground": "#9d9d9d",
            "statusBar.background": "#007acc",
            "statusBar.foreground": "#ffffff",
            "editorWidget.background": "#252526",
            "list.activeSelectionBackground": "#37373d",
            "list.hoverBackground": "#2a2d2e",
            "input.background": "#3c3c3c",
            "focusBorder": "#007acc",
            "editorError.foreground": "#f14c4c",
            "editorWarning.foreground": "#cca700",
            "editorInfo.foreground": "#3794ff",
            "editorGutter.addedBackground": "#2ea043",
            "editorGutter.modifiedBackground": "#1b81a8",
            "editorGutter.deletedBackground": "#f85149",
            "editor.findMatchHighlightBackground": "#ea5c0055",
            "editor.findMatchBackground": "#51c0a880",
            "editorRuler.foreground": "#5a5a5a",
            "editor.wordHighlightBackground": "#57575740",
            "editor.wordHighlightStrongBackground": "#0e639c40"
        },
        "tokenColors": [
            { "scope": ["comment", "punctuation.definition.comment"], "settings": { "foreground": "#6a9955", "fontStyle": "italic" } },
            { "scope": ["string", "string.quoted"], "settings": { "foreground": "#ce9178" } },
            { "scope": ["constant.character.escape", "string.regexp"], "settings": { "foreground": "#d7ba7d" } },
            { "scope": ["keyword", "storage", "storage.type", "keyword.control"], "settings": { "foreground": "#c586c0" } },
            { "scope": ["entity.name.function", "support.function", "meta.function-call"], "settings": { "foreground": "#dcdcaa" } },
            { "scope": ["entity.name.type", "support.type", "support.class", "entity.name.class"], "settings": { "foreground": "#4ec9b0" } },
            { "scope": ["constant.numeric"], "settings": { "foreground": "#b5cea8" } },
            { "scope": ["constant.language", "constant.language.boolean", "support.constant"], "settings": { "foreground": "#569cd6" } },
            { "scope": ["variable", "variable.other"], "settings": { "foreground": "#9cdcfe" } },
            { "scope": ["variable.language"], "settings": { "foreground": "#569cd6", "fontStyle": "italic" } },
            { "scope": ["variable.other.property", "support.variable.property", "meta.object-literal.key"], "settings": { "foreground": "#9cdcfe" } },
            { "scope": ["entity.name.tag"], "settings": { "foreground": "#569cd6" } },
            { "scope": ["entity.other.attribute-name"], "settings": { "foreground": "#9cdcfe" } },
            { "scope": ["keyword.operator"], "settings": { "foreground": "#d4d4d4" } },
            { "scope": ["punctuation", "meta.brace"], "settings": { "foreground": "#808080" } },
            { "scope": ["markup.heading", "entity.name.section"], "settings": { "foreground": "#569cd6", "fontStyle": "bold" } },
            { "scope": ["markup.italic"], "settings": { "fontStyle": "italic" } },
            { "scope": ["markup.bold"], "settings": { "fontStyle": "bold" } },
            { "scope": ["markup.underline.link"], "settings": { "foreground": "#3794ff", "fontStyle": "underline" } },
            { "scope": ["markup.inline.raw", "markup.fenced_code"], "settings": { "foreground": "#ce9178" } },
            { "scope": ["support.type.property-name.json"], "settings": { "foreground": "#9cdcfe" } },
            { "scope": ["invalid", "invalid.illegal"], "settings": { "foreground": "#f44747" } }
        ]
    })
}

/// A documented default custom theme (VS Code JSON, pretty-printed) containing every
/// `colors` key that `Theme::load` reads (plus a few extra) and ~20 tokenColors examples.
pub fn theme_template() -> String {
    serde_json::to_string_pretty(&template_value("My Custom Theme")).unwrap_or_default() + "\n"
}

// ---------------------------------------------------------------------------
// Directories
// ---------------------------------------------------------------------------

fn home_dir() -> Option<PathBuf> {
    crate::paths::home()
}

/// `$XDG_CONFIG_HOME/lumen-text/themes` or `~/.config/lumen-text/themes`.
pub fn user_theme_dir() -> Option<PathBuf> {
    let base = crate::paths::config_home()?;
    Some(base.join("lumen-text").join("themes"))
}

/// All `*.json` files in the user theme dir as `(file stem, path)`, sorted by name.
pub fn list_user_themes() -> Vec<(String, PathBuf)> {
    user_theme_dir().map(|d| list_json_in(&d)).unwrap_or_default()
}

fn list_json_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.1.cmp(&b.1)));
    v
}

/// `~/.config/zed/themes` plus every `~/.local/share/zed/extensions/installed/*/themes`.
pub fn default_zed_theme_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else { return vec![] };
    let mut v = vec![home.join(".config/zed/themes")];
    let mut exts: Vec<PathBuf> = fs::read_dir(home.join(".local/share/zed/extensions/installed"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("themes"))
        .filter(|p| p.is_dir())
        .collect();
    exts.sort();
    v.extend(exts);
    v
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Convert every Zed theme file (`*.json`) found in `zed_dirs` into `out_dir/zed-<slug>.json`.
/// A theme whose output file is newer than its source file is left untouched. Returns the
/// names of all themes that are available in `out_dir` afterwards (written or up to date).
pub fn install_zed_themes(zed_dirs: &[PathBuf], out_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for dir in zed_dirs {
        for (_, path) in list_json_in(dir) {
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let Ok(themes) = convert(&text) else { continue };
            let src_time = fs::metadata(&path).and_then(|m| m.modified()).ok();
            for (name, json) in themes {
                let s = slug(&name);
                if s.is_empty() {
                    continue;
                }
                let target = out_dir.join(format!("zed-{s}.json"));
                let out_time = fs::metadata(&target).and_then(|m| m.modified()).ok();
                let fresh = matches!((src_time, out_time), (Some(a), Some(b)) if b > a);
                if !fresh && (fs::create_dir_all(out_dir).is_err() || fs::write(&target, json).is_err()) {
                    continue;
                }
                names.push(name);
            }
        }
    }
    names
}

/// Write the template to a unique `custom-<slug>.json` in `out_dir` (never overwrites;
/// appends `-2`, `-3`, ... on collision) and return its path.
pub fn create_custom_theme_file(out_dir: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(out_dir)?;
    let base = match slug(name) {
        s if s.is_empty() => "theme".to_string(),
        s => s,
    };
    let name = if name.trim().is_empty() { "My Custom Theme" } else { name.trim() };
    let text = serde_json::to_string_pretty(&template_value(name)).unwrap_or_default() + "\n";
    for n in 1.. {
        let file = if n == 1 { format!("custom-{base}.json") } else { format!("custom-{base}-{n}.json") };
        let path = out_dir.join(file);
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                io::Write::write_all(&mut f, text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let d = base.join(format!("zedtheme-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const SAMPLE: &str = r##"{
      // header comment
      "name": "Fam", "author": "me",
      "themes": [
        { "name": "Dark One", "appearance": "dark", "style": {
            "editor.background": "#101010", "editor.foreground": "#eeeeee",
            "editor.active_line.background": "#ffffff10", /* block */
            "border": "#333333", "text.muted": "#888888",
            "players": [ { "cursor": "#ff0000", "selection": "#00ff0040", }, ],
            "syntax": {
              "comment": { "color": "#667788", "font_style": "italic", },
              "keyword": { "color": "#aa00ff", "font_weight": 700 },
              "string": { "color": "#00aa00ff" },
              "bogus": { "color": "#123456" },
            },
        } },
        { "name": "Light One", "appearance": "light", "style": { "editor.background": "#fafafa" } }
      ]
    }"##;

    fn val(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn strips_comments_outside_strings() {
        let c = clean_json("{\"a\": 1 // x\n, /* y */ \"b\": 2}");
        assert_eq!(val(&c), json!({"a":1,"b":2}));
    }

    #[test]
    fn keeps_comment_like_text_inside_strings() {
        let c = clean_json(r#"{"u": "http://x.y/*not*/ \" // still"}"#);
        assert_eq!(val(&c)["u"], "http://x.y/*not*/ \" // still");
    }

    #[test]
    fn strips_trailing_commas_only_outside_strings() {
        let c = clean_json(r#"{"a": [1, 2,], "b": "x,]", }"#);
        assert_eq!(val(&c), json!({"a":[1,2],"b":"x,]"}));
    }

    #[test]
    fn trailing_comma_before_comment_then_bracket() {
        let c = clean_json("[1, 2, // c\n ]");
        assert_eq!(val(&c), json!([1, 2]));
    }

    #[test]
    fn converts_all_themes_with_names_and_types() {
        let r = convert(SAMPLE).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Dark One");
        assert_eq!(val(&r[0].1)["type"], "dark");
        assert_eq!(val(&r[1].1)["type"], "light");
        assert_eq!(val(&r[1].1)["name"], "Light One");
    }

    #[test]
    fn maps_colors() {
        let v = val(&convert(SAMPLE).unwrap()[0].1);
        let c = &v["colors"];
        assert_eq!(c["editor.background"], "#101010");
        assert_eq!(c["editor.lineHighlightBackground"], "#ffffff10");
        assert_eq!(c["panel.border"], "#333333");
        assert_eq!(c["tab.inactiveForeground"], "#888888");
        assert_eq!(c["editorCursor.foreground"], "#ff0000");
        assert_eq!(c["editor.selectionBackground"], "#00ff0040");
    }

    #[test]
    fn alpha_colors_are_kept_and_transparent_skipped() {
        let z = r##"{"themes":[{"name":"A","appearance":"dark","style":{
            "panel.background":"#00000000","surface.background":"#12345678","editor.background":"#000000"}}]}"##;
        let v = val(&convert(z).unwrap()[0].1);
        assert_eq!(v["colors"]["sideBar.background"], "#12345678");
    }

    #[test]
    fn maps_syntax_with_italic_and_bold() {
        let v = val(&convert(SAMPLE).unwrap()[0].1);
        let rules = v["tokenColors"].as_array().unwrap();
        let find =
            |scope: &str| rules.iter().find(|r| r["scope"].as_array().unwrap().iter().any(|s| s == scope)).unwrap();
        let c = find("comment");
        assert_eq!(c["settings"]["foreground"], "#667788");
        assert_eq!(c["settings"]["fontStyle"], "italic");
        assert_eq!(find("keyword")["settings"]["fontStyle"], "bold");
        assert_eq!(find("string.quoted")["settings"]["foreground"], "#00aa00ff");
        assert_eq!(rules.len(), 3, "unknown syntax keys are ignored");
    }

    #[test]
    fn malformed_input_errors() {
        assert!(convert("{ not json").is_err());
        assert!(convert("[]").is_err());
        assert!(convert(r#"{"name":"x"}"#).is_err());
        assert!(convert(r#"{"themes":[{"appearance":"dark"}]}"#).is_err());
    }

    #[test]
    fn converted_theme_loads_with_theme_load() {
        let d = tmp();
        let r = convert(SAMPLE).unwrap();
        let p = d.join("a.json");
        fs::write(&p, &r[0].1).unwrap();
        let t = Theme::load(&p).unwrap();
        assert_eq!(t.background.0, 0x10);
        assert_eq!(t.cursor.0, 0xff);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn template_loads_and_has_enough_rules() {
        let d = tmp();
        let p = d.join("t.json");
        fs::write(&p, theme_template()).unwrap();
        let t = Theme::load(&p).unwrap();
        assert_eq!(t.name, "My Custom Theme");
        let v = val(&theme_template());
        assert!(v["tokenColors"].as_array().unwrap().len() >= 20);
        assert!(v["_comment"].is_array());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn template_covers_every_color_key_read_by_theme_load() {
        let src = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/theme.rs")).unwrap();
        let v = val(&theme_template());
        let colors = v["colors"].as_object().unwrap();
        for line in src.lines() {
            for pre in ["color(\"", "ck(\""] {
                if let Some(i) = line.find(pre) {
                    let rest = &line[i + pre.len()..];
                    let key = &rest[..rest.find('"').unwrap()];
                    assert!(colors.contains_key(key), "template misses {key}");
                }
            }
        }
    }

    #[test]
    fn real_min_theme_converts_and_loads() {
        let Some(home) = home_dir() else { return };
        let p = home.join(".local/share/zed/extensions/installed/min-theme/themes/min-theme.json");
        let Ok(text) = fs::read_to_string(&p) else { return };
        let r = convert(&text).unwrap();
        assert!(!r.is_empty());
        let d = tmp();
        for (i, (name, json)) in r.iter().enumerate() {
            let f = d.join(format!("{i}.json"));
            fs::write(&f, json).unwrap();
            let t = Theme::load(&f).unwrap();
            let light = val(json)["type"] == "light";
            let lum = t.background.luminance();
            assert!(if light { lum > 0.5 } else { lum < 0.5 }, "{name}: luminance {lum} vs light={light}");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn install_writes_then_skips_up_to_date() {
        let d = tmp();
        let src = d.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("fam.json"), SAMPLE).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let out = d.join("out");
        let names = install_zed_themes(&[src.clone()], &out);
        assert_eq!(names, vec!["Dark One".to_string(), "Light One".to_string()]);
        let f = out.join("zed-dark-one.json");
        assert!(f.exists() && out.join("zed-light-one.json").exists());
        fs::write(&f, "SENTINEL").unwrap();
        let names2 = install_zed_themes(&[src.clone()], &out);
        assert_eq!(names2.len(), 2);
        assert_eq!(fs::read_to_string(&f).unwrap(), "SENTINEL", "up-to-date output must not be rewritten");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn install_reconverts_when_source_is_newer() {
        let d = tmp();
        let src = d.join("src");
        fs::create_dir_all(&src).unwrap();
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("zed-dark-one.json"), "OLD").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(src.join("fam.json"), SAMPLE).unwrap();
        install_zed_themes(&[src], &out);
        assert_ne!(fs::read_to_string(out.join("zed-dark-one.json")).unwrap(), "OLD");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn install_ignores_bad_files_and_missing_dirs() {
        let d = tmp();
        let src = d.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("bad.json"), "{nope").unwrap();
        let names = install_zed_themes(&[src, d.join("missing")], &d.join("out"));
        assert!(names.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_custom_never_overwrites() {
        let d = tmp();
        let a = create_custom_theme_file(&d, "My Theme!").unwrap();
        let b = create_custom_theme_file(&d, "My Theme!").unwrap();
        assert_eq!(a.file_name().unwrap(), "custom-my-theme.json");
        assert_eq!(b.file_name().unwrap(), "custom-my-theme-2.json");
        assert_eq!(Theme::load(&b).unwrap().name, "My Theme!");
        let c = create_custom_theme_file(&d, "  ").unwrap();
        assert_eq!(c.file_name().unwrap(), "custom-theme.json");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn list_json_sorted_and_filtered() {
        let d = tmp();
        fs::write(d.join("b.json"), "{}").unwrap();
        fs::write(d.join("A.json"), "{}").unwrap();
        fs::write(d.join("c.txt"), "").unwrap();
        let l = list_json_in(&d);
        assert_eq!(l.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["A", "b"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn slug_rules() {
        assert_eq!(slug("Min Dark (Solid)"), "min-dark-solid");
        assert_eq!(slug("--"), "");
    }

    #[test]
    fn dirs_helpers_do_not_panic() {
        let _ = user_theme_dir();
        let _ = list_user_themes();
        let _ = default_zed_theme_dirs();
    }
}
