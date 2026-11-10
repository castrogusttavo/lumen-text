use super::*;

impl App {
    // ── documents ──

    pub fn say(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// Like [`say`](Self::say), but the message stays visible `extra_secs` longer (warnings).
    pub fn say_for(&mut self, msg: impl Into<String>, extra_secs: u64) {
        self.status = Some((msg.into(), Instant::now() + Duration::from_secs(extra_secs)));
    }

    pub fn request_redraw(&self) {
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    pub(crate) fn tokenizer_for(&mut self, lang: &str) -> Option<SharedTokenizer> {
        let reg = crate::textmate::Registry::new(&self.assets).ok()?;
        let scope = reg.scope_for_language(lang)?;
        if let Some(t) = self.tokenizers.get(&scope) {
            return Some(t.clone());
        }
        let t = make_tokenizer(&self.assets, &scope)?;
        self.tokenizers.insert(scope, t.clone());
        Some(t)
    }

    /// Open (or activate) a file. Returns its index.
    pub fn open_path(&mut self, path: &Path, at: Option<(usize, usize)>) -> Option<usize> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let idx = if let Some(i) = self.docs.iter().position(|d| d.path() == Some(path.as_path())) {
            i
        } else {
            let buf = match Buffer::open(&path) {
                Ok(b) => b,
                Err(e) => {
                    self.say(format!("Could not open {}: {e}", path.display()));
                    return None;
                }
            };
            let lang = language_for(&path.to_string_lossy());
            let hl = lang.and_then(|l| self.tokenizer_for(l)).map(Highlighter::new);
            let comment = comment_prefix(&path.to_string_lossy());
            let mut doc = Doc::new(buf, hl, lang, comment, self.tx.cell_w);
            doc.bps = self.breakpoints.get(&path).cloned().unwrap_or_default();
            doc.minimap = self.config.minimap;
            doc.indent_guides = self.config.indent_guides;
            doc.sticky = self.config.sticky_scroll;
            doc.lens_refs = self.config.code_lens_refs;
            doc.lens_impls = self.config.code_lens_impls;
            doc.set_word_wrap(self.config.word_wrap);
            doc.git_base = git::head_text(&path);
            self.docs.push(doc);
            self.docs.len() - 1
        };
        self.activate(idx);
        if let Some((line, col)) = at
            && let Some(d) = self.docs.get_mut(idx)
        {
            let p = d.buf.pos(line, col);
            d.ed.click(&mut d.buf, p, false);
            d.reveal = true;
        }
        self.tree.reveal(&path);
        Some(idx)
    }

    pub(super) fn here(&self) -> Option<(PathBuf, usize)> {
        let d = self.docs.get(self.active)?;
        Some((d.path()?.to_path_buf(), d.ed.head()))
    }

    /// Remember where we are before a jump (definition, search result, symbol…).
    pub fn push_nav(&mut self) {
        if let Some(h) = self.here() {
            if self.nav_back.last() != Some(&h) {
                self.nav_back.push(h);
                if self.nav_back.len() > 100 {
                    self.nav_back.remove(0);
                }
            }
            self.nav_fwd.clear();
        }
    }

    pub(super) fn jump_to(&mut self, path: &Path, at: usize) {
        if let Some(i) = self.open_path(path, None) {
            let d = &mut self.docs[i];
            let at = at.min(d.buf.len_chars());
            d.ed.click(&mut d.buf, at, false);
            d.reveal = true;
        }
    }

    pub fn navigate(&mut self, back: bool) {
        let target = if back { self.nav_back.pop() } else { self.nav_fwd.pop() };
        let Some((path, at)) = target else {
            return self.say(if back { "no previous position" } else { "no next position" });
        };
        if let Some(h) = self.here() {
            if back { self.nav_fwd.push(h) } else { self.nav_back.push(h) }
        }
        self.jump_to(&path, at);
    }

    /// Watch the project root, every expanded folder of the tree, and the folders of open files.
    pub fn watch_dirs(&mut self) {
        let Some(w) = self.watcher.as_mut() else { return };
        let _ = w.watch_dir(&self.root);
        for r in &self.tree.rows {
            if r.is_dir && r.expanded {
                let _ = w.watch_dir(&r.path);
            }
        }
        for d in &self.docs {
            if let Some(p) = d.path().and_then(Path::parent) {
                let _ = w.watch_dir(p);
            }
        }
        // A running language server needs to hear about every source file changing on disk, also in
        // folders that are collapsed in the tree.
        for l in &self.lsps {
            if (l.name == "tsgo" || l.is_generic()) && !self.lsp_watched.contains(&l.root) {
                self.lsp_watched.push(l.root.clone());
                crate::watcher::watch_tree(w, &l.root, 4000);
            }
        }
    }

    /// React to file-system events: reload changed files right away and refresh the tree.
    pub fn pump_watcher(&mut self) {
        let mut evs = Vec::new();
        if let Some(w) = &self.watcher {
            while let Ok(e) = w.rx.try_recv() {
                evs.push(e);
            }
        }
        if evs.is_empty() {
            return;
        }
        self.lsp_fs_events(&evs);
        self.last_disk_check = Instant::now() - Duration::from_secs(60);
        if self.last_tree_refresh.elapsed() > Duration::from_millis(400) {
            self.last_tree_refresh = Instant::now();
            if evs.iter().any(|e| crate::ignore_filter::IgnoreFilter::is_ignore_file(&e.path)) {
                self.tree.invalidate_ignore();
            }
            self.tree.rebuild();
            self.file_index = None;
            self.refresh_git_status();
            self.watch_dirs();
        }
        self.request_redraw();
    }

    pub fn activate(&mut self, idx: usize) {
        if idx < self.docs.len() {
            self.active = idx;
            self.hover = None;
            self.completion = None;
            self.close_armed = None;
            self.request_redraw();
        }
    }

    pub fn close_tab(&mut self, idx: usize, force: bool) {
        let Some(d) = self.docs.get(idx) else { return };
        if d.buf.is_dirty() && !force && self.close_armed != Some(idx) {
            self.close_armed = Some(idx);
            let n = d.name.clone();
            self.say(format!("{n} has unsaved changes — close again to discard"));
            return;
        }
        self.lsp_did_close(idx);
        self.docs.remove(idx);
        self.split_doc_removed(idx);
        self.close_armed = None;
        if self.active >= self.docs.len() {
            self.active = self.docs.len().saturating_sub(1);
        } else if idx < self.active {
            self.active -= 1;
        }
        self.request_redraw();
    }

    pub fn save_active(&mut self) {
        if self.docs.get(self.active).is_some_and(|d| d.path().is_none()) {
            // Untitled document: nowhere to write yet, ask for a path.
            return self.ask_save_as(self.active, Vec::new(), None, String::new());
        }
        let warn = self.format_for_save(self.active);
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let name = d.name.clone();
        match d.buf.save() {
            Ok(()) => {
                d.git_stale = true;
                d.disk_mtime = d.stat_mtime();
                d.conflict = false;
                let idx = self.active;
                self.lsp_did_save(idx);
                self.refresh_git_status();
                match warn {
                    Some(w) => self.say_for(format!("Saved {name}, but {w}"), 8),
                    None => self.say(format!("Saved {name}")),
                }
                self.tests_on_save();
            }
            Err(e) => self.say(format!("Error saving {name}: {e}")),
        }
    }

    // ── session ──

    pub(super) fn session_path(&self) -> Option<PathBuf> {
        let base = crate::paths::state_home()?;
        let h = self
            .root
            .to_string_lossy()
            .bytes()
            .fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
        Some(base.join("lumen-text").join(format!("session-{h:016x}.json")))
    }

    pub fn save_session(&self) {
        let Some(path) = self.session_path() else { return };
        let files: Vec<serde_json::Value> = self
            .docs
            .iter()
            .filter_map(|d| {
                let (l, c) = d.buf.line_col(d.ed.head());
                let mut entry = serde_json::json!({"path": d.path()?.to_string_lossy(), "line": l, "col": c, "top": d.target, "pinned": d.pinned, "md": d.md.is_some()});
                self.picks_to_json(d.path()?, &mut entry);
                Some(entry)
            })
            .collect();
        // Index of each document's entry in `files` (documents without a path are not saved).
        let mut file_ix = vec![None; self.docs.len()];
        for (k, i) in self.docs.iter().enumerate().filter(|(_, d)| d.path().is_some()).map(|(i, _)| i).enumerate() {
            file_ix[i] = Some(k);
        }
        let mut v = serde_json::json!({"active": self.active, "files": files});
        if let Some(sp) = self.split_session(&file_ix) {
            v["split"] = sp;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, v.to_string());
    }

    pub fn restore_session(&mut self) {
        let Some(path) = self.session_path() else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let mut opened: Vec<Option<usize>> = Vec::new();
        for f in v["files"].as_array().into_iter().flatten() {
            opened.push(None);
            let Some(p) = f["path"].as_str() else { continue };
            if !Path::new(p).exists() {
                continue;
            }
            let at = Some((f["line"].as_u64().unwrap_or(0) as usize, f["col"].as_u64().unwrap_or(0) as usize));
            let i = self.open_path(Path::new(p), at);
            *opened.last_mut().unwrap() = i;
            if let Some(i) = i {
                let top = f["top"].as_f64().unwrap_or(0.0);
                self.docs[i].pinned = f["pinned"].as_bool().unwrap_or(false);
                self.docs[i].md_restore(f["md"].as_bool().unwrap_or(false));
                self.apply_picks_json(i, f);
                self.docs[i].target = top;
                self.docs[i].top = top;
                self.docs[i].reveal = false;
            }
        }
        let active = v["active"].as_u64().unwrap_or(0) as usize;
        if active < self.docs.len() {
            self.activate(active);
        }
        if v["split"].is_object() {
            self.split_restore(&v["split"], &opened);
        }
    }
}
