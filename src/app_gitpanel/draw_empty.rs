//! Friendly empty states of the Git panel, through the shared `ui_kit::empty` (icon tile, one-line
//! title, one-line hint) plus the action that makes sense next (never a blank panel).

use super::*;
use crate::ds;
use crate::ui_kit::{
    button::{BtnStyle, draw_button},
    empty::empty_head,
};

/// One button of an empty state: label, what it does, primary (accent fill) or secondary.
pub(super) struct EmptyBtn {
    pub label: &'static str,
    pub hit: Hit,
    pub primary: bool,
}

impl App {
    /// The shared empty state near the top of `body` (`icon` tile, `title`, `hint`) and the buttons
    /// stacked under it. Every button is a hit of the panel (hand, hover and pressed look).
    pub(super) fn gp_empty_state(
        &mut self,
        cv: &mut Canvas,
        gp: &mut GitPanel,
        body: Rect,
        icon: &str,
        (title, hint): (&str, &str),
        buttons: &[EmptyBtn],
    ) {
        let c = self.theme.chrome;
        let cx = body.x + body.w / 2;
        let mut y = empty_head(cv, &mut self.tx, &self.theme, body, body, icon, title, hint);
        for b in buttons {
            let bw = (self.tx.ui_width(b.label, b.primary) + 2 * ds::S5).min(body.w - 2 * ds::S3);
            let r = Rect::new(cx - bw / 2, y, bw, ds::HIT_MIN + ds::S1);
            let hot = self.hv(r);
            let style = if b.primary { BtnStyle::Primary } else { BtnStyle::Secondary };
            draw_button(cv, &mut self.tx, &c, r, b.label, style, false, hot);
            if hot && gp.pressed == Some(b.hit) {
                cv.fill_round(r, ds::R_ROW, c.pressed_overlay);
            }
            gp.hits.push((r, b.hit));
            y += r.h + ds::S2;
        }
    }
}
