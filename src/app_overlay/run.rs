use super::*;

impl App {
    // ───────────── commands ─────────────

    pub fn run_cmd(&mut self, c: Cmd) {
        if self.run_display_cmd(c) {
            return;
        }
        match c {
            Cmd::Save => self.save_active(),
            Cmd::CloseTab => {
                let a = self.active;
                self.close_tab(a, false)
            }
            Cmd::ReopenClosedTab => self.reopen_closed_tab(),
            Cmd::NextTab | Cmd::PrevTab => {
                let len = self.docs.len();
                if len > 0 {
                    let n = if c == Cmd::NextTab { (self.active + 1) % len } else { (self.active + len - 1) % len };
                    self.activate(n);
                }
            }
            Cmd::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Cmd::FocusSidebar => {
                self.sidebar_open = true;
                self.focus = Focus::Sidebar;
            }
            Cmd::QuickOpen => self.open_quick_open(),
            Cmd::Commands => self.open_commands(),
            Cmd::ProjectSearch => self.open_project_search(),
            Cmd::Find => self.open_find(false),
            Cmd::Replace => self.open_find(true),
            Cmd::GotoLine => self.open_goto_line(),
            Cmd::GotoSymbol => self.open_symbols(),
            Cmd::Format => self.format_document(),
            Cmd::Rename => self.start_rename(),
            Cmd::ShowHover => self.request_hover_at_cursor(),
            Cmd::GotoDefinition => self.goto_definition(crate::lsp::DefKind::Definition),
            Cmd::GotoTypeDefinition => self.goto_definition(crate::lsp::DefKind::TypeDefinition),
            Cmd::GotoImplementation => self.goto_definition(crate::lsp::DefKind::Implementation),
            Cmd::FindReferences => self.find_references(),
            Cmd::CodeActions => self.code_actions(),
            Cmd::ToggleComment => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.ed.toggle_comment(&mut d.buf);
                    d.reveal = true;
                }
            }
            Cmd::Undo | Cmd::Redo => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    if c == Cmd::Undo {
                        d.ed.undo(&mut d.buf)
                    } else {
                        d.ed.redo(&mut d.buf)
                    }
                    d.reveal = true;
                }
            }
            Cmd::NextDiagnostic => self.next_diagnostic(true),
            Cmd::PrevDiagnostic => self.next_diagnostic(false),
            Cmd::ReloadFile => self.reload_active(),
            Cmd::LocalHistory => self.open_local_history(),
            Cmd::LocalHistoryDeleted => self.open_local_history_deleted(),
            Cmd::ReopenWithEncoding => self.open_encoding_picker(false),
            Cmd::SaveWithEncoding => self.open_encoding_picker(true),
            Cmd::ToggleRenderWhitespace => self.toggle_render_whitespace(),
            Cmd::RestartLsp => self.restart_lsp(),
            Cmd::ShowLspLog => self.lspx_show_log(),
            Cmd::ToggleAutoImport => self.toggle_auto_import(),
            Cmd::RunTask => self.open_task_picker(),
            Cmd::RepeatTask => self.repeat_task(),
            Cmd::StopTask => {
                self.stop_task();
                self.tests_stop();
                self.build_stop();
            }
            Cmd::TestFile => self.run_tests(false),
            Cmd::TestAtCursor => self.run_tests(true),
            Cmd::TestAll => self.tests_run_all(),
            Cmd::TestFailed => self.tests_run_failed(),
            Cmd::TestsPanel => self.tests_open_panel(),
            Cmd::TestDebug => self.tests_debug_cursor(),
            Cmd::TestWatch => self.tests_toggle_watch(),
            Cmd::TestRunner => self.tests_pick_runner(),
            Cmd::BuildBuild => self.build_build(),
            Cmd::BuildRun => self.build_run(),
            Cmd::BuildStop => self.build_stop(),
            Cmd::BuildClean => self.build_clean(),
            Cmd::BuildSelectTarget => self.build_pick_target(),
            Cmd::BuildPanel => self.build_open_panel(),
            Cmd::ToggleOutput => self.toggle_panel(),
            Cmd::NewFile => self.prompt_new_file(false),
            Cmd::NewDir => self.prompt_new_file(true),
            Cmd::RenamePath => self.prompt_rename_path(),
            Cmd::DeletePath => self.prompt_delete_path(),
            Cmd::GitChanges => self.open_git_changes(),
            Cmd::GitPanel => self.toggle_git_panel(),
            Cmd::AiPanel => self.ai_toggle(),
            Cmd::AiNewSession => self.ai_new_session_menu(),
            Cmd::AiStop => self.ai_stop(),
            Cmd::AiChooseAgent => self.ai_choose_agent(),
            Cmd::AiSendSelection => self.ai_send_selection(),
            Cmd::AiNextWaiting => self.ai_next_waiting(),
            Cmd::AiRenameSession => self.ai_rename_prompt(),
            Cmd::AiCloseSession => self.ai_close_current(),
            Cmd::AiNewSessionInWorktree => self.worktree_pick_for_ai(),
            Cmd::GitStage => self.git_stage(true),
            Cmd::GitUnstage => self.git_stage(false),
            Cmd::GitStageAll => self.git_stage_all(),
            Cmd::GitDiscard => self.prompt_git_discard(),
            Cmd::GitCommit => self.prompt_git_commit(),
            Cmd::GitStageHunk => self.git_stage_hunk(),
            Cmd::GitRevertHunk => self.git_revert_hunk(),
            Cmd::GitDiffFile => self.open_diff_head(),
            Cmd::GitFileHistory => self.open_file_history(),
            Cmd::GitCompare => self.git_compare_pick(),
            Cmd::GitCompareTwo => self.git_compare_two_pick(),
            Cmd::GitCompareFile => self.git_compare_file_pick(),
            Cmd::GitDiffWhitespace => self.toggle_diff_ws_setting(),
            Cmd::GitCreateBranch => self.git_new_branch(None, ""),
            Cmd::GitDeleteBranch => self.git_delete_branch_pick(),
            Cmd::GitCreateTag => self.git_new_tag(None),
            Cmd::GitDeleteTag => self.git_delete_tag_pick(),
            Cmd::GitPushTags => self.git_push_tags_pick(),
            Cmd::GitCreateRelease => self.git_release_start(),
            Cmd::GitCherryPick => self.git_cherry_pick_pick(),
            Cmd::GitCherryPickContinue => self.git_cherry_continue(),
            Cmd::GitCherryPickAbort => self.git_cherry_abort(),
            Cmd::GitUndoCommit => self.git_undo_commit(),
            Cmd::GitWorktrees => self.worktrees_open(),
            Cmd::GitWorktreeNew => self.worktree_new(),
            Cmd::GitWorktreeRemove => self.worktree_remove_pick(),
            Cmd::GitWorktreePrune => self.worktree_prune(),
            Cmd::GitStash => self.git_stash_cmd(false),
            Cmd::GitStashUntracked => self.git_stash_cmd(true),
            Cmd::GitStashPop => self.git_stash_pop_latest(),
            Cmd::GitStashList => self.git_stash_list(),
            Cmd::MarkdownPreview => self.toggle_md_reader(),
            Cmd::SplitRight => self.split_right(),
            Cmd::SplitDown => self.split_down(),
            Cmd::CloseSplit => self.split_close(),
            Cmd::ClosePane => self.close_focused_pane(),
            Cmd::FocusOtherPane => self.focus_next_pane(1),
            Cmd::FocusPrevPane => self.focus_next_pane(-1),
            Cmd::FocusLeftPane => self.focus_pane_n(0),
            Cmd::FocusRightPane => self.focus_pane_n(1),
            Cmd::FocusPane3 => self.focus_pane_n(2),
            Cmd::FocusPane4 => self.focus_pane_n(3),
            Cmd::MoveToNextPane => self.move_to_pane(1),
            Cmd::MoveToPrevPane => self.move_to_pane(-1),
            Cmd::ConflictOurs => self.resolve_conflict(crate::gitx::Pick::Ours),
            Cmd::ConflictTheirs => self.resolve_conflict(crate::gitx::Pick::Theirs),
            Cmd::ConflictBoth => self.resolve_conflict(crate::gitx::Pick::Both),
            Cmd::WorkspaceSymbols => self.open_workspace_symbols(),
            Cmd::Problems => self.open_problems(),
            Cmd::ZoomIn => self.zoom(1.0),
            Cmd::ZoomOut => self.zoom(-1.0),
            Cmd::ZoomReset => self.zoom(0.0),
            Cmd::ToggleNarrow => self.toggle_narrow(),
            Cmd::NavBack => self.navigate(true),
            Cmd::NavForward => self.navigate(false),
            Cmd::OpenTerminal => self.toggle_terminal_focus(),
            Cmd::CloseTerminal => self.close_terminal_request(),
            Cmd::NewTerminal => self.new_terminal(),
            Cmd::SplitTerminal => self.split_terminal(),
            Cmd::RenameTerminal => self.prompt_rename_terminal(self.terms.active),
            Cmd::NextTerminal => self.cycle_terminal(1),
            Cmd::PrevTerminal => self.cycle_terminal(-1),
            Cmd::DebugStart
            | Cmd::DebugStop
            | Cmd::ToggleBreakpoint
            | Cmd::ClearBreakpoints
            | Cmd::StepOver
            | Cmd::StepInto
            | Cmd::StepOut => self.run_debug_cmd(c),
            Cmd::CreateTheme => self.create_custom_theme(),
            Cmd::ImportZedThemes => {
                let n = self.import_zed_themes();
                self.say(format!("{n} Zed theme(s) imported"));
            }
            Cmd::OpenSettings => self.open_settings(crate::settingsui::Page::Settings),
            Cmd::OpenKeybindings => self.open_settings(crate::settingsui::Page::Keybinds),
            Cmd::ToggleMinimap => {
                for d in &mut self.docs {
                    d.minimap = !d.minimap;
                }
                self.minimap_default = self.docs.first().is_none_or(|d| d.minimap);
            }
            Cmd::ProjectMenu => self.open_topmenu_cmd(crate::app_topbar::MenuKind::Project),
            Cmd::BranchMenu => self.open_topmenu_cmd(crate::app_topbar::MenuKind::Branch),
            Cmd::ToggleWordWrap if self.split.is_some() => self.split_toggle_wrap(),
            Cmd::ToggleWordWrap => {
                self.config.word_wrap = !self.config.word_wrap;
                self.config.save();
                for d in &mut self.docs {
                    d.set_word_wrap(self.config.word_wrap);
                }
                let on = self.config.word_wrap;
                self.say(if on { "word wrap: on" } else { "word wrap: off" });
            }
            Cmd::Fold | Cmd::Unfold | Cmd::FoldAll | Cmd::UnfoldAll => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    let did = match c {
                        Cmd::Fold => d.fold_at_cursor(),
                        Cmd::Unfold => d.unfold_at_cursor(),
                        Cmd::FoldAll => {
                            d.fold_all();
                            true
                        }
                        _ => {
                            d.unfold_all();
                            true
                        }
                    };
                    if !did {
                        self.say(if c == Cmd::Fold { "no region to fold here" } else { "nothing folded here" });
                    }
                }
            }
            Cmd::ToggleCodeLens => {
                self.config.code_lens = !self.config.code_lens;
                self.config.save();
                for d in &mut self.docs {
                    d.lenses.clear();
                    d.lens_due = Some(Instant::now());
                }
                for l in &mut self.lsps {
                    l.notify("workspace/didChangeConfiguration", serde_json::json!({"settings": {}}));
                }
                let on = self.config.code_lens;
                self.say(if on { "code lens: on" } else { "code lens: off" });
            }
            Cmd::ToggleInlayHints => {
                self.config.inlay_hints = !self.config.inlay_hints;
                self.config.save();
                self.refresh_inlay_hints();
                let on = self.config.inlay_hints;
                self.say(if on { "inlay hints: on" } else { "inlay hints: off" });
            }
            Cmd::ToggleGitBlame => self.toggle_git_blame(),
            Cmd::CycleInlineDiagnostics => self.cycle_inline_diagnostics(),
            // Handled by `run_display_cmd` before this match.
            Cmd::ToggleBookmark
            | Cmd::NextBookmark
            | Cmd::PrevBookmark
            | Cmd::ToggleLineNumbers
            | Cmd::ToggleBookmarkMarks
            | Cmd::ToggleBreakpointMarks
            | Cmd::CycleScrollbar
            | Cmd::ToggleLsp
            | Cmd::ChooseBufferFont
            | Cmd::ChooseUiFont => {}
            Cmd::ToggleOutline => {
                self.sidebar_open = true;
                self.sidebar_mode = if self.sidebar_mode == crate::app::SidebarMode::Files {
                    crate::app::SidebarMode::Outline
                } else {
                    crate::app::SidebarMode::Files
                };
                self.tree.scroll = 0.0;
            }
            Cmd::ExpandSelection => self.expand_selection(),
            Cmd::ShrinkSelection => self.shrink_selection(),
            Cmd::IncomingCalls => self.call_hierarchy(true),
            Cmd::OutgoingCalls => self.call_hierarchy(false),
            Cmd::FollowSystemTheme => {
                self.config.appearance =
                    if self.config.appearance == "system" { "fixed".into() } else { "system".into() };
                self.config.save();
                let on = self.config.appearance == "system";
                self.say(if on { "theme: following the system" } else { "theme: fixed" });
                self.follow_system_theme();
            }
            Cmd::CycleTsgoSource => self.cycle_tsgo_source(),
            Cmd::UndoBranches => self.open_undo_branches(),
            Cmd::OrganizeImports => self.organize_imports(),
            Cmd::ToggleFormatOnSave => self.toggle_format_on_save(),
            Cmd::PickTheme => self.open_theme_picker(),
            Cmd::ToggleHideIgnored => self.toggle_hide_ignored(),
            Cmd::CheckUpdate => self.update_check_manual(),
            Cmd::RestartUpdate => self.update_restart_cmd(),
            Cmd::About => self.update_show_about(),
            Cmd::ColumnSelectUp => self.column_select(-1, 0),
            Cmd::ColumnSelectDown => self.column_select(1, 0),
            Cmd::ColumnSelectLeft => self.column_select(0, -1),
            Cmd::ColumnSelectRight => self.column_select(0, 1),
            Cmd::JumpTo => self.jump_start(),
            Cmd::RefreshExplorer => self.refresh_explorer(),
            Cmd::ProjectReplace => self.open_project_replace(),
            Cmd::MacroRecord => self.macro_toggle_record(),
            Cmd::MacroReplay => self.macro_replay(1),
            Cmd::MacroReplayTimes => self.macro_ask_times(),
            Cmd::MacroSave => self.macro_ask_name(),
            Cmd::MacroRunSaved => self.macro_pick_saved(),
            Cmd::OpenProjectSettings => self.open_project_settings(),
            Cmd::ProjectTrust => self.projcfg_trust_prompt(true),
            Cmd::PythonSelectInterpreter => self.open_interpreter_picker(),
            Cmd::Quit => {
                if self.try_quit() {
                    std::process::exit(0);
                }
            }
        }
    }

    /// Run a command reached through the keymap (user file or `keymap::DEFAULTS`). Returns true
    /// when the app should quit. Text-editing commands behave like their hard-wired keys did:
    /// extra cursors collapse and the edit is synced/notified like any other keystroke.
    pub fn run_key_cmd(&mut self, c: Cmd) -> bool {
        match c {
            Cmd::Quit => return self.try_quit(),
            Cmd::Undo | Cmd::Redo | Cmd::ToggleComment => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.ed.clear_extra();
                }
                self.run_cmd(c);
                return self.after_key(None);
            }
            _ => self.run_cmd(c),
        }
        self.request_redraw();
        false
    }

    /// Run the command a keybinding names (`"none"` does nothing; an unknown id says so).
    pub fn run_bound(&mut self, id: &str) -> bool {
        // `"ctrl+alt+1": "macro:wrap"` runs the macro saved as "wrap" in macros.json.
        if let Some(name) = id.strip_prefix(crate::macros::BIND_PREFIX) {
            self.macro_run_saved(name);
            self.request_redraw();
            return false;
        }
        match Cmd::from_id(id) {
            Some(c) => self.run_key_cmd(c),
            None => {
                if id != "none" {
                    self.say(format!("shortcut points to an unknown command: {id}"));
                }
                false
            }
        }
    }

    pub fn reload_active(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(PathBuf::from) else { return };
        if d.buf.is_dirty() {
            self.say("Unsaved changes — save or close the tab before reloading");
            return;
        }
        let idx = self.active;
        self.close_tab(idx, true);
        self.open_path(&path, None);
        self.say("Reloaded from disk");
    }
}
