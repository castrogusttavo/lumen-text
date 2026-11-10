//! User key bindings: `~/.config/lumen-text/keybindings.json` maps a key combination to a
//! command id (the `Cmd` variant name shown in the palette source, e.g. "TestFile").
//! Example: {"ctrl+shift+t": "TestFile", "f5": "RepeatTask", "ctrl+w": "none"}
//! A value of "none" disables a built-in shortcut.
//! The built-in shortcuts live in [`DEFAULTS`] (one table, combo -> `Cmd` id); `lookup` asks the
//! user's file first and the defaults second.

use std::{collections::HashMap, path::Path};
use winit::keyboard::{Key, NamedKey};

/// Built-in shortcuts: normalized combo -> `Cmd` variant name. Single source of truth for the
/// key dispatch (`App::handle_key`) and for the "default" column of the shortcuts modal. Only
/// plain combos live here: chords (`Ctrl+K R`) and explorer-only keys stay in code. Tests check
/// it against `COMMANDS` (every id exists, no duplicate combos, the palette text agrees).
pub const DEFAULTS: &[(&str, &str)] = &[
    ("ctrl+s", "Save"),
    ("ctrl+w", "CloseTab"),
    ("ctrl+q", "Quit"),
    ("ctrl+p", "QuickOpen"),
    ("ctrl+shift+p", "Commands"),
    ("ctrl+b", "ToggleSidebar"),
    ("ctrl+shift+e", "FocusSidebar"),
    ("ctrl+tab", "NextTab"),
    ("ctrl+shift+tab", "PrevTab"),
    ("ctrl+f", "Find"),
    ("ctrl+h", "Replace"),
    ("ctrl+shift+f", "ProjectSearch"),
    ("ctrl+g", "GotoLine"),
    ("ctrl+shift+o", "GotoSymbol"),
    ("ctrl+shift+g", "GitPanel"),
    ("ctrl+t", "WorkspaceSymbols"),
    ("f12", "GotoDefinition"),
    ("shift+f12", "FindReferences"),
    ("f8", "NextDiagnostic"),
    ("shift+f8", "PrevDiagnostic"),
    ("f2", "Rename"),
    ("shift+alt+f", "Format"),
    ("ctrl+.", "CodeActions"),
    ("ctrl+/", "ToggleComment"),
    ("ctrl+z", "Undo"),
    ("ctrl+shift+z", "Redo"),
    ("ctrl+j", "ToggleOutput"),
    ("ctrl+shift+v", "MarkdownPreview"),
    ("ctrl+=", "ZoomIn"),
    ("ctrl+-", "ZoomOut"),
    ("ctrl+0", "ZoomReset"),
    ("alt+left", "NavBack"),
    ("alt+right", "NavForward"),
    ("ctrl+`", "OpenTerminal"),
    ("ctrl+shift+[", "Fold"),
    ("ctrl+shift+]", "Unfold"),
    ("ctrl+,", "OpenSettings"),
    ("ctrl+\\", "SplitRight"),
    ("ctrl+1", "FocusLeftPane"),
    ("ctrl+2", "FocusRightPane"),
    ("f5", "DebugStart"),
    ("shift+f5", "DebugStop"),
    ("f9", "ToggleBreakpoint"),
    ("f10", "StepOver"),
    ("f11", "StepInto"),
    ("shift+f11", "StepOut"),
    ("shift+alt+right", "ExpandSelection"),
    ("shift+alt+left", "ShrinkSelection"),
];

/// Built-in combo of a command id (first entry of [`DEFAULTS`]).
pub fn default_combo(id: &str) -> Option<&'static str> {
    DEFAULTS.iter().find(|(_, i)| *i == id).map(|(c, _)| *c)
}

/// Record `combo -> id` in a keybindings map and disable the command's other built-in combos
/// with "none", so the old default really stops working after a re-assignment.
pub fn bind_in(map: &mut HashMap<String, String>, combo: &str, id: &str) {
    if id != "none" {
        for (c, i) in DEFAULTS {
            if *i == id && *c != combo {
                map.entry(c.to_string()).or_insert_with(|| "none".into());
            }
        }
    }
    map.insert(combo.to_string(), id.to_string());
}

#[derive(Default)]
pub struct Keymap {
    map: HashMap<String, String>,
}

/// Canonical text for a combination: modifiers in fixed order, key lowercase.
pub fn combo(ctrl: bool, shift: bool, alt: bool, key: &str) -> String {
    let mut s = String::new();
    if ctrl {
        s.push_str("ctrl+");
    }
    if shift {
        s.push_str("shift+");
    }
    if alt {
        s.push_str("alt+");
    }
    s.push_str(&key.to_lowercase());
    s
}

/// Normalize a user-written combination ("Shift+Ctrl+T" -> "ctrl+shift+t").
pub fn normalize(text: &str) -> String {
    let (mut ctrl, mut shift, mut alt) = (false, false, false);
    let mut key = String::new();
    let parts: Vec<&str> = text.split('+').collect();
    for (i, p) in parts.iter().enumerate() {
        let p = match p.trim().to_lowercase().as_str() {
            "←" => "left".to_string(),
            "→" => "right".to_string(),
            "↑" => "up".to_string(),
            "↓" => "down".to_string(),
            other => other.to_string(),
        };
        match p.as_str() {
            "ctrl" | "control" if i + 1 < parts.len() => ctrl = true,
            "shift" if i + 1 < parts.len() => shift = true,
            "alt" | "option" if i + 1 < parts.len() => alt = true,
            "" if i + 1 == parts.len() => key.push('+'), // the '+' key itself ("ctrl++")
            _ => key = p,
        }
    }
    combo(ctrl, shift, alt, &key)
}

pub fn key_name(k: &Key) -> Option<String> {
    Some(match k {
        Key::Character(c) => c.to_lowercase(),
        Key::Named(n) => match n {
            NamedKey::ArrowUp => "up".into(),
            NamedKey::ArrowDown => "down".into(),
            NamedKey::ArrowLeft => "left".into(),
            NamedKey::ArrowRight => "right".into(),
            NamedKey::Enter => "enter".into(),
            NamedKey::Escape => "escape".into(),
            NamedKey::Tab => "tab".into(),
            NamedKey::Space => "space".into(),
            NamedKey::Backspace => "backspace".into(),
            NamedKey::Delete => "delete".into(),
            NamedKey::Home => "home".into(),
            NamedKey::End => "end".into(),
            NamedKey::PageUp => "pageup".into(),
            NamedKey::PageDown => "pagedown".into(),
            other => format!("{other:?}").to_lowercase(), // f1..f12
        },
        _ => return None,
    })
}

impl Keymap {
    pub fn from_json(text: &str) -> Keymap {
        let mut map = HashMap::new();
        if let Ok(serde_json::Value::Object(o)) = serde_json::from_str(text) {
            for (k, v) in o {
                if let Some(cmd) = v.as_str() {
                    map.insert(normalize(&k), cmd.to_string());
                }
            }
        }
        Keymap { map }
    }

    pub fn load_from(path: &Path) -> Keymap {
        std::fs::read_to_string(path).map(|t| Keymap::from_json(&t)).unwrap_or_default()
    }

    pub fn load() -> Keymap {
        crate::config::default_path().map(|p| Keymap::load_from(&p.with_file_name("keybindings.json"))).unwrap_or_default()
    }

    /// Command id bound to this combination: the user's file first (`"none"` means disabled),
    /// then [`DEFAULTS`]. Shifted symbol keys also try their base key (Ctrl+Shift+= types "+",
    /// Ctrl+Shift+[ types "{"), so the table only needs the unshifted spelling.
    pub fn lookup(&self, ctrl: bool, shift: bool, alt: bool, key: &Key) -> Option<&str> {
        let name = key_name(key)?;
        let mut cands = vec![combo(ctrl, shift, alt, &name)];
        match name.as_str() {
            "+" => cands.push(combo(ctrl, false, alt, "=")),
            "_" => cands.push(combo(ctrl, false, alt, "-")),
            "{" => cands.push(combo(ctrl, shift, alt, "[")),
            "}" => cands.push(combo(ctrl, shift, alt, "]")),
            _ => {}
        }
        if let Some(v) = cands.iter().find_map(|c| self.map.get(c)) {
            return Some(v.as_str());
        }
        cands.iter().find_map(|c| DEFAULTS.iter().find(|(k, _)| k == c).map(|(_, id)| *id))
    }

    /// Copy of the user's combo → command-id table.
    pub fn entries(&self) -> HashMap<String, String> {
        self.map.clone()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_parses() {
        assert_eq!(normalize("Shift+Ctrl+T"), "ctrl+shift+t");
        assert_eq!(normalize("F5"), "f5");
        assert_eq!(normalize("ctrl++"), "ctrl++");
        let k = Keymap::from_json(r#"{"Ctrl+Shift+T": "TestFile", "f5": "RepeatTask", "bad": 3}"#);
        assert_eq!(k.len(), 2);
        assert_eq!(k.lookup(true, true, false, &Key::Character("T".into())), Some("TestFile"));
        assert_eq!(k.lookup(false, false, false, &Key::Named(NamedKey::F5)), Some("RepeatTask"));
        // ctrl+t is not in the user's file, so the built-in default answers.
        assert_eq!(k.lookup(true, false, false, &Key::Character("t".into())), Some("WorkspaceSymbols"));
        assert_eq!(k.lookup(true, false, true, &Key::Character("t".into())), None);
        assert!(Keymap::from_json("not json").map.is_empty());
        assert_eq!(normalize("Alt+←"), "alt+left");
        assert_eq!(normalize("Shift+Alt+→"), "shift+alt+right");
    }

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn defaults_point_to_existing_commands() {
        use crate::app_overlay::{COMMANDS, Cmd};
        for (combo, id) in DEFAULTS {
            let cmd = Cmd::from_id(id).unwrap_or_else(|| panic!("default {combo} -> unknown command {id}"));
            assert_eq!(normalize(combo), *combo, "default {combo} is not normalized");
            // The palette text agrees with the table.
            let (_, text, _) = COMMANDS.iter().find(|(_, _, c)| *c == cmd).unwrap();
            assert_eq!(normalize(text), *combo, "{id}: palette says {text}");
        }
        // And the other way round: every plain combo shown in the palette is a default.
        for (label, text, cmd) in COMMANDS {
            if text.is_empty() || text.contains(' ') {
                continue; // no shortcut, a chord (Ctrl+K R) or explorer-only keys
            }
            let id = format!("{cmd:?}");
            assert_eq!(default_combo(&id), Some(normalize(text).as_str()), "{label}: {text} missing in keymap::DEFAULTS");
        }
    }

    #[test]
    fn defaults_have_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for (combo, id) in DEFAULTS {
            assert!(seen.insert(*combo), "duplicate combo {combo}");
            assert!(ids.insert(*id), "command {id} has two default combos");
        }
    }

    #[test]
    fn user_overrides_defaults_and_none_disables() {
        let k = Keymap::from_json(r#"{"ctrl+b": "TestFile", "ctrl+w": "none", "ctrl+shift+b": "ToggleSidebar"}"#);
        assert_eq!(k.lookup(true, false, false, &ch("b")), Some("TestFile"));
        assert_eq!(k.lookup(true, false, false, &ch("w")), Some("none"));
        assert_eq!(k.lookup(true, true, false, &ch("B")), Some("ToggleSidebar"));
        assert_eq!(k.lookup(true, false, false, &ch("s")), Some("Save")); // untouched default
        let empty = Keymap::default();
        assert_eq!(empty.lookup(true, false, false, &ch("b")), Some("ToggleSidebar"));
        assert_eq!(empty.lookup(false, false, false, &Key::Named(NamedKey::F12)), Some("GotoDefinition"));
        assert_eq!(empty.lookup(false, true, false, &Key::Named(NamedKey::F12)), Some("FindReferences"));
        assert_eq!(empty.lookup(true, true, false, &Key::Named(NamedKey::Tab)), Some("PrevTab"));
    }

    #[test]
    fn shifted_symbols_fall_back_to_base_key() {
        let k = Keymap::default();
        assert_eq!(k.lookup(true, true, false, &ch("+")), Some("ZoomIn"));
        assert_eq!(k.lookup(true, false, false, &ch("=")), Some("ZoomIn"));
        assert_eq!(k.lookup(true, true, false, &ch("_")), Some("ZoomOut"));
        assert_eq!(k.lookup(true, true, false, &ch("{")), Some("Fold"));
        assert_eq!(k.lookup(true, true, false, &ch("}")), Some("Unfold"));
    }

    #[test]
    fn rebinding_disables_the_old_default() {
        let mut m = HashMap::new();
        bind_in(&mut m, "ctrl+shift+b", "ToggleSidebar");
        assert_eq!(m.get("ctrl+b").map(String::as_str), Some("none"));
        assert_eq!(m.get("ctrl+shift+b").map(String::as_str), Some("ToggleSidebar"));
        let k = Keymap { map: m };
        assert_eq!(k.lookup(true, false, false, &ch("b")), Some("none"));
        assert_eq!(k.lookup(true, true, false, &ch("b")), Some("ToggleSidebar"));
        // Binding a command to its own default adds nothing else; "none" itself is just stored.
        let mut m = HashMap::new();
        bind_in(&mut m, "ctrl+b", "ToggleSidebar");
        bind_in(&mut m, "ctrl+w", "none");
        assert_eq!(m.len(), 2);
    }
}
