//! Kitty keyboard protocol ("progressive enhancement",
//! <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>): the per-screen stack of enhancement
//! flags a program pushes (`CSI > flags u`), pops (`CSI < n u`), queries (`CSI ? u`) and edits
//! (`CSI = flags ; mode u`), and the key encoder used while any flag is in effect.
//!
//! Implemented flags: 1 (disambiguate escape codes), 4 (report alternate keys: the shifted key),
//! 8 (report all keys as escape codes) and 16 (report associated text). Flag 2 (press / repeat /
//! release events) is not: the editor only sees key presses here, so it is masked out and a query
//! answers without it. Modifier keys pressed alone are not reported. With no flags pushed the
//! terminal keeps the legacy xterm encoding (`terminal::key_bytes_app`).

use std::fmt::Write;

/// Flag 1: Esc, Alt+key, Ctrl+key... become unambiguous `CSI u` sequences.
pub const DISAMBIGUATE: u8 = 1;
/// Flag 4: with Shift held, also report the shifted key (`CSI 97:65;2u`).
pub const ALTERNATES: u8 = 4;
/// Flag 8: every key is an escape code, plain letters and Enter / Tab / Backspace included.
pub const ALL_KEYS: u8 = 8;
/// Flag 16: with flag 8, the text a key types rides along (`CSI 97;;97u`).
pub const TEXT: u8 = 16;
/// Flags this terminal implements; others (2: repeat / release events) are dropped when pushed or set.
pub const SUPPORTED: u8 = DISAMBIGUATE | ALTERNATES | ALL_KEYS | TEXT;
/// Entries per stack; pushing onto a full stack drops the oldest (as the spec asks).
const MAX_DEPTH: usize = 16;

const SHIFT: u8 = 1;

/// Modifiers held with a key.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub sup: bool,
}

impl Mods {
    /// Protocol bits (the sequence carries 1 + these).
    fn bits(self) -> u8 {
        u8::from(self.shift) | (u8::from(self.alt) << 1) | (u8::from(self.ctrl) << 2) | (u8::from(self.sup) << 3)
    }
}

/// The enhancement-flag stacks: the main and the alternate screen keep their own (a full-screen
/// program's flags vanish with its screen).
#[derive(Clone, Default, Debug)]
pub struct KeyStack {
    main: Vec<u8>,
    alt: Vec<u8>,
}

impl KeyStack {
    fn stack_mut(&mut self, alt: bool) -> &mut Vec<u8> {
        if alt { &mut self.alt } else { &mut self.main }
    }

    /// Flags in effect on the main (`alt` false) or alternate screen.
    pub fn flags(&self, alt: bool) -> u8 {
        let s = if alt { &self.alt } else { &self.main };
        s.last().copied().unwrap_or(0)
    }

    /// `CSI > flags u`.
    pub fn push(&mut self, alt: bool, flags: u8) {
        let s = self.stack_mut(alt);
        if s.len() >= MAX_DEPTH {
            s.remove(0);
        }
        s.push(flags & SUPPORTED);
    }

    /// `CSI < n u`: pop `n` entries; popping everything means back to the legacy encoding.
    pub fn pop(&mut self, alt: bool, n: usize) {
        let s = self.stack_mut(alt);
        s.truncate(s.len().saturating_sub(n.max(1)));
    }

    /// `CSI = flags ; mode u`: 1 sets the flags, 2 adds them, 3 removes them (on the top entry).
    pub fn set(&mut self, alt: bool, flags: u8, mode: u16) {
        let s = self.stack_mut(alt);
        let (cur, f) = (s.last().copied().unwrap_or(0), flags & SUPPORTED);
        let new = match mode {
            2 => cur | f,
            3 => cur & !f,
            _ => f,
        };
        match s.last_mut() {
            Some(top) => *top = new,
            None => s.push(new),
        }
    }
}

/// Number and final byte of a functional key (`CSI number ; mods final`); `None` for keys the
/// protocol does not name.
fn functional(name: &str) -> Option<(u32, char)> {
    Some(match name {
        "Escape" => (27, 'u'),
        "Enter" => (13, 'u'),
        "Tab" => (9, 'u'),
        "Backspace" => (127, 'u'),
        "Insert" => (2, '~'),
        "Delete" => (3, '~'),
        "ArrowLeft" => (1, 'D'),
        "ArrowRight" => (1, 'C'),
        "ArrowUp" => (1, 'A'),
        "ArrowDown" => (1, 'B'),
        "PageUp" => (5, '~'),
        "PageDown" => (6, '~'),
        "Home" => (1, 'H'),
        "End" => (1, 'F'),
        "F1" => (1, 'P'),
        "F2" => (1, 'Q'),
        // Not `CSI 1;m R`: that reads as a cursor position report.
        "F3" => (13, '~'),
        "F4" => (1, 'S'),
        "F5" => (15, '~'),
        "F6" => (17, '~'),
        "F7" => (18, '~'),
        "F8" => (19, '~'),
        "F9" => (20, '~'),
        "F10" => (21, '~'),
        "F11" => (23, '~'),
        "F12" => (24, '~'),
        "PrintScreen" => (57361, 'u'),
        "Pause" => (57362, 'u'),
        "ContextMenu" => (57363, 'u'),
        f if f.starts_with('F') => {
            let k: u32 = f[1..].parse().ok()?;
            if !(13..=35).contains(&k) {
                return None;
            }
            (57376 + k - 13, 'u')
        }
        _ => return None,
    })
}

/// Shifted symbols of the US layout and their unshifted key: the protocol names a key by its
/// unshifted character, and winit only hands us the shifted one.
const US_SHIFTED: &[(char, char)] = &[
    ('!', '1'),
    ('@', '2'),
    ('#', '3'),
    ('$', '4'),
    ('%', '5'),
    ('^', '6'),
    ('&', '7'),
    ('*', '8'),
    ('(', '9'),
    (')', '0'),
    ('_', '-'),
    ('+', '='),
    ('{', '['),
    ('}', ']'),
    ('|', '\\'),
    (':', ';'),
    ('"', '\''),
    ('<', ','),
    ('>', '.'),
    ('?', '/'),
    ('~', '`'),
];

/// The key a character was typed with: letters in lower case, shifted US symbols unshifted.
fn base_key(c: char, shift: bool) -> char {
    if c.is_uppercase() {
        let mut l = c.to_lowercase();
        if let (Some(x), None) = (l.next(), l.next()) {
            return x;
        }
    }
    if shift && let Some((_, b)) = US_SHIFTED.iter().find(|(s, _)| *s == c) {
        return *b;
    }
    c
}

/// `CSI key[:shifted] [; mods] [; text] final`, leaving out what the spec lets go (`CSI A` for an
/// unmodified arrow, no `;1` without modifiers unless text follows).
fn csi(key: u32, shifted: Option<char>, bits: u8, text: Option<char>, end: char) -> Vec<u8> {
    let mut s = String::from("\x1b[");
    let more = bits != 0 || text.is_some();
    if key != 1 || shifted.is_some() || more {
        let _ = write!(s, "{key}");
    }
    if let Some(sh) = shifted {
        let _ = write!(s, ":{}", sh as u32);
    }
    if more {
        s.push(';');
        if bits != 0 {
            let _ = write!(s, "{}", bits as u32 + 1);
        }
    }
    if let Some(t) = text {
        let _ = write!(s, ";{}", t as u32);
    }
    s.push(end);
    s.into_bytes()
}

/// A typed character under `flags`.
fn encode_char(flags: u8, c: char, m: Mods) -> Vec<u8> {
    let bits = m.bits();
    let plain = bits & !SHIFT == 0;
    // Without flag 8, text typing (Shift included) stays text: only Ctrl / Alt / Super combos change.
    if flags & ALL_KEYS == 0 && plain {
        return c.to_string().into_bytes();
    }
    let base = base_key(c, m.shift);
    let shifted = (flags & ALTERNATES != 0 && m.shift && c != base).then_some(c);
    let text = (flags & ALL_KEYS != 0 && flags & TEXT != 0 && plain).then_some(c);
    csi(base as u32, shifted, bits, text, 'u')
}

/// Bytes for a key while kitty flags are in effect (`flags` != 0). `named` is a winit `NamedKey`
/// name ("Enter", "ArrowUp", "F5"...), `text` the typed text. `None`: the key sends nothing.
pub fn encode(flags: u8, named: Option<&str>, text: Option<&str>, m: Mods) -> Option<Vec<u8>> {
    let bits = m.bits();
    if let Some(n) = named {
        if n == "Space" {
            return Some(encode_char(flags, ' ', m));
        }
        let (key, end) = functional(n)?;
        // Enter / Tab / Backspace stay legacy unless every key is reported, so a shell is still
        // usable after a program that pushed flags crashed without popping them.
        if flags & ALL_KEYS == 0 && bits == 0 {
            match n {
                "Enter" => return Some(b"\r".to_vec()),
                "Tab" => return Some(b"\t".to_vec()),
                "Backspace" => return Some(vec![0x7f]),
                _ => {}
            }
        }
        return Some(csi(key, None, bits, None, end));
    }
    let t = text?;
    let mut chars = t.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_control() {
        // Composed text (an input method, a dead key) goes as typed.
        return Some(t.as_bytes().to_vec());
    }
    Some(encode_char(flags, c, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { shift: false, alt: false, ctrl: false, sup: false };
    const SHIFT_M: Mods = Mods { shift: true, ..NONE };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };

    fn s(v: Option<Vec<u8>>) -> String {
        String::from_utf8(v.expect("some bytes")).unwrap()
    }

    #[test]
    fn disambiguate_encodes_modified_keys_and_escape() {
        let f = DISAMBIGUATE;
        let key = |n: &str, m: Mods| s(encode(f, Some(n), None, m));
        let ch = |t: &str, m: Mods| s(encode(f, None, Some(t), m));
        assert_eq!(key("Escape", NONE), "\x1b[27u", "Esc is no longer a bare ESC");
        assert_eq!(key("Escape", SHIFT_M), "\x1b[27;2u");
        // Enter / Tab / Backspace keep their bytes unmodified, carry modifiers otherwise.
        assert_eq!(key("Enter", NONE), "\r");
        assert_eq!(key("Tab", NONE), "\t");
        assert_eq!(key("Backspace", NONE), "\x7f");
        assert_eq!(key("Enter", SHIFT_M), "\x1b[13;2u");
        assert_eq!(key("Enter", CTRL), "\x1b[13;5u");
        assert_eq!(key("Enter", ALT), "\x1b[13;3u");
        assert_eq!(key("Tab", SHIFT_M), "\x1b[9;2u");
        assert_eq!(key("Backspace", CTRL), "\x1b[127;5u");
        assert_eq!(key("Backspace", ALT), "\x1b[127;3u");
        // Text keys: plain and shifted typing stays text, Ctrl / Alt / Super become CSI u.
        assert_eq!(ch("a", NONE), "a");
        assert_eq!(ch("A", SHIFT_M), "A");
        assert_eq!(ch("é", NONE), "é");
        assert_eq!(ch("c", CTRL), "\x1b[99;5u", "Ctrl+C is a key, not SIGINT");
        assert_eq!(ch("A", Mods { shift: true, ctrl: true, ..NONE }), "\x1b[97;6u", "the key, not the shifted letter");
        assert_eq!(ch("x", ALT), "\x1b[120;3u");
        assert_eq!(ch("!", Mods { shift: true, alt: true, ..NONE }), "\x1b[49;4u", "Shift+Alt+1");
        assert_eq!(ch("[", CTRL), "\x1b[91;5u");
        assert_eq!(ch("a", Mods { sup: true, ..NONE }), "\x1b[97;9u");
        assert_eq!(key("Space", NONE), " ");
        assert_eq!(key("Space", CTRL), "\x1b[32;5u");
        // Composed text is sent as typed.
        assert_eq!(ch("ñü", NONE), "ñü");
    }

    #[test]
    fn functional_keys_use_the_short_forms() {
        let key = |n: &str, m: Mods| s(encode(DISAMBIGUATE, Some(n), None, m));
        assert_eq!(key("ArrowUp", NONE), "\x1b[A");
        assert_eq!(key("ArrowLeft", CTRL), "\x1b[1;5D");
        assert_eq!(key("Home", SHIFT_M), "\x1b[1;2H");
        assert_eq!(key("End", NONE), "\x1b[F");
        assert_eq!(key("Delete", NONE), "\x1b[3~");
        assert_eq!(key("PageDown", ALT), "\x1b[6;3~");
        assert_eq!(key("F1", NONE), "\x1b[P");
        assert_eq!(key("F3", NONE), "\x1b[13~", "F3 avoids CSI R");
        assert_eq!(key("F4", CTRL), "\x1b[1;5S");
        assert_eq!(key("F12", NONE), "\x1b[24~");
        assert_eq!(key("F13", NONE), "\x1b[57376u");
        assert_eq!(key("ContextMenu", NONE), "\x1b[57363u");
        assert_eq!(encode(DISAMBIGUATE, Some("Shift"), None, SHIFT_M), None, "modifiers alone send nothing");
        assert_eq!(encode(DISAMBIGUATE, None, None, NONE), None);
    }

    #[test]
    fn report_all_keys_alternates_and_text() {
        let all = DISAMBIGUATE | ALL_KEYS;
        assert_eq!(s(encode(all, None, Some("a"), NONE)), "\x1b[97u");
        assert_eq!(s(encode(all, None, Some("A"), SHIFT_M)), "\x1b[97;2u");
        assert_eq!(s(encode(all, Some("Enter"), None, NONE)), "\x1b[13u");
        assert_eq!(s(encode(all, Some("Tab"), None, NONE)), "\x1b[9u");
        assert_eq!(s(encode(all, Some("Backspace"), None, NONE)), "\x1b[127u");
        assert_eq!(s(encode(all, Some("Space"), None, NONE)), "\x1b[32u");
        assert_eq!(s(encode(all, Some("Escape"), None, NONE)), "\x1b[27u");
        // Flag 4: the shifted key after a colon (only with Shift, only when it differs).
        let alt = all | ALTERNATES;
        assert_eq!(s(encode(alt, None, Some("A"), SHIFT_M)), "\x1b[97:65;2u");
        assert_eq!(s(encode(alt, None, Some(":"), SHIFT_M)), "\x1b[59:58;2u");
        assert_eq!(s(encode(alt, None, Some("a"), NONE)), "\x1b[97u");
        assert_eq!(
            s(encode(DISAMBIGUATE | ALTERNATES, None, Some("A"), Mods { shift: true, ctrl: true, ..NONE })),
            "\x1b[97:65;6u"
        );
        // Flag 16: the typed text as a third field; nothing when Ctrl / Alt type no text.
        let text = all | TEXT;
        assert_eq!(s(encode(text, None, Some("a"), NONE)), "\x1b[97;;97u");
        assert_eq!(s(encode(text, None, Some("A"), SHIFT_M)), "\x1b[97;2;65u");
        assert_eq!(s(encode(text, None, Some("c"), CTRL)), "\x1b[99;5u");
        assert_eq!(s(encode(text, None, Some("é"), NONE)), "\x1b[233;;233u");
        // Caps Lock: the key is still the lower-case letter, the text the capital.
        assert_eq!(s(encode(text, None, Some("Q"), NONE)), "\x1b[113;;81u");
    }

    #[test]
    fn stack_push_pop_set_and_screens() {
        let mut k = KeyStack::default();
        assert_eq!(k.flags(false), 0);
        k.push(false, 1);
        k.push(false, 0b11111);
        assert_eq!(k.flags(false), SUPPORTED, "flag 2 (event types) is dropped");
        assert_eq!(k.flags(true), 0, "the alternate screen has its own stack");
        k.pop(false, 1);
        assert_eq!(k.flags(false), 1);
        k.set(false, 8, 2);
        assert_eq!(k.flags(false), 9, "mode 2 adds");
        k.set(false, 1, 3);
        assert_eq!(k.flags(false), 8, "mode 3 removes");
        k.set(false, 5, 1);
        assert_eq!(k.flags(false), 5, "mode 1 replaces");
        k.pop(false, 5);
        assert_eq!(k.flags(false), 0, "popping past the bottom empties the stack");
        k.set(true, 1, 1);
        assert_eq!((k.flags(true), k.flags(false)), (1, 0), "set on an empty stack creates the entry");
        k.push(false, 1);
        for _ in 0..40 {
            k.push(false, 8);
        }
        k.pop(false, MAX_DEPTH - 1);
        assert_eq!(k.flags(false), 8);
        k.pop(false, 1);
        assert_eq!(k.flags(false), 0, "the stack is bounded: the oldest entry (1) was dropped");
    }
}
