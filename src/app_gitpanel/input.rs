use super::*;

impl App {
    // ───────────── input ─────────────

    pub fn gitpanel_key(&mut self, ev: &KeyIn) -> bool {
        let Some(mut gp) = self.gitpanel.take() else { return false };
        let consumed = if gp.focused { self.gp_key(&mut gp, ev) } else { false };
        let pending = gp.pending.take();
        if gp.closed {
            self.git_detach_job(&mut gp);
            self.focus = crate::app::Focus::Editor;
        } else {
            self.gitpanel = Some(gp);
        }
        // Actions that open cards or prompts run with the panel back in place.
        if let Some(act) = pending {
            self.git_act(act);
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
        if gp.filter_focus && gp.tab == Tab::Changes {
            return self.gp_filter_key(gp, ev);
        }
        if entered && ctrl && gp.tab == Tab::Changes {
            self.gp_commit(gp);
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if gp.menu.take().is_some()
                || gp.form.take().is_some()
                || gp.confirm.take().is_some()
                || gp.publish.take().is_some()
                || gp.force.take().is_some()
                || gp.ask.take().is_some()
                || std::mem::take(&mut gp.strategy_ask)
                || !std::mem::take(&mut gp.selected).is_empty()
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
        // Anything bound to a command (Ctrl+P, Ctrl+Shift+G, ...) keeps working, except the
        // editing keys of the message box (Ctrl+A / C / X / V / Z / Y, word moves...).
        let bound = (ctrl || alt) && self.keymap.lookup(ctrl, shift, alt, key).is_some();
        if gp.form.is_some() && gp.tab == Tab::Changes {
            return self.gp_form_key(gp, ev, bound);
        }
        if gp.msg_focus && gp.tab == Tab::Changes {
            return self.gp_msg_key(gp, ev, bound);
        }
        if bound {
            return Self::gp_pass(gp);
        }
        // List mode. Ctrl/Alt + a character key means nothing here (the arrows, Enter, Space,
        // Delete and Tab below keep working with modifiers): let the global shortcut run.
        if (ctrl || alt) && matches!(key, Key::Character(_)) {
            return Self::gp_pass(gp);
        }
        let at_cursor = gp.row_at(gp.cursor);
        match key {
            Key::Named(NamedKey::ArrowDown) => gp.move_cursor(1),
            Key::Named(NamedKey::ArrowUp) => gp.move_cursor(-1),
            Key::Named(NamedKey::ArrowLeft) => {
                if gp.tab != Tab::Changes {
                    gp.tab = Tab::Changes;
                    gp.cursor = 0;
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if gp.tab != Tab::History {
                    gp.tab = Tab::History;
                    gp.cursor = 0;
                }
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
                if let Some(r) = at_cursor {
                    self.gp_activate(gp, r);
                }
            }
            Key::Named(NamedKey::Space) => match at_cursor {
                Some(Row::File(i)) => self.gp_toggle_stage(gp, i),
                Some(Row::Header(g)) => self.gp_section(gp, g),
                Some(r) => self.gp_activate(gp, r),
                None => {}
            },
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => match at_cursor {
                Some(Row::File(i)) if gp.changes[i].discardable() => gp.confirm = Some(i),
                Some(Row::Stash(i)) => {
                    if let Some(e) = gp.stashes.get(i) {
                        gp.pending = Some(GitAct::StashDropAsk { sha: e.sha.clone() });
                    }
                }
                _ => {}
            },
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
        let view = self.gitpanel_rect().h
            - gp.chrome_h()
            - gp.filter_h()
            - if gp.tab == Tab::Changes { gp.commit_area_h() } else { 0 };
        gp.reveal(view);
        true
    }

    /// A key typed into the "Filter changes" box: Esc clears it (then leaves it), Enter leaves it.
    fn gp_filter_key(&mut self, gp: &mut GitPanel, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        match &ev.key {
            Key::Named(NamedKey::Escape) => {
                if gp.filter.text.is_empty() {
                    gp.filter_focus = false;
                } else {
                    gp.filter = TextField::default();
                }
                return true;
            }
            Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                gp.filter_focus = false;
                return true;
            }
            _ => {}
        }
        match gp.filter.key(&ev.key, ctrl, shift, alt) {
            KeyAct::Handled => {}
            KeyAct::Copy(t) => self.clip.set(&t),
            KeyAct::Paste => {
                let t = self.clip.get();
                gp.filter.insert(&t.replace(['\n', '\r'], " "));
            }
            KeyAct::Ignored => match ev.text.as_deref() {
                Some(t) if !ctrl && !alt && !t.is_empty() && !t.chars().any(char::is_control) => gp.filter.insert(t),
                _ => {
                    gp.filter_focus = false;
                    return Self::gp_pass(gp);
                }
            },
        }
        gp.scroll[0] = 0.0;
        gp.cursor = 0;
        true
    }

    /// A key while the pull request form is open: Tab switches between the title and the description,
    /// Enter in the title goes to the description, Ctrl+Enter creates, the rest edits the box.
    fn gp_form_key(&mut self, gp: &mut GitPanel, ev: &KeyIn, bound: bool) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let Some(f) = gp.form.as_mut() else { return false };
        if matches!(ev.key, Key::Named(NamedKey::Tab)) && !ctrl && !alt {
            f.focus = if f.focus == FormField::Title { FormField::Body } else { FormField::Title };
            return true;
        }
        if matches!(ev.key, Key::Named(NamedKey::Enter)) {
            if ctrl {
                gp.submit_form(false);
                return true;
            }
            if f.focus == FormField::Title {
                f.focus = FormField::Body;
                return true;
            }
        }
        let tx = &mut self.tx;
        let area = if f.focus == FormField::Title { &mut f.title } else { &mut f.body };
        area.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
        match area.key(&ev.key, ctrl, shift, alt) {
            KeyAct::Handled => return true,
            KeyAct::Copy(t) => {
                self.clip.set(&t);
                return true;
            }
            KeyAct::Paste => {
                let t = self.clip.get();
                let t = if f.focus == FormField::Title { t.replace(['\n', '\r'], " ") } else { t };
                area.insert(&t);
                return true;
            }
            KeyAct::Ignored => {}
        }
        let text = ev.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
        let altgr = ctrl && alt && text.is_some() && !bound;
        if (ctrl || alt) && !altgr {
            return Self::gp_pass(gp);
        }
        if let Some(t) = text {
            area.type_text(t);
        }
        true
    }

    /// A key while the message box has the focus. `bound`: the key runs a command elsewhere.
    fn gp_msg_key(&mut self, gp: &mut GitPanel, ev: &KeyIn, bound: bool) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if matches!(ev.key, Key::Named(NamedKey::Tab)) && !ctrl && !alt {
            gp.msg_focus = false;
            return true;
        }
        // The text may have changed since the last frame: Up / Down / Home / End need its lines.
        let tx = &mut self.tx;
        gp.msg.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
        match gp.msg.key(&ev.key, ctrl, shift, alt) {
            KeyAct::Handled => return true,
            KeyAct::Copy(t) => {
                self.clip.set(&t);
                return true;
            }
            KeyAct::Paste => {
                let t = self.clip.get();
                gp.msg.insert(&t);
                return true;
            }
            KeyAct::Ignored => {}
        }
        let text = ev.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
        // AltGr arrives as Ctrl+Alt on some systems: its characters are typed, not shortcuts.
        let altgr = ctrl && alt && text.is_some() && !bound;
        if (ctrl || alt) && !altgr {
            // No meaning here: let the global shortcut run.
            return Self::gp_pass(gp);
        }
        if let Some(t) = text {
            gp.msg.type_text(t);
        }
        true
    }

    /// Right click on the Pull button: choose how pulls integrate (merge / rebase / ff-only).
    /// On a History row: the commit menu (cherry-pick, copy hash, branch / tag here, undo).
    pub fn gitpanel_right_click(&mut self, x: f64, y: f64) -> bool {
        let Some(gp) = self.gitpanel.as_mut() else { return false };
        if let Some(Hit::Commit(i)) = gp.hit_at(x, y)
            && gp.top.is_some()
            && let Some(e) = gp.commits.get(i)
        {
            let branches =
                gp.chips_of(i).iter().filter(|c| c.kind == ChipKind::Branch).map(|c| c.name.clone()).collect();
            let c = CommitRef { hash: e.hash.clone(), head: gp.is_head(i), branches };
            if let Some(k) = gp.index_of(Row::Commit(i)) {
                gp.cursor = k;
            }
            gp.focused = true;
            self.git_open_commit_menu(c, (x as i32, y as i32));
            return true;
        }
        // A changed file: the selection (or just this file) can be stashed from its menu.
        if let Some(Hit::Row(i)) = gp.hit_at(x, y)
            && let Some(top) = gp.top.clone()
            && let Some(ch) = gp.changes.get(i)
        {
            if !gp.selected.contains(&ch.path) {
                gp.selected.clear();
                gp.sel_anchor = None;
            }
            let rels: Vec<String> =
                if gp.selected.is_empty() { vec![ch.path.clone()] } else { gp.selected.iter().cloned().collect() };
            gp.focused = true;
            let paths = rels.iter().map(|r| top.join(r)).collect();
            self.open_ctxmenu(crate::app_ctxmenu::Target::GitFiles(paths), (x as i32, y as i32));
            return true;
        }
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
        let Some(mut gp) = self.gitpanel.take() else { return false };
        let p = self.gitpanel_rect();
        let inside = p.contains(x, y);
        // Card buttons run once the panel is back in place (they may reopen or refresh it).
        let mut deferred: Option<GitAct> = None;
        let mut undo = false;
        let mut merge_path: Option<PathBuf> = None;
        let mut branch_anchor: Option<Rect> = None;
        let mut open_url: Option<String> = None;
        let mut ai_prompt: Option<String> = None;
        if !inside {
            gp.focused = false;
            gp.msg_focus = false;
            gp.confirm = None;
            gp.menu = None;
        } else {
            gp.focused = true;
            self.focus = crate::app::Focus::Editor;
            // Any click closes the open dropdown; an item of it acts like the same button on the panel.
            let open_menu = gp.menu.take();
            let mut hit = gp.hit_at(x, y);
            if let Some(Hit::MenuItem(n)) = hit {
                hit = open_menu.and_then(|m| gp.menu_items(m).get(n).map(|it| it.hit));
            }
            let toggled = if let Some(Hit::Menu(m)) = hit { (open_menu != Some(m)).then_some(m) } else { None };
            let keep_confirm = matches!(hit, Some(Hit::ConfirmDiscard(_)));
            if !keep_confirm {
                gp.confirm = None;
            }
            if !matches!(hit, Some(Hit::Msg)) {
                gp.msg_focus = false;
            }
            gp.filter_focus = matches!(hit, Some(Hit::Filter));
            let (ctrl, shift) = (self.mods.control_key(), self.mods.shift_key());
            match hit {
                Some(Hit::Tab(t)) => {
                    gp.tab = t;
                    gp.cursor = 0;
                    if t == Tab::Checks {
                        gp.scroll_pr = 0.0;
                        gp.detail.loaded = false;
                    }
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
                Some(Hit::Merge(i)) => {
                    merge_path = gp.top.as_ref().zip(gp.changes.get(i)).map(|(t, c)| t.join(&c.path))
                }
                Some(Hit::ConfirmDiscard(i)) => self.gp_discard(&mut gp, i),
                Some(Hit::Row(i)) => {
                    if let Some(k) = gp.index_of(Row::File(i)) {
                        gp.cursor = k;
                    }
                    if ctrl || shift {
                        gp.select_change(i, ctrl, shift);
                    } else {
                        gp.selected.clear();
                        gp.sel_anchor = Some(i);
                        self.gp_open_diff_change(&mut gp, i);
                    }
                }
                Some(Hit::StashNew) => deferred = Some(GitAct::StashStart),
                Some(Hit::Stash(i)) => self.gp_toggle_stash(&mut gp, i),
                Some(Hit::StashApply(i) | Hit::StashPop(i)) => {
                    let pop = matches!(hit, Some(Hit::StashPop(_)));
                    deferred = gp.stashes.get(i).map(|e| GitAct::StashApply { sha: e.sha.clone(), pop });
                }
                Some(Hit::StashDrop(i)) => {
                    deferred = gp.stashes.get(i).map(|e| GitAct::StashDropAsk { sha: e.sha.clone() });
                }
                Some(Hit::SFile(j)) => {
                    if let Some(k) = gp.index_of(Row::SFile(j)) {
                        gp.cursor = k;
                    }
                    self.gp_open_diff_stash(&mut gp, j);
                }
                Some(Hit::Section(g)) => self.gp_section(&mut gp, g),
                Some(Hit::Folder(g, i)) => self.gp_toggle_dir(&mut gp, g, i),
                Some(Hit::DiscardAll(g)) => deferred = Some(GitAct::DiscardAllAsk { group: g }),
                Some(Hit::Menu(_)) => gp.menu = toggled,
                Some(Hit::MenuItem(_)) => {}
                Some(Hit::Branches) => branch_anchor = Some(Rect::new(p.x + 8, p.y + HEAD_H, p.w / 2, BRANCH_H)),
                Some(Hit::Generate) => gp.generate_message(),
                Some(Hit::Search) => {
                    gp.tab = Tab::Changes;
                    gp.filter_focus = true;
                }
                Some(Hit::PrOpen) => gp.open_form(),
                Some(Hit::PrCancel) => gp.form = None,
                Some(Hit::PrGenerate) => gp.generate_pr(),
                Some(Hit::PrTitle | Hit::PrBody) => {
                    let title = matches!(hit, Some(Hit::PrTitle));
                    if let Some(f) = gp.form.as_mut() {
                        f.focus = if title { FormField::Title } else { FormField::Body };
                        let b = if title { f.title_box } else { f.body_box };
                        let (ox, oy) = (b.x + 12, b.y + if title { 11 } else { 10 });
                        let tx = &mut self.tx;
                        let area = if title { &mut f.title } else { &mut f.body };
                        area.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
                        area.press((x - ox as f64) as f32, (y - oy as f64) as f32, 1, self.mods.shift_key());
                    }
                }
                Some(Hit::PrCreate) => gp.submit_form(false),
                Some(Hit::PrCreateDraft) => gp.submit_form(true),
                Some(Hit::PrBase) => {
                    if gp.form.is_none()
                        && let Some(i) = gp.detail.info.as_ref()
                    {
                        open_url = Some(i.url.clone());
                    }
                }
                Some(Hit::PrFix) => ai_prompt = gp.detail.info.as_ref().map(fix_prompt),
                Some(Hit::Filter) => {}
                Some(Hit::FilterClear) => {
                    gp.filter = TextField::default();
                    gp.filter_focus = true;
                    gp.scroll[0] = 0.0;
                }
                Some(Hit::Init) => match git_run(&gp.root.clone(), &["init"]) {
                    Ok(_) => {
                        let mut fresh = Box::new(GitPanel::new(&gp.root));
                        fresh.wake = gp.wake.clone();
                        fresh.say("Repository initialized", true);
                        gp = fresh;
                        self.refresh_git_status();
                    }
                    Err(e) => gp.say(format!("git init: {e}"), false),
                },
                Some(Hit::CommitPush) => self.gp_commit_push(&mut gp),
                Some(Hit::PullChoose) => {
                    if gp.top.is_some() && gp.job.is_none() {
                        gp.sync_note = None;
                        gp.publish = None;
                        gp.force = None;
                        gp.strategy_ask = true;
                    }
                }
                Some(Hit::Toggle(g)) => self.gp_toggle_group(&mut gp, g),
                Some(Hit::Msg) => {
                    gp.msg_focus = true;
                    let clicks = gp.msg_click_count(x, y);
                    let (ox, oy) = gp.msg_origin();
                    let tx = &mut self.tx;
                    gp.msg.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
                    gp.msg.press((x - ox) as f32, (y - oy) as f32, clicks, self.mods.shift_key());
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
                Some(Hit::Ask(i)) => {
                    deferred = gp.card().and_then(|a| a.buttons.get(i).map(|b| b.act.clone()));
                    gp.ask = None;
                }
                Some(Hit::UndoLast) => undo = true,
                Some(Hit::JobCancel) => {
                    if let Some(j) = gp.job.as_mut() {
                        j.cancel();
                    }
                }
                Some(Hit::Note) => gp.sync_note = None,
                Some(Hit::CommitBtn) => self.gp_commit(&mut gp),
                Some(Hit::Commit(i)) => {
                    if let Some(k) = gp.index_of(Row::Commit(i)) {
                        gp.cursor = k;
                    }
                    self.gp_toggle_commit(&mut gp, i);
                }
                Some(Hit::CFile(j)) => {
                    if let Some(k) = gp.index_of(Row::CFile(j)) {
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
        if let Some(act) = deferred {
            self.git_act(act);
        }
        if undo {
            self.git_undo_commit();
        }
        if let Some(p) = merge_path {
            self.merge_open(&p);
        }
        if let Some(a) = branch_anchor {
            self.open_topmenu(crate::app_topbar::MenuKind::Branch, a);
        }
        if let Some(u) = open_url {
            crate::paths::open_url(&u);
        }
        if let Some(t) = ai_prompt {
            self.ai_ask(&t);
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
        // Over a message box with more text than it shows: the box scrolls.
        if gp.tab == Tab::Changes && gp.msg_box.contains(x, y) && gp.msg.max_scroll() > 0.0 {
            let px = crate::scroller::wheel_px(delta, Self::gp_msg_line() as f64);
            gp.msg.scroll_by(px as f32);
            return true;
        }
        if gp.tab == Tab::Checks {
            let px = crate::scroller::wheel_px(delta, crate::ui::line_h() as f64);
            gp.scroll_pr = (gp.scroll_pr + px).clamp(0.0, gp.max_scroll_pr);
            return true;
        }
        if let Some(f) = gp.form.as_mut()
            && f.body_box.contains(x, y)
            && f.body.max_scroll() > 0.0
        {
            let px = crate::scroller::wheel_px(delta, 18.0);
            f.body.scroll_by(px as f32);
            return true;
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
        let Some(gp) = self.gitpanel.as_mut() else { return false };
        // Selecting in the message box with the button down (also outside the box).
        if gp.msg.dragging() {
            let (ox, oy) = gp.msg_origin();
            let tx = &mut self.tx;
            gp.msg.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
            gp.msg.drag_to((x - ox) as f32, (y - oy) as f32);
            return true;
        }
        let h = if p.contains(x, y) { gp.hit_at(x, y) } else { None };
        let changed = h != gp.hover;
        gp.hover = h;
        changed
    }

    /// Cursor over the panel: hand on clickable things, I-beam in the message box.
    pub fn gitpanel_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let gp = self.gitpanel.as_ref()?;
        if !self.gitpanel_rect().contains(x, y) {
            return None;
        }
        Some(match gp.hit_at(x, y) {
            Some(Hit::Msg | Hit::Filter | Hit::PrTitle | Hit::PrBody) => CursorIcon::Text,
            Some(Hit::PrBase) if gp.form.is_some() => CursorIcon::Default,
            Some(_) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        })
    }
}

/// What "fix with AI" tells the agent: the pull request and every review comment on it.
fn fix_prompt(pr: &PrInfo) -> String {
    let mut s = format!("Inspect and fix the review feedback for PR #{} ({}).\n", pr.number, pr.title);
    for c in &pr.comments {
        let place = if c.place.is_empty() { String::new() } else { format!(" on {}", c.place) };
        s.push_str(&format!("\n- {}{}: {}", c.author, place, clean_markdown(&c.body).replace('\n', " ")));
        for r in &c.replies {
            s.push_str(&format!("\n  - reply from {}: {}", r.author, clean_markdown(&r.body).replace('\n', " ")));
        }
    }
    s
}
