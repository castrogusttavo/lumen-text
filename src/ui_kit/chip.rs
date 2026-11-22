//! Chips: small pills with a word or a counter ("function", "+12", "v3", "deprecated"). One
//! implementation for the hover header, the viewers and the tool pages.

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

/// The primitive: a pill of `bg` (with an optional `ring`) and `fg` text at (`x`, `y`), clipped
/// to `clip`; returns its rect.
#[allow(clippy::too_many_arguments)]
pub fn pill(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    (x, y): (i32, i32),
    text: &str,
    fg: Rgba,
    bg: Rgba,
    ring: Option<Rgba>,
    clip: Rect,
) -> Rect {
    let r = Rect::new(x, y, chip_w(tx, text), CHIP_H);
    cv.fill_round_clip(r, ds::R_CTRL, bg, clip);
    if let Some(rc) = ring {
        cv.ring(r, ds::R_CTRL, rc);
    }
    tx.draw_ui_sized_i(
        cv,
        r.x + CHIP_PAD,
        r.y + (CHIP_H - CHIP_LINE) / 2,
        text,
        fg,
        clip.intersect(&r),
        false,
        false,
        CHIP_FONT,
    );
    r
}

/// Pill with explicit colors, clipped to the canvas; returns its width. For counters like `+12`.
pub fn chip(cv: &mut Canvas, tx: &mut TextCtx, pos: (i32, i32), text: &str, fg: Rgba, bg: Rgba) -> i32 {
    let clip = cv.bounds();
    pill(cv, tx, pos, text, fg, bg, None, clip).w
}

/// Tinted pill of a [`Tone`] at (`x`, `y`); returns its rect.
pub fn draw_chip(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    c: &Chrome,
    pos: (i32, i32),
    text: &str,
    tone: Tone,
    clip: Rect,
) -> Rect {
    let col = tone_color(c, tone);
    pill(cv, tx, pos, text, col, col.with_alpha(0x24), Some(col.with_alpha(0x40)), clip)
}

/// Text color that reads on a `tone`-tinted chip over `page`.
pub fn tone_fg(tone: Rgba, page: Rgba) -> Rgba {
    tone.with_alpha(255).readable_on(&[tone_bg(tone, page)], 4.5)
}

/// Soft fill of a chip of `tone` over `page`.
pub fn tone_bg(tone: Rgba, page: Rgba) -> Rgba {
    tone.with_alpha(0x28).over(page.with_alpha(255))
}

/// A pill tinted by a raw `tone` color over `page`, its text nudged to read; returns its width.
/// For the viewers, where the tone is a semantic color (coverage, results) and not a [`Tone`].
pub fn chip_on(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    pos: (i32, i32),
    text: &str,
    tone: Rgba,
    page: Rgba,
    clip: Rect,
) -> i32 {
    pill(cv, tx, pos, text, tone_fg(tone, page), tone_bg(tone, page), None, clip).w
}

/// Same as [`chip_on`] but right-aligned: its right edge at `right`. Returns the chip's left edge.
pub fn chip_on_right(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    (right, y): (i32, i32),
    text: &str,
    tone: Rgba,
    page: Rgba,
    clip: Rect,
) -> i32 {
    let x = right - chip_w(tx, text);
    chip_on(cv, tx, (x, y), text, tone, page, clip);
    x
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
    fn chip_text_reads_on_its_fill() {
        for page in [Rgba(20, 20, 24, 255), Rgba(250, 250, 250, 255)] {
            for tone in [Rgba(63, 185, 80, 255), Rgba(227, 160, 8, 255), Rgba(248, 81, 73, 255)] {
                assert!(tone_fg(tone, page).contrast(tone_bg(tone, page)) >= 3.0);
            }
        }
    }
}
