//! The composer on the App: geometry, drawing, keys, pointer, send / attach, and the header's
//! sessions menu. The rules (what a key means, history, what attaching inserts, the bytes sent)
//! are in `composer`, the text editing in `crate::textarea`.

use super::chat::{self, PermMode};
use super::composer::{self, Route};
use super::*;
use crate::{app_overlay::Action, textarea::KeyAct, theme::Rgba};

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
/// Height of an attachment chip.
const ATT_H: i32 = 24;
/// Height of a row of the autocomplete menu, and of its hint line.
const MENU_ROW_H: i32 = 30;
const MENU_HINT_H: i32 = 22;
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
    /// Height of the row of attachment chips above the text (0 without attachments).
    pub att_h: i32,
}

impl App {
    /// Wrap the draft at the box's width and size the box (2 to 8 rows, less when the panel is
    /// short); the terminal above gets the rest.
    pub(super) fn ai_composer_geo(&mut self, p: Rect, top: i32) -> Geo {
        let lh = (crate::ui::ui_line().ceil() as i32).max(18);
        let chat = self.ai.cur().is_some_and(|s| s.chat.is_some());
        self.ai_refresh_menu();
        let att_h = if chat && !self.ai.attachments.is_empty() { ATT_H + GAP } else { 0 };
        let box_w = (p.w - 1 - 2 * PAD).max(1).min(if chat { chat_view::MAX_COL } else { i32::MAX });
        let inner_w = (box_w - 2 * BOX_PAD_X).max(1);
        let tx = &mut self.tx;
        self.ai.composer.layout(inner_w as f32, &mut |s: &str| tx.ui_char_xs(s, false, None));
        let chrome = BOX_PAD_TOP + att_h + GAP + CHIPS_H + BOX_PAD_BOTTOM;
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
        self.ai.composer_origin = (boxr.x + BOX_PAD_X, boxr.y + BOX_PAD_TOP + att_h);
        Geo { area, boxr, lines, lh, att_h }
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

        // ── attachments (chat) ──
        let chat = self.ai.cur().is_some_and(|s| s.chat.is_some());
        let (mx, my) = self.mouse;
        if g.att_h > 0 {
            let ay = b.y + BOX_PAD_TOP;
            let mut ax = b.x + BOX_PAD_X;
            let atts = self.ai.attachments.clone();
            for (i, a) in atts.iter().enumerate() {
                let nm = crate::ui::fit_ellipsis(&mut self.tx, &a.name, 150, false);
                let w = 8 + 14 + 4 + self.tx.ui_width_sized(&nm, false, SMALL) + 6 + 18 + 2;
                if ax + w > b.right() - BOX_PAD_X {
                    break;
                }
                let chip = Rect::new(ax, ay, w, ATT_H);
                cv.fill_round(chip, 7, strong.with_alpha(0x0e));
                cv.ring(chip, 7, c.border);
                let icon = if a.image.is_some() { "ui/image" } else { "ui/file_code" };
                self.tx.icon(cv, icon, chip.x + 8, chip.y + 5, 14, dim, chip);
                self.tx.draw_ui_sized(cv, chip.x + 8 + 14 + 4, chip.y + 4, &nm, strong, chip, false, SMALL);
                let xr = Rect::new(chip.right() - 20, chip.y + 2, 18, 20);
                let hv = self.overlay.is_none() && xr.contains(mx, my);
                if hv {
                    cv.fill_round(xr, 5, strong.with_alpha(0x20));
                }
                self.tx.icon(cv, "ui/close", xr.x + 3, xr.y + 4, 12, if hv { strong } else { dim }, chip);
                self.ai.hits.push((xr, AiHit::Unattach(i)));
                ax += w + 6;
            }
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
        let show_name = !chat && x + full + two_btns <= left_limit;
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
        if chat {
            // The permission mode (Claude Code) and the model: what the agent may do, and who.
            let reserve = 2 * 28;
            if self.ai_kind() == Some(backend::Kind::Claude) {
                let m = self.ai_mode();
                let col = match m {
                    PermMode::Plan => c.accent_text,
                    PermMode::Bypass => c.error,
                    _ => dim,
                };
                if let Some(nx) = self.ai_chip(cv, x, cy, m.icon(), m.label(), col, AiHit::Mode, left_limit - reserve) {
                    x = nx;
                }
            }
            let model = self
                .ai
                .cur()
                .and_then(|s| s.chat.as_deref())
                .and_then(|cs| cs.chat.model.clone().or(cs.be.model.clone()))
                .map_or_else(|| "Model".to_string(), |m| chat::short_model(&m));
            if let Some(nx) = self.ai_chip(cv, x, cy, "ui/cpu", &model, dim, AiHit::Model, left_limit - reserve) {
                x = nx;
            }
        }
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
            let keys = if chat { "Enter send · Esc stop" } else { "Enter send · Tab terminal" };
            let hint = crate::ui::fit_ellipsis(&mut self.tx, keys, room, false);
            let hr = Rect::new(x + 4, cy, room, CHIPS_H);
            self.tx.draw_ui_sized(cv, hr.x, cy + 6, &hint, dim.with_alpha(0xa0), hr, false, SMALL);
        }
        // Send, or Stop while the agent works and nothing is typed.
        let has_text = !self.ai.composer.text().trim().is_empty() || (chat && !self.ai.attachments.is_empty());
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
        if chat {
            self.ai_draw_menu(cv, b);
        }
    }

    /// A rounded chip with an icon, a label and a chevron (mode, model); `None` when it does not
    /// fit before `limit`. Returns where the next one starts.
    #[allow(clippy::too_many_arguments)]
    fn ai_chip(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        cy: i32,
        icon: &str,
        label: &str,
        col: Rgba,
        hit: AiHit,
        limit: i32,
    ) -> Option<i32> {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let w = 8 + 14 + 5 + self.tx.ui_width(label, false) + 4 + 12 + 6;
        if x + w > limit {
            return None;
        }
        let chip = Rect::new(x, cy + 1, w, 26);
        let (mx, my) = self.mouse;
        let hv = self.overlay.is_none() && chip.contains(mx, my);
        cv.fill_round(chip, 13, strong.with_alpha(if hv { 0x1c } else { 0x0e }));
        self.tx.icon(cv, icon, chip.x + 8, chip.y + 6, 14, col, chip);
        let text_col = if col == dim { strong } else { col };
        self.tx.draw_ui(cv, chip.x + 8 + 14 + 5, chip.y + 4, label, text_col, chip, false);
        self.tx.icon(cv, "ui/chevron_down", chip.right() - 6 - 12, chip.y + 7, 12, dim, chip);
        self.ai.hits.push((chip, hit));
        Some(chip.right() + 4)
    }

    /// The autocomplete menu over the composer: rows of commands or files, the selected one lit,
    /// the keys on a line below.
    fn ai_draw_menu(&mut self, cv: &mut Canvas, boxr: Rect) {
        let Some(m) = self.ai.menu.clone() else { return };
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let top = self.aipanel_rect().y + self.ai_top_h() + 4;
        let avail = ((boxr.y - 6 - top - 8 - MENU_HINT_H) / MENU_ROW_H).max(1) as usize;
        let rows = m.entries.len().min(menu::ROWS).min(avail);
        let h = rows as i32 * MENU_ROW_H + 8 + MENU_HINT_H;
        let r = Rect::new(boxr.x, boxr.y - 6 - h, boxr.w, h);
        cv.shadow(r, 10);
        cv.card(r, 10, c.widget_bg, c.border);
        let (mx, my) = self.mouse;
        let first = m.first_row(rows);
        for (k, e) in m.entries.iter().enumerate().skip(first).take(rows) {
            let row = Rect::new(r.x + 4, r.y + 4 + (k - first) as i32 * MENU_ROW_H, r.w - 8, MENU_ROW_H);
            let hot = self.overlay.is_none() && row.contains(mx, my);
            if k == m.sel {
                cv.fill_round(row, 7, c.accent.with_alpha(0x2a));
            } else if hot {
                cv.fill_round(row, 7, strong.with_alpha(0x12));
            }
            let (fg, sub) = if e.off.is_some() { (dim.with_alpha(0xa0), dim.with_alpha(0x80)) } else { (strong, dim) };
            self.tx.icon(cv, e.icon, row.x + 8, row.y + 8, 14, if k == m.sel { c.accent_text } else { dim }, row);
            let tag_w = if e.tag.is_empty() { 0 } else { self.tx.ui_width_sized(&e.tag, false, SMALL) + 8 };
            let label_w = self.tx.ui_width(&e.label, false);
            let lx = row.x + 30;
            let room = row.right() - 8 - tag_w - lx;
            let label = crate::ui::fit_ellipsis(&mut self.tx, &e.label, room.max(40), false);
            self.tx.draw_ui(cv, lx, row.y + 5, &label, fg, row, false);
            let dx = lx + label_w.min(room.max(40)) + 12;
            let droom = row.right() - 8 - tag_w - dx;
            let detail = e.off.as_deref().unwrap_or(&e.detail);
            if droom > 40 && !detail.is_empty() {
                let t = crate::ui::fit_ellipsis(&mut self.tx, detail, droom, false);
                self.tx.draw_ui_sized(cv, dx, row.y + 8, &t, sub, row, false, SMALL);
            }
            if tag_w > 0 {
                let tx0 = row.right() - tag_w;
                self.tx.draw_ui_sized(cv, tx0, row.y + 8, &e.tag, dim.with_alpha(0x90), row, false, SMALL);
            }
            self.ai.hits.push((row, AiHit::MenuRow(k)));
        }
        let keys = if m.kind == menu::MenuKind::Slash {
            "↑↓ move · Tab complete · Enter run · Esc close"
        } else {
            "↑↓ move · Tab or Enter attach · Esc close"
        };
        let hr = Rect::new(r.x + 12, r.bottom() - MENU_HINT_H, r.w - 24, MENU_HINT_H);
        self.tx.draw_ui_sized(cv, hr.x, hr.y + 2, keys, dim.with_alpha(0x90), hr, false, SMALL - 1.0);
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
        if self.ai_is_chat() {
            self.ai_refresh_menu();
            if self.ai_menu_key(&ev.key, ctrl, shift, alt) {
                return true;
            }
            // Shift+Tab: the next permission mode, as in Claude Code's own screen.
            if shift && !ctrl && !alt && matches!(&ev.key, Key::Named(winit::keyboard::NamedKey::Tab)) {
                self.ai_cycle_mode();
                return true;
            }
        }
        if self.chat_scroll_key(&ev.key, ctrl) {
            return true;
        }
        let empty = self.ai.composer.text().is_empty() && self.ai.menu.is_none();
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
        let chat = self.ai_is_chat();
        match self.clip.offer() {
            // The chat attaches copied files (pictures are shown to the agent); a terminal gets paths.
            crate::clipboard::Offer::Files(f) if chat => {
                for p in &f.paths {
                    self.ai_attach_path(p);
                }
            }
            crate::clipboard::Offer::Files(f) => {
                let paths: Vec<String> = f.paths.iter().map(|p| paste_path(p)).collect();
                self.ai.composer.insert(&format!("{} ", paths.join(" ")));
            }
            crate::clipboard::Offer::Text(t) => {
                self.ai.composer.insert(&t);
            }
            crate::clipboard::Offer::Image if chat => {
                if !self.ai_paste_image() {
                    self.say("Could not read the image on the clipboard");
                }
            }
            crate::clipboard::Offer::Image | crate::clipboard::Offer::Empty => {
                if !chat {
                    self.ai_write(&[0x16]);
                }
            }
        }
        self.ai.history.reset();
    }

    /// Send the draft to the agent of the session shown: one paste, then Enter a moment later. An
    /// ended agent starts again first and the draft stays for a second Enter.
    pub fn ai_send(&mut self) {
        let text = self.ai.composer.text().to_string();
        let chat = self.ai_is_chat();
        if text.trim().is_empty() && !(chat && !self.ai.attachments.is_empty()) {
            return;
        }
        if chat {
            self.ai.menu = None;
            // `/command`: Lumen's own run here; the agent's (and skills) go on as a message.
            if text.trim_start().starts_with('/') && self.ai_run_slash(text.trim()) {
                self.ai.history.push(&text);
                self.ai.composer.set_text("");
                self.request_redraw();
                return;
            }
            let (msg, images) = self.ai_compose_message(&text);
            if let Some(s) = self.ai.cur_mut()
                && let Some(c) = s.chat.as_mut()
            {
                let cs: &mut ChatSession = c;
                cs.send_with(&msg, &images);
                s.track.on_input();
            }
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

    /// The composer's paperclip: in the chat it types `@`, which opens the list of the project's
    /// files to attach; in a terminal it mentions the file open in the editor.
    pub fn ai_attach_file(&mut self) {
        self.ai.composer_focus = true;
        if self.ai_is_chat() {
            let cur = self.ai.composer.text();
            let sep = !cur.is_empty() && !cur.ends_with(char::is_whitespace);
            self.ai.composer.insert(if sep { " @" } else { "@" });
            return;
        }
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
