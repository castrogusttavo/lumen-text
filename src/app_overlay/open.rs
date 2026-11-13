use super::*;

impl App {
    // ───────────── opening ─────────────

    pub fn rel(&self, p: &std::path::Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().into_owned()
    }

    pub fn open_quick_open(&mut self) {
        if self.file_index.is_none() {
            self.file_index = Some(search::list_files(&self.root, 50_000));
        }
        let files = self.file_index.clone().unwrap_or_default();
        let open: Vec<PathBuf> = self.docs.iter().filter_map(|d| d.path().map(PathBuf::from)).collect();
        let mut items: Vec<Item> = Vec::new();
        for p in open.iter().rev().chain(files.iter().filter(|f| !open.contains(f))) {
            items.push(Item { label: self.rel(p), detail: String::new(), action: Action::Open(p.clone(), None) });
        }
        self.overlay = Overlay::Picker(Picker::new(PickKind::QuickOpen, "Open file", items));
    }

    pub fn open_commands(&mut self) {
        let items = COMMANDS
            .iter()
            .map(|(n, k, c)| Item { label: n.to_string(), detail: k.to_string(), action: Action::Cmd(*c) })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(PickKind::Commands, "Commands", items));
    }

    pub fn open_project_search(&mut self) {
        let mut p = Picker::new(PickKind::ProjectSearch, "Search in project", Vec::new());
        if let Some(d) = self.docs.get(self.active) {
            let sel = d.ed.selected_text(&d.buf);
            if !sel.is_empty() && !sel.contains('\n') {
                p.field = TextField::with(&sel);
            }
        }
        let has_query = !p.field.text.is_empty();
        self.overlay = Overlay::Picker(p);
        if has_query {
            self.start_search();
        }
    }

    pub fn open_locations(&mut self, title: &str, items: Vec<Item>) {
        self.overlay = Overlay::Picker(Picker::new(PickKind::Locations, title, items));
    }

    pub fn open_symbols_with(&mut self, items: Vec<Item>) {
        self.overlay = Overlay::Picker(Picker::new(PickKind::Symbols, "Go to symbol", items));
    }

    pub fn open_code_actions_with(&mut self, items: Vec<Item>) {
        self.overlay = Overlay::Picker(Picker::new(PickKind::CodeActions, "Quick actions", items));
    }

    /// Bundled themes plus the user's (`~/.config/lumen-text/themes`, incl. converted Zed themes).
    pub fn theme_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.assets.join("themes"))
            .map(|rd| {
                rd.flatten().filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned())).collect()
            })
            .unwrap_or_default();
        names.extend(crate::zedtheme::list_user_themes().into_iter().map(|(n, _)| n));
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names
    }

    pub fn theme_path(&self, name: &str) -> std::path::PathBuf {
        if let Some((_, p)) = crate::zedtheme::list_user_themes().into_iter().find(|(n, _)| n == name) {
            return p;
        }
        self.assets.join("themes").join(format!("{name}.json"))
    }

    /// Converts the Zed themes installed on this machine (min-theme etc.) into our format.
    pub fn import_zed_themes(&mut self) -> usize {
        let Some(out) = crate::zedtheme::user_theme_dir() else { return 0 };
        crate::zedtheme::install_zed_themes(&crate::zedtheme::default_zed_theme_dirs(), &out).len()
    }

    pub fn open_theme_picker(&mut self) {
        self.import_zed_themes();
        let user: std::collections::HashSet<String> =
            crate::zedtheme::list_user_themes().into_iter().map(|(n, _)| n).collect();
        let current = self.theme.name.clone();
        let names = self.theme_names();
        let sel = names.iter().position(|n| *n == current).unwrap_or(0);
        let items = names
            .into_iter()
            .map(|n| Item {
                detail: if user.contains(&n) { "your theme".into() } else { String::new() },
                label: n.clone(),
                action: Action::Theme(n),
            })
            .collect();
        self.theme_orig = Some(current);
        let mut p = Picker::new(PickKind::Themes, "Choose theme (arrow keys to preview)", items);
        p.sel = sel;
        self.overlay = Overlay::Picker(p);
    }

    /// While the theme picker is open, show the highlighted theme live.
    pub fn preview_theme_selection(&mut self) {
        let Overlay::Picker(p) = &self.overlay else { return };
        if p.kind != PickKind::Themes {
            return;
        }
        if let Some(Item { action: Action::Theme(n), .. }) = p.selected()
            && n != self.theme.name
        {
            self.set_theme_inner(&n, false);
        }
    }

    /// Esc / click outside the theme picker: put the original theme back.
    pub fn restore_theme_preview(&mut self) {
        if let Some(orig) = self.theme_orig.take()
            && orig != self.theme.name
        {
            self.set_theme_inner(&orig, false);
        }
    }

    pub fn create_custom_theme(&mut self) {
        let Some(dir) = crate::zedtheme::user_theme_dir() else { return };
        match crate::zedtheme::create_custom_theme_file(&dir, "My theme") {
            Ok(path) => {
                self.say(format!("template created: {} (save to apply, then pick the theme)", path.display()));
                self.open_path(&path, None);
            }
            Err(e) => self.say(format!("could not create the theme: {e}")),
        }
    }

    pub fn pump_eslint(&mut self) {
        let mut results = Vec::new();
        while let Ok(r) = self.eslint_rx.try_recv() {
            results.push(r);
        }
        for (path, res) in results {
            self.eslint_busy.remove(&path);
            let Some(i) = self.docs.iter().position(|d| d.path() == Some(path.as_path())) else { continue };
            match res {
                Ok(msgs) => {
                    let d = &mut self.docs[i];
                    let diags: Vec<crate::doc::Diag> = msgs
                        .iter()
                        .map(|m| {
                            let a = d.pos_from_lsp(m.line.saturating_sub(1), m.col.saturating_sub(1));
                            let b = d.pos_from_lsp(m.end_line.saturating_sub(1), m.end_col.saturating_sub(1));
                            let (sl, sc) = d.buf.line_col(a);
                            let (el, ec) = d.buf.line_col(b.max(a));
                            crate::doc::Diag {
                                sl,
                                sc,
                                el,
                                ec,
                                severity: if m.severity >= 2 { 1 } else { 2 },
                                message: format!("{} (eslint)", m.message),
                                code: m.rule.clone(),
                                source: String::new(),
                            }
                        })
                        .collect();
                    d.set_eslint_diags(diags);
                }
                Err(e) => {
                    self.docs[i].set_eslint_diags(Vec::new());
                    self.say(format!("eslint: {e}"));
                }
            }
            self.request_redraw();
        }
    }

    /// Messages from other `lumen-text` invocations and windows (see `instance`).
    pub fn pump_inbox(&mut self) {
        let mut msgs: Vec<crate::instance::Msg> = Vec::new();
        if let Some(rx) = &self.inbox {
            while let Ok(m) = rx.try_recv() {
                msgs.push(m);
            }
        }
        if msgs.is_empty() {
            return;
        }
        for m in msgs {
            match m {
                crate::instance::Msg::Open(args) => {
                    for arg in args {
                        let (p, at) = crate::parse_target(&arg);
                        if !p.is_dir() {
                            self.open_path(&p, at);
                        }
                    }
                }
                crate::instance::Msg::Quit => return self.win_quit_requested(),
            }
        }
        if let Some(g) = &self.gfx {
            g.window.focus_window();
        }
    }

    pub fn open_undo_branches(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let items: Vec<Item> = d
            .buf
            .undo_branches()
            .into_iter()
            .map(|(n, desc)| Item { label: desc, detail: String::new(), action: Action::UndoBranch(n) })
            .collect();
        if items.is_empty() {
            return self.say("no alternative branches from here (use Ctrl+Z and edit to create one)");
        }
        self.open_locations("Undo branches", items);
    }

    pub fn set_theme(&mut self, name: &str) {
        self.config.appearance = "fixed".into();
        self.set_theme_inner(name, true);
    }

    pub fn set_theme_inner(&mut self, name: &str, persist: bool) {
        let path = self.theme_path(name);
        match crate::theme::Theme::load(&path) {
            Ok(t) => {
                self.theme = t;
                for d in &mut self.docs {
                    if let Some(h) = d.hl.as_mut() {
                        h.theme_changed();
                    }
                }
                self.config.theme = Some(name.to_string());
                if persist {
                    self.config.save();
                }
                self.say(format!("theme: {name}"));
            }
            Err(e) => self.say(format!("invalid theme: {e}")),
        }
    }

    pub fn open_workspace_symbols(&mut self) {
        if self.with_server_pub().is_none() {
            return self.say("project symbols need the language server (open a TS/JS file)");
        }
        self.overlay =
            Overlay::Picker(Picker::new(PickKind::WorkspaceSymbols, "Symbol in project (type to search)", Vec::new()));
    }

    pub fn open_goto_line(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        self.overlay = Overlay::Prompt(Prompt::new(
            PromptKind::GotoLine,
            "Go to line (line[:column])".into(),
            TextField::default(),
        ));
    }

    pub fn open_prompt_rename(&mut self, name: &str, line: usize, col: usize) {
        self.overlay = Overlay::Prompt(Prompt::new(
            PromptKind::Rename { line, col },
            "New symbol name".into(),
            TextField::with(name),
        ));
    }

    pub fn start_search(&mut self) {
        let Overlay::Picker(p) = &mut self.overlay else { return };
        if p.kind == PickKind::WorkspaceSymbols {
            let q = p.field.text.trim().to_string();
            return self.request_workspace_symbols(q);
        }
        if p.kind != PickKind::ProjectSearch {
            return;
        }
        let overrides = self.unsaved_texts();
        let Overlay::Picker(p) = &mut self.overlay else { return };
        let q = search::Query {
            text: p.field.text.clone(),
            case: p.case,
            regex: p.regex,
            word: p.word,
            replace: p.rep.as_ref().map(|r| r.field.text.clone()),
        };
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Some(r) = p.rep.as_mut() {
            // Select the same match again once the refreshed results arrive.
            if r.want.is_none() {
                r.want = r.hits.get(p.sel).map(|h| (h.path.clone(), h.line, h.start));
            }
            r.hits.clear();
            r.confirm = None;
            r.done = done.clone();
            r.query = q.clone();
        }
        p.items.clear();
        p.filtered.clear();
        p.sel = 0;
        let my_gen = self.search_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if q.text.is_empty() {
            done.store(true, Ordering::SeqCst);
            return;
        }
        let (root, generation, tx, wake) =
            (self.root.clone(), self.search_gen.clone(), self.search_tx.clone(), self.wake());
        thread::spawn(move || {
            search::search(&root, &q, &overrides, generation, my_gen, tx, || wake());
            done.store(true, Ordering::SeqCst);
            wake();
        });
    }

    pub fn pump_background(&mut self) {
        self.pump_lsp();
        self.pump_task();
        self.pump_http();
        self.pump_tests();
        self.pump_build();
        self.pump_term();
        self.pump_ai();
        self.pump_ide();
        self.pump_inbox();
        self.pump_eslint();
        self.pump_debug();
        self.pump_watcher();
        self.pump_update();
        self.pump_elevated();
        let cur = self.search_gen.load(Ordering::SeqCst);
        let mut incoming: Vec<Hit> = Vec::new();
        while let Ok((g, hits)) = self.search_rx.try_recv() {
            if g == cur {
                incoming.extend(hits);
            }
        }
        if incoming.is_empty() {
            return;
        }
        let items: Vec<Item> = incoming
            .iter()
            .map(|h| Item {
                label: h.text.clone(),
                detail: format!("{}:{}", self.rel(&h.path), h.line + 1),
                action: Action::Open(h.path.clone(), Some((h.line, h.col))),
            })
            .collect();
        if let Overlay::Picker(p) = &mut self.overlay
            && p.kind == PickKind::ProjectSearch
        {
            p.items.extend(items);
            let sel = p.sel;
            p.refilter();
            if let Some(r) = p.rep.as_mut() {
                let base = r.hits.len();
                r.hits.extend(incoming);
                p.sel = sel.min(p.filtered.len().saturating_sub(1));
                if let Some(w) = &r.want
                    && let Some(i) = r.hits[base..].iter().position(|h| (&h.path, h.line, h.start) == (&w.0, w.1, w.2))
                {
                    p.sel = base + i;
                    r.want = None;
                }
                p.keep_visible();
            }
        }
    }
}
