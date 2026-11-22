//! Small shared pieces of the remaining viewers (multi-buffer, merge, coverage, markdown): tinted
//! chips and coverage thresholds. Kept separate from the other viewer helpers on purpose so the
//! parallel branches never collide; unify with `ui_viewer` / `ui_popup` later.

#![allow(dead_code)]

use crate::{
    ds,
    theme::Rgba,
    ui::{Canvas, Rect, TextCtx},
};

/// UI text size of a chip.
const CHIP_SIZE: f32 = 11.0;
/// Height of a chip.
pub const CHIP_H: i32 = 18;

/// How well covered a file is: green from 80 %, yellow from 50 %, red below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Good,
    Fair,
    Poor,
}

impl Level {
    pub fn of(pct: f32) -> Level {
        if pct >= 80.0 {
            Level::Good
        } else if pct >= 50.0 {
            Level::Fair
        } else {
            Level::Poor
        }
    }
}

/// Text color that reads on a `tone`-tinted chip over `page`.
pub fn tone_fg(tone: Rgba, page: Rgba) -> Rgba {
    tone.with_alpha(255).readable_on(&[tone_bg(tone, page)], 4.5)
}

/// Soft fill of a chip of `tone` over `page`.
pub fn tone_bg(tone: Rgba, page: Rgba) -> Rgba {
    tone.with_alpha(0x28).over(page.with_alpha(255))
}

/// Width of a chip showing `text`.
pub fn chip_w(tx: &mut TextCtx, text: &str) -> i32 {
    tx.ui_width_sized(text, true, CHIP_SIZE) + 2 * ds::S2
}

/// A pill of `tone` over `page` with `text`, left edge at `x`, top at `y`; returns its width.
pub fn chip(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    (x, y): (i32, i32),
    text: &str,
    tone: Rgba,
    page: Rgba,
    clip: Rect,
) -> i32 {
    let w = chip_w(tx, text);
    let r = Rect::new(x, y, w, CHIP_H);
    cv.fill_round_clip(r, CHIP_H / 2, tone_bg(tone, page), clip);
    tx.draw_ui_sized(
        cv,
        x + ds::S2,
        y + (CHIP_H - 15) / 2,
        text,
        tone_fg(tone, page),
        clip.intersect(&r),
        true,
        CHIP_SIZE,
    );
    w
}

/// Same as [`chip`] but right-aligned: its right edge at `right`. Returns the chip's left edge.
pub fn chip_right(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    (right, y): (i32, i32),
    text: &str,
    tone: Rgba,
    page: Rgba,
    clip: Rect,
) -> i32 {
    let x = right - chip_w(tx, text);
    chip(cv, tx, (x, y), text, tone, page, clip);
    x
}

/// "3 results" / "1 result".
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_levels_follow_the_thresholds() {
        assert_eq!(Level::of(100.0), Level::Good);
        assert_eq!(Level::of(80.0), Level::Good);
        assert_eq!(Level::of(79.9), Level::Fair);
        assert_eq!(Level::of(50.0), Level::Fair);
        assert_eq!(Level::of(49.9), Level::Poor);
        assert_eq!(Level::of(0.0), Level::Poor);
    }

    #[test]
    fn chip_text_reads_on_its_fill() {
        for page in [Rgba(20, 20, 24, 255), Rgba(250, 250, 250, 255)] {
            for tone in [Rgba(63, 185, 80, 255), Rgba(227, 160, 8, 255), Rgba(248, 81, 73, 255)] {
                assert!(tone_fg(tone, page).contrast(tone_bg(tone, page)) >= 3.0);
            }
        }
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "result", "results"), "1 result");
        assert_eq!(plural(0, "result", "results"), "0 results");
        assert_eq!(plural(12, "conflict", "conflicts"), "12 conflicts");
    }
}
