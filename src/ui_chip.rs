//! Small chips for the editor's inline UI: tinted pills (the kind of a hovered symbol, counters)
//! and keycaps ("Tab" next to an inline suggestion). Colors come from the theme, geometry from
//! `ds`. Kept in its own module so it merges next to `ui_popup` / `ui_viewer` without collisions;
//! when those settle, the three can share one chip implementation.

use crate::ds;
use crate::hover_md::Kind;
use crate::theme::{Chrome, Rgba};
use crate::ui::{Canvas, Rect, TextCtx};

/// Chip text size (the design system asks for 10-11 px).
pub const CHIP_FONT: f32 = 11.0;
/// Chip height and horizontal padding.
pub const CHIP_H: i32 = 18;
const CHIP_PAD: i32 = ds::S2;
/// Line box of the chip font, to center the text vertically.
const CHIP_LINE: i32 = 15;

/// Color family of a tinted chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Info,
    Warn,
    Good,
}

/// Text color of a tone.
pub fn tone_color(c: &Chrome, t: Tone) -> Rgba {
    match t {
        Tone::Accent => c.accent_text,
        Tone::Info => c.info,
        Tone::Warn => c.warning,
        Tone::Good => c.git_added,
    }
}

/// Width of a chip showing `text`.
pub fn chip_w(tx: &mut TextCtx, text: &str) -> i32 {
    tx.ui_width_sized(text, false, CHIP_FONT) + 2 * CHIP_PAD
}

/// Tinted pill at (`x`, `y`); returns its rect. The caller makes sure it lies inside `clip`.
pub fn draw_chip(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    c: &Chrome,
    (x, y): (i32, i32),
    text: &str,
    tone: Tone,
    clip: Rect,
) -> Rect {
    let r = Rect::new(x, y, chip_w(tx, text), CHIP_H);
    let col = tone_color(c, tone);
    cv.fill_round(r, ds::R_CTRL, col.with_alpha(0x24));
    cv.ring(r, ds::R_CTRL, col.with_alpha(0x40));
    tx.draw_ui_sized_i(cv, r.x + CHIP_PAD, r.y + (CHIP_H - CHIP_LINE) / 2, text, col, clip, false, false, CHIP_FONT);
    r
}

/// Keycap (a key name in a bordered chip) at (`x`, `y`); returns its rect.
pub fn draw_keycap(cv: &mut Canvas, tx: &mut TextCtx, c: &Chrome, (x, y): (i32, i32), text: &str, clip: Rect) -> Rect {
    let r = Rect::new(x, y, chip_w(tx, text), CHIP_H);
    cv.card(r, ds::R_CTRL, c.input_bg, c.border);
    tx.draw_ui_sized_i(
        cv,
        r.x + CHIP_PAD,
        r.y + (CHIP_H - CHIP_LINE) / 2,
        text,
        c.tab_inactive_fg,
        clip,
        false,
        false,
        CHIP_FONT,
    );
    r
}

/// Word and tone of the kind chip in a hover header; `None` for kinds with nothing to say.
pub fn kind_chip(kind: Kind) -> Option<(&'static str, Tone)> {
    Some(match kind {
        Kind::Function => ("function", Tone::Accent),
        Kind::Method => ("method", Tone::Accent),
        Kind::Constructor => ("constructor", Tone::Accent),
        Kind::Class => ("class", Tone::Warn),
        Kind::Interface => ("interface", Tone::Warn),
        Kind::Enum => ("enum", Tone::Warn),
        Kind::TypeAlias => ("type", Tone::Warn),
        Kind::TypeParam => ("type parameter", Tone::Warn),
        Kind::Variable => ("variable", Tone::Info),
        Kind::Property => ("property", Tone::Info),
        Kind::Param => ("parameter", Tone::Info),
        Kind::EnumMember => ("enum member", Tone::Info),
        Kind::Accessor => ("accessor", Tone::Info),
        Kind::Namespace => ("namespace", Tone::Good),
        Kind::Module => ("module", Tone::Good),
        Kind::Other => return None,
    })
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
    let y = y + (line_h - CHIP_H) / 2;
    let r = Rect::new(x, y, key_w, CHIP_H);
    (r.x >= body.x && r.right() <= body.right() && r.y >= body.y && r.bottom() <= body.bottom()).then_some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_but_other_has_a_chip() {
        assert!(kind_chip(Kind::Function).is_some());
        assert!(kind_chip(Kind::Module).is_some());
        assert_eq!(kind_chip(Kind::Other), None);
        assert_eq!(kind_chip(Kind::Class).map(|k| k.0), Some("class"));
    }

    #[test]
    fn keycap_follows_the_ghost_text() {
        let body = Rect::new(0, 0, 400, 200);
        // One line, nothing after the caret: right after the ghost text, centered on the row.
        let p = ghost_keycap_pos((100, 40), 20, 60, None, 20, 0, 30, true, body);
        assert_eq!(p, Some((100 + 60 + ds::S2, 40 + (20 - CHIP_H) / 2)));
        // Text after the caret would be covered: no chip.
        assert_eq!(ghost_keycap_pos((100, 40), 20, 60, None, 20, 0, 30, false, body), None);
        // Several lines: after the last virtual row.
        let p = ghost_keycap_pos((100, 40), 20, 60, Some(50), 20, 2, 30, false, body);
        assert_eq!(p, Some((20 + 50 + ds::S2, 80 + (20 - CHIP_H) / 2)));
        // Out of room on the right.
        assert_eq!(ghost_keycap_pos((380, 40), 20, 60, None, 20, 0, 30, true, body), None);
    }
}
