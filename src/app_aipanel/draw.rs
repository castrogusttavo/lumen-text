use super::*;
use crate::{app_topbar::StatusBtn, theme::Rgba};

/// Secondary text size.
const SMALL: f32 = 12.0;

impl App {
    /// Text color and wording of the status chip.
    fn ai_status_look(&self) -> (String, Rgba) {
        let c = self.theme.chrome;
        let s = self.ai.status;
        let col = match s {
            Status::Starting => c.tab_inactive_fg,
            Status::Working => c.accent_text,
            Status::Waiting => c.warning,
            Status::Done => c.git_added,
            Status::Exited(0) => c.tab_inactive_fg,
            Status::Exited(_) => c.error,
        };
        (s.label(), col)
    }

    /// 28 px icon button (hover tint comes from the app's hover system).
    fn ai_icon_btn(&mut self, cv: &mut Canvas, r: Rect, icon: &str, hit: Option<AiHit>) {
        let c = self.theme.chrome;
        let tint = if hit.is_some() { c.tab_inactive_fg } else { c.tab_inactive_fg.with_alpha(0x60) };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + (r.h - 16) / 2, 16, tint, r);
        if let Some(h) = hit {
            self.ai.hits.push((r, h));
        }
    }

    /// The "Git | AI" switch on top of the right-hand slot, and the AI panel when it is the one shown.
    pub fn draw_aipanel(&mut self, cv: &mut Canvas) {
        self.ai.hits.clear();
        if self.gitpanel.is_none() && !self.ai.open {
            return;
        }
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let slot = self.right_slot_rect();
        let strip = Rect::new(slot.x, slot.y, slot.w, SLOT_H);
        cv.fill(strip, c.sidebar_bg);
        cv.fill(Rect::new(strip.x, strip.y, 1, strip.h), c.border);
        cv.fill(Rect::new(strip.x + 1, strip.bottom() - 1, strip.w - 1, 1), c.border);
        let mut x = strip.x + 12;
        let ai_dot = self.ai.term.as_ref().map(|_| self.ai_status_look().1).filter(|_| !self.ai.open);
        for (hit, icon, label, active) in
            [(AiHit::TabGit, "ui/git_graph", "Git", !self.ai.open), (AiHit::TabAi, "ui/sparkle", "AI", self.ai.open)]
        {
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
        cv.fill(Rect::new(p.x, p.y, 1, p.h), c.border);
        // Resize handle: the left edge lights up under the pointer and while dragging.
        let (mx, my) = self.mouse;
        if self.ai.resizing || (self.overlay.is_none() && self.ai_on_edge(mx, my)) {
            cv.fill(
                Rect::new(slot.x - 1, slot.y, 3, slot.h),
                c.accent.with_alpha(if self.ai.resizing { 0xc0 } else { 0x70 }),
            );
        }
        self.ai_draw_header(cv, Rect::new(p.x + 1, p.y, p.w - 1, HEAD_H));
        let body = Rect::new(p.x + 1, p.y + HEAD_H, p.w - 1, (p.h - HEAD_H).max(0));
        cv.fill(body, self.theme.background);
        cv.fill(Rect::new(body.x, body.y, body.w, 1), c.border);
        if self.ai.term.is_some() {
            self.ai_draw_term(cv, body);
        } else {
            self.ai_draw_empty(cv, body);
        }
    }

    /// Agent picker, status chip, and New session / Restart / Stop.
    fn ai_draw_header(&mut self, cv: &mut Canvas, h: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (icon, name) = self.ai.agent.map_or(("ui/sparkle", "Choose agent"), |a| (a.icon, a.name));
        let nw = self.tx.ui_width(name, true);
        let pill = Rect::new(h.x + 11, h.y + 8, 10 + 16 + 8 + nw + 8 + 12 + 10, 28);
        cv.fill_round(pill, 8, strong.with_alpha(0x0e));
        self.tx.icon(cv, icon, pill.x + 10, pill.y + 6, 16, strong, pill);
        self.tx.draw_ui(cv, pill.x + 34, pill.y + 5, name, strong, pill, true);
        self.tx.icon(cv, "ui/chevron_down", pill.right() - 22, pill.y + 8, 12, dim, pill);
        self.ai.hits.push((pill, AiHit::Agent));

        let alive = self.ai.term.as_ref().is_some_and(|t| t.alive);
        let has_agent = self.ai.agent.is_some_and(|a| self.ai.path_of(a).is_some());
        let mut right = h.right() - 11;
        for (icon, hit, on) in [
            ("ui/stop", AiHit::Stop, alive),
            ("ui/rotate_cw", AiHit::Restart, has_agent),
            ("ui/plus", AiHit::New, has_agent),
        ] {
            let b = Rect::new(right - 28, h.y + 8, 28, 28);
            self.ai_icon_btn(cv, b, icon, on.then_some(hit));
            right -= 32;
        }
        if self.ai.term.is_none() {
            return;
        }
        // Status chip in the room left between the picker and the buttons: dot + words, or a lone dot.
        let (label, col) = self.ai_status_look();
        let lw = self.tx.ui_width_sized(&label, false, SMALL);
        let room = right - 4 - (pill.right() + 8);
        let full = 10 + 8 + 6 + lw + 10;
        let w = if room >= full {
            full
        } else if room >= 24 {
            24
        } else {
            return;
        };
        let chip = Rect::new(pill.right() + 8, h.y + 11, w, 22);
        let bg = c.sidebar_bg;
        cv.fill_round(chip, 8, col.with_alpha(0x1a).over(bg));
        let (dx, dy) = (chip.x + if w == 24 { 8 } else { 10 }, chip.y + 7);
        if self.ai.status == Status::Working {
            // Turns as output arrives (each frame), quiet otherwise: no timer of its own.
            let t = self.ai.term.as_ref().map_or(0.0, |t| t.started.elapsed().as_secs_f32());
            crate::app_tests::spinner_t(cv, dx + 4, dy + 4, 4, col, col.with_alpha(0x1a).over(bg), t, chip);
        } else {
            cv.fill_round(Rect::new(dx, dy, 8, 8), 4, col);
        }
        if w == full {
            self.tx.draw_ui_sized(cv, dx + 14, chip.y + 3, &label, col, chip, false, SMALL);
        }
        self.ai.hits.push((chip, AiHit::Status));
    }

    /// The agent's terminal, plus the "exited" card when the process has ended.
    fn ai_draw_term(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let inner = Rect::new(body.x + PAD, body.y + PAD, body.w - 2 * PAD, (body.h - 2 * PAD).max(0));
        let (rows, cols) = crate::termview::fit(self.tx.cell_w, inner.w, inner.h);
        let focused = self.ai.focused && self.overlay.is_none();
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        let look = self.term_look(Which::Ai, focused, (inner.x, inner.y));
        let clip = Rect::new(body.x, body.y + 1, body.w, body.h - 1);
        let Some(t) = self.ai.term.as_mut() else { return };
        t.resize(rows, cols);
        t.grid.colors = (fg, bg);
        t.set_focus(focused);
        crate::termview::draw(&mut self.tx, cv, t, inner.x, inner.y, clip, &look);
        let exited = !t.alive;
        if focused {
            cv.ring(body.inset(4), 8, c.accent.with_alpha(0x60));
        }
        if exited {
            self.ai_draw_exited(cv, body);
        }
    }

    /// Bottom card over the last screen of an agent that ended: exit code and Restart.
    fn ai_draw_exited(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let name = self.ai.agent.map_or("The agent", |a| a.name);
        let code = match self.ai.status {
            Status::Exited(n) => n,
            _ => self.ai.term.as_ref().and_then(|t| t.exit_code).unwrap_or(-1),
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
        let total = 48 + 16 + 24 + 40 + rows_h + 12 + 30;
        let mut y = body.y + ((body.h - total) / 2).max(24);
        // Icon in a soft accent disc.
        let disc = Rect::new(body.x + (body.w - 48) / 2, y, 48, 48);
        cv.fill_round(disc, 24, c.accent.with_alpha(0x1c));
        self.tx.icon(cv, "ui/sparkle", disc.x + 14, disc.y + 14, 20, c.accent_text, disc);
        y += 48 + 16;
        let title = if any { "Start an agent" } else { "No agent CLI found" };
        let tw = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, body.x + (body.w - tw) / 2, y, title, strong, body, true);
        y += 24;
        let sub = if any {
            "Runs in this project with your own login."
        } else {
            "Install one, then Refresh. It runs here with your own login."
        };
        let sub = crate::ui::fit_ellipsis(&mut self.tx, sub, w, false);
        let sw = self.tx.ui_width_sized(&sub, false, SMALL);
        self.tx.draw_ui_sized(cv, body.x + (body.w - sw) / 2, y, &sub, dim, body, false, SMALL);
        y += 40;
        for (i, a) in AGENTS.iter().enumerate() {
            let r = Rect::new(x, y, w, 50);
            let installed = self.ai.path_of(a).is_some();
            cv.card(r, 8, c.sidebar_bg, c.border);
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

    /// Status bar badge while the panel is hidden and the agent finished or wants an answer;
    /// returns the x after it.
    pub fn draw_ai_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        if self.ai.open || !self.ai.unseen || self.ai.term.is_none() {
            return x;
        }
        let c = self.theme.chrome;
        let r = self.layout.status;
        let (label, col) = match self.ai.status {
            Status::Done => ("Agent done", c.git_added),
            Status::Exited(_) => ("Agent exited", c.tab_inactive_fg),
            _ => ("Waiting for you", c.accent_text),
        };
        let lw = self.tx.ui_width(label, false);
        let b = Rect::new(x, r.y + 3, 8 + 14 + 6 + lw + 8, r.h - 6);
        cv.fill_round(b, 6, c.accent.with_alpha(0x22));
        self.tx.icon(cv, "ui/sparkle", b.x + 8, r.y + (r.h - 14) / 2, 14, col, r);
        self.tx.draw_ui(cv, b.x + 8 + 14 + 6, r.y + (r.h - 18) / 2, label, col, r, false);
        self.layout.status_btns.push((b, StatusBtn::Ai));
        b.right() + 2
    }
}
