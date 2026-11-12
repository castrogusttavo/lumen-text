//! Per-project settings.
//!
//! `.lumen/config.json` at the project root (shared, meant to be committed) and
//! `.lumen/config.local.json` (personal, meant to be git-ignored) override the user's
//! `config.json` for that project: **user < project < local**. Every file may also carry
//! per-language overrides, `"languages": {"python": {"tab_width": 4, "format_on_save": true}}`
//! (see [`LANG_KEYS`]).
//!
//! Window, UI and app-wide keys stay global ([`GLOBAL_ONLY`]): a project file setting them is
//! ignored. Anything that names a program to run ([`COMMAND_KEYS`], `language_servers` entries
//! with `command`/`args`/`env`) is left out until the user trusted the project's commands once
//! ([`TrustStore`], remembered per project and per set of commands in the state directory).
//!
//! [`Layers`] (inside `Config`) remembers the three layers so that `Config::save` writes each
//! change to the file it belongs to. Pure logic plus small file I/O; the app glue (live reload,
//! trust prompt, settings scope, status bar) is `app_projcfg.rs`.

use crate::config::Config;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Folder at the project root that holds the project files.
pub const DIR: &str = ".lumen";
pub const FILE: &str = "config.json";
pub const LOCAL_FILE: &str = "config.local.json";

/// Keys only the user's `config.json` can set: the window, the interface and app-wide behaviour.
/// (`test_watch` runs the tests on every save, so a cloned project must not switch it on.)
pub const GLOBAL_ONLY: &[&str] = &[
    "ui_scale",
    "scroll_speed",
    "updates",
    "sidebar_width",
    "git_panel_width",
    "md_split",
    "status_items",
    "status_order",
    "status_left_order",
    "ram_display",
    "ui_font_family",
    "ui_font_size",
    "ui_font_weight",
    "ui_line_height",
    "ui_font_ligatures",
    "theme_chrome",
    "test_watch",
    "lsp_lean_v",
    "scrollbar_v",
];

/// Project keys that run a program: applied only once the project is trusted.
pub const COMMAND_KEYS: &[&str] = &["build_command", "run_command", "test_command"];

/// Fields of a `language_servers` entry that decide what process starts.
pub const SERVER_EXEC_FIELDS: &[&str] = &["command", "args", "env"];

/// Keys a `languages.<id>` entry may override.
pub const LANG_KEYS: &[&str] = &["tab_width", "format_on_save", "word_wrap", "inlay_hints", "rulers"];

/// Which settings file the settings modal edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

/// Is the settings-modal key `key` one only the user file can hold? (The bottom bar switches and
/// order steppers are `status_*` rows.)
pub fn global_only_setting(key: &str) -> bool {
    GLOBAL_ONLY.contains(&key) || key.starts_with("status_")
}

pub fn project_file(root: &Path) -> PathBuf {
    root.join(DIR).join(FILE)
}

pub fn local_file(root: &Path) -> PathBuf {
    root.join(DIR).join(LOCAL_FILE)
}

/// A layer file: its object, `Null` when missing or empty, and a message when it is not valid JSON
/// (or not an object).
pub fn read_layer(path: &Path) -> (Value, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else { return (Value::Null, None) };
    if text.trim().is_empty() {
        return (Value::Null, None);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v @ Value::Object(_)) => (v, None),
        Ok(_) => (Value::Null, Some(format!("{}: must be a JSON object", path.display()))),
        Err(e) => (Value::Null, Some(format!("{}: {e}", path.display()))),
    }
}

/// Does `v` hold at least one key?
pub fn non_empty(v: &Value) -> bool {
    v.as_object().is_some_and(|o| !o.is_empty())
}

/// Merge `src` into `dst`: objects key by key (recursively), anything else replaces.
pub fn deep_merge(dst: &mut Value, src: &Value) {
    match (dst.as_object_mut(), src.as_object()) {
        (Some(d), Some(s)) => {
            for (k, v) in s {
                deep_merge(d.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        _ => *dst = src.clone(),
    }
}

/// `language_servers` without the fields that choose a program (untrusted project).
fn strip_exec(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        for e in o.values_mut().filter_map(Value::as_object_mut) {
            for f in SERVER_EXEC_FIELDS {
                e.remove(*f);
            }
        }
        o.retain(|_, e| non_empty(e));
    }
    v
}

/// The effective configuration object: `user` with each layer (project, then local) on top.
/// Global-only keys of a layer are ignored; without `allow_commands` its commands are too. A
/// layer that picks a theme without an `appearance` also pins it (`"fixed"`), so the user's
/// "follow the system" does not hide the project's choice.
pub fn merge(user: &Value, layers: &[&Value], allow_commands: bool) -> Value {
    let mut out = if user.is_object() { user.clone() } else { json!({}) };
    for layer in layers {
        let Some(o) = layer.as_object() else { continue };
        let Some(dst) = out.as_object_mut() else { break };
        for (k, v) in o {
            let k = k.as_str();
            if GLOBAL_ONLY.contains(&k) || (!allow_commands && COMMAND_KEYS.contains(&k)) {
                continue;
            }
            let v = if k == "language_servers" && !allow_commands { strip_exec(v) } else { v.clone() };
            if matches!(k, "languages" | "language_servers") {
                deep_merge(dst.entry(k).or_insert(Value::Null), &v);
            } else {
                dst.insert(k.to_string(), v);
            }
        }
        if o.contains_key("theme") && !o.contains_key("appearance") {
            dst.insert("appearance".into(), json!("fixed"));
        }
    }
    out
}

/// Keys of a layer that are ignored because only the user file may set them.
pub fn ignored_keys(layer: &Value) -> Vec<String> {
    layer
        .as_object()
        .map_or_else(Vec::new, |o| o.keys().filter(|k| GLOBAL_ONLY.contains(&k.as_str())).cloned().collect())
}

/// What a layer would run, one readable line each (`build_command: make`,
/// `language_servers.rust: ./ra --x`). Empty: nothing to trust.
pub fn commands(layer: &Value) -> Vec<String> {
    let Some(o) = layer.as_object() else { return Vec::new() };
    let mut out = Vec::new();
    for k in COMMAND_KEYS {
        if let Some(s) = o.get(*k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
            out.push(format!("{k}: {s}"));
        }
    }
    for (id, e) in o.get("language_servers").and_then(Value::as_object).into_iter().flatten() {
        let Some(e) = e.as_object() else { continue };
        if !SERVER_EXEC_FIELDS.iter().any(|f| e.contains_key(*f)) {
            continue;
        }
        let mut line = e.get("command").and_then(Value::as_str).unwrap_or("(built-in command)").to_string();
        for a in e.get("args").and_then(Value::as_array).into_iter().flatten() {
            line.push(' ');
            line.push_str(&a.as_str().map_or_else(|| a.to_string(), str::to_string));
        }
        if let Some(env) = e.get("env").and_then(Value::as_object).filter(|m| !m.is_empty()) {
            let keys: Vec<&str> = env.keys().map(String::as_str).collect();
            line.push_str(&format!(" (env {})", keys.join(", ")));
        }
        out.push(format!("language_servers.{id}: {line}"));
    }
    out
}

/// Stable id of a set of commands: a changed command asks again.
pub fn fingerprint(cmds: &[String]) -> String {
    let mut sorted: Vec<&String> = cmds.iter().collect();
    sorted.sort();
    let h = sorted
        .iter()
        .flat_map(|s| s.bytes().chain(std::iter::once(0u8)))
        .fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{h:016x}")
}

// ───────────────────────────── trust ─────────────────────────────

/// Remembered answers to "run this project's commands?": `{root: {"trusted": bool, "fingerprint": "…"}}`.
pub struct TrustStore {
    pub path: PathBuf,
}

impl TrustStore {
    fn read(&self) -> Map<String, Value> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    /// The answer given for these exact commands of this project, if any.
    pub fn decision(&self, root: &Path, fp: &str) -> Option<bool> {
        let all = self.read();
        let e = all.get(&*root.to_string_lossy())?;
        if e["fingerprint"].as_str() == Some(fp) { e["trusted"].as_bool() } else { None }
    }

    pub fn set(&self, root: &Path, fp: &str, trusted: bool) -> std::io::Result<()> {
        let mut all = self.read();
        all.insert(root.to_string_lossy().into_owned(), json!({"trusted": trusted, "fingerprint": fp}));
        self.write(all)
    }

    /// Forget the answer for a project (it is asked again).
    #[cfg(test)]
    pub fn forget(&self, root: &Path) -> std::io::Result<()> {
        let mut all = self.read();
        all.remove(&*root.to_string_lossy());
        self.write(all)
    }

    fn write(&self, all: Map<String, Value>) -> std::io::Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&Value::Object(all)).unwrap_or_default())
    }
}

// ───────────────────────────── layers ─────────────────────────────

/// The layers behind an effective configuration.
#[derive(Clone, Debug, Default)]
pub struct LayerState {
    pub root: PathBuf,
    /// The user layer, complete (`Config::to_json` of the user's own settings).
    pub user: Value,
    /// `.lumen/config.json` / `.lumen/config.local.json` as written (`Null` when absent).
    pub project: Value,
    pub local: Value,
    /// The effective settings as of the last load or save: saves write what changed since.
    pub loaded: Value,
    /// Where changes go: `None` = the layer that defines the key (local, project, else user);
    /// the settings modal sets it to its scope.
    pub target: Option<Scope>,
    /// Saves are ignored (while a reload re-applies values).
    pub muted: bool,
    /// The project's commands are applied (trusted, or there are none).
    pub commands_allowed: bool,
    /// Commands of the project files that were left out (not trusted).
    pub untrusted: Vec<String>,
}

impl LayerState {
    /// Is a project file in effect (with at least one key)?
    pub fn project_active(&self) -> bool {
        non_empty(&self.project) || non_empty(&self.local)
    }
}

/// [`LayerState`] behind a lock so `Config` stays `Clone + PartialEq + Send` and `save(&self)` can
/// update it. Never part of the comparison of two configurations.
#[derive(Default)]
pub struct Layers(Mutex<Option<LayerState>>);

impl Clone for Layers {
    fn clone(&self) -> Self {
        Layers(Mutex::new(self.lock().clone()))
    }
}

impl PartialEq for Layers {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Layers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Layers")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dest {
    User,
    Project,
    Local,
}

/// The part of `new` that differs from `old`: changed and added keys of objects (recursively),
/// else `new` itself.
fn delta(old: &Value, new: &Value) -> Value {
    match (old.as_object(), new.as_object()) {
        (Some(o), Some(n)) => Value::Object(
            n.iter()
                .filter(|(k, v)| o.get(k.as_str()) != Some(*v))
                .map(|(k, v)| (k.clone(), o.get(k.as_str()).map_or_else(|| v.clone(), |ov| delta(ov, v))))
                .collect(),
        ),
        _ => new.clone(),
    }
}

fn put(layer: &mut Value, key: &str, d: &Value) {
    if !layer.is_object() {
        *layer = json!({});
    }
    let slot = &mut layer[key];
    if slot.is_object() && d.is_object() {
        deep_merge(slot, d);
    } else {
        *slot = d.clone();
    }
}

/// Spread the changes between `st.loaded` and `full` over the layers. Returns which files must be
/// written: (user, project, local).
pub fn distribute(st: &mut LayerState, full: &Value) -> (bool, bool, bool) {
    let mut dirty = (false, false, false);
    let Some(fo) = full.as_object() else { return dirty };
    for (k, v) in fo {
        let old = st.loaded.get(k).cloned().unwrap_or(Value::Null);
        if &old == v {
            continue;
        }
        let d = delta(&old, v);
        let has = |layer: &Value| layer.get(k).is_some();
        let dest = if GLOBAL_ONLY.contains(&k.as_str()) {
            Dest::User
        } else {
            match st.target {
                Some(Scope::User) => Dest::User,
                Some(Scope::Project) if has(&st.local) => Dest::Local,
                Some(Scope::Project) => Dest::Project,
                None if has(&st.local) => Dest::Local,
                None if has(&st.project) => Dest::Project,
                None => Dest::User,
            }
        };
        if dest == Dest::User {
            put(&mut st.user, k, &d);
            dirty.0 = true;
            continue;
        }
        // What the key would be without this layer: the user's value (under the local file, the
        // project's when it has one).
        let below = if dest == Dest::Local { st.project.get(k).or_else(|| st.user.get(k)) } else { st.user.get(k) };
        let inherited = below == Some(v);
        let layer = if dest == Dest::Local { &mut st.local } else { &mut st.project };
        // Back to the user's own value: the project file no longer needs the key.
        if inherited && !v.is_object() {
            if let Some(o) = layer.as_object_mut() {
                o.remove(k);
            }
        } else {
            put(layer, k, &d);
        }
        if dest == Dest::Local {
            dirty.2 = true;
        } else {
            dirty.1 = true;
        }
    }
    st.loaded = full.clone();
    dirty
}

fn write_json(path: &Path, v: &Value) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let v = if v.is_object() { v.clone() } else { json!({}) };
    std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default() + "\n")
}

impl Layers {
    fn lock(&self) -> MutexGuard<'_, Option<LayerState>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, st: Option<LayerState>) {
        *self.lock() = st;
    }

    pub fn get(&self) -> Option<LayerState> {
        self.lock().clone()
    }

    /// Run `f` on the state (`None` for a plain user config).
    pub fn with<R>(&self, f: impl FnOnce(&mut LayerState) -> R) -> Option<R> {
        self.lock().as_mut().map(f)
    }

    pub fn project_active(&self) -> bool {
        self.lock().as_ref().is_some_and(LayerState::project_active)
    }

    /// `Config::save` with layers: write the changes since the last load/save to their files.
    /// False when there are no layers (the caller writes the user file as before).
    pub fn save(&self, cfg: &Config, user_path: &Path) -> bool {
        let mut g = self.lock();
        let Some(st) = g.as_mut() else { return false };
        if st.muted {
            return true;
        }
        let (u, p, l) = distribute(st, &cfg.to_json());
        if u || !user_path.exists() {
            let _ = write_json(user_path, &st.user);
        }
        if p {
            let _ = write_json(&project_file(&st.root), &st.project);
        }
        if l {
            let _ = write_json(&local_file(&st.root), &st.local);
        }
        true
    }
}

/// Does the settings-modal key `key` have a value in `layer`? Dotted keys
/// (`language_servers.pyright.enabled`) are paths.
pub fn layer_has(layer: &Value, key: &str) -> bool {
    if let Some(rest) = key.strip_prefix("language_servers.") {
        let ptr = format!("/language_servers/{}", rest.replace('.', "/"));
        return layer.pointer(&ptr).is_some();
    }
    layer.get(key).is_some()
}

/// Lines for the tooltip of the "project settings" indicator.
pub fn summary(st: &LayerState) -> Vec<String> {
    let mut out = Vec::new();
    let keys = |v: &Value| v.as_object().map_or_else(Vec::new, |o| o.keys().cloned().collect::<Vec<_>>());
    for (name, layer) in [(FILE, &st.project), (LOCAL_FILE, &st.local)] {
        let k = keys(layer);
        if !k.is_empty() {
            out.push(format!("{DIR}/{name}: {}", k.join(", ")));
        }
    }
    let mut ignored = ignored_keys(&st.project);
    ignored.extend(ignored_keys(&st.local));
    if !ignored.is_empty() {
        out.push(format!("Ignored (user settings only): {}", ignored.join(", ")));
    }
    if !st.untrusted.is_empty() {
        out.push(format!("Not applied until trusted: {}", st.untrusted.join(" · ")));
    }
    out
}

// ───────────────────────────── per-language overrides ─────────────────────────────

impl Config {
    /// `languages.<lang>.<key>` of the effective settings.
    pub fn lang_override(&self, lang: Option<&str>, key: &str) -> Option<&Value> {
        self.languages.get(lang?)?.get(key)
    }

    /// Tab width for documents of `lang` (an override may use any width from 1 to 16).
    pub fn tab_width_for(&self, lang: Option<&str>) -> usize {
        self.lang_override(lang, "tab_width")
            .and_then(Value::as_u64)
            .filter(|n| (1..=16).contains(n))
            .map_or(self.tab_width, |n| n as usize)
    }

    pub fn format_on_save_for(&self, lang: Option<&str>) -> bool {
        self.lang_override(lang, "format_on_save").and_then(Value::as_bool).unwrap_or(self.format_on_save)
    }

    pub fn word_wrap_for(&self, lang: Option<&str>) -> bool {
        self.lang_override(lang, "word_wrap").and_then(Value::as_bool).unwrap_or(self.word_wrap)
    }

    pub fn inlay_hints_for(&self, lang: Option<&str>) -> bool {
        self.lang_override(lang, "inlay_hints").and_then(Value::as_bool).unwrap_or(self.inlay_hints)
    }

    /// Ruler columns for documents of `lang` (a `rulers` override replaces the global list).
    pub fn rulers_for(&self, lang: Option<&str>) -> Vec<usize> {
        self.lang_override(lang, "rulers").map_or_else(|| self.rulers.clone(), crate::assist::parse_rulers)
    }

    /// Unknown keys of `languages` entries, as friendly messages.
    pub fn languages_errors(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (lang, e) in self.languages.as_object().into_iter().flatten() {
            match e.as_object() {
                Some(o) => out.extend(
                    o.keys()
                        .filter(|k| !LANG_KEYS.contains(&k.as_str()))
                        .map(|k| format!("languages.{lang}.{k}: unknown key ({})", LANG_KEYS.join(", "))),
                ),
                None => out.push(format!("languages.{lang} must be an object, e.g. {{\"tab_width\": 4}}")),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-projcfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn precedence_is_user_then_project_then_local() {
        let user = json!({"tab_width": 4, "format_on_save": false, "word_wrap": false, "theme": "a"});
        let project = json!({"tab_width": 2, "format_on_save": true});
        let local = json!({"tab_width": 8});
        let m = merge(&user, &[&project, &local], true);
        assert_eq!(m["tab_width"], json!(8), "local wins");
        assert_eq!(m["format_on_save"], json!(true), "project beats user");
        assert_eq!(m["word_wrap"], json!(false), "untouched keys keep the user's value");
        let c = Config::from_value(&m);
        assert_eq!((c.tab_width, c.format_on_save), (8, true));
        assert_eq!(merge(&user, &[&Value::Null, &Value::Null], true), user, "no project files: the user config");
    }

    #[test]
    fn language_overrides_merge_per_language_and_key() {
        let user = json!({"tab_width": 4, "languages": {"python": {"tab_width": 4, "word_wrap": true}, "go": {"tab_width": 8}}});
        let project = json!({"languages": {"python": {"tab_width": 2, "format_on_save": true}}});
        let local = json!({"languages": {"python": {"format_on_save": false}}});
        let c = Config::from_value(&merge(&user, &[&project, &local], true));
        assert_eq!(c.tab_width_for(Some("python")), 2);
        assert!(c.word_wrap_for(Some("python")), "user's python entry survives the project's");
        assert!(!c.format_on_save_for(Some("python")), "local wins inside a language");
        assert_eq!(c.tab_width_for(Some("go")), 8);
        assert_eq!(c.tab_width_for(Some("rust")), 4, "no override: the global value");
        assert_eq!(c.tab_width_for(None), 4);
        assert!(!c.inlay_hints_for(Some("python")));
        let bad = Config::from_value(&json!({"languages": {"python": {"tab_width": 99, "colour": 1}, "go": 3}}));
        assert_eq!(bad.tab_width_for(Some("python")), 4, "out of range falls back");
        let errs = bad.languages_errors();
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("languages.python.colour")));
    }

    #[test]
    fn global_only_keys_are_ignored_and_reported() {
        let user = json!({"ui_scale": 1.0, "updates": "auto", "tab_width": 4});
        let project = json!({"ui_scale": 3.0, "updates": "off", "status_order": ["ram"], "tab_width": 2});
        let m = merge(&user, &[&project], true);
        assert_eq!((m["ui_scale"].clone(), m["updates"].clone()), (json!(1.0), json!("auto")));
        assert!(m.get("status_order").is_none());
        assert_eq!(m["tab_width"], json!(2));
        let mut ign = ignored_keys(&project);
        ign.sort();
        assert_eq!(ign, ["status_order", "ui_scale", "updates"]);
        assert!(global_only_setting("status_pos_git") && global_only_setting("ui_scale"));
        assert!(!global_only_setting("tab_width") && !global_only_setting("language_servers.ruff.enabled"));
    }

    #[test]
    fn a_project_theme_is_pinned() {
        let user = json!({"appearance": "system", "theme": "x"});
        let m = merge(&user, &[&json!({"theme": "dracula"})], true);
        assert_eq!((m["theme"].as_str(), m["appearance"].as_str()), (Some("dracula"), Some("fixed")));
        let m = merge(&user, &[&json!({"theme": "dracula", "appearance": "system"})], true);
        assert_eq!(m["appearance"], json!("system"), "an explicit appearance is kept");
    }

    #[test]
    fn untrusted_commands_are_left_out() {
        let user = json!({"build_command": "", "language_servers": {"pyright": {"enabled": false}}});
        let project = json!({
            "build_command": "make",
            "run_command": "  ",
            "language_servers": {
                "rust": {"command": "./evil", "args": ["--x"], "settings": {"a": 1}},
                "ruff": {"env": {"LD_PRELOAD": "x.so"}},
                "pyright": {"settings": {"python": {}}}
            }
        });
        let cmds = commands(&project);
        assert_eq!(cmds.len(), 3, "{cmds:?}");
        assert!(cmds.contains(&"build_command: make".to_string()));
        assert!(cmds.contains(&"language_servers.rust: ./evil --x".to_string()));
        assert!(cmds.iter().any(|c| c.starts_with("language_servers.ruff:") && c.contains("LD_PRELOAD")));
        assert!(commands(&json!({"tab_width": 2, "language_servers": {"x": {"enabled": true}}})).is_empty());

        let m = merge(&user, &[&project], false);
        assert_eq!(m["build_command"], json!(""), "command keys stay the user's");
        assert!(m.pointer("/language_servers/rust/command").is_none());
        assert!(m.pointer("/language_servers/rust/args").is_none());
        assert_eq!(m.pointer("/language_servers/rust/settings/a"), Some(&json!(1)), "harmless fields still apply");
        assert!(m.pointer("/language_servers/ruff").is_none(), "an entry that was only env is gone");
        assert_eq!(m.pointer("/language_servers/pyright/enabled"), Some(&json!(false)), "user's entry merged");
        let m = merge(&user, &[&project], true);
        assert_eq!(m["build_command"], json!("make"));
        assert_eq!(m.pointer("/language_servers/rust/command"), Some(&json!("./evil")));

        // The fingerprint ignores order and changes with any command.
        let fp = fingerprint(&cmds);
        let mut rev = cmds.clone();
        rev.reverse();
        assert_eq!(fingerprint(&rev), fp);
        let mut other = cmds.clone();
        other[0].push('!');
        assert_ne!(fingerprint(&other), fp);
    }

    #[test]
    fn trust_is_remembered_per_project_and_per_command_set() {
        let d = tmp("trust");
        let store = TrustStore { path: d.join("state/trusted-projects.json") };
        let (a, b) = (Path::new("/p/a"), Path::new("/p/b"));
        assert_eq!(store.decision(a, "f1"), None, "never asked");
        store.set(a, "f1", true).unwrap();
        store.set(b, "f1", false).unwrap();
        assert_eq!(store.decision(a, "f1"), Some(true));
        assert_eq!(store.decision(b, "f1"), Some(false), "a refusal is remembered too");
        assert_eq!(store.decision(a, "f2"), None, "changed commands: ask again");
        store.forget(a).unwrap();
        assert_eq!(store.decision(a, "f1"), None);
        assert_eq!(store.decision(b, "f1"), Some(false));
        std::fs::write(&store.path, "garbage").unwrap();
        assert_eq!(store.decision(b, "f1"), None, "a broken file means: not decided");
        std::fs::remove_dir_all(&d).ok();
    }

    fn state(user: Value, project: Value, local: Value) -> LayerState {
        let loaded = merge(&user, &[&project, &local], true);
        LayerState { user, project, local, loaded, commands_allowed: true, ..Default::default() }
    }

    #[test]
    fn saves_go_to_the_layer_that_defines_the_key() {
        let mut st = state(
            json!({"tab_width": 4, "word_wrap": false, "theme": "a", "ui_scale": 1.0}),
            json!({"word_wrap": true, "theme": "b"}),
            json!({"theme": "c"}),
        );
        let mut full = st.loaded.clone();
        full["tab_width"] = json!(2); // only the user file has it
        full["word_wrap"] = json!(false); // the project defines it, back to the user's value
        full["theme"] = json!("d"); // the local file defines it
        let dirty = distribute(&mut st, &full);
        assert_eq!(dirty, (true, true, true));
        assert_eq!(st.user["tab_width"], json!(2));
        assert!(st.project.get("word_wrap").is_none(), "back to the inherited value: removed from the project");
        assert_eq!(st.local["theme"], json!("d"));
        assert_eq!(st.project["theme"], json!("b"), "the layer below the local file is untouched");
        full["theme"] = json!("b");
        distribute(&mut st, &full);
        assert!(st.local.get("theme").is_none(), "back to the project's value: removed from the local file");
        assert_eq!(distribute(&mut st, &full), (false, false, false), "nothing changed since");
    }

    #[test]
    fn the_scope_chooses_the_file() {
        let mut st = state(json!({"tab_width": 4, "ui_scale": 1.0, "format_on_save": false}), Value::Null, Value::Null);
        st.target = Some(Scope::Project);
        let mut full = st.loaded.clone();
        full["tab_width"] = json!(2);
        full["ui_scale"] = json!(2.0);
        assert_eq!(distribute(&mut st, &full), (true, true, false));
        assert_eq!(st.project, json!({"tab_width": 2}), "the project file is created with the key");
        assert_eq!(st.user["ui_scale"], json!(2.0), "global-only keys always go to the user file");
        assert_eq!(st.user["tab_width"], json!(4));
        st.target = Some(Scope::User);
        full["format_on_save"] = json!(true);
        distribute(&mut st, &full);
        assert_eq!(st.user["format_on_save"], json!(true));
        assert!(st.project.get("format_on_save").is_none());
    }

    #[test]
    fn object_settings_only_write_what_changed() {
        let mut st = state(
            json!({"language_servers": {"rust": {"command": "ra"}}}),
            json!({"language_servers": {"pyright": {"enabled": false}}}),
            Value::Null,
        );
        let mut full = st.loaded.clone();
        full["language_servers"]["pyright"]["enabled"] = json!(true);
        distribute(&mut st, &full);
        assert_eq!(
            st.project,
            json!({"language_servers": {"pyright": {"enabled": true}}}),
            "user's rust entry not copied"
        );
        assert_eq!(st.user["language_servers"], json!({"rust": {"command": "ra"}}));
    }

    #[test]
    fn layered_save_writes_the_files() {
        let d = tmp("save");
        let root = d.join("proj");
        let user_path = d.join("cfg/config.json");
        let user = Config::default().to_json();
        std::fs::create_dir_all(root.join(DIR)).unwrap();
        std::fs::write(project_file(&root), r#"{"tab_width": 2}"#).unwrap();
        let (project, err) = read_layer(&project_file(&root));
        assert!(err.is_none());
        let merged = merge(&user, &[&project], true);
        let mut c = Config::from_value(&merged);
        let loaded = c.to_json();
        c.layers.set(Some(LayerState { root: root.clone(), user, project, loaded, ..Default::default() }));
        assert!(c.layers.project_active());
        assert_eq!(c.tab_width, 2);
        c.tab_width = 8;
        c.word_wrap = true;
        assert!(c.layers.save(&c, &user_path));
        assert_eq!(Config::load_from(&user_path).tab_width, 4, "the project's key stays out of the user file");
        assert!(Config::load_from(&user_path).word_wrap);
        let (p, _) = read_layer(&project_file(&root));
        assert_eq!(p, json!({"tab_width": 8}));
        assert!(!local_file(&root).exists());
        // Muted (a reload re-applying values): nothing is written.
        c.layers.with(|s| s.muted = true);
        c.tab_width = 2;
        assert!(c.layers.save(&c, &user_path));
        assert_eq!(read_layer(&project_file(&root)).0, json!({"tab_width": 8}));
        // Plain configs have no layers.
        assert!(!Config::default().layers.save(&Config::default(), &user_path));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn layer_files_are_read_leniently() {
        let d = tmp("read");
        let f = d.join("c.json");
        assert_eq!(read_layer(&f), (Value::Null, None), "missing");
        std::fs::write(&f, "  \n").unwrap();
        assert_eq!(read_layer(&f), (Value::Null, None), "empty");
        std::fs::write(&f, "{ nope").unwrap();
        assert!(read_layer(&f).1.is_some(), "broken JSON is reported");
        std::fs::write(&f, "[1]").unwrap();
        assert!(read_layer(&f).1.unwrap().contains("object"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn layer_lookup_and_summary() {
        let p = json!({"tab_width": 2, "ui_scale": 2, "language_servers": {"ruff": {"enabled": false}}});
        assert!(layer_has(&p, "tab_width") && layer_has(&p, "language_servers.ruff.enabled"));
        assert!(!layer_has(&p, "word_wrap") && !layer_has(&p, "language_servers.pyright.enabled"));
        let st = LayerState {
            project: p,
            local: json!({"theme": "x"}),
            untrusted: vec!["build_command: make".into()],
            ..Default::default()
        };
        let s = summary(&st);
        assert!(s[0].starts_with(".lumen/config.json: ") && s[0].contains("tab_width"), "{s:?}");
        assert_eq!(s[1], ".lumen/config.local.json: theme");
        assert!(s[2].contains("ui_scale"));
        assert!(s[3].contains("build_command: make"));
        assert!(st.project_active());
        assert!(!LayerState::default().project_active());
    }

    #[test]
    fn config_json_roundtrip_keeps_languages() {
        let c = Config { languages: json!({"rust": {"tab_width": 4}}), tab_width: 2, ..Config::default() };
        assert_eq!(Config::from_value(&c.to_json()), c);
        assert!(Config::default().to_json().get("languages").is_none(), "absent when unset");
        assert_eq!(Config::from_value(&json!({"languages": 3})).languages, Value::Null);
    }
}
