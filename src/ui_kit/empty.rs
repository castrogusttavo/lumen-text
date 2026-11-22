//! The one empty state: an icon tile, a one-line title, a one-line hint and an optional primary
//! action button. [`draw_empty`] centers the whole stack in a pane; [`empty_head`] draws the
//! text part near the top of a pane and hands back the y where the caller puts its own action.
//! The returned button rect is the hit area (register it as a clickable region, pointer cursor).

use crate::{
    anim::fade,
    app_gitpanel::fit,
    ds::{ICON_L, R_CARD, S1, S2, S3, S4, S5},
    theme::Theme,
    ui::{Canvas, Rect, TextCtx, ui_line},
    ui_kit::button::{BtnStyle, draw_button_a},
};

/// What to say: all strings are drawn on one line each (ellipsized to the pane width).
pub struct Empty<'a> {
    pub icon: &'a str,
    pub title: &'a str,
    pub hint: &'a str,
    /// Label of the primary action button, if any.
    pub action: Option<&'a str>,
}

const TILE: i32 = 48;
const BTN_H: i32 = 30;

/// Height of the whole stack: used to center it, exposed so callers can reserve room.
pub fn empty_height(has_action: bool) -> i32 {
    let line = ui_line() as i32;
    TILE + S3 + line + S1 + line + if has_action { S4 + BTN_H } else { 0 }
}

/// Tile, title and hint with the tile's top at `top`, centered on `area`, faded by `a`.
/// Returns the y just below the hint.
#[allow(clippy::too_many_arguments)]
fn stack(
    tx: &mut TextCtx,
    cv: &mut Canvas,
    th: &Theme,
    area: Rect,
    clip: Rect,
    top: i32,
    (icon, title, hint): (&str, &str, &str),
    a: f32,
) -> i32 {
    let c = th.chrome;
    let line = ui_line() as i32;
    let cx = area.x + area.w / 2;
    let max_w = (area.w - 2 * S4).max(40);
    let mut y = top;

    let tile = Rect::new(cx - TILE / 2, y, TILE, TILE);
    cv.fill_round(tile, R_CARD, fade(c.widget_bg, a));
    cv.ring(tile, R_CARD, fade(c.border_glow, a));
    tx.icon(cv, icon, cx - ICON_L / 2 - 2, y + (TILE - ICON_L - 4) / 2, ICON_L + 4, fade(c.accent_text, a), clip);
    y += TILE + S3;

    let title = fit(tx, title, max_w, true);
    let tw = tx.ui_width(&title, true);
    tx.draw_ui(cv, cx - tw / 2, y, &title, fade(c.tab_active_fg, a), clip, true);
    y += line + S1;
    let hint = fit(tx, hint, max_w, false);
    let hw = tx.ui_width(&hint, false);
    tx.draw_ui(cv, cx - hw / 2, y, &hint, fade(c.tab_inactive_fg, a), clip, false);
    y + line
}

/// Icon tile, title and hint near the top of `area` (clipped to `clip`). Returns the y below
/// them, where the caller puts its primary action.
#[allow(clippy::too_many_arguments)]
pub fn empty_head(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    th: &Theme,
    area: Rect,
    clip: Rect,
    icon: &str,
    title: &str,
    hint: &str,
) -> i32 {
    let top = area.y + ((area.h - empty_height(true)) / 3).clamp(S5, 96);
    stack(tx, cv, th, area, clip, top, (icon, title, hint), 1.0) + S3
}

/// Draw the empty state centered in `area`, faded by `a` (0..1, for entrances); returns the action
/// button rect when there is one.
pub fn draw_empty(
    tx: &mut TextCtx,
    cv: &mut Canvas,
    th: &Theme,
    area: Rect,
    e: &Empty,
    mouse: (f64, f64),
    a: f32,
) -> Option<Rect> {
    let c = th.chrome;
    let top = area.y + ((area.h - empty_height(e.action.is_some())) / 2).max(S2);
    let y = stack(tx, cv, th, area, area, top, (e.icon, e.title, e.hint), a) + S4;

    let label = e.action?;
    let cx = area.x + area.w / 2;
    let max_w = (area.w - 2 * S4).max(40);
    let w = (tx.ui_width(label, true) + 2 * S5).min(max_w);
    let r = Rect::new(cx - w / 2, y, w, BTN_H);
    let hot = r.contains(mouse.0, mouse.1);
    let label = fit(tx, label, w - 2 * S3, true);
    draw_button_a(cv, tx, &c, r, &label, BtnStyle::Primary, false, hot, a);
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_adds_height() {
        assert!(empty_height(true) > empty_height(false));
        assert_eq!(empty_height(true) - empty_height(false), S4 + BTN_H);
    }
}
