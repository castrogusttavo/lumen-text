//! Editor chrome: the tab strip (pill tabs with an indicator that glides to the active tab, a dirty
//! dot that turns into the close button under the pointer) and the diagnostics chip in the corner of
//! the text area (errors / warnings of the current file, click opens Problems).
//!
//! Motion comes from `anim`; every animated value is advanced with the frame `dt` and reported
//! through the returned `busy` flag, so an idle editor still sleeps.

use crate::{
    anim::{Anim, Hot, dur, fade},
    app::{App, TAB_NAME_MAX},
    app_topbar::StatusBtn,
    ds,
    theme::Rgba,
    ui::{Canvas, Rect},
};

/// Left padding of the strip and the room around a tab's label (icon, close button).
const STRIP_PAD: i32 = 6;
const LEAD: i32 = 34;
const TRAIL: i32 = 34;
/// Width of the accent underline that rides on the active pill.
const MARK_W: i32 = 14;

/// Animation state of the editor chrome.
#[derive(Debug)]
pub struct TabsFx {
    /// The active-tab pill in strip coordinates (before the strip scrolls).
    ind_x: Anim,
    ind_w: Anim,
    ind_ready: bool,
    /// Diagnostics chip visibility and the counts it keeps showing while it fades out.
    chip: Hot,
    chip_counts: (usize, usize),
}

impl Default for TabsFx {
    fn default() -> Self {
        Self {
            ind_x: Anim::new(0.0, dur::SLOW),
            ind_w: Anim::new(0.0, dur::SLOW),
            ind_ready: false,
            chip: Hot::default(),
            chip_counts: (0, 0),
        }
    }
}

impl TabsFx {
    /// Point the pill at the tab spanning `x..x + w` (strip coordinates). The first target jumps.
    fn aim(&mut self, x: i32, w: i32) {
        if self.ind_ready {
            self.ind_x.to(x as f32);
            self.ind_w.to(w as f32);
        } else {
            self.ind_ready = true;
            self.ind_x.snap(x as f32);
            self.ind_w.snap(w as f32);
        }
    }

    /// Advance the pill; true while it moves.
    fn step(&mut self, dt: f64) -> bool {
        let a = self.ind_x.step(dt);
        let b = self.ind_w.step(dt);
        a | b
    }

    /// The pill's current span (strip coordinates).
    fn span(&self) -> (i32, i32) {
        (self.ind_x.value.round() as i32, self.ind_w.value.round().max(0.0) as i32)
    }
}

/// (errors, warnings) of one document.
pub fn doc_counts(diags: &[crate::doc::Diag]) -> (usize, usize) {
    let e = diags.iter().filter(|g| g.severity == 1).count();
    let w = diags.iter().filter(|g| g.severity == 2).count();
    (e, w)
}

impl App {
    /// Draw the tab strip. Returns true while an animation needs more frames.
    pub fn draw_tabs(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        let c = self.theme.chrome;
        let bar = self.layout.tabbar;
        cv.fill(bar, c.tabbar_bg);
        self.layout.tabs.clear();
        let pane_docs = self.split_pane_docs();
        // Geometry of every tab in strip coordinates, then scroll so the active one is visible.
        let order = crate::app_ctxmenu::tab_order(&self.docs);
        let mut geo: Vec<(usize, i32, i32)> = Vec::with_capacity(order.len());
        let mut x = STRIP_PAD;
        for &i in &order {
            let w = self.tx.ui_width(&self.tab_display_name(i), false).min(TAB_NAME_MAX)
                + LEAD
                + TRAIL
                + 11 * pane_docs.iter().filter(|p| p.0 == i).count() as i32;
            geo.push((i, x, w));
            x += w;
        }
        let view = (bar.w - 2 * STRIP_PAD - crate::app_quick::QUICK_W).max(1) as f64;
        self.tabs_max = ((x - STRIP_PAD) as f64 - view).max(0.0);
        self.inp.tabs.geo.clone_from(&geo);
        let dragging = self.tab_dragging();
        if self.tabs_seen != Some(self.active) {
            self.tabs_seen = Some(self.active);
            if let Some(&(_, tx0, tw)) = geo.iter().find(|(i, _, _)| *i == self.active) {
                let (l, r) = (tx0 as f64 - 6.0, (tx0 + tw) as f64 + 6.0);
                if l < self.tabs_scroll {
                    self.tabs_scroll = l;
                } else if r > self.tabs_scroll + view + 6.0 {
                    self.tabs_scroll = r - view - 6.0;
                }
            }
        }
        self.tabs_scroll = self.tabs_scroll.clamp(0.0, self.tabs_max);
        let off = self.tabs_scroll.round() as i32;
        let strip = Rect::new(bar.x + 2, bar.y, bar.w - 4, bar.h);

        // The active pill glides to its tab instead of jumping.
        let active_geo = geo.iter().find(|(i, _, _)| *i == self.active).copied();
        if let Some((_, tx0, tw)) = active_geo {
            self.tabs_fx.aim(tx0 + 2, tw - 4);
        }
        let busy = self.tabs_fx.step(dt);
        let (px, pw) = self.tabs_fx.span();
        if active_geo.is_some() && pw > 0 {
            let pill = Rect::new(bar.x + px - off, bar.y + 5, pw, bar.h - 10);
            cv.fill_round_clip(pill, ds::R_ROW, c.tab_active_bg, strip);
            if pane_docs.len() < 2 {
                let mark = Rect::new(pill.x + (pill.w - MARK_W) / 2, pill.bottom() - 3, MARK_W, 2);
                cv.fill_round_clip(mark, 1, c.accent, strip);
            }
        }

        let (mx, my) = self.mouse;
        for (i, tx0, w) in geo {
            let r = Rect::new(bar.x + tx0 - off, bar.y, w, bar.h);
            if r.right() < strip.x || r.x > strip.right() {
                continue;
            }
            let active = i == self.active;
            let fg = if active { c.tab_active_fg } else { c.tab_inactive_fg };
            let ty = r.y + (bar.h - 18) / 2;
            let fname = self.tx.icons.file_icon(&self.docs[i].name);
            self.tx.icon(cv, &fname, r.x + 12, r.y + (bar.h - 16) / 2, ds::ICON_M, fg, strip);
            let (label, name_w) = self.tab_label(i, active);
            self.tx.draw_ui(cv, r.x + LEAD, ty, &label, fg, strip, active);
            let d = &self.docs[i];
            let close = Rect::new(r.right() - 28, r.y, 28, bar.h);
            let over_tab = strip.contains(mx, my) && r.contains(mx, my) && self.overlay.is_none() && !dragging;
            let over_close = over_tab && close.contains(mx, my);
            let dirty = d.buf.is_dirty() || d.mb.as_ref().is_some_and(|m| m.is_dirty());
            let pinned = d.pinned;
            if over_tab {
                // Hovering a tab (focused or not) turns the dirty dot / pin into the close button; it
                // gets its own background when the pointer is on it.
                let cb = Rect::new(close.x + 4, r.y + (bar.h - 20) / 2, 20, 20);
                if over_close {
                    cv.fill_round_clip(cb, ds::R_CTRL, self.theme.foreground.with_alpha(0x38), strip);
                }
                let ic = if over_close { c.tab_active_fg } else { c.tab_inactive_fg };
                self.tx.icon(cv, "ui/close", close.x + 8, r.y + (bar.h - 12) / 2, 12, ic, strip);
            } else if dirty {
                // Like Zed: the unsaved dot takes the close button's place.
                cv.fill_round_clip(Rect::new(close.x + 10, r.y + bar.h / 2 - 4, 8, 8), 4, c.accent, strip);
            } else if pinned {
                self.tx.icon(cv, "ui/pin", close.x + 8, r.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, strip);
            } else if active {
                self.tx.icon(cv, "ui/close", close.x + 8, r.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, strip);
            }
            // Split editor: an underline and the pane number for every pane that shows this tab, in
            // the accent color for the focused pane and gray for the others.
            let shown = pane_docs.iter().filter(|p| p.0 == i).count() as i32;
            for (k, &(_, focused)) in pane_docs.iter().enumerate().filter(|(_, p)| p.0 == i) {
                let (col, label) = if focused {
                    (c.accent, c.accent_text)
                } else {
                    (c.tab_inactive_fg.with_alpha(0x80), c.tab_inactive_fg)
                };
                let seg = (r.w - 28) / shown;
                let nth = pane_docs.iter().take(k).filter(|p| p.0 == i).count() as i32;
                cv.fill_clip(Rect::new(r.x + 14 + nth * seg, bar.bottom() - 5, (seg - 2).max(4), 2), col, strip);
                self.tx.draw_ui(
                    cv,
                    r.x + LEAD + name_w + 6 + nth * 11,
                    ty + 1,
                    &(k + 1).to_string(),
                    label,
                    strip,
                    focused,
                );
            }
            self.layout.tabs.push((r.intersect(&strip), close.intersect(&strip), i));
        }
        // Soft edge fades hint that more tabs hide beyond the strip.
        let bg = c.tabbar_bg;
        if self.tabs_scroll > 0.5 {
            for k in 0..14 {
                cv.fill_clip(
                    Rect::new(strip.x + k, bar.y, 1, bar.h),
                    bg.with_alpha((0xd0 - k * 14).max(0) as u8),
                    strip,
                );
            }
        }
        if self.tabs_scroll < self.tabs_max - 0.5 {
            for k in 0..14 {
                cv.fill_clip(
                    Rect::new(strip.right() - 1 - k, bar.y, 1, bar.h),
                    bg.with_alpha((0xd0 - k * 14).max(0) as u8),
                    strip,
                );
            }
        }
        busy
    }

    /// The diagnostics chip in the top-right corner of the text area: errors and warnings of this
    /// file, fading in when there are any. A click opens Problems (it joins the status buttons, so
    /// the pointer cursor and the tooltip come for free). Returns true while it fades.
    pub fn draw_diag_chip(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        let shown = self
            .docs
            .get(self.active)
            .filter(|d| !d.showing_image() && d.sql.is_none() && d.mb.is_none() && d.api.is_none() && d.rect.w > 0);
        let counts = shown.map_or((0, 0), |d| doc_counts(&d.diags));
        let rect = shown.map_or(Rect::default(), |d| d.rect);
        let fx = &mut self.tabs_fx;
        if counts.0 + counts.1 > 0 {
            fx.chip_counts = counts;
        }
        fx.chip.set(counts.0 + counts.1 > 0);
        let busy = fx.chip.step(dt);
        let k = fx.chip.v();
        let (e, w) = fx.chip_counts;
        if k < 0.01 || rect.w <= 0 {
            return busy;
        }
        let c = self.theme.chrome;
        let kinds: Vec<(&str, usize, Rgba)> =
            [("ui/x_circle", e, c.error), ("ui/warning", w, c.warning)].into_iter().filter(|t| t.1 > 0).collect();
        let texts: Vec<String> = kinds.iter().map(|t| t.1.to_string()).collect();
        let mut body = ds::S2;
        for t in &texts {
            body += 12 + 3 + self.tx.ui_width(t, false) + ds::S2;
        }
        // Left of the overlay scrollbar, a little below the top edge; settles 4 px as it fades in.
        let chip = Rect::new(rect.right() - 18 - body, rect.y + ds::S2 + ((1.0 - k) * 4.0) as i32, body, 22);
        let hot = self.overlay.is_none() && chip.contains(self.mouse.0, self.mouse.1);
        cv.fill_round(chip, ds::R_CTRL, fade(self.theme.background.with_alpha(0xe6), k));
        cv.fill_round(chip, ds::R_CTRL, fade(c.list_hover_bg.with_alpha(if hot { 0xff } else { 0x60 }), k));
        cv.ring(chip, ds::R_CTRL, fade(c.border, k));
        let y = chip.y + (chip.h - 18) / 2;
        let mut x = chip.x + ds::S2;
        for ((icon, _, col), t) in kinds.iter().zip(&texts) {
            let col = fade(*col, k);
            self.tx.icon(cv, icon, x, chip.y + (chip.h - 12) / 2, 12, col, chip);
            x += 15;
            self.tx.draw_ui(cv, x, y, t, col, chip, false);
            x += self.tx.ui_width(t, false) + ds::S2;
        }
        self.layout.status_btns.push((chip, StatusBtn::Problems));
        busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicator_jumps_first_then_glides() {
        let mut fx = TabsFx::default();
        fx.aim(10, 100);
        assert_eq!(fx.span(), (10, 100), "first target snaps");
        assert!(!fx.step(0.016));
        fx.aim(200, 80);
        assert!(fx.step(0.016), "moving after a new target");
        let (x, _) = fx.span();
        assert!(x > 10 && x < 200, "in between: {x}");
        for _ in 0..200 {
            fx.step(0.016);
        }
        assert_eq!(fx.span(), (200, 80));
    }

    #[test]
    fn counts_split_by_severity() {
        let g = |severity| crate::doc::Diag {
            sl: 0,
            sc: 0,
            el: 0,
            ec: 0,
            severity,
            message: String::new(),
            code: String::new(),
            source: String::new(),
        };
        assert_eq!(doc_counts(&[g(1), g(1), g(2), g(3), g(4)]), (2, 1));
        assert_eq!(doc_counts(&[]), (0, 0));
    }
}
