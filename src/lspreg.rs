//! Language-server registry: which servers exist, which files they serve and how to find them.
//!
//! The built-in table is `lsp_servers.json` (embedded in the binary). The user's `language_servers`
//! object in `config.json` is merged on top of it and validated; problems become friendly messages
//! (`Registry::errors`), never a crash. See `web/src/content/docs/language-servers.md` for the schema.
//!
//! This module is pure data and filesystem probing: no processes, no UI.

use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const BUILTIN: &str = include_str!("lsp_servers.json");

/// Language ids the editor itself assigns (see `highlight::language_for`); a config key with one of
/// these names configures "the server of that language" instead of defining a new server.
const EDITOR_LANGS: &[&str] = &[
    "typescript",
    "tsx",
    "javascript",
    "jsx",
    "json",
    "rust",
    "python",
    "go",
    "markdown",
    "html",
    "css",
    "scss",
    "shellscript",
    "toml",
    "yaml",
    "c",
    "cpp",
    "java",
    "swift",
    "lua",
    "ruby",
    "php",
    "sql",
    "vue",
    "svelte",
    "zig",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Serves the language: completion, hover, definition... come from it (first installed one wins).
    Primary,
    /// Runs next to the primary (tailwind, ruff): its diagnostics, code actions, hover and completion are merged in.
    Companion,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerSpec {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub root_markers: Vec<String>,
    /// Editor language ids and/or file extensions (without the dot) this server handles.
    pub languages: Vec<String>,
    /// Extra file extensions on top of `languages`.
    pub extensions: Vec<String>,
    pub initialization_options: Value,
    pub settings: Value,
    pub enabled: bool,
    pub role: Role,
    /// Any-of: files (relative to the project root or an ancestor) that must exist for the server to apply.
    pub requires: Vec<String>,
    pub install: String,
    pub builtin: bool,
    /// Why it is off when it was switched off by something other than `enabled: false` on itself.
    pub disabled_by: Option<String>,
}

impl ServerSpec {
    fn blank(id: &str) -> ServerSpec {
        ServerSpec {
            id: id.to_string(),
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            root_markers: Vec::new(),
            languages: Vec::new(),
            extensions: Vec::new(),
            initialization_options: Value::Null,
            settings: Value::Null,
            enabled: true,
            role: Role::Primary,
            requires: Vec::new(),
            install: String::new(),
            builtin: false,
            disabled_by: None,
        }
    }

    /// Does this server handle a document of language `lang` / file extension `ext`?
    pub fn applies(&self, lang: Option<&str>, ext: &str) -> bool {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        let hit = |s: &String| {
            let s = s.trim_start_matches('.');
            lang.is_some_and(|l| l.eq_ignore_ascii_case(s)) || (!ext.is_empty() && ext.eq_ignore_ascii_case(s))
        };
        self.languages.iter().any(hit) || self.extensions.iter().any(hit)
    }

    /// The command line with `${root}` filled in.
    pub fn args_for(&self, root: &Path) -> Vec<String> {
        self.args.iter().map(|a| subst(a, root)).collect()
    }

    pub fn env_for(&self, root: &Path) -> Vec<(String, String)> {
        self.env.iter().map(|(k, v)| (k.clone(), subst(v, root))).collect()
    }

    pub fn init_options_for(&self, root: &Path) -> Value {
        subst_value(&self.initialization_options, root)
    }

    pub fn settings_for(&self, root: &Path) -> Value {
        subst_value(&self.settings, root)
    }

    /// Is a `requires` condition satisfied for a file in `dir`?
    pub fn requirement_met(&self, dir: &Path) -> bool {
        self.requires.is_empty() || dir.ancestors().any(|a| self.requires.iter().any(|r| a.join(r).exists()))
    }

    /// Workspace root for a file: the nearest ancestor holding a root marker, else the file's directory.
    pub fn root_for(&self, file: &Path) -> PathBuf {
        let dir = file.parent().unwrap_or(Path::new("/"));
        for a in dir.ancestors() {
            if self.root_markers.iter().any(|m| a.join(m).exists()) {
                return a.to_path_buf();
            }
        }
        dir.to_path_buf()
    }
}

/// The LSP `languageId` for a document: the editor's language id (TS/JS in the LSP spelling), or the
/// file extension for languages the editor has no id for (`.astro`).
pub fn language_id(lang: Option<&str>, path: Option<&Path>) -> String {
    let ext = path.and_then(|p| p.extension()).and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match lang {
        Some("tsx") => "typescriptreact".into(),
        Some("jsx") => "javascriptreact".into(),
        Some("json") if ext == "jsonc" => "jsonc".into(),
        Some(l) => l.to_string(),
        None if ext.is_empty() => "plaintext".into(),
        None => ext,
    }
}

fn subst(s: &str, root: &Path) -> String {
    s.replace("${root}", &root.to_string_lossy())
}

fn subst_value(v: &Value, root: &Path) -> Value {
    match v {
        Value::String(s) => Value::String(subst(s, root)),
        Value::Array(a) => Value::Array(a.iter().map(|x| subst_value(x, root)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), subst_value(x, root))).collect()),
        x => x.clone(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub servers: Vec<ServerSpec>,
    /// Problems found in the user's `language_servers` (friendly, one line each).
    pub errors: Vec<String>,
}

fn str_list(v: &Value, what: &str, errs: &mut Vec<String>) -> Option<Vec<String>> {
    let Some(a) = v.as_array() else {
        errs.push(format!("{what} must be an array of strings"));
        return None;
    };
    let mut out = Vec::new();
    for x in a {
        match x.as_str() {
            Some(s) => out.push(s.to_string()),
            None => {
                errs.push(format!("{what} must be an array of strings"));
                return None;
            }
        }
    }
    Some(out)
}

/// Apply the fields of one config entry onto `spec`; invalid fields are reported and skipped.
fn apply_fields(spec: &mut ServerSpec, entry: &Map<String, Value>, key: &str, errs: &mut Vec<String>) {
    for (k, v) in entry {
        let what = format!("language_servers.{key}.{k}");
        match k.as_str() {
            "command" => match v.as_str().map(str::trim) {
                Some(s) if !s.is_empty() => spec.command = s.to_string(),
                _ => errs.push(format!("{what} must be a non-empty string")),
            },
            "args" => {
                if let Some(l) = str_list(v, &what, errs) {
                    spec.args = l;
                }
            }
            "env" => match v.as_object() {
                Some(o) if o.values().all(Value::is_string) => {
                    spec.env = o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect();
                }
                _ => errs.push(format!("{what} must be an object of strings")),
            },
            "root_markers" => {
                if let Some(l) = str_list(v, &what, errs) {
                    spec.root_markers = l;
                }
            }
            "languages" => {
                if let Some(l) = str_list(v, &what, errs) {
                    spec.languages = l;
                }
            }
            "extensions" => {
                if let Some(l) = str_list(v, &what, errs) {
                    spec.extensions = l;
                }
            }
            "requires" => {
                if let Some(l) = str_list(v, &what, errs) {
                    spec.requires = l;
                }
            }
            "initialization_options" => spec.initialization_options = v.clone(),
            "settings" => spec.settings = v.clone(),
            "enabled" => match v.as_bool() {
                Some(b) => spec.enabled = b,
                None => errs.push(format!("{what} must be true or false")),
            },
            "role" => match v.as_str() {
                Some("primary") => spec.role = Role::Primary,
                Some("companion") => spec.role = Role::Companion,
                _ => errs.push(format!("{what} must be \"primary\" or \"companion\"")),
            },
            "install" => {
                if let Some(s) = v.as_str() {
                    spec.install = s.to_string();
                }
            }
            _ => errs.push(format!("{what}: unknown field")),
        }
    }
}

impl Registry {
    pub fn builtin() -> Registry {
        let mut r = Registry::default();
        let v: Value = serde_json::from_str(BUILTIN).unwrap_or(Value::Null);
        for e in v["servers"].as_array().into_iter().flatten() {
            let Some(id) = e["id"].as_str() else { continue };
            let mut spec = ServerSpec::blank(id);
            spec.builtin = true;
            if let Some(e) = e.as_object() {
                let mut ignore = Vec::new();
                let fields: Map<String, Value> =
                    e.iter().filter(|(k, _)| *k != "id").map(|(k, v)| (k.clone(), v.clone())).collect();
                apply_fields(&mut spec, &fields, id, &mut ignore);
                debug_assert!(ignore.is_empty(), "bad built-in table: {ignore:?}");
            }
            r.servers.push(spec);
        }
        r
    }

    /// The built-in table with the user's `language_servers` object merged in (and validated).
    pub fn with_user(user: &Value) -> Registry {
        let mut r = Registry::builtin();
        match user {
            Value::Null => {}
            Value::Object(o) => {
                for (key, entry) in o {
                    r.apply_user(key, entry);
                }
            }
            _ => r.errors.push("language_servers must be an object".to_string()),
        }
        r
    }

    fn knows_language(&self, key: &str) -> bool {
        EDITOR_LANGS.contains(&key) || self.servers.iter().any(|s| s.languages.iter().any(|l| l == key))
    }

    fn apply_user(&mut self, key: &str, entry: &Value) {
        let Some(e) = entry.as_object() else {
            self.errors.push(format!("language_servers.{key} must be an object, e.g. {{\"command\": \"my-server\"}}"));
            return;
        };
        let mut errs = Vec::new();
        if let Some(i) = self.servers.iter().position(|s| s.id == key) {
            // A known server (built-in or defined earlier): patch it.
            apply_fields(&mut self.servers[i], e, key, &mut errs);
        } else if self.knows_language(key) {
            // A language: switch its primaries off, or replace them by the user's own command.
            if let Some(Value::String(cmd)) = e.get("command")
                && !cmd.trim().is_empty()
            {
                let mut spec = ServerSpec::blank(key);
                spec.root_markers = vec![".git".into()];
                spec.languages = vec![key.to_string()];
                apply_fields(&mut spec, e, key, &mut errs);
                for s in
                    self.servers.iter_mut().filter(|s| s.role == Role::Primary && s.languages.iter().any(|l| l == key))
                {
                    s.enabled = false;
                    s.disabled_by = Some(format!("language_servers.{key} replaces it"));
                }
                self.servers.insert(0, spec);
            } else {
                for s in
                    self.servers.iter_mut().filter(|s| s.role == Role::Primary && s.languages.iter().any(|l| l == key))
                {
                    let mut local = Vec::new();
                    apply_fields(s, e, key, &mut local);
                    if errs.is_empty() {
                        errs = local;
                    }
                }
            }
        } else if e.get("command").is_some() {
            let mut spec = ServerSpec::blank(key);
            spec.root_markers = vec![".git".into()];
            spec.languages = vec![key.to_string()];
            apply_fields(&mut spec, e, key, &mut errs);
            if spec.command.is_empty() {
                errs.push(format!("language_servers.{key}: \"command\" is required"));
            } else {
                self.servers.insert(0, spec);
            }
        } else {
            errs.push(format!(
                "language_servers.{key}: not a known server or language (to define a server, give it a \"command\")"
            ));
        }
        self.errors.extend(errs);
    }

    pub fn get(&self, id: &str) -> Option<&ServerSpec> {
        self.servers.iter().find(|s| s.id == id)
    }

    /// Enabled servers of a document, in priority order (primaries first, then companions).
    /// `dir` is the directory of the file: servers whose `requires` is not met are skipped.
    pub fn for_doc(&self, lang: Option<&str>, path: &Path) -> Vec<&ServerSpec> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let dir = path.parent().unwrap_or(Path::new("/"));
        let mut v: Vec<&ServerSpec> =
            self.servers.iter().filter(|s| s.enabled && s.applies(lang, ext) && s.requirement_met(dir)).collect();
        v.sort_by_key(|s| s.role == Role::Companion); // stable: table order inside each role
        v
    }
}

// ───────────────────────────── finding executables ─────────────────────────────

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn expand_home(s: &str) -> PathBuf {
    match (s.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => Path::new(&h).join(rest),
        _ => PathBuf::from(s),
    }
}

/// Where a server's executable would be searched, in order (for the "not installed" tooltip too).
pub fn search_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = root.ancestors().map(|d| d.join("node_modules/.bin")).collect();
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    if let Some(h) = std::env::var_os("HOME") {
        let h = PathBuf::from(h);
        for rel in [".local/share/lumen-text/lsp/bin", ".cargo/bin", ".local/bin", "go/bin"] {
            dirs.push(h.join(rel));
        }
    }
    dirs
}

/// Absolute path of the server's executable, or `None` when it is not installed.
pub fn resolve_command(command: &str, root: &Path) -> Option<PathBuf> {
    if command.contains('/') {
        let p = expand_home(command);
        let p = if p.is_relative() { root.join(p) } else { p };
        return is_exec(&p).then_some(p);
    }
    search_dirs(root).into_iter().map(|d| d.join(command)).find(|p| is_exec(p))
}

/// Is `command` available when there is no particular project (status listings)?
pub fn installed_anywhere(command: &str) -> Option<PathBuf> {
    resolve_command(command, Path::new("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtin_table_parses_and_has_the_popular_servers() {
        let r = Registry::builtin();
        assert!(r.errors.is_empty());
        for id in [
            "rust-analyzer",
            "pyright",
            "basedpyright",
            "pylsp",
            "ruff",
            "gopls",
            "clangd",
            "lua-language-server",
            "taplo",
            "vscode-json-language-server",
            "vscode-css-language-server",
            "vscode-html-language-server",
            "yaml-language-server",
            "bash-language-server",
            "marksman",
            "zls",
            "ruby-lsp",
            "solargraph",
            "phpactor",
            "intelephense",
            "svelte",
            "astro",
            "vue",
            "tailwindcss",
            "deno",
        ] {
            let s = r.get(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!(!s.command.is_empty() && !s.languages.is_empty(), "{id}");
        }
        assert_eq!(r.get("tailwindcss").unwrap().role, Role::Companion);
    }

    #[test]
    fn matching_is_by_language_or_extension() {
        let r = Registry::builtin();
        let ids = |lang, p: &str| r.for_doc(lang, Path::new(p)).iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(Some("rust"), "/x/a.rs"), vec!["rust-analyzer"]);
        assert_eq!(ids(Some("python"), "/x/a.py"), vec!["pyright", "basedpyright", "pylsp", "ruff"]);
        // extension only: the editor has no language id for `.astro` files
        assert_eq!(ids(None, "/x/a.astro"), vec!["astro"]);
        assert_eq!(ids(Some("cpp"), "/x/a.hpp"), vec!["clangd"]);
        assert!(ids(Some("typescript"), "/nonexistent-project/a.ts").is_empty(), "tailwind/deno need their files");
    }

    #[test]
    fn requires_and_root_markers_walk_up() {
        let dir = std::env::temp_dir().join(format!("lumen-reg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join("deno.json"), "{}").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        let r = Registry::builtin();
        let f = dir.join("src/deep/a.ts");
        let ids: Vec<_> = r.for_doc(Some("typescript"), &f).iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["deno"]);
        assert_eq!(r.get("deno").unwrap().root_for(&f), dir);
        assert_eq!(r.get("rust-analyzer").unwrap().root_for(&dir.join("src/deep/a.rs")), dir);
        // no marker anywhere: the file's own directory
        assert_eq!(r.get("zls").unwrap().root_for(Path::new("/nonexistent/x/a.zig")), Path::new("/nonexistent/x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_config_patches_replaces_disables_and_adds() {
        let r = Registry::with_user(&json!({
            "rust-analyzer": {"args": ["--log-file", "/tmp/x"], "env": {"RA_LOG": "info"}},
            "python": {"enabled": false},
            "go": {"command": "/opt/my-gopls", "args": ["serve"]},
            "my-server": {"command": "mysrv", "languages": ["foo"]},
            "taplo": {"enabled": false},
        }));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.get("rust-analyzer").unwrap().args, vec!["--log-file", "/tmp/x"]);
        assert_eq!(r.get("rust-analyzer").unwrap().env["RA_LOG"], "info");
        assert!(!r.get("pyright").unwrap().enabled && !r.get("pylsp").unwrap().enabled);
        assert!(r.get("ruff").unwrap().enabled, "companions are kept: name them to switch them off");
        let go = r.get("go").unwrap();
        assert_eq!((go.command.as_str(), go.languages.clone()), ("/opt/my-gopls", vec!["go".to_string()]));
        assert!(!r.get("gopls").unwrap().enabled, "a language override replaces the built-in");
        assert_eq!(r.get("my-server").unwrap().languages, vec!["foo"]);
        assert!(r.get("my-server").unwrap().applies(None, "foo"));
        assert!(!r.get("taplo").unwrap().enabled);
    }

    #[test]
    fn invalid_config_gives_friendly_errors_and_keeps_going() {
        let r = Registry::with_user(&json!({
            "rust-analyzer": {"args": "nope", "enabled": "yes", "colour": 1, "command": ""},
            "mystery": {"languages": ["x"]},
            "zls": 5,
            "svelte": {"enabled": false},
        }));
        let e = r.errors.join("\n");
        assert!(e.contains("language_servers.rust-analyzer.args must be an array of strings"), "{e}");
        assert!(e.contains("language_servers.rust-analyzer.enabled must be true or false"), "{e}");
        assert!(e.contains("language_servers.rust-analyzer.colour: unknown field"), "{e}");
        assert!(e.contains("language_servers.rust-analyzer.command must be a non-empty string"), "{e}");
        assert!(e.contains("language_servers.mystery: not a known server or language"), "{e}");
        assert!(e.contains("language_servers.zls must be an object"), "{e}");
        assert_eq!(r.errors.len(), 6, "{e}");
        assert!(!r.get("svelte").unwrap().enabled, "valid entries still apply");
        assert!(Registry::with_user(&json!([1])).errors[0].contains("must be an object"));
    }

    #[test]
    fn placeholders_and_command_resolution() {
        let r = Registry::builtin();
        let a = r.get("astro").unwrap();
        let o = a.init_options_for(Path::new("/p"));
        assert_eq!(o["typescript"]["tsdk"], "/p/node_modules/typescript/lib");
        assert!(resolve_command("definitely-not-a-real-server-xyz", Path::new("/")).is_none());
        assert!(resolve_command("sh", Path::new("/")).is_some());
        assert!(resolve_command("/bin/sh", Path::new("/")).is_some());
    }
}
