//! Keycaps: a shortcut such as `Ctrl+Shift+P` drawn as small rounded keys, the way the tooltips,
//! menus, footers, the welcome screen and the inline suggestion show shortcuts.

use crate::{
    anim::fade,
    ds::{self, R_CTRL},
    theme::Chrome,
    ui::{Canvas, Rect, TextCtx, ui_size},
};

/// Height of a keycap, in logical px.
pub const KEY_H: i32 = 18;
const CAP_PAD: i32 = 6;
const CAP_GAP: i32 = 3;

fn cap_size() -> f32 {
    (ui_size() * 0.85).round().max(10.0)
}

fn cap_w(tx: &mut TextCtx, key: &str, s: f32) -> i32 {
    (tx.ui_width_sized(key, false, s) + 2 * CAP_PAD).max(KEY_H)
}

/// The caps of a shortcut: "Ctrl+Shift+P" -> ["Ctrl", "Shift", "P"], "Ctrl+K Ctrl+S" -> four caps,
/// a trailing "+" is the key itself ("Ctrl++").
pub fn keycaps(combo: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chord in combo.split_whitespace() {
        let mut rest = chord;
        while !rest.is_empty() {
            if let Some(p) = rest.strip_prefix('+') {
                // A "+" that is a key: the last one of "Ctrl++".
                if p.is_empty() {
                    out.push("+".to_string());
                }
                rest = p;
                continue;
            }
            let end = rest.find('+').unwrap_or(rest.len());
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

/// Total width of the caps for `keys`.
pub fn keys_width(tx: &mut TextCtx, keys: &[String]) -> i32 {
    let s = cap_size();
    let w: i32 = keys.iter().map(|k| cap_w(tx, k, s)).sum();
    w + keys.len().saturating_sub(1) as i32 * CAP_GAP
}

/// Draw the caps with the top-left at (`x`, `y`), faded by `a` (0..1); returns the width used.
pub fn draw_keys(cv: &mut Canvas, tx: &mut TextCtx, c: &Chrome, x: i32, y: i32, keys: &[String], a: f32) -> i32 {
    let clip = cv.bounds();
    draw_keys_in(cv, tx, c, x, y, keys, a, clip)
}

/// [`draw_keys`] clipped to `clip`.
#[allow(clippy::too_many_arguments)]
pub fn draw_keys_in(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    c: &Chrome,
    x: i32,
    y: i32,
    keys: &[String],
    a: f32,
    clip: Rect,
) -> i32 {
    let s = cap_size();
    let lh = (s * 1.4).round() as i32;
    let mut cx = x;
    for k in keys {
        let w = cap_w(tx, k, s);
        let r = Rect::new(cx, y, w, KEY_H);
        cv.card(r, R_CTRL - ds::S1 / 2, fade(c.input_bg, a), fade(c.border, a));
        let tw = tx.ui_width_sized(k, false, s);
        tx.draw_ui_sized(cv, r.x + (w - tw) / 2, y + (KEY_H - lh) / 2, k, fade(c.tab_inactive_fg, a), clip, false, s);
        cx += w + CAP_GAP;
    }
    (cx - x - CAP_GAP).max(0)
}

/// One keycap ("Tab" next to an inline suggestion) at (`x`, `y`); returns its rect.
pub fn draw_keycap(cv: &mut Canvas, tx: &mut TextCtx, c: &Chrome, (x, y): (i32, i32), text: &str, clip: Rect) -> Rect {
    let keys = [text.to_string()];
    let w = draw_keys_in(cv, tx, c, x, y, &keys, 1.0, clip);
    Rect::new(x, y, w, KEY_H)
}

/// Width of a single keycap showing `text`.
pub fn keycap_w(tx: &mut TextCtx, text: &str) -> i32 {
    keys_width(tx, &[text.to_string()])
}

/// Where the keycap of an inline suggestion goes, given the caret cell's top-left `caret`, the
/// ghost text's first-line and last-row widths in px and the row count below the caret line.
/// `None` when it would not fit inside `body` (or text after the caret would be covered).
#[allow(clippy::too_many_arguments)]
pub fn ghost_keycap_pos(
    caret: (i32, i32),
    line_x0: i32,
    first_w: i32,
    rest_w: Option<i32>,
    line_h: i32,
    rest_rows: usize,
    key_w: i32,
    tail_empty: bool,
    body: Rect,
) -> Option<(i32, i32)> {
    let gap = ds::S2;
    let (x, y) = match rest_w {
        Some(w) => (line_x0 + w + gap, caret.1 + line_h * rest_rows as i32),
        None if tail_empty => (caret.0 + first_w + gap, caret.1),
        None => return None,
    };
    let y = y + (line_h - KEY_H) / 2;
    let r = Rect::new(x, y, key_w, KEY_H);
    (r.x >= body.x && r.right() <= body.right() && r.y >= body.y && r.bottom() <= body.bottom()).then_some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_split_into_caps() {
        assert_eq!(keycaps("Ctrl+Shift+P"), ["Ctrl", "Shift", "P"]);
        assert_eq!(keycaps("Ctrl++"), ["Ctrl", "+"]);
        assert_eq!(keycaps("Ctrl+K Ctrl+S"), ["Ctrl", "K", "Ctrl", "S"]);
        assert_eq!(keycaps("F5"), ["F5"]);
        assert!(keycaps("").is_empty());
    }

    #[test]
    fn keycap_follows_the_ghost_text() {
        let body = Rect::new(0, 0, 400, 200);
        // One line, nothing after the caret: right after the ghost text, centered on the row.
        let p = ghost_keycap_pos((100, 40), 20, 60, None, 20, 0, 30, true, body);
        assert_eq!(p, Some((100 + 60 + ds::S2, 40 + (20 - KEY_H) / 2)));
        // Text after the caret would be covered: no cap.
        assert_eq!(ghost_keycap_pos((100, 40), 20, 60, None, 20, 0, 30, false, body), None);
        // Several lines: after the last virtual row.
        let p = ghost_keycap_pos((100, 40), 20, 60, Some(50), 20, 2, 30, false, body);
        assert_eq!(p, Some((20 + 50 + ds::S2, 80 + (20 - KEY_H) / 2)));
        // Out of room on the right.
        assert_eq!(ghost_keycap_pos((380, 40), 20, 60, None, 20, 0, 30, true, body), None);
    }
}
