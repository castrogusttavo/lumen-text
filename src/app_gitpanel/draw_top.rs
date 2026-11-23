//! The top of the panel, after Athas' Source Control: the header with the Changes / History tabs,
//! the branch row with the fetch / pull / push buttons, the commit composer, and the dropdowns they open.

use super::*;

impl App {
    /// Two rows: the tabs (Changes, History, Checks as icons) with the close button, then the
    /// actions: Create PR (or the pull request's number), search and the "..." menu.
    pub(super) fn gp_draw_header(&mut self, cv: &mut Canvas, gp: &mut GitPanel, p: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let y = p.y + (40 - 28) / 2;
        self.gp_icon_btn(cv, gp, Rect::new(p.right() - 8 - 28, y, 28, 28), "ui/close", Hit::Close);
        // Icon tabs, spread across the row like the rail of an app: the active one tinted.
        let tabs = [
            (Tab::Changes, "ui/git_branch", gp.changes.len()),
            (Tab::History, "ui/clock", 0),
            (Tab::Checks, "ui/list_todo", 0),
        ];
        let avail = p.w - 16 - 36;
        let slot = (avail / tabs.len() as i32).clamp(44, 72);
        let mut x = p.x + 8;
        for (tab, icon, n) in tabs {
            let r = Rect::new(x, y, slot - 4, 28);
            let active = gp.tab == tab;
            if active {
                cv.fill_round(r, 8, strong.with_alpha(0x1e));
            } else if self.hv(r) {
                cv.fill_round(r, 8, strong.with_alpha(0x12));
            }
            let col = if active || self.hv(r) { strong } else { dim };
            self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + 6, 16, col, r);
            if n > 0 && tab == Tab::Changes {
                // A small dot says there is something to commit.
                cv.fill_round(Rect::new(r.x + r.w / 2 + 6, r.y + 5, 7, 7), 3, c.accent);
            }
            gp.hits.push((r, Hit::Tab(tab)));
            x += slot;
        }
        cv.fill(Rect::new(p.x + 1, p.y + 40, p.w - 1, 1), c.border.with_alpha(0x70));

        // Second row: the pull request pill, search, more.
        let y2 = p.y + 40 + (40 - 28) / 2;
        let mut right = p.right() - 8;
        self.gp_icon_btn(cv, gp, Rect::new(right - 28, y2, 28, 28), "ui/ellipsis", Hit::Menu(GpMenu::Actions));
        right -= 30;
        self.gp_icon_btn(cv, gp, Rect::new(right - 28, y2, 28, 28), "hugeicons/search", Hit::Search);
        let existing = self.gitbg.pr.snap.as_ref().and_then(|s| s.info.as_ref()).map(|i| i.number);
        let (label, hit) = match existing {
            Some(n) => (format!("PR #{n}"), Hit::Tab(Tab::Checks)),
            None => ("Create PR".to_string(), Hit::PrOpen),
        };
        let lw = self.tx.ui_width(&label, true);
        let pill = Rect::new(p.x + 10, y2, 8 + 16 + 6 + lw + 14, 28);
        let hot = self.hv(pill);
        cv.fill_round(pill, 14, strong.with_alpha(if hot { 0x30 } else { 0x20 }));
        self.tx.icon(cv, "ui/git_branch", pill.x + 9, pill.y + 6, 16, strong, pill);
        self.tx.draw_ui(cv, pill.x + 8 + 16 + 6, pill.y + 4, &label, strong, pill, true);
        gp.hits.push((pill, hit));
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
        if self.hv(chev) || gp.menu == Some(GpMenu::Sync) {
            cv.fill_round(chev, 8, strong.with_alpha(0x1c));
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
            let col = if enabled { tone } else { dim.with_alpha(0x70) };
            if enabled && self.hv(r) {
                cv.fill_round(r, 8, strong.with_alpha(0x1c));
            }
            self.tx.icon(cv, icon, r.x + 8, y + 7, 14, col, r);
            if !cnt.is_empty() {
                self.tx.draw_ui(cv, r.x + 8 + 14 + 3, y + 5, &cnt, col, r, true);
            }
            if enabled {
                gp.hits.push((r, hit));
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

        // Under the chip: where the branch goes (its upstream, else the base of a pull request).
        let target = gp.sync.upstream.clone().or_else(|| gp.branch.as_ref().map(|b| b.base.clone()));
        if let Some(t) = target {
            let line = format!("→ {t}");
            let txt = fit(&mut self.tx, &line, (p.w - 28).max(0), false);
            self.tx.draw_ui_sized(cv, p.x + 12, by + 33, &txt, dim.with_alpha(0xa0), p, false, 12.0);
        }

        // What the working tree changes in all: +added -removed, when there is room beside the chip.
        let (mut add, mut del) = (0u32, 0u32);
        if let Some(st) = gp.stats.as_ref() {
            for ch in &gp.changes {
                if let Some(s) = st.of(ch) {
                    add += s.add.unwrap_or(0);
                    del += s.del.unwrap_or(0);
                }
            }
        }
        let mut sx = chip.right() + 4;
        for (text, col) in [(format!("+{add}"), c.git_added), (format!("-{del}"), c.error)] {
            let w = self.tx.ui_width_sized(&text, false, 12.0);
            if (add > 0 || del > 0) && sx + w <= main.x - 4 {
                self.tx.draw_ui_sized(cv, sx, y + 7, &text, col.with_alpha(0xcc), p, false, 12.0);
                sx += w + 6;
            }
        }
    }

    /// The commit composer: a rounded box with the message and the sparkle (write one) in its corner,
    /// under it the Commit button across the width (its options behind the chevron), and a small
    /// caption with what the commit takes (or the result of the last action).
    pub(super) fn gp_draw_commit_area(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let msg_h = gp.msg_h();
        let bx = Rect::new(area.x + COMPOSER_MARGIN, area.y + 6, area.w - 2 * COMPOSER_MARGIN, msg_h);
        cv.fill_round(bx, 12, c.input_bg);
        if gp.msg_focus {
            cv.ring(bx, 12, c.accent.with_alpha(0xa0));
        } else if self.hv(bx) {
            cv.ring(bx, 12, strong.with_alpha(0x30));
        } else {
            cv.ring(bx, 12, c.border.with_alpha(0xa0));
        }
        gp.msg_box = bx;
        self.gp_draw_msg(cv, gp, bx);
        let sp = Rect::new(bx.right() - 6 - 26, bx.y + 6, 26, 26);
        self.gp_icon_btn(cv, gp, sp, "ui/sparkle", Hit::Generate);

        // Commit across the width, with its options behind the chevron.
        let staged = gp.staged();
        let ready =
            !gp.msg.text().trim().is_empty() && (gp.amend || staged > 0 || (gp.include_all && !gp.changes.is_empty()));
        let label = if gp.amend { "Amend" } else { "Commit" };
        let ry = bx.bottom() + 8;
        let blk = Rect::new(bx.x, ry, bx.w, 32);
        let cw = 32;
        let main = Rect::new(blk.x, blk.y, blk.w - cw, 32);
        let chev = Rect::new(main.right(), blk.y, cw, 32);
        cv.fill_round(blk, 10, if ready { c.accent } else { c.accent.with_alpha(0x48) });
        let over = c.on_accent.with_alpha(0x2a);
        if self.hv(main) {
            cv.fill_round_clip(blk, 10, over, main);
        }
        if self.hv(chev) || gp.menu == Some(GpMenu::Commit) {
            cv.fill_round_clip(blk, 10, over, chev);
        }
        let text_col = if ready { c.on_accent } else { strong.with_alpha(0xc0) };
        cv.fill(Rect::new(main.right(), blk.y + 8, 1, 16), text_col.with_alpha(0x50));
        let lw = self.tx.ui_width(label, true);
        let content_w = 16 + 8 + lw;
        let cx = main.x + (main.w - content_w) / 2;
        self.tx.icon(cv, "ui/check", cx, blk.y + 8, 16, text_col, main);
        self.tx.draw_ui(cv, cx + 24, blk.y + 7, label, text_col, main, true);
        self.tx.icon(cv, "ui/chevron_down", chev.x + 8, blk.y + 8, 16, text_col, chev);
        gp.hits.push((main, Hit::CommitBtn));
        gp.hits.push((chev, Hit::Menu(GpMenu::Commit)));

        // Caption under the button: the notice of the last action, otherwise what is in scope.
        let cy = blk.bottom() + 4;
        let room = bx.w - 4;
        if let Some((m, ok, t)) = gp.notice.clone()
            && t.elapsed().as_secs() < 12
        {
            let txt = fit(&mut self.tx, &m, room, false);
            self.tx.draw_ui_sized(cv, bx.x + 2, cy, &txt, if ok { c.git_added } else { c.error }, area, false, 12.0);
        } else {
            let (scope, add, del) = gp.composer_scope();
            let txt = fit(&mut self.tx, &scope, room, false);
            self.tx.draw_ui_sized(cv, bx.x + 2, cy, &txt, dim, area, false, 12.0);
            let mut sx = bx.x + 2 + self.tx.ui_width_sized(&txt, false, 12.0) + 8;
            for (text, col) in [(format!("+{add}"), c.git_added), (format!("-{del}"), c.error)] {
                let w = self.tx.ui_width_sized(&text, false, 12.0);
                if (add > 0 || del > 0) && sx + w <= bx.x + room {
                    self.tx.draw_ui_sized(cv, sx, cy, &text, col.with_alpha(0xcc), area, false, 12.0);
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
