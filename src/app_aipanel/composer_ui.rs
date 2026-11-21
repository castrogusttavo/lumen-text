//! The composer on the App: geometry, drawing, keys, pointer, send / attach, and the header's
//! sessions menu. The rules (what a key means, history, what attaching inserts, the bytes sent)
//! are in `composer`, the text editing in `crate::textarea`.

use super::composer::{self, Route};
use super::*;
use crate::{app_overlay::Action, textarea::KeyAct};

/// Secondary text size.
const SMALL: f32 = 12.0;
/// Space around the text inside the box.
const BOX_PAD_X: i32 = 12;
const BOX_PAD_TOP: i32 = 10;
const BOX_PAD_BOTTOM: i32 = 8;
/// Gap between the text and the chip row.
const GAP: i32 = 6;
/// Height of the chip row (agent, attachments, send).
const CHIPS_H: i32 = 28;
/// The terminal above keeps at least this much height, however tall the box wants to be.
const MIN_TERM_H: i32 = 120;
/// The Enter that submits goes this long after the paste, so an agent that treats input arriving
/// together as one paste burst still sees a key press.
const SUBMIT_DELAY_MS: u64 = 80;

/// Where the composer sits this frame.
pub(super) struct Geo {
    /// The whole strip at the bottom of the panel (box and its gaps).
    pub area: Rect,
    pub boxr: Rect,
    /// Rows of text shown.
    pub lines: usize,
    /// Row height.
    pub lh: i32,
}

impl App {
    /// Wrap the draft at the box's width and size the box (2 to 8 rows, less when the panel is
    /// short); the terminal above gets the rest.
    pub(super) fn ai_composer_geo(&mut self, p: Rect, top: i32) -> Geo {
        let lh = (crate::ui::ui_line().ceil() as i32).max(18);
        let chat = self.ai.cur().is_some_and(|s| s.chat.is_some());
        let box_w = (p.w - 1 - 2 * PAD).max(1).min(if chat { chat_view::MAX_COL } else { i32::MAX });
        let inner_w = (box_w - 2 * BOX_PAD_X).max(1);
        let tx = &mut self.tx;
        self.ai.composer.layout(inner_w as f32, &mut |s: &str| tx.ui_char_xs(s, false, None));
        let chrome = BOX_PAD_TOP + GAP + CHIPS_H + BOX_PAD_BOTTOM;
        let room = p.bottom() - top - MIN_TERM_H - 8 - 12 - chrome;
        let max_lines = (room / lh).clamp(1, composer::MAX_LINES as i32) as usize;
        let lines = composer::visible_lines(self.ai.composer.line_count()).min(max_lines);
        let box_h = chrome + lines as i32 * lh;
        let area_h = box_h + 8 + 12;
        self.ai.composer_area = area_h;
        let area = Rect::new(p.x + 1, p.bottom() - area_h, p.w - 1, area_h);
        let boxr = Rect::new(area.x + (area.w - box_w) / 2, area.y + 8, box_w, box_h);
        self.ai.composer.set_view(lh as f32, (lines as i32 * lh) as f32);
        self.ai.composer_box = boxr;
        self.ai.composer_origin = (boxr.x + BOX_PAD_X, boxr.y + BOX_PAD_TOP);
        Geo { area, boxr, lines, lh }
    }

    /// The rounded box: wrapped text (placeholder when empty), selection, caret, then the chip
    /// row: agent, attach file, attach selection on the left, the round send button on the right.
    pub(super) fn ai_draw_composer(&mut self, cv: &mut Canvas, g: &Geo) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(g.area, c.sidebar_bg);
        let b = g.boxr;
        let on = self.ai.focused && self.ai.composer_focus && !self.ai.filter_focus && self.overlay.is_none();
        let (mx, my) = self.mouse;
        let hot = self.overlay.is_none() && b.contains(mx, my);
        let ring = if on {
            c.accent.with_alpha(0xa0)
        } else if hot {
            strong.with_alpha(0x30)
        } else {
            c.border
        };
        cv.shadow(b, 12);
        cv.card(b, 12, c.widget_bg, ring);
        self.ai.hits.push((b, AiHit::Composer));

        // ── text ──
        let (ox, oy) = self.ai.composer_origin;
        let inner_w = b.w - 2 * BOX_PAD_X;
        let text_h = g.lines as i32 * g.lh;
        let clip = Rect::new(b.x + 4, oy, b.w - 8, text_h);
        let ty = ((g.lh as f32 - crate::ui::ui_line()) / 2.0).max(0.0).round() as i32;
        if self.ai.composer.text().is_empty() {
            let name = self.ai.agent().map_or("the agent", |a| a.name);
            let ph = format!("Message {name}…  (Enter sends, Shift+Enter adds a line)");
            let ph =
                if self.ai.cur().is_some_and(|s| s.chat.is_some()) { format!("Ask {name} anything…") } else { ph };
            let ph = crate::ui::fit_ellipsis(&mut self.tx, &ph, inner_w, false);
            self.tx.draw_ui(cv, ox, oy + ty, &ph, dim, clip, false);
        }
        let scroll = self.ai.composer.scroll.round() as i32;
        let row_y = |k: usize| oy + k as i32 * g.lh - scroll;
        let first = (scroll / g.lh).max(0) as usize;
        let last = ((scroll + text_h) / g.lh + 1).max(0) as usize;
        let sel = self.theme.selection;
        for (k, x0, x1) in self.ai.composer.sel_spans() {
            if (first..=last).contains(&k) {
                let (a, z) = (x0.round() as i32, x1.round() as i32);
                cv.fill_clip(Rect::new(ox + a, row_y(k), (z - a).max(1), g.lh), sel, clip);
            }
        }
        for (k, l) in self.ai.composer.lines().iter().enumerate().take(last + 1).skip(first) {
            let t = self.ai.composer.line_text(l);
            self.tx.draw_ui(cv, ox, row_y(k) + ty, &t, strong, clip, false);
        }
        if on {
            let (k, x) = self.ai.composer.caret();
            let cx = ox + (x.round() as i32).min(inner_w + 4);
            cv.fill_clip(Rect::new(cx, row_y(k) + 1, 1, g.lh - 2), strong, clip);
        }

        // ── chips ──
        let cy = oy + text_h + GAP;
        let send = Rect::new(b.right() - 8 - CHIPS_H, cy, CHIPS_H, CHIPS_H);
        let left_limit = send.x - 8;
        let mut x = b.x + 8;
        // Agent chip (icon only when the room is tight).
        let (icon, name) = self.ai.agent().map_or(("ui/sparkle", "Choose agent"), |a| (a.icon, a.name));
        let nw = self.tx.ui_width(name, false);
        let full = 10 + 14 + 6 + nw + 6 + 12 + 8;
        let short = 10 + 14 + 4 + 12 + 8;
        let two_btns = 2 * (26 + 2) + 4;
        let show_name = x + full + two_btns <= left_limit;
        let w = if show_name { full } else { short };
        let chip = Rect::new(x, cy + 1, w, 26);
        let hv = self.overlay.is_none() && chip.contains(mx, my);
        cv.fill_round(chip, 13, strong.with_alpha(if hv { 0x1c } else { 0x0e }));
        self.tx.icon(cv, icon, chip.x + 10, chip.y + 6, 14, strong, chip);
        let mut cx = chip.x + 10 + 14 + 4;
        if show_name {
            self.tx.draw_ui(cv, cx + 2, chip.y + 4, name, strong, chip, false);
            cx += 2 + nw + 4;
        }
        self.tx.icon(cv, "ui/chevron_down", cx, chip.y + 7, 12, dim, chip);
        self.ai.hits.push((chip, AiHit::Agent));
        x = chip.right() + 4;
        for (icon, hit) in [("ui/paperclip", AiHit::AttachFile), ("ui/quote", AiHit::AttachSel)] {
            let r = Rect::new(x, cy + 1, 26, 26);
            if x + 26 <= left_limit {
                self.ai_icon_btn(cv, r, icon, Some(hit));
            }
            x += 28;
        }
        // The keys hint, when there is room for it.
        let room = left_limit - x - 4;
        if room > 90 && on {
            let keys = if self.ai.cur().is_some_and(|s| s.chat.is_some()) {
                "Enter send · Esc stop"
            } else {
                "Enter send · Tab terminal"
            };
            let hint = crate::ui::fit_ellipsis(&mut self.tx, keys, room, false);
            let hr = Rect::new(x + 4, cy, room, CHIPS_H);
            self.tx.draw_ui_sized(cv, hr.x, cy + 6, &hint, dim.with_alpha(0xa0), hr, false, SMALL);
        }
        // Send, or Stop while the agent works and nothing is typed.
        let has_text = !self.ai.composer.text().trim().is_empty();
        let working = self.ai.cur().is_some_and(|s| s.status() == Status::Working);
        let sh = self.overlay.is_none() && send.contains(mx, my);
        if has_text || !working {
            let (fill, fg) = if has_text {
                (if sh { c.accent.with_alpha(0xe0) } else { c.accent }, c.on_accent)
            } else {
                (strong.with_alpha(0x14), dim.with_alpha(0x90))
            };
            cv.fill_round(send, CHIPS_H / 2, fill);
            self.tx.icon(cv, "ui/arrow_up", send.x + 6, send.y + 6, 16, fg, send);
            self.ai.hits.push((send, AiHit::Send));
        } else {
            cv.fill_round(send, CHIPS_H / 2, strong.with_alpha(if sh { 0x30 } else { 0x20 }));
            self.tx.icon(cv, "ui/stop", send.x + 6, send.y + 6, 16, strong, send);
            self.ai.hits.push((send, AiHit::Interrupt));
        }
    }

    /// Write raw bytes to the agent of the session shown (a key it understands: Esc, Ctrl+C...).
    fn ai_write(&mut self, bytes: &[u8]) {
        self.ai.on_input();
        if let Some(t) = self.ai.term_mut()
            && t.alive
        {
            t.sel = None;
            t.write(bytes);
        }
    }

    /// The Interrupt button / Esc in the composer: Esc goes to the agent (it stops its turn).
    pub(super) fn ai_interrupt(&mut self) {
        if self.ai.cur().is_some_and(|s| s.chat.is_some()) {
            return self.ai_stop();
        }
        self.ai_write(b"\x1b");
    }

    /// A key while the composer has the keyboard. Always consumed except a Ctrl / Alt combination
    /// the composer has no use for (the editor's shortcut then runs).
    pub(super) fn ai_composer_key(&mut self, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if matches!(&ev.key, Key::Named(n) if input::is_modifier(*n)) {
            return true;
        }
        if self.chat_scroll_key(&ev.key, ctrl) {
            return true;
        }
        let empty = self.ai.composer.text().is_empty();
        let browsing = self.ai.history.browsing();
        match composer::route(&ev.key, ctrl, shift, alt, empty, browsing) {
            Route::Send => {
                self.ai_send();
                return true;
            }
            Route::HistoryPrev => {
                let cur = self.ai.composer.text().to_string();
                if let Some(t) = self.ai.history.prev(&cur) {
                    self.ai.composer.set_text(&t);
                }
                return true;
            }
            Route::HistoryNext => {
                if let Some(t) = self.ai.history.next() {
                    self.ai.composer.set_text(&t);
                }
                return true;
            }
            Route::ToTerminal => {
                if self.ai.cur().is_none_or(|s| s.chat.is_none()) {
                    self.ai.composer_focus = false;
                }
                return true;
            }
            Route::Interrupt => {
                // Walking the history: Esc gives the draft back; otherwise it interrupts the agent.
                if browsing {
                    let mut back = None;
                    while self.ai.history.browsing() {
                        back = self.ai.history.next();
                    }
                    self.ai.composer.set_text(&back.unwrap_or_default());
                } else {
                    self.ai_interrupt();
                }
                return true;
            }
            Route::Edit => {}
        }
        // Ctrl+C with nothing selected is the agent's interrupt, as in its own terminal.
        let ctrl_c = ctrl && !shift && !alt && matches!(&ev.key, Key::Character(c) if c.eq_ignore_ascii_case("c"));
        if ctrl_c && !self.ai.composer.has_selection() {
            if self.ai.cur().is_some_and(|s| s.chat.is_some()) {
                // A chat: Ctrl+C copies what is selected in the transcript, else stops the turn.
                match self.chat_selected_text() {
                    Some(t) => self.clip.set(&t),
                    None => self.ai_stop(),
                }
            } else {
                self.ai_write(&[0x03]);
            }
            return true;
        }
        // The text may have changed since the last frame: Up / Down / Home / End need its lines.
        let tx = &mut self.tx;
        self.ai.composer.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
        let before = self.ai.composer.text().to_string();
        match self.ai.composer.key(&ev.key, ctrl, shift, alt) {
            KeyAct::Handled => {
                if self.ai.composer.text() != before {
                    self.ai.history.reset();
                }
                return true;
            }
            KeyAct::Copy(t) => {
                self.clip.set(&t);
                return true;
            }
            KeyAct::Paste => {
                self.ai_composer_paste();
                return true;
            }
            KeyAct::Ignored => {}
        }
        let text = ev.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
        // AltGr arrives as Ctrl+Alt on some systems: its characters are typed, not shortcuts.
        let altgr = ctrl && alt && text.is_some();
        if (ctrl || alt) && !altgr {
            return false;
        }
        if let Some(t) = text {
            self.ai.history.reset();
            self.ai.composer.type_text(t);
        }
        true
    }

    /// `Ctrl+V` in the composer: copied files become their paths, text is inserted; an image
    /// (nothing else on the clipboard) goes to the agent as its own Ctrl+V, which attaches it.
    pub(super) fn ai_composer_paste(&mut self) {
        match self.clip.offer() {
            crate::clipboard::Offer::Files(f) => {
                let paths: Vec<String> = f.paths.iter().map(|p| paste_path(p)).collect();
                self.ai.composer.insert(&format!("{} ", paths.join(" ")));
            }
            crate::clipboard::Offer::Text(t) => {
                self.ai.composer.insert(&t);
            }
            crate::clipboard::Offer::Image | crate::clipboard::Offer::Empty => self.ai_write(&[0x16]),
        }
        self.ai.history.reset();
    }

    /// Send the draft to the agent of the session shown: one paste, then Enter a moment later. An
    /// ended agent starts again first and the draft stays for a second Enter.
    pub fn ai_send(&mut self) {
        let text = self.ai.composer.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        if let Some(s) = self.ai.cur_mut()
            && let Some(c) = s.chat.as_mut()
        {
            let cs: &mut ChatSession = c;
            cs.send(&text);
            s.track.on_input();
            self.ai.history.push(&text);
            self.ai.composer.set_text("");
            self.request_redraw();
            return;
        }
        if self.ai.term().is_none_or(|t| !t.alive) {
            self.ai_respawn();
            self.say("Agent restarted — press Enter again to send");
            return;
        }
        let Some(s) = self.ai.cur_mut() else { return };
        let Some(t) = s.term.as_mut() else { return };
        let Some((paste, _)) = composer::submit_bytes(&text, t.grid.bracketed_paste) else { return };
        t.sel = None;
        t.write(&paste);
        let id = s.id;
        self.ai.pending_enter = Some((id, Instant::now() + Duration::from_millis(SUBMIT_DELAY_MS)));
        self.ai.on_input();
        self.ai.history.push(&text);
        self.ai.composer.set_text("");
        self.request_redraw();
    }

    /// Per-event hook: the Enter that follows a sent message, once its moment has come.
    pub(crate) fn ai_flush_enter(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some((id, at)) = self.ai.pending_enter else { return };
        if now < at {
            return want(at);
        }
        self.ai.pending_enter = None;
        if let Some(i) = self.ai.index_of(id)
            && let Some(t) = self.ai.sessions[i].term.as_mut()
            && t.alive
        {
            t.write(b"\r");
        }
    }

    /// The composer's "attach file": an `@` mention of the file open in the editor.
    pub fn ai_attach_file(&mut self) {
        self.ai.composer_focus = true;
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path()).map(Path::to_path_buf) else {
            return self.say("Open a saved file to attach it");
        };
        let m = composer::mention(&self.rel(&path));
        self.ai.composer.insert(&m);
        self.ai.history.reset();
    }

    /// The composer's "attach selection": the editor's selected text, fenced, with its place.
    pub fn ai_attach_selection(&mut self) {
        self.ai.composer_focus = true;
        let Some(d) = self.docs.get(self.active) else { return self.say("Open a file and select some code first") };
        let text = d.ed.selected_text(&d.buf);
        if text.trim().is_empty() {
            return self.say("Select some code in the editor first");
        }
        let first = d.ed.selected_line_range(&d.buf).0 + 1;
        let path = d.path().map(Path::to_path_buf);
        let rel = path.map_or_else(|| "untitled".to_string(), |p| self.rel(&p));
        let mut block = composer::selection_block(&rel, first, &text);
        let cur = self.ai.composer.text();
        if !cur.is_empty() && !cur.ends_with(char::is_whitespace) {
            block.insert(0, '\n');
        }
        self.ai.composer.insert(&block);
        self.ai.history.reset();
    }

    /// A press in the composer box: the keyboard goes to it and the caret lands under the pointer
    /// (double click selects a word, triple the line).
    pub(super) fn ai_composer_press(&mut self, x: f64, y: f64) {
        self.ai.composer_focus = true;
        let now = Instant::now();
        let at = (x as i32, y as i32);
        let clicks = match self.ai.composer_click {
            Some((t, (px, py), n))
                if t.elapsed().as_millis() < DOUBLE_CLICK_MS && (px - at.0).abs() < 4 && (py - at.1).abs() < 4 =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.ai.composer_click = Some((now, at, clicks));
        let (ox, oy) = self.ai.composer_origin;
        let tx = &mut self.tx;
        self.ai.composer.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
        self.ai.composer.press((x - ox as f64) as f32, (y - oy as f64) as f32, clicks, self.mods.shift_key());
    }

    /// Pointer moved with the button down after a press in the composer: extend the selection.
    pub(super) fn ai_composer_drag(&mut self, x: f64, y: f64) -> bool {
        if !self.ai.composer.dragging() {
            return false;
        }
        let (ox, oy) = self.ai.composer_origin;
        let tx = &mut self.tx;
        self.ai.composer.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
        self.ai.composer.drag_to((x - ox as f64) as f32, (y - oy as f64) as f32);
        true
    }

    /// The header's `…` menu: the sessions (pick one to show it) and the session commands.
    pub fn ai_menu(&mut self) {
        use crate::app_overlay::{Cmd, Item};
        let mut items: Vec<Item> = Vec::new();
        for s in &self.ai.sessions {
            let cur = if s.id == self.ai.sessions.get(self.ai.active).map_or(0, |a| a.id) { "current · " } else { "" };
            items.push(Item {
                label: s.name.clone(),
                detail: format!("{cur}{} · {}", s.status().label(), s.agent.name),
                action: Action::AiSession(s.id),
            });
        }
        let cmd = |label: &str, detail: &str, c: Cmd| Item {
            label: label.to_string(),
            detail: detail.to_string(),
            action: Action::Cmd(c),
        };
        items.push(Item {
            label: "Filter sessions…".into(),
            detail: "Narrow the session pills by name or agent".into(),
            action: Action::AiFilter,
        });
        items.push(cmd("Rename session…", "Name the session shown", Cmd::AiRenameSession));
        items.push(cmd("Next session waiting for you", "Jump to the one that asks", Cmd::AiNextWaiting));
        items.push(cmd("Choose agent…", "Restart this session with another agent", Cmd::AiChooseAgent));
        items.push(cmd("New session in worktree…", "Another agent in a git worktree", Cmd::AiNewSessionInWorktree));
        self.open_locations("AI sessions", items);
    }

    /// A session picked in the menu.
    pub fn ai_pick_session(&mut self, id: u64) {
        if let Some(i) = self.ai.index_of(id) {
            self.ai_activate(i);
            self.ai_show();
        }
    }

    /// "Filter sessions…": the filter row opens with the keyboard in it.
    pub fn ai_open_filter(&mut self) {
        self.ai_show();
        self.ai.filter_open = true;
        self.ai.filter_focus = true;
    }
}
