use super::*;

impl App {
    // ───────────── overlay input ─────────────

    pub fn overlay_key(&mut self, ev: &KeyIn) -> bool {
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_key(ev);
            return false;
        }
        if matches!(self.overlay, Overlay::TopMenu(_)) {
            self.topmenu_key(ev);
            return false;
        }
        if matches!(self.overlay, Overlay::Ctx(_)) {
            self.ctxmenu_key(ev);
            return false;
        }
        if matches!(self.overlay, Overlay::Font(_)) {
            self.fontpicker_key(ev);
            return false;
        }
        if matches!(self.overlay, Overlay::Font(_)) {
            self.fontpicker_key(ev);
            return false;
        }
        // Project search: replace row, whole-word toggle, "replace all" question.
        if self.project_search_key(ev) {
            self.request_redraw();
            return false;
        }
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        let key = ev.key.clone();

        // Shared text-field editing.
        enum Next {
            Close,
            Accept,
            Changed,
            Nothing,
        }
        let mut paste: Option<String> = None;
        if ctrl && let Key::Character(c) = &key {
            match c.to_lowercase().as_str() {
                "v" => paste = Some(self.clip.get()),
                "a" => {
                    match &mut self.overlay {
                        Overlay::Picker(p) => p.field.select_all(),
                        Overlay::Prompt(p) => p.field.select_all(),
                        _ => {}
                    }
                    self.request_redraw();
                    return false;
                }
                _ => {}
            }
        }

        // Take the field out while editing so the picker can be borrowed independently.
        let mut field = match &mut self.overlay {
            Overlay::Picker(p) => std::mem::take(&mut p.field),
            Overlay::Prompt(p) => std::mem::take(&mut p.field),
            Overlay::None | Overlay::Settings(_) | Overlay::TopMenu(_) | Overlay::Ctx(_) | Overlay::Font(_) => {
                return false;
            }
        };
        let sel_move = |app: &mut App, d: isize| {
            if let Overlay::Picker(p) = &mut app.overlay {
                p.move_sel(d)
            }
        };
        let next = match &key {
            Key::Named(NamedKey::Escape) => Next::Close,
            Key::Named(NamedKey::Enter) => Next::Accept,
            Key::Named(NamedKey::ArrowDown) => {
                sel_move(self, 1);
                Next::Nothing
            }
            Key::Named(NamedKey::ArrowUp) => {
                sel_move(self, -1);
                Next::Nothing
            }
            Key::Named(NamedKey::PageDown) => {
                sel_move(self, PICKER_ROWS as isize);
                Next::Nothing
            }
            Key::Named(NamedKey::PageUp) => {
                sel_move(self, -(PICKER_ROWS as isize));
                Next::Nothing
            }
            Key::Named(NamedKey::Tab) => {
                sel_move(self, if shift { -1 } else { 1 });
                Next::Nothing
            }
            Key::Named(NamedKey::Backspace) => {
                let ch = if ctrl { field.delete_word() } else { field.backspace() };
                if ch { Next::Changed } else { Next::Nothing }
            }
            Key::Named(NamedKey::Delete) => {
                if field.delete() {
                    Next::Changed
                } else {
                    Next::Nothing
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                field.left();
                Next::Nothing
            }
            Key::Named(NamedKey::ArrowRight) => {
                field.right();
                Next::Nothing
            }
            Key::Named(NamedKey::Home) => {
                field.home();
                Next::Nothing
            }
            Key::Named(NamedKey::End) => {
                field.end();
                Next::Nothing
            }
            Key::Character(c)
                if alt
                    && c.to_lowercase() == "w"
                    && matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickKind::Problems) =>
            {
                let project = !self.problems_project();
                self.set_problems_scope(project);
                Next::Changed
            }
            Key::Character(c) if alt && (c.as_str() == "c" || c.as_str() == "r") => {
                if let Overlay::Picker(p) = &mut self.overlay {
                    if c.as_str() == "c" { p.case = !p.case } else { p.regex = !p.regex }
                }
                Next::Changed
            }
            _ => {
                if let Some(t) = paste.take() {
                    field.insert(&t.replace('\n', " "));
                    Next::Changed
                } else if !ctrl && !alt {
                    match &ev.text {
                        Some(t) if !t.is_empty() => {
                            field.insert(t);
                            Next::Changed
                        }
                        _ => Next::Nothing,
                    }
                } else {
                    Next::Nothing
                }
            }
        };
        match &mut self.overlay {
            Overlay::Picker(p) => p.field = field,
            Overlay::Prompt(p) => p.field = field,
            Overlay::None | Overlay::Settings(_) | Overlay::TopMenu(_) | Overlay::Ctx(_) | Overlay::Font(_) => {}
        }

        match next {
            Next::Close => {
                self.overlay = Overlay::None;
                self.restore_theme_preview();
            }
            Next::Accept => self.overlay_accept(),
            Next::Changed => {
                let mut schedule = false;
                if let Overlay::Picker(p) = &mut self.overlay {
                    if p.kind == PickKind::ProjectSearch || p.kind == PickKind::WorkspaceSymbols {
                        schedule = true;
                    } else {
                        p.refilter();
                    }
                }
                if schedule {
                    self.search_due = Some(std::time::Instant::now() + std::time::Duration::from_millis(180));
                }
                if let Overlay::Prompt(p) = &mut self.overlay
                    && matches!(p.kind, PromptKind::NewFile { .. })
                {
                    p.icon_due = Some(Instant::now() + PROMPT_ICON_DEBOUNCE);
                }
            }
            Next::Nothing => {}
        }
        self.preview_theme_selection();
        self.request_redraw();
        false
    }

    /// New-file prompt: resolve the type icon of the typed name once typing paused.
    pub fn prompt_icon_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Overlay::Prompt(p) = &self.overlay else { return };
        let Some(due) = p.icon_due else { return };
        if due > now {
            return want(due);
        }
        let name = p.icon_name().to_string();
        let icon = self.tx.icons.file_icon(&name);
        if let Overlay::Prompt(p) = &mut self.overlay {
            p.icon_due = None;
            p.icon = Some(icon);
        }
        self.request_redraw();
    }

    pub fn overlay_accept(&mut self) {
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        match overlay {
            Overlay::Settings(_) | Overlay::TopMenu(_) | Overlay::Ctx(_) | Overlay::Font(_) => {}
            Overlay::Picker(p) => {
                if let Some(item) = p.selected() {
                    self.run_action(item.action);
                } else {
                    self.overlay = Overlay::Picker(p);
                }
            }
            Overlay::Prompt(p) => match p.kind {
                PromptKind::GotoLine => {
                    let mut it = p.field.text.trim().split(':');
                    let line = it.next().and_then(|s| s.trim().parse::<usize>().ok());
                    let col = it.next().and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1);
                    if let (Some(l), Some(d)) = (line, self.docs.get_mut(self.active)) {
                        let pos = d.buf.pos(l.saturating_sub(1), col.saturating_sub(1));
                        d.ed.click(&mut d.buf, pos, false);
                        d.reveal = true;
                    }
                }
                PromptKind::Rename { line, col } => self.send_rename(line, col, p.field.text.trim().to_string()),
                PromptKind::NewFile { dir } => self.create_path(&dir, &p.field.text, false),
                PromptKind::NewDir { dir } => self.create_path(&dir, &p.field.text, true),
                PromptKind::RenamePath { from } => self.rename_path(&from, &p.field.text),
                PromptKind::DeletePath { path } => {
                    if matches!(p.field.text.trim().to_lowercase().as_str(), "sim" | "s" | "y" | "yes") {
                        self.delete_path(&path)
                    } else {
                        self.say("delete cancelled")
                    }
                }
                PromptKind::GitDiscard { path } => {
                    if matches!(p.field.text.trim().to_lowercase().as_str(), "sim" | "s" | "y" | "yes") {
                        self.git_discard(&path)
                    } else {
                        self.say("cancelado")
                    }
                }
                PromptKind::GitCommit => self.git_commit(&p.field.text),
                PromptKind::Git(ask) => self.git_prompt_answer(ask, p.field.text),
                PromptKind::SaveAs { doc, rest, then } => self.save_as_answer(doc, rest, then, p.field.text),
                PromptKind::RenameTerminal { id } => self.rename_terminal(id, &p.field.text),
                PromptKind::MacroCount => self.macro_replay_answer(&p.field.text),
                PromptKind::MacroName => self.macro_save_answer(&p.field.text),
                PromptKind::AiRename { id } => self.ai_rename(id, &p.field.text),
                PromptKind::BpCondition { path, line } => self.bp_set_condition(&path, line, &p.field.text),
                PromptKind::BpLog { path, line } => self.bp_set_log(&path, line, &p.field.text),
                PromptKind::RemoteOpen => self.remote_open_answer(&p.field.text),
            },
            Overlay::None => {}
        }
    }

    pub fn run_action(&mut self, a: Action) {
        match a {
            Action::Open(path, at) => {
                self.push_nav();
                self.open_path(&path, at);
                self.focus = Focus::Editor;
            }
            Action::Cmd(c) => self.run_cmd(c),
            Action::Goto(p) => {
                self.push_nav();
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.ed.click(&mut d.buf, p, false);
                    d.reveal = true;
                }
            }
            Action::SetIndent(spec) => self.set_doc_indent(&spec),
            Action::SetLang(id) => self.set_doc_language(id),
            Action::Encoding(spec) => self.encoding_action(&spec),
            Action::WorkspaceEdit(v) => {
                if v.get("__resolve").is_some() {
                    self.resolve_code_action(&v["__resolve"]);
                } else {
                    self.apply_workspace_edit(&v)
                }
            }
            Action::TestRunner(r) => self.tests_set_runner(r),
            Action::TestTarget(id) => self.tests_set_target(&id),
            Action::BuildTarget(id) => self.build_set_target(&id),
            Action::AiAgent(id) => self.ai_pick(&id),
            Action::AiNew(id) => self.ai_pick_new(&id),
            Action::Run(cmd) => {
                let title = cmd.clone();
                self.run_task(&cmd, &title)
            }
            Action::Theme(name) => {
                self.theme_orig = None;
                self.set_theme(&name)
            }
            Action::GitShow(path, commit, subject) => self.open_diff_commit(&path, &commit, &subject),
            Action::LocalHist(path, Some(file)) => self.hist_show(&path, &file),
            Action::LocalHist(path, None) => self.hist_list(&path),
            Action::Git(p) => self.git_pick(p),
            Action::Macro(name) => self.macro_run_saved(&name),
            Action::DebugConfig(id) => self.debug_pick_config(&id),
            Action::Compare(p) => self.cmp_pick(p),
            Action::Project(p) => self.projcfg_pick(p),
            Action::Res(p) => self.res_pick(p),
            Action::UndoBranch(n) => {
                if let Some(d) = self.docs.get_mut(self.active)
                    && let Some(sel) = d.buf.redo_branch(n)
                {
                    d.ed.sel = sel;
                    d.ed.clear_extra();
                    d.reveal = true;
                }
            }
        }
    }

    pub(super) fn picker_rect(&self) -> Rect {
        let (w, _) = self.size;
        let pw = (w as i32 - 40).clamp(300, 760);
        Rect::new((w as i32 - pw) / 2, 70, pw, 0)
    }

    pub fn overlay_mouse(&mut self, x: f64, y: f64) -> bool {
        if self.overlay.is_none() {
            return false;
        }
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_mouse(x, y);
            return true;
        }
        if matches!(self.overlay, Overlay::TopMenu(_)) {
            return self.topmenu_mouse(x, y);
        }
        if matches!(self.overlay, Overlay::Ctx(_)) {
            return self.ctxmenu_mouse(x, y);
        }
        if matches!(self.overlay, Overlay::Font(_)) {
            return self.fontpicker_mouse(x, y);
        }
        if self.problems_chip_click(self.picker_rect(), INPUT_H, x, y) {
            self.request_redraw();
            return true;
        }
        let rows = self.layout.overlay_rows.clone();
        if let Some((_, idx)) = rows.iter().find(|(r, _)| r.contains(x, y)) {
            if let Overlay::Picker(p) = &mut self.overlay {
                p.sel = *idx;
            }
            self.overlay_accept();
            return true;
        }
        let r = self.picker_rect();
        let extra = match &self.overlay {
            Overlay::Picker(p) if p.rep.is_some() => preplace::REP_H,
            _ => 0,
        };
        if x < r.x as f64
            || x > r.right() as f64
            || y < r.y as f64
            || y > (r.y + INPUT_H + extra + PICKER_ROWS as i32 * ROW + 12) as f64
        {
            self.overlay = Overlay::None;
            self.restore_theme_preview();
        }
        true
    }

    pub fn overlay_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_wheel(delta);
            return true;
        }
        if matches!(self.overlay, Overlay::TopMenu(_)) {
            self.topmenu_wheel(delta);
            return true;
        }
        if matches!(self.overlay, Overlay::Ctx(_)) {
            return true;
        }
        if matches!(self.overlay, Overlay::Font(_)) {
            self.fontpicker_wheel(delta);
            return true;
        }
        if matches!(self.overlay, Overlay::Font(_)) {
            self.fontpicker_wheel(delta);
            return true;
        }
        let Overlay::Picker(p) = &mut self.overlay else { return !self.overlay.is_none() };
        let d = crate::scroller::wheel_rows_int(delta, ROW as f64);
        let max = p.filtered.len().saturating_sub(PICKER_ROWS) as isize;
        p.scroll = (p.scroll as isize + d).clamp(0, max) as usize;
        true
    }
}
