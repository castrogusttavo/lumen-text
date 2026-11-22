//! Buttons: primary (accent fill), secondary (bordered) and danger. The caller owns the hit
//! registration; this only draws. `focused` is the keyboard default (Enter), `hot` the pointer.

use crate::{
    anim::fade,
    ds,
    theme::{Chrome, Rgba},
    ui::{Canvas, Rect, TextCtx},
};

/// Look of a button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnStyle {
    /// The answer to give: accent fill.
    Primary,
    /// The other choices: quiet, bordered.
    Secondary,
    /// Destructive: error text, error fill when focused.
    Danger,
}

/// One button.
#[allow(clippy::too_many_arguments)]
pub fn draw_button(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    c: &Chrome,
    r: Rect,
    label: &str,
    style: BtnStyle,
    focused: bool,
    hot: bool,
) {
    draw_button_a(cv, tx, c, r, label, style, focused, hot, 1.0);
}

/// [`draw_button`] faded by `a` (0..1), for entrances.
#[allow(clippy::too_many_arguments)]
pub fn draw_button_a(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    c: &Chrome,
    r: Rect,
    label: &str,
    style: BtnStyle,
    focused: bool,
    hot: bool,
    a: f32,
) {
    let fg = c.tab_active_fg;
    let rad = ds::R_ROW;
    let text = match style {
        BtnStyle::Primary => {
            let fill = if hot { c.accent.mix(fg, 0.14) } else { c.accent };
            cv.card(r, rad, fade(fill, a), fade(c.accent, a));
            c.on_accent
        }
        BtnStyle::Secondary => {
            cv.card(r, rad, fade(c.widget_bg, a), fade(c.border, a));
            if hot || focused {
                cv.fill_round(r.inset(1), rad - 1, fade(c.hover_overlay, a));
            }
            fg
        }
        BtnStyle::Danger => {
            cv.card(r, rad, fade(c.widget_bg, a), fade(c.error.with_alpha(0x90), a));
            if focused {
                cv.fill_round(r.inset(1), rad - 1, fade(c.error, a));
                if c.error.luminance() > 0.55 { Rgba(0, 0, 0, 255) } else { Rgba(255, 255, 255, 255) }
            } else {
                if hot {
                    cv.fill_round(r.inset(1), rad - 1, fade(c.error.with_alpha(0x28), a));
                }
                c.error
            }
        }
    };
    if focused {
        cv.ring(r.inset(-2), rad + 2, fade(c.focus_ring.with_alpha(0xa0), a));
    }
    let tw = tx.ui_width(label, true);
    tx.draw_ui(cv, r.x + (r.w - tw) / 2, r.y + (r.h - 18) / 2, label, fade(text, a), r, true);
}
