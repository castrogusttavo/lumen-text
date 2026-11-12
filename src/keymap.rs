//! User key bindings: `~/.config/lumen-text/keybindings.json` maps a key combination to a
//! command id (the `Cmd` variant name shown in the palette source, e.g. "TestFile").
//! Example: {"ctrl+shift+t": "TestFile", "f5": "RepeatTask", "ctrl+w": "none"}
//! A value of "none" disables a built-in shortcut.
//! The built-in shortcuts live in [`DEFAULTS`] (one table, combo -> `Cmd` id); `lookup` asks the
//! user's file first and the defaults second.
//!
//! Combo syntax: `ctrl+shift+p` (plain), `ctrl+k r` (a chord: two strokes separated by a space)
//! and `explorer:a` (only while the file explorer has the focus; no modifiers needed).

use std::{collections::HashMap, path::Path};
use winit::keyboard::{Key, NamedKey};

/// Built-in shortcuts: normalized combo -> `Cmd` variant name. Single source of truth for the
/// key dispatch (`App::handle_key`) and for the "default" column of the shortcuts modal.
/// A command's first row is its main default (shown in the palette and the modal); later rows
/// are aliases (`ctrl+y` for Redo, `ctrl+k s` for Save). Tests check it against `COMMANDS` (every
/// id exists, no duplicate combos, the palette text agrees with the main default).
pub const DEFAULTS: &[(&str, &str)] = &[
    ("ctrl+s", "Save"),
    ("ctrl+w", "CloseTab"),
    ("ctrl+shift+t", "ReopenClosedTab"),
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
    ("ctrl+shift+a", "AiPanel"),
    ("ctrl+alt+k", "AiSendSelection"),
    ("ctrl+alt+a", "AiNextWaiting"),
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
    ("ctrl+shift+`", "NewTerminal"),
    ("ctrl+shift+[", "Fold"),
    ("ctrl+shift+]", "Unfold"),
    ("ctrl+,", "OpenSettings"),
    ("ctrl+\\", "SplitRight"),
    ("ctrl+1", "FocusLeftPane"),
    ("ctrl+2", "FocusRightPane"),
    ("ctrl+3", "FocusPane3"),
    ("ctrl+4", "FocusPane4"),
    ("f5", "DebugStart"),
    ("shift+f5", "DebugStop"),
    ("ctrl+f5", "BuildRun"),
    ("ctrl+shift+b", "BuildBuild"),
    ("f9", "ToggleBreakpoint"),
    ("ctrl+f2", "ToggleBookmark"),
    ("ctrl+alt+f2", "NextBookmark"),
    ("ctrl+shift+alt+f2", "PrevBookmark"),
    ("f10", "StepOver"),
    ("f11", "StepInto"),
    ("shift+f11", "StepOut"),
    ("shift+alt+right", "ExpandSelection"),
    ("shift+alt+left", "ShrinkSelection"),
    ("ctrl+shift+alt+up", "ColumnSelectUp"),
    ("ctrl+shift+alt+down", "ColumnSelectDown"),
    ("ctrl+shift+alt+left", "ColumnSelectLeft"),
    ("ctrl+shift+alt+right", "ColumnSelectRight"),
    ("alt+j", "JumpTo"),
    ("ctrl+shift+h", "ProjectReplace"),
    ("ctrl+shift+r", "MacroRecord"),
    ("ctrl+alt+r", "MacroReplay"),
    // Chords: `Ctrl+K` then a letter.
    ("ctrl+k r", "RepeatTask"),
    ("ctrl+k x", "StopTask"),
    ("ctrl+k f", "TestFile"),
    ("ctrl+k t", "TestAtCursor"),
    ("ctrl+k a", "TestAll"),
    ("ctrl+k e", "TestFailed"),
    ("ctrl+k d", "TestDebug"),
    ("ctrl+k i", "ShowHover"),
    ("ctrl+k \\", "SplitDown"),
    ("ctrl+k right", "FocusOtherPane"),
    ("ctrl+k left", "FocusPrevPane"),
    ("ctrl+k shift+right", "MoveToNextPane"),
    ("ctrl+k shift+left", "MoveToPrevPane"),
    ("ctrl+k shift+w", "ClosePane"),
    // Aliases of commands that already have a main combo above.
    ("ctrl+y", "Redo"),
    ("ctrl+k s", "Save"),
    ("ctrl+k w", "CloseTab"),
    // Only while the file explorer has the focus.
    ("explorer:a", "NewFile"),
    ("explorer:shift+a", "NewDir"),
    ("explorer:f2", "RenamePath"),
    ("explorer:delete", "DeletePath"),
];

/// Commands whose `Ctrl+<letter>` default also fires with Shift held (`Ctrl+Shift+W` closes the
/// tab, `Ctrl+Shift+Q` quits, ...). An exact `Ctrl+Shift+<letter>` row or user binding still wins,
/// which is why `Ctrl+Shift+G` opens the Git panel while `Ctrl+G` goes to a line.
pub const SHIFT_TOLERANT: &[&str] =
    &["Save", "CloseTab", "Quit", "GotoLine", "ToggleOutput", "Replace", "ToggleSidebar", "WorkspaceSymbols"];

/// Prefix that scopes a combo to the file explorer.
pub const EXPLORER: &str = "explorer:";

/// Built-in combo of a command id (first entry of [`DEFAULTS`]).
pub fn default_combo(id: &str) -> Option<&'static str> {
    DEFAULTS.iter().find(|(_, i)| *i == id).map(|(c, _)| *c)
}

/// Every built-in combo of a command id, main one first.
#[cfg(test)]
pub fn default_combos(id: &str) -> Vec<&'static str> {
    DEFAULTS.iter().filter(|(_, i)| *i == id).map(|(c, _)| *c).collect()
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

/// What the status bar shows while a chord waits for its second stroke: "ctrl+k" -> "Ctrl+K …".
pub fn chord_label(prefix: &str) -> String {
    format!("{} …", crate::app_tooltip::pretty_combo(prefix))
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
    let text = text.trim();
    if let Some(rest) =
        text.get(..EXPLORER.len()).filter(|p| p.eq_ignore_ascii_case(EXPLORER)).map(|_| &text[EXPLORER.len()..])
    {
        return format!("{EXPLORER}{}", normalize(rest));
    }
    // A chord: each stroke is normalized on its own ("Ctrl+K  R" -> "ctrl+k r").
    if text.contains(char::is_whitespace) {
        return text.split_whitespace().map(normalize).collect::<Vec<_>>().join(" ");
    }
    normalize_stroke(text)
}

fn normalize_stroke(text: &str) -> String {
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

fn is_fkey(n: &str) -> bool {
    n.len() > 1 && n.starts_with('f') && n[1..].chars().all(|c| c.is_ascii_digit())
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

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn load() -> Keymap {
        crate::config::default_path()
            .map(|p| Keymap::load_from(&p.with_file_name("keybindings.json")))
            .unwrap_or_default()
    }

    /// First hit for `cands` (in order): the user's file, then the built-in table.
    fn resolve(&self, cands: &[String]) -> Option<&str> {
        if let Some(v) = cands.iter().find_map(|c| self.map.get(c)) {
            return Some(v.as_str());
        }
        cands.iter().find_map(|c| DEFAULTS.iter().find(|(k, _)| k == c).map(|(_, id)| *id))
    }

    /// Command id bound to this combination: the user's file first (`"none"` means disabled),
    /// then [`DEFAULTS`]. Forgiving matches, tried after the exact combo:
    /// - shifted symbol keys also try their base key (Ctrl+Shift+= types "+", Ctrl+Shift+[ types
    ///   "{"), so the table only needs the unshifted spelling;
    /// - F-keys with extra Ctrl/Alt fire the plain F-key command (`Ctrl+F5` = `F5`);
    /// - `Ctrl+Shift+<letter>` fires the `Ctrl+<letter>` command when it is in [`SHIFT_TOLERANT`].
    pub fn lookup(&self, ctrl: bool, shift: bool, alt: bool, key: &Key) -> Option<&str> {
        let name = key_name(key)?;
        let mut cands = vec![combo(ctrl, shift, alt, &name)];
        match name.as_str() {
            "+" => cands.push(combo(ctrl, false, alt, "=")),
            "_" => cands.push(combo(ctrl, false, alt, "-")),
            "{" => cands.push(combo(ctrl, shift, alt, "[")),
            "}" => cands.push(combo(ctrl, shift, alt, "]")),
            // Shift+` types `~` on most layouts.
            "~" => cands.push(combo(ctrl, shift, alt, "`")),
            n if is_fkey(n) && (ctrl || alt) => {
                cands.push(combo(false, shift, false, n));
                if shift {
                    cands.push(combo(false, false, false, n));
                }
            }
            _ => {}
        }
        if let Some(id) = self.resolve(&cands) {
            return Some(id);
        }
        if ctrl && shift && !alt && name.chars().count() == 1 {
            let base = combo(true, false, false, &name);
            let (_, id) = DEFAULTS.iter().find(|(k, _)| *k == base)?;
            if SHIFT_TOLERANT.contains(id) {
                return Some(self.map.get(&base).map_or(*id, String::as_str));
            }
        }
        None
    }

    /// Does `prefix` (a normalized first stroke such as "ctrl+k") start any chord?
    pub fn is_chord_prefix(&self, prefix: &str) -> bool {
        let p = format!("{prefix} ");
        self.map.keys().any(|k| k.starts_with(&p)) || DEFAULTS.iter().any(|(k, _)| k.starts_with(&p))
    }

    /// The normalized first stroke of a chord for this key press, when it starts one.
    pub fn chord_start(&self, ctrl: bool, shift: bool, alt: bool, key: &Key) -> Option<String> {
        let c = combo(ctrl, shift, alt, &key_name(key)?);
        self.is_chord_prefix(&c).then_some(c)
    }

    /// Command bound to the second stroke of a chord that began with `prefix`. The second stroke
    /// may be typed with or without the modifiers still held (`Ctrl+K R` and `Ctrl+K Ctrl+R`).
    pub fn lookup_chord(&self, prefix: &str, ctrl: bool, shift: bool, alt: bool, key: &Key) -> Option<&str> {
        let name = key_name(key)?;
        let mut cands = vec![format!("{prefix} {}", combo(ctrl, shift, alt, &name))];
        cands.push(format!("{prefix} {name}"));
        self.resolve(&cands)
    }

    /// Command bound to a key while the file explorer has the focus (`explorer:` rows).
    pub fn lookup_explorer(&self, ctrl: bool, shift: bool, alt: bool, key: &Key) -> Option<&str> {
        let name = key_name(key)?;
        self.resolve(&[format!("{EXPLORER}{}", combo(ctrl, shift, alt, &name))])
    }

    /// Copy of the user's combo → command-id table.
    pub fn entries(&self) -> HashMap<String, String> {
        self.map.clone()
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

    /// Combo a palette hint stands for: "Ctrl+K R" is a chord, "A (explorer)" an explorer key.
    fn palette_combo(text: &str) -> String {
        match text.strip_suffix(" (explorer)") {
            Some(t) => format!("{EXPLORER}{}", normalize(t)),
            None => normalize(text),
        }
    }

    #[test]
    fn defaults_point_to_existing_commands() {
        use crate::app_overlay::{COMMANDS, Cmd};
        for (combo, id) in DEFAULTS {
            Cmd::from_id(id).unwrap_or_else(|| panic!("default {combo} -> unknown command {id}"));
            assert_eq!(normalize(combo), *combo, "default {combo} is not normalized");
        }
        // The palette text agrees with the command's main (first) default, and every shortcut the
        // palette shows is in the table (plain combos, chords and explorer keys alike).
        for (label, text, cmd) in COMMANDS {
            let id = format!("{cmd:?}");
            if text.is_empty() {
                continue;
            }
            assert_eq!(
                default_combo(&id),
                Some(palette_combo(text).as_str()),
                "{label}: {text} disagrees with keymap::DEFAULTS"
            );
        }
        // Every default of a command that has a palette hint is its main one or an alias of it.
        for (combo, id) in DEFAULTS {
            let cmd = Cmd::from_id(id).unwrap();
            assert!(COMMANDS.iter().any(|(_, _, c)| *c == cmd), "{combo}: {id} is not in the palette");
        }
    }

    #[test]
    fn defaults_have_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (combo, _) in DEFAULTS {
            assert!(seen.insert(*combo), "duplicate combo {combo}");
        }
        // A chord prefix must not also be a plain command.
        for (combo, id) in DEFAULTS {
            if let Some((first, _)) = combo.split_once(' ') {
                assert!(
                    DEFAULTS.iter().all(|(c, _)| *c != first),
                    "{combo} ({id}): its prefix {first} is a command too"
                );
            }
        }
        for id in SHIFT_TOLERANT {
            assert!(default_combo(id).is_some(), "{id} in SHIFT_TOLERANT has no default");
        }
    }

    #[test]
    fn normalizes_chords_and_explorer_keys() {
        assert_eq!(normalize("Ctrl+K  R"), "ctrl+k r");
        assert_eq!(normalize("Ctrl+K Ctrl+Shift+T"), "ctrl+k ctrl+shift+t");
        assert_eq!(normalize("Explorer:Shift+A"), "explorer:shift+a");
        assert_eq!(normalize("explorer:Delete"), "explorer:delete");
    }

    #[test]
    fn chords_resolve_through_the_table() {
        let k = Keymap::default();
        assert_eq!(k.chord_start(true, false, false, &ch("k")).as_deref(), Some("ctrl+k"));
        assert_eq!(k.chord_start(true, false, false, &ch("j")), None);
        assert_eq!(k.lookup(true, false, false, &ch("k")), None, "the prefix alone is not a command");
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("r")), Some("RepeatTask"));
        assert_eq!(k.lookup_chord("ctrl+k", true, false, false, &ch("r")), Some("RepeatTask"), "Ctrl still held");
        assert_eq!(
            k.lookup_chord("ctrl+k", false, true, false, &ch("R")),
            Some("RepeatTask"),
            "Shift is ignored by the fallback"
        );
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("s")), Some("Save"));
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("w")), Some("CloseTab"));
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("i")), Some("ShowHover"));
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("z")), None);
        // The user can rebind, disable and add chords.
        let k = Keymap::from_json(r#"{"Ctrl+K R": "none", "ctrl+k g": "GotoLine", "ctrl+q q": "Quit"}"#);
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("r")), Some("none"));
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("g")), Some("GotoLine"));
        assert_eq!(k.lookup_chord("ctrl+k", false, false, false, &ch("x")), Some("StopTask"));
        assert!(k.is_chord_prefix("ctrl+q"));
    }

    #[test]
    fn explorer_keys_are_scoped() {
        let k = Keymap::default();
        assert_eq!(k.lookup_explorer(false, false, false, &ch("a")), Some("NewFile"));
        assert_eq!(k.lookup_explorer(false, true, false, &ch("A")), Some("NewDir"));
        assert_eq!(k.lookup_explorer(false, false, false, &Key::Named(NamedKey::F2)), Some("RenamePath"));
        assert_eq!(k.lookup_explorer(false, false, false, &Key::Named(NamedKey::Delete)), Some("DeletePath"));
        assert_eq!(k.lookup_explorer(true, false, false, &ch("a")), None);
        // Never leak into the global table: a plain "a" in the editor is just typing.
        assert_eq!(k.lookup(false, false, false, &ch("a")), None);
        assert_eq!(k.lookup(false, false, false, &Key::Named(NamedKey::F2)), Some("Rename"));
        let k = Keymap::from_json(r#"{"explorer:a": "none", "explorer:n": "NewFile"}"#);
        assert_eq!(k.lookup_explorer(false, false, false, &ch("a")), Some("none"));
        assert_eq!(k.lookup_explorer(false, false, false, &ch("n")), Some("NewFile"));
    }

    #[test]
    fn ctrl_y_redoes_and_redo_aliases_are_rebound_together() {
        let k = Keymap::default();
        assert_eq!(k.lookup(true, false, false, &ch("y")), Some("Redo"));
        assert_eq!(default_combos("Redo"), vec!["ctrl+shift+z", "ctrl+y"]);
        let mut m = HashMap::new();
        bind_in(&mut m, "ctrl+alt+z", "Redo");
        assert_eq!(m.get("ctrl+y").map(String::as_str), Some("none"), "the alias goes away with the main default");
        assert_eq!(m.get("ctrl+shift+z").map(String::as_str), Some("none"));
    }

    #[test]
    fn shift_variants_of_ctrl_letters_fire() {
        let k = Keymap::default();
        assert_eq!(k.lookup(true, true, false, &ch("W")), Some("CloseTab"));
        assert_eq!(k.lookup(true, true, false, &ch("Q")), Some("Quit"));
        assert_eq!(k.lookup(true, true, false, &ch("J")), Some("ToggleOutput"));
        assert_eq!(k.lookup(true, true, false, &ch("S")), Some("Save"));
        // An exact Ctrl+Shift row wins over the tolerant fallback.
        assert_eq!(k.lookup(true, true, false, &ch("G")), Some("GitPanel"));
        assert_eq!(k.lookup(true, true, false, &ch("B")), Some("BuildBuild"));
        assert_eq!(k.lookup(true, true, false, &ch("H")), Some("ProjectReplace"));
        assert_eq!(k.lookup(true, false, false, &ch("h")), Some("Replace"));
        assert_eq!(k.lookup(true, false, false, &ch("b")), Some("ToggleSidebar"));
        assert_eq!(k.lookup(true, false, false, &ch("g")), Some("GotoLine"));
        assert_eq!(k.lookup(true, true, false, &ch("Z")), Some("Redo"));
        // Commands outside the list keep their exact combo.
        assert_eq!(k.lookup(true, true, false, &ch("Y")), None);
        assert_eq!(k.lookup(true, true, false, &ch("D")), None);
        assert_eq!(k.lookup(true, true, true, &ch("W")), None, "Alt as well: no guessing");
        // A user binding on the plain combo ("none") also covers the Shift variant.
        let k = Keymap::from_json(r#"{"ctrl+w": "none"}"#);
        assert_eq!(k.lookup(true, true, false, &ch("W")), Some("none"));
        // ...and an exact user binding on the Shift variant wins.
        let k = Keymap::from_json(r#"{"ctrl+shift+w": "ToggleSidebar"}"#);
        assert_eq!(k.lookup(true, true, false, &ch("W")), Some("ToggleSidebar"));
    }

    #[test]
    fn fkeys_with_extra_modifiers_fire() {
        let k = Keymap::default();
        let f5 = Key::Named(NamedKey::F5);
        assert_eq!(k.lookup(false, false, false, &f5), Some("DebugStart"));
        assert_eq!(
            k.lookup(true, false, false, &f5),
            Some("BuildRun"),
            "Ctrl+F5 has its own row: run without debugging"
        );
        assert_eq!(k.lookup(false, false, true, &f5), Some("DebugStart"), "Alt+F5");
        assert_eq!(k.lookup(true, false, false, &Key::Named(NamedKey::F9)), Some("ToggleBreakpoint"), "Ctrl+F9");
        assert_eq!(k.lookup(false, true, false, &f5), Some("DebugStop"), "Shift+F5 keeps its own row");
        assert_eq!(k.lookup(true, true, false, &f5), Some("DebugStop"), "Ctrl+Shift+F5 -> Shift+F5");
        assert_eq!(k.lookup(true, false, false, &Key::Named(NamedKey::F12)), Some("GotoDefinition"));
        assert_eq!(k.lookup(true, false, false, &Key::Named(NamedKey::F7)), None);
        // A user row for the exact combo is preferred.
        let k = Keymap::from_json(r#"{"ctrl+f5": "RepeatTask"}"#);
        assert_eq!(k.lookup(true, false, false, &f5), Some("RepeatTask"));
        assert_eq!(k.lookup(false, false, false, &f5), Some("DebugStart"));
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

    #[test]
    fn pending_chord_label() {
        assert_eq!(chord_label("ctrl+k"), "Ctrl+K …");
        assert_eq!(chord_label("ctrl+shift+k"), "Ctrl+Shift+K …");
    }
}
