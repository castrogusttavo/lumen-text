//! The top of the panel, after Athas' Source Control: the header with the section switcher, the
//! branch row with the sync button, the commit composer, and the dropdowns they open.

use super::*;

impl App {
    /// "Source Control" and the section switcher on the left, refresh / actions / close on the right.
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

        let (label, count) = match gp.tab {
            Tab::Changes => ("Changes", gp.changes.len()),
            Tab::History => ("History", 0),
        };
        let badge = if count > 0 { count.to_string() } else { String::new() };
        let lw = self.tx.ui_width(label, true);
        let bw = if badge.is_empty() { 0 } else { (self.tx.ui_width(&badge, true) + 12).max(20) + 6 };
        let sw = 10 + lw + bw + 4 + 14 + 6;
        let title = "Source Control";
        let tw = self.tx.ui_width(title, false);
        let show_title = tw + 14 + sw <= right - (p.x + 12);
        let mut x = p.x + 6;
        if show_title {
            self.tx.draw_ui(cv, p.x + 12, y + 5, title, dim, p, false);
            x = p.x + 12 + tw + 6;
        }
        let sr = Rect::new(x, y, sw.min((right - x).max(0)), 28);
        let open = gp.menu == Some(GpMenu::Sections);
        if open || self.hv(sr) {
            cv.fill_round(sr, 8, strong.with_alpha(if open { 0x1e } else { 0x14 }));
        }
        self.tx.draw_ui(cv, sr.x + 10, sr.y + 5, label, strong, sr, true);
        if !badge.is_empty() {
            let pill = Rect::new(sr.x + 10 + lw + 6, sr.y + 6, bw - 6, 16);
            cv.fill_round(pill, 8, strong.with_alpha(0x22));
            let tw = self.tx.ui_width(&badge, true);
            self.tx.draw_ui_sized(cv, pill.x + (pill.w - tw) / 2, pill.y + 1, &badge, dim, pill, true, 12.0);
        }
        self.tx.icon(cv, "ui/chevron_down", sr.x + sw - 6 - 14, sr.y + 7, 14, dim, sr);
        gp.hits.push((sr, Hit::Menu(GpMenu::Sections)));
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
        let plan = sync_plan(ahead, behind, has_up);
        let tone = if behind > 0 {
            c.warning
        } else if ahead > 0 || !has_up {
            c.accent_text
        } else {
            dim
        };
        let tone = if busy { dim } else { tone };
        let cnt = if plan.count > 0 { plan.count.to_string() } else { String::new() };
        let cw = if cnt.is_empty() { 0 } else { self.tx.ui_width(&cnt, true) + 3 };
        let main_w = 8 + 14 + cw + 6;
        let chev_w = 20;
        let main = Rect::new(p.right() - 8 - main_w - chev_w, y, main_w, 28);
        let chev = Rect::new(main.right(), y, chev_w, 28);
        if !busy && self.hv(main) {
            cv.fill_round(main, 8, strong.with_alpha(0x1c));
        }
        if self.hv(chev) || gp.menu == Some(GpMenu::Sync) {
            cv.fill_round(chev, 8, strong.with_alpha(0x1c));
        }
        let icon = if busy { "ui/rotate_cw" } else { plan.icon };
        self.tx.icon(cv, icon, main.x + 8, y + 7, 14, tone, main);
        if !cnt.is_empty() {
            self.tx.draw_ui(cv, main.x + 8 + 14 + 3, y + 5, &cnt, tone, main, true);
        }
        if !busy {
            gp.hits.push((main, plan.hit));
        }
        self.tx.icon(cv, "ui/chevron_down", chev.x + 3, y + 7, 14, tone, chev);
        gp.hits.push((chev, Hit::Menu(GpMenu::Sync)));

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
        if self.hv(chip) {
            cv.fill_round(chip, 8, strong.with_alpha(0x14));
        }
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
        if gp.msg_focus {
            cv.ring(bx, 10, c.accent.with_alpha(0xa0));
        } else if self.hv(bx) {
            cv.ring(bx, 10, strong.with_alpha(0x30));
        } else {
            cv.ring(bx, 10, c.border.with_alpha(0xa0));
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
        cv.fill_round(blk, 8, if ready { c.accent } else { c.accent.with_alpha(0x48) });
        let over = c.on_accent.with_alpha(0x2a);
        if self.hv(main) {
            cv.fill_round_clip(blk, 8, over, main);
        }
        if self.hv(chev) || gp.menu == Some(GpMenu::Commit) {
            cv.fill_round_clip(blk, 8, over, chev);
        }
        let text_col = if ready { c.on_accent } else { strong.with_alpha(0xc0) };
        cv.fill(Rect::new(main.right(), blk.y + 6, 1, 14), text_col.with_alpha(0x50));
        self.tx.draw_ui(cv, main.x + 11, main.y + 4, label, text_col, main, true);
        self.tx.icon(cv, "ui/chevron_down", chev.x + 4, blk.y + 6, 14, text_col, chev);
        gp.hits.push((main, Hit::CommitBtn));
        gp.hits.push((chev, Hit::Menu(GpMenu::Commit)));

        // Between them: the notice of the last action, otherwise what is in scope.
        let x0 = sp.right() + 8;
        let room = blk.x - 8 - x0;
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
        let x = if m == GpMenu::Sections { anchor.x } else { anchor.right() - w };
        let x = x.clamp(p.x + 8, (p.right() - 8 - w).max(p.x + 8));
        let below = anchor.bottom() + 4;
        let y = if below + h <= p.bottom() - 4 { below } else { (anchor.y - 4 - h).max(p.y + 4) };
        let r = Rect::new(x, y, w, h);
        cv.shadow(r, 10);
        cv.fill_round(r, 10, c.widget_bg);
        cv.ring(r, 10, c.border);
        // Clicks on the padding are swallowed (they only close the menu).
        gp.hits.push((r, Hit::MenuItem(usize::MAX)));
        for (i, it) in items.iter().enumerate() {
            let ir = Rect::new(r.x + 4, r.y + PAD + i as i32 * ROW, r.w - 8, ROW);
            let hov = self.hv(ir);
            if hov {
                cv.fill_round(ir, 6, c.list_hover_bg);
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
