//! Keyboard macros. `Ctrl+Shift+R` starts recording the keys the editor receives (commands
//! reached by shortcut or through the palette, typed text; never the mouse) and stops it;
//! `Ctrl+Alt+R` replays the last macro, "Macro: Replay…" replays it N times. A replay goes
//! through the normal key handling and becomes one undo step in every document it edited.
//!
//! Macros can be saved by name in `macros.json` (next to `config.json`) as `--keys` script tokens
//! (`src/script.rs`), and bound in `keybindings.json` with a `macro:` id:
//! `macros.json`: `{"wrap": ["home", "type:<b>", "end", "type:</b>"]}`,
//! `keybindings.json`: `{"ctrl+alt+1": "macro:wrap"}`.

use crate::{
    app::{App, KeyIn},
    ui::{Canvas, Rect},
};
use std::{collections::BTreeMap, path::PathBuf};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Prefix of a keybinding id that runs a saved macro.
pub const BIND_PREFIX: &str = "macro:";
/// Most replays "Macro: Replay…" accepts.
pub const MAX_TIMES: usize = 10_000;

/// One recorded key press.
#[derive(Clone, Debug, PartialEq)]
pub struct MacroKey {
    pub mods: ModifiersState,
    pub key: Key,
    pub text: Option<String>,
}

#[derive(Default)]
pub struct Macros {
    /// Keys recorded so far, while recording.
    pub recording: Option<Vec<MacroKey>>,
    /// Index of the latest key recorded with no overlay open: where the command that stops the
    /// recording began (its shortcut, or the key that opened the palette).
    mark: usize,
    pub last: Option<Vec<MacroKey>>,
    pub replaying: bool,
}

fn modifier_only(k: &Key) -> bool {
    matches!(
        k,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::CapsLock
                | NamedKey::NumLock
        )
    )
}

/// Typed text of a key press: printable text without Ctrl / Alt.
fn typed(k: &MacroKey) -> Option<&str> {
    let t = k.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control))?;
    let plain = !k.mods.control_key() && !k.mods.alt_key() && !k.mods.super_key();
    (plain && matches!(k.key, Key::Character(_) | Key::Named(NamedKey::Space))).then_some(t)
}

/// Keys as `--keys` tokens: runs of typed text become `type:...`, the rest combos ("ctrl+shift+p").
/// Keys the syntax cannot name are left out.
pub fn to_tokens(keys: &[MacroKey]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut run = String::new();
    for k in keys {
        if let Some(t) = typed(k) {
            run.push_str(t);
            continue;
        }
        if !run.is_empty() {
            out.push(format!("type:{}", std::mem::take(&mut run)));
        }
        let name = match &k.key {
            Key::Character(c) => c.to_string(),
            Key::Named(n) => match crate::script::named_token(*n) {
                Some(n) => n.to_string(),
                None => continue,
            },
            _ => continue,
        };
        let mut tok = String::new();
        for (on, m) in [(k.mods.control_key(), "ctrl+"), (k.mods.shift_key(), "shift+"), (k.mods.alt_key(), "alt+")] {
            if on {
                tok.push_str(m);
            }
        }
        tok.push_str(&name);
        out.push(tok);
    }
    if !run.is_empty() {
        out.push(format!("type:{run}"));
    }
    out
}

/// Tokens back into key presses (unknown tokens are skipped).
pub fn from_tokens(tokens: &[String]) -> Vec<MacroKey> {
    tokens
        .iter()
        .flat_map(|t| crate::script::parse_token(t))
        .map(|(mods, ki)| MacroKey { mods, key: ki.key, text: ki.text })
        .collect()
}

fn store_path() -> Option<PathBuf> {
    crate::config::default_path().map(|p| p.with_file_name("macros.json"))
}

/// Saved macros: name -> tokens (a missing or broken file is an empty list).
pub fn load_saved() -> BTreeMap<String, Vec<String>> {
    store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Add or replace one saved macro.
pub fn save_named(name: &str, tokens: Vec<String>) -> std::io::Result<PathBuf> {
    let path = store_path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    let mut all = load_saved();
    all.insert(name.to_string(), tokens);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&all).map_err(std::io::Error::other)?;
    crate::replace::write_atomic(&path, format!("{text}\n").as_bytes())?;
    Ok(path)
}

impl App {
    /// Called for every key press before it is handled: records it while recording.
    pub fn macro_record(&mut self, ev: &KeyIn) {
        let m = &mut self.macros;
        if m.replaying || modifier_only(&ev.key) {
            return;
        }
        let Some(rec) = m.recording.as_mut() else { return };
        if self.overlay.is_none() {
            m.mark = rec.len();
        }
        rec.push(MacroKey { mods: self.mods, key: ev.key.clone(), text: ev.text.clone() });
    }

    /// Drop the keys that invoked the macro command being run (its shortcut, a chord, or the
    /// palette keys) from the recording.
    fn macro_trim(&mut self) {
        let mark = self.macros.mark;
        let Some(rec) = self.macros.recording.as_ref() else { return };
        let Some(k) = rec.get(mark) else { return };
        let (ctrl, shift, alt) = (k.mods.control_key(), k.mods.shift_key(), k.mods.alt_key());
        let id = self.keymap.lookup(ctrl, shift, alt, &k.key).map(str::to_string);
        let is_macro = |id: &str| id == "Commands" || id.starts_with("Macro") || id.starts_with(BIND_PREFIX);
        let cut = if id.as_deref().is_some_and(is_macro) {
            Some(mark)
        } else if mark > 0 {
            // The second stroke of a chord: the first one goes too.
            let p = &rec[mark - 1];
            let (pc, ps, pa) = (p.mods.control_key(), p.mods.shift_key(), p.mods.alt_key());
            self.keymap
                .chord_start(pc, ps, pa, &p.key)
                .and_then(|prefix| self.keymap.lookup_chord(&prefix, ctrl, shift, alt, &k.key).map(str::to_string))
                .filter(|id| is_macro(id))
                .map(|_| mark - 1)
        } else {
            None
        };
        if let (Some(cut), Some(rec)) = (cut, self.macros.recording.as_mut()) {
            rec.truncate(cut);
        }
    }

    /// `Ctrl+Shift+R`: start recording, or stop and keep what was recorded as the last macro.
    pub fn macro_toggle_record(&mut self) {
        if self.macros.replaying {
            return;
        }
        if self.macros.recording.is_none() {
            self.macros.recording = Some(Vec::new());
            self.macros.mark = 0;
            let stop = self.shortcut_label("MacroRecord");
            return self.say(format!("recording a macro ({stop} stops)"));
        }
        self.macro_trim();
        let keys = self.macros.recording.take().unwrap_or_default();
        if keys.is_empty() {
            return self.say("macro empty: nothing recorded");
        }
        let n = keys.len();
        self.macros.last = Some(keys);
        let play = self.shortcut_label("MacroReplay");
        self.say(format!("macro recorded: {n} keys ({play} replays)"));
    }

    /// The main shortcut of a command for messages ("Ctrl+Alt+R"), or its palette name.
    fn shortcut_label(&self, id: &str) -> String {
        let user = self.keymap.entries();
        let combo = user
            .iter()
            .filter(|(_, v)| v.as_str() == id)
            .map(|(k, _)| k.clone())
            .min()
            .or_else(|| crate::keymap::default_combo(id).filter(|c| !user.contains_key(*c)).map(str::to_string));
        match combo {
            Some(c) => crate::app_tooltip::pretty_combo(&c),
            None => "Macro: Replay last".to_string(),
        }
    }

    /// Replay the last macro `times` times.
    pub fn macro_replay(&mut self, times: usize) {
        if self.macros.recording.is_some() {
            self.macro_trim();
            let stop = self.shortcut_label("MacroRecord");
            return self.say(format!("stop recording first ({stop})"));
        }
        let Some(keys) = self.macros.last.clone() else {
            let rec = self.shortcut_label("MacroRecord");
            return self.say(format!("no macro yet: {rec} records one"));
        };
        self.play_keys(&keys, times);
    }

    /// Send `keys` through the normal key handling `times` times; every document it edits gets
    /// the whole replay as one undo step.
    pub fn play_keys(&mut self, keys: &[MacroKey], times: usize) {
        if self.macros.replaying || keys.is_empty() {
            return;
        }
        self.macros.replaying = true;
        let marks: Vec<(u64, Option<usize>)> = self
            .docs
            .iter_mut()
            .map(|d| {
                d.buf.close_group();
                (d.buf.gen_id(), d.buf.undo_mark())
            })
            .collect();
        let held = self.mods;
        for _ in 0..times {
            for k in keys {
                self.mods = k.mods;
                // A quit request from inside a macro is ignored.
                let _ = self.handle_key(&KeyIn { key: k.key.clone(), text: k.text.clone() });
            }
        }
        self.mods = held;
        for d in &mut self.docs {
            if let Some((_, m)) = marks.iter().find(|(g, _)| *g == d.buf.gen_id()) {
                d.buf.squash_since(*m);
            }
        }
        self.macros.replaying = false;
        self.request_redraw();
    }

    /// "Macro: Replay…": ask how many times.
    pub fn macro_ask_times(&mut self) {
        if self.macros.last.is_none() {
            return self.macro_replay(1);
        }
        self.overlay = crate::app_overlay::Overlay::Prompt(crate::app_overlay::Prompt::new(
            crate::app_overlay::PromptKind::MacroCount,
            "Replay the last macro how many times?".into(),
            crate::field::TextField::with("1"),
        ));
        if let crate::app_overlay::Overlay::Prompt(p) = &mut self.overlay {
            p.field.select_all();
        }
    }

    pub fn macro_replay_answer(&mut self, text: &str) {
        match text.trim().parse::<usize>() {
            Ok(n) if (1..=MAX_TIMES).contains(&n) => self.macro_replay(n),
            _ => self.say(format!("not a count from 1 to {MAX_TIMES}: {}", text.trim())),
        }
    }

    /// "Macro: Save last as…": ask for a name.
    pub fn macro_ask_name(&mut self) {
        if self.macros.last.is_none() {
            return self.macro_replay(1);
        }
        self.overlay = crate::app_overlay::Overlay::Prompt(crate::app_overlay::Prompt::new(
            crate::app_overlay::PromptKind::MacroName,
            "Save the last macro as (name in macros.json)".into(),
            crate::field::TextField::default(),
        ));
    }

    pub fn macro_save_answer(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return self.say("macro not saved: empty name");
        }
        let Some(keys) = self.macros.last.as_ref() else { return };
        match save_named(name, to_tokens(keys)) {
            Ok(_) => self.say(format!("macro saved as \"{name}\" (bind it with \"{BIND_PREFIX}{name}\")")),
            Err(e) => self.say(format!("could not save the macro: {e}")),
        }
    }

    /// "Macro: Run saved…": pick one of `macros.json`.
    pub fn macro_pick_saved(&mut self) {
        let saved = load_saved();
        if saved.is_empty() {
            return self.say("no saved macros (Macro: Save last as…)");
        }
        let items = saved
            .iter()
            .map(|(name, toks)| crate::app_overlay::Item {
                label: name.clone(),
                detail: toks.join(" ; ").chars().take(80).collect(),
                action: crate::app_overlay::Action::Macro(name.clone()),
            })
            .collect();
        self.open_locations("Run saved macro", items);
    }

    /// Run the macro saved as `name` (palette pick or a `macro:name` key binding).
    pub fn macro_run_saved(&mut self, name: &str) {
        if self.macros.recording.is_some() {
            self.macro_trim();
            return self.say("stop recording first");
        }
        match load_saved().get(name) {
            Some(toks) => {
                let keys = from_tokens(toks);
                self.play_keys(&keys, 1);
            }
            None => self.say(format!("no saved macro named \"{name}\"")),
        }
    }

    /// Status bar: the recording pill. Returns the x after it.
    pub fn draw_macro_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        let Some(rec) = self.macros.recording.as_ref() else { return x };
        let n = rec.len();
        let c = self.theme.chrome;
        let r = self.layout.status;
        let y = r.y + (r.h - 18) / 2;
        let label = format!("REC {n}");
        let lw = self.tx.ui_width(&label, true);
        let pill = Rect::new(x + 2, r.y + 4, lw + 30, r.h - 8);
        cv.fill_round(pill, (pill.h / 2).min(10), c.error.with_alpha(0x38));
        let dot = 8;
        cv.fill_round(Rect::new(pill.x + 9, pill.y + (pill.h - dot) / 2, dot, dot), dot / 2, c.error);
        self.tx.draw_ui(cv, pill.x + 22, y, &label, c.error, pill, true);
        let hint = format!("{} stops", self.shortcut_label("MacroRecord"));
        let hw = self.tx.ui_width(&hint, false);
        let dim = c.tab_inactive_fg;
        self.tx.draw_ui(cv, pill.right() + 8, y, &hint, dim, Rect::new(pill.right(), r.y, hw + 16, r.h), false);
        pill.right() + 8 + hw + 14
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app_unsaved::testkit::{lock, test_app, tmp},
        script::send_keys,
    };
    use winit::keyboard::SmolStr;

    fn mk(mods: ModifiersState, key: Key, text: Option<&str>) -> MacroKey {
        MacroKey { mods, key, text: text.map(str::to_string) }
    }

    #[test]
    fn tokens_round_trip() {
        let ch = |c: &str| mk(ModifiersState::empty(), Key::Character(SmolStr::new(c)), Some(c));
        let keys = vec![
            ch("a"),
            mk(ModifiersState::empty(), Key::Named(NamedKey::Space), Some(" ")),
            mk(ModifiersState::SHIFT, Key::Character("B".into()), Some("B")),
            ch(";"),
            mk(ModifiersState::empty(), Key::Named(NamedKey::Enter), Some("\r")),
            mk(ModifiersState::SHIFT, Key::Named(NamedKey::Home), None),
            mk(ModifiersState::CONTROL | ModifiersState::SHIFT, Key::Character("P".into()), None),
            mk(ModifiersState::CONTROL, Key::Character("+".into()), None),
            mk(ModifiersState::empty(), Key::Named(NamedKey::Tab), Some("\t")),
        ];
        let toks = to_tokens(&keys);
        assert_eq!(toks, ["type:a B;", "enter", "shift+home", "ctrl+shift+P", "ctrl++", "tab"]);
        let back = to_tokens(&from_tokens(&toks));
        assert_eq!(back, toks, "parsing the tokens gives the same keys back");
    }

    #[test]
    fn record_and_replay_through_the_key_script() {
        let _g = lock();
        let dir = tmp("macros");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("one\ntwo\nthree\nfour\n");
        let d = &mut app.docs[0];
        d.ed.click(&mut d.buf, 0, false);
        // Record: decorate the line and go down one line.
        send_keys(&mut app, "ctrl+shift+r;home;type:#;end;type:.;down");
        assert!(app.macros.recording.is_some());
        send_keys(&mut app, "ctrl+shift+r");
        assert!(app.macros.recording.is_none());
        assert_eq!(app.docs[0].buf.to_string(), "#one.\ntwo\nthree\nfour\n");
        assert_eq!(
            to_tokens(app.macros.last.as_ref().unwrap()),
            ["home", "type:#", "end", "type:.", "down"],
            "the stop shortcut is not part of the macro"
        );
        // Replay once with Ctrl+Alt+R: one undo step.
        send_keys(&mut app, "ctrl+alt+r");
        assert_eq!(app.docs[0].buf.to_string(), "#one.\n#two.\nthree\nfour\n");
        send_keys(&mut app, "ctrl+z");
        assert_eq!(app.docs[0].buf.to_string(), "#one.\ntwo\nthree\nfour\n");
        send_keys(&mut app, "ctrl+alt+r");
        // Twice more through "Macro: Replay…": still one undo step.
        app.run_cmd(crate::app_overlay::Cmd::MacroReplayTimes);
        assert!(matches!(&app.overlay, crate::app_overlay::Overlay::Prompt(_)));
        send_keys(&mut app, "type:2;enter");
        assert_eq!(app.docs[0].buf.to_string(), "#one.\n#two.\n#three.\n#four.\n");
        send_keys(&mut app, "ctrl+z");
        assert_eq!(app.docs[0].buf.to_string(), "#one.\n#two.\nthree\nfour\n");

        // Recording through the palette: the palette keys of the stop command are dropped too.
        send_keys(&mut app, "ctrl+shift+r;type:x;ctrl+shift+p;type:macro start stop;enter");
        assert!(app.macros.recording.is_none(), "stopped from the palette");
        assert_eq!(to_tokens(app.macros.last.as_ref().unwrap()), ["type:x"]);

        // Save it by name, bind it in keybindings.json, run it with the binding.
        app.macro_save_answer("ex");
        assert_eq!(load_saved().get("ex"), Some(&vec!["type:x".to_string()]));
        app.keymap = crate::keymap::Keymap::from_json(r#"{"ctrl+alt+1": "macro:ex"}"#);
        let before = app.docs[0].buf.to_string();
        send_keys(&mut app, "ctrl+alt+1");
        assert_eq!(app.docs[0].buf.to_string().len(), before.len() + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
