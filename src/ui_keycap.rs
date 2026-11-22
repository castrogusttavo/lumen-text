//! Keycap chips: a shortcut such as `Ctrl+Shift+P` drawn as small rounded keys, the way the
//! tooltips and the welcome screen show shortcuts. Colors come from the theme chrome.

use crate::{
    anim::fade,
    ds::{R_CTRL, S1},
    theme::Chrome,
    ui::{Canvas, Rect, TextCtx, ui_size},
};

const CAP_H: i32 = 18;
const CAP_PAD: i32 = 6;
const CAP_GAP: i32 = 3;

fn cap_size() -> f32 {
    (ui_size() * 0.85).round().max(10.0)
}

/// `Ctrl+Shift+G` -> ["Ctrl", "Shift", "G"]; a trailing `+` key (`Ctrl++`) stays a key.
pub fn split_keys(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch == '+' && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Total width of the chips for `keys`.
pub fn keys_width(tx: &mut TextCtx, keys: &[String]) -> i32 {
    let s = cap_size();
    let w: i32 = keys.iter().map(|k| tx.ui_width_sized(k, false, s) + 2 * CAP_PAD).sum();
    w + (keys.len() as i32 - 1).max(0) * CAP_GAP
}

/// Draw the chips with the top-left at (`x`, `y`) faded by `a` (0..1); returns the width used.
pub fn draw_keys(tx: &mut TextCtx, cv: &mut Canvas, x: i32, y: i32, keys: &[String], c: Chrome, a: f32) -> i32 {
    let s = cap_size();
    let lh = (s * 1.4).round() as i32;
    let clip = cv.bounds();
    let mut cx = x;
    for k in keys {
        let w = tx.ui_width_sized(k, false, s) + 2 * CAP_PAD;
        let r = Rect::new(cx, y, w, CAP_H);
        cv.fill_round(r, R_CTRL - S1 / 2, fade(c.widget_bg, a));
        cv.ring(r, R_CTRL - S1 / 2, fade(c.border, a));
        tx.draw_ui_sized(cv, cx + CAP_PAD, y + (CAP_H - lh) / 2, k, fade(c.tab_inactive_fg, a), clip, false, s);
        cx += w + CAP_GAP;
    }
    cx - x - if keys.is_empty() { 0 } else { CAP_GAP }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_chords_and_plus_key() {
        assert_eq!(split_keys("Ctrl+Shift+G"), ["Ctrl", "Shift", "G"]);
        assert_eq!(split_keys("Ctrl++"), ["Ctrl", "+"]);
        assert_eq!(split_keys("F5"), ["F5"]);
        assert!(split_keys("").is_empty());
    }
}
