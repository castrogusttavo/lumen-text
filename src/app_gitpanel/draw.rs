use super::draw_empty::EmptyBtn;
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

    /// Draws the panel and advances its motion by `dt`; true while something still moves (a tab
    /// pill gliding, a fade, a focus ring, a running remote command) so the window keeps redrawing.
    pub fn draw_gitpanel(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        let Some(mut gp) = self.gitpanel.take() else { return false };
        self.gp_sync(&mut gp);
        gp.mo.step(dt);
        gp.mo.show(gp.tab);
        gp.hits.clear();
        let p = self.gitpanel_rect();
        let c = self.theme.chrome;
        cv.fill(p, c.sidebar_bg);
        cv.fill(Rect::new(p.x, p.y, 1, p.h), c.border);
        // Resize handle: the left edge lights up under the pointer and while dragging.
        let (mx, my) = self.mouse;
        if self.ai.resizing || (self.overlay.is_none() && self.panel_on_edge(mx, my)) {
            cv.fill(Rect::new(p.x - 1, p.y, 3, p.h), c.accent.with_alpha(if self.ai.resizing { 0xc0 } else { 0x70 }));
        }

        // Header (section switcher, refresh, actions, close) and the branch row (branch, sync).
        self.gp_draw_header(cv, &mut gp, p);
        let by = p.y + HEAD_H;
        self.gp_draw_identity(cv, &mut gp, p, by);

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
        // The composer sits right under the branch row, the lists fill what is left.
        let comp_y = by + BRANCH_H + strip_h;
        let body =
            Rect::new(p.x + 1, comp_y + commit_h, p.w - 1, (p.h - HEAD_H - BRANCH_H - strip_h - commit_h).max(0));
        if gp.top.is_none() {
            let btn = [EmptyBtn { label: "Initialize repository", hit: Hit::Init, primary: true }];
            let copy = ("No repository here", "Initialize one to track changes, commit and push.");
            self.gp_empty_state(cv, &mut gp, body, ("ui/git_branch", c.accent_text), copy, &btn);
        } else if gp.tab == Tab::Changes {
            self.gp_draw_commit_area(cv, &mut gp, Rect::new(p.x + 1, comp_y, p.w - 1, commit_h));
            self.gp_draw_changes(cv, &mut gp, body);
        } else {
            self.gp_draw_history(cv, &mut gp, body);
        }
        // After a tab switch the body fades in from the panel background.
        let veil = gp.mo.veil_alpha();
        if veil > 0 {
            let top = by + BRANCH_H + strip_h;
            cv.fill(Rect::new(p.x + 1, top, p.w - 1, (p.bottom() - top).max(0)), c.sidebar_bg.with_alpha(veil));
        }
        self.gp_draw_menu(cv, &mut gp, p);
        gp.hover = if p.contains(self.mouse.0, self.mouse.1) { gp.hit_at(self.mouse.0, self.mouse.1) } else { None };
        let busy = gp.mo.moving() || gp.job.is_some();
        self.gitpanel = Some(gp);
        busy
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
            let spin_t = job.started.elapsed().as_secs_f32();
            let head = format!("{}… {secs}s", job.op.busy());
            let hw = self.tx.ui_width(&head, true);
            self.tx.draw_ui(cv, area.x + 16, area.y + 5, &head, strong, area, true);
            let cancel_w = self.tx.ui_width("Cancel", false) + 18;
            let room = area.w - 32 - hw - cancel_w - 16;
            let prog = fit(&mut self.tx, &job.progress.clone(), room, false);
            self.tx.draw_ui(cv, area.x + 16 + hw + 10, area.y + 5, &prog, dim, area, false);
            self.gp_text_btn(cv, gp, area.right() - 8, area.y + 2, "Cancel", Hit::JobCancel, c.error);
            // Indeterminate progress: a segment sweeps along a hairline track under the text.
            let track = Rect::new(area.x + 16, area.bottom() - 4, area.w - 32, 2);
            cv.fill_round(track, 1, c.border.with_alpha(0x90));
            let (off, w) = super::motion::sweep(spin_t, track.w);
            if w > 0 {
                cv.fill_round_clip(Rect::new(track.x + off, track.y, w, track.h), 1, c.accent, track);
            }
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

    /// Row height of the message box (the UI line, at least `LINE`).
    pub(super) fn gp_msg_line() -> i32 {
        (crate::ui::ui_line().ceil() as i32).max(LINE)
    }

    /// Wrap the message at the box's inner width and size the box: 3 visual lines minimum, up
    /// to 10 (less when the panel is short, so the changes list keeps room), then it scrolls.
    pub(super) fn gp_msg_layout(&mut self, gp: &mut GitPanel, p: Rect, strip_h: i32) {
        let lh = Self::gp_msg_line();
        let inner_w = (p.w - 1) - 2 * COMPOSER_MARGIN - 2 * MSG_PAD_X;
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
    pub(super) fn gp_draw_msg(&mut self, cv: &mut Canvas, gp: &mut GitPanel, b: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        gp.hits.push((b, Hit::Msg));
        let lh = Self::gp_msg_line();
        let (ox, oy) = (b.x + MSG_PAD_X, b.y + MSG_PAD_Y);
        let inner_w = b.w - 2 * MSG_PAD_X;
        // Scrolled rows slide under the padding, up to 4 px from the border.
        let clip = Rect::new(b.x + 4, b.y + 4, b.w - 8, b.h - 8);
        // Text centred in its row.
        let ty = ((lh as f32 - crate::ui::ui_line()) / 2.0).max(0.0).round() as i32;
        if gp.msg.text().is_empty() {
            let ph = fit(&mut self.tx, "Message", inner_w, false);
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
}
