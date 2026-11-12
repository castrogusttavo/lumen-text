//! Formatting: one chain for "Format document" and format-on-save, plus code actions on save.
//!
//! Order (`formatter: "auto"`, the default): the project's own formatter (Biome / Prettier, run on
//! stdin) → the file's language server (`textDocument/formatting`, tsgo for TS/JS) → a built-in
//! JSON re-indenter. When nothing applies the user is told, once per file type and session. The
//! `formatter` setting can force one of them per language ("biome", "prettier", "lsp",
//! "lsp:<server>") or switch formatting off ("none").
//!
//! `code_actions_on_save` kinds (`source.organizeImports`, `source.fixAll`, ...) run before the
//! formatter, in order, on every server of the file that offers them; each request waits at most
//! [`ACTION_WAIT`] and a server that did not answer is skipped for the rest of that save, so a hung
//! server never blocks saving.

use crate::{
    app::App,
    app_lsp::parse_edits,
    lsp::{Msg, Pending},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// How long saving waits for a language server to answer before writing the text unformatted.
pub const SAVE_WAIT: Duration = Duration::from_millis(1500);
/// Same wait for the explicit "Format document" command.
pub const MANUAL_WAIT: Duration = Duration::from_secs(5);
/// How long one on-save code action request (and its resolve) may take.
pub const ACTION_WAIT: Duration = Duration::from_millis(1000);

#[derive(Debug, PartialEq, Eq)]
pub enum Fmt {
    /// The text was rewritten by this formatter.
    Changed(String),
    /// Already formatted.
    Same(String),
    Failed(String),
    /// The server did not answer in time (the text is untouched).
    TimedOut(String),
    /// The project's formatter config leaves this file out (e.g. Biome's `files.includes`).
    Ignored(String),
    NoFormatter,
    /// The `formatter` setting is "none" for this language.
    Off,
}

impl App {
    #[cfg(test)]
    pub fn format_on_save_on(&self) -> bool {
        self.config.format_on_save
    }

    /// Auto-import in completion; `LUMEN_AUTOIMPORT` overrides the setting for this run.
    pub fn auto_import_on(&self) -> bool {
        std::env::var("LUMEN_AUTOIMPORT").map_or(self.config.auto_import, |v| v == "1")
    }

    pub fn toggle_format_on_save(&mut self) {
        let on = !self.config.format_on_save;
        self.apply_setting("format_on_save", &json!(on));
        self.say(if on { "format on save: on" } else { "format on save: off" });
    }

    /// Run the chain on document `i`, applying the result as one undo step.
    pub fn format_doc(&mut self, i: usize, wait: Duration) -> Fmt {
        let Some(path) = self.docs.get(i).and_then(|d| d.path().map(Path::to_path_buf)) else {
            return Fmt::NoFormatter;
        };
        let choice = self.config.formatter_for(self.docs[i].lang).to_string();
        match choice.as_str() {
            "none" => return Fmt::Off,
            "biome" | "prettier" => {
                return match crate::format::detect_named(&path, &choice) {
                    Some(f) => self.format_external(i, &f, &path),
                    None => Fmt::Failed(format!(
                        "{choice} is not set up for this file (needs its config and node_modules/.bin/{choice})"
                    )),
                };
            }
            "lsp" => {
                return self
                    .format_with_lsp(i, wait)
                    .or_else(|| self.format_with_companion(i, wait))
                    .unwrap_or_else(|| Fmt::Failed("no language server of this file can format it".into()));
            }
            c if c.starts_with("lsp:") => return self.format_with_named(i, &c["lsp:".len()..], wait),
            _ => {}
        }
        if let Some(f) = crate::format::detect(&path) {
            return self.format_external(i, &f, &path);
        }
        if let Some(r) = self.format_with_lsp(i, wait) {
            return r;
        }
        if let Some(r) = self.format_with_companion(i, wait) {
            return r;
        }
        if path.extension().is_some_and(|e| e == "json") {
            let d = &self.docs[i];
            let indent = if d.ed.indent.contains('\t') { "\t".to_string() } else { d.ed.indent.clone() };
            let text = d.buf.to_string();
            return match crate::format::format_json(&text, &indent) {
                Some(new) if new == text => Fmt::Same("built-in JSON".into()),
                Some(new) => {
                    self.docs[i].replace_text(&new);
                    Fmt::Changed("built-in JSON".into())
                }
                None => Fmt::Failed("built-in JSON: not plain JSON (comments or a syntax error)".into()),
            };
        }
        Fmt::NoFormatter
    }

    /// Biome / Prettier on stdin.
    fn format_external(&mut self, i: usize, f: &crate::format::Formatter, path: &Path) -> Fmt {
        let text = self.docs[i].buf.to_string();
        match crate::format::run(f, &text) {
            // Unchanged may also mean "not one of mine": Biome says nothing about ignored files.
            Ok(new) if new == text && crate::format::biome_ignores(f, path) => Fmt::Ignored(f.name.into()),
            Ok(new) if new == text => Fmt::Same(f.name.into()),
            Ok(new) => {
                self.docs[i].replace_text(&new);
                Fmt::Changed(f.name.into())
            }
            Err(e) => Fmt::Failed(e),
        }
    }

    /// `formatter: "lsp:<name>"`: the document's primary server or one of its companions, by name.
    fn format_with_named(&mut self, i: usize, name: &str, wait: Duration) -> Fmt {
        // Start the primary server if needed (a fresh file may not be open on it yet).
        let primary = self.ensure_lsp(i);
        if primary.is_some_and(|li| self.lsps[li].name == name) {
            return self
                .format_with_lsp(i, wait)
                .unwrap_or_else(|| Fmt::Failed(format!("{name} cannot format this file")));
        }
        match self.lspx_open_companions(i).into_iter().find(|&c| self.lsps[c].name == name) {
            Some(c) => self.format_via(i, c, wait),
            None => Fmt::Failed(format!("language server \"{name}\" is not running for this file")),
        }
    }

    /// `textDocument/formatting` from the language server of this file; `None` when there is no
    /// server for it or it cannot format.
    fn format_with_lsp(&mut self, i: usize, wait: Duration) -> Option<Fmt> {
        let li = self.ensure_lsp(i)?;
        let name = self.lsps[li].name.clone();
        let cap = &self.lsps[li].caps["documentFormattingProvider"];
        if self.lsps[li].ready && (cap.is_null() || cap.as_bool() == Some(false)) {
            return None;
        }
        if !self.docs[i].lsp_open {
            let params = Self::doc_text_params(&self.docs[i])?;
            self.lsps[li].notify("textDocument/didOpen", params);
            self.docs[i].lsp_open = true;
            self.docs[i].changed_at = None;
        }
        self.flush(i);
        let d = &self.docs[i];
        let uri = d.uri()?;
        let tab = if d.ed.indent.contains('\t') { d.ed.tab_w } else { d.ed.indent.len().max(1) };
        let params = json!({
            "textDocument": {"uri": uri},
            "options": {"tabSize": tab, "insertSpaces": !d.ed.indent.contains('\t'),
                        "trimTrailingWhitespace": true, "insertFinalNewline": true}
        });
        let version = d.version;
        let v = match self.lsp_request_wait(li, "textDocument/formatting", params, wait) {
            Ok(v) => v,
            Err(e) if e == TIMEOUT => return Some(Fmt::TimedOut(name)),
            Err(e) => return Some(Fmt::Failed(format!("{name}: {e}"))),
        };
        if self.docs.get(i).is_none_or(|d| d.version != version) {
            return Some(Fmt::Failed("the text changed during formatting".into()));
        }
        let edits = parse_edits(&v);
        if edits.is_empty() {
            return Some(Fmt::Same(name));
        }
        let before = self.docs[i].buf.to_string();
        self.docs[i].apply_lsp_edits(&edits);
        Some(if self.docs[i].buf.to_string() == before { Fmt::Same(name) } else { Fmt::Changed(name) })
    }

    /// Send a request to server `li` and wait for its answer (other messages are handled as
    /// usual meanwhile, so a server that is still starting can finish its handshake).
    pub(crate) fn lsp_request_wait(
        &mut self,
        li: usize,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, String> {
        let id = self.lsps[li].request(method, params, Pending::Blocking);
        let deadline = Instant::now() + wait;
        let mut dead = Vec::new();
        let mut out = Err(TIMEOUT.to_string());
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let m = match self.lsps[li].rx.recv_timeout(left.min(Duration::from_millis(25))) {
                Ok(m) => m,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => {
                    out = Err("the language server exited".into());
                    break;
                }
            };
            match m {
                Msg::Response { id: rid, result } if rid == id => {
                    self.lsps[li].pending.remove(&id);
                    out = result.map_err(|e| e["message"].as_str().unwrap_or("request failed").to_string());
                    break;
                }
                Msg::Exited => {
                    dead.push(li);
                    out = Err("the language server exited".into());
                    break;
                }
                m => self.handle_lsp_msg(li, m, &mut dead),
            }
        }
        self.drop_dead_servers(dead);
        out
    }

    /// Save step for document `i`: code actions on save, then format-on-save. Returns what to tell
    /// the user *after* saving (problems must not be overwritten by the plain "Saved" message).
    pub fn format_for_save(&mut self, i: usize) -> Option<String> {
        let actions = self.code_actions_for_save(i);
        let fmt = self.format_step_for_save(i);
        match (actions, fmt) {
            (Some(a), Some(f)) => Some(format!("{a}; {f}")),
            (a, f) => a.or(f),
        }
    }

    fn format_step_for_save(&mut self, i: usize) -> Option<String> {
        // `languages.<id>.format_on_save` overrides the global switch for that language.
        if !self.config.format_on_save_for(self.docs.get(i).and_then(|d| d.lang)) {
            return None;
        }
        let path = self.docs.get(i)?.path()?.to_path_buf();
        match self.format_doc(i, SAVE_WAIT) {
            Fmt::Changed(_) | Fmt::Same(_) | Fmt::Off => None,
            Fmt::Failed(e) => Some(format!("formatting failed: {e}")),
            Fmt::TimedOut(n) => Some(format!("{n} did not answer in time, so it was saved unformatted")),
            // Once per file: saving it again must not keep repeating it.
            Fmt::Ignored(n) => self.fmt_told.insert(format!("ignored:{}", path.display())).then(|| ignored_msg(&n)),
            Fmt::NoFormatter => {
                let key = crate::format::type_key(&path);
                self.fmt_told.insert(key.clone()).then(|| no_formatter_msg(&key))
            }
        }
    }

    /// Palette / shortcut "Format document".
    pub fn format_document(&mut self) {
        let i = self.active;
        let Some(path) = self.docs.get(i).and_then(|d| d.path().map(Path::to_path_buf)) else {
            return self.say("save the file first to format it");
        };
        match self.format_doc(i, MANUAL_WAIT) {
            Fmt::Changed(n) => {
                self.docs[i].reveal = true;
                self.say(format!("formatted with {n}"));
            }
            Fmt::Same(n) => self.say(format!("already formatted ({n})")),
            Fmt::Failed(e) => self.say_for(format!("failed to format — {e}"), 8),
            Fmt::TimedOut(n) => self.say_for(format!("{n} did not answer in time — try again"), 8),
            Fmt::Ignored(n) => self.say_for(ignored_msg(&n), 8),
            Fmt::Off => {
                let lang = self.docs[i].lang.unwrap_or("this file");
                self.say_for(format!("formatting is off for {lang} (\"formatter\": \"none\" in config.json)"), 8)
            }
            Fmt::NoFormatter => {
                let key = crate::format::type_key(&path);
                self.fmt_told.insert(key.clone());
                self.say_for(no_formatter_msg(&key), 8);
            }
        }
    }
}

pub(crate) const TIMEOUT: &str = "timeout";

/// Does an action of kind `got` belong to the requested kind `want` (`source.fixAll.eslint` is a
/// `source.fixAll`)? An action without a kind is taken (the server already filtered by `only`).
pub(crate) fn kind_matches(want: &str, got: Option<&str>) -> bool {
    match got {
        None | Some("") => true,
        Some(g) => g == want || g.strip_prefix(want).is_some_and(|rest| rest.starts_with('.')),
    }
}

/// Does a server's `codeActionProvider` capability allow kind `want`? `true` / an object without
/// `codeActionKinds` allows everything; a listed kind allows itself and its sub-kinds and parents
/// (asking for `source` is answered with `source.organizeImports`).
pub(crate) fn offers_kind(cap: &Value, want: &str) -> bool {
    if cap.is_null() || cap.as_bool() == Some(false) {
        return false;
    }
    match cap["codeActionKinds"].as_array() {
        Some(kinds) => {
            kinds.iter().filter_map(Value::as_str).any(|k| kind_matches(want, Some(k)) || kind_matches(k, Some(want)))
        }
        None => true,
    }
}

impl App {
    /// Run the `code_actions_on_save` kinds of document `i`'s language, in order, on every server
    /// of the document that offers them. Returns a message when something timed out.
    pub(crate) fn code_actions_for_save(&mut self, i: usize) -> Option<String> {
        let kinds = self.config.code_actions_for(self.docs.get(i)?.lang).to_vec();
        if kinds.is_empty() || !self.lsp_enabled() || self.docs[i].path().is_none() {
            return None;
        }
        if self.docs[i].buf.line_count() > crate::bigfile::BIG_LINES {
            return None;
        }
        // Primary (tsgo or the registry primary), then companions, then Biome's server.
        let mut servers: Vec<usize> = Vec::new();
        if let Some(li) = self.doc_lsp(i).filter(|_| self.docs[i].lsp_open) {
            servers.push(li);
        }
        servers.extend(self.lspx_open_companions(i));
        if let Some(lb) = self.biome_index(i).filter(|_| self.docs[i].biome_open) {
            servers.push(lb);
        }
        servers.dedup();
        let mut slow: Vec<String> = Vec::new();
        let mut hung: Vec<usize> = Vec::new();
        for kind in &kinds {
            for &li in &servers {
                if hung.contains(&li) || li >= self.lsps.len() || !self.lsps[li].ready {
                    continue;
                }
                if !offers_kind(&self.lsps[li].caps["codeActionProvider"], kind) {
                    continue;
                }
                if !self.run_save_action(i, li, kind) {
                    hung.push(li);
                    slow.push(format!("{kind} ({})", self.lsps[li].name));
                }
            }
        }
        (!slow.is_empty()).then(|| format!("{} timed out", slow.join(", ")))
    }

    /// One on-save code action kind on server `li`. False when the server did not answer in time.
    fn run_save_action(&mut self, i: usize, li: usize, kind: &str) -> bool {
        self.sync_for_action(i, li);
        let d = &self.docs[i];
        let tsgo = self.lsps[li].name == "tsgo";
        let Some(uri) = (if tsgo { d.uri() } else { d.real_uri() }) else { return true };
        let last = d.buf.line_count().saturating_sub(1);
        let (el, ec) = d.lsp_pos(d.buf.pos(last, d.buf.line_len(last)));
        let params = json!({
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": el, "character": ec}},
            "context": {"diagnostics": [], "only": [kind], "triggerKind": 2}
        });
        let version = d.version;
        let v = match self.lsp_request_wait(li, "textDocument/codeAction", params, ACTION_WAIT) {
            Ok(v) => v,
            Err(e) => return e != TIMEOUT,
        };
        if li >= self.lsps.len() || self.docs.get(i).is_none_or(|d| d.version != version) {
            return true;
        }
        let Some(action) = v
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["command"].is_string() || kind_matches(kind, a["kind"].as_str()))
            .cloned()
        else {
            return true;
        };
        // A bare `Command` (no edit, `command` is the id).
        if let Some(cmd) = action["command"].as_str() {
            let params = json!({"command": cmd, "arguments": action["arguments"]});
            return self.lsp_request_wait(li, "workspace/executeCommand", params, ACTION_WAIT).err().as_deref()
                != Some(TIMEOUT);
        }
        let mut action = action;
        if !action["edit"].is_object() && action["data"].is_object() && !action["command"].is_object() {
            match self.lsp_request_wait(li, "codeAction/resolve", action.clone(), ACTION_WAIT) {
                Ok(r) => action = r,
                Err(e) => return e != TIMEOUT,
            }
        }
        if action["edit"].is_object() {
            self.apply_edit_to_doc(i, &action["edit"]);
        }
        if action["command"].is_object() && li < self.lsps.len() {
            let c = &action["command"];
            let params = json!({"command": c["command"], "arguments": c["arguments"]});
            // The server applies its edit through `workspace/applyEdit`, handled while waiting.
            return self.lsp_request_wait(li, "workspace/executeCommand", params, ACTION_WAIT).err().as_deref()
                != Some(TIMEOUT);
        }
        true
    }

    /// Make sure server `li` has the document's current text before asking it for actions.
    fn sync_for_action(&mut self, i: usize, li: usize) {
        if self.lsps[li].name == "biome" {
            if self.docs[i].lint_changed_at.is_some()
                && let Some(uri) = self.docs[i].real_uri()
            {
                let d = &mut self.docs[i];
                d.lint_changed_at = None;
                let params = json!({"textDocument": {"uri": uri, "version": d.version}, "contentChanges": [{"text": d.buf.to_string()}]});
                self.lsps[li].notify("textDocument/didChange", params);
            }
            return;
        }
        if self.docs[i].changed_at.is_some() {
            self.send_change(i);
        }
    }

    /// Apply the part of a `WorkspaceEdit` that touches document `i` (on-save actions never open
    /// or change other files).
    fn apply_edit_to_doc(&mut self, i: usize, edit: &Value) {
        let Some(path) = self.docs[i].path().map(Path::to_path_buf) else { return };
        let mine = |uri: &str| crate::doc::uri_to_path(uri).is_some_and(|p| p == path);
        let mut edits = Vec::new();
        for (uri, e) in edit["changes"].as_object().into_iter().flatten() {
            if mine(uri) {
                edits.extend(parse_edits(e));
            }
        }
        for dc in edit["documentChanges"].as_array().into_iter().flatten() {
            if dc["textDocument"]["uri"].as_str().is_some_and(mine) {
                edits.extend(parse_edits(&dc["edits"]));
            }
        }
        if !edits.is_empty() {
            self.docs[i].apply_lsp_edits(&edits);
        }
    }
}

fn ignored_msg(name: &str) -> String {
    let cfg = if name == "biome" { " (files.includes / ignore in biome.json)" } else { "" };
    format!("{name} ignores this file{cfg} — not formatted")
}

fn no_formatter_msg(key: &str) -> String {
    format!("no formatter for {key} — add a biome.json or a prettier config")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, scratch_base, test_app};
    use std::os::unix::fs::PermissionsExt;

    fn project(tag: &str) -> std::path::PathBuf {
        let d = scratch_base().join(format!("fmtsave-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        crate::paths::canonical(&d).unwrap()
    }

    /// A prettier config plus a local "prettier" shell script: stdin → `script` → stdout.
    fn fake_prettier(dir: &Path, script: &str) {
        std::fs::write(dir.join(".prettierrc"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        let bin = dir.join("node_modules/.bin/prettier");
        std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn status(app: &App) -> String {
        app.status.as_ref().map(|s| s.0.clone()).unwrap_or_default()
    }

    fn open(app: &mut App, f: &Path, text: &str) -> usize {
        std::fs::write(f, text).unwrap();
        let i = app.open_path(f, None).expect("open");
        app.active = i;
        i
    }

    #[test]
    fn settings_toggle_takes_effect_without_restart() {
        let _g = lock();
        let dir = project("toggle");
        let mut app = test_app(&dir);
        fake_prettier(&dir, "sed 's/  */ /g'");
        let f = dir.join("src/a.css");
        let i = open(&mut app, &f, "a   {  }\n");
        app.docs[i].replace_text("a   {   color:red }\n");
        // Off (default): saved as typed.
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a   {   color:red }\n");
        // Settings modal toggles `format_on_save` through the same event path.
        app.apply_setting("format_on_save", &json!(true));
        app.docs[i].replace_text("a   {   color:blue }\n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a { color:blue }\n");
        assert_eq!(app.docs[i].buf.to_string(), "a { color:blue }\n", "the buffer shows the formatted text");
        assert_eq!(status(&app), "Saved a.css");
        app.apply_setting("format_on_save", &json!(false));
        app.docs[i].replace_text("b   {  }\n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "b   {  }\n");
        // The palette toggle goes through the same state.
        app.toggle_format_on_save();
        assert!(app.config.format_on_save && app.format_on_save_on());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_biome_ignores_is_reported_once_not_silently_skipped() {
        let _g = lock();
        let dir = project("biomeign");
        let mut app = test_app(&dir);
        app.apply_setting("format_on_save", &json!(true));
        // biome.json covers src/ only: through stdin Biome hands lib/ files back untouched and
        // silent; asked about the path, it reports it as ignored (like the real Biome 2).
        std::fs::write(dir.join("biome.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        let bin = dir.join("node_modules/.bin/biome");
        let script = r#"#!/bin/sh
case "$2" in
  --stdin-file-path=*/src/*) sed 's/  */ /g' ;;
  --stdin-file-path=*) cat ;;
  src/*) echo 'Checked 1 file' ;;
  *) echo 'No files were processed in the specified paths.'; echo 'These paths were provided but ignored:' ;;
esac
"#;
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let lib = dir.join("lib/y.ts");
        let i = open(&mut app, &lib, "const   b  =  2\n");
        app.docs[i].replace_text("const   b  =  3\n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&lib).unwrap(), "const   b  =  3\n", "ignored: saved as typed");
        assert!(status(&app).contains("biome ignores this file"), "told why: {}", status(&app));
        // Once per file: the next save is quiet.
        app.docs[i].replace_text("const   b  =  4\n");
        app.save_active();
        assert!(!status(&app).contains("ignores"), "not repeated: {}", status(&app));

        // A covered file is still formatted, with no notice.
        let src = dir.join("src/x.ts");
        let j = open(&mut app, &src, "let   z  =  3\n");
        app.docs[j].replace_text("let   z  =  4\n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&src).unwrap(), "let z = 4\n");
        assert!(!status(&app).contains("ignores"));
        // Already-formatted covered file: unchanged, and not mistaken for ignored.
        app.save_active();
        assert!(!status(&app).contains("ignores"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_import_setting_is_read_from_config() {
        let _g = lock();
        let dir = project("autoimp");
        let mut app = test_app(&dir);
        unsafe { std::env::remove_var("LUMEN_AUTOIMPORT") };
        assert!(!app.auto_import_on());
        app.apply_setting("auto_import", &json!(true));
        assert!(app.auto_import_on());
        app.toggle_auto_import();
        assert!(!app.auto_import_on());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn formatter_failure_stays_visible_after_saving() {
        let _g = lock();
        let dir = project("fail");
        let mut app = test_app(&dir);
        app.apply_setting("format_on_save", &json!(true));
        fake_prettier(&dir, "echo 'i schema mismatch' >&2; echo '× Syntax error on line 1' >&2; exit 2");
        let f = dir.join("src/a.ts");
        let i = open(&mut app, &f, "x\n");
        app.docs[i].replace_text("let   x = (\n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "let   x = (\n", "still saved");
        let m = status(&app);
        assert!(m.starts_with("Saved a.ts, but formatting failed:") && m.contains("Syntax error on line 1"), "{m}");
        assert!(!m.contains("schema mismatch"), "{m}");
        // Still shown after the usual 6 seconds.
        assert!(app.status.as_ref().unwrap().1 > Instant::now());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_formatter_is_reported_once_per_file_type() {
        let _g = lock();
        let dir = project("none");
        let mut app = test_app(&dir);
        app.apply_setting("format_on_save", &json!(true));
        let f = dir.join("src/a.css");
        let i = open(&mut app, &f, "a{}\n");
        app.docs[i].replace_text("a{ }\n");
        app.save_active();
        let m = status(&app);
        assert!(m.contains("no formatter for .css") && m.contains("biome.json"), "{m}");
        app.docs[i].replace_text("a{  }\n");
        app.save_active();
        assert_eq!(status(&app), "Saved a.css", "second save is quiet");
        // The explicit command always answers.
        app.format_document();
        assert!(status(&app).contains("no formatter for .css"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_falls_back_to_the_built_in_formatter() {
        let _g = lock();
        let dir = project("json");
        let mut app = test_app(&dir);
        app.apply_setting("format_on_save", &json!(true));
        let f = dir.join("data.json");
        let i = open(&mut app, &f, "{}\n");
        app.docs[i].replace_text("{\"a\":1,\"b\":[1,2]}");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ]\n}\n");
        assert_eq!(status(&app), "Saved data.json");
        app.docs[i].replace_text("{\"a\": // c\n 1}");
        app.save_active();
        assert!(status(&app).contains("formatting failed"), "{}", status(&app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manual_format_reports_the_outcome() {
        let _g = lock();
        let dir = project("manual");
        let mut app = test_app(&dir);
        fake_prettier(&dir, "sed 's/  */ /g'");
        let f = dir.join("src/a.ts");
        let i = open(&mut app, &f, "a   b\n");
        app.format_document();
        assert_eq!(status(&app), "formatted with prettier");
        assert_eq!(app.docs[i].buf.to_string(), "a b\n");
        app.format_document();
        assert_eq!(status(&app), "already formatted (prettier)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With the real tsgo: a plain tsconfig project (no biome / prettier) is reformatted on save.
    #[test]
    fn real_tsgo_formats_a_plain_tsconfig_project_on_save() {
        if crate::lsp::find_tsgo(Path::new("/nonexistent"), "bundled").is_none() {
            eprintln!("tsgo not installed, skipping");
            return;
        }
        let _g = lock();
        let dir = project("tsgo");
        std::fs::write(
            dir.join("tsconfig.json"),
            r#"{"compilerOptions":{"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","strict":true,"skipLibCheck":true},"include":["src"]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("package.json"), r#"{"name":"x","type":"module"}"#).unwrap();
        let mut app = test_app(&dir);
        app.config.tsgo_source = "bundled".into();
        app.apply_setting("format_on_save", &json!(true));
        let f = dir.join("src/a.ts");
        let bad = "export   function  f( a:number,b:string ){\nconst x = {a:1,\n  b:[1,2,3]};\n    return   x\n}\n";
        let i = open(&mut app, &f, bad);
        app.docs[i].replace_text(&format!("{bad}// touched\n"));
        app.save_active();
        let got = std::fs::read_to_string(&f).unwrap();
        assert!(got.contains("export function f(a: number, b: string) {"), "not formatted:\n{got}\n{}", status(&app));
        assert!(!got.contains("{a:1"), "{got}");
        assert_eq!(app.docs[i].buf.to_string(), got);
        assert_eq!(status(&app), "Saved a.ts");
        // One undo step brings the pre-format text back.
        let d = &mut app.docs[i];
        d.ed.undo(&mut d.buf);
        assert!(app.docs[i].buf.to_string().contains("export   function"), "{}", app.docs[i].buf.to_string());
        for l in &mut app.lsps {
            l.shutdown();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fake server built from `examples/fake_lsp.rs` (cargo builds examples for `cargo test`).
    fn fake_exe() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let p = exe.parent()?.parent()?.join("examples").join(format!("fake_lsp{}", std::env::consts::EXE_SUFFIX));
        p.exists().then_some(p)
    }

    fn fake_server(exe: &Path, env: Value) -> Value {
        let mut env = env;
        env["FAKE_NAME"] = json!("a");
        json!({"fake-a": {"command": exe, "languages": ["fk"], "role": "primary", "env": env}})
    }

    /// Open `f` and wait until the fake server has it open.
    fn open_on_server(app: &mut App, f: &Path, text: &str) -> usize {
        let i = open(app, f, text);
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            app.lsp_tick(Instant::now(), &mut |_| {});
            app.pump_lsp();
            if app.lsps.first().is_some_and(|l| l.ready && !l.open_uris.is_empty()) {
                return i;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        panic!("the fake server never opened the file: {:?}", app.status);
    }

    #[test]
    fn formatter_setting_picks_the_winner_or_turns_formatting_off() {
        let _g = lock();
        let dir = project("choice");
        let mut app = test_app(&dir);
        app.apply_setting("format_on_save", &json!(true));
        // Both are set up; Biome would win on its own.
        fake_prettier(&dir, "sed 's/  */ /g'");
        std::fs::write(dir.join("biome.json"), "{}").unwrap();
        let bin = dir.join("node_modules/.bin/biome");
        std::fs::write(&bin, "#!/bin/sh\ntr a-z A-Z\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let f = dir.join("src/a.css");
        let i = open(&mut app, &f, "a   {  }\n");
        assert_eq!(app.format_doc(i, MANUAL_WAIT), Fmt::Changed("biome".into()), "auto: Biome first");
        app.docs[i].replace_text("a   {  }\n");
        app.config.formatter = [("*".to_string(), "prettier".to_string())].into();
        assert_eq!(app.format_doc(i, MANUAL_WAIT), Fmt::Changed("prettier".into()));
        assert_eq!(app.docs[i].buf.to_string(), "a { }\n");
        // "none": saved as typed, silently; the explicit command says why.
        app.config.formatter = [("*".to_string(), "none".to_string())].into();
        app.docs[i].replace_text("b   {  }\n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "b   {  }\n");
        assert_eq!(status(&app), "Saved a.css");
        app.format_document();
        assert!(status(&app).contains("formatting is off"), "{}", status(&app));
        // A forced formatter that is not set up is an error, not a silent fallback.
        std::fs::remove_file(dir.join("node_modules/.bin/prettier")).unwrap();
        app.config.formatter = [("*".to_string(), "prettier".to_string())].into();
        let r = app.format_doc(i, MANUAL_WAIT);
        assert!(matches!(&r, Fmt::Failed(m) if m.contains("prettier is not set up")), "{r:?}");
        // A server that is not running.
        app.config.formatter = [("*".to_string(), "lsp:nope".to_string())].into();
        let r = app.format_doc(i, MANUAL_WAIT);
        assert!(matches!(&r, Fmt::Failed(m) if m.contains("\"nope\" is not running")), "{r:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn formatter_lsp_name_beats_the_project_formatter() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("lspname");
        let mut app = test_app(&dir);
        fake_prettier(&dir, "sed 's/  */ /g'");
        app.config.language_servers = fake_server(&exe, json!({"FAKE_FORMAT": "1"}));
        let i = open_on_server(&mut app, &dir.join("src/x.fk"), "x   y\n");
        assert_eq!(app.format_doc(i, MANUAL_WAIT), Fmt::Changed("prettier".into()), "auto: the project's formatter");
        app.config.formatter = [("*".to_string(), "lsp:fake-a".to_string())].into();
        assert_eq!(app.format_doc(i, MANUAL_WAIT), Fmt::Changed("fake-a".into()));
        assert!(app.docs[i].buf.to_string().starts_with("// formatted by a\n"));
        app.config.formatter = [("*".to_string(), "lsp".to_string())].into();
        assert_eq!(app.format_doc(i, MANUAL_WAIT), Fmt::Changed("fake-a".into()));
        for l in &mut app.lsps {
            l.shutdown();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_actions_run_in_order_before_saving() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("onsave");
        let mut app = test_app(&dir);
        app.config.language_servers = fake_server(&exe, json!({"FAKE_ACTIONS": "1"}));
        app.config.code_actions_on_save =
            [("*".to_string(), vec!["source.organizeImports".to_string(), "source.fixAll".to_string()])].into();
        let f = dir.join("src/x.fk");
        let i = open_on_server(&mut app, &f, "x\n");
        app.docs[i].replace_text("y\n");
        app.save_active();
        // organizeImports (an edit) first, then fixAll (resolved): each saw the other's result.
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "// fixed\n// imports organized\ny\n");
        assert_eq!(app.docs[i].buf.to_string(), "// fixed\n// imports organized\ny\n");
        assert_eq!(status(&app), "Saved x.fk");
        assert!(app.lsps[0].log_text().contains("codeAction/resolve"));
        // One undo step per action, never a save of something else.
        let d = &mut app.docs[i];
        d.ed.undo(&mut d.buf);
        assert_eq!(app.docs[i].buf.to_string(), "// imports organized\ny\n");
        // A language whose list is empty runs nothing.
        let lang = app.docs[i].lang.unwrap_or("*").to_string();
        app.config.code_actions_on_save.insert(lang, Vec::new());
        app.config.code_actions_on_save.insert("*".into(), Vec::new());
        app.docs[i].replace_text("z\n");
        app.save_active();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "z\n");
        for l in &mut app.lsps {
            l.shutdown();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hung_server_never_blocks_saving_and_big_files_are_skipped() {
        let _g = lock();
        let Some(exe) = fake_exe() else { return eprintln!("fake_lsp example not built, skipping") };
        let dir = project("onsavehang");
        let mut app = test_app(&dir);
        app.config.language_servers = fake_server(&exe, json!({"FAKE_HANG_ACTIONS": "1"}));
        app.config.code_actions_on_save =
            [("*".to_string(), vec!["source.organizeImports".to_string(), "source.fixAll".to_string()])].into();
        let f = dir.join("src/x.fk");
        let i = open_on_server(&mut app, &f, "x\n");
        app.docs[i].replace_text("typed\n");
        let t0 = Instant::now();
        app.save_active();
        let took = t0.elapsed();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "typed\n", "saved as typed");
        assert!(took < ACTION_WAIT * 2, "the hung server was asked once, then skipped: {took:?}");
        let m = status(&app);
        assert!(m.starts_with("Saved x.fk, but source.organizeImports (fake-a) timed out"), "{m}");
        assert!(app.status.as_ref().unwrap().1 > Instant::now() + Duration::from_secs(5), "shown for long enough");
        // Above the big-file threshold nothing is asked at all.
        let big = "line\n".repeat(crate::bigfile::BIG_LINES + 10);
        app.docs[i].replace_text(&big);
        let t0 = Instant::now();
        app.save_active();
        assert!(t0.elapsed() < ACTION_WAIT, "no wait for a big file");
        assert_eq!(status(&app), "Saved x.fk");
        for l in &mut app.lsps {
            l.shutdown();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn action_kinds_match_sub_kinds_and_capabilities() {
        assert!(kind_matches("source.fixAll", Some("source.fixAll.eslint")));
        assert!(kind_matches("source.fixAll", Some("source.fixAll")));
        assert!(!kind_matches("source.fixAll", Some("source.fixAllX")));
        assert!(!kind_matches("source.fixAll", Some("source.organizeImports")));
        assert!(kind_matches("source.fixAll", None));
        assert!(offers_kind(&json!(true), "source.fixAll"));
        assert!(!offers_kind(&Value::Null, "source.fixAll"));
        assert!(!offers_kind(&json!(false), "source.fixAll"));
        assert!(offers_kind(&json!({"resolveProvider": true}), "source.fixAll"));
        let ruff = json!({"codeActionKinds": ["quickfix", "source.fixAll.ruff", "source.organizeImports.ruff"]});
        assert!(offers_kind(&ruff, "source.fixAll"));
        assert!(offers_kind(&ruff, "source.organizeImports"));
        assert!(!offers_kind(&json!({"codeActionKinds": ["quickfix"]}), "source.fixAll"));
        assert!(offers_kind(&json!({"codeActionKinds": ["source"]}), "source.organizeImports"));
    }
}
