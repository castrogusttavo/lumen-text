//! The top of the panel, after Athas' Source Control: the header with the Changes / History tabs,
//! the branch row with the fetch / pull / push buttons, the commit composer, and the dropdowns they open.

use super::*;
use crate::ds;

impl App {
    /// The Changes / History tabs on the left, refresh / actions / close on the right.
    pub(super) fn gp_draw_header(&mut self, cv: &mut Canvas, gp: &mut GitPanel, p: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let y = p.y + (HEAD_H - 28) / 2;
        let mut right = p.right() - 8;
        for (icon, hit) in
            [("ui/close", Hit::Close), ("ui/ellipsis", Hit::Menu(GpMenu::Actions)), ("ui/rotate_cw", Hit::Refresh)]
        {
            self.gp_icon_btn(cv, gp, Rect::new(right - 28, y, 28, 28), icon, hit);
            right -= 30;
        }

        // Segmented pill: Changes (with the count) and History share one track; the active tab's
        // pill glides between them. Every tab has the width of its bold label, so nothing shifts.
        let n = gp.changes.len();
        let pad = 3;
        let mut items: Vec<(Tab, &str, Rect, String)> = Vec::new();
        let mut x = p.x + 8 + pad;
        for (tab, label) in [(Tab::Changes, "Changes"), (Tab::History, "History")] {
            let badge = if tab == Tab::Changes && n > 0 { n.to_string() } else { String::new() };
            let lw = self.tx.ui_width(label, true);
            let bw = if badge.is_empty() { 0 } else { (self.tx.ui_width(&badge, true) + 12).max(20) + 6 };
            let w = 10 + lw + bw + 10;
            if x + w + pad > right - 4 {
                break;
            }
            items.push((tab, label, Rect::new(x, y + pad, w, 28 - 2 * pad), badge));
            x += w + 2;
        }
        let Some(last) = items.last().map(|i| i.2) else { return };
        let track = Rect::new(p.x + 8, y, last.right() + pad - (p.x + 8), 28);
        cv.card(track, ds::R_ROW, c.input_bg, c.border.with_alpha(0x70));
        if let Some(cur) = items.iter().find(|i| i.0 == gp.tab) {
            gp.mo.place_tab(cur.2.x, cur.2.w);
        }
        let pill = Rect::new(gp.mo.tab_x.value.round() as i32, y + pad, gp.mo.tab_w.value.round() as i32, 28 - 2 * pad);
        cv.fill_round(pill, ds::R_ROW - 2, c.widget_bg);
        cv.ring(pill, ds::R_ROW - 2, strong.with_alpha(0x26));
        for (tab, label, r, badge) in items {
            let active = gp.tab == tab;
            let hot = self.hv(r);
            if !active {
                self.gp_wash(cv, gp, r, ds::R_ROW - 2, Hit::Tab(tab));
            }
            let col = if active || hot { strong } else { dim };
            let lw = self.tx.ui_width(label, true);
            self.tx.draw_ui(cv, r.x + 10, r.y + 2, label, col, r, active);
            if !badge.is_empty() {
                let bw = (self.tx.ui_width(&badge, true) + 12).max(20);
                let b = Rect::new(r.x + 10 + lw + 6, r.y + 3, bw, 16);
                cv.fill_round(b, 8, if active { c.accent } else { strong.with_alpha(0x22) });
                let tw = self.tx.ui_width(&badge, true);
                let tc = if active { c.on_accent } else { dim };
                self.tx.draw_ui_sized(cv, b.x + (b.w - tw) / 2, b.y + 1, &badge, tc, b, true, 12.0);
            }
            gp.hits.push((r, Hit::Tab(tab)));
        }
    }

    /// Branch chip (repo / branch, opens the branch switcher) and the sync button on its right.
    pub(super) fn gp_draw_identity(&mut self, cv: &mut Canvas, gp: &mut GitPanel, p: Rect, by: i32) {
        if gp.top.is_none() {
            return;
        }
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let y = by + 3;
        let busy = gp.job.is_some();
        let (ahead, behind, has_up) = (gp.sync.ahead, gp.sync.behind, gp.sync.upstream.is_some());
        // Fetch / Pull / Push as buttons of their own (the counts ride on Pull and Push) and a
        // chevron for the rest (pull strategy). Only what can run now is clickable.
        let chev_w = 20;
        let chev = Rect::new(p.right() - 8 - chev_w, y, chev_w, 28);
        self.gp_wash(cv, gp, chev, ds::R_ROW, Hit::Menu(GpMenu::Sync));
        if gp.menu == Some(GpMenu::Sync) {
            cv.fill_round(chev, ds::R_ROW, c.pressed_overlay);
        }
        self.tx.icon(cv, "ui/chevron_down", chev.x + 3, y + 7, 14, dim, chev);
        gp.hits.push((chev, Hit::Menu(GpMenu::Sync)));
        let mut right = chev.x;
        let buttons = [
            (Hit::Push, "ui/arrow_up", if has_up { ahead } else { 0 }, !busy && (ahead > 0 || !has_up), c.accent_text),
            (Hit::Pull, "ui/arrow_down", behind, !busy && behind > 0, c.warning),
            (Hit::Fetch, if busy { "ui/rotate_cw" } else { "ui/cloud_download" }, 0, !busy, dim),
        ];
        for (hit, icon, n, enabled, tone) in buttons {
            let cnt = if n > 0 { n.to_string() } else { String::new() };
            let cw = if cnt.is_empty() { 0 } else { self.tx.ui_width(&cnt, true) + 3 };
            let w = 8 + 14 + cw + 6;
            let r = Rect::new(right - w, y, w, 28);
            let col = if busy && hit == Hit::Fetch {
                // A running command: the fetch icon breathes.
                let t = gp.job.as_ref().map_or(0.0, |j| j.started.elapsed().as_secs_f32());
                c.accent_text.with_alpha((255.0 * super::motion::pulse(t)) as u8)
            } else if enabled {
                tone
            } else {
                dim.with_alpha(0x70)
            };
            if enabled && n > 0 {
                // Something to send / receive: the button carries its tone.
                cv.fill_round(r, ds::R_CTRL + 2, tone.with_alpha(0x1a));
            }
            if enabled {
                self.gp_wash(cv, gp, r, ds::R_ROW, hit);
            }
            self.tx.icon(cv, icon, r.x + 8, y + 7, 14, col, r);
            if !cnt.is_empty() {
                self.tx.draw_ui(cv, r.x + 8 + 14 + 3, y + 5, &cnt, col, r, true);
            }
            if enabled {
                gp.hits.push((r, hit));
            } else {
                gp.disabled.push(r);
            }
            right = r.x;
        }
        let main = Rect::new(right, y, 0, 28);

        // The chip: icon, "repo / branch", chevron.
        let repo =
            gp.top.as_ref().and_then(|t| t.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let branch = self.branch.clone().unwrap_or_else(|| "detached".into());
        let max_w = (main.x - 4 - (p.x + 8)).max(0);
        let inner = max_w - (8 + 14 + 6 + 6 + 14 + 4);
        let repo_txt = format!("{repo} / ");
        let rw = self.tx.ui_width(&repo_txt, false);
        let bw = self.tx.ui_width(&branch, true);
        let show_repo = !repo.is_empty() && rw + bw <= inner;
        let used_repo = if show_repo { rw } else { 0 };
        let branch = fit(&mut self.tx, &branch, inner - used_repo, true);
        let tw = used_repo + self.tx.ui_width(&branch, true);
        let chip = Rect::new(p.x + 8, y, (8 + 14 + 6 + tw + 6 + 14 + 4).min(max_w), 28);
        let edge = if self.hv(chip) { c.border_glow } else { c.border };
        cv.card(chip, ds::R_ROW, c.input_bg, edge);
        self.gp_wash(cv, gp, chip, ds::R_ROW, Hit::Branches);
        self.tx.icon(cv, "ui/git_branch", chip.x + 8, y + 7, 14, dim, chip);
        let tx0 = chip.x + 8 + 14 + 6;
        if show_repo {
            self.tx.draw_ui(cv, tx0, y + 5, &repo_txt, dim, chip, false);
        }
        self.tx.draw_ui(cv, tx0 + used_repo, y + 5, &branch, strong, chip, true);
        self.tx.icon(cv, "ui/chevron_down", chip.right() - 4 - 14, y + 7, 14, dim, chip);
        gp.hits.push((chip, Hit::Branches));
    }

    /// The commit composer: a rounded box with the message, and under it the sparkle button, what
    /// the commit takes (with its +N / -N) and the Commit button with its options.
    pub(super) fn gp_draw_commit_area(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let msg_h = gp.msg_h();
        let bx = Rect::new(area.x + COMPOSER_MARGIN, area.y + 6, area.w - 2 * COMPOSER_MARGIN, msg_h + COMPOSER_ROW_H);
        cv.fill_round(bx, 10, c.input_bg);
        // Focus ring: eases from the border to the accent and gains a soft glow.
        gp.mo.msg_ring.set(gp.msg_focus && gp.focused);
        let f = gp.mo.msg_ring.v();
        let idle = if self.hv(bx) { c.border_glow } else { c.border };
        cv.ring(bx, ds::R_ROW + 2, crate::anim::lerp_rgba(idle, c.focus_ring, f));
        if f > 0.01 {
            cv.ring(bx.inset(-2), ds::R_ROW + 4, crate::anim::fade(c.focus_ring, 0.3 * f));
        }
        let b = Rect::new(bx.x, bx.y, bx.w, msg_h);
        gp.msg_box = b;
        self.gp_draw_msg(cv, gp, b);

        let ry = b.bottom() + 4;
        let sp = Rect::new(bx.x + 6, ry, 26, 26);
        self.gp_icon_btn(cv, gp, sp, "ui/sparkle", Hit::Generate);

        // Commit button with its chevron, one block.
        let staged = gp.staged();
        let ready =
            !gp.msg.text().trim().is_empty() && (gp.amend || staged > 0 || (gp.include_all && !gp.changes.is_empty()));
        let label = if gp.amend { "Amend" } else { "Commit" };
        let lw = self.tx.ui_width(label, true) + 22;
        let cw = 22;
        let blk = Rect::new(bx.right() - 6 - lw - cw, ry, lw + cw, 26);
        let main = Rect::new(blk.x, blk.y, lw, 26);
        let chev = Rect::new(main.right(), blk.y, cw, 26);
        cv.fill_round(blk, ds::R_ROW, if ready { c.accent } else { c.accent.with_alpha(0x48) });
        let over = c.on_accent.with_alpha(0x2a);
        let held = c.on_accent.with_alpha(0x44);
        if self.hv(main) {
            let a = if gp.pressed == Some(Hit::CommitBtn) { held } else { over };
            cv.fill_round_clip(blk, ds::R_ROW, a, main);
        }
        if self.hv(chev) || gp.menu == Some(GpMenu::Commit) {
            let a = if gp.pressed == Some(Hit::Menu(GpMenu::Commit)) || gp.menu == Some(GpMenu::Commit) {
                held
            } else {
                over
            };
            cv.fill_round_clip(blk, ds::R_ROW, a, chev);
        }
        let text_col = if ready { c.on_accent } else { strong.with_alpha(0xc0) };
        cv.fill(Rect::new(main.right(), blk.y + 6, 1, 14), text_col.with_alpha(0x50));
        self.tx.draw_ui(cv, main.x + 11, main.y + 4, label, text_col, main, true);
        self.tx.icon(cv, "ui/chevron_down", chev.x + 4, blk.y + 6, 14, text_col, chev);
        gp.hits.push((main, Hit::CommitBtn));
        gp.hits.push((chev, Hit::Menu(GpMenu::Commit)));

        // Between them: the notice of the last action, otherwise what is in scope.
        let x0 = sp.right() + 8;
        let mut room = blk.x - 8 - x0;
        // Subject length guidance: silent until the line gets long, then "N/72" in amber / red.
        let sl = super::motion::subject_len(gp.msg.text());
        let tone = match super::motion::subject_tone(sl) {
            super::motion::SubjectTone::Ok => None,
            super::motion::SubjectTone::Long => Some(c.warning),
            super::motion::SubjectTone::Over => Some(c.error),
        };
        if let Some(col) = tone {
            let t = format!("{sl}/{}", super::motion::SUBJECT_HARD);
            let w = self.tx.ui_width_sized(&t, false, 11.0);
            self.tx.draw_ui_sized(cv, x0 + room - w, ry + 6, &t, col, bx, false, 11.0);
            room -= w + 8;
        }
        if let Some((m, ok, t)) = gp.notice.clone()
            && t.elapsed().as_secs() < 12
        {
            let txt = fit(&mut self.tx, &m, room, false);
            self.tx.draw_ui(cv, x0, ry + 4, &txt, if ok { c.git_added } else { c.error }, bx, false);
        } else {
            let (scope, add, del) = gp.composer_scope();
            let txt = fit(&mut self.tx, &scope, room, false);
            self.tx.draw_ui(cv, x0, ry + 4, &txt, dim, bx, false);
            let mut sx = x0 + self.tx.ui_width(&txt, false) + 8;
            for (text, col) in [(format!("+{add}"), c.git_added), (format!("-{del}"), c.error)] {
                let w = self.tx.ui_width_sized(&text, false, 12.0);
                if (add > 0 || del > 0) && sx + w <= x0 + room {
                    self.tx.draw_ui_sized(cv, sx, ry + 6, &text, col.with_alpha(0xcc), bx, false, 12.0);
                    sx += w + 6;
                }
            }
        }
    }

    /// The open dropdown, over everything else; it hangs under its button (above it when it does
    /// not fit below, like the Commit options).
    pub(super) fn gp_draw_menu(&mut self, cv: &mut Canvas, gp: &mut GitPanel, p: Rect) {
        let Some(m) = gp.menu else { return };
        let anchor = gp.hits.iter().find(|(_, h)| *h == Hit::Menu(m)).map(|(r, _)| *r);
        let items = gp.menu_items(m);
        let Some(anchor) = anchor.filter(|_| !items.is_empty()) else {
            gp.menu = None;
            return;
        };
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        const ROW: i32 = 28;
        const PAD: i32 = 4;
        let mut w = 0;
        for it in &items {
            let mut iw = 10 + 16 + 8 + self.tx.ui_width(&it.label, false) + 12;
            if !it.trailing.is_empty() {
                iw += 12 + self.tx.ui_width(&it.trailing, false);
            }
            w = w.max(iw);
        }
        let w = w.clamp(150, (p.w - 16).max(150));
        let h = items.len() as i32 * ROW + 2 * PAD;
        let x = anchor.right() - w;
        let x = x.clamp(p.x + 8, (p.right() - 8 - w).max(p.x + 8));
        let below = anchor.bottom() + 4;
        let y = if below + h <= p.bottom() - 4 { below } else { (anchor.y - 4 - h).max(p.y + 4) };
        let r = Rect::new(x, y, w, h);
        cv.float_card(r, ds::R_ROW + 2, &c, ds::Elev::Pop);
        // Clicks on the padding are swallowed (they only close the menu).
        gp.hits.push((r, Hit::MenuItem(usize::MAX)));
        for (i, it) in items.iter().enumerate() {
            let ir = Rect::new(r.x + 4, r.y + PAD + i as i32 * ROW, r.w - 8, ROW);
            let hov = self.hv(ir);
            if hov {
                cv.fill_round(ir, ds::R_CTRL, c.list_hover_bg);
                if gp.pressed == Some(Hit::MenuItem(i)) {
                    cv.fill_round(ir, ds::R_CTRL, c.pressed_overlay);
                }
            }
            match it.checked {
                Some(on) => {
                    let st = if on { CheckState::On } else { CheckState::Off };
                    self.gp_checkbox(cv, Rect::new(ir.x + 6, ir.y + 6, 16, 16), st, hov);
                }
                None => self.tx.icon(cv, it.icon, ir.x + 6, ir.y + 6, 16, if hov { strong } else { dim }, ir),
            }
            self.tx.draw_ui(cv, ir.x + 30, ir.y + 5, &it.label, strong, ir, false);
            if !it.trailing.is_empty() {
                let tw = self.tx.ui_width(&it.trailing, false);
                self.tx.draw_ui(cv, ir.right() - 8 - tw, ir.y + 5, &it.trailing, dim, ir, false);
            }
            gp.hits.push((ir, Hit::MenuItem(i)));
        }
    }
}
