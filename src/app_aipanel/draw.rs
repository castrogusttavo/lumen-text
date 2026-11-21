use super::*;
use crate::{app_topbar::StatusBtn, theme::Rgba};

/// Secondary text size.
const SMALL: f32 = 12.0;

impl App {
    /// The color of a status (chip, tab dot, badge).
    fn ai_status_color(&self, s: Status) -> Rgba {
        let c = self.theme.chrome;
        match s {
            Status::Starting => c.tab_inactive_fg,
            Status::Working => c.accent_text,
            Status::Waiting => c.warning,
            Status::Done => c.git_added,
            Status::Exited(0) => c.tab_inactive_fg,
            Status::Exited(_) => c.error,
            Status::Ready => c.tab_inactive_fg,
        }
    }

    /// Text color and wording of the status chip (the session shown).
    fn ai_status_look(&self) -> (String, Rgba) {
        let s = self.ai.cur().map_or(Status::Starting, Session::status);
        (s.label(), self.ai_status_color(s))
    }

    /// 28 px icon button (hover tint comes from the app's hover system).
    pub(super) fn ai_icon_btn(&mut self, cv: &mut Canvas, r: Rect, icon: &str, hit: Option<AiHit>) {
        let c = self.theme.chrome;
        let (mx, my) = self.mouse;
        if hit.is_some() && self.overlay.is_none() && r.contains(mx, my) {
            cv.fill_round(r, 8, c.tab_active_fg.with_alpha(0x18));
        }
        let tint = if hit.is_some() { c.tab_inactive_fg } else { c.tab_inactive_fg.with_alpha(0x60) };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + (r.h - 16) / 2, 16, tint, r);
        if let Some(h) = hit {
            self.ai.hits.push((r, h));
        }
    }

    /// The "Git | AI" switch on top of the left-hand slot, and the AI panel when it is the one shown.
    pub fn draw_aipanel(&mut self, cv: &mut Canvas) {
        self.ai.hits.clear();
        if self.gitpanel.is_none() && !self.ai.open && !self.gh_open() {
            return;
        }
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let slot = self.slot_rect();
        let strip = Rect::new(slot.x, slot.y, slot.w, SLOT_H);
        cv.fill(strip, c.sidebar_bg);
        cv.fill(Rect::new(strip.right() - 1, strip.y, 1, strip.h), c.border);
        cv.fill(Rect::new(strip.x, strip.bottom() - 1, strip.w - 1, 1), c.border);
        let gh_on = self.gh_open();
        let mut x = strip.x + 12;
        // On the AI switch while hidden: the most pressing session's color.
        let ai_dot = match self.ai.badge() {
            Some((_, s)) => Some(self.ai_status_color(s)),
            None => self.ai.cur().map(|_| self.ai_status_look().1),
        }
        .filter(|_| !self.ai.open);
        for (hit, icon, label, active) in [
            (AiHit::TabGit, "ui/git_graph", "Git", !self.ai.open && !gh_on),
            (AiHit::TabGh, "ui/github", "GitHub", gh_on),
            (AiHit::TabAi, "ui/sparkle", "AI", self.ai.open),
        ] {
            let lw = self.tx.ui_width(label, active);
            let dot = hit == AiHit::TabAi && ai_dot.is_some();
            let w = 10 + 16 + 7 + lw + 12 + if dot { 12 } else { 0 };
            let r = Rect::new(x, strip.y + 6, w, 28);
            if active {
                cv.fill_round(r, 8, strong.with_alpha(0x1e));
            }
            let col = if active { strong } else { dim };
            self.tx.icon(cv, icon, r.x + 10, r.y + 6, 16, col, r);
            self.tx.draw_ui(cv, r.x + 33, r.y + 5, label, col, r, active);
            if let (true, Some(d)) = (dot, ai_dot) {
                cv.fill_round(Rect::new(r.x + 33 + lw + 6, r.y + 11, 6, 6), 3, d);
            }
            self.ai.hits.push((r, hit));
            x += w + 4;
        }
        if !self.ai.open {
            return;
        }
        let close = Rect::new(strip.right() - 12 - 28, strip.y + 6, 28, 28);
        self.ai_icon_btn(cv, close, "ui/close", Some(AiHit::Close));

        let p = self.aipanel_rect();
        cv.fill(p, c.sidebar_bg);
        cv.fill(Rect::new(p.right() - 1, p.y, 1, p.h), c.border);
        // Resize handle: the right edge lights up under the pointer and while dragging.
        let (mx, my) = self.mouse;
        if self.ai.resizing || (self.overlay.is_none() && self.ai_on_edge(mx, my)) {
            cv.fill(
                Rect::new(slot.right() - 2, slot.y, 3, slot.h),
                c.accent.with_alpha(if self.ai.resizing { 0xc0 } else { 0x70 }),
            );
        }
        let has = !self.ai.sessions.is_empty();
        let mut top = p.y;
        self.ai_draw_header(cv, Rect::new(p.x + 1, top, p.w - 1, HEAD_H));
        top += HEAD_H;
        if has {
            self.ai_draw_tabs(cv, Rect::new(p.x + 1, top, p.w - 1, TABS_H));
            top += TABS_H;
            if self.ai_filter_shown() {
                self.ai_draw_filter(cv, Rect::new(p.x + 1, top, p.w - 1, FILTER_H));
                top += FILTER_H;
            }
        } else {
            cv.fill(Rect::new(p.x + 1, top - 1, p.w - 1, 1), c.border);
        }
        let body = Rect::new(p.x + 1, top, p.w - 1, (p.bottom() - top).max(0));
        cv.fill(body, self.theme.background);
        if has {
            // The composer is measured first: the terminal gets what is left above it.
            let geo = self.ai_composer_geo(p, top);
            let term_body = Rect::new(body.x, body.y, body.w, (geo.area.y - body.y).max(0));
            if self.ai.cur().is_some_and(|s| s.chat.is_some()) {
                self.ai_draw_chat(cv, term_body);
            } else {
                self.ai_draw_term(cv, term_body);
            }
            self.ai_draw_composer(cv, &geo);
        } else {
            self.ai_draw_empty(cv, body);
        }
    }

    /// One pill per session (status dot, agent icon, name, close); the `+` lives in the header.
    fn ai_draw_tabs(&mut self, cv: &mut Canvas, r: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), c.border);
        let left = r.x + 12;
        let avail = r.right() - 12 - left;
        let vis = self.ai.visible();
        let n = vis.len() as i32;
        if vis.is_empty() {
            let msg = "No sessions match";
            let clip = Rect::new(left, r.y, avail.max(0), r.h);
            self.tx.draw_ui(cv, left + 4, r.y + 9, msg, dim, clip, false);
        }
        // Dot, icon, name, close: natural widths, squeezed evenly when they do not fit.
        let natural: Vec<i32> = vis
            .iter()
            .map(|&i| {
                let unseen = self.ai.sessions[i].track.unseen;
                (42 + self.tx.ui_width(&self.ai.sessions[i].name, unseen) + 28).min(220)
            })
            .collect();
        let total: i32 = natural.iter().sum::<i32>() + 4 * (n - 1);
        let squeezed = (total > avail).then(|| ((avail - 4 * (n - 1)) / n.max(1)).max(64));
        let (mx, my) = self.mouse;
        let mut x = left;
        for (&i, &nat) in vis.iter().zip(natural.iter()) {
            let w = squeezed.unwrap_or(nat);
            if x + w > r.right() - 8 {
                // No room left: the rest stays reachable by "Next session waiting" and the menu.
                break;
            }
            let tr = Rect::new(x, r.y + 5, w, 26);
            let active = i == self.ai.active;
            let s = &self.ai.sessions[i];
            let (status, unseen, icon, name) = (s.status(), s.track.unseen, s.agent.icon, s.name.clone());
            let hovered = tr.contains(mx, my) && self.overlay.is_none();
            if active {
                cv.fill_round(tr, 13, strong.with_alpha(0x1e));
                cv.ring(tr, 13, c.accent.with_alpha(0x60));
            } else if hovered {
                cv.fill_round(tr, 13, strong.with_alpha(0x0e));
            }
            let dot = self.ai_status_color(status);
            cv.fill_round(Rect::new(tr.x + 10, tr.y + 10, 6, 6), 3, dot);
            let col = if active || unseen { strong } else { dim };
            self.tx.icon(cv, icon, tr.x + 22, tr.y + 6, 14, col, tr);
            let show_close = active || hovered;
            let name_w = w - 42 - if show_close { 26 } else { 10 };
            if name_w > 8 {
                let label = crate::ui::fit_ellipsis(&mut self.tx, &name, name_w, unseen);
                let clip = Rect::new(tr.x, tr.y, tr.w - if show_close { 24 } else { 6 }, tr.h);
                self.tx.draw_ui(cv, tr.x + 42, tr.y + 4, &label, col, clip, unseen);
            }
            self.ai.hits.push((tr, AiHit::Session(i)));
            if show_close {
                let cb = Rect::new(tr.right() - 22, tr.y + 5, 16, 16);
                self.tx.icon(cv, "ui/close", cb.x + 2, cb.y + 2, 12, dim, cb);
                self.ai.hits.push((cb, AiHit::CloseSession(i)));
            }
            x += w + 4;
        }
    }

    /// The filter field under the tabs: narrows them by name or agent.
    fn ai_draw_filter(&mut self, cv: &mut Canvas, r: Rect) {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        cv.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), c.border);
        let f = Rect::new(r.x + 8, r.y + 4, r.w - 16, r.h - 9);
        let (mx, my) = self.mouse;
        let on = self.ai.filter_focus && self.ai.focused;
        cv.card(f, 7, c.widget_bg, if on { c.accent } else { c.border });
        self.ai.hits.push((f, AiHit::Filter));
        self.tx.icon(cv, "hugeicons/search", f.x + 8, f.y + (f.h - 14) / 2, 14, dim, f);
        let has = !self.ai.filter.text.is_empty();
        let tx = f.x + 29;
        let right = f.right() - if has { 26 } else { 8 };
        let clip = Rect::new(f.x, f.y, (right - f.x).max(0), f.h);
        let (txt, col) = if has { (self.ai.filter.text.clone(), fg) } else { ("Filter sessions…".to_string(), dim) };
        if self.ai.filter.sel_all && has {
            let w = self.tx.ui_width(&txt, false);
            cv.fill_round_clip(Rect::new(tx - 2, f.y + 3, w + 4, f.h - 6), 4, c.accent.with_alpha(0x55), clip);
        }
        self.tx.draw_ui(cv, tx, f.y + (f.h - 18) / 2, &txt, col, clip, false);
        if on {
            let prefix: String = self.ai.filter.text.chars().take(self.ai.filter.cursor).collect();
            let cx = tx + self.tx.ui_width(&prefix, false);
            cv.fill_clip(Rect::new(cx, f.y + (f.h - 16) / 2, 1, 16), fg, clip);
        }
        if has {
            let b = Rect::new(f.right() - 24, f.y + (f.h - 20) / 2, 20, 20);
            if self.overlay.is_none() && b.contains(mx, my) {
                cv.fill_round(b, 5, c.tab_active_fg.with_alpha(0x18));
            }
            self.tx.icon(cv, "ui/close", b.x + 4, b.y + 4, 12, dim, b);
            self.ai.hits.push((b, AiHit::FilterClear));
        }
    }

    /// Header: the session's icon, title and status line, then (right to left) close, menu,
    /// Stop / Restart, new session and the IDE bridge. Without a session: a plain title and `+`.
    fn ai_draw_header(&mut self, cv: &mut Canvas, h: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let has = !self.ai.sessions.is_empty();
        let by = h.y + (h.h - 28) / 2;
        // Buttons, right to left.
        let mut right = h.right() - 10;
        if has {
            let b = Rect::new(right - 28, by, 28, 28);
            self.ai_icon_btn(cv, b, "ui/close", Some(AiHit::CloseSession(self.ai.active)));
            right -= 30;
            let b = Rect::new(right - 28, by, 28, 28);
            self.ai_icon_btn(cv, b, "ui/ellipsis", Some(AiHit::Menu));
            right -= 30;
            let alive = self
                .ai
                .cur()
                .is_some_and(|s| if s.chat.is_some() { s.busy() } else { s.term.as_ref().is_some_and(|t| t.alive) });
            let (icon, hit) = if alive { ("ui/stop", AiHit::Stop) } else { ("ui/rotate_cw", AiHit::Restart) };
            let b = Rect::new(right - 28, by, 28, 28);
            self.ai_icon_btn(cv, b, icon, Some(hit));
            right -= 30;
        }
        let b = Rect::new(right - 28, by, 28, 28);
        self.ai_icon_btn(cv, b, "ui/plus", Some(AiHit::New));
        right -= 30;
        if has {
            right = self.ai_draw_bridge(cv, h, right);
        }
        let (icon, name) = match self.ai.cur() {
            Some(s) => (s.agent.icon, s.name.clone()),
            None => ("ui/sparkle", "AI agents".to_string()),
        };
        let disc = Rect::new(h.x + 12, by, 28, 28);
        cv.fill_round(disc, 14, c.accent.with_alpha(0x1c));
        self.tx.icon(cv, icon, disc.x + 6, disc.y + 6, 16, c.accent_text, disc);
        let tx = disc.right() + 10;
        let room = right - 6 - tx;
        if room <= 24 {
            return;
        }
        let clip = Rect::new(tx, h.y, room, h.h);
        let title = crate::ui::fit_ellipsis(&mut self.tx, &name, room, true);
        self.tx.draw_ui(cv, tx, h.y + 6, &title, strong, clip, true);
        // Status line: dot (or spinner while working), the status, and the task the agent shows.
        let sy = h.y + 26;
        if !has {
            self.tx.draw_ui_sized(cv, tx, sy, "No session running", dim, clip, false, SMALL);
            return;
        }
        let (label, col) = self.ai_status_look();
        let bg = c.sidebar_bg;
        if self.ai.cur().is_some_and(|s| s.status() == Status::Working) {
            // Turns as output arrives (each frame), quiet otherwise: no timer of its own.
            let t = self.ai.cur().map_or(0.0, |s| {
                let since = s.chat.as_ref().map(|c| c.started).or(s.term.as_ref().map(|t| t.started));
                since.map_or(0.0, |t| t.elapsed().as_secs_f32())
            });
            crate::app_tests::spinner_t(cv, tx + 4, sy + 9, 4, col, bg, t, clip);
        } else {
            cv.fill_round(Rect::new(tx, sy + 5, 8, 8), 4, col);
        }
        let lw = self.tx.ui_width_sized(&label, false, SMALL);
        self.tx.draw_ui_sized(cv, tx + 14, sy, &label, col, clip, false, SMALL);
        let task = self.ai.cur().map(Session::task).unwrap_or_default();
        let left = room - 14 - lw - 12;
        if !task.is_empty() && left > 40 {
            let text = crate::ui::fit_ellipsis(&mut self.tx, &task, left, false);
            self.tx.draw_ui_sized(cv, tx + 14 + lw + 8, sy, &format!("· {text}"), dim, clip, false, SMALL);
        }
        let status = Rect::new(tx, sy, (14 + lw).min(room), 18);
        self.ai.hits.push((status, AiHit::Status));
    }

    /// Claude Code only: the IDE bridge indicator (link icon, green when connected) and the count of
    /// proposed changes waiting for an answer. Returns the new right edge of the free room.
    fn ai_draw_bridge(&mut self, cv: &mut Canvas, h: Rect, mut right: i32) -> i32 {
        if self.ai.cur().is_some_and(|s| s.chat.is_some()) {
            return right;
        }
        let connected = self.ai_bridge_connected();
        if self.ai.agent().is_none_or(|a| a.id != "claude") && !connected {
            return right;
        }
        let c = self.theme.chrome;
        let pending = self.ai.bridge.proposals.len();
        if pending > 0 {
            let label = pending.to_string();
            let lw = self.tx.ui_width_sized(&label, true, SMALL);
            let w = 8 + 14 + 5 + lw + 9;
            let r = Rect::new(right - w, h.y + (h.h - 22) / 2, w, 22);
            cv.fill_round(r, 8, c.accent.with_alpha(0x2a).over(c.sidebar_bg));
            self.tx.icon(cv, "ui/file_diff", r.x + 8, r.y + 4, 14, c.accent_text, r);
            self.tx.draw_ui_sized(cv, r.x + 27, r.y + 3, &label, c.accent_text, r, true, SMALL);
            self.ai.hits.push((r, AiHit::Diffs));
            right = r.x - 4;
        }
        let b = Rect::new(right - 28, h.y + (h.h - 28) / 2, 28, 28);
        let (icon, tint) =
            if connected { ("ui/link", c.git_added) } else { ("ui/disconnected", c.tab_inactive_fg.with_alpha(0x80)) };
        self.tx.icon(cv, icon, b.x + 6, b.y + 6, 16, tint, b);
        if connected {
            // A small "live" dot on the icon's corner.
            cv.fill_round(Rect::new(b.right() - 9, b.y + 5, 6, 6), 3, c.git_added);
        }
        self.ai.hits.push((b, AiHit::Ide));
        b.x - 4
    }

    /// The agent's terminal, plus the "exited" card when the process has ended.
    fn ai_draw_term(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let inner = Rect::new(body.x + PAD, body.y + PAD, body.w - 2 * PAD, (body.h - 2 * PAD).max(0));
        let (rows, cols) = crate::termview::fit(self.tx.cell_w, inner.w, inner.h);
        let focused = self.ai.focused && !self.ai.filter_focus && !self.ai.composer_focus && self.overlay.is_none();
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        let look = self.term_look(Which::Ai, focused, (inner.x, inner.y));
        let clip = Rect::new(inner.x, inner.y, inner.w, inner.h);
        // The terminal sits on a rounded surface inset from the panel edges.
        let card = Rect::new(inner.x - 6, inner.y - 6, inner.w + 12, inner.h + 12);
        let strip = Rect::new(body.x, body.y, body.w, body.h + self.ai_composer_h());
        cv.fill(strip, c.sidebar_bg);
        cv.card(card, 12, self.theme.background, c.border);
        let Some(t) = self.ai.term_mut() else { return };
        t.resize(rows, cols);
        t.grid.colors = (fg, bg);
        t.set_focus(focused);
        crate::termview::draw(&mut self.tx, cv, t, inner.x, inner.y, clip, &look);
        let exited = !t.alive;
        if focused {
            cv.ring(card, 12, c.accent.with_alpha(0x70));
        }
        if exited {
            self.ai_draw_exited(cv, body);
        }
    }

    /// Bottom card over the last screen of an agent that ended: exit code and Restart.
    fn ai_draw_exited(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let name = self.ai.cur().map_or_else(|| "The agent".to_string(), |s| s.name.clone());
        let code = match self.ai.cur().map(Session::status) {
            Some(Status::Exited(n)) => n,
            _ => self.ai.term().and_then(|t| t.exit_code).unwrap_or(-1),
        };
        let w = (body.w - 32).min(340);
        if w < 160 {
            return;
        }
        let card = Rect::new(body.x + (body.w - w) / 2, body.bottom() - 16 - 112, w, 112);
        cv.shadow(card, 10);
        cv.card(card, 10, c.widget_bg, c.border);
        let icon_col = if code == 0 { c.tab_inactive_fg } else { c.error };
        self.tx.icon(cv, "ui/power", card.x + 16, card.y + 18, 16, icon_col, card);
        let title = crate::ui::fit_ellipsis(&mut self.tx, &format!("{name} exited"), w - 56, true);
        self.tx.draw_ui(cv, card.x + 42, card.y + 16, &title, c.tab_active_fg, card, true);
        let sub = if code == 0 { "Exit code 0".to_string() } else { format!("Exit code {code}") };
        self.tx.draw_ui_sized(cv, card.x + 42, card.y + 38, &sub, c.tab_inactive_fg, card, false, SMALL);
        // Restart (accent) and New session.
        let label = "Restart";
        let bw = self.tx.ui_width(label, true) + 28;
        let b = Rect::new(card.x + 42, card.bottom() - 16 - 30, bw, 30);
        cv.fill_round(b, 8, c.accent);
        self.tx.draw_ui(cv, b.x + 14, b.y + 6, label, c.on_accent, b, true);
        self.ai.hits.push((b, AiHit::Restart));
        let pick = "Choose agent";
        let pw = self.tx.ui_width(pick, false) + 24;
        let pb = Rect::new(b.right() + 8, b.y, pw, 30);
        if pb.right() <= card.right() - 12 {
            cv.ring(pb, 8, c.border);
            self.tx.draw_ui(cv, pb.x + 12, pb.y + 6, pick, c.tab_active_fg, pb, false);
            self.ai.hits.push((pb, AiHit::Agent));
        }
    }

    /// Nothing running: an icon, a title and one button per installed agent (click starts a
    /// session); the agents not installed follow, dimmed, with their install command (click copies it).
    fn ai_draw_empty(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let installed: Vec<usize> = (0..AGENTS.len()).filter(|&i| self.ai.path_of(&AGENTS[i]).is_some()).collect();
        let missing: Vec<usize> = (0..AGENTS.len()).filter(|i| !installed.contains(i)).collect();
        let any = !installed.is_empty();
        let w = (body.w - 48).clamp(120, 380);
        let x = body.x + (body.w - w) / 2;
        let rows_h = installed.len() as i32 * 46 + if missing.is_empty() { 0 } else { 28 + missing.len() as i32 * 40 };
        let total = 56 + 16 + 24 + 36 + rows_h + 12 + 30;
        let mut y = body.y + ((body.h - total) / 2).max(24);
        // Icon in a soft accent disc.
        let disc = Rect::new(body.x + (body.w - 56) / 2, y, 56, 56);
        cv.fill_round(disc, 28, c.accent.with_alpha(0x1c));
        self.tx.icon(cv, "ui/sparkle", disc.x + 16, disc.y + 16, 24, c.accent_text, disc);
        y += 56 + 16;
        let title = if any { "Start an agent session" } else { "No agent CLI found" };
        let tw = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, body.x + (body.w - tw) / 2, y, title, strong, body, true);
        y += 26;
        let sub = if any {
            "Pick an agent to work on this workspace with you."
        } else {
            "Install one, then Refresh. It runs here with your own login."
        };
        let sub = crate::ui::fit_ellipsis(&mut self.tx, sub, w, false);
        let sw = self.tx.ui_width_sized(&sub, false, SMALL);
        self.tx.draw_ui_sized(cv, body.x + (body.w - sw) / 2, y, &sub, dim, body, false, SMALL);
        y += 36;
        let (mx, my) = self.mouse;
        for &i in &installed {
            let a = &AGENTS[i];
            let r = Rect::new(x, y, w, 38);
            let hot = self.overlay.is_none() && r.contains(mx, my);
            cv.card(r, 19, if hot { c.widget_bg } else { c.sidebar_bg }, if hot { c.accent } else { c.border });
            self.tx.icon(cv, a.icon, r.x + 14, r.y + 11, 16, strong, r);
            self.tx.draw_ui(cv, r.x + 40, r.y + 9, a.name, strong, r, true);
            self.tx.icon(cv, "ui/arrow_up_right", r.right() - 32, r.y + 11, 16, dim, r);
            self.ai.hits.push((r, AiHit::Install(i)));
            y += 46;
        }
        if !missing.is_empty() {
            self.tx.draw_ui_sized(cv, x + 4, y + 4, "Not installed", dim, body, false, SMALL);
            y += 28;
            for &i in &missing {
                let a = &AGENTS[i];
                let r = Rect::new(x, y, w, 34);
                let hot = self.overlay.is_none() && r.contains(mx, my);
                if hot {
                    cv.fill_round(r, 10, strong.with_alpha(0x0e));
                }
                self.tx.icon(cv, a.icon, r.x + 12, r.y + 9, 16, dim, r);
                let name_w = self.tx.ui_width(a.name, false);
                self.tx.draw_ui(cv, r.x + 38, r.y + 7, a.name, dim, r, false);
                let room = w - 38 - name_w - 16 - 30;
                if room > 40 {
                    let line = crate::ui::fit_ellipsis(&mut self.tx, a.install, room, false);
                    let faint = dim.with_alpha(0xa0);
                    self.tx.draw_ui_sized(cv, r.x + 38 + name_w + 12, r.y + 9, &line, faint, r, false, SMALL);
                }
                self.tx.icon(cv, "ui/copy", r.right() - 28, r.y + 9, 16, dim, r);
                self.ai.hits.push((r, AiHit::Install(i)));
                y += 40;
            }
        }
        y += 12;
        let label = "Refresh";
        let bw = 12 + 16 + 6 + self.tx.ui_width(label, false) + 14;
        let b = Rect::new(body.x + (body.w - bw) / 2, y, bw, 30);
        cv.ring(b, 15, c.border);
        self.tx.icon(cv, "ui/rotate_cw", b.x + 12, b.y + 7, 16, strong, b);
        self.tx.draw_ui(cv, b.x + 34, b.y + 6, label, strong, b, false);
        self.ai.hits.push((b, AiHit::Refresh));
    }

    /// Status bar badge for the sessions that finished or want an answer while nobody looked
    /// ("Waiting for you", "2 waiting · 1 done"); returns the x after it.
    pub fn draw_ai_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        let Some((label, status)) = self.ai.badge() else { return x };
        let c = self.theme.chrome;
        let r = self.layout.status;
        let col = match status {
            Status::Done => c.git_added,
            Status::Exited(_) => c.tab_inactive_fg,
            _ => c.accent_text,
        };
        let label = label.as_str();
        let lw = self.tx.ui_width(label, false);
        let b = Rect::new(x, r.y + 3, 8 + 14 + 6 + lw + 8, r.h - 6);
        cv.fill_round(b, 6, c.accent.with_alpha(0x22));
        self.tx.icon(cv, "ui/sparkle", b.x + 8, r.y + (r.h - 14) / 2, 14, col, r);
        self.tx.draw_ui(cv, b.x + 8 + 14 + 6, r.y + (r.h - 18) / 2, label, col, r, false);
        self.layout.status_btns.push((b, StatusBtn::Ai));
        b.right() + 2
    }
}
