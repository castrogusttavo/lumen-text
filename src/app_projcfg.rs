//! App glue of per-project settings (`projcfg`) and Python interpreters (`pyenv`): loading and
//! live-reloading the layers, applying what changed, the one-time trust prompt for project
//! commands, the "User | Project" scope of the settings modal, the bottom bar indicator and its
//! tooltip, and the "Python: Select interpreter…" picker.

use crate::{
    app::App,
    app_overlay::{Action, Item, Overlay, choice_picker},
    app_tooltip::TipText,
    app_topbar::StatusBtn,
    config::Config,
    projcfg::{self, LayerState, Scope, TrustStore},
    pyenv,
    ui::{Canvas, Rect},
    watcher::FsEvent,
};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How long the interpreter shown in the language tooltip is reused.
const PY_CACHE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct ProjState {
    /// Fingerprint of the commands the trust prompt was shown for in this session (Esc leaves the
    /// question open: it comes back at the next start, not at every reload).
    asked: Option<String>,
    /// Last problem reported for a settings file (not repeated by every reload).
    told: Option<String>,
    /// Active interpreter for the language tooltip: (root, when, interpreter).
    py_cache: RefCell<Option<(PathBuf, Instant, Option<pyenv::Interp>)>>,
    /// Folder for the remembered answers (trust, interpreter); `None` = the state directory.
    pub state_dir: Option<PathBuf>,
}

/// What the trust prompt and the interpreter picker hand back.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjPick {
    Trust(bool),
    /// `None` = automatic (the project's venv).
    Interpreter(Option<PathBuf>),
}

const THEME_KEYS: &[&str] = &["theme", "theme_light", "theme_dark", "appearance"];

impl App {
    fn proj_state_file(&self, name: &str) -> Option<PathBuf> {
        match &self.proj.state_dir {
            Some(d) => Some(d.join(name)),
            None => crate::paths::state_home().map(|d| d.join("lumen-text").join(name)),
        }
    }

    fn trust_store(&self) -> Option<TrustStore> {
        self.proj_state_file("trusted-projects.json").map(|path| TrustStore { path })
    }

    /// The user layer behind the current settings (complete, as `Config::to_json` writes it).
    fn user_layer(&self) -> Value {
        self.config.layers.get().map(|s| s.user).filter(Value::is_object).unwrap_or_else(|| self.config.to_json())
    }

    /// Load the settings of the project at `self.root` (startup, project switch): its `.lumen/`
    /// files on top of the user's, and the interpreter picked for it.
    pub fn projcfg_init(&mut self) {
        let choice = self.proj_state_file("python-interpreters.json").and_then(|f| pyenv::load_choice(&f, &self.root));
        pyenv::set_choice(&self.root, choice);
        self.proj.py_cache.replace(None);
        self.proj.asked = None;
        let user = self.user_layer();
        self.projcfg_load(user);
    }

    /// A settings file changed: merge again and apply what differs. `reread_user` also reads the
    /// user's `config.json` again (else the user layer in memory is kept).
    pub fn projcfg_reload(&mut self, reread_user: bool) {
        let user = if reread_user { Config::load().to_json() } else { self.user_layer() };
        self.projcfg_load(user);
    }

    fn projcfg_load(&mut self, user: Value) {
        let root = self.root.clone();
        let (project, e1) = projcfg::read_layer(&projcfg::project_file(&root));
        let (local, e2) = projcfg::read_layer(&projcfg::local_file(&root));
        // Multi-root workspace: the other folders' `.lumen/config.json` sit under the primary
        // root's own files (which win), so each folder can set what the others leave alone.
        let extras: Vec<Value> =
            self.ws.extra.iter().map(|r| projcfg::read_layer(&projcfg::project_file(r)).0).collect();
        let mut cmds: Vec<String> = extras.iter().flat_map(projcfg::commands).collect();
        cmds.extend(projcfg::commands(&project));
        cmds.extend(projcfg::commands(&local));
        let fp = projcfg::fingerprint(&cmds);
        let decision =
            if cmds.is_empty() { Some(true) } else { self.trust_store().and_then(|t| t.decision(&root, &fp)) };
        let allowed = decision == Some(true);
        let mut layers: Vec<&Value> = extras.iter().collect();
        layers.extend([&project, &local]);
        let merged = projcfg::merge(&user, &layers, allowed);
        let new = Config::from_value(&merged);
        let mut problems: Vec<String> = e1.into_iter().chain(e2).collect();
        problems.extend(new.languages_errors());
        let st = LayerState {
            root,
            user,
            project,
            local,
            loaded: new.to_json(),
            target: self.settings_scope(),
            muted: false,
            commands_allowed: allowed,
            untrusted: if allowed { Vec::new() } else { cmds.clone() },
        };
        new.layers.set(Some(st));
        self.apply_config(new);
        match problems.first().cloned() {
            Some(p) if self.proj.told.as_ref() != Some(&p) => {
                let more = if problems.len() > 1 { format!(" (+{} more)", problems.len() - 1) } else { String::new() };
                self.say_for(format!("settings: {p}{more}"), 8);
                self.proj.told = Some(p);
            }
            Some(_) => {}
            None => self.proj.told = None,
        }
        if !cmds.is_empty() && decision.is_none() && self.proj.asked.as_deref() != Some(fp.as_str()) {
            self.proj.asked = Some(fp);
            self.projcfg_trust_prompt(false);
        }
    }

    /// Make `new` the current configuration, re-applying every setting whose value changed
    /// (theme, fonts, tab width, word wrap, language servers...) without writing any file.
    pub fn apply_config(&mut self, new: Config) {
        let old_vals = self.config_values();
        let new_vals = self.config_values_of(&new);
        let (old_json, new_json) = (self.config.to_json(), new.to_json());
        // Saves made by the setters below are only the old configuration moving to the new one.
        let mut quiet = new.layers.get().unwrap_or_default();
        quiet.muted = true;
        self.config.layers.set(Some(quiet));
        for (k, v) in new_vals.as_object().into_iter().flatten() {
            if old_vals.get(k) == Some(v) || THEME_KEYS.contains(&k.as_str()) || k.starts_with("status_pos_") {
                continue;
            }
            self.apply_setting(k, v);
        }
        let theme_changed = THEME_KEYS.iter().any(|k| old_json.get(*k) != new_json.get(*k));
        let servers_changed = old_json.get("language_servers") != new_json.get("language_servers");
        self.config = new;
        if theme_changed && std::env::var_os("LUMEN_THEME").is_none() {
            let dark = if self.config.appearance == "system" { crate::config::detect_dark() } else { None };
            let want = self.config.resolve_theme(dark);
            let keep = self.config.theme.clone();
            self.config.layers.with(|s| s.muted = true);
            self.set_theme_inner(&want, false);
            self.config.theme = keep;
            self.config.layers.with(|s| s.muted = false);
        }
        if servers_changed {
            self.lspx_sync_config();
        }
        for i in 0..self.docs.len() {
            self.apply_lang_overrides(i);
        }
        self.request_redraw();
    }

    /// Tab width and word wrap of document `i` from the `languages` overrides (a width picked in
    /// the bottom bar for the file wins).
    pub fn apply_lang_overrides(&mut self, i: usize) {
        let Some(d) = self.docs.get(i) else { return };
        let lang = d.lang;
        // A width picked for the file, or set by its `.editorconfig`, beats every settings layer.
        let own = d.path().and_then(|p| self.picks.get(p)).is_some_and(|p| p.tab_w.is_some())
            || d.fileconf.tab_width.is_some();
        let (tw, ww) = (self.config.tab_width_for(lang), self.config.word_wrap_for(lang));
        let d = &mut self.docs[i];
        if !own {
            d.ed.tab_w = tw;
        }
        d.set_word_wrap(ww);
    }

    /// File-system events: reload when a settings file changed. Events of the user's config folder
    /// are dropped (they are not project files) unless a document lives there. True when nothing
    /// else is left to handle.
    pub fn projcfg_fs_events(&mut self, evs: &mut Vec<FsEvent>) -> bool {
        let dir = self.root.join(projcfg::DIR);
        let user = crate::config::default_path();
        let project_hit = evs.iter().any(|e| e.path == dir || e.path.parent() == Some(dir.as_path()));
        let user_hit = user.as_ref().is_some_and(|u| evs.iter().any(|e| &e.path == u));
        if let Some(ud) = user.as_deref().and_then(Path::parent)
            && !ud.starts_with(&self.root)
            && !self.docs.iter().any(|d| d.path().and_then(Path::parent) == Some(ud))
        {
            evs.retain(|e| e.path.parent() != Some(ud));
        }
        if project_hit || user_hit {
            self.projcfg_reload(user_hit);
        }
        evs.is_empty()
    }

    // ───────────── trust ─────────────

    /// Ask whether the project's commands may run. `always`: also when nothing is pending (the
    /// palette command, to change an earlier answer).
    pub fn projcfg_trust_prompt(&mut self, always: bool) {
        let st = self.config.layers.get().unwrap_or_default();
        let mut cmds = projcfg::commands(&st.project);
        cmds.extend(projcfg::commands(&st.local));
        if cmds.is_empty() {
            if always {
                self.say("this project's settings run no commands");
            }
            return;
        }
        if !always && st.commands_allowed {
            return;
        }
        let now = if st.commands_allowed { "trusted now" } else { "ignored now" };
        let items = vec![
            Item {
                label: "Trust: run these commands".into(),
                detail: cmds.join(" · "),
                action: Action::Project(ProjPick::Trust(true)),
            },
            Item {
                label: "Don't trust: ignore them".into(),
                detail: format!("{now}; remembered for this project until the commands change"),
                action: Action::Project(ProjPick::Trust(false)),
            },
        ];
        let name = crate::app_topbar::project_name(&self.root);
        let title = format!("{name}: .lumen settings want to run commands");
        self.overlay = Overlay::Picker(choice_picker(&title, items, if st.commands_allowed { 0 } else { 1 }));
        self.request_redraw();
    }

    fn projcfg_set_trust(&mut self, yes: bool) {
        let st = self.config.layers.get().unwrap_or_default();
        let mut cmds = projcfg::commands(&st.project);
        cmds.extend(projcfg::commands(&st.local));
        let fp = projcfg::fingerprint(&cmds);
        let saved = self.trust_store().map(|t| t.set(&self.root, &fp, yes));
        self.proj.asked = Some(fp);
        self.projcfg_reload(false);
        match saved {
            Some(Err(e)) => self.say(format!("could not remember the answer: {e}")),
            _ => self.say(if yes { "project commands: trusted" } else { "project commands: ignored" }),
        }
    }

    pub fn projcfg_pick(&mut self, p: ProjPick) {
        match p {
            ProjPick::Trust(yes) => self.projcfg_set_trust(yes),
            ProjPick::Interpreter(py) => self.set_interpreter(py),
        }
    }

    // ───────────── settings modal scope ─────────────

    /// The scope of the open settings modal.
    pub fn settings_scope(&self) -> Option<Scope> {
        match &self.overlay {
            Overlay::Settings(m) => m.scope,
            _ => None,
        }
    }

    /// The settings modal on the Project scope.
    pub fn open_project_settings(&mut self) {
        self.open_settings(crate::settingsui::Page::Settings);
        self.settings_set_scope(Scope::Project);
    }

    /// Show the settings of `scope` in the open modal and write changes to its file from now on.
    /// User: your own values, rows the project overrides are marked. Project: the effective values,
    /// user-only rows hidden, rows the project files set are marked, and the reset icon goes back
    /// to your own value (which removes the key from the project file).
    pub fn settings_set_scope(&mut self, scope: Scope) {
        let st = self.config.layers.get().unwrap_or_default();
        let user_cfg = if st.user.is_object() { Config::from_value(&st.user) } else { self.config.clone() };
        let shown = if scope == Scope::User { self.config_values_of(&user_cfg) } else { self.config_values() };
        let user_vals = self.config_values_of(&user_cfg);
        let footer = match scope {
            Scope::User => format!(
                "Editing {} — your settings, in every project (Alt+P: this project)",
                crate::config::default_path().map_or_else(|| "config.json".into(), |p| p.display().to_string())
            ),
            Scope::Project => format!(
                "Editing {}/{} — overrides your settings in {} (Alt+U: yours)",
                projcfg::DIR,
                projcfg::FILE,
                crate::app_topbar::project_name(&self.root)
            ),
        };
        // Without project settings the User side keeps the key hints.
        let footer = (scope == Scope::Project || st.project_active()).then_some(footer);
        let Overlay::Settings(m) = &mut self.overlay else { return };
        m.scope = Some(scope);
        let mut values = m.settings.values();
        if let (Some(dst), Some(src)) = (values.as_object_mut(), shown.as_object()) {
            dst.extend(src.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        m.settings.set_values(values);
        let keys: Vec<&'static str> = m.settings.defs.iter().map(|d| d.key).collect();
        let mut badges = HashMap::new();
        let mut hidden = Vec::new();
        for k in keys {
            let (in_local, in_project) = (projcfg::layer_has(&st.local, k), projcfg::layer_has(&st.project, k));
            let note = match (scope, in_local, in_project) {
                (Scope::User, true, _) => Some("overridden in config.local.json"),
                (Scope::User, false, true) => Some("overridden by this project"),
                (Scope::Project, true, _) => Some("set in config.local.json"),
                (Scope::Project, false, true) => Some("set in .lumen/config.json"),
                _ => None,
            };
            if let Some(n) = note {
                badges.insert(k.to_string(), n.to_string());
            }
            if scope == Scope::Project && projcfg::global_only_setting(k) {
                hidden.push(k.to_string());
            }
        }
        if m.settings.hidden != hidden {
            m.settings.set_hidden(hidden);
        }
        let mut defaults = m.user_defaults.clone();
        if scope == Scope::Project
            && let (Some(dst), Some(src)) = (defaults.as_object_mut(), user_vals.as_object())
        {
            dst.extend(src.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        m.settings.set_defaults(defaults);
        m.settings.badges = badges;
        m.settings.footer = footer;
        self.config.layers.with(|s| s.target = Some(scope));
    }

    /// After a change made on the settings page: with a project layer in play, merge again (a user
    /// value the project overrides must not show as effective) and refresh the marks.
    pub(crate) fn settings_scoped_after_change(&mut self) {
        let Some(scope) = self.settings_scope() else { return };
        if scope == Scope::Project || self.config.layers.project_active() {
            self.projcfg_reload(false);
            self.settings_set_scope(scope);
        }
    }

    // ───────────── bottom bar ─────────────

    /// The bottom bar indicator while project settings are in effect (amber while some of their
    /// commands wait for trust). Returns the x after it.
    pub fn draw_projcfg_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        let (active, untrusted) =
            self.config.layers.with(|s| (s.project_active(), !s.untrusted.is_empty())).unwrap_or((false, false));
        if !active {
            return x;
        }
        let c = self.theme.chrome;
        let r = self.layout.status;
        let tint = if untrusted { c.warning } else { c.accent_text };
        let b = Rect::new(x, r.y + 3, 8 + 14 + 8, r.h - 6);
        if self.settings_scope() == Some(Scope::Project) {
            cv.fill_round(b, 6, c.list_active_bg);
        }
        self.tx.icon(cv, "hugeicons/settings", b.x + 8, r.y + (r.h - 14) / 2, 14, tint, r);
        self.layout.status_btns.push((b, StatusBtn::Project));
        b.right() + 2
    }

    pub fn projcfg_tip(&self) -> TipText {
        let st = self.config.layers.get().unwrap_or_default();
        TipText {
            title: "Project settings".into(),
            keys: self.shortcut_text("OpenProjectSettings"),
            detail: Some(format!("{}/ overrides your settings in this project — click to edit", projcfg::DIR)),
            more: projcfg::summary(&st),
        }
    }

    /// Bottom bar language item: the language, its overrides and, for Python, the interpreter.
    pub fn language_tip(&self) -> TipText {
        let d = self.docs.get(self.active);
        let lang = d.and_then(|d| d.lang);
        let mut more = Vec::new();
        if lang == Some("python") {
            more.push(match self.python_active() {
                Some(i) => format!("Interpreter: {} — {}", i.label(), i.python.display()),
                None => "Interpreter: no virtualenv found (Python: Select interpreter…)".into(),
            });
        }
        if let (Some(l), Some(o)) = (lang, lang.and_then(|l| self.config.languages.get(l)).and_then(Value::as_object))
            && !o.is_empty()
        {
            let kv: Vec<String> = o.iter().map(|(k, v)| format!("{k} {v}")).collect();
            more.push(format!("languages.{l}: {}", kv.join(", ")));
        }
        if self.config.layers.project_active() {
            more.push(format!("Project settings ({}/) are in effect", projcfg::DIR));
        }
        TipText {
            title: "Change language…".into(),
            keys: None,
            detail: d.map(|d| format!("Current: {}", d.lang.unwrap_or("text"))),
            more,
        }
    }

    // ───────────── Python interpreter ─────────────

    /// Folder the Python servers of the current document run in (their workspace root).
    fn python_root(&self) -> PathBuf {
        self.lspx_plan(self.active)
            .and_then(|p| p.entries.iter().find(|e| e.primary).map(|e| e.root.clone()))
            .unwrap_or_else(|| self.root.clone())
    }

    /// The interpreter the current document's Python servers get (cached for a few seconds).
    pub fn python_active(&self) -> Option<pyenv::Interp> {
        let root = self.python_root();
        if let Some((r, at, i)) = self.proj.py_cache.borrow().as_ref()
            && *r == root
            && at.elapsed() < PY_CACHE
        {
            return i.clone();
        }
        let i = pyenv::active(&root);
        self.proj.py_cache.replace(Some((root, Instant::now(), i.clone())));
        i
    }

    /// "Python: Select interpreter…": the project's venvs, then the system pythons.
    pub fn open_interpreter_picker(&mut self) {
        let root = self.python_root();
        let env = pyenv::Env::from_process();
        let cur = pyenv::active_with(&root, &env);
        let pinned = pyenv::choice_for(&root).is_some();
        let auto = pyenv::discover(&root, &env);
        let mut items = vec![Item {
            label: "Automatic".into(),
            detail: match auto.first() {
                Some(i) => format!("the project's virtualenv: {}", i.label()),
                None => "the project's virtualenv (none found)".into(),
            },
            action: Action::Project(ProjPick::Interpreter(None)),
        }];
        let mut sel = 0;
        let found = auto.into_iter().chain(pyenv::system_pythons());
        for i in found {
            if pinned && cur.as_ref().is_some_and(|c| c.python == i.python) {
                sel = items.len();
            }
            let place = i
                .python
                .strip_prefix(&self.root)
                .map_or_else(|_| i.python.display().to_string(), |p| p.display().to_string());
            items.push(Item {
                label: i.label(),
                detail: format!("{} · {place}", i.source),
                action: Action::Project(ProjPick::Interpreter(Some(i.python.clone()))),
            });
        }
        self.overlay = Overlay::Picker(choice_picker("Python interpreter for this project", items, sel));
        self.request_redraw();
    }

    /// Pin `python` for the project (`None` = automatic), remember it and restart the Python servers.
    pub fn set_interpreter(&mut self, python: Option<PathBuf>) {
        pyenv::set_choice(&self.root, python.clone());
        let stored = self
            .proj_state_file("python-interpreters.json")
            .map(|f| pyenv::store_choice(&f, &self.root, python.as_deref()));
        self.proj.py_cache.replace(None);
        self.lspx_restart_python();
        let label = match &python {
            Some(p) => pyenv::venv_of(p)
                .and_then(|v| v.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| p.display().to_string()),
            None => "automatic".into(),
        };
        match stored {
            Some(Err(e)) => self.say(format!("Python interpreter: {label} (not remembered: {e})")),
            _ => self.say(format!("Python interpreter: {label}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use serde_json::json;

    fn write(root: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(root.join(projcfg::DIR)).unwrap();
        std::fs::write(root.join(projcfg::DIR).join(name), text).unwrap();
    }

    fn read(root: &Path, name: &str) -> Value {
        projcfg::read_layer(&root.join(projcfg::DIR).join(name)).0
    }

    #[test]
    fn project_settings_apply_reload_live_and_save_to_their_file() {
        let _g = lock();
        let dir = tmp("projcfg-app");
        write(&dir, projcfg::FILE, r#"{"tab_width": 2, "ui_scale": 3, "languages": {"python": {"tab_width": 8}}}"#);
        let mut app = test_app(&dir);
        app.proj.state_dir = Some(dir.join(".state"));
        assert_eq!(app.config.tab_width, 2, "the project's value is in effect from the start");
        assert_eq!(app.config.ui_scale, 0.0, "user-only keys are ignored");
        assert!(app.config.layers.project_active());
        assert!(app.projcfg_tip().more.iter().any(|l| l.contains("ui_scale")), "the tooltip says what was ignored");

        // A document of a language with an override gets its own width.
        std::fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn main() {}\n").unwrap();
        let py = app.open_path(&dir.join("a.py"), None).unwrap();
        let rs = app.open_path(&dir.join("b.rs"), None).unwrap();
        assert_eq!((app.docs[py].ed.tab_w, app.docs[rs].ed.tab_w), (8, 2));

        // Live reload: the local file wins over the project file.
        write(&dir, projcfg::LOCAL_FILE, r#"{"tab_width": 4, "word_wrap": true}"#);
        let mut evs = vec![FsEvent { path: projcfg::local_file(&dir), kind: crate::watcher::FsKind::Modified }];
        assert!(!app.projcfg_fs_events(&mut evs), "a project file is also a project event");
        assert_eq!(app.config.tab_width, 4);
        assert!(app.config.word_wrap && app.docs[rs].view.wrap);
        assert_eq!(app.docs[rs].ed.tab_w, 4);
        assert_eq!(app.docs[py].ed.tab_w, 8, "the python override still wins");

        // A change made in the app goes to the file that defines the key; others to the user file.
        app.apply_setting("word_wrap", &json!(false));
        app.apply_setting("minimap", &json!(false));
        let local = read(&dir, projcfg::LOCAL_FILE);
        assert!(local.get("word_wrap").is_none(), "back to the user's value: the key leaves the local file");
        assert_eq!(local["tab_width"], json!(4));
        let user = Config::load_from(&dir.join(".config/lumen-text/config.json"));
        assert!(!user.minimap, "minimap is not set by the project: the user file");
        assert_eq!(user.tab_width, 4, "the project's tab width never leaks into the user file");
        assert!(read(&dir, projcfg::FILE).get("minimap").is_none());

        // Removing the project files brings the user's settings back.
        std::fs::remove_dir_all(dir.join(projcfg::DIR)).unwrap();
        let mut evs = vec![FsEvent { path: dir.join(projcfg::DIR), kind: crate::watcher::FsKind::Removed }];
        app.projcfg_fs_events(&mut evs);
        assert_eq!(app.config.tab_width, 4);
        assert!(!app.config.layers.project_active());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn project_commands_need_trust_once() {
        let _g = lock();
        let dir = tmp("projcfg-trust");
        write(
            &dir,
            projcfg::FILE,
            r#"{"build_command": "make evil", "tab_width": 2, "language_servers": {"rust": {"command": "./ra"}}}"#,
        );
        let mut app = test_app(&dir);
        // The trust store of the test lives in the project's scratch folder.
        app.proj.state_dir = Some(dir.join(".state"));
        app.projcfg_init();
        assert_eq!(app.config.tab_width, 2, "harmless keys apply right away");
        assert_eq!(app.config.build_command, "", "the command waits for trust");
        assert!(app.config.language_servers.pointer("/rust/command").is_none());
        let Overlay::Picker(p) = &app.overlay else { panic!("the trust prompt is shown") };
        assert!(p.items[0].detail.contains("build_command: make evil") && p.items[0].detail.contains("./ra"));
        assert_eq!(p.sel, 1, "\"Don't trust\" is preselected");
        assert!(app.projcfg_tip().more.iter().any(|l| l.starts_with("Not applied until trusted")));

        // Esc: not asked again by a reload in this session.
        app.overlay = Overlay::None;
        app.projcfg_reload(false);
        assert!(app.overlay.is_none());

        app.run_action(Action::Project(ProjPick::Trust(true)));
        assert_eq!(app.config.build_command, "make evil");
        assert_eq!(app.config.language_servers.pointer("/rust/command"), Some(&json!("./ra")));
        // Remembered: a new start of the same project does not ask.
        app.proj.asked = None;
        app.projcfg_init();
        assert!(app.overlay.is_none() && app.config.build_command == "make evil");
        // A changed command asks again and is not applied meanwhile.
        write(&dir, projcfg::FILE, r#"{"build_command": "make worse", "tab_width": 2}"#);
        app.projcfg_reload(false);
        assert_eq!(app.config.build_command, "");
        assert!(matches!(app.overlay, Overlay::Picker(_)));
        app.run_action(Action::Project(ProjPick::Trust(false)));
        assert_eq!(app.config.build_command, "", "refused");
        app.overlay = Overlay::None;
        app.proj.asked = None;
        app.projcfg_init();
        assert!(app.overlay.is_none(), "the refusal is remembered");
        // The palette command shows the question again on demand.
        app.run_cmd(crate::app_overlay::Cmd::ProjectTrust);
        assert!(matches!(app.overlay, Overlay::Picker(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_modal_scope_writes_the_right_file() {
        let _g = lock();
        let dir = tmp("projcfg-scope");
        let mut app = test_app(&dir);
        app.proj.state_dir = Some(dir.join(".state"));
        app.open_settings(crate::settingsui::Page::Settings);
        assert_eq!(app.settings_scope(), Some(Scope::User), "opens on the user's settings");
        app.apply_settings_event(crate::settingsui::Event::Scope(Scope::Project));
        let Overlay::Settings(m) = &app.overlay else { panic!() };
        assert!(m.settings.hidden.iter().any(|k| k == "ui_scale"), "user-only rows are hidden in the Project scope");
        assert!(!m.settings.hidden.iter().any(|k| k == "tab_width"));
        assert!(m.settings.footer.as_deref().is_some_and(|f| f.contains(".lumen/config.json")));

        app.apply_settings_event(crate::settingsui::Event::Setting { key: "tab_width".into(), value: json!("2") });
        assert_eq!(read(&dir, projcfg::FILE), json!({"tab_width": 2}), "the project file is created");
        assert_eq!(app.config.tab_width, 2);
        assert_eq!(Config::load_from(&dir.join(".config/lumen-text/config.json")).tab_width, 4, "user file untouched");
        let Overlay::Settings(m) = &app.overlay else { panic!() };
        assert_eq!(m.settings.badges.get("tab_width").map(String::as_str), Some("set in .lumen/config.json"));

        // User scope: the user's own value shows, marked as overridden; editing it keeps the
        // project's value in effect.
        app.apply_settings_event(crate::settingsui::Event::Scope(Scope::User));
        let Overlay::Settings(m) = &app.overlay else { panic!() };
        assert_eq!(m.settings.values()["tab_width"], json!("4"));
        assert_eq!(m.settings.badges.get("tab_width").map(String::as_str), Some("overridden by this project"));
        app.apply_settings_event(crate::settingsui::Event::Setting { key: "tab_width".into(), value: json!("8") });
        assert_eq!(Config::load_from(&dir.join(".config/lumen-text/config.json")).tab_width, 8);
        assert_eq!(app.config.tab_width, 2, "the project still decides here");

        // Project scope, back to the user's value: the key leaves the project file.
        app.apply_settings_event(crate::settingsui::Event::Scope(Scope::Project));
        app.apply_settings_event(crate::settingsui::Event::Setting { key: "tab_width".into(), value: json!("8") });
        assert!(read(&dir, projcfg::FILE).get("tab_width").is_none());
        assert_eq!(app.config.tab_width, 8);
        app.apply_settings_event(crate::settingsui::Event::Close);
        assert_eq!(app.config.layers.get().unwrap().target, None, "closing ends the scoped writes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn interpreter_picker_lists_venvs_and_pins_the_choice() {
        let _g = lock();
        let dir = tmp("projcfg-py");
        let venv = dir.join(".venv");
        let py = if cfg!(windows) { venv.join("Scripts").join("python.exe") } else { venv.join("bin").join("python") };
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\nversion = 3.12.4\n").unwrap();
        std::fs::write(&py, "").unwrap();
        let mut app = test_app(&dir);
        app.proj.state_dir = Some(dir.join(".state"));
        std::fs::write(dir.join("m.py"), "import os\n").unwrap();
        app.open_path(&dir.join("m.py"), None);
        let tip = app.language_tip();
        assert!(tip.more.iter().any(|l| l.contains(".venv (Python 3.12.4)")), "{:?}", tip.more);

        app.run_cmd(crate::app_overlay::Cmd::PythonSelectInterpreter);
        let Overlay::Picker(p) = &app.overlay else { panic!("picker") };
        assert_eq!(p.items[0].label, "Automatic");
        assert!(p.items[0].detail.contains(".venv"));
        assert!(p.items.iter().any(|i| i.label == ".venv (Python 3.12.4)"));

        let other = dir.join("envs/old");
        let other_py =
            if cfg!(windows) { other.join("Scripts").join("python.exe") } else { other.join("bin").join("python") };
        std::fs::create_dir_all(other_py.parent().unwrap()).unwrap();
        std::fs::write(other.join("pyvenv.cfg"), "version = 3.9.0\n").unwrap();
        std::fs::write(&other_py, "").unwrap();
        app.overlay = Overlay::None;
        app.run_action(Action::Project(ProjPick::Interpreter(Some(other_py.clone()))));
        assert_eq!(pyenv::active(&dir).map(|i| i.python), Some(other_py.clone()));
        assert!(app.language_tip().more.iter().any(|l| l.contains("old (Python 3.9.0)")));
        let file = dir.join(".state/python-interpreters.json");
        assert_eq!(pyenv::load_choice(&file, &dir), Some(other_py.clone()), "remembered");
        // A new start of the project reads it back; Automatic forgets it.
        pyenv::set_choice(&dir, None);
        app.projcfg_init();
        assert_eq!(pyenv::choice_for(&dir), Some(other_py));
        app.run_action(Action::Project(ProjPick::Interpreter(None)));
        assert_eq!(pyenv::active(&dir).map(|i| i.python), Some(py));
        assert_eq!(pyenv::load_choice(&file, &dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
