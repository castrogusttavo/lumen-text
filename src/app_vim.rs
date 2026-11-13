//! Glue between the Vim engine (`vim`), the keymap presets (`keymap`) and the app: key routing,
//! the status pill, the `vim_mode` / `keymap_preset` settings, the palette commands, ex commands
//! and `q` / `@` macros (recorded and replayed by `macros`).

use crate::{
    app::{App, Focus, KeyIn},
    app_overlay::Cmd,
    buffer::Sel,
    clipboard::Clipboard,
    ui::{Canvas, Rect},
    vim::{Clip, Ctx, ExCmd, Feed, Mode, VKey, Vim},
};
use serde_json::Value;
use std::path::PathBuf;
use winit::keyboard::{Key, NamedKey};

struct ClipAdapter<'a>(&'a mut Clipboard);

impl Clip for ClipAdapter<'_> {
    fn get(&mut self) -> String {
        self.0.get()
    }
    fn set(&mut self, text: &str) {
        self.0.set(text);
    }
}

/// The engine's key for a key press, or `None` when the key is not the engine's (global
/// shortcuts, function keys, anything in Insert mode that the editor types itself).
pub(crate) fn vkey_of(key: &Key, text: Option<&str>, ctrl: bool, shift: bool, insert: bool) -> Option<VKey> {
    match key {
        Key::Named(NamedKey::Escape) => Some(VKey::Esc),
        Key::Named(NamedKey::Enter) if !ctrl => Some(VKey::Enter),
        Key::Named(NamedKey::Backspace) if !ctrl => Some(VKey::Backspace),
        Key::Named(NamedKey::Space) if !ctrl => Some(VKey::Char(' ')),
        Key::Named(n) if !insert && !ctrl && !shift => match n {
            NamedKey::ArrowLeft => Some(VKey::Char('h')),
            NamedKey::ArrowRight => Some(VKey::Char('l')),
            NamedKey::ArrowUp => Some(VKey::Char('k')),
            NamedKey::ArrowDown => Some(VKey::Char('j')),
            NamedKey::Home => Some(VKey::Char('0')),
            NamedKey::End => Some(VKey::Char('$')),
            NamedKey::Delete => Some(VKey::Char('x')),
            _ => None,
        },
        Key::Character(c) if ctrl => {
            if shift {
                return None;
            }
            let ch = c.chars().next()?.to_ascii_lowercase();
            match ch {
                'r' | 'u' | 'd' if !insert => Some(VKey::Ctrl(ch)),
                '[' => Some(VKey::Ctrl('[')),
                _ => None,
            }
        }
        Key::Character(c) => text.and_then(|t| t.chars().next()).or_else(|| c.chars().next()).map(VKey::Char),
        _ => None,
    }
}

impl App {
    /// Called for keys while the editor has the focus and Vim mode is on. `Some(quit)` when the
    /// engine took the key; `None` lets the normal handling run (typing in Insert mode, shortcuts).
    pub(crate) fn vim_key(&mut self, ev: &KeyIn) -> Option<bool> {
        let (ctrl, shift) = (self.mods.control_key(), self.mods.shift_key());
        if self.mods.alt_key() || self.mods.super_key() {
            return None;
        }
        let insert = self.vim.as_ref()?.mode == Mode::Insert;
        let d = self.docs.get(self.active)?;
        if d.buf.read_only {
            return None;
        }
        let vk = vkey_of(&ev.key, ev.text.as_deref(), ctrl, shift, insert)?;
        // A completion popup or suggestion gets its Esc first.
        if insert && vk == VKey::Esc && self.completion.is_some() {
            return None;
        }
        let page = d.visible_lines() as usize;
        let feed = {
            let vim = self.vim.as_mut()?;
            let d = self.docs.get_mut(self.active)?;
            if !insert {
                d.ed.clear_extra();
            }
            // Tree-sitter text objects (`if`/`af` function, `ic`/`ac` class, `ia`/`aa` argument,
            // `iC`/`aC` comment) for the file types with a bundled grammar.
            vim.objects = crate::structural::Lang::from_path(std::path::Path::new(&d.name))
                .map(|lang| Box::new(TsObjects(lang)) as Box<dyn crate::vim::TextObjects>);
            let indent = d.ed.indent.clone();
            let mut clip = ClipAdapter(&mut self.clip);
            let mut cx = Ctx { buf: &mut d.buf, sel: &mut d.ed.sel, clip: &mut clip, indent: &indent, page };
            vim.feed(vk, &mut cx)
        };
        let d = self.docs.get_mut(self.active)?;
        d.reveal = true;
        match feed {
            Feed::Pass => return None,
            Feed::Done => {}
            Feed::Undo(n) => {
                for _ in 0..n {
                    d.ed.undo(&mut d.buf);
                }
                if let Some(v) = self.vim.as_mut() {
                    v.settle(&d.buf, &mut d.ed.sel);
                }
            }
            Feed::Redo(n) => {
                for _ in 0..n {
                    d.ed.redo(&mut d.buf);
                }
                if let Some(v) = self.vim.as_mut() {
                    v.settle(&d.buf, &mut d.ed.sel);
                }
            }
            Feed::Message(m) => self.say(m),
            Feed::Ex(c) => self.vim_ex(c),
            Feed::MacroStart(r) => {
                self.macros.recording = Some(Vec::new());
                self.say(format!("recording @{r}"));
            }
            Feed::MacroStop(r) => {
                let mut keys = self.macros.recording.take().unwrap_or_default();
                // The `q` that stopped the recording is not part of it.
                keys.pop();
                if keys.is_empty() {
                    self.say(format!("@{r} is empty: nothing recorded"));
                } else {
                    let n = keys.len();
                    self.macros.last = Some(keys.clone());
                    if let Some(v) = self.vim.as_mut() {
                        v.macro_regs.insert(r, keys);
                    }
                    self.say(format!("recorded @{r}: {n} keys"));
                }
            }
            Feed::MacroPlay(r, n) => {
                let keys = self.vim.as_ref().and_then(|v| v.macro_regs.get(&r).cloned());
                match keys {
                    Some(keys) => self.play_keys(&keys, n),
                    None => self.say(format!("register @{r} is empty")),
                }
            }
        }
        Some(self.after_key(None))
    }

    /// `:w`, `:q`, `:wq`, `:e` from the Vim command line.
    fn vim_ex(&mut self, c: ExCmd) {
        match c {
            ExCmd::Write => self.save_active(),
            ExCmd::Quit { force } => {
                let a = self.active;
                self.close_tab(a, force);
            }
            ExCmd::WriteQuit => {
                self.save_active();
                let a = self.active;
                self.close_tab(a, false);
            }
            ExCmd::Edit(p) => {
                let path = PathBuf::from(&p);
                let full = if path.is_absolute() { path } else { self.root.join(path) };
                self.open_path(&full, None);
            }
        }
    }

    /// A keymap row `key:ctrl+x,delete`: press those keys, without the preset (so `Ctrl+W` bound
    /// to `key:ctrl+x` cuts instead of looping) and without recording them in a macro.
    pub(crate) fn inject_keys(&mut self, spec: &str) {
        let tokens: Vec<String> = spec.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        let keys = crate::macros::from_tokens(&tokens);
        let (held, was) = (self.mods, self.macros.replaying);
        self.keymap.bypass = true;
        self.macros.replaying = true;
        for k in &keys {
            self.mods = k.mods;
            let _ = self.handle_key(&KeyIn { key: k.key.clone(), text: k.text.clone() });
        }
        self.mods = held;
        self.macros.replaying = was;
        self.keymap.bypass = false;
    }

    // ───────────── settings and commands ─────────────

    /// At startup: the configured preset and Vim mode.
    pub(crate) fn vim_startup(&mut self) {
        let preset = crate::keymap::normalize_preset(Some(&self.config.keymap_preset));
        self.config.keymap_preset = preset.to_string();
        self.keymap.set_preset(preset);
        if self.config.vim_mode || preset == "vim" {
            self.config.vim_mode = true;
            self.vim = Some(Box::new(Vim::new()));
        }
    }

    fn vim_switch(&mut self, on: bool) {
        self.config.vim_mode = on;
        if on == self.vim.is_some() {
            return;
        }
        self.vim = on.then(|| Box::new(Vim::new()));
        if let Some(d) = self.docs.get_mut(self.active) {
            let h = d.ed.sel.head;
            d.ed.sel = Sel::caret(h);
            // The engine may have been in the middle of a grouped edit.
            d.buf.close_group();
        }
    }

    /// `vim_mode` from the settings or the palette.
    pub(crate) fn vim_set_enabled(&mut self, on: bool) {
        self.vim_switch(on);
        if !on && self.config.keymap_preset == "vim" {
            self.config.keymap_preset = "vscode".into();
            self.keymap.set_preset("vscode");
        }
        self.config.save();
        self.say(if on { "Vim mode on (Esc for Normal mode)" } else { "Vim mode off" });
        self.request_redraw();
    }

    /// `keymap_preset` from the settings or the palette.
    pub(crate) fn set_keymap_preset(&mut self, name: &str) {
        let name = crate::keymap::normalize_preset(Some(name));
        let prev = std::mem::replace(&mut self.config.keymap_preset, name.to_string());
        self.keymap.set_preset(name);
        if name == "vim" {
            self.vim_switch(true);
        } else if prev == "vim" {
            self.vim_switch(false);
        }
        self.config.save();
        self.say(format!("keymap preset: {name}"));
        self.request_redraw();
    }

    pub(crate) fn vim_apply_setting(&mut self, key: &str, v: &Value) -> bool {
        match key {
            "vim_mode" => self.vim_set_enabled(v.as_bool().unwrap_or(false)),
            "keymap_preset" => self.set_keymap_preset(v.as_str().unwrap_or("vscode")),
            _ => return false,
        }
        true
    }

    pub(crate) fn run_vim_cmd(&mut self, c: Cmd) -> bool {
        match c {
            Cmd::ToggleVimMode => self.vim_set_enabled(self.vim.is_none()),
            Cmd::KeymapPresetVscode => self.set_keymap_preset("vscode"),
            Cmd::KeymapPresetEmacs => self.set_keymap_preset("emacs"),
            Cmd::KeymapPresetVim => self.set_keymap_preset("vim"),
            _ => return false,
        }
        true
    }

    // ───────────── status bar ─────────────

    /// Status bar: the mode pill (and the command line or pending keys). Returns the x after it.
    pub fn draw_vim_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        if self.focus != Focus::Editor {
            return x;
        }
        let Some(v) = self.vim.as_ref() else { return x };
        let label = v.status();
        let c = self.theme.chrome;
        let r = self.layout.status;
        let y = r.y + (r.h - 18) / 2;
        let lw = self.tx.ui_width(&label, true);
        let pill = Rect::new(x + 2, r.y + 4, lw + 18, r.h - 8);
        let tint = if v.mode == Mode::Insert { c.accent } else { c.tab_inactive_fg };
        cv.fill_round(pill, (pill.h / 2).min(10), tint.with_alpha(0x38));
        self.tx.draw_ui(cv, pill.x + 9, y, &label, c.accent_text, pill, true);
        pill.right() + 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app_unsaved::testkit::{lock, test_app, tmp},
        script::send_keys,
    };

    fn text(app: &App) -> String {
        app.docs[0].buf.to_string()
    }

    #[test]
    fn vim_is_off_by_default_and_costs_nothing() {
        let _g = lock();
        let dir = tmp("vim-off");
        let mut app = test_app(&dir);
        assert!(app.vim.is_none());
        app.docs[0].replace_text("abc\n");
        send_keys(&mut app, "type:x");
        assert!(text(&app).contains('x'), "typing inserts when Vim mode is off");
    }

    #[test]
    fn keys_map_to_the_engine() {
        let ch = |s: &str| Key::Character(s.into());
        assert_eq!(vkey_of(&ch("a"), Some("a"), false, false, false), Some(VKey::Char('a')));
        assert_eq!(vkey_of(&ch("A"), Some("A"), false, true, false), Some(VKey::Char('A')));
        assert_eq!(vkey_of(&ch("r"), None, true, false, false), Some(VKey::Ctrl('r')));
        assert_eq!(vkey_of(&ch("r"), None, true, false, true), None, "Insert keeps Ctrl+R");
        assert_eq!(vkey_of(&ch("s"), None, true, false, false), None, "Ctrl+S stays a shortcut");
        assert_eq!(vkey_of(&ch("["), None, true, false, true), Some(VKey::Ctrl('[')));
        assert_eq!(vkey_of(&Key::Named(NamedKey::Escape), None, false, false, true), Some(VKey::Esc));
        assert_eq!(vkey_of(&Key::Named(NamedKey::ArrowLeft), None, false, false, false), Some(VKey::Char('h')));
        assert_eq!(vkey_of(&Key::Named(NamedKey::ArrowLeft), None, false, false, true), None, "Insert arrows move");
        assert_eq!(vkey_of(&Key::Named(NamedKey::ArrowLeft), None, false, true, false), None, "Shift+arrow selects");
        assert_eq!(vkey_of(&Key::Named(NamedKey::F5), None, false, false, false), None);
    }

    #[test]
    fn modal_editing_through_the_key_handler() {
        let _g = lock();
        let dir = tmp("vim-on");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("one two\nthree\n");
        let d = &mut app.docs[0];
        d.ed.click(&mut d.buf, 0, false);
        app.run_cmd(Cmd::ToggleVimMode);
        assert!(app.vim.is_some() && app.config.vim_mode);
        // Normal mode: keys are commands, not text.
        send_keys(&mut app, "type:dw");
        assert_eq!(text(&app), "two\nthree\n");
        // Insert mode types through the editor, Esc leaves it; one undo step.
        send_keys(&mut app, "type:Ahello;escape");
        assert_eq!(text(&app), "twohello\nthree\n");
        assert_eq!(app.vim.as_ref().unwrap().mode, Mode::Normal);
        send_keys(&mut app, "type:u");
        assert_eq!(text(&app), "two\nthree\n");
        // `.` repeats, global shortcuts still work.
        send_keys(&mut app, "type:x.");
        assert_eq!(text(&app), "t\nthree\n");
        send_keys(&mut app, "ctrl+z");
        assert_eq!(text(&app), "tw\nthree\n");
        app.run_cmd(Cmd::ToggleVimMode);
        assert!(app.vim.is_none());
        send_keys(&mut app, "type:q");
        assert!(text(&app).contains('q'));
    }

    #[test]
    fn macros_recorded_with_q_replay_with_at() {
        let _g = lock();
        let dir = tmp("vim-macro");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("a\nb\nc\nd\n");
        let d = &mut app.docs[0];
        d.ed.click(&mut d.buf, 0, false);
        app.run_cmd(Cmd::ToggleVimMode);
        send_keys(&mut app, "type:qaA!;escape;type:jq");
        assert!(app.macros.recording.is_none());
        assert_eq!(text(&app), "a!\nb\nc\nd\n");
        send_keys(&mut app, "type:2@a");
        assert_eq!(text(&app), "a!\nb!\nc!\nd\n");
        send_keys(&mut app, "type:@@");
        assert_eq!(text(&app), "a!\nb!\nc!\nd!\n");
    }

    #[test]
    fn the_presets_switch_through_settings_and_the_palette() {
        let _g = lock();
        let dir = tmp("vim-preset");
        let mut app = test_app(&dir);
        app.run_cmd(Cmd::KeymapPresetVim);
        assert!(app.vim.is_some());
        assert_eq!(app.config.keymap_preset, "vim");
        app.apply_setting("keymap_preset", &serde_json::json!("emacs"));
        assert!(app.vim.is_none(), "leaving the vim preset turns modal editing off");
        assert_eq!(app.config.keymap_preset, "emacs");
        // Emacs: Ctrl+F moves right, Ctrl+K kills to the end of the line.
        app.docs[0].replace_text("abc def\nxyz\n");
        let d = &mut app.docs[0];
        d.ed.click(&mut d.buf, 0, false);
        send_keys(&mut app, "ctrl+f;ctrl+f");
        assert_eq!(app.docs[0].ed.sel.head, 2);
        send_keys(&mut app, "ctrl+k");
        assert_eq!(text(&app), "ab\nxyz\n");
        // The default shortcuts still work where the preset does not take them.
        send_keys(&mut app, "ctrl+z");
        assert_eq!(text(&app), "abc def\nxyz\n");
        app.run_cmd(Cmd::KeymapPresetVscode);
        assert_eq!(app.config.keymap_preset, "vscode");
        send_keys(&mut app, "ctrl+f");
        assert!(app.find.is_some(), "Ctrl+F is Find again");
    }
}

/// Vim text objects answered by the tree-sitter grammar of the file (see `structural`).
struct TsObjects(crate::structural::Lang);

impl crate::vim::TextObjects for TsObjects {
    fn object(&self, buf: &crate::buffer::Buffer, pos: usize, kind: char, inner: bool) -> Option<(usize, usize)> {
        use crate::structural::{self, TextObject};
        let obj = match kind {
            'f' => TextObject::Function,
            'c' => TextObject::Class,
            'a' => TextObject::Argument,
            'C' => TextObject::Comment,
            _ => return None,
        };
        let rope = buf.snapshot();
        if rope.len_bytes() > structural::MAX_BYTES {
            return None;
        }
        let src = rope.to_string();
        let tree = structural::parse(self.0, &src)?;
        let at = rope.char_to_byte(pos.min(rope.len_chars()));
        // An empty selection at the caret: the smallest object around it.
        let r = structural::text_object(&tree, &src, at..at, obj, inner)?;
        Some((rope.byte_to_char(r.start), rope.byte_to_char(r.end)))
    }
}
