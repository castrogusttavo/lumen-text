use super::*;

impl App {
    // ── timers ──

    /// Run whatever is due; returns the next deadline, if any.
    pub fn tick(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut want = |t: Instant| next = Some(next.map_or(t, |n| n.min(t)));

        self.lsp_tick(now, &mut want);
        self.assist_tick(now, &mut want);
        self.inline_tick(now, &mut want);
        self.guard_lsp_memory(now, &mut want);
        self.res_tick(now, &mut want);
        if self.last_trim.elapsed() > Duration::from_secs(4) {
            self.last_trim = Instant::now();
            crate::app_lsp::release_memory();
        }
        want(self.last_trim + Duration::from_secs(4));
        self.check_disk(now, &mut want);
        self.backup_tick(now, &mut want);
        self.gitpanel_tick(now, &mut want);
        self.update_tick(now, &mut want);
        self.git_bg_tick(now, &mut want);
        self.fswatch_tick(now, &mut want);
        self.md_tick(now, &mut want);
        self.img_tick(now, &mut want);
        self.tests_tick(now, &mut want);
        self.coverage_sync_docs();
        self.build_tick(now, &mut want);
        self.ai_tick(now, &mut want);
        self.ide_tick(now, &mut want);
        self.term_tick(&mut want);

        // Git gutter against the buffer, debounced.
        let active = self.active;
        for (i, d) in self.docs.iter_mut().enumerate() {
            if !d.git_stale {
                continue;
            }
            let due = d.changed_at.map_or(now, |t| t + Duration::from_millis(350));
            if due > now && i == active {
                want(due);
                continue;
            }
            if i != active && d.changed_at.is_some() {
                // background tabs refresh when activated
            }
            d.git_stale = false;
            d.git = match (&d.git_base, d.buf.line_count() < 200_000) {
                (Some(base), true) => git::marks(base, &d.buf.to_string()),
                _ => Vec::new(),
            };
        }

        self.pump_git_status();
        self.import_idx_tick(now, &mut want);

        // Inline blame: follows the cursor line, computed on a worker after the cursor rests a moment.
        let mut blamed = false;
        while let Ok((p, l, res)) = self.blame_ch.1.try_recv() {
            if let Some(d) = self.docs.get_mut(self.active)
                && self.config.git_blame
                && d.path() == Some(p.as_path())
                && d.buf.line_col(d.ed.head()).0 == l
                && !d.buf.is_dirty()
            {
                d.blame = res.map(|t| (l, t));
                blamed = true;
            }
        }
        if let Some(d) = self.docs.get_mut(self.active) {
            let (line, _) = d.buf.line_col(d.ed.head());
            if d.blame.as_ref().is_some_and(|(l, _)| *l != line) || d.buf.is_dirty() || !self.config.git_blame {
                d.blame = None;
            }
            if d.buf.is_dirty() || !self.config.git_blame {
                self.blame_asked = None;
            }
            let tracked = self.config.git_blame && d.git_base.is_some() && !d.buf.is_dirty();
            let asked = self.blame_asked.as_ref().is_some_and(|(p, l)| *l == line && d.path() == Some(p.as_path()));
            if tracked && d.blame.is_none() && !asked {
                match self.blame_for {
                    Some((i, l, since)) if i == self.active && l == line => {
                        let due = since + Duration::from_millis(280);
                        if due <= now {
                            if let Some(p) = d.path().map(Path::to_path_buf) {
                                self.blame_asked = Some((p.clone(), line));
                                if cfg!(test) {
                                    d.blame = git::blame_line(&p, line).map(|t| (line, t));
                                    blamed = true;
                                } else {
                                    let (tx, wake) = (self.blame_ch.0.clone(), self.wake_fn.clone());
                                    std::thread::spawn(move || {
                                        let res = git::blame_line(&p, line);
                                        let _ = tx.send((p, line, res));
                                        wake();
                                    });
                                }
                            }
                            self.blame_for = None;
                        } else {
                            want(due);
                        }
                    }
                    _ => {
                        self.blame_for = Some((self.active, line, now));
                        want(now + Duration::from_millis(280));
                    }
                }
            }
        }

        if blamed {
            self.request_redraw();
        }

        // Merge-conflict markers, a moment after the text settles.
        for d in &mut self.docs {
            let Some(due) = d.conflicts_due else { continue };
            if due > now {
                want(due);
                continue;
            }
            d.conflicts_due = None;
            let text = d.buf.to_string();
            d.conflicts = if text.contains("<<<<<<<") { crate::gitx::parse_conflicts(&text) } else { Vec::new() };
        }

        // ESLint on a worker thread, a moment after typing stops.
        for i in 0..self.docs.len() {
            let Some(due) = self.docs[i].eslint_due else { continue };
            // The previous run for this file is still going: no second process next to it. The
            // request stays (without a deadline, so nothing spins) and is served by the tick that
            // follows `pump_eslint` taking that run's result: exactly one more run, on the latest text.
            if self.docs[i].path().is_some_and(|p| self.eslint_busy.contains(p)) {
                continue;
            }
            if due > now {
                want(due);
                continue;
            }
            self.docs[i].eslint_due = None;
            let Some(path) = self.docs[i].path().map(Path::to_path_buf) else { continue };
            if !matches!(self.docs[i].lang, Some("typescript" | "tsx" | "javascript" | "jsx")) {
                continue;
            }
            let Some((root, bin)) = crate::eslint::detect_cached(&path) else { continue };
            self.eslint_busy.insert(path.clone());
            let text = self.docs[i].buf.to_string();
            let (tx, wake) = (self.eslint_tx.clone(), self.wake());
            std::thread::spawn(move || {
                let res = crate::eslint::run(&root, &bin, &path, &text);
                let _ = tx.send((path, res));
                wake();
            });
        }

        if let Some(t) = self.search_due {
            if t <= now {
                self.search_due = None;
                self.start_search();
            } else {
                want(t);
            }
        }
        self.prompt_icon_tick(now, &mut want);
        self.tooltip_tick(now, &mut want);
        self.hover_tick(now, &mut want);
        if let Some((since, pos, requested)) = self.hover_probe {
            let due = since + Duration::from_millis(450);
            if !requested {
                if due <= now {
                    self.hover_probe = Some((since, pos, true));
                    self.request_hover(pos);
                } else {
                    want(due);
                }
            }
        }
        next
    }

    /// Pick up edits made by other programs (git checkout, formatters, other editors).
    pub(super) fn check_disk(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        // The inotify watcher reports changes immediately; this is only a safety net.
        const EVERY: Duration = Duration::from_millis(6000);
        let due = self.last_disk_check + EVERY;
        if due > now {
            want(due);
            return;
        }
        self.last_disk_check = now;
        want(now + EVERY);
        let mut notes: Vec<String> = Vec::new();
        let hist = self.hist_store();
        for d in &mut self.docs {
            let (Some(old), Some(cur)) = (d.disk_mtime, d.stat_mtime()) else { continue };
            if old == cur {
                continue;
            }
            if d.buf.is_dirty() {
                d.disk_mtime = Some(cur);
                if !d.conflict {
                    d.conflict = true;
                    notes.push(format!("{} changed on disk and you have unsaved changes (Ctrl+S overwrites)", d.name));
                }
            } else {
                // Keep the old text in the local history before another program's version replaces it.
                if let (Some(s), Some(p)) = (&hist, d.path()) {
                    crate::localhist::record_buf(s, p, &d.buf, crate::localhist::Source::Reload);
                }
                if d.reload_from_disk().is_ok() {
                    notes.push(format!("{} reloaded (changed on disk)", d.name));
                }
            }
        }
        if let Some(n) = notes.pop() {
            self.say(n);
            self.request_redraw();
        }
    }
}
