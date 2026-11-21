//! What the chat composer's commands do on the App: the autocomplete menu (slash commands and
//! `@` files), running a typed `/command` (Lumen's own, or sent to the agent as typed), the
//! pickers for `/resume`, `/model`, `/permissions` and `/memory`, the permission mode, and the
//! files and images attached to a message. The rules are plain code in `slash`, `resume`,
//! `mention` and `menu`, all unit-tested; this is the glue.

use super::{
    chat::{self, PermMode, Role},
    mention,
    menu::{self, MenuKind},
    resume,
    slash::{self, Handling, Native},
    *,
};
use crate::app_overlay::{Action, Item};

/// Files listed for `@` mentions (a bigger project is cut here).
const FILES_MAX: usize = 30_000;
/// How long a listing of the project's files (or of the commands on disk) is trusted.
const TTL: Duration = Duration::from_secs(15);

fn pick(spec: String) -> Action {
    Action::AiPick(spec)
}

impl App {
    fn ai_chat_mut(&mut self) -> Option<&mut ChatSession> {
        self.ai.cur_mut().and_then(|s| s.chat.as_deref_mut())
    }

    /// The folder the session shown works in.
    fn ai_cwd(&self) -> PathBuf {
        self.ai.cur().map_or_else(|| self.root.clone(), |s| s.cwd.clone())
    }

    /// The agent of the chat shown.
    pub(super) fn ai_kind(&self) -> Option<backend::Kind> {
        self.ai.cur()?.chat.as_deref().map(|c| c.be.kind)
    }

    /// Say something in the transcript on Lumen's behalf (Markdown).
    pub(super) fn ai_note(&mut self, md: &str) {
        if let Some(cs) = self.ai_chat_mut() {
            cs.chat.note(md);
            cs.view.follow = true;
        }
        self.request_redraw();
    }

    /// Every command the chat shown offers: Lumen's, the agent's, the ones on disk.
    pub(super) fn ai_catalog(&mut self) -> Vec<slash::Cmd> {
        let cwd = self.ai_cwd();
        let Some(cs) = self.ai.cur().and_then(|s| s.chat.as_deref()) else { return Vec::new() };
        let kind = cs.be.kind;
        let meta = slash::Meta { commands: cs.chat.commands.clone(), terminal_only: cs.chat.terminal_only.clone() };
        let stale = self.ai.found_cmds.as_ref().is_none_or(|(p, t, _)| *p != cwd || t.elapsed() > TTL);
        if stale {
            let v = slash::discover(&cwd, crate::paths::home().as_deref());
            self.ai.found_cmds = Some((cwd, Instant::now(), v));
        }
        let found = self.ai.found_cmds.as_ref().map(|(_, _, v)| v.clone()).unwrap_or_default();
        slash::catalog(kind, found, &meta)
    }

    /// The project's files for `@` mentions, listed again when stale.
    fn ai_load_files(&mut self) {
        let cwd = self.ai_cwd();
        let stale = self.ai.files.as_ref().is_none_or(|(p, t, _)| *p != cwd || t.elapsed() > TTL);
        if stale {
            let all = crate::search::list_files_in(std::slice::from_ref(&cwd), FILES_MAX);
            let rel = mention::relative_files(&cwd, &all);
            self.ai.files = Some((cwd, Instant::now(), rel));
        }
    }

    /// Open, update or close the autocomplete menu for the draft (cheap when nothing changed).
    pub(super) fn ai_refresh_menu(&mut self) {
        if !self.ai_is_chat() {
            self.ai.menu = None;
            return;
        }
        let text = self.ai.composer.text().to_string();
        let caret = self.ai.composer.caret_byte();
        if self.ai.menu_for.0 == text && self.ai.menu_for.1 == caret {
            return;
        }
        self.ai.menu_for = (text.clone(), caret);
        if self.ai.menu_closed.as_deref() == Some(text.as_str()) {
            self.ai.menu = None;
            return;
        }
        self.ai.menu_closed = None;
        let menu = if slash::query(&text).is_some() {
            let cmds = self.ai_catalog();
            menu::slash(&cmds, &text)
        } else if mention::active(&text, caret).is_some() {
            self.ai_load_files();
            let all = self.ai.files.as_ref().map(|f| &f.2[..]).unwrap_or(&[]);
            menu::files(all, &text, caret)
        } else {
            None
        };
        self.ai.menu = menu;
    }

    /// A key while the menu is open: Up / Down move, Tab completes, Enter completes (and runs a
    /// command that needs no arguments), Esc closes it. False for any other key.
    pub(super) fn ai_menu_key(&mut self, key: &Key, ctrl: bool, shift: bool, alt: bool) -> bool {
        use winit::keyboard::NamedKey as N;
        if ctrl || alt || self.ai.menu.is_none() {
            return false;
        }
        match key {
            Key::Named(N::ArrowUp) if !shift => {
                if let Some(m) = self.ai.menu.as_mut() {
                    m.up();
                }
            }
            Key::Named(N::ArrowDown) if !shift => {
                if let Some(m) = self.ai.menu.as_mut() {
                    m.down();
                }
            }
            Key::Named(N::Escape) if !shift => {
                self.ai.menu_closed = Some(self.ai.composer.text().to_string());
                self.ai.menu = None;
            }
            Key::Named(N::Tab) if !shift => self.ai_menu_accept(false),
            Key::Named(N::Enter) if !shift => self.ai_menu_accept(true),
            _ => return false,
        }
        self.request_redraw();
        true
    }

    /// Take the selected row of the menu. `run`: a command that needs no arguments also runs.
    pub(super) fn ai_menu_accept(&mut self, run: bool) {
        let Some(m) = self.ai.menu.clone() else { return };
        let Some(e) = m.selected().cloned() else { return };
        self.ai.menu = None;
        match m.kind {
            MenuKind::Slash => {
                if let Some(why) = &e.off {
                    self.ai.composer.set_text("");
                    let name = e.label.split_whitespace().next().unwrap_or("").to_string();
                    return self.ai_note(&format!("`{name}` is not available here: {why}."));
                }
                self.ai.composer.set_text(&e.pick);
                if run && e.runs {
                    self.ai_send();
                }
            }
            MenuKind::Mention => {
                let caret = self.ai.composer.caret_byte();
                self.ai.composer.replace_bytes(m.start, caret, &mention::insertion(&e.pick));
            }
        }
        self.request_redraw();
    }

    // ───────────────────────────── slash commands ─────────────────────────────

    /// Run what was typed when it is a command of Lumen's. False means "not handled": the text
    /// goes to the agent as an ordinary message (its own commands, skills, custom commands).
    pub(super) fn ai_run_slash(&mut self, text: &str) -> bool {
        let Some((name, args)) = slash::parse(text) else { return false };
        let (name, args) = (name.to_string(), args.to_string());
        let cmds = self.ai_catalog();
        let Some(kind) = self.ai_kind() else { return false };
        match slash::find(&cmds, &name).map(|c| c.handling.clone()) {
            Some(Handling::Native(Native::Config)) if !args.is_empty() && kind == backend::Kind::Claude => false,
            Some(Handling::Native(n)) => {
                self.ai_native(n, &args);
                true
            }
            Some(Handling::Pass) => false,
            Some(Handling::Off(why)) => {
                self.ai_note(&format!("`/{name}` is not available here: {why}."));
                true
            }
            None if kind == backend::Kind::Claude => false,
            None => {
                self.ai_note(&format!("Unknown command `/{name}`. `/help` lists what works."));
                true
            }
        }
    }

    fn ai_native(&mut self, n: Native, args: &str) {
        match n {
            Native::Resume => self.ai_resume_menu(),
            Native::Clear => self.ai_clear(),
            Native::Model if args.is_empty() => self.ai_model_menu(),
            Native::Model => self.ai_set_model(args),
            Native::Help => {
                let cmds = self.ai_catalog();
                self.ai_note(&slash::help_text(&cmds));
            }
            Native::Cost => {
                let t = self.ai_cost_text();
                self.ai_note(&t);
            }
            Native::Status => {
                let t = self.ai_status_text();
                self.ai_note(&t);
            }
            Native::Config => self.open_settings(crate::settingsui::Page::Settings),
            Native::Agents => {
                let t = self.ai_agents_text();
                self.ai_note(&t);
            }
            Native::Memory => self.ai_memory_menu(),
            Native::Permissions => self.ai_mode_menu(),
            Native::Plan => {
                self.ai_set_mode(PermMode::Plan);
                if !args.is_empty()
                    && let Some(cs) = self.ai_chat_mut()
                {
                    cs.send(args);
                }
            }
            Native::Copy => {
                let last = self.ai.cur().and_then(|s| s.chat.as_deref()).and_then(|c| {
                    let m = c.chat.messages.iter().rposition(|m| m.role == Role::Assistant)?;
                    Some(chat_view::message_text(&c.chat, m)).filter(|t| !t.is_empty())
                });
                match last {
                    Some(t) => {
                        self.clip.set(&t);
                        self.say("Copied the last answer");
                    }
                    None => self.ai_note("There is no answer to copy yet."),
                }
            }
        }
    }

    /// `/clear`: a new conversation (the agent's process ends, the transcript empties).
    pub(super) fn ai_clear(&mut self) {
        self.ai.attachments.clear();
        if let Some(cs) = self.ai_chat_mut() {
            cs.be.reset(&mut cs.chat);
            cs.view = chat_view::ChatView::new();
        }
        self.request_redraw();
    }

    fn ai_cost_text(&self) -> String {
        let Some(cs) = self.ai.cur().and_then(|s| s.chat.as_deref()) else { return String::new() };
        let u = cs.chat.usage();
        let mut s = String::from("**Cost and tokens**\n\n");
        let line = chat::usage_line(&u);
        if line.is_empty() {
            s.push_str("Nothing spent yet in this conversation.\n");
        } else {
            s.push_str(&format!("- This conversation: {line}\n"));
        }
        if cs.chat.context_window > 0 {
            s.push_str(&format!(
                "- Context: {} of {} tokens ({:.0}%)\n",
                chat::format_tokens(cs.chat.context_used),
                chat::format_tokens(cs.chat.context_window),
                cs.chat.context_fraction() * 100.0
            ));
        }
        if cs.be.kind == backend::Kind::Claude {
            s.push_str("\n`/usage` shows your plan's limits; `/context` breaks the context down.\n");
        }
        s
    }

    fn ai_status_text(&self) -> String {
        let Some(s) = self.ai.cur() else { return String::new() };
        let Some(cs) = s.chat.as_deref() else { return String::new() };
        let mode = cs.chat.mode.unwrap_or(cs.be.mode);
        let mut t = format!("**Status**\n\n- Agent: {}\n", cs.be.kind.name());
        t.push_str(&format!(
            "- Model: {}\n",
            cs.chat.model.as_deref().or(cs.be.model.as_deref()).unwrap_or("the CLI's default")
        ));
        if cs.be.kind == backend::Kind::Claude {
            t.push_str(&format!("- Permission mode: {} ({})\n", mode.label(), mode.detail()));
        }
        t.push_str(&format!("- Folder: `{}`\n", crate::app_topbar::tilde(&s.cwd)));
        match &cs.chat.session_id {
            Some(id) => t.push_str(&format!("- Session: `{id}`\n")),
            None => t.push_str("- Session: not started (it starts with your first message)\n"),
        }
        if cs.chat.context_window > 0 {
            t.push_str(&format!(
                "- Context: {} of {} tokens ({:.0}%)\n",
                chat::format_tokens(cs.chat.context_used),
                chat::format_tokens(cs.chat.context_window),
                cs.chat.context_fraction() * 100.0
            ));
        }
        let usage = chat::usage_line(&cs.chat.usage());
        if !usage.is_empty() {
            t.push_str(&format!("- Usage: {usage}\n"));
        }
        t
    }

    fn ai_agents_text(&self) -> String {
        let cwd = self.ai.cur().map(|s| s.cwd.clone()).unwrap_or_else(|| self.root.clone());
        let mut found: Vec<(String, String)> = Vec::new();
        let mut dirs = vec![cwd.join(".claude/agents")];
        dirs.extend(crate::paths::home().map(|h| h.join(".claude/agents")));
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            let mut names: Vec<String> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect();
            names.sort();
            let place = if d.starts_with(&cwd) { "project" } else { "user" };
            found.extend(names.into_iter().map(|n| (n, place.to_string())));
        }
        let reported = self.ai.cur().and_then(|s| s.chat.as_deref()).map(|c| c.chat.agents.clone()).unwrap_or_default();
        let mut s = String::from("**Sub-agents**\n\n");
        for (n, place) in &found {
            s.push_str(&format!("- `{n}` ({place}, `.claude/agents/{n}.md`)\n"));
        }
        for n in reported.iter().filter(|n| !found.iter().any(|(f, _)| f == *n)) {
            s.push_str(&format!("- `{n}`\n"));
        }
        if found.is_empty() && reported.is_empty() {
            s.push_str("None yet: they are listed after your first message. Ask Claude to create one, or add a file to `.claude/agents/`.\n");
        } else {
            s.push_str("\nAsk Claude to use one by name, or to create a new one.\n");
        }
        s
    }

    // ───────────────────────────── pickers ─────────────────────────────

    /// `/resume`: the earlier conversations of this project, newest first.
    pub(super) fn ai_resume_menu(&mut self) {
        let Some(home) = crate::paths::home() else { return self.ai_note("Cannot find your home folder.") };
        let cwd = self.ai_cwd();
        let cur = self.ai.cur().and_then(|s| s.chat.as_deref()).and_then(|c| c.chat.session_id.clone());
        let list = resume::list(&home, &cwd);
        if list.is_empty() {
            return self.ai_note(&format!(
                "No earlier conversations for this folder (looked in `{}`).",
                crate::app_topbar::tilde(&resume::dir_of(&home, &cwd))
            ));
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let items: Vec<Item> = list
            .iter()
            .map(|s| {
                let here = if cur.as_deref() == Some(s.id.as_str()) { "current · " } else { "" };
                Item {
                    label: s.title.clone(),
                    detail: format!(
                        "{here}{} · {} message{} · {}",
                        resume::ago(now, s.modified),
                        s.messages,
                        if s.messages == 1 { "" } else { "s" },
                        s.id.chars().take(8).collect::<String>()
                    ),
                    action: pick(format!("resume:{}", s.id)),
                }
            })
            .collect();
        self.open_locations("Resume a conversation", items);
    }

    /// Show the conversation `id` and continue it with the next message.
    fn ai_resume(&mut self, id: &str) {
        let Some(home) = crate::paths::home() else { return };
        let cwd = self.ai_cwd();
        if self.ai.cur().and_then(|s| s.chat.as_deref()).is_some_and(|c| c.chat.working()) {
            return self.say("Stop the agent first (Esc), then resume");
        }
        let path = resume::dir_of(&home, &cwd).join(format!("{id}.jsonl"));
        let messages = resume::load(&path);
        let title = resume::inspect(&path).map_or_else(|| id.to_string(), |i| i.title);
        let n = messages.len();
        if let Some(cs) = self.ai_chat_mut() {
            cs.be.resume(&mut cs.chat, id, messages);
            cs.view = chat_view::ChatView::new();
            cs.chat.note(&format!("Resumed **{title}** ({n} messages). Your next message continues it."));
        }
        self.ai.attachments.clear();
        self.ai.composer_focus = true;
        self.request_redraw();
    }

    /// `/model` without a name: the models of this agent.
    pub(super) fn ai_model_menu(&mut self) {
        let Some(kind) = self.ai_kind() else { return };
        let cur =
            self.ai.cur().and_then(|s| s.chat.as_deref()).and_then(|c| c.chat.model.clone().or(c.be.model.clone()));
        let items: Vec<Item> = slash::models(kind)
            .into_iter()
            .map(|(id, desc)| {
                let on = cur.as_deref().is_some_and(|c| c == id || c.contains(id));
                Item {
                    label: id.to_string(),
                    detail: format!("{}{desc}", if on { "current · " } else { "" }),
                    action: pick(format!("model:{id}")),
                }
            })
            .collect();
        self.open_locations("Model (or /model <name> for any other)", items);
    }

    fn ai_set_model(&mut self, name: &str) {
        let name = name.trim().to_string();
        if let Some(cs) = self.ai_chat_mut() {
            cs.be.set_model(&mut cs.chat, &name);
        }
        self.say(format!("Model: {name}"));
        self.request_redraw();
    }

    /// `/permissions` and the mode chip: the four permission modes.
    pub(super) fn ai_mode_menu(&mut self) {
        if self.ai_kind() != Some(backend::Kind::Claude) {
            let name = self.ai_kind().map_or("This agent", backend::Kind::name);
            return self.say(format!("Permission modes are Claude Code's; {name} runs with a fixed sandbox here"));
        }
        let cur = self.ai_mode();
        let items: Vec<Item> = PermMode::ALL
            .into_iter()
            .map(|m| Item {
                label: m.label().to_string(),
                detail: format!("{}{}", if m == cur { "current · " } else { "" }, m.detail()),
                action: pick(format!("mode:{}", m.id())),
            })
            .collect();
        self.open_locations("Permission mode", items);
    }

    /// The permission mode of the chat shown.
    pub(super) fn ai_mode(&self) -> PermMode {
        self.ai.cur().and_then(|s| s.chat.as_deref()).map_or(PermMode::default(), |c| c.chat.mode.unwrap_or(c.be.mode))
    }

    pub(super) fn ai_set_mode(&mut self, mode: PermMode) {
        if self.ai_kind() != Some(backend::Kind::Claude) {
            return self.ai_mode_menu();
        }
        if let Some(cs) = self.ai_chat_mut() {
            cs.be.set_mode(&mut cs.chat, mode);
        }
        self.say(format!("Mode: {} — {}", mode.label(), mode.detail()));
        self.request_redraw();
    }

    /// Shift+Tab: the next mode.
    pub(super) fn ai_cycle_mode(&mut self) {
        if self.ai_kind() != Some(backend::Kind::Claude) {
            return;
        }
        let next = self.ai_mode().next();
        self.ai_set_mode(next);
    }

    /// `/memory`: the instruction and memory files, to open in the editor.
    fn ai_memory_menu(&mut self) {
        let cwd = self.ai_cwd();
        let kind = self.ai_kind();
        let home = crate::paths::home();
        let mut files: Vec<(PathBuf, &str)> = Vec::new();
        match kind {
            Some(backend::Kind::Gemini) => files.push((cwd.join("GEMINI.md"), "project")),
            Some(backend::Kind::Codex) => files.push((cwd.join("AGENTS.md"), "project")),
            _ => {
                files.push((cwd.join("CLAUDE.md"), "project"));
                files.push((cwd.join(".claude/CLAUDE.md"), "project"));
                files.push((cwd.join("CLAUDE.local.md"), "project, not shared"));
            }
        }
        if let Some(h) = &home {
            match kind {
                Some(backend::Kind::Gemini) => files.push((h.join(".gemini/GEMINI.md"), "user")),
                Some(backend::Kind::Codex) => files.push((h.join(".codex/AGENTS.md"), "user")),
                _ => files.push((h.join(".claude/CLAUDE.md"), "user")),
            }
        }
        let memory = self.ai.cur().and_then(|s| s.chat.as_deref()).map(|c| c.chat.memory.clone()).unwrap_or_default();
        for d in memory {
            let d = PathBuf::from(d);
            if let Ok(rd) = std::fs::read_dir(&d) {
                let mut v: Vec<PathBuf> =
                    rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect();
                v.sort();
                files.extend(v.into_iter().map(|p| (p, "auto memory")));
            }
        }
        let mut items: Vec<Item> = files
            .iter()
            .filter(|(p, _)| p.is_file())
            .map(|(p, what)| Item {
                label: p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                detail: format!("{what} · {}", crate::app_topbar::tilde(p)),
                action: pick(format!("memory:{}", p.display())),
            })
            .collect();
        if let Some((first, _)) = files.first().filter(|(p, _)| !p.is_file()) {
            items.push(Item {
                label: format!(
                    "Create {}",
                    first.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                ),
                detail: "Instructions the agent reads at the start of every conversation".into(),
                action: pick(format!("memnew:{}", first.display())),
            });
        }
        self.open_locations("Memory and instructions", items);
    }

    /// A row picked in one of the chat's pickers (`resume:<id>`, `model:<name>`, `mode:<id>`,
    /// `memory:<path>`, `memnew:<path>`).
    pub fn ai_pick_chat(&mut self, spec: &str) {
        let Some((what, arg)) = spec.split_once(':') else { return };
        match what {
            "resume" => self.ai_resume(arg),
            "model" => self.ai_set_model(arg),
            "mode" => {
                if let Some(m) = PermMode::from_id(arg) {
                    self.ai_set_mode(m);
                }
            }
            "memory" => {
                self.push_nav();
                self.open_path(Path::new(arg), None);
                self.ai.focused = false;
            }
            "memnew" => {
                let p = PathBuf::from(arg);
                if !p.exists() {
                    let _ = std::fs::write(&p, "# Project instructions\n\n");
                }
                self.push_nav();
                self.open_path(&p, None);
                self.ai.focused = false;
            }
            _ => {}
        }
    }

    // ───────────────────────────── attachments ─────────────────────────────

    /// Attach a file: a picture the agent can look at, or any file as an `@` mention.
    pub fn ai_attach_path(&mut self, path: &Path) {
        if !path.is_file() || self.ai.attachments.iter().any(|a| a.path == path) {
            return;
        }
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.ai.attachments.push(Attachment { name, path: path.to_path_buf(), image: mention::image_type(path) });
        self.ai.composer_focus = true;
        self.request_redraw();
    }

    /// Ctrl+V with an image on the clipboard: saved to a temporary file and attached.
    pub(super) fn ai_paste_image(&mut self) -> bool {
        let Some((media, bytes)) = self.clip.get_image() else { return false };
        let ext = media.rsplit('/').next().unwrap_or("png");
        let dir = std::env::temp_dir().join(format!("lumen-ai-{}", std::process::id()));
        let n = self.ai.attachments.len() + 1;
        let path = dir.join(format!(
            "pasted-image-{}-{n}.{ext}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
        ));
        if std::fs::create_dir_all(&dir).is_err() || std::fs::write(&path, &bytes).is_err() {
            self.say("Could not keep the pasted image");
            return true;
        }
        self.ai.attachments.push(Attachment { name: format!("image.{ext}"), path, image: Some(media) });
        self.request_redraw();
        true
    }

    /// The message to send and the images that go with it: the typed text, then a line per file
    /// attached (`@path`) or per picture (inline for Claude Code, by path for the others).
    pub(super) fn ai_compose_message(&mut self, text: &str) -> (String, Vec<(String, Vec<u8>)>) {
        let kind = self.ai_kind();
        let cwd = self.ai_cwd();
        let mut out = text.trim_end().to_string();
        let mut images = Vec::new();
        let atts = std::mem::take(&mut self.ai.attachments);
        for a in atts {
            let inline = a.image.filter(|_| kind == Some(backend::Kind::Claude));
            match (inline, std::fs::read(&a.path)) {
                (Some(media), Ok(bytes)) => {
                    out.push_str(&format!("\n[image: {}]", a.name));
                    images.push((media.to_string(), bytes));
                }
                _ => {
                    let shown = a.path.strip_prefix(&cwd).unwrap_or(&a.path).to_string_lossy().into_owned();
                    out.push('\n');
                    out.push_str(composer::mention(&shown).trim_end());
                }
            }
        }
        (out, images)
    }

    /// Message `mi` goes back into the composer to be edited and sent again.
    pub(super) fn ai_edit_message(&mut self, text: &str) {
        self.ai.composer.set_text(text);
        self.ai.composer_focus = true;
        self.ai.history.reset();
        self.request_redraw();
    }
}
