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
        }
    }

    /// 28 px icon button (hover tint comes from the app's hover system).
    fn ai_icon_btn(&mut self, cv: &mut Canvas, r: Rect, icon: &str, hit: Option<AiHit>) {
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

    /// The AI page: one group of session tabs, or two side by side (like Orca): each group has a strip
    /// of its own (tabs, `+`, `...`) over the terminal of the tab it shows. The last group's strip also
    /// holds the layout button and the one that leaves the page.
    pub fn draw_aipanel(&mut self, cv: &mut Canvas) {
        self.ai.hits.clear();
        if !self.ai.open {
            return;
        }
        let c = self.theme.chrome;
        let p = self.aipanel_rect();
        cv.fill(p, self.theme.background);
        self.ai.sync_groups();
        self.ai.pane_rects.clear();
        if self.ai.sessions.is_empty() {
            let strip = Rect::new(p.x, p.y, p.w, TABS_H);
            cv.fill(strip, c.sidebar_bg);
            cv.fill(Rect::new(strip.x, strip.bottom() - 1, strip.w, 1), c.border);
            let close = Rect::new(strip.right() - 8 - 28, strip.y + 4, 28, 28);
            self.ai_icon_btn(cv, close, "hugeicons/close", Some(AiHit::Close));
            let body = Rect::new(p.x, p.y + TABS_H, p.w, (p.h - TABS_H).max(0));
            return self.ai_draw_empty(cv, body);
        }
        let n = self.ai.groups.len();
        let saved = self.ai.active;
        let half = p.w / 2;
        for g in 0..n {
            let gr = match (n, g) {
                (1, _) => p,
                (_, 0) => Rect::new(p.x, p.y, half, p.h),
                _ => Rect::new(p.x + half + 1, p.y, p.w - half - 1, p.h),
            };
            self.ai_draw_strip(cv, g, Rect::new(gr.x, gr.y, gr.w, TABS_H), g + 1 == n);
            let Some(idx) = self.ai.index_of(self.ai.groups[g].active) else { continue };
            let body = Rect::new(gr.x, gr.y + TABS_H, gr.w, (gr.h - TABS_H).max(0));
            // The terminal and its "exited" card draw for "the session shown": this group's turn.
            self.ai.active = idx;
            if self.ai.sessions[idx].chat.is_some() {
                self.ai_draw_chat(cv, body, g == self.ai.focus);
            } else {
                self.ai_draw_term(cv, body, g == self.ai.focus);
            }
            self.ai.active = saved;
            self.ai.pane_rects.push((gr, g));
        }
        if n > 1 {
            cv.fill(Rect::new(p.x + half, p.y, 1, p.h), c.border);
        }
    }

    /// One group's strip: its tabs (status dot, agent icon, name, close), `+` right after them, and
    /// `...` at the right edge; the last group also has the layout and the leave-the-page buttons.
    fn ai_draw_strip(&mut self, cv: &mut Canvas, g: usize, r: Rect, last: bool) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(r, c.sidebar_bg);
        cv.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), c.border);
        let focused = g == self.ai.focus;
        // Buttons at the right edge, from the right.
        let mut right = r.right() - 8;
        if last {
            let close = Rect::new(right - 28, r.y + 4, 28, 28);
            self.ai_icon_btn(cv, close, "hugeicons/close", Some(AiHit::Close));
            right -= 32;
            let layout_icon = if self.ai.groups.len() > 1 { "ui/square_minus" } else { "hugeicons/layout" };
            let split = Rect::new(right - 28, r.y + 4, 28, 28);
            self.ai_icon_btn(cv, split, layout_icon, Some(AiHit::Split));
            right -= 32;
        }
        let menu = Rect::new(right - 28, r.y + 4, 28, 28);
        self.ai_icon_btn(cv, menu, "hugeicons/more", Some(AiHit::Menu(g)));
        right -= 32;
        // The IDE bridge of the session this group shows (Claude Code only).
        let saved = self.ai.active;
        if let Some(i) = self.ai.index_of(self.ai.groups[g].active)
            && self.ai.sessions[i].chat.is_none()
        {
            self.ai.active = i;
            right = self.ai_draw_bridge(cv, Rect::new(r.x, r.y - 4, r.w, r.h), right + 4) - 4;
            self.ai.active = saved;
        }
        // Tabs: dot, icon, name, close; natural widths, squeezed evenly when they do not fit.
        let left = r.x + 8;
        let avail = right - 4 - 32 - left;
        let tabs: Vec<usize> = self.ai.groups[g].tabs.iter().filter_map(|id| self.ai.index_of(*id)).collect();
        let n = tabs.len() as i32;
        let natural: Vec<i32> = tabs
            .iter()
            .map(|&i| {
                let unseen = self.ai.sessions[i].track.unseen;
                (40 + self.tx.ui_width(&self.ai.sessions[i].name, unseen) + 26).min(220)
            })
            .collect();
        let total: i32 = natural.iter().sum::<i32>() + 2 * (n - 1);
        let squeezed = (total > avail).then(|| ((avail - 2 * (n - 1)) / n.max(1)).max(56));
        let (mx, my) = self.mouse;
        let shown = self.ai.groups[g].active;
        let mut x = left;
        for (&i, &nat) in tabs.iter().zip(natural.iter()) {
            let w = squeezed.unwrap_or(nat);
            if x + w > right - 4 - 32 {
                break;
            }
            let tr = Rect::new(x, r.y + 4, w, 28);
            let s = &self.ai.sessions[i];
            let active = s.id == shown;
            let (status, unseen, icon, name) = (s.status(), s.track.unseen, s.agent.icon, s.name.clone());
            let hovered = tr.contains(mx, my) && self.overlay.is_none();
            if active {
                cv.fill_round(tr, 8, strong.with_alpha(0x1e));
                let underline = if focused { c.accent } else { c.accent.with_alpha(0x50) };
                cv.fill_round(Rect::new(tr.x + 10, tr.bottom() - 2, (tr.w - 20).max(0), 2), 1, underline);
            } else if hovered {
                cv.fill_round(tr, 8, strong.with_alpha(0x0c));
            }
            let dot = self.ai_status_color(status);
            cv.fill_round(Rect::new(tr.x + 9, tr.y + 11, 6, 6), 3, dot);
            let col = if active || unseen { strong } else { dim };
            self.tx.icon(cv, icon, tr.x + 21, tr.y + 7, 14, col, tr);
            let show_close = active || hovered;
            let name_w = w - 41 - if show_close { 24 } else { 8 };
            if name_w > 8 {
                let label = crate::ui::fit_ellipsis(&mut self.tx, &name, name_w, unseen);
                let clip = Rect::new(tr.x, tr.y, tr.w - if show_close { 22 } else { 4 }, tr.h);
                self.tx.draw_ui(cv, tr.x + 41, tr.y + 5, &label, col, clip, unseen);
            }
            self.ai.hits.push((tr, AiHit::Session(i)));
            if show_close {
                let cb = Rect::new(tr.right() - 22, tr.y + 6, 16, 16);
                self.tx.icon(cv, "ui/close", cb.x + 2, cb.y + 2, 12, dim, cb);
                self.ai.hits.push((cb, AiHit::CloseSession(i)));
            }
            x += w + 2;
        }
        let plus = Rect::new((x + 2).min(right - 4 - 28), r.y + 4, 28, 28);
        self.ai_icon_btn(cv, plus, "hugeicons/plus", Some(AiHit::New(g)));
    }

    /// Claude Code only: the IDE bridge indicator (link icon, green when connected) and the count of
    /// proposed changes waiting for an answer. Returns the new right edge of the free room.
    fn ai_draw_bridge(&mut self, cv: &mut Canvas, h: Rect, mut right: i32) -> i32 {
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
            let r = Rect::new(right - w, h.y + 11, w, 22);
            cv.fill_round(r, 8, c.accent.with_alpha(0x2a).over(c.sidebar_bg));
            self.tx.icon(cv, "ui/file_diff", r.x + 8, r.y + 4, 14, c.accent_text, r);
            self.tx.draw_ui_sized(cv, r.x + 27, r.y + 3, &label, c.accent_text, r, true, SMALL);
            self.ai.hits.push((r, AiHit::Diffs));
            right = r.x - 4;
        }
        let b = Rect::new(right - 28, h.y + 8, 28, 28);
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
    fn ai_draw_term(&mut self, cv: &mut Canvas, body: Rect, is_focus: bool) {
        let inner = Rect::new(body.x + PAD, body.y + PAD, body.w - 2 * PAD, (body.h - 2 * PAD).max(0));
        let (rows, cols) = crate::termview::fit(self.tx.cell_w, inner.w, inner.h);
        let focused = is_focus && self.ai.focused && !self.ai.filter_focus && self.overlay.is_none();
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        let id = self.ai.cur().map_or(0, |s| s.id);
        let look = self.term_look(Which::Ai(id), focused, (inner.x, inner.y));
        let clip = Rect::new(body.x, body.y + 1, body.w, body.h - 1);
        // Flat: the terminal fills its pane (the focused group is told by its tab's underline).
        cv.fill(Rect::new(body.x, body.y, body.w, body.h), self.theme.background);
        let Some(t) = self.ai.term_mut() else { return };
        t.resize(rows, cols);
        t.grid.colors = (fg, bg);
        t.set_focus(focused);
        crate::termview::draw(&mut self.tx, cv, t, inner.x, inner.y, clip, &look);
        let exited = !t.alive;
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

    /// Nothing running: the agents Lumen knows, installed ones ready to start, the others with
    /// their install command (click copies it).
    fn ai_draw_empty(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let any = self.ai.detected.as_ref().is_some_and(|d| d.iter().any(|(_, p)| p.is_some()));
        let w = (body.w - 48).clamp(120, 380);
        let x = body.x + (body.w - w) / 2;
        let rows_h = AGENTS.len() as i32 * 58;
        let total = 48 + 16 + 24 + 80 + rows_h + 12 + 30;
        let mut y = body.y + ((body.h - total) / 2).max(24);
        // Icon in a soft accent disc.
        let disc = Rect::new(body.x + (body.w - 48) / 2, y, 48, 48);
        cv.fill_round(disc, 24, c.accent.with_alpha(0x1c));
        self.tx.icon(cv, "ui/sparkle", disc.x + 14, disc.y + 14, 20, c.accent_text, disc);
        y += 48 + 16;
        let title = if any { "No agents yet" } else { "No agent CLI found" };
        let tw = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, body.x + (body.w - tw) / 2, y, title, strong, body, true);
        y += 24;
        let sub = if any {
            "Start an agent to work on this workspace with you."
        } else {
            "Install one, then Refresh. It runs here with your own login."
        };
        let sub = crate::ui::fit_ellipsis(&mut self.tx, sub, w, false);
        let sw = self.tx.ui_width_sized(&sub, false, SMALL);
        self.tx.draw_ui_sized(cv, body.x + (body.w - sw) / 2, y, &sub, dim, body, false, SMALL);
        y += 32;
        if any {
            let label = "New Agent";
            let bw = 14 + 16 + 6 + self.tx.ui_width(label, true) + 16;
            let b = Rect::new(body.x + (body.w - bw) / 2, y, bw, 32);
            cv.fill_round(b, 10, c.accent);
            self.tx.icon(cv, "ui/plus", b.x + 14, b.y + 8, 16, c.on_accent, b);
            self.tx.draw_ui(cv, b.x + 36, b.y + 7, label, c.on_accent, b, true);
            self.ai.hits.push((b, AiHit::New(0)));
        }
        y += 48;
        for (i, a) in AGENTS.iter().enumerate() {
            let r = Rect::new(x, y, w, 50);
            let installed = self.ai.path_of(a).is_some();
            let (mx, my) = self.mouse;
            let hot = self.overlay.is_none() && r.contains(mx, my);
            cv.card(r, 10, if hot { c.widget_bg } else { c.sidebar_bg }, c.border);
            let tint = if installed { strong } else { dim };
            self.tx.icon(cv, a.icon, r.x + 14, r.y + 17, 16, tint, r);
            self.tx.draw_ui(cv, r.x + 42, r.y + 7, a.name, tint, r, true);
            let (line, col) = if installed { ("Installed — click to start", c.git_added) } else { (a.install, dim) };
            let line = crate::ui::fit_ellipsis(&mut self.tx, line, w - 56 - 22, false);
            self.tx.draw_ui_sized(cv, r.x + 42, r.y + 27, &line, col, r, false, SMALL);
            let glyph = if installed { "ui/play_outlined" } else { "ui/copy" };
            self.tx.icon(cv, glyph, r.right() - 30, r.y + 17, 16, dim, r);
            self.ai.hits.push((r, AiHit::Install(i)));
            y += 58;
        }
        y += 12;
        let label = "Refresh";
        let bw = 12 + 16 + 6 + self.tx.ui_width(label, false) + 14;
        let b = Rect::new(body.x + (body.w - bw) / 2, y, bw, 30);
        cv.ring(b, 8, c.border);
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
