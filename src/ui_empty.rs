//! Reusable empty state: an icon tile, a one-line title, a one-line hint and an optional primary
//! action button, centered in a pane. Other modules call `draw_empty` instead of leaving a blank
//! panel; the returned rect is the button's hit area (register it as a clickable region with a
//! pointer cursor).

use crate::{
    anim::fade,
    ds::{ICON_L, R_CARD, R_CTRL, S1, S2, S3, S4, S5},
    theme::Theme,
    ui::{Canvas, Rect, TextCtx, ui_line},
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
    let line = ui_line() as i32;
    let mut y = area.y + ((area.h - empty_height(e.action.is_some())) / 2).max(S2);
    let cx = area.x + area.w / 2;
    let max_w = (area.w - 2 * S4).max(40);

    let tile = Rect::new(cx - TILE / 2, y, TILE, TILE);
    cv.fill_round(tile, R_CARD, fade(c.widget_bg, a));
    cv.ring(tile, R_CARD, fade(c.border, a));
    tx.icon(cv, e.icon, cx - ICON_L / 2 - 2, y + (TILE - ICON_L - 4) / 2, ICON_L + 4, fade(c.accent_text, a), area);
    y += TILE + S3;

    let title = crate::app_gitpanel::fit(tx, e.title, max_w, true);
    let tw = tx.ui_width(&title, true);
    tx.draw_ui(cv, cx - tw / 2, y, &title, fade(c.tab_active_fg, a), area, true);
    y += line + S1;
    let hint = crate::app_gitpanel::fit(tx, e.hint, max_w, false);
    let hw = tx.ui_width(&hint, false);
    tx.draw_ui(cv, cx - hw / 2, y, &hint, fade(c.tab_inactive_fg, a), area, false);
    y += line + S4;

    let label = e.action?;
    let lw = tx.ui_width(label, true);
    let w = (lw + 2 * S5).min(max_w);
    let r = Rect::new(cx - w / 2, y, w, BTN_H);
    let hot = r.contains(mouse.0, mouse.1);
    cv.fill_round(r, R_CTRL + 2, fade(if hot { c.accent.shift(14) } else { c.accent }, a));
    let label = crate::app_gitpanel::fit(tx, label, w - 2 * S3, true);
    let lw = tx.ui_width(&label, true);
    tx.draw_ui(cv, r.x + (r.w - lw) / 2, r.y + (BTN_H - line) / 2, &label, fade(c.on_accent, a), r, true);
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
