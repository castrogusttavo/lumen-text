//! Friendly empty states of the Git panel: an icon badge, a one-line title, a short hint and the
//! action that makes sense next (never a blank panel).

use super::*;
use crate::ds;
use crate::ui_kit::button::{BtnStyle, draw_button};

/// One button of an empty state: label, what it does, primary (accent fill) or secondary.
pub(super) struct EmptyBtn {
    pub label: &'static str,
    pub hit: Hit,
    pub primary: bool,
}

impl App {
    /// Centered stack in `body`: badge with `icon` in `tint`, bold `title`, wrapped `hint`, buttons.
    pub(super) fn gp_empty_state(
        &mut self,
        cv: &mut Canvas,
        gp: &mut GitPanel,
        body: Rect,
        (icon, tint): (&str, Rgba),
        (title, hint): (&str, &str),
        buttons: &[EmptyBtn],
    ) {
        let c = self.theme.chrome;
        let cx = body.x + body.w / 2;
        let mut y = body.y + ds::S6 + ds::S2;
        let badge = Rect::new(cx - 24, y, 48, 48);
        cv.fill_round(badge, 24, tint.with_alpha(0x1e));
        cv.ring(badge, 24, tint.with_alpha(0x48));
        self.tx.icon(cv, icon, badge.x + 12, badge.y + 12, ds::ICON_L + 4, tint, body);
        y = badge.bottom() + ds::S3;
        let tw = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, cx - tw / 2, y, title, c.tab_active_fg, body, true);
        y += LINE + ds::S1;
        for line in wrap_lines(&mut self.tx, hint, body.w - 2 * ds::S5, 3) {
            let lw = self.tx.ui_width(&line, false);
            self.tx.draw_ui(cv, cx - lw / 2, y, &line, c.tab_inactive_fg, body, false);
            y += STRIP_LINE;
        }
        y += ds::S3;
        for b in buttons {
            let bw = (self.tx.ui_width(b.label, b.primary) + 2 * ds::S4).min(body.w - 2 * ds::S3);
            let r = Rect::new(cx - bw / 2, y, bw, ds::HIT_MIN + ds::S1);
            let hot = self.hv(r);
            let style = if b.primary { BtnStyle::Primary } else { BtnStyle::Secondary };
            draw_button(cv, &mut self.tx, &c, r, b.label, style, false, hot);
            gp.hits.push((r, b.hit));
            y += r.h + ds::S2;
        }
    }
}
