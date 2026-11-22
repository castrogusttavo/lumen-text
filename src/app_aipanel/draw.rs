use super::*;
use crate::{anim::fade, app_topbar::StatusBtn, ds, theme::Rgba};

/// Secondary text size.
const SMALL: f32 = 12.0;
/// Rise in pixels of an item of the empty state while it fades in.
const RISE: f32 = 8.0;
/// Widths of the skeleton bars (fraction of the card).
const BARS: [f32; 6] = [0.62, 0.9, 0.78, 0.5, 0.84, 0.66];
/// Height of one suggested prompt and of one agent row in the empty state.
const PROMPT_H: i32 = 32;
const AGENT_H: i32 = 46;

/// Vertical plan of the empty state: whether the suggested prompts fit, and the total height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EmptyPlan {
    pub prompts: bool,
    pub total: i32,
}

/// Hero (disc, title, sub), the primary button, the prompts when there is room, the agents, Refresh.
pub(crate) fn empty_plan(body_h: i32, any: bool, agents: usize, prompts: usize) -> EmptyPlan {
    let hero = 48 + 16 + 24 + 32;
    let button = if any { 48 } else { 0 };
    let agents_h = 24 + agents as i32 * (AGENT_H + 6) + 12 + 30;
    let prompts_h = 24 + prompts as i32 * (PROMPT_H + 6) + 12;
    let base = hero + button + agents_h;
    let show = any && prompts > 0 && body_h >= base + prompts_h + 2 * ds::S5;
    EmptyPlan { prompts: show, total: base + if show { prompts_h } else { 0 } }
}

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

    /// Text color and wording of the status chip (the session shown).
    fn ai_status_look(&self) -> (String, Rgba) {
        let s = self.ai.cur().map_or(Status::Starting, Session::status);
        (s.label(), self.ai_status_color(s))
    }

    /// The hover / pressed tint of a button: glides in and out with the pointer, stronger while held.
    fn ai_tint(&self, cv: &mut Canvas, r: Rect, rad: i32, hit: AiHit, base: Rgba, max: u8) {
        let k = self.ai.motion.hot(hit);
        let down = k > 0.5 && self.ai.pressed == Some(hit);
        let a = super::motion::tint_alpha(k, down, max);
        if a > 0 {
            cv.fill_round(r, rad, base.with_alpha(a));
        }
    }

    /// A status dot centered on (cx, cy): solid, with a soft ring that breathes while the agent
    /// starts or works.
    fn ai_dot(&self, cv: &mut Canvas, (cx, cy): (i32, i32), r: i32, status: Status, col: Rgba) {
        if matches!(status, Status::Starting | Status::Working) {
            let p = self.ai.motion.pulse();
            let rr = r + (p * 4.0) as i32;
            cv.fill_round(Rect::new(cx - rr, cy - rr, 2 * rr, 2 * rr), rr, col.with_alpha(((1.0 - p) * 90.0) as u8));
        }
        cv.fill_round(Rect::new(cx - r, cy - r, 2 * r, 2 * r), r, col);
    }

    /// 28 px icon button: hover and pressed tints glide, a disabled one (no hit) is dimmed.
    fn ai_icon_btn(&mut self, cv: &mut Canvas, r: Rect, icon: &str, hit: Option<AiHit>) {
        let c = self.theme.chrome;
        if let Some(h) = hit {
            self.ai_tint(cv, r, ds::R_ROW, h, c.tab_active_fg, 0x1c);
        }
        let tint = if hit.is_some() { c.tab_inactive_fg } else { c.tab_inactive_fg.with_alpha(0x60) };
        self.tx.icon(cv, icon, r.x + (r.w - ds::ICON_M) / 2, r.y + (r.h - ds::ICON_M) / 2, ds::ICON_M, tint, r);
        if let Some(h) = hit {
            self.ai.hits.push((r, h));
        }
    }

    /// The "Git | AI" switch on top of the right-hand slot, and the AI panel when it is the one shown.
    /// Returns true while something in it is still moving (hover glow, pulse, slide, skeleton).
    pub fn draw_aipanel(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        // What the pointer was on in the last frame's layout drives this frame's hover glow.
        let (mx, my) = self.mouse;
        let hovered = if self.overlay.is_none() { self.ai.hit_at(mx, my).map(|(_, h)| h) } else { None };
        self.ai.hits.clear();
        let shown = self.gitpanel.is_some() || self.ai.open;
        let working = self.ai.sessions.iter().any(|s| matches!(s.status(), Status::Starting | Status::Working));
        let empty = self.ai.open && self.ai.sessions.is_empty();
        self.ai.motion.hover(hovered);
        self.ai.motion.step(dt, self.ai.open && working, empty);
        if !shown {
            return self.ai.motion.moving();
        }
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let slot = self.slot_rect();
        let strip = Rect::new(slot.x, slot.y, slot.w, SLOT_H);
        cv.fill(strip, c.sidebar_bg);
        cv.fill(Rect::new(strip.x, strip.y, 1, strip.h), c.border);
        cv.fill(Rect::new(strip.x + 1, strip.bottom() - 1, strip.w - 1, 1), c.border);
        let mut x = strip.x + 12;
        // On the AI switch while hidden: the most pressing session's color.
        let ai_dot = match self.ai.badge() {
            Some((_, s)) => Some(self.ai_status_color(s)),
            None => self.ai.cur().map(|_| self.ai_status_look().1),
        }
        .filter(|_| !self.ai.open);
        for (hit, icon, label, active) in
            [(AiHit::TabGit, "ui/git_graph", "Git", !self.ai.open), (AiHit::TabAi, "ui/sparkle", "AI", self.ai.open)]
        {
            let lw = self.tx.ui_width(label, active);
            let dot = hit == AiHit::TabAi && ai_dot.is_some();
            let w = 10 + 16 + 7 + lw + 12 + if dot { 12 } else { 0 };
            let r = Rect::new(x, strip.y + 6, w, 28);
            if active {
                self.dock_pill(cv, strip, r);
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
            return self.ai.motion.moving();
        }
        let close = Rect::new(strip.right() - 12 - 28, strip.y + 6, 28, 28);
        self.ai_icon_btn(cv, close, "ui/close", Some(AiHit::Close));

        let p = self.aipanel_rect();
        cv.fill(p, c.sidebar_bg);
        cv.fill(Rect::new(p.x, p.y, 1, p.h), c.border);
        // Resize handle: the left edge lights up under the pointer and while dragging.
        if self.ai.resizing || (self.overlay.is_none() && self.ai_on_edge(mx, my)) {
            cv.fill(
                Rect::new(slot.x - 1, slot.y, 3, slot.h),
                c.accent.with_alpha(if self.ai.resizing { 0xc0 } else { 0x70 }),
            );
        }
        let mut top = p.y;
        if !self.ai.sessions.is_empty() {
            self.ai_draw_tabs(cv, Rect::new(p.x + 1, p.y, p.w - 1, TABS_H));
            top += TABS_H;
            self.ai_draw_filter(cv, Rect::new(p.x + 1, top, p.w - 1, FILTER_H));
            top += FILTER_H;
        }
        self.ai_draw_header(cv, Rect::new(p.x + 1, top, p.w - 1, HEAD_H));
        let top = top + HEAD_H;
        let body = Rect::new(p.x + 1, top, p.w - 1, (p.bottom() - top).max(0));
        cv.fill(body, self.theme.background);
        cv.fill(Rect::new(body.x, body.y, body.w, 1), c.border);
        if !self.ai.sessions.is_empty() {
            self.ai_draw_term(cv, body);
        } else {
            self.ai_draw_empty(cv, body);
        }
        self.ai.motion.moving()
    }

    /// One tab per session (status dot, agent icon, name, close) and `+` for another one. A pill
    /// slides under the active tab when another one becomes active.
    fn ai_draw_tabs(&mut self, cv: &mut Canvas, r: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), c.border);
        let plus = Rect::new(r.right() - 8 - 28, r.y + 4, 28, 28);
        let left = r.x + 8;
        let avail = plus.x - 4 - left;
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
                (40 + self.tx.ui_width(&self.ai.sessions[i].name, unseen) + 26).min(220)
            })
            .collect();
        let total: i32 = natural.iter().sum::<i32>() + 2 * (n - 1);
        let squeezed = (total > avail).then(|| ((avail - 2 * (n - 1)) / n.max(1)).max(56));
        // Geometry first, so the sliding pill goes under every label.
        let mut geo: Vec<(usize, Rect)> = Vec::with_capacity(vis.len());
        let mut x = left;
        for (&i, &nat) in vis.iter().zip(natural.iter()) {
            let w = squeezed.unwrap_or(nat);
            if x + w > plus.x - 4 {
                // No room left: the rest stays reachable by "Next session waiting" and the menu.
                break;
            }
            geo.push((i, Rect::new(x, r.y + 4, w, 28)));
            x += w + 2;
        }
        let active_tab = geo.iter().find(|(i, _)| *i == self.ai.active).map(|(_, tr)| *tr);
        if let (Some(tr), Some(id)) = (active_tab, self.ai.cur().map(|s| s.id)) {
            let (ix, iw) = self.ai.motion.indicator(id, tr.x, tr.w);
            cv.fill_round(Rect::new(ix, tr.y, iw, tr.h), ds::R_ROW, strong.with_alpha(0x1e));
            cv.fill_round(Rect::new(ix + 10, tr.bottom() - 2, (iw - 20).max(0), 2), 1, c.accent);
        }
        let (mx, my) = self.mouse;
        for (i, tr) in geo {
            let active = i == self.ai.active;
            let s = &self.ai.sessions[i];
            let (status, unseen, icon, name) = (s.status(), s.track.unseen, s.agent.icon, s.name.clone());
            let hovered = tr.contains(mx, my) && self.overlay.is_none();
            if !active {
                self.ai_tint(cv, tr, ds::R_ROW, AiHit::Session(i), strong, 0x14);
            }
            let dot = self.ai_status_color(status);
            self.ai_dot(cv, (tr.x + 12, tr.y + 14), 3, status, dot);
            let col = if active || unseen { strong } else { dim };
            self.tx.icon(cv, icon, tr.x + 21, tr.y + 7, ds::ICON_S, col, tr);
            // The close button's room is always reserved: nothing shifts when it appears.
            let name_w = tr.w - 41 - 24;
            if name_w > 8 {
                let label = crate::ui::fit_ellipsis(&mut self.tx, &name, name_w, unseen);
                let clip = Rect::new(tr.x, tr.y, tr.w - 22, tr.h);
                self.tx.draw_ui(cv, tr.x + 41, tr.y + 5, &label, col, clip, unseen);
            }
            self.ai.hits.push((tr, AiHit::Session(i)));
            let k = self.ai.motion.hot(AiHit::Session(i)).max(self.ai.motion.hot(AiHit::CloseSession(i)));
            if active || hovered || k > 0.05 {
                let cb = Rect::new(tr.right() - 22, tr.y + 6, 16, 16);
                self.ai_tint(cv, cb, 4, AiHit::CloseSession(i), strong, 0x28);
                self.tx.icon(cv, "ui/close", cb.x + 2, cb.y + 2, 12, fade(dim, if active { 1.0 } else { k }), cb);
                self.ai.hits.push((cb, AiHit::CloseSession(i)));
            }
        }
        self.ai_icon_btn(cv, plus, "ui/plus", Some(AiHit::New));
    }

    /// The filter field under the tabs: narrows them by name or agent.
    fn ai_draw_filter(&mut self, cv: &mut Canvas, r: Rect) {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        cv.fill(Rect::new(r.x, r.bottom() - 1, r.w, 1), c.border);
        let f = Rect::new(r.x + 8, r.y + 4, r.w - 16, r.h - 9);
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
            self.ai_tint(cv, b, 5, AiHit::FilterClear, c.tab_active_fg, 0x28);
            self.tx.icon(cv, "ui/close", b.x + 4, b.y + 4, 12, dim, b);
            self.ai.hits.push((b, AiHit::FilterClear));
        }
    }

    /// Agent picker, status chip, Restart / Stop.
    fn ai_draw_header(&mut self, cv: &mut Canvas, h: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (icon, name) = self.ai.agent().map_or(("ui/sparkle", "Choose agent"), |a| (a.icon, a.name));
        let nw = self.tx.ui_width(name, true);
        let pill = Rect::new(h.x + 11, h.y + 8, 10 + 16 + 8 + nw + 8 + 12 + 10, 28);
        cv.fill_round(pill, ds::R_ROW, strong.with_alpha(0x0e));
        self.ai_tint(cv, pill, ds::R_ROW, AiHit::Agent, strong, 0x14);
        cv.ring(pill, ds::R_ROW, c.border);
        self.tx.icon(cv, icon, pill.x + 10, pill.y + 6, 16, strong, pill);
        self.tx.draw_ui(cv, pill.x + 34, pill.y + 5, name, strong, pill, true);
        self.tx.icon(cv, "ui/chevron_down", pill.right() - 22, pill.y + 8, 12, dim, pill);
        self.ai.hits.push((pill, AiHit::Agent));

        let alive = self.ai.term().is_some_and(|t| t.alive);
        let has_agent = self.ai.agent().is_some_and(|a| self.ai.path_of(a).is_some());
        let mut right = h.right() - 11;
        for (icon, hit, on) in
            [("ui/stop", AiHit::Stop, alive), ("ui/rotate_cw", AiHit::Restart, has_agent && self.ai.cur().is_some())]
        {
            let b = Rect::new(right - 28, h.y + 8, 28, 28);
            self.ai_icon_btn(cv, b, icon, on.then_some(hit));
            right -= 32;
        }
        if self.ai.sessions.is_empty() {
            return;
        }
        right = self.ai_draw_bridge(cv, h, right);
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
        cv.fill_round(chip, ds::R_ROW, col.with_alpha(0x1a).over(bg));
        let (dx, dy) = (chip.x + if w == 24 { 8 } else { 10 }, chip.y + 7);
        let status = self.ai.cur().map_or(Status::Starting, Session::status);
        if status == Status::Working {
            // Turns as output arrives (each frame), quiet otherwise: no timer of its own.
            let t = self.ai.term().map_or(0.0, |t| t.started.elapsed().as_secs_f32());
            crate::app_tests::spinner_t(cv, dx + 4, dy + 4, 4, col, col.with_alpha(0x1a).over(bg), t, chip);
        } else {
            self.ai_dot(cv, (dx + 4, dy + 4), 4, status, col);
        }
        if w == full {
            self.tx.draw_ui_sized(cv, dx + 14, chip.y + 3, &label, col, chip, false, SMALL);
        }
        self.ai.hits.push((chip, AiHit::Status));
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
            cv.fill_round(r, ds::R_ROW, c.accent.with_alpha(0x2a).over(c.sidebar_bg));
            self.ai_tint(cv, r, ds::R_ROW, AiHit::Diffs, c.accent, 0x20);
            self.tx.icon(cv, "ui/file_diff", r.x + 8, r.y + 4, 14, c.accent_text, r);
            self.tx.draw_ui_sized(cv, r.x + 27, r.y + 3, &label, c.accent_text, r, true, SMALL);
            self.ai.hits.push((r, AiHit::Diffs));
            right = r.x - 4;
        }
        let b = Rect::new(right - 28, h.y + 8, 28, 28);
        self.ai_tint(cv, b, ds::R_ROW, AiHit::Ide, c.tab_active_fg, 0x1c);
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

    /// The agent's terminal inside a rounded frame, plus the "starting" skeleton while it boots and
    /// the "exited" card when the process has ended.
    fn ai_draw_term(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let inner = Rect::new(body.x + PAD, body.y + PAD, body.w - 2 * PAD, (body.h - 2 * PAD).max(0));
        let (rows, cols) = crate::termview::fit(self.tx.cell_w, inner.w, inner.h);
        let focused = self.ai.focused && !self.ai.filter_focus && self.overlay.is_none();
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        let look = self.term_look(Which::Ai, focused, (inner.x, inner.y));
        let clip = Rect::new(body.x, body.y + 1, body.w, body.h - 1);
        // The terminal sits on a rounded surface inset from the panel edges.
        let card = Rect::new(inner.x - 6, inner.y - 6, inner.w + 12, inner.h + 12);
        cv.fill(Rect::new(body.x, body.y + 1, body.w, (body.h - 1).max(0)), c.sidebar_bg);
        cv.card(card, ds::R_CARD, self.theme.background, c.border);
        let booting = self
            .ai
            .cur()
            .is_some_and(|s| s.term.alive && s.status() == Status::Starting && s.track.last_activity.is_none());
        self.ai.motion.set_skeleton(booting);
        self.ai.motion.set_focus(focused);
        let Some(t) = self.ai.term_mut() else { return };
        t.resize(rows, cols);
        t.grid.colors = (fg, bg);
        t.set_focus(focused);
        crate::termview::draw(&mut self.tx, cv, t, inner.x, inner.y, clip, &look);
        let exited = !t.alive;
        // Focus: the frame's border turns accent, with a faint halo around it.
        let f = self.ai.motion.focus();
        if f > 0.01 {
            cv.ring(card.inset(-1), ds::R_CARD + 1, c.accent.with_alpha((40.0 * f) as u8));
            cv.ring(card, ds::R_CARD, c.accent.with_alpha((112.0 * f) as u8));
        }
        let sk = self.ai.motion.skeleton();
        if sk > 0.01 {
            self.ai_draw_skeleton(cv, card, sk);
        }
        if exited {
            self.ai_draw_exited(cv, body);
        }
    }

    /// "Starting <agent>…" with shimmering placeholder lines, while the process has printed nothing.
    fn ai_draw_skeleton(&mut self, cv: &mut Canvas, card: Rect, k: f32) {
        let c = self.theme.chrome;
        let Some((icon, name, cwd)) = self.ai.cur().map(|s| (s.agent.icon, s.agent.name, s.cwd.clone())) else {
            return;
        };
        let (x, w) = (card.x + ds::S5, card.w - 2 * ds::S5);
        if w < 80 {
            return;
        }
        let mut y = card.y + ds::S5;
        self.tx.icon(cv, icon, x, y, ds::ICON_M, fade(c.tab_active_fg, k), card);
        let title = crate::ui::fit_ellipsis(&mut self.tx, &format!("Starting {name}…"), w - 26, true);
        self.tx.draw_ui(cv, x + 26, y - 1, &title, fade(c.tab_active_fg, k), card, true);
        y += 24;
        let sub = crate::ui::fit_ellipsis(&mut self.tx, &format!("in {}", crate::app_topbar::tilde(&cwd)), w, false);
        self.tx.draw_ui_sized(cv, x, y, &sub, fade(c.tab_inactive_fg, k), card, false, SMALL);
        y += 32;
        for (i, frac) in BARS.iter().enumerate() {
            if y + 10 > card.bottom() - ds::S4 {
                break;
            }
            let a = (16.0 + 26.0 * self.ai.motion.wave(i)) * k;
            let bar = Rect::new(x, y, (w as f32 * frac) as i32, 10);
            cv.fill_round_clip(bar, 5, c.tab_active_fg.with_alpha(a as u8), card);
            y += 22;
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
        cv.card(card, ds::R_CARD, c.widget_bg, c.border);
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
        cv.fill_round(b, ds::R_ROW, c.accent);
        self.ai_tint(cv, b, ds::R_ROW, AiHit::Restart, c.on_accent, 0x30);
        self.tx.draw_ui(cv, b.x + 14, b.y + 6, label, c.on_accent, b, true);
        self.ai.hits.push((b, AiHit::Restart));
        let pick = "Choose agent";
        let pw = self.tx.ui_width(pick, false) + 24;
        let pb = Rect::new(b.right() + 8, b.y, pw, 30);
        if pb.right() <= card.right() - 12 {
            self.ai_tint(cv, pb, ds::R_ROW, AiHit::Agent, c.tab_active_fg, 0x14);
            cv.ring(pb, ds::R_ROW, c.border);
            self.tx.draw_ui(cv, pb.x + 12, pb.y + 6, pick, c.tab_active_fg, pb, false);
            self.ai.hits.push((pb, AiHit::Agent));
        }
    }

    /// Entrance progress of item `n` of the empty state and how far it still has to rise.
    fn ai_enter(&self, n: usize) -> (f32, i32) {
        let e = self.ai.motion.enter(n);
        (e, ((1.0 - e) * RISE) as i32)
    }

    /// Nothing running: a calm hero with one primary action, suggested prompts that are typed into
    /// the agent, the agents Lumen knows (installed ones start on click, the others copy their
    /// install command) and Refresh. The items rise and fade in one after the other.
    fn ai_draw_empty(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let any = self.ai.detected.as_ref().is_some_and(|d| d.iter().any(|(_, p)| p.is_some()));
        let plan = empty_plan(body.h, any, AGENTS.len(), PROMPTS.len());
        let w = (body.w - 48).clamp(120, 380);
        let x = body.x + (body.w - w) / 2;
        let mut y = body.y + ((body.h - plan.total) / 2).max(ds::S5);
        let mut n = 0;
        // Icon in a soft accent disc.
        let (e, dy) = self.ai_enter(n);
        n += 1;
        let disc = Rect::new(body.x + (body.w - 48) / 2, y + dy, 48, 48);
        cv.fill_round(disc, 24, fade(c.accent.with_alpha(0x1c), e));
        self.tx.icon(cv, "ui/sparkle", disc.x + 14, disc.y + 14, ds::ICON_L, fade(c.accent_text, e), disc);
        y += 48 + 16;
        let (e, dy) = self.ai_enter(n);
        n += 1;
        let title = if any { "Start an agent" } else { "No agent CLI found" };
        let tw = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, body.x + (body.w - tw) / 2, y + dy, title, fade(strong, e), body, true);
        y += 24;
        let (e, dy) = self.ai_enter(n);
        n += 1;
        let sub = if any {
            "It works on this workspace with you, in a real terminal."
        } else {
            "Install one, then Refresh. It runs here with your own login."
        };
        let sub = crate::ui::fit_ellipsis(&mut self.tx, sub, w, false);
        let sw = self.tx.ui_width_sized(&sub, false, SMALL);
        self.tx.draw_ui_sized(cv, body.x + (body.w - sw) / 2, y + dy, &sub, fade(dim, e), body, false, SMALL);
        y += 32;
        if any {
            let (e, dy) = self.ai_enter(n);
            n += 1;
            let label = "New Agent";
            let bw = 14 + 16 + 6 + self.tx.ui_width(label, true) + 16;
            let b = Rect::new(body.x + (body.w - bw) / 2, y + dy, bw, 32);
            cv.fill_round(b, 10, fade(c.accent, e));
            self.ai_tint(cv, b, 10, AiHit::New, c.on_accent, 0x30);
            self.tx.icon(cv, "ui/plus", b.x + 14, b.y + 8, 16, fade(c.on_accent, e), b);
            self.tx.draw_ui(cv, b.x + 36, b.y + 7, label, fade(c.on_accent, e), b, true);
            self.ai.hits.push((Rect::new(b.x, y, b.w, b.h), AiHit::New));
            y += 48;
        }
        if plan.prompts {
            let (e, dy) = self.ai_enter(n);
            n += 1;
            self.tx.draw_ui_sized(cv, x, y + dy, "Try asking", fade(dim, e), body, true, SMALL);
            y += 24;
            for (i, (icon, label, _)) in PROMPTS.iter().enumerate() {
                let (e, dy) = self.ai_enter(n);
                n += 1;
                let r = Rect::new(x, y + dy, w, PROMPT_H);
                cv.card(r, ds::R_ROW, fade(c.sidebar_bg, e), fade(c.border, e));
                self.ai_tint(cv, r, ds::R_ROW, AiHit::Prompt(i), strong, 0x14);
                self.tx.icon(cv, icon, r.x + 12, r.y + 9, ds::ICON_S, fade(dim, e), r);
                let text = crate::ui::fit_ellipsis(&mut self.tx, label, w - 34 - 32, false);
                self.tx.draw_ui(cv, r.x + 34, r.y + 7, &text, fade(strong, e), r, false);
                let k = 0.4 + 0.6 * self.ai.motion.hot(AiHit::Prompt(i));
                self.tx.icon(cv, "ui/arrow_up_right", r.right() - 28, r.y + 9, ds::ICON_S, fade(dim, e * k), r);
                self.ai.hits.push((Rect::new(x, y, w, PROMPT_H), AiHit::Prompt(i)));
                y += PROMPT_H + 6;
            }
            y += 12;
        }
        let (e, dy) = self.ai_enter(n);
        n += 1;
        self.tx.draw_ui_sized(cv, x, y + dy, "Agents on this machine", fade(dim, e), body, true, SMALL);
        y += 24;
        for (i, a) in AGENTS.iter().enumerate() {
            let (e, dy) = self.ai_enter(n);
            n += 1;
            let r = Rect::new(x, y + dy, w, AGENT_H);
            let installed = self.ai.path_of(a).is_some();
            cv.card(r, ds::R_ROW, fade(c.sidebar_bg, e), fade(c.border, e));
            self.ai_tint(cv, r, ds::R_ROW, AiHit::Install(i), strong, 0x14);
            let tint = fade(if installed { strong } else { dim }, e);
            self.tx.icon(cv, a.icon, r.x + 14, r.y + 15, 16, tint, r);
            self.tx.draw_ui(cv, r.x + 42, r.y + 5, a.name, tint, r, true);
            let (line, col) = if installed { ("Installed — click to start", c.git_added) } else { (a.install, dim) };
            let line = crate::ui::fit_ellipsis(&mut self.tx, line, w - 56 - 22, false);
            self.tx.draw_ui_sized(cv, r.x + 42, r.y + 24, &line, fade(col, e), r, false, SMALL);
            let glyph = if installed { "ui/play_outlined" } else { "ui/copy" };
            self.tx.icon(cv, glyph, r.right() - 30, r.y + 15, 16, fade(dim, e), r);
            self.ai.hits.push((Rect::new(x, y, w, AGENT_H), AiHit::Install(i)));
            y += AGENT_H + 6;
        }
        y += 12;
        let (e, dy) = self.ai_enter(n);
        let label = "Refresh";
        let bw = 12 + 16 + 6 + self.tx.ui_width(label, false) + 14;
        let b = Rect::new(body.x + (body.w - bw) / 2, y + dy, bw, 30);
        self.ai_tint(cv, b, ds::R_ROW, AiHit::Refresh, strong, 0x14);
        cv.ring(b, ds::R_ROW, fade(c.border, e));
        self.tx.icon(cv, "ui/rotate_cw", b.x + 12, b.y + 7, 16, fade(strong, e), b);
        self.tx.draw_ui(cv, b.x + 34, b.y + 6, label, fade(strong, e), b, false);
        self.ai.hits.push((Rect::new(b.x, y, b.w, b.h), AiHit::Refresh));
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

#[cfg(test)]
mod plan_tests {
    use super::*;

    #[test]
    fn prompts_only_show_when_they_fit_and_an_agent_exists() {
        let tall = empty_plan(900, true, 3, 4);
        assert!(tall.prompts);
        let short = empty_plan(400, true, 3, 4);
        assert!(!short.prompts);
        assert!(tall.total > short.total);
        assert!(!empty_plan(900, false, 3, 4).prompts, "nothing to type into without an installed agent");
        assert!(!empty_plan(900, true, 3, 0).prompts);
    }

    #[test]
    fn plan_height_matches_the_items() {
        // hero + button + agents (header, rows, gap, refresh), no prompts.
        let p = empty_plan(0, true, 3, 4);
        assert_eq!(p.total, 120 + 48 + 24 + 3 * (AGENT_H + 6) + 12 + 30);
        assert_eq!(empty_plan(0, false, 3, 4).total, p.total - 48);
    }
}
