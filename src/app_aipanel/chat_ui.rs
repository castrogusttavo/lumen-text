//! The native chat's drawing, keys and mouse (the model is `chat`, the state `chat_tab`). Lean on
//! purpose: the transcript is laid out only when the conversation or the width changes, only the
//! rows in view are drawn, and nothing animates unless the agent is working.

use super::{
    backend::{Kind, Spawn},
    chat::{self, Block, PermMode, Phase, Role, ToolStatus},
    chat_tab::{self, ChatHit, ChatTab, DropKind, Row, RowKind},
    mention,
    menu::{self, Entry, MenuKind},
    slash::{self, Handling, Native},
    *,
};
use crate::{theme::Rgba, ui::ui_line};

/// Outer margin of the transcript and the composer.
const MARGIN: i32 = 22;
/// Gap between two rows.
const GAP: i32 = 10;
/// Most lines the composer grows to.
const MAX_INPUT_LINES: usize = 8;

fn lh() -> i32 {
    ui_line().round() as i32 + 6
}

impl App {
    /// The session shown has a native chat.
    pub fn ai_cur_is_chat(&self) -> bool {
        self.ai.cur().is_some_and(|s| s.chat.is_some())
    }

    /// Start `a` as a native chat tab (in `cwd`, the project by default). False when it has no
    /// structured mode or could not start.
    pub fn ai_new_chat(&mut self, a: &'static Agent, cwd: Option<PathBuf>) -> bool {
        let Some(kind) = Kind::of(a.id) else { return false };
        self.ai_detect(false);
        let Some(exe) = self.ai.path_of(a) else { return false };
        self.ai.last_agent = Some(a);
        let cwd = cwd.unwrap_or_else(|| self.root.clone());
        let (program, prefix) = command_for(&exe);
        let path = path_with(exe.parent(), std::env::var_os("PATH").as_deref());
        // A CLI started from inside another agent session must not think it is nested.
        let mut env: Vec<(String, Option<String>)> =
            ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SSE_PORT"]
                .iter()
                .map(|k| (k.to_string(), None))
                .collect();
        env.push(("TERM_PROGRAM".into(), Some("lumen-text".into())));
        if let Some(p) = path {
            env.push(("PATH".into(), Some(p)));
        }
        let sp = Spawn { program, prefix, cwd: cwd.clone(), env };
        let id = self.ai.new_id();
        let taken: Vec<&str> = self.ai.sessions.iter().map(|s| s.name.as_str()).collect();
        let name = format!("{} chat", session_name(a, &cwd, &self.root, &taken));
        if let Some(t) = self.ai.term_mut() {
            t.set_focus(false);
        }
        let tab = Box::new(ChatTab::new(kind, sp, self.wake()));
        self.ai.sessions.push(Session {
            id,
            agent: a,
            name,
            cwd,
            term: None,
            chat: Some(tab),
            track: Track::default(),
        });
        self.ai.active = self.ai.sessions.len() - 1;
        self.ai.sync_groups();
        self.ai.root = self.root.clone();
        self.request_redraw();
        true
    }

    /// An agent picked for a new chat session.
    pub fn ai_pick_new_chat(&mut self, id: &str) {
        let Some(a) = agent(id) else { return };
        self.ai_detect(false);
        if self.ai.path_of(a).is_none() {
            return self.ai_not_installed(a);
        }
        if self.ai.root != self.root {
            self.ai_close_all();
        }
        if self.ai_new_chat(a, None) {
            self.ai_show();
        }
    }

    /// Run `f` on the chat of the session shown (taken out of the session for the call, so `self`
    /// stays free for the drawing context).
    fn with_chat<R>(&mut self, f: impl FnOnce(&mut Self, &mut ChatTab) -> R) -> Option<R> {
        let mut tab = self.ai.cur_mut().and_then(|s| s.chat.take())?;
        let r = f(self, &mut tab);
        if let Some(s) = self.ai.cur_mut() {
            s.chat = Some(tab);
        }
        Some(r)
    }

    // ───────────── layout ─────────────

    /// Wrap the conversation to `w` px: one row per block, stacked top to bottom.
    fn chat_layout(&mut self, tab: &mut ChatTab, w: i32) {
        if tab.laid == Some((tab.chat.rev, w)) {
            return;
        }
        let text_w = (w - 2 * MARGIN).max(80);
        let tx = &mut self.tx;
        let mut rows: Vec<Row> = Vec::new();
        let mut y = MARGIN / 2;
        let mut push = |kind: RowKind, lines: Vec<String>, tool: Option<String>, h: i32, rows: &mut Vec<Row>| {
            rows.push(Row { kind, lines, tool, y, h });
            y += h + GAP;
        };
        for m in &tab.chat.messages {
            for b in &m.blocks {
                match (m.role, b) {
                    (Role::User, Block::Text(t)) => {
                        let bw = (text_w * 4 / 5).min(560);
                        let lines = chat::wrap_text(t, (bw - 28) as f32, &mut |s| tx.ui_char_xs(s, false, None));
                        let h = lines.len() as i32 * lh() + 20;
                        push(RowKind::User, lines, None, h, &mut rows);
                    }
                    (_, Block::Text(t)) => {
                        let lines = chat::wrap_text(t, text_w as f32, &mut |s| tx.ui_char_xs(s, false, None));
                        let h = lines.len() as i32 * lh();
                        push(RowKind::Text, lines, None, h, &mut rows);
                    }
                    (_, Block::Thinking(t)) => {
                        let first = t.lines().find(|l| !l.trim().is_empty()).unwrap_or("Thinking").trim().to_string();
                        push(RowKind::Thinking, vec![first], None, lh(), &mut rows);
                    }
                    (_, Block::Tool(tc)) => {
                        push(
                            RowKind::Tool,
                            vec![chat::tool_summary(&tc.name, &tc.input)],
                            Some(tc.id.clone()),
                            lh() + 4,
                            &mut rows,
                        );
                    }
                    (_, Block::Error(t)) => {
                        let lines = chat::wrap_text(t, text_w as f32, &mut |s| tx.ui_char_xs(s, false, None));
                        let h = lines.len() as i32 * lh();
                        push(RowKind::Error, lines, None, h, &mut rows);
                    }
                    (_, Block::Note(t)) => {
                        let lines = chat::wrap_text(t, text_w as f32, &mut |s| tx.ui_char_xs(s, false, None));
                        let h = lines.len() as i32 * lh();
                        push(RowKind::Note, lines, None, h, &mut rows);
                    }
                }
            }
        }
        // Permission requests wait at the end, then the "working" line.
        for p in &tab.chat.perms {
            let summary = chat::tool_summary(&p.name, &p.input);
            push(RowKind::Perm, vec![p.name.clone(), summary], Some(p.request_id.clone()), 78, &mut rows);
        }
        if tab.chat.working() {
            push(RowKind::Working, Vec::new(), None, lh() + 6, &mut rows);
        }
        tab.total_h = y + MARGIN / 2;
        tab.rows = rows;
        tab.laid = Some((tab.chat.rev, w));
    }

    // ───────────── drawing ─────────────

    /// A chat tab: the transcript over the composer.
    pub(super) fn ai_draw_chat(&mut self, cv: &mut Canvas, body: Rect, is_focus: bool) {
        self.with_chat(|app, tab| app.chat_paint(cv, tab, body, is_focus));
    }

    fn chat_paint(&mut self, cv: &mut Canvas, tab: &mut ChatTab, body: Rect, is_focus: bool) {
        let c = self.theme.chrome;
        let (fg, bg) = (self.theme.foreground, self.theme.background);
        let dim = c.tab_inactive_fg;
        cv.fill(body, bg);
        tab.hits.clear();
        let focused = is_focus && self.ai.focused && tab.focused && self.overlay.is_none();

        // Composer geometry.
        let cw = (body.w - 2 * 14).max(120);
        let inner_w = cw - 32;
        {
            let tx = &mut self.tx;
            tab.input.layout(inner_w as f32, &mut |s: &str| tx.ui_char_xs(s, false, None));
        }
        let lines = tab.input.line_count().clamp(1, MAX_INPUT_LINES) as i32;
        let text_h = lines * lh();
        let box_h = 14 + text_h + 8 + 36 + 6;
        let input_box = Rect::new(body.x + 14, body.bottom() - 14 - box_h, cw, box_h);
        tab.input.set_view(lh() as f32, text_h as f32);
        tab.input_box = input_box;

        // Transcript.
        let view = Rect::new(body.x, body.y, body.w, (input_box.y - 8 - body.y).max(0));
        tab.view = view;
        self.chat_layout(tab, view.w);
        tab.max_scroll = (tab.total_h - view.h).max(0) as f64;
        if tab.follow {
            tab.scroll = tab.max_scroll;
        }
        tab.scroll = tab.scroll.clamp(0.0, tab.max_scroll);
        let scroll = tab.scroll.round() as i32;
        let clip = view;
        let ty = ((lh() as f32 - ui_line()) / 2.0).max(0.0).round() as i32;
        let text_x = view.x + MARGIN;
        let mut any = false;
        for (i, r) in tab.rows.iter().enumerate() {
            let top = view.y + r.y - scroll;
            if top + r.h < view.y || top > view.bottom() {
                continue;
            }
            any = true;
            match r.kind {
                RowKind::User => {
                    let bw = (view.w - 2 * MARGIN) * 4 / 5;
                    let bw = bw.min(560);
                    let widest = r.lines.iter().map(|l| self.tx.ui_width(l, false)).max().unwrap_or(0);
                    let w = (widest + 28).min(bw).max(48);
                    let b = Rect::new(view.right() - MARGIN - w, top, w, r.h);
                    cv.fill_round_clip(b, 14, fg.with_alpha(0x14).over(bg), clip);
                    for (k, l) in r.lines.iter().enumerate() {
                        self.tx.draw_ui(cv, b.x + 14, top + 10 + k as i32 * lh() + ty, l, fg, clip, false);
                    }
                }
                RowKind::Text | RowKind::Error | RowKind::Note => {
                    let col = match r.kind {
                        RowKind::Error => c.error,
                        RowKind::Note => dim,
                        _ => fg,
                    };
                    for (k, l) in r.lines.iter().enumerate() {
                        self.tx.draw_ui(cv, text_x, top + k as i32 * lh() + ty, l, col, clip, false);
                    }
                }
                RowKind::Thinking => {
                    self.tx.icon(cv, "ui/tool_think", text_x, top + (lh() - 14) / 2, 14, dim, clip);
                    let t = crate::ui::fit_ellipsis(&mut self.tx, &r.lines[0], view.w - 2 * MARGIN - 24, false);
                    self.tx.draw_ui(cv, text_x + 22, top + ty, &t, dim, clip, false);
                }
                RowKind::Tool => {
                    let tc = r.tool.as_deref().and_then(|id| {
                        tab.chat.messages.iter().flat_map(|m| m.blocks.iter()).find_map(|b| match b {
                            Block::Tool(t) if t.id == id => Some(t),
                            _ => None,
                        })
                    });
                    let Some(tc) = tc else { continue };
                    let running = matches!(tc.status, ToolStatus::Running | ToolStatus::Asking);
                    let col = match tc.status {
                        ToolStatus::Error => c.error,
                        ToolStatus::Denied => dim,
                        _ => dim,
                    };
                    let mid = top + (r.h - 14) / 2;
                    if running {
                        let t = tab.started.elapsed().as_secs_f32();
                        crate::app_tests::spinner_t(cv, text_x + 7, top + r.h / 2, 6, dim, bg, t, clip);
                    } else {
                        self.tx.icon(cv, chat::tool_icon(&tc.name), text_x, mid, 14, col, clip);
                    }
                    let verb = chat::tool_verb(&tc.name, running);
                    self.tx.draw_ui(cv, text_x + 22, top + 2 + ty, verb, dim, clip, false);
                    let vw = self.tx.ui_width(verb, false);
                    let room = (view.w - 2 * MARGIN - 22 - vw - 10).max(20);
                    let s = crate::ui::fit_ellipsis(&mut self.tx, &r.lines[0], room, false);
                    self.tx.draw_ui(cv, text_x + 22 + vw + 8, top + 2 + ty, &s, fg, clip, false);
                }
                RowKind::Perm => {
                    let card = Rect::new(view.x + MARGIN, top, view.w - 2 * MARGIN, r.h);
                    cv.card(card, 12, c.widget_bg, c.border);
                    let title = format!("Allow {}?", r.lines[0]);
                    self.tx.draw_ui(cv, card.x + 14, card.y + 10, &title, fg, card, true);
                    let s = crate::ui::fit_ellipsis(&mut self.tx, &r.lines[1], card.w - 28, false);
                    self.tx.draw_ui(cv, card.x + 14, card.y + 10 + lh(), &s, dim, card, false);
                    let pi = tab.chat.perms.iter().position(|p| Some(&p.request_id) == r.tool.as_ref());
                    if let Some(pi) = pi {
                        let by = card.bottom() - 34;
                        let allow = Rect::new(card.x + 14, by, 76, 26);
                        let deny = Rect::new(allow.right() + 8, by, 66, 26);
                        cv.fill_round(allow, 8, c.accent);
                        self.tx.draw_ui(cv, allow.x + 16, allow.y + 4, "Allow", c.on_accent, allow, true);
                        cv.ring(deny, 8, c.border);
                        self.tx.draw_ui(cv, deny.x + 16, deny.y + 4, "Deny", fg, deny, false);
                        tab.hits.push((allow, ChatHit::Allow(pi)));
                        tab.hits.push((deny, ChatHit::Deny(pi)));
                    }
                }
                RowKind::Working => {
                    let secs = tab.chat.turn_started.map_or(0, |t| t.elapsed().as_secs());
                    crate::app_tests::spinner_t(
                        cv,
                        text_x + 7,
                        top + r.h / 2,
                        6,
                        c.accent_text,
                        bg,
                        tab.started.elapsed().as_secs_f32(),
                        clip,
                    );
                    let label = if tab.chat.phase == Phase::Waiting {
                        "Waiting for you".to_string()
                    } else {
                        format!("Working for {secs}s")
                    };
                    self.tx.draw_ui(cv, text_x + 22, top + 3 + ty, &label, dim, clip, false);
                }
            }
            let _ = i;
        }
        if !any && tab.chat.messages.is_empty() {
            let name = self.ai.cur().map_or("the agent", |s| s.agent.name);
            let msg = format!("Ask {name} anything about this project");
            let w = self.tx.ui_width(&msg, false);
            self.tx.draw_ui(cv, view.x + (view.w - w) / 2, view.y + view.h / 2 - 9, &msg, dim, view, false);
        }

        // Composer.
        cv.card(
            input_box,
            16,
            fg.with_alpha(0x0a).over(bg),
            if focused { c.accent.with_alpha(0x80) } else { c.border },
        );
        tab.hits.push((input_box, ChatHit::Input));
        let (ox, oy) = (input_box.x + 16, input_box.y + 14);
        let tclip = Rect::new(input_box.x + 4, input_box.y + 4, input_box.w - 8, text_h + 20);
        if tab.input.text().is_empty() {
            let ph = crate::ui::fit_ellipsis(
                &mut self.tx,
                "Ask anything, @ to mention files, / for commands",
                inner_w,
                false,
            );
            self.tx.draw_ui(cv, ox, oy + ty, &ph, dim, tclip, false);
        }
        let scroll_y = tab.input.scroll.round() as i32;
        let row_y = |k: usize| oy + k as i32 * lh() - scroll_y;
        let first = (scroll_y / lh()).max(0) as usize;
        let last = ((scroll_y + text_h) / lh() + 1).max(0) as usize;
        for (k, x0, x1) in tab.input.sel_spans() {
            if (first..=last).contains(&k) {
                let (a, z) = (x0.round() as i32, x1.round() as i32);
                cv.fill_clip(Rect::new(ox + a, row_y(k), (z - a).max(1), lh()), self.theme.selection, tclip);
            }
        }
        for (k, l) in tab.input.lines().iter().enumerate().take(last + 1).skip(first) {
            let t = tab.input.line_text(l);
            self.tx.draw_ui(cv, ox, row_y(k) + ty, &t, fg, tclip, false);
        }
        if focused {
            let (k, x) = tab.input.caret();
            let cx = ox + (x.round() as i32).min(inner_w + 4);
            cv.fill_clip(Rect::new(cx, row_y(k) + 1, 1, lh() - 2), fg, tclip);
        }
        // Toolbar: the permission mode (Claude Code) on the left; model, effort and send / stop on the
        // right. Each setting is a small button that opens a dropdown over itself.
        let by = input_box.bottom() - 6 - 36 + 2;
        let tool_clip = input_box;
        if tab.backend.kind == Kind::Claude {
            let mode = tab.backend.mode;
            let col = if mode == chat::PermMode::Bypass { c.warning } else { dim };
            let w = 8 + 14 + 6 + self.tx.ui_width(mode.label(), false) + 12;
            let pill = Rect::new(input_box.x + 12, by + 3, w, 26);
            self.chat_pill(
                cv,
                pill,
                Some(mode.icon()),
                mode.label(),
                col,
                tab.dropdown.as_ref().is_some_and(|d| d.kind == DropKind::Mode),
                tool_clip,
            );
            tab.hits.push((pill, ChatHit::Mode));
            tab.anchors[2] = pill;
        }
        let send = Rect::new(input_box.right() - 12 - 32, by, 32, 32);
        let working = tab.chat.working();
        let has_text = !tab.input.text().trim().is_empty();
        let sc = if working || has_text { c.accent } else { fg.with_alpha(0x22) };
        cv.fill_round(send, 16, sc);
        let (icon, hit) =
            if working { ("hugeicons/stop", ChatHit::Stop) } else { ("hugeicons/arrow_up", ChatHit::Send) };
        self.tx.icon(cv, icon, send.x + 8, send.y + 8, 16, if working || has_text { c.on_accent } else { dim }, send);
        tab.hits.push((send, hit));
        let mut right = send.x - 6;
        let model = tab.model_now().map_or_else(|| tab.backend.kind.name().to_string(), |m| chat::short_model(&m));
        let mw = 12 + self.tx.ui_width(&model, false) + 6 + 12 + 8;
        let mr = Rect::new(right - mw, by + 3, mw, 26);
        self.chat_pill(
            cv,
            mr,
            None,
            &model,
            dim,
            tab.dropdown.as_ref().is_some_and(|d| d.kind == DropKind::Model),
            tool_clip,
        );
        tab.hits.push((mr, ChatHit::Model));
        tab.anchors[0] = mr;
        right = mr.x - 2;
        if !chat_tab::efforts(tab.backend.kind).is_empty() {
            let label = tab.backend.effort.clone().unwrap_or_else(|| "effort".to_string());
            let ew = 8 + 14 + 6 + self.tx.ui_width(&label, false) + 6 + 12 + 8;
            let er = Rect::new(right - ew, by + 3, ew, 26);
            self.chat_pill(
                cv,
                er,
                Some("ui/tool_think"),
                &label,
                dim,
                tab.dropdown.as_ref().is_some_and(|d| d.kind == DropKind::Effort),
                tool_clip,
            );
            tab.hits.push((er, ChatHit::Effort));
            tab.anchors[1] = er;
        }
        self.chat_paint_menu(cv, tab, input_box);
        self.chat_paint_dropdown(cv, tab, input_box);
    }

    /// A toolbar button: optional icon, label and a chevron; tinted under the pointer or while its
    /// dropdown is open.
    fn chat_pill(
        &mut self,
        cv: &mut Canvas,
        r: Rect,
        icon: Option<&str>,
        label: &str,
        col: Rgba,
        open: bool,
        clip: Rect,
    ) {
        let fg = self.theme.foreground;
        let hot = self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1);
        if hot || open {
            cv.fill_round_clip(r, 8, fg.with_alpha(if open { 0x1c } else { 0x12 }), clip);
        }
        let ty = ((r.h as f32 - ui_line()) / 2.0).max(0.0).round() as i32;
        let mut x = r.x + 8;
        if let Some(i) = icon {
            self.tx.icon(cv, i, x, r.y + (r.h - 14) / 2, 14, col, clip);
            x += 20;
        }
        self.tx.draw_ui(cv, x, r.y + ty, label, col, clip, false);
        let lw = self.tx.ui_width(label, false);
        self.tx.icon(cv, "ui/chevron_down", x + lw + 6, r.y + (r.h - 12) / 2, 12, col, clip);
    }

    /// The open dropdown: a card of rows right over its button, the value in use ticked.
    fn chat_paint_dropdown(&mut self, cv: &mut Canvas, tab: &mut ChatTab, input_box: Rect) {
        let Some(d) = tab.dropdown.as_ref() else { return };
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let idx = match d.kind {
            DropKind::Model => 0,
            DropKind::Effort => 1,
            DropKind::Mode => 2,
        };
        let anchor = tab.anchors[idx];
        let rh = 36;
        let w = 320.min(input_box.w);
        let h = d.items.len() as i32 * rh + 10;
        let x = if d.kind == DropKind::Mode { anchor.x } else { (anchor.right() - w).max(input_box.x) };
        let card = Rect::new(x.min(input_box.right() - w), (anchor.y - 8 - h).max(4), w, h);
        cv.shadow(card, 12);
        cv.card(card, 12, c.widget_bg, c.border);
        let rows: Vec<(usize, chat_tab::DropItem)> = d.items.iter().cloned().enumerate().collect();
        let sel = d.sel;
        for (i, it) in rows {
            let r = Rect::new(card.x + 5, card.y + 5 + i as i32 * rh, card.w - 10, rh);
            if i == sel || (self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1)) {
                cv.fill_round(r, 8, fg.with_alpha(0x18));
            }
            let ty = (rh - 18) / 2;
            self.tx.draw_ui(cv, r.x + 12, r.y + ty, &it.label, fg, r, true);
            let lw = self.tx.ui_width(&it.label, true);
            let room = (r.w - 12 - lw - 12 - 30).max(0);
            if room > 40 {
                let t = crate::ui::fit_ellipsis(&mut self.tx, &it.detail, room, false);
                self.tx.draw_ui(cv, r.x + 12 + lw + 12, r.y + ty, &t, dim, r, false);
            }
            if it.on {
                self.tx.icon(cv, "ui/check", r.right() - 26, r.y + (rh - 14) / 2, 14, c.accent_text, r);
            }
            tab.hits.push((r, ChatHit::DropItem(i)));
        }
    }

    /// The `/` or `@` menu above the composer: a card of rows, the selected one tinted.
    fn chat_paint_menu(&mut self, cv: &mut Canvas, tab: &mut ChatTab, input_box: Rect) {
        let Some(m) = tab.menu.as_ref() else { return };
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let rows = m.entries.len().min(menu::ROWS) as i32;
        let rh = 34;
        let h = rows * rh + 10;
        let card = Rect::new(input_box.x, (input_box.y - 8 - h).max(4), input_box.w, h);
        cv.shadow(card, 12);
        cv.card(card, 12, c.widget_bg, c.border);
        let first = m.first_row(menu::ROWS);
        let entries: Vec<(usize, Entry, bool)> = m
            .entries
            .iter()
            .enumerate()
            .skip(first)
            .take(menu::ROWS)
            .map(|(i, e)| (i, e.clone(), i == m.sel))
            .collect();
        for (k, (i, e, sel)) in entries.into_iter().enumerate() {
            let r = Rect::new(card.x + 5, card.y + 5 + k as i32 * rh, card.w - 10, rh);
            if sel {
                cv.fill_round(r, 8, fg.with_alpha(0x18));
            }
            let col = if e.off.is_some() { dim.with_alpha(0x90) } else { fg };
            self.tx.icon(cv, e.icon, r.x + 10, r.y + (rh - 14) / 2, 14, dim, r);
            let lw = self.tx.ui_width(&e.label, true);
            self.tx.draw_ui(cv, r.x + 34, r.y + (rh - 18) / 2, &e.label, col, r, true);
            let tag_w = if e.tag.is_empty() { 0 } else { self.tx.ui_width(&e.tag, false) + 14 };
            let room = (r.w - 34 - lw - 16 - tag_w - 10).max(0);
            if room > 40 {
                let d = crate::ui::fit_ellipsis(&mut self.tx, &e.detail, room, false);
                self.tx.draw_ui(cv, r.x + 34 + lw + 12, r.y + (rh - 18) / 2, &d, dim, r, false);
            }
            if tag_w > 0 {
                self.tx.draw_ui(cv, r.right() - tag_w, r.y + (rh - 18) / 2, &e.tag, dim, r, false);
            }
            tab.hits.push((r, ChatHit::Pick(i)));
        }
    }

    // ───────────── the `/` and `@` menu, commands, pickers ─────────────

    /// Open, update or close the menu for what is typed: slash commands while a `/word` starts the
    /// text, files while an `@word` is at the caret.
    fn chat_refresh_menu(&mut self, tab: &mut ChatTab) {
        let text = tab.input.text().to_string();
        let chars = tab.input.selection().1;
        let caret = text.char_indices().nth(chars).map_or(text.len(), |(i, _)| i);
        let root = self.root.clone();
        let old = tab.menu.take();
        let mut menu = if slash::query(&text).is_some() {
            menu::slash(tab.commands(&root), &text)
        } else if mention::active(&text, caret).is_some() {
            let files = tab.files.get_or_insert_with(|| {
                let found = crate::search::list_files_in(std::slice::from_ref(&root), 6000);
                mention::relative_files(&root, &found)
            });
            menu::files(files, &text, caret)
        } else {
            None
        };
        // The same query keeps the row the user moved to.
        if let (Some(m), Some(o)) = (menu.as_mut(), old.as_ref())
            && m.kind == o.kind
            && m.query == o.query
        {
            m.sel = o.sel.min(m.entries.len().saturating_sub(1));
        }
        tab.menu = menu;
    }

    /// Take the selected menu row: a command completes the composer (and runs when it takes no
    /// arguments and Enter picked it), a file replaces the `@word` with its mention.
    fn chat_pick(&mut self, tab: &mut ChatTab, run: bool, sent: &std::cell::RefCell<Option<String>>) {
        let Some(m) = tab.menu.take() else { return };
        let Some(e) = m.selected().cloned() else { return };
        match m.kind {
            MenuKind::Slash => {
                if let Some(why) = e.off {
                    self.say(why);
                    tab.menu = Some(m);
                    return;
                }
                tab.input.set_text(&e.pick);
                if run && e.runs {
                    *sent.borrow_mut() = self.chat_run_typed(tab);
                }
            }
            MenuKind::Mention => {
                let text = tab.input.text().to_string();
                let chars = tab.input.selection().1;
                let caret = text.char_indices().nth(chars).map_or(text.len(), |(i, _)| i);
                let end = caret.max(m.start);
                let new = format!("{}{}{}", &text[..m.start], mention::insertion(&e.pick), text[end..].trim_start());
                tab.input.set_text(&new);
            }
        }
        self.chat_refresh_menu(tab);
    }

    /// Enter: a typed `/command` runs the way its catalog entry says; anything else is sent.
    fn chat_run_typed(&mut self, tab: &mut ChatTab) -> Option<String> {
        let text = tab.input.text().trim().to_string();
        tab.menu = None;
        if let Some((name, args)) = slash::parse(&text) {
            let root = self.root.clone();
            let found = slash::find(tab.commands(&root), name).cloned();
            if let Some(c) = found {
                match c.handling {
                    Handling::Native(n) => {
                        tab.input.set_text("");
                        self.chat_native(tab, n, args);
                        return None;
                    }
                    Handling::Off(why) => {
                        self.say(why);
                        return None;
                    }
                    Handling::Pass => {}
                }
            }
        }
        tab.submit()
    }

    /// The commands Lumen runs itself in a chat.
    fn chat_native(&mut self, tab: &mut ChatTab, n: Native, args: &str) {
        match n {
            Native::Clear => {
                tab.backend.reset(&mut tab.chat);
                tab.laid = None;
                tab.follow = true;
            }
            Native::Model if args.is_empty() => tab.open_dropdown(DropKind::Model),
            Native::Model => {
                tab.backend.set_model(&mut tab.chat, args);
                self.say(format!("Model: {args}"));
            }
            Native::Help => {
                let root = self.root.clone();
                let t = slash::help_text(tab.commands(&root));
                tab.chat.note(&t);
            }
            Native::Cost => tab.chat.note(&chat::usage_line(&tab.chat.usage())),
            Native::Status => {
                let t = format!(
                    "{} · {} · {} · {}",
                    tab.backend.kind.name(),
                    tab.chat.model.as_deref().map_or_else(|| "default model".to_string(), chat::short_model),
                    tab.backend.mode.label(),
                    self.root.display()
                );
                tab.chat.note(&t);
            }
            Native::Copy => {
                let last = tab.chat.messages.iter().rev().filter(|m| m.role == Role::Assistant).find_map(|m| {
                    m.blocks.iter().rev().find_map(|b| match b {
                        Block::Text(t) => Some(t.clone()),
                        _ => None,
                    })
                });
                match last {
                    Some(t) => {
                        self.clip.set(&t);
                        self.say("Copied the last answer");
                    }
                    None => self.say("Nothing to copy yet"),
                }
            }
            Native::Config => self.open_settings(crate::settingsui::Page::Settings),
            Native::Memory => {
                let p = self.root.join("CLAUDE.md");
                if p.exists() {
                    self.open_path(&p, None);
                    self.ai_hide();
                } else {
                    self.say("No CLAUDE.md in this project");
                }
            }
            Native::Agents => {
                let t = if tab.chat.agents.is_empty() {
                    "No sub-agents reported by the agent.".to_string()
                } else {
                    format!("Sub-agents: {}", tab.chat.agents.join(", "))
                };
                tab.chat.note(&t);
            }
            Native::Permissions => tab.open_dropdown(DropKind::Mode),
            Native::Plan => {
                tab.backend.set_mode(&mut tab.chat, PermMode::Plan);
                if !args.is_empty() {
                    tab.input.set_text(args);
                    tab.submit();
                }
            }
            Native::Resume => {
                tab.chat.note("Continuing an earlier conversation is not available in this view yet.");
            }
        }
        tab.laid = None;
    }

    // ───────────── input ─────────────

    /// A key in a chat tab: the composer's editing keys; Enter sends, Shift+Enter breaks the line, Esc
    /// stops the turn, Shift+Tab cycles the permission mode.
    pub fn ai_chat_key(&mut self, ev: &KeyIn) -> bool {
        let sent = std::cell::RefCell::new(None);
        let done = self.ai_chat_key_in(ev, &sent);
        self.ai_chat_title(sent.into_inner());
        done
    }

    /// The tab is named after the first thing asked in it.
    fn ai_chat_title(&mut self, sent: Option<String>) {
        let Some(text) = sent else { return };
        if let Some(s) = self.ai.cur_mut().filter(|s| s.chat.is_some() && s.name.ends_with(" chat")) {
            let first = text.lines().next().unwrap_or("").trim();
            let mut t: String = first.chars().take(28).collect();
            if first.chars().count() > 28 {
                t.push('…');
            }
            if !t.is_empty() {
                s.name = t;
            }
        }
    }

    fn ai_chat_key_in(&mut self, ev: &KeyIn, sent: &std::cell::RefCell<Option<String>>) -> bool {
        use winit::keyboard::NamedKey as N;
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        self.with_chat(|app, tab| {
            tab.focused = true;
            if let Some(d) = tab.dropdown.as_mut() {
                match &ev.key {
                    Key::Named(N::ArrowUp) => d.sel = (d.sel + d.items.len() - 1) % d.items.len(),
                    Key::Named(N::ArrowDown) => d.sel = (d.sel + 1) % d.items.len(),
                    Key::Named(N::Enter) => {
                        let i = d.sel;
                        tab.pick_dropdown(i);
                    }
                    _ => tab.dropdown = None,
                }
                return true;
            }
            if tab.menu.is_some() {
                match &ev.key {
                    Key::Named(N::ArrowUp) => {
                        if let Some(m) = tab.menu.as_mut() {
                            m.up();
                        }
                        return true;
                    }
                    Key::Named(N::ArrowDown) => {
                        if let Some(m) = tab.menu.as_mut() {
                            m.down();
                        }
                        return true;
                    }
                    Key::Named(N::Escape) => {
                        tab.menu = None;
                        return true;
                    }
                    Key::Named(N::Tab) if !shift => {
                        app.chat_pick(tab, false, sent);
                        return true;
                    }
                    Key::Named(N::Enter) if !shift && !ctrl && !alt => {
                        app.chat_pick(tab, true, sent);
                        return true;
                    }
                    _ => {}
                }
            }
            if matches!(&ev.key, Key::Named(N::Escape)) {
                tab.backend.interrupt(&mut tab.chat);
                return true;
            }
            if matches!(&ev.key, Key::Named(N::Tab)) && shift && tab.backend.kind == Kind::Claude {
                let m = tab.backend.mode.next();
                tab.backend.set_mode(&mut tab.chat, m);
                return true;
            }
            if matches!(&ev.key, Key::Named(N::Enter)) && !shift && !ctrl && !alt {
                *sent.borrow_mut() = app.chat_run_typed(tab);
                app.chat_refresh_menu(tab);
                return true;
            }
            if matches!(&ev.key, Key::Named(N::PageUp | N::PageDown)) {
                let d = if matches!(&ev.key, Key::Named(N::PageUp)) { -1.0 } else { 1.0 };
                tab.follow = false;
                tab.scroll = (tab.scroll + d * tab.view.h as f64 * 0.8).clamp(0.0, tab.max_scroll);
                tab.follow = tab.scroll >= tab.max_scroll - 1.0;
                return true;
            }
            let tx = &mut app.tx;
            tab.input.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
            let act = tab.input.key(&ev.key, ctrl, shift, alt);
            if matches!(act, crate::textarea::KeyAct::Handled | crate::textarea::KeyAct::Paste) {
                // Typing may have opened or closed the `/` or `@` menu (the paste is done below).
            }
            match act {
                crate::textarea::KeyAct::Handled => {
                    app.chat_refresh_menu(tab);
                    return true;
                }
                crate::textarea::KeyAct::Copy(t) => {
                    app.clip.set(&t);
                    return true;
                }
                crate::textarea::KeyAct::Paste => {
                    let t = app.clip.get();
                    tab.input.insert(&t);
                    app.chat_refresh_menu(tab);
                    return true;
                }
                crate::textarea::KeyAct::Ignored => {}
            }
            let text = ev.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
            if let Some(t) = text
                && (!ctrl || alt)
            {
                tab.input.type_text(t);
                app.chat_refresh_menu(tab);
            }
            true
        })
        .unwrap_or(false)
    }

    /// A press in a chat tab (the session shown): buttons, permission answers, the composer's caret.
    pub fn ai_chat_press(&mut self, x: f64, y: f64) {
        let sent = std::cell::RefCell::new(None);
        self.ai_chat_press_in(x, y, &sent);
        self.ai_chat_title(sent.into_inner());
    }

    fn ai_chat_press_in(&mut self, x: f64, y: f64, sent: &std::cell::RefCell<Option<String>>) {
        let clicks = 1;
        let shift = self.mods.shift_key();
        self.with_chat(|app, tab| {
            tab.focused = true;
            if tab.dropdown.is_some() {
                match tab.hit_at(x, y) {
                    Some(ChatHit::DropItem(i)) => {
                        tab.pick_dropdown(i);
                        app.request_redraw();
                        return;
                    }
                    // A button of the toolbar toggles its own dropdown below; anywhere else closes it.
                    Some(ChatHit::Model | ChatHit::Effort | ChatHit::Mode) => {}
                    _ => {
                        tab.dropdown = None;
                        app.request_redraw();
                        return;
                    }
                }
            }
            match tab.hit_at(x, y) {
                Some(ChatHit::DropItem(_)) => {}
                Some(ChatHit::Send) => *sent.borrow_mut() = tab.submit(),
                Some(ChatHit::Stop) => tab.backend.interrupt(&mut tab.chat),
                Some(ChatHit::Mode) => tab.open_dropdown(DropKind::Mode),
                Some(ChatHit::Model) => tab.open_dropdown(DropKind::Model),
                Some(ChatHit::Effort) => tab.open_dropdown(DropKind::Effort),
                Some(ChatHit::Pick(i)) => {
                    if let Some(m) = tab.menu.as_mut() {
                        m.sel = i;
                    }
                    app.chat_pick(tab, true, sent);
                }
                Some(ChatHit::Allow(i) | ChatHit::Deny(i)) => {
                    let allow = matches!(tab.hit_at(x, y), Some(ChatHit::Allow(_)));
                    if let Some(id) = tab.chat.perms.get(i).map(|p| p.request_id.clone()) {
                        tab.backend.answer(&mut tab.chat, &id, allow);
                    }
                }
                Some(ChatHit::Input) | None => {
                    let b = tab.input_box;
                    if b.contains(x, y) {
                        let (ox, oy) = (b.x + 16, b.y + 14);
                        tab.input.press((x - ox as f64) as f32, (y - oy as f64) as f32, clicks, shift);
                    }
                    app.chat_refresh_menu(tab);
                }
            }
            app.request_redraw();
        });
    }

    /// Wheel over a chat tab: the transcript scrolls (the composer's own text when it overflows).
    pub fn ai_chat_wheel(&mut self, delta: MouseScrollDelta, x: f64, y: f64) {
        self.with_chat(|app, tab| {
            if tab.input_box.contains(x, y) && tab.input.max_scroll() > 0.0 {
                let px = crate::scroller::wheel_px(delta, lh() as f64);
                tab.input.scroll_by(px as f32);
            } else {
                let px = crate::scroller::wheel_px(delta, lh() as f64);
                tab.scroll = (tab.scroll + px).clamp(0.0, tab.max_scroll);
                tab.follow = tab.scroll >= tab.max_scroll - 1.0;
            }
            app.request_redraw();
        });
    }

    /// Cursor over a chat tab: hand on buttons, text in the composer.
    pub fn ai_chat_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let i = self.ai.index_of(self.ai_pane_at(x, y)?)?;
        let tab = self.ai.sessions[i].chat.as_ref()?;
        match tab.hit_at(x, y)? {
            ChatHit::Input => Some(CursorIcon::Text),
            _ => Some(CursorIcon::Pointer),
        }
    }
}

#[allow(dead_code)]
fn _unused(_: Rgba) {}
