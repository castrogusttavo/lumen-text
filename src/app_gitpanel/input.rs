use super::*;

impl App {
    // ───────────── input ─────────────

    pub fn gitpanel_key(&mut self, ev: &KeyIn) -> bool {
        let Some(mut gp) = self.gitpanel.take() else { return false };
        let consumed = if gp.focused { self.gp_key(&mut gp, ev) } else { false };
        if gp.closed {
            self.git_detach_job(&mut gp);
            self.focus = crate::app::Focus::Editor;
        } else {
            self.gitpanel = Some(gp);
        }
        consumed
    }

    /// A key the panel does not use: hand the focus back and report "not consumed" so the
    /// global shortcut handler sees it.
    pub(super) fn gp_pass(gp: &mut GitPanel) -> bool {
        gp.focused = false;
        gp.msg_focus = false;
        false
    }

    pub(super) fn gp_key(&mut self, gp: &mut GitPanel, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let key = &ev.key;
        let entered = matches!(key, Key::Named(NamedKey::Enter));
        if entered && ctrl && gp.tab == Tab::Changes {
            self.gp_commit(gp);
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if gp.confirm.take().is_some()
                || gp.publish.take().is_some()
                || gp.force.take().is_some()
                || std::mem::take(&mut gp.strategy_ask)
            {
            } else if gp.msg_focus && gp.tab == Tab::Changes && self.diff.is_none() {
                gp.msg_focus = false;
            } else if self.diff.is_some() {
                self.diff = None;
            } else {
                gp.closed = true;
            }
            return true;
        }
        if is_modifier_key(key) {
            return true;
        }
        // Anything bound to a command (Ctrl+P, Ctrl+Shift+G, ...) keeps working.
        let bound = (ctrl || alt) && self.keymap.lookup(ctrl, shift, alt, key).is_some();
        if bound {
            return Self::gp_pass(gp);
        }
        if gp.msg_focus && gp.tab == Tab::Changes {
            let f = &mut gp.msg;
            match key {
                Key::Named(NamedKey::Enter) => ml_insert(f, "\n"),
                Key::Named(NamedKey::Backspace) => {
                    if ctrl {
                        f.delete_word();
                    } else {
                        f.backspace();
                    }
                }
                Key::Named(NamedKey::Delete) => {
                    f.delete();
                }
                Key::Named(NamedKey::ArrowLeft) => f.left(),
                Key::Named(NamedKey::ArrowRight) => f.right(),
                Key::Named(NamedKey::ArrowUp) => ml_vertical(f, -1),
                Key::Named(NamedKey::ArrowDown) => ml_vertical(f, 1),
                Key::Named(NamedKey::Home) => ml_home(f),
                Key::Named(NamedKey::End) => ml_end(f),
                Key::Named(NamedKey::Tab) => gp.msg_focus = false,
                Key::Character(c) if ctrl && !alt && c.to_lowercase() == "v" => {
                    let t = self.clip.get();
                    ml_insert(&mut gp.msg, &t);
                }
                // Any other Ctrl/Alt combo has no meaning here: let the global shortcut run.
                _ if ctrl || alt => return Self::gp_pass(gp),
                _ => {
                    if let Some(t) = &ev.text {
                        ml_insert(f, t);
                    }
                }
            }
            return true;
        }
        // List mode. Ctrl/Alt + a character key means nothing here (the arrows, Enter, Space,
        // Delete and Tab below keep working with modifiers): let the global shortcut run.
        if (ctrl || alt) && matches!(key, Key::Character(_)) {
            return Self::gp_pass(gp);
        }
        let rows = gp.rows();
        match key {
            Key::Named(NamedKey::ArrowDown) => gp.move_cursor(1),
            Key::Named(NamedKey::ArrowUp) => gp.move_cursor(-1),
            Key::Named(NamedKey::ArrowLeft) => gp.tab = Tab::Changes,
            Key::Named(NamedKey::ArrowRight) => {
                gp.tab = Tab::History;
                if gp.commits.is_empty() {
                    gp.load_more();
                }
            }
            Key::Named(NamedKey::Tab) => {
                if gp.tab == Tab::Changes {
                    gp.msg_focus = true;
                } else {
                    gp.tab = Tab::Changes;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(r) = rows.get(gp.cursor).copied() {
                    self.gp_activate(gp, r);
                }
            }
            Key::Named(NamedKey::Space) => match rows.get(gp.cursor).copied() {
                Some(Row::File(i)) => self.gp_toggle_stage(gp, i),
                Some(Row::Header(g)) => self.gp_section(gp, g),
                Some(r) => self.gp_activate(gp, r),
                None => {}
            },
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if let Some(Row::File(i)) = rows.get(gp.cursor).copied()
                    && gp.changes[i].discardable()
                {
                    gp.confirm = Some(i);
                }
            }
            Key::Named(NamedKey::PageUp | NamedKey::PageDown | NamedKey::Home | NamedKey::End)
                if self.diff.is_some() =>
            {
                self.diff_key(key);
            }
            Key::Character(c) if matches!(c.as_str(), "n" | "p" | "s" | "q") && self.diff.is_some() => {
                self.diff_key(key);
            }
            Key::Character(c) if c.as_str() == "y" && gp.confirm.is_some() => {
                if let Some(i) = gp.confirm.take() {
                    self.gp_discard(gp, i);
                }
            }
            Key::Character(c) if c.as_str() == "r" => {
                gp.refresh();
                gp.reset_history();
            }
            _ if ctrl || alt => return Self::gp_pass(gp),
            _ => {}
        }
        let view = self.gitpanel_rect().h - gp.chrome_h() - if gp.tab == Tab::Changes { COMMIT_AREA_H } else { 0 };
        gp.reveal(view);
        true
    }

    /// Right click on the Pull button: choose how pulls integrate (merge / rebase / ff-only).
    pub fn gitpanel_right_click(&mut self, x: f64, y: f64) -> bool {
        let Some(gp) = self.gitpanel.as_mut() else { return false };
        if gp.top.is_none() || gp.job.is_some() || gp.hit_at(x, y) != Some(Hit::Pull) {
            return false;
        }
        gp.sync_note = None;
        gp.publish = None;
        gp.force = None;
        gp.strategy_ask = true;
        true
    }

    pub fn gitpanel_mouse(&mut self, x: f64, y: f64) -> bool {
        // A click on a "stage this hunk" button of the open diff.
        if self.diff_click(x, y) {
            return true;
        }
        if self.gp_on_edge(x, y) {
            if let Some(gp) = self.gitpanel.as_mut() {
                gp.resizing = true;
                gp.focused = true;
            }
            return true;
        }
        let Some(mut gp) = self.gitpanel.take() else { return false };
        let p = self.gitpanel_rect();
        let inside = p.contains(x, y);
        if !inside {
            gp.focused = false;
            gp.msg_focus = false;
            gp.confirm = None;
        } else {
            gp.focused = true;
            self.focus = crate::app::Focus::Editor;
            let hit = gp.hit_at(x, y);
            let keep_confirm = matches!(hit, Some(Hit::ConfirmDiscard(_)));
            if !keep_confirm {
                gp.confirm = None;
            }
            if !matches!(hit, Some(Hit::Msg)) {
                gp.msg_focus = false;
            }
            let rows = gp.rows();
            match hit {
                Some(Hit::Tab(t)) => {
                    gp.tab = t;
                    gp.cursor = 0;
                    if t == Tab::History && gp.commits.is_empty() {
                        gp.load_more();
                    }
                }
                Some(Hit::Refresh) => {
                    gp.refresh();
                    gp.reset_history();
                    self.refresh_git_status();
                }
                Some(Hit::Close) => gp.closed = true,
                Some(Hit::Check(i)) => self.gp_toggle_stage(&mut gp, i),
                Some(Hit::Discard(i)) => gp.confirm = Some(i),
                Some(Hit::ConfirmDiscard(i)) => self.gp_discard(&mut gp, i),
                Some(Hit::Row(i)) => {
                    if let Some(k) = rows.iter().position(|r| *r == Row::File(i)) {
                        gp.cursor = k;
                    }
                    self.gp_open_diff_change(&mut gp, i);
                }
                Some(Hit::Section(g)) => self.gp_section(&mut gp, g),
                Some(Hit::Msg) => {
                    gp.msg_focus = true;
                    let b = gp.msg_box;
                    let line = gp.msg_top + ((y as i32 - b.y - 8).max(0) / LINE) as usize;
                    let text_line = gp
                        .msg
                        .text
                        .split('\n')
                        .nth(line.min(gp.msg.text.split('\n').count() - 1))
                        .unwrap_or("")
                        .to_string();
                    let target = x as i32 - (b.x + 12) + gp.msg_hscroll;
                    let mut col = 0;
                    for n in 1..=text_line.chars().count() {
                        let pre: String = text_line.chars().take(n).collect();
                        let w = self.tx.ui_width(&pre, false);
                        let prev: String = text_line.chars().take(n - 1).collect();
                        let wp = self.tx.ui_width(&prev, false);
                        if target >= (w + wp) / 2 {
                            col = n;
                        } else {
                            break;
                        }
                    }
                    gp.msg.cursor = ml_index(&gp.msg, line, col);
                }
                Some(Hit::Opt) => gp.include_all = !gp.include_all,
                Some(Hit::Amend) => self.gp_toggle_amend(&mut gp),
                Some(Hit::Fetch) => self.gp_start(&mut gp, Op::Fetch),
                Some(Hit::Pull) => self.gp_pull(&mut gp),
                Some(Hit::Push) => self.gp_push(&mut gp),
                Some(Hit::PublishYes) => {
                    if let Some((remote, branch)) = gp.publish.take() {
                        self.gp_start(&mut gp, Op::Publish { remote, branch });
                    }
                }
                Some(Hit::PublishNo) => gp.publish = None,
                Some(Hit::ForceYes) => {
                    if gp.force.take().is_some() {
                        self.gp_start(&mut gp, Op::ForcePush);
                    }
                }
                Some(Hit::ForceNo) => gp.force = None,
                Some(Hit::Strategy(st)) => self.gp_pull_with(&mut gp, st),
                Some(Hit::StrategyNo) => gp.strategy_ask = false,
                Some(Hit::JobCancel) => {
                    if let Some(j) = gp.job.as_mut() {
                        j.cancel();
                    }
                }
                Some(Hit::Note) => gp.sync_note = None,
                Some(Hit::CommitBtn) => self.gp_commit(&mut gp),
                Some(Hit::Commit(i)) => {
                    if let Some(k) = rows.iter().position(|r| *r == Row::Commit(i)) {
                        gp.cursor = k;
                    }
                    self.gp_toggle_commit(&mut gp, i);
                }
                Some(Hit::CFile(j)) => {
                    if let Some(k) = rows.iter().position(|r| *r == Row::CFile(j)) {
                        gp.cursor = k;
                    }
                    self.gp_open_diff_commit(&mut gp, j);
                }
                None => {}
            }
        }
        if gp.closed {
            self.git_detach_job(&mut gp);
            self.focus = crate::app::Focus::Editor;
        } else {
            self.gitpanel = Some(gp);
        }
        inside
    }

    pub fn gitpanel_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let p = self.gitpanel_rect();
        let Some(gp) = self.gitpanel.as_mut() else { return false };
        if !p.contains(x, y) {
            return false;
        }
        let px = crate::scroller::wheel_px(delta, crate::ui::line_h() as f64);
        let k = if gp.tab == Tab::Changes { 0 } else { 1 };
        gp.scroll[k] =
            (gp.scroll[k] + px).clamp(0.0, gp.max_scroll[k].max(if k == 1 && !gp.log_done { f64::MAX } else { 0.0 }));
        true
    }

    /// Pointer moved: true when the panel needs a redraw (hover changed).
    pub fn gitpanel_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let p = self.gitpanel_rect();
        let win = self.size.0 as i32;
        let Some(gp) = self.gitpanel.as_mut() else { return false };
        if gp.resizing {
            // The panel is docked on the right, so its width is the distance to the window edge.
            let w = clamp_panel_width(win - x as i32, win);
            if self.config.git_panel_width != w {
                self.config.git_panel_width = w;
                return true;
            }
            return false;
        }
        let h = if p.contains(x, y) { gp.hit_at(x, y) } else { None };
        let changed = h != gp.hover;
        gp.hover = h;
        changed
    }

    /// Cursor over the panel: hand on clickable things, I-beam in the message box.
    pub fn gitpanel_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let gp = self.gitpanel.as_ref()?;
        if gp.resizing || self.gp_on_edge(x, y) {
            return Some(CursorIcon::ColResize);
        }
        if !self.gitpanel_rect().contains(x, y) {
            return None;
        }
        Some(match gp.hit_at(x, y) {
            Some(Hit::Msg) => CursorIcon::Text,
            Some(_) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        })
    }
}
