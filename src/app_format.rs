//! Formatting: one chain for "Format document" and format-on-save.
//!
//! Order: the project's own formatter (Biome / Prettier, run on stdin) → the file's language
//! server (`textDocument/formatting`, tsgo for TS/JS) → a built-in JSON re-indenter. When nothing
//! applies the user is told, once per file type and session.

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

#[derive(Debug, PartialEq, Eq)]
pub enum Fmt {
    /// The text was rewritten by this formatter.
    Changed(String),
    /// Already formatted.
    Same(String),
    Failed(String),
    /// The server did not answer in time (the text is untouched).
    TimedOut(String),
    NoFormatter,
}

impl App {
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
        if let Some(f) = crate::format::detect(&path) {
            let text = self.docs[i].buf.to_string();
            return match crate::format::run(&f, &text) {
                Ok(new) if new == text => Fmt::Same(f.name.into()),
                Ok(new) => {
                    self.docs[i].replace_text(&new);
                    Fmt::Changed(f.name.into())
                }
                Err(e) => Fmt::Failed(e),
            };
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

    /// Format-on-save step for document `i`. Returns what to tell the user *after* saving
    /// (formatting problems must not be overwritten by the plain "Saved" message).
    pub fn format_for_save(&mut self, i: usize) -> Option<String> {
        if !self.format_on_save_on() {
            return None;
        }
        let path = self.docs.get(i)?.path()?.to_path_buf();
        match self.format_doc(i, SAVE_WAIT) {
            Fmt::Changed(_) | Fmt::Same(_) => None,
            Fmt::Failed(e) => Some(format!("formatting failed: {e}")),
            Fmt::TimedOut(n) => Some(format!("{n} did not answer in time, so it was saved unformatted")),
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
            Fmt::NoFormatter => {
                let key = crate::format::type_key(&path);
                self.fmt_told.insert(key.clone());
                self.say_for(no_formatter_msg(&key), 8);
            }
        }
    }
}

pub(crate) const TIMEOUT: &str = "timeout";

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
        d.canonicalize().unwrap()
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
}
