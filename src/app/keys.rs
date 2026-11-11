use super::*;

impl App {
    // ── keyboard ──

    pub fn on_key(&mut self, ev: &KeyEvent) -> bool {
        let ki = KeyIn { key: ev.logical_key.clone(), text: ev.text.as_ref().map(|t| t.to_string()) };
        self.handle_key(&ki)
    }

    pub fn handle_key(&mut self, ev: &KeyIn) -> bool {
        self.compute_layout();
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        if self.hover_key(ev) {
            return false;
        }
        self.hover = None;
        self.dismiss_tooltip();
        let key = ev.key.clone();

        if !self.overlay.is_none() {
            return self.overlay_key(ev);
        }
        if self.update_card_key(&key) {
            self.request_redraw();
            return false;
        }
        if self.gitpanel_key(ev) {
            self.request_redraw();
            return false;
        }
        if self.tests_key(ev) {
            self.request_redraw();
            return false;
        }
        if self.find.as_ref().is_some_and(|f| f.focused) && self.find_key(ev) {
            self.request_redraw();
            return false;
        }

        if self.focus == Focus::Terminal && self.term_key(ev) {
            self.request_redraw();
            return false;
        }

        // User key bindings win over the built-in shortcuts.
        if self.diff.is_some() && self.diff_pane_focused() && self.diff_key(&key) {
            self.request_redraw();
            return false;
        }
        // Second stroke of a chord (`Ctrl+K` then a letter): modifier presses alone keep it pending.
        if let Some(prefix) = self.chord.clone() {
            if matches!(
                &key,
                Key::Named(
                    NamedKey::Shift
                        | NamedKey::Control
                        | NamedKey::Alt
                        | NamedKey::AltGraph
                        | NamedKey::Super
                        | NamedKey::Meta
                        | NamedKey::CapsLock
                )
            ) {
                return false;
            }
            self.chord = None;
            if let Some(id) = self.keymap.lookup_chord(&prefix, ctrl, shift, alt, &key).map(str::to_string) {
                let quit = self.run_bound(&id);
                self.request_redraw();
                return quit;
            }
            self.request_redraw();
            return false; // an unbound second stroke only cancels the chord
        }
        // The explorer's own keys (A, Shift+A, F2, Delete) win over the global table while it has the focus.
        if self.focus == Focus::Sidebar
            && let Some(id) = self.keymap.lookup_explorer(ctrl, shift, alt, &key).map(str::to_string)
        {
            let quit = self.run_bound(&id);
            self.request_redraw();
            return quit;
        }
        // One table: keybindings.json first ("none" disables), then keymap::DEFAULTS.
        if let Some(id) = self.keymap.lookup(ctrl, shift, alt, &key).map(str::to_string) {
            let cmd = crate::app_overlay::Cmd::from_id(&id);
            // Ctrl+Shift+V is also "paste" outside Markdown files: let it through.
            let paste = cmd == Some(crate::app_overlay::Cmd::MarkdownPreview)
                && !self.active_is_markdown()
                && ctrl
                && shift
                && !alt;
            if !paste {
                let quit = self.run_bound(&id);
                self.request_redraw();
                return quit;
            }
        }
        // First stroke of a chord.
        if let Some(c) = self.keymap.chord_start(ctrl, shift, alt, &key) {
            self.chord = Some(c);
            self.request_redraw();
            return false;
        }

        if (ctrl || alt)
            && let Some(r) = self.shortcut(&key, ctrl, alt)
        {
            self.request_redraw();
            return r;
        }
        if let Key::Named(n) = &key
            && let Some(r) = self.named_shortcut(*n, ctrl, shift, alt)
        {
            self.request_redraw();
            return r;
        }

        if self.completion_key(&key) {
            self.request_redraw();
            return false;
        }

        if self.focus == Focus::Sidebar && self.sidebar_key(&key) {
            self.request_redraw();
            return false;
        }

        let page = self.docs.get(self.active).map_or(20.0, |d| (d.visible_lines() - 2.0).max(1.0));
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        let mut typed = None;
        d.reveal = true;
        let multi = !d.ed.extra.is_empty();

        if ctrl && let Key::Character(c) = &key {
            let k = c.to_lowercase();
            // Commands that understand several cursors; anything else collapses to one.
            let multi_aware =
                matches!((k.as_str(), shift), ("d", false) | ("l", true) | ("c", _) | ("x", _) | ("v", _));
            if multi && !multi_aware {
                d.ed.clear_extra();
            }
            match (k.as_str(), shift) {
                ("a", _) => d.ed.select_all(&mut d.buf),
                ("d", false) => d.ed.add_next_occurrence(&mut d.buf),
                ("l", true) => d.ed.select_all_occurrences(&mut d.buf),
                ("c", _) => {
                    let t = d.copy_cmd();
                    self.clip.set(&t);
                }
                ("x", _) => {
                    if multi {
                        let t = d.ed.copy_text_multi(&d.buf);
                        self.clip.set(&t);
                        d.ed.edit_multi(&mut d.buf, EditKind::Other, |e, b| e.backspace(b));
                    } else {
                        let t = d.cut_cmd();
                        self.clip.set(&t);
                    }
                }
                ("v", _) => {
                    let t = self.clip.get();
                    let d = &mut self.docs[self.active];
                    d.ed.paste_multi(&mut d.buf, &t);
                }
                ("d", true) => d.duplicate_cmd(),
                ("k", true) => d.delete_line_cmd(),
                ("]", _) => d.ed.tab(&mut d.buf, false),
                ("[", _) => d.ed.tab(&mut d.buf, true),
                _ => {}
            }
            return self.after_key(None);
        }

        match &key {
            Key::Named(n) => match n {
                NamedKey::ArrowUp if ctrl && alt => d.ed.add_cursor_vertical(&mut d.buf, -1),
                NamedKey::ArrowDown if ctrl && alt => d.ed.add_cursor_vertical(&mut d.buf, 1),
                NamedKey::ArrowLeft => d.ed.move_multi(&mut d.buf, |e, b| e.left(b, shift, ctrl)),
                NamedKey::ArrowRight => d.ed.move_multi(&mut d.buf, |e, b| e.right(b, shift, ctrl)),
                NamedKey::ArrowUp if alt => {
                    d.ed.clear_extra();
                    d.move_lines_cmd(false)
                }
                NamedKey::ArrowDown if alt => {
                    d.ed.clear_extra();
                    d.move_lines_cmd(true)
                }
                NamedKey::ArrowUp if ctrl => {
                    d.reveal = false;
                    d.scroll_by(-1.0)
                }
                NamedKey::ArrowDown if ctrl => {
                    d.reveal = false;
                    d.scroll_by(1.0)
                }
                NamedKey::ArrowUp => d.move_vertical(shift, -1),
                NamedKey::ArrowDown => d.move_vertical(shift, 1),
                NamedKey::PageUp => {
                    d.ed.clear_extra();
                    d.move_vertical(shift, -(page as isize));
                    d.glide_by(-page);
                }
                NamedKey::PageDown => {
                    d.ed.clear_extra();
                    d.move_vertical(shift, page as isize);
                    d.glide_by(page);
                }
                NamedKey::Home if ctrl => {
                    d.ed.clear_extra();
                    d.scr.glide_next = true;
                    d.ed.doc_start(&mut d.buf, shift)
                }
                NamedKey::End if ctrl => {
                    d.ed.clear_extra();
                    d.scr.glide_next = true;
                    d.ed.doc_end(&mut d.buf, shift)
                }
                // With word wrap the first press goes to the start / end of the visual row (every cursor).
                NamedKey::Home => d.home_cmd(shift),
                NamedKey::End => d.end_cmd(shift),
                NamedKey::Backspace => d.ed.edit_multi(&mut d.buf, EditKind::Backspace, |e, b| {
                    if ctrl { e.delete_word(b, false) } else { e.backspace(b) }
                }),
                NamedKey::Delete => d.ed.edit_multi(&mut d.buf, EditKind::Delete, |e, b| {
                    if ctrl { e.delete_word(b, true) } else { e.delete(b) }
                }),
                NamedKey::Enter => d.ed.edit_multi(&mut d.buf, EditKind::Other, |e, b| e.newline(b)),
                NamedKey::Tab if !shift && d.ed.extra.is_empty() && d.snippet_next() => {}
                NamedKey::Tab
                    if !shift && d.ed.extra.is_empty() && d.ed.sel.is_empty() && self.completion.is_none() && {
                        // Snippet prefix right before the cursor?
                        let head = d.ed.head();
                        let mut st = head;
                        while st > 0 && d.buf.char_at(st - 1).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            st -= 1;
                        }
                        let word = d.buf.slice(st, head);
                        match crate::snippets::find(d.lang, &word) {
                            Some(sn) => {
                                d.insert_snippet(st, head, sn.body);
                                true
                            }
                            None => false,
                        }
                    } => {}
                NamedKey::Tab
                    if !shift && d.ed.extra.is_empty() && d.ed.sel.is_empty() && self.completion.is_none() && {
                        // Emmet abbreviation right before the cursor (HTML/Vue/Svelte/JSX/TSX)?
                        let flavor = match d.lang {
                            Some("html" | "vue" | "svelte") => Some((crate::emmet::Flavor::Html, false)),
                            Some("tsx" | "jsx") => Some((crate::emmet::Flavor::Jsx, true)),
                            _ => None,
                        };
                        match flavor {
                            Some((fl, jsx_code)) => {
                                let head = d.ed.head();
                                let (l, col) = d.buf.line_col(head);
                                let prefix: String = d.buf.line(l).chars().take(col).collect();
                                match crate::emmet::extract_abbr(&prefix) {
                                    Some((start_col, abbr))
                                        if crate::snippets::emmet_allowed(
                                            &abbr,
                                            &prefix.chars().take(start_col).collect::<String>(),
                                            jsx_code,
                                        ) =>
                                    {
                                        match crate::emmet::expand(&abbr, fl) {
                                            Some(body) => {
                                                let start = d.buf.line_start(l) + start_col;
                                                d.insert_snippet(start, head, &body);
                                                true
                                            }
                                            None => false,
                                        }
                                    }
                                    _ => false,
                                }
                            }
                            None => false,
                        }
                    } => {}
                NamedKey::Tab => d.ed.edit_multi(&mut d.buf, EditKind::Other, |e, b| e.tab(b, shift)),
                NamedKey::Escape => {
                    d.snippet = None;
                    if d.ed.extra.is_empty() {
                        let h = d.ed.head();
                        d.ed.click(&mut d.buf, h, false);
                        self.find = None;
                        // Nothing else to dismiss: Esc also forgets a pending file cut (the
                        // dimmed explorer rows), whichever pane has the focus.
                        self.cancel_cut();
                    } else {
                        d.ed.clear_extra();
                    }
                }
                _ => typed = self.type_text(ev),
            },
            _ => typed = self.type_text(ev),
        }
        self.after_key(typed)
    }

    pub(super) fn type_text(&mut self, ev: &KeyIn) -> Option<char> {
        let alt = self.mods.alt_key();
        let d = self.docs.get_mut(self.active)?;
        if alt || self.mods.control_key() || self.mods.super_key() {
            d.reveal = false;
            return None;
        }
        let Some(t) = &ev.text else {
            d.reveal = false;
            return None;
        };
        let mut last = None;
        for ch in t.chars().filter(|c| !c.is_control()) {
            d.ed.edit_multi(&mut d.buf, EditKind::Typing, |e, b| e.type_char(b, ch));
            last = Some(ch);
        }
        if last.is_none() {
            d.reveal = false;
        }
        last
    }

    pub fn after_key(&mut self, typed: Option<char>) -> bool {
        self.quit_armed = false;
        self.close_armed = None;
        if let Some(d) = self.docs.get_mut(self.active) {
            d.sync_edits();
        }
        self.on_typed(typed);
        self.request_redraw();
        false
    }

    /// Named keys that are not `Cmd`s (the rest goes through `keymap::DEFAULTS` in `handle_key`).
    pub(super) fn named_shortcut(&mut self, n: NamedKey, ctrl: bool, shift: bool, _alt: bool) -> Option<bool> {
        match n {
            NamedKey::Space if ctrl && shift => {
                self.request_signature();
                Some(false)
            }
            NamedKey::Space if ctrl => {
                self.request_completion(None);
                Some(false)
            }
            _ => None,
        }
    }

    /// Hard-wired shortcuts that are not `Cmd`s (everything else is dispatched through
    /// `keymap::DEFAULTS` in `handle_key`): Alt+digit tab jumps.
    pub(super) fn shortcut(&mut self, key: &Key, ctrl: bool, alt: bool) -> Option<bool> {
        let Key::Character(c) = key else { return None };
        let c = c.to_lowercase();
        if alt && !ctrl {
            let n = c.chars().next().and_then(|ch| ch.to_digit(10)).filter(|&n| n >= 1)?;
            self.activate(crate::app_ctxmenu::tab_order(&self.docs).get(n as usize - 1).copied().unwrap_or(usize::MAX));
            return Some(false);
        }
        None
    }

    pub(super) fn sidebar_key(&mut self, key: &Key) -> bool {
        match key {
            Key::Named(NamedKey::ArrowDown) => self.tree.step(1, self.mods.shift_key()),
            Key::Named(NamedKey::ArrowUp) => self.tree.step(-1, self.mods.shift_key()),
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Enter) => self.sidebar_activate(),
            Key::Character(c)
                if self.mods.control_key() && !self.mods.alt_key() && self.explorer_ctrl_key(c.as_str()) => {}
            Key::Named(NamedKey::ArrowLeft) => self.tree.collapse_or_parent(),
            Key::Named(NamedKey::Escape) => {
                // First Esc forgets a pending cut (the dimmed rows); the next one leaves the explorer.
                if !self.explorer_escape() {
                    self.focus = Focus::Editor;
                }
            }
            _ => return false,
        }
        // Keep the selection in view.
        let sel_y = self.tree.sel as f64 * ROW_H as f64;
        let view = (self.layout.sidebar.h - TAB_H).max(ROW_H) as f64;
        if sel_y < self.tree.scroll {
            self.tree.scroll = sel_y;
        } else if sel_y + ROW_H as f64 > self.tree.scroll + view {
            self.tree.scroll = sel_y + ROW_H as f64 - view;
        }
        true
    }

    /// Change the font size by `delta` px (0 resets), keeping the cursor in view.
    pub fn zoom(&mut self, delta: f32) {
        let size = if delta == 0.0 { self.tx.reset_zoom() } else { self.tx.set_zoom(crate::ui::font_size() + delta) };
        self.config.buffer_font_size = size;
        self.config.save();
        for d in &mut self.docs {
            d.cell_w = self.tx.cell_w;
            d.reveal = true;
        }
        self.say(format!("zoom: {size:.0} px"));
    }

    pub fn try_quit(&mut self) -> bool {
        let dirty: Vec<&str> = self.docs.iter().filter(|d| d.buf.is_dirty()).map(|d| d.name.as_str()).collect();
        if !dirty.is_empty() && !self.quit_armed {
            self.quit_armed = true;
            let list = dirty.join(", ");
            self.say(format!("Unsaved: {list} — Ctrl+S saves, Ctrl+Q again discards"));
            self.request_redraw();
            return false;
        }
        mem::report("exit");
        self.save_session();
        self.stop_task();
        for l in &mut self.lsps {
            l.shutdown();
        }
        true
    }
}
