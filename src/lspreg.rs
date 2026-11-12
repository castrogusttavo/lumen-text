//! Language-server registry: which servers exist, which files they serve and how to find them.
//!
//! The built-in table is `lsp_servers.json` (embedded in the binary). The user's `language_servers`
//! object in `config.json` is merged on top of it and validated; problems become friendly messages
//! (`Registry::errors`), never a crash. See `web/src/content/docs/language-servers.md` for the schema.
//!
//! This module is pure data and filesystem probing: no processes, no UI.

use serde_json::{Map, Value, json};
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
    "csharp",
    "fsharp",
    "kotlin",
    "scala",
    "dart",
    "elixir",
    "haskell",
    "ocaml",
    "docker",
    "make",
    "cmake",
    "terraform",
    "astro",
    "less",
    "xml",
    "objective-c",
    "objective-cpp",
    "perl",
    "r",
    "julia",
    "nix",
    "graphql",
    "prisma",
];

/// Built-in spell checkers (companions for prose in comments, strings, Markdown, plain text and
/// commit messages). They are off unless the `spell_check` setting is on (see [`Registry::with_options`]).
pub const SPELL_SERVERS: &[&str] = &["harper-ls", "typos-lsp"];

pub fn is_spell_server(id: &str) -> bool {
    SPELL_SERVERS.contains(&id)
}

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

    /// Does this server handle a document of language `lang` / file extension `ext`? A language
    /// `"*"` matches every document (GitHub Copilot's inline completions).
    pub fn applies(&self, lang: Option<&str>, ext: &str) -> bool {
        if self.languages.iter().any(|l| l == "*") {
            return true;
        }
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
        let mut env: Vec<(String, String)> = self.env.iter().map(|(k, v)| (k.clone(), subst(v, root))).collect();
        // Python tools (ruff, pylsp plugins) find the project's packages through the venv.
        if self.is_python()
            && !env.iter().any(|(k, _)| k == "VIRTUAL_ENV")
            && let Some(venv) = crate::pyenv::active(root).and_then(|i| i.venv)
        {
            env.push(("VIRTUAL_ENV".into(), venv.to_string_lossy().into_owned()));
        }
        env
    }

    pub fn init_options_for(&self, root: &Path) -> Value {
        let mut v = subst_value(&self.initialization_options, root);
        if self.is_pyright()
            && (v.is_null() || v.is_object())
            && v.pointer("/python/pythonPath").is_none()
            && let Some(py) = project_python(root)
        {
            if v.is_null() {
                v = json!({});
            }
            v["python"]["pythonPath"] = json!(py.to_string_lossy());
        }
        v
    }

    /// Does it serve Python?
    pub fn is_python(&self) -> bool {
        self.languages.iter().any(|l| l == "python")
    }

    /// pyright or basedpyright (they read `python.pythonPath`, `python.venvPath` and `python.venv`).
    fn is_pyright(&self) -> bool {
        self.is_python() && self.command.contains("pyright")
    }

    /// The executable for a project: a Python server installed in the project's venv (ruff,
    /// basedpyright pinned in the dev dependencies) wins over one on the `PATH`.
    pub fn resolve_for(&self, root: &Path) -> Option<PathBuf> {
        let bare = !self.command.contains('/') && !self.command.contains('\\');
        if self.is_python()
            && bare
            && let Some(venv) = crate::pyenv::active(root).and_then(|i| i.venv)
            && let Some(p) =
                candidates(crate::pyenv::venv_bin(&venv).join(&self.command)).into_iter().find(|p| is_exec(p))
        {
            return Some(p);
        }
        resolve_command(&self.command, root)
    }

    pub fn settings_for(&self, root: &Path) -> Value {
        let mut v = subst_value(&self.settings, root);
        // Python servers do not look for the project's virtualenv themselves (VS Code's Python
        // extension tells them): without it every installed dependency is "could not be resolved".
        if self.is_python()
            && let Some(interp) = crate::pyenv::active(root)
        {
            let py = json!(interp.python.to_string_lossy());
            if !v.is_object() {
                v = json!({});
            }
            if v.pointer("/python/pythonPath").is_none() {
                v["python"]["pythonPath"] = py.clone();
                // pyright's `venvPath` + `venv` pair (the folder holding the venv, and its name).
                if let Some(venv) = &interp.venv
                    && let (Some(parent), Some(name)) = (venv.parent(), venv.file_name())
                    && v.pointer("/python/venvPath").is_none()
                {
                    v["python"]["venvPath"] = json!(parent.to_string_lossy());
                    v["python"]["venv"] = json!(name.to_string_lossy());
                }
            }
            if self.id == "pylsp" && v.pointer("/pylsp/plugins/jedi/environment").is_none() {
                v["pylsp"]["plugins"]["jedi"]["environment"] = py;
            }
        }
        v
    }

    /// Is a `requires` condition satisfied for a file in `dir`?
    pub fn requirement_met(&self, dir: &Path) -> bool {
        self.requires.is_empty() || dir.ancestors().any(|a| self.requires.iter().any(|r| marker_in(a, r)))
    }

    /// Workspace root for a file: the nearest ancestor holding a root marker, else the file's directory.
    pub fn root_for(&self, file: &Path) -> PathBuf {
        let dir = file.parent().unwrap_or(Path::new("/"));
        // The home folder is never the workspace of a file below it: a stray `~/package.json` or
        // `~/.git` would make a server index the whole home for `~/Downloads/x.css`.
        let home = home_dir().map(PathBuf::from);
        for a in dir.ancestors() {
            if a != dir && home.as_deref() == Some(a) {
                break;
            }
            if self.root_markers.iter().any(|m| marker_in(a, m)) {
                return a.to_path_buf();
            }
        }
        dir.to_path_buf()
    }
}

/// The interpreter the project's Python servers get: the one picked with "Python: Select
/// interpreter…", else the project's virtualenv (see `pyenv`).
pub fn project_python(root: &Path) -> Option<PathBuf> {
    crate::pyenv::active(root).map(|i| i.python)
}

/// Does `dir` hold `marker`? A marker is a relative path, or `*.ext` for "any file with that
/// extension directly in `dir`" (C# solutions and projects have per-project names: `App.csproj`).
fn marker_in(dir: &Path, marker: &str) -> bool {
    match marker.strip_prefix("*.") {
        Some(ext) if !ext.contains('/') => std::fs::read_dir(dir)
            .is_ok_and(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case(ext)))),
        _ => dir.join(marker).exists(),
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
        // Editor (grammar) ids that differ from the LSP spec's well-known language identifiers.
        Some("docker") => "dockerfile".into(),
        Some("make") => "makefile".into(),
        Some("shellscript") => "shellscript".into(),
        Some(l) => l.to_string(),
        None if ext.is_empty() || ext == "txt" || ext == "text" => "plaintext".into(),
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

    /// [`with_user`](Self::with_user) plus the `spell_check` setting: it turns the built-in spell
    /// checkers on (or keeps them off), unless the user's own entry for one says `enabled`.
    pub fn with_options(user: &Value, spell_check: bool) -> Registry {
        let mut r = Registry::with_user(user);
        for s in r.servers.iter_mut().filter(|s| s.builtin && is_spell_server(&s.id)) {
            if user.get(&s.id).and_then(|e| e.get("enabled")).is_some() {
                continue;
            }
            s.enabled = spell_check;
            s.disabled_by = (!spell_check).then(|| "spell checking is off (setting spell_check)".to_string());
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

#[cfg(unix)]
fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Windows has no exec bit: a file counts when it exists with an executable extension.
#[cfg(not(unix))]
fn is_exec(p: &Path) -> bool {
    p.is_file()
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["exe", "cmd", "bat", "com"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// `base`, plus its `.exe`/`.cmd`/... variants on Windows (npm installs servers as `.cmd` shims).
fn candidates(base: PathBuf) -> Vec<PathBuf> {
    if cfg!(unix) {
        return vec![base];
    }
    let mut v = vec![base.clone()];
    for ext in ["exe", "cmd", "bat", "com"] {
        let mut s = base.clone().into_os_string();
        s.push(".");
        s.push(ext);
        v.push(PathBuf::from(s));
    }
    v
}

fn home_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
}

fn expand_home(s: &str) -> PathBuf {
    let rest = s.strip_prefix("~/").or_else(|| if cfg!(windows) { s.strip_prefix("~\\") } else { None });
    match (rest, home_dir()) {
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
    dirs.extend(user_tool_dirs());
    dirs
}

/// Per-user tool folders that are often missing from the PATH of an editor started from the
/// desktop: Lumen's own server folder, cargo, pipx/uv, go, .NET global tools, Ruby user gems.
fn user_tool_dirs() -> Vec<PathBuf> {
    let Some(h) = home_dir().map(PathBuf::from) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> =
        [".local/share/lumen-text/lsp/bin", ".cargo/bin", ".local/bin", "go/bin", ".dotnet/tools"]
            .iter()
            .map(|r| h.join(r))
            .collect();
    // `gem install --user-install` (the default on Arch, Fedora...) puts executables in a folder per Ruby version.
    for base in [h.join(".local/share/gem/ruby"), h.join(".gem/ruby")] {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&base)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path().join("bin"))
            .filter(|p| p.is_dir())
            .collect();
        v.sort();
        dirs.extend(v.into_iter().rev());
    }
    dirs
}

/// `PATH` for a server process: the folder of its executable (and of the symlink target, so a
/// server installed in a venv or node_modules finds its siblings), the per-user tool folders, then
/// the editor's own `PATH`. Servers run helpers (`bundle`, `zig`, `go`, `node`, `dotnet`) that an
/// editor launched from the desktop menu would otherwise not find.
pub fn child_path(exe: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |d: PathBuf| {
        if !d.as_os_str().is_empty() && !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    if let Some(d) = exe.parent() {
        push(d.to_path_buf());
    }
    if let Some(d) = crate::paths::canonical(exe).ok().as_deref().and_then(Path::parent) {
        push(d.to_path_buf());
    }
    for d in user_tool_dirs().into_iter().filter(|d| d.is_dir()) {
        push(d);
    }
    for d in std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default() {
        push(d);
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

/// Absolute path of the server's executable, or `None` when it is not installed.
pub fn resolve_command(command: &str, root: &Path) -> Option<PathBuf> {
    let is_path =
        command.contains('/') || (cfg!(windows) && (command.contains('\\') || Path::new(command).is_absolute()));
    if is_path {
        let p = expand_home(command);
        let p = if p.is_relative() { root.join(p) } else { p };
        return candidates(p).into_iter().find(|p| is_exec(p));
    }
    search_dirs(root).into_iter().flat_map(|d| candidates(d.join(command))).find(|p| is_exec(p))
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
    fn copilot_is_an_off_by_default_companion_for_every_file() {
        let r = Registry::builtin();
        let c = r.get("copilot").expect("copilot in the table");
        assert_eq!((c.role, c.enabled, c.args.clone()), (Role::Companion, false, vec!["--stdio".to_string()]));
        assert!(c.applies(Some("rust"), "rs") && c.applies(None, "") && c.applies(None, "weird"));
        assert!(c.init_options_for(Path::new("/p"))["editorInfo"]["name"].is_string());
        assert!(!r.for_doc(Some("rust"), Path::new("/x/a.rs")).iter().any(|s| s.id == "copilot"));
        let on = Registry::with_user(&json!({"copilot": {"enabled": true}}));
        let ids: Vec<String> = on.for_doc(None, Path::new("/x/notes.txt")).iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["copilot"]);
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
        #[cfg(unix)]
        {
            assert!(resolve_command("sh", Path::new("/")).is_some());
            assert!(resolve_command("/bin/sh", Path::new("/")).is_some());
        }
        #[cfg(windows)]
        {
            let cmd = PathBuf::from(std::env::var_os("ComSpec").expect("ComSpec"));
            assert!(resolve_command("cmd", Path::new("/")).is_some_and(|p| p.is_file()));
            assert_eq!(resolve_command(&cmd.to_string_lossy(), Path::new("/")).as_deref(), Some(cmd.as_path()));
            // An absolute path without the extension (`C:\Windows\System32\cmd`) finds the `.exe`.
            let bare = cmd.with_extension("");
            assert!(resolve_command(&bare.to_string_lossy(), Path::new("/")).is_some_and(|p| p.is_file()));
        }
    }

    /// npm installs a server as `node_modules/.bin/<name>.cmd` on Windows (`<name>` alone is a
    /// shell script Windows can't run): the shim is what gets started.
    #[cfg(windows)]
    #[test]
    fn npm_cmd_shims_are_found_on_windows() {
        let base = std::env::temp_dir().join(format!("lumen-reg-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let bin = base.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("lumen-fake-ls"), "#!/bin/sh\n").unwrap();
        std::fs::write(bin.join("lumen-fake-ls.cmd"), "@echo off\r\n").unwrap();
        assert_eq!(resolve_command("lumen-fake-ls", &base), Some(bin.join("lumen-fake-ls.cmd")));
        assert_eq!(
            resolve_command("node_modules/.bin/lumen-fake-ls", &base),
            Some(base.join("node_modules/.bin/lumen-fake-ls.cmd"))
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_home_folder_is_not_a_workspace_for_files_below_it() {
        let Some(home) = home_dir().map(PathBuf::from) else { return };
        let r = Registry::builtin();
        let json = r.get("vscode-json-language-server").unwrap();
        // Whatever markers the real home holds, a file in a sub-folder roots at that folder.
        let f = home.join("lumen-no-such-dir-xyz/a.json");
        assert_eq!(json.root_for(&f), home.join("lumen-no-such-dir-xyz"));
    }

    #[test]
    fn glob_root_markers_find_per_project_names() {
        let base = std::env::temp_dir().join(format!("lumen-reg-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("App/src")).unwrap();
        std::fs::write(base.join("App/App.csproj"), "<Project/>").unwrap();
        let r = Registry::builtin();
        let cs = r.get("csharp-ls").unwrap();
        assert_eq!(cs.root_for(&base.join("App/src/Program.cs")), base.join("App"));
        assert!(!marker_in(&base.join("App/src"), "*.csproj"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn python_servers_get_the_project_virtualenv() {
        let base = std::env::temp_dir().join(format!("lumen-reg-venv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // Where `python -m venv .venv` puts the interpreter on this OS.
        let venv = base.join(".venv");
        let py = if cfg!(windows) { venv.join("Scripts").join("python.exe") } else { venv.join("bin").join("python") };
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        std::fs::write(&py, "").unwrap();
        let r = Registry::builtin();
        assert_eq!(project_python(&base).as_deref(), Some(py.as_path()));
        let py = py.to_string_lossy().into_owned();
        assert_eq!(r.get("pyright").unwrap().settings_for(&base)["python"]["pythonPath"], json!(py));
        assert_eq!(r.get("pylsp").unwrap().settings_for(&base)["pylsp"]["plugins"]["jedi"]["environment"], json!(py));
        assert!(r.get("gopls").unwrap().settings_for(&base).pointer("/python").is_none());
        // pyright's venvPath + venv pair, also in the initialization options.
        let pr = r.get("pyright").unwrap();
        let s = pr.settings_for(&base);
        assert_eq!(s["python"]["venvPath"], json!(base.to_string_lossy()));
        assert_eq!(s["python"]["venv"], json!(".venv"));
        assert_eq!(pr.init_options_for(&base)["python"]["pythonPath"], json!(py));
        assert!(r.get("ruff").unwrap().init_options_for(&base).pointer("/python").is_none(), "only pyright's");
        // Every Python server runs with the venv activated; others do not.
        let venv_s = venv.to_string_lossy().into_owned();
        assert!(r.get("ruff").unwrap().env_for(&base).contains(&("VIRTUAL_ENV".to_string(), venv_s)));
        assert!(!r.get("gopls").unwrap().env_for(&base).iter().any(|(k, _)| k == "VIRTUAL_ENV"));
        // A server installed in the venv wins over the PATH.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ruff = venv.join("bin/ruff");
            std::fs::write(&ruff, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&ruff, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(r.get("ruff").unwrap().resolve_for(&base), Some(ruff));
        }
        // The user's own setting is never overwritten.
        let mut custom = pr.clone();
        custom.settings = json!({"python": {"pythonPath": "/opt/py"}});
        assert_eq!(custom.settings_for(&base)["python"], json!({"pythonPath": "/opt/py"}));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn spell_checkers_follow_the_spell_check_setting() {
        let ids = |r: &Registry, lang, p: &str| {
            r.for_doc(lang, Path::new(p)).iter().map(|s| s.id.clone()).collect::<Vec<_>>()
        };
        let off = Registry::with_options(&Value::Null, false);
        assert!(off.errors.is_empty());
        for id in SPELL_SERVERS {
            let s = off.get(id).unwrap();
            assert_eq!(s.role, Role::Companion, "{id}");
            assert!(!s.enabled && s.disabled_by.as_deref().is_some_and(|w| w.contains("spell_check")), "{id}");
        }
        assert_eq!(ids(&off, Some("rust"), "/x/a.rs"), vec!["rust-analyzer"]);
        let on = Registry::with_options(&Value::Null, true);
        assert_eq!(ids(&on, Some("rust"), "/x/a.rs"), vec!["rust-analyzer", "harper-ls", "typos-lsp"]);
        assert_eq!(ids(&on, Some("markdown"), "/x/README.md"), vec!["marksman", "harper-ls", "typos-lsp"]);
        assert_eq!(ids(&on, None, "/x/notes.txt"), vec!["harper-ls", "typos-lsp"]);
        let msg = "/x/repo/COMMIT_EDITMSG";
        let commit = crate::highlight::language_for(msg);
        assert_eq!(commit, Some("git-commit"));
        assert_eq!(ids(&on, commit, msg), vec!["harper-ls", "typos-lsp"]);
        // The user's own `enabled` wins over the setting, both ways.
        let user = json!({"typos-lsp": {"enabled": false}, "harper-ls": {"enabled": true}});
        assert_eq!(ids(&Registry::with_options(&user, true), None, "/x/a.txt"), vec!["harper-ls"]);
        assert_eq!(ids(&Registry::with_options(&user, false), None, "/x/a.txt"), vec!["harper-ls"]);
        assert!(is_spell_server("harper-ls") && !is_spell_server("ruff"));
    }

    #[test]
    fn language_ids_use_the_lsp_spelling() {
        assert_eq!(language_id(None, Some(Path::new("/p/notes.txt"))), "plaintext");
        assert_eq!(language_id(None, Some(Path::new("/p/LICENSE"))), "plaintext");
        assert_eq!(language_id(Some("git-commit"), Some(Path::new("/p/COMMIT_EDITMSG"))), "git-commit");
        assert_eq!(language_id(Some("docker"), Some(Path::new("/p/Dockerfile"))), "dockerfile");
        assert_eq!(language_id(Some("make"), Some(Path::new("/p/Makefile"))), "makefile");
        assert_eq!(language_id(Some("csharp"), Some(Path::new("/p/a.cs"))), "csharp");
        assert_eq!(language_id(Some("kotlin"), Some(Path::new("/p/a.kt"))), "kotlin");
    }

    #[test]
    fn every_builtin_language_is_one_the_editor_assigns() {
        // A server for a language `highlight::language_for` never returns would never start.
        let assigned: Vec<&str> = [
            "a.ts",
            "a.tsx",
            "a.js",
            "a.jsx",
            "a.json",
            "a.rs",
            "a.py",
            "a.go",
            "a.md",
            "a.html",
            "a.css",
            "a.scss",
            "a.less",
            "a.sh",
            "a.toml",
            "a.yaml",
            "a.c",
            "a.cpp",
            "a.java",
            "a.kt",
            "a.cs",
            "a.swift",
            "a.m",
            "a.mm",
            "a.dart",
            "a.ex",
            "a.hs",
            "a.scala",
            "a.tf",
            "a.lua",
            "a.rb",
            "a.php",
            "a.zig",
            "a.vue",
            "a.svelte",
            "a.astro",
            "Dockerfile",
        ]
        .iter()
        .filter_map(|f| crate::highlight::language_for(f))
        .collect();
        for s in &Registry::builtin().servers {
            assert!(
                s.languages.iter().any(|l| l == "*" || assigned.contains(&l.as_str())),
                "{}: {:?}",
                s.id,
                s.languages
            );
        }
    }

    #[test]
    fn server_path_starts_with_the_executable_folder() {
        let p = child_path(Path::new("/opt/srv/bin/server"));
        let dirs: Vec<PathBuf> = std::env::split_paths(&p).collect();
        assert_eq!(dirs[0], PathBuf::from("/opt/srv/bin"));
        if let Some(path) = std::env::var_os("PATH") {
            for d in std::env::split_paths(&path).filter(|d| !d.as_os_str().is_empty()) {
                assert!(dirs.contains(&d), "the editor's PATH is kept: {}", d.display());
            }
        }
    }
}
