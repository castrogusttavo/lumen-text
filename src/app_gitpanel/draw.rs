use super::*;

impl App {
    // ───────────── drawing ─────────────

    pub(super) fn hv(&self, r: Rect) -> bool {
        self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1)
    }

    pub(super) fn gp_icon_btn(&mut self, cv: &mut Canvas, gp: &mut GitPanel, r: Rect, icon: &str, hit: Hit) {
        let c = self.theme.chrome;
        if self.hv(r) {
            cv.fill_round(r, 8, c.tab_active_fg.with_alpha(0x1c));
        }
        let tint = if self.hv(r) { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + (r.h - 16) / 2, 16, tint, r);
        gp.hits.push((r, hit));
    }

    /// Small text button (pill) with hover; returns its width.
    pub(super) fn gp_text_btn(
        &mut self,
        cv: &mut Canvas,
        gp: &mut GitPanel,
        right: i32,
        y: i32,
        label: &str,
        hit: Hit,
        tint: Rgba,
    ) -> i32 {
        let w = self.tx.ui_width(label, false) + 18;
        let r = Rect::new(right - w, y, w, 24);
        let hov = self.hv(r);
        if hov {
            cv.fill_round(r, 8, tint.with_alpha(0x26));
        }
        self.tx.draw_ui(cv, r.x + 9, r.y + 3, label, tint, r, false);
        gp.hits.push((r, hit));
        w
    }

    pub fn draw_gitpanel(&mut self, cv: &mut Canvas) {
        let Some(mut gp) = self.gitpanel.take() else { return };
        self.gp_sync(&mut gp);
        gp.hits.clear();
        let p = self.gitpanel_rect();
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(p, c.sidebar_bg);
        cv.fill(Rect::new(p.x, p.y, 1, p.h), c.border);
        // Resize handle: the left edge lights up under the pointer and while dragging.
        let (mx, my) = (self.mouse.0, self.mouse.1);
        if gp.resizing || (self.overlay.is_none() && Rect::new(p.x - GP_GRAB, p.y, 2 * GP_GRAB, p.h).contains(mx, my)) {
            cv.fill(Rect::new(p.x - 1, p.y, 3, p.h), c.accent.with_alpha(if gp.resizing { 0xc0 } else { 0x70 }));
        }

        // Header: tabs on the left, refresh / close on the right.
        let mut x = p.x + 12;
        for (tab, label) in [(Tab::Changes, "Changes"), (Tab::History, "History")] {
            let n = gp.changes.len();
            let text = if tab == Tab::Changes && n > 0 { format!("{label}  {n}") } else { label.to_string() };
            let active = gp.tab == tab;
            let w = self.tx.ui_width(&text, active) + 24;
            let r = Rect::new(x, p.y + 8, w, 28);
            if active {
                cv.fill_round(r, 8, strong.with_alpha(0x1e));
            } else if self.hv(r) {
                cv.fill_round(r, 8, strong.with_alpha(0x12));
            }
            let col = if active || self.hv(r) { strong } else { dim };
            self.tx.draw_ui(cv, r.x + 12, r.y + 5, &text, col, r, active);
            gp.hits.push((r, Hit::Tab(tab)));
            x += w + 4;
        }
        self.gp_icon_btn(cv, &mut gp, Rect::new(p.right() - 12 - 28, p.y + 8, 28, 28), "ui/close", Hit::Close);
        self.gp_icon_btn(
            cv,
            &mut gp,
            Rect::new(p.right() - 12 - 28 - 32, p.y + 8, 28, 28),
            "ui/rotate_cw",
            Hit::Refresh,
        );

        // Branch line: name on the left, fetch / pull / push on the right.
        let by = p.y + HEAD_H;
        let mut right = p.right() - 10;
        if gp.top.is_some() {
            let busy = gp.job.is_some();
            let (ahead, behind, has_up) = (gp.sync.ahead, gp.sync.behind, gp.sync.upstream.is_some());
            let count = |n: usize| if n > 0 { format!(" {n}") } else { String::new() };
            let push = format!("Push{}", if has_up { count(ahead) } else { String::new() });
            right -= self.gp_sync_btn(
                cv,
                &mut gp,
                right,
                by + 5,
                "ui/arrow_up",
                &push,
                Hit::Push,
                !busy && (ahead > 0 || !has_up),
            ) + 4;
            let pull = format!("Pull{}", count(behind));
            right -=
                self.gp_sync_btn(cv, &mut gp, right, by + 5, "ui/arrow_down", &pull, Hit::Pull, !busy && behind > 0)
                    + 4;
            right -= self.gp_sync_btn(cv, &mut gp, right, by + 5, "ui/cloud_download", "", Hit::Fetch, !busy) + 4;
        }
        self.tx.icon(cv, "ui/git_branch", p.x + 16, by + 10, 14, dim, p);
        let branch = self.branch.clone().unwrap_or_else(|| "no branch".into());
        let branch = fit(&mut self.tx, &branch, right - (p.x + 36) - 4, false);
        self.tx.draw_ui(cv, p.x + 36, by + 8, &branch, dim, p, false);

        // Strip: set-upstream prompt, progress of the running command, or the last result.
        self.gp_measure_prompt(&mut gp, p.w);
        let strip_h = gp.strip_h();
        if strip_h > 0 && gp.top.is_some() {
            self.gp_draw_strip(cv, &mut gp, Rect::new(p.x + 1, by + BRANCH_H, p.w - 1, strip_h));
        }

        let commit_h = if gp.tab == Tab::Changes && gp.top.is_some() {
            self.gp_msg_layout(&mut gp, p, strip_h);
            gp.commit_area_h()
        } else {
            0
        };
        let body =
            Rect::new(p.x + 1, by + BRANCH_H + strip_h, p.w - 1, (p.h - HEAD_H - BRANCH_H - strip_h - commit_h).max(0));
        if gp.top.is_none() {
            let msg = "This folder is not a git repository";
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, p.x + (p.w - w) / 2, body.y + 40, msg, dim, body, false);
        } else if gp.tab == Tab::Changes {
            self.gp_draw_changes(cv, &mut gp, body);
            self.gp_draw_commit_area(cv, &mut gp, Rect::new(p.x + 1, body.bottom(), p.w - 1, commit_h));
        } else {
            self.gp_draw_history(cv, &mut gp, body);
        }
        gp.hover = if p.contains(self.mouse.0, self.mouse.1) { gp.hit_at(self.mouse.0, self.mouse.1) } else { None };
        self.gitpanel = Some(gp);
    }

    /// Pill with an icon and an optional label (fetch / pull / push); returns its width.
    pub(super) fn gp_sync_btn(
        &mut self,
        cv: &mut Canvas,
        gp: &mut GitPanel,
        right: i32,
        y: i32,
        icon: &str,
        label: &str,
        hit: Hit,
        active: bool,
    ) -> i32 {
        let c = self.theme.chrome;
        let lw = if label.is_empty() { 0 } else { self.tx.ui_width(label, false) + 5 };
        let w = 12 + 14 + lw + if label.is_empty() { 0 } else { 6 };
        let r = Rect::new(right - w, y, w, 26);
        let hov = self.hv(r);
        if hov {
            cv.fill_round(r, 8, c.tab_active_fg.with_alpha(0x1c));
        }
        let tint = if active {
            c.accent_text
        } else if hov {
            c.tab_active_fg
        } else {
            c.tab_inactive_fg
        };
        self.tx.icon(cv, icon, r.x + 6, r.y + 6, 14, tint, r);
        if !label.is_empty() {
            self.tx.draw_ui(cv, r.x + 6 + 14 + 5, r.y + 4, label, tint, r, false);
        }
        gp.hits.push((r, hit));
        w
    }

    /// Buttons of the force-push / strategy cards: (label, hit, colored as a warning).
    pub(super) fn gp_card_buttons(gp: &GitPanel) -> Vec<(&'static str, Hit, bool)> {
        GitPanel::gp_card_buttons_for(gp.force.is_some())
    }

    /// Height of the force-push / strategy card for a panel `w` px wide (text wrapped, buttons in rows).
    pub(super) fn gp_measure_prompt(&mut self, gp: &mut GitPanel, w: i32) {
        if gp.force.is_none() && !gp.strategy_ask {
            if let Some(a) = gp.card() {
                let lines = wrap_lines(&mut self.tx, &a.text, w - 32, 6).len() as i32;
                let widths: Vec<i32> = a.buttons.iter().map(|b| self.tx.ui_width(&b.label, false) + 18).collect();
                let rows = button_rows(&widths, w - 20, 4).len() as i32;
                gp.prompt_h = 8 + lines * STRIP_LINE + 6 + rows * 28 + 4;
            }
            return;
        }
        let text = gp.force.map_or(STRATEGY_TEXT, ForceWhy::text);
        let lines = wrap_lines(&mut self.tx, text, w - 32, 5).len() as i32;
        let widths: Vec<i32> =
            Self::gp_card_buttons(gp).iter().map(|(l, _, _)| self.tx.ui_width(l, false) + 18).collect();
        let rows = button_rows(&widths, w - 20, 4).len() as i32;
        gp.prompt_h = 8 + lines * STRIP_LINE + 6 + rows * 28 + 4;
    }

    /// Draw the force-push / strategy card; true when one was drawn.
    pub(super) fn gp_draw_card(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) -> bool {
        if gp.force.is_none() && !gp.strategy_ask {
            return self.gp_draw_ask(cv, gp, area);
        }
        let c = self.theme.chrome;
        let strong = c.tab_active_fg;
        let warn = gp.force.is_some();
        if warn {
            cv.fill(Rect::new(area.x, area.y, 3, area.h), c.error.with_alpha(0xc0));
        }
        let text = gp.force.map_or(STRATEGY_TEXT, ForceWhy::text);
        let lines = wrap_lines(&mut self.tx, text, area.w - 32, 5);
        for (i, l) in lines.iter().enumerate() {
            self.tx.draw_ui(cv, area.x + 16, area.y + 6 + i as i32 * STRIP_LINE, l, strong, area, false);
        }
        let btns = Self::gp_card_buttons(gp);
        let widths: Vec<i32> = btns.iter().map(|(l, _, _)| self.tx.ui_width(l, false) + 18).collect();
        let rows = button_rows(&widths, area.w - 20, 4);
        let mut k = 0;
        let first_y = area.y + 6 + lines.len() as i32 * STRIP_LINE + 6;
        for (ri, n) in rows.iter().enumerate() {
            let y = first_y + ri as i32 * 28;
            let mut right = area.right() - 10;
            for &(label, hit, danger) in btns[k..k + n].iter().rev() {
                let tint = if danger {
                    c.error
                } else if matches!(hit, Hit::ForceNo | Hit::StrategyNo) {
                    c.tab_inactive_fg
                } else {
                    c.accent_text
                };
                right -= self.gp_text_btn(cv, gp, right, y, label, hit, tint) + 4;
            }
            k += n;
        }
        true
    }

    /// Draw the confirmation card (`GitPanel::card`); true when there is one.
    pub(super) fn gp_draw_ask(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) -> bool {
        let Some(a) = gp.card() else { return false };
        let c = self.theme.chrome;
        if a.warn {
            cv.fill(Rect::new(area.x, area.y, 3, area.h), c.error.with_alpha(0xc0));
        }
        let lines = wrap_lines(&mut self.tx, &a.text, area.w - 32, 6);
        for (i, l) in lines.iter().enumerate() {
            self.tx.draw_ui(cv, area.x + 16, area.y + 6 + i as i32 * STRIP_LINE, l, c.tab_active_fg, area, false);
        }
        let widths: Vec<i32> = a.buttons.iter().map(|b| self.tx.ui_width(&b.label, false) + 18).collect();
        let rows = button_rows(&widths, area.w - 20, 4);
        let first_y = area.y + 6 + lines.len() as i32 * STRIP_LINE + 6;
        let mut k = 0;
        for (ri, n) in rows.iter().enumerate() {
            let y = first_y + ri as i32 * 28;
            let mut right = area.right() - 10;
            for i in (k..k + n).rev() {
                let b = &a.buttons[i];
                let tint = match b.tone {
                    Tone::Accent => c.accent_text,
                    Tone::Danger => c.error,
                    Tone::Quiet => c.tab_inactive_fg,
                };
                right -= self.gp_text_btn(cv, gp, right, y, &b.label, Hit::Ask(i), tint) + 4;
            }
            k += n;
        }
        true
    }

    pub(super) fn gp_draw_strip(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(Rect::new(area.x, area.y, area.w, area.h), c.input_bg.with_alpha(0x80).over(c.sidebar_bg));
        if gp.publish.is_none() && self.gp_draw_card(cv, gp, area) {
            return;
        }
        if let Some((remote, branch)) = gp.publish.clone() {
            let q = format!("Publish “{branch}” to {remote} and start tracking it (upstream)?");
            let lines = wrap_lines(&mut self.tx, &q, area.w - 32, 2);
            for (i, l) in lines.iter().enumerate() {
                self.tx.draw_ui(cv, area.x + 16, area.y + 6 + i as i32 * STRIP_LINE, l, strong, area, false);
            }
            let y = area.bottom() - 30;
            let w = self.gp_text_btn(cv, gp, area.right() - 10, y, "Publish", Hit::PublishYes, c.accent_text);
            self.gp_text_btn(cv, gp, area.right() - 10 - w - 4, y, "Cancel", Hit::PublishNo, dim);
            return;
        }
        if let Some(job) = &gp.job {
            let secs = job.started.elapsed().as_secs();
            let head = format!("{}… {secs}s", job.op.busy());
            let hw = self.tx.ui_width(&head, true);
            self.tx.draw_ui(cv, area.x + 16, area.y + 5, &head, strong, area, true);
            let cancel_w = self.tx.ui_width("Cancel", false) + 18;
            let room = area.w - 32 - hw - cancel_w - 16;
            let prog = fit(&mut self.tx, &job.progress.clone(), room, false);
            self.tx.draw_ui(cv, area.x + 16 + hw + 10, area.y + 5, &prog, dim, area, false);
            self.gp_text_btn(cv, gp, area.right() - 8, area.y + 2, "Cancel", Hit::JobCancel, c.error);
            return;
        }
        if let Some((text, ok, _)) = gp.sync_note.clone() {
            let lines = wrap_lines(&mut self.tx, &text, area.w - 32, 3);
            gp.note_lines = lines.len() as i32;
            let col = if ok { c.git_added } else { c.error };
            for (i, l) in lines.iter().enumerate() {
                self.tx.draw_ui(cv, area.x + 16, area.y + 5 + i as i32 * STRIP_LINE, l, col, area, false);
            }
            gp.hits.push((area, Hit::Note));
        }
    }

    pub(super) fn gp_checkbox(&mut self, cv: &mut Canvas, r: Rect, state: CheckState, hov: bool) {
        let c = self.theme.chrome;
        if state == CheckState::On {
            cv.fill_round(r, 5, if hov { c.accent.shift(22) } else { c.accent });
            self.tx.icon(cv, "ui/check", r.x + 2, r.y + 2, r.w - 4, c.on_accent, r);
        } else {
            if hov {
                cv.fill_round(r, 5, c.tab_active_fg.with_alpha(0x1c));
            }
            cv.ring(r, 5, c.tab_inactive_fg.with_alpha(if hov { 0xb0 } else { 0x70 }));
        }
    }

    pub(super) fn gp_draw_changes(&mut self, cv: &mut Canvas, gp: &mut GitPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let rows = gp.rows();
        if rows.is_empty() {
            let msg = "No changes";
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, body.x + (body.w - w) / 2, body.y + 40, msg, dim, body, false);
            return;
        }
        let total: i32 = rows.iter().map(|r| GitPanel::row_h(*r)).sum::<i32>() + 16;
        gp.max_scroll[0] = (total - body.h).max(0) as f64;
        gp.scroll[0] = gp.scroll[0].clamp(0.0, gp.max_scroll[0]);
        let mut y = body.y - gp.scroll[0] as i32;
        for (k, row) in rows.iter().enumerate() {
            let h = GitPanel::row_h(*row);
            if y + h < body.y || y > body.bottom() {
                y += h;
                continue;
            }
            let clip = body;
            let vis = Rect::new(body.x, y.max(body.y), body.w, (y + h).min(body.bottom()) - y.max(body.y));
            match *row {
                Row::Header(g) => {
                    let n = gp.changes.iter().filter(|c| c.group == g).count();
                    self.tx.draw_ui(cv, body.x + 16, y + 9, &format!("{}  {n}", g.title()), dim, clip, true);
                    let label = if g == Group::Staged { "Remove all" } else { "Add all" };
                    let before = gp.hits.len();
                    self.gp_text_btn(cv, gp, body.right() - 10, y + 5, label, Hit::Section(g), c.accent_text);
                    if let Some(last) = gp.hits.get_mut(before) {
                        last.0 = last.0.intersect(&vis);
                    }
                }
                Row::File(i) => {
                    let ch = gp.changes[i].clone();
                    let r = Rect::new(body.x + 6, y, body.w - 12, h);
                    let hov = self.hv(r) && self.hv(vis);
                    let cur = gp.focused && !gp.msg_focus && gp.cursor == k;
                    if cur {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x1e), clip);
                    } else if hov {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x12), clip);
                    }
                    let cb = Rect::new(r.x + 8, y + 7, 16, 16);
                    let cbhit = Rect::new(r.x, y, 34, h);
                    self.gp_checkbox(cv, cb, ch.check_state(), self.hv(cbhit) && self.hv(vis));
                    let (name, dir) = split_name(&ch.path);
                    let icon = self.tx.icons.file_icon(name);
                    self.tx.icon(cv, &icon, r.x + 34, y + 7, 16, dim, clip);
                    let reserved = if ch.discardable() { 66 } else { 36 };
                    let avail = r.w - 58 - reserved;
                    let nm = fit(&mut self.tx, name, avail, false);
                    let nw = self.tx.ui_width(&nm, false);
                    let deleted = ch.code == 'D';
                    self.tx.draw_ui(cv, r.x + 58, y + 6, &nm, if deleted { dim } else { strong }, clip, false);
                    if !dir.is_empty() && avail - nw > 40 {
                        let d = fit(&mut self.tx, dir, avail - nw - 10, false);
                        self.tx.draw_ui_sized(cv, r.x + 58 + nw + 8, y + 8, &d, dim, clip, false, 12.0);
                    }
                    let letter = ch.code.to_string();
                    let lw = self.tx.ui_width(&letter, true);
                    let lx = r.right() - 14 - lw / 2 - 4;
                    self.tx.draw_ui(cv, lx, y + 6, &letter, code_color(&c, ch.code), clip, true);
                    gp.hits.push((Rect::new(r.x + 34, vis.y, r.w - 34, vis.h).intersect(&vis), Hit::Row(i)));
                    gp.hits.push((cbhit.intersect(&vis), Hit::Check(i)));
                    if ch.discardable() {
                        if gp.confirm == Some(i) {
                            let label = "Discard?";
                            let w = self.tx.ui_width(label, false) + 16;
                            let br = Rect::new(r.right() - 34 - w, y + 3, w, 24);
                            let bh = self.hv(br);
                            cv.fill_round_clip(br, 8, c.error.with_alpha(if bh { 0x55 } else { 0x33 }), clip);
                            self.tx.draw_ui(cv, br.x + 8, br.y + 3, label, c.error, br.intersect(&vis), false);
                            gp.hits.push((br.intersect(&vis), Hit::ConfirmDiscard(i)));
                        } else {
                            let br = Rect::new(r.right() - 58, y + 3, 24, 24);
                            if hov || cur {
                                let bh = self.hv(br);
                                if bh {
                                    cv.fill_round_clip(br, 8, strong.with_alpha(0x22), clip);
                                }
                                self.tx.icon(cv, "ui/undo", br.x + 4, br.y + 4, 16, if bh { strong } else { dim }, vis);
                            }
                            gp.hits.push((br.intersect(&vis), Hit::Discard(i)));
                        }
                    }
                }
                _ => {}
            }
            y += h;
        }
        let (pos, visible) = (gp.scroll[0], body.h as f64);
        self.region_bar(cv, crate::scrollreg::Reg::GitChanges, body, total as f64, visible, pos, 1.0, false);
    }

    /// Row height of the message box (the UI line, at least `LINE`).
    pub(super) fn gp_msg_line() -> i32 {
        (crate::ui::ui_line().ceil() as i32).max(LINE)
    }

    /// Wrap the message at the box's inner width and size the box: 3 visual lines minimum, up
    /// to 10 (less when the panel is short, so the changes list keeps room), then it scrolls.
    pub(super) fn gp_msg_layout(&mut self, gp: &mut GitPanel, p: Rect, strip_h: i32) {
        let lh = Self::gp_msg_line();
        let inner_w = (p.w - 1) - 24 - 2 * MSG_PAD_X;
        let tx = &mut self.tx;
        gp.msg.layout(inner_w.max(1) as f32, &mut |s: &str| tx.ui_char_xs(s, false, None));
        let room = p.h - HEAD_H - BRANCH_H - strip_h - COMMIT_CHROME_H - MSG_LIST_ROOM - 2 * MSG_PAD_Y;
        let max_lines = (room / lh).clamp(MSG_MIN_LINES, MSG_MAX_LINES);
        let lines = (gp.msg.line_count() as i32).clamp(MSG_MIN_LINES, max_lines);
        gp.msg_box.h = lines * lh + 2 * MSG_PAD_Y;
        gp.msg.set_view(lh as f32, (lines * lh) as f32);
    }

    /// The message box: wrapped lines (never a horizontal scroll), selection, caret, placeholder,
    /// and soft fades at the edges while there is more text above / below.
    fn gp_draw_msg(&mut self, cv: &mut Canvas, gp: &mut GitPanel, b: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill_round(b, 10, c.input_bg);
        if gp.msg_focus {
            cv.ring(b, 10, c.accent.with_alpha(0xa0));
        } else if self.hv(b) {
            cv.ring(b, 10, strong.with_alpha(0x30));
        }
        gp.hits.push((b, Hit::Msg));
        let lh = Self::gp_msg_line();
        let (ox, oy) = (b.x + MSG_PAD_X, b.y + MSG_PAD_Y);
        let inner_w = b.w - 2 * MSG_PAD_X;
        // Scrolled rows slide under the padding, up to 4 px from the border.
        let clip = Rect::new(b.x + 4, b.y + 4, b.w - 8, b.h - 8);
        // Text centred in its row.
        let ty = ((lh as f32 - crate::ui::ui_line()) / 2.0).max(0.0).round() as i32;
        if gp.msg.text().is_empty() {
            let ph = fit(&mut self.tx, "Commit message (Ctrl+Enter to commit)", inner_w, false);
            self.tx.draw_ui(cv, ox, oy + ty, &ph, dim, clip, false);
        }
        let scroll = gp.msg.scroll.round() as i32;
        let row_y = |k: usize| oy + k as i32 * lh - scroll;
        let first = (scroll / lh).max(0) as usize;
        let last = ((scroll + b.h) / lh + 1).max(0) as usize;
        let sel = self.theme.selection;
        for (k, x0, x1) in gp.msg.sel_spans() {
            if (first..=last).contains(&k) {
                let (a, z) = (x0.round() as i32, x1.round() as i32);
                cv.fill_clip(Rect::new(ox + a, row_y(k), (z - a).max(1), lh), sel, clip);
            }
        }
        for (k, l) in gp.msg.lines().iter().enumerate().take(last + 1).skip(first) {
            let t = gp.msg.line_text(l);
            self.tx.draw_ui(cv, ox, row_y(k) + ty, &t, strong, clip, false);
        }
        if gp.msg_focus && gp.focused {
            let (k, x) = gp.msg.caret();
            // Spaces hanging past the wrap edge never push the caret out of the box.
            let cx = ox + (x.round() as i32).min(inner_w + 4);
            cv.fill_clip(Rect::new(cx, row_y(k) + 1, 1, lh - 2), strong, clip);
        }
        let fade = |cv: &mut Canvas, edge: i32, down: bool| {
            for i in 0..MSG_PAD_Y {
                let a = (255 * (MSG_PAD_Y - i) / MSG_PAD_Y) as u8;
                let y = if down { edge + i } else { edge - 1 - i };
                cv.fill_exact(Rect::new(b.x + 4, y, b.w - 8, 1), c.input_bg.with_alpha(a), clip);
            }
        };
        if gp.msg.scroll > 0.5 {
            fade(cv, b.y + 4, true);
        }
        if gp.msg.scroll < gp.msg.max_scroll() - 0.5 {
            fade(cv, b.bottom() - 4, false);
        }
    }

    /// Message box + options + Commit button, pinned to the bottom of the panel.
    pub(super) fn gp_draw_commit_area(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(Rect::new(area.x, area.y, area.w, 1), c.border);
        let mut y = area.y + 10;
        // Notice (success / error), fades after a while.
        if let Some((m, ok, t)) = gp.notice.clone()
            && t.elapsed().as_secs() < 12
        {
            let txt = fit(&mut self.tx, &m, area.w - 32, false);
            self.tx.draw_ui(cv, area.x + 16, y + 1, &txt, if ok { c.git_added } else { c.error }, area, false);
        }
        y += 22;
        // Message box (sized by `gp_msg_layout`).
        let b = Rect::new(area.x + 12, y, area.w - 24, gp.msg_h());
        gp.msg_box = b;
        self.gp_draw_msg(cv, gp, b);
        y += b.h + 8;

        // "Undo last commit" on the right of the Amend row (only the last one, never during a
        // merge / rebase / cherry-pick).
        let mut undo_w = 0;
        if gp.can_undo && !gp.amend && gp.state == RepoState::Clean {
            let label = "Undo last commit";
            let lw = self.tx.ui_width(label, false) + 18 + 20;
            if area.w - 24 - lw > 150 {
                let r = Rect::new(area.right() - 12 - lw, y + 2, lw, 24);
                let hov = self.hv(r);
                if hov {
                    cv.fill_round(r, 8, strong.with_alpha(0x1c));
                }
                let tint = if hov { strong } else { dim };
                self.tx.icon(cv, "ui/undo", r.x + 8, r.y + 4, 16, tint, r);
                self.tx.draw_ui(cv, r.x + 28, r.y + 3, label, tint, r, false);
                gp.hits.push((r, Hit::UndoLast));
                undo_w = lw + 6;
            }
        }
        // Amend checkbox.
        let am = Rect::new(area.x + 12, y, area.w - 24 - undo_w, AMEND_H);
        let ah = self.hv(am);
        if ah {
            cv.fill_round(am, 8, strong.with_alpha(0x12));
        }
        self.gp_checkbox(
            cv,
            Rect::new(am.x + 6, y + 6, 16, 16),
            if gp.amend { CheckState::On } else { CheckState::Off },
            ah,
        );
        let txt = fit(&mut self.tx, "Amend last commit", am.w - 36, false);
        self.tx.draw_ui(cv, am.x + 30, y + 5, &txt, if ah || gp.amend { strong } else { dim }, am, false);
        gp.hits.push((am, Hit::Amend));
        y += AMEND_H;

        // Option + Commit button.
        let staged = gp.staged();
        let ready =
            !gp.msg.text().trim().is_empty() && (gp.amend || staged > 0 || (gp.include_all && !gp.changes.is_empty()));
        let label = if gp.amend { "Amend" } else { "Commit" };
        let bw = self.tx.ui_width(label, true) + 36;
        let btn = Rect::new(area.right() - 12 - bw, y, bw, 34);
        let bh = self.hv(btn);
        let fill = if ready {
            if bh { c.accent.shift(22) } else { c.accent }
        } else if bh {
            c.accent.with_alpha(0x70)
        } else {
            c.accent.with_alpha(0x48)
        };
        cv.fill_round(btn, 10, fill);
        let text_col = if ready { c.on_accent } else { strong.with_alpha(0xc0) };
        self.tx.draw_ui(cv, btn.x + 18, btn.y + 8, label, text_col, btn, true);
        gp.hits.push((btn, Hit::CommitBtn));

        let opt = Rect::new(area.x + 12, y, btn.x - area.x - 20, 34);
        let oh = self.hv(opt);
        if oh {
            cv.fill_round(opt, 8, strong.with_alpha(0x12));
        }
        self.gp_checkbox(
            cv,
            Rect::new(opt.x + 6, y + 9, 16, 16),
            if gp.include_all { CheckState::On } else { CheckState::Off },
            oh,
        );
        let txt = "Include everything if nothing is staged".to_string();
        let txt = fit(&mut self.tx, &txt, opt.w - 36, false);
        self.tx.draw_ui(cv, opt.x + 30, y + 8, &txt, if oh { strong } else { dim }, opt, false);
        gp.hits.push((opt, Hit::Opt));
    }

    pub(super) fn gp_draw_history(&mut self, cv: &mut Canvas, gp: &mut GitPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        // Incremental loading: keep a few screens of rows ahead of the scroll position.
        let mut guard = 0;
        while !gp.log_done && guard < 4 && (gp.scroll[1] as i32 + body.h + 600) >= gp.content_h() {
            gp.load_more();
            guard += 1;
        }
        if gp.commits.is_empty() {
            let msg = "No commits";
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, body.x + (body.w - w) / 2, body.y + 40, msg, dim, body, false);
            return;
        }
        let total = gp.content_h() + 28;
        gp.max_scroll[1] = (total - body.h).max(0) as f64;
        gp.scroll[1] = gp.scroll[1].clamp(0.0, gp.max_scroll[1]);
        let rows = gp.rows();
        let now = now_secs();
        let mut y = body.y - gp.scroll[1] as i32;
        for (k, row) in rows.iter().enumerate() {
            let h = GitPanel::row_h(*row);
            if y + h < body.y || y > body.bottom() {
                y += h;
                continue;
            }
            let vis = Rect::new(body.x, y.max(body.y), body.w, (y + h).min(body.bottom()) - y.max(body.y));
            let r = Rect::new(body.x + 6, y + 1, body.w - 12, h - 2);
            let hov = self.hv(r) && self.hv(vis);
            let cur = gp.focused && gp.cursor == k;
            match *row {
                Row::Commit(i) => {
                    let e = &gp.commits[i];
                    let open = gp.open_commit == Some(i);
                    if open {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x1a), body);
                    } else if hov || cur {
                        cv.fill_round_clip(r, 8, strong.with_alpha(if cur { 0x1e } else { 0x12 }), body);
                    }
                    self.tx.icon(
                        cv,
                        if open { "ui/chevron_down" } else { "ui/chevron_right" },
                        r.x + 8,
                        y + 17,
                        16,
                        dim,
                        vis,
                    );
                    let subj = if e.subject.is_empty() { "(no subject)".to_string() } else { e.subject.clone() };
                    let subj = fit(&mut self.tx, &subj, r.w - 44, open);
                    self.tx.draw_ui(cv, r.x + 32, y + 7, &subj, strong, vis, open);
                    let meta = format!("{}  ·  {}  ·  {}", e.short, e.author, git::ago((now - e.ts).max(0)));
                    // Tags as small pills at the end of the second line (at most half of it).
                    let tags = gp.tags.get(&e.hash).cloned().unwrap_or_default();
                    let mut pill_right = r.right() - 10;
                    let pill_min = r.x + 32 + (r.w - 44) / 2;
                    for t in &tags {
                        let label = fit(&mut self.tx, t, 120, false);
                        let pw = self.tx.ui_width(&label, false) * 12 / 13 + 14;
                        if pill_right - pw < pill_min {
                            break;
                        }
                        let pr = Rect::new(pill_right - pw, y + 27, pw, 18);
                        cv.fill_round_clip(pr, 8, c.accent.with_alpha(0x2e), vis);
                        self.tx.draw_ui_sized(
                            cv,
                            pr.x + 7,
                            pr.y + 1,
                            &label,
                            c.accent_text,
                            pr.intersect(&vis),
                            false,
                            12.0,
                        );
                        pill_right -= pw + 4;
                    }
                    let meta = fit(&mut self.tx, &meta, pill_right - (r.x + 32) - 4, false);
                    self.tx.draw_ui_sized(cv, r.x + 32, y + 28, &meta, dim, vis, false, 12.0);
                    gp.hits.push((r.intersect(&vis), Hit::Commit(i)));
                }
                Row::CFile(j) => {
                    let f = gp.files[j].clone();
                    let sel = gp.open_file == Some(j);
                    if sel {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x1e), body);
                    } else if hov || cur {
                        cv.fill_round_clip(r, 8, strong.with_alpha(if cur { 0x1e } else { 0x12 }), body);
                    }
                    let letter = f.code.to_string();
                    self.tx.draw_ui(cv, r.x + 34, y + 5, &letter, code_color(&c, f.code), vis, true);
                    let (name, dir) = split_name(&f.path);
                    let avail = r.w - 76;
                    let nm = fit(&mut self.tx, name, avail, false);
                    let nw = self.tx.ui_width(&nm, false);
                    self.tx.draw_ui(cv, r.x + 56, y + 5, &nm, strong, vis, false);
                    if !dir.is_empty() && avail - nw > 40 {
                        let d = fit(&mut self.tx, dir, avail - nw - 10, false);
                        self.tx.draw_ui_sized(cv, r.x + 56 + nw + 8, y + 7, &d, dim, vis, false, 12.0);
                    }
                    gp.hits.push((r.intersect(&vis), Hit::CFile(j)));
                }
                _ => {}
            }
            y += h;
        }
        let msg = if gp.log_done { format!("{} commits", gp.commits.len()) } else { "Loading…".to_string() };
        let w = self.tx.ui_width(&msg, false);
        self.tx.draw_ui(cv, body.x + (body.w - w) / 2, y + 4, &msg, dim, body, false);
        let (pos, visible) = (gp.scroll[1], body.h as f64);
        self.region_bar(cv, crate::scrollreg::Reg::GitLog, body, total as f64, visible, pos, 1.0, false);
    }
}
