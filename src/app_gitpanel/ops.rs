use super::*;

impl App {
    /// Width the panel takes when open (also valid while the panel state is taken out of `self`).
    pub(super) fn gp_width(&self) -> i32 {
        let w = self.size.0 as i32;
        match self.config.git_panel_width {
            0 => auto_panel_width(w),
            n => clamp_panel_width(n, w),
        }
    }

    /// Is (x, y) on the draggable left edge of the open panel (the switch above it included)?
    pub(super) fn gp_on_edge(&self, x: f64, y: f64) -> bool {
        let p = self.right_slot_rect();
        self.gitpanel.is_some() && Rect::new(p.x - GP_GRAB, p.y, 2 * GP_GRAB, p.h).contains(x, y)
    }

    /// Mouse button released: ends an edge drag and stores the width in `config.json`.
    pub fn gitpanel_release(&mut self) {
        let selected = self.gitpanel.as_mut().is_some_and(|gp| {
            gp.msg_auto = None;
            gp.msg.release()
        });
        if selected {
            self.request_redraw();
        }
        if self.gitpanel.as_mut().is_some_and(|gp| std::mem::take(&mut gp.resizing)) {
            self.config.save();
            self.request_redraw();
        }
    }

    /// Width taken from the editor area by the right-hand slot (Git or AI panel): 0 when closed.
    pub fn gitpanel_w(&self) -> i32 {
        if self.gitpanel.is_none() && !self.ai.open { 0 } else { self.gp_width() }
    }

    /// The whole right-hand slot, as if open: the "Git | AI" switch on top, then the panel.
    pub fn right_slot_rect(&self) -> Rect {
        let w = self.gp_width();
        let (ww, hh) = (self.size.0 as i32, self.size.1 as i32);
        Rect::new(ww - w, crate::app::TOP_H, w, (hh - crate::app::TOP_H - crate::app::STATUS_H).max(0))
    }

    /// Panel rectangle (as if open): the slot under its switch.
    pub fn gitpanel_rect(&self) -> Rect {
        let s = self.right_slot_rect();
        let sw = crate::app_aipanel::SLOT_H;
        Rect::new(s.x, s.y + sw, s.w, (s.h - sw).max(0))
    }

    /// Ctrl+Shift+G / palette: open, or focus, or close when it already has the focus.
    pub fn toggle_git_panel(&mut self) {
        match self.gitpanel.as_mut() {
            None => self.open_git_panel(),
            Some(gp) if gp.focused => self.close_git_panel(),
            Some(gp) => gp.focused = true,
        }
    }

    /// Bottom bar button: plain open / close.
    pub fn git_button(&mut self) {
        if self.gitpanel.is_some() {
            self.close_git_panel();
        } else {
            self.open_git_panel();
        }
    }

    pub fn open_git_panel(&mut self) {
        // The Git and AI panels share the right-hand slot (the agent keeps running).
        self.ai_hide();
        self.refresh_git_status();
        let mut gp = Box::new(GitPanel::new(&self.root));
        // A command that kept running while the panel was closed is shown again.
        gp.job = self.gitbg.detached.take();
        self.gitpanel = Some(gp);
    }

    pub fn close_git_panel(&mut self) {
        if let Some(mut gp) = self.gitpanel.take() {
            self.git_detach_job(&mut gp);
        }
        self.focus = crate::app::Focus::Editor;
    }

    pub(crate) fn sync_docs_with_head(&mut self) {
        for d in &mut self.docs {
            if let Some(p) = d.path().map(Path::to_path_buf) {
                d.git_base = git::head_text(&p);
                d.git_stale = true;
            }
        }
    }

    /// Re-read status / history when the index, HEAD or the working tree moved under us.
    pub(super) fn gp_sync(&mut self, gp: &mut GitPanel) {
        let mut h = DefaultHasher::new();
        let mut acc = 0u64;
        for (p, b) in &self.git_status {
            let mut e = DefaultHasher::new();
            (p, b).hash(&mut e);
            acc ^= e.finish();
        }
        acc.hash(&mut h);
        self.root.hash(&mut h);
        let gitdir = gp.top.clone().unwrap_or_else(|| self.root.clone()).join(".git");
        mtime(&gitdir.join("index")).hash(&mut h);
        let sig = h.finish();
        let hist =
            mtime(&gitdir.join("logs").join("HEAD")) as u64 ^ (mtime(&gitdir.join("HEAD")) as u64).rotate_left(7);
        if gp.root != self.root {
            gp.root = self.root.clone();
            gp.reset_history();
        }
        if sig != gp.sig {
            gp.sig = sig;
            gp.refresh();
        }
        if hist != gp.hist_sig {
            gp.hist_sig = hist;
            gp.reset_history();
        }
    }

    // ───────────── actions ─────────────

    pub(super) fn gp_after_change(&mut self, gp: &mut GitPanel) {
        self.refresh_git_status();
        gp.refresh();
    }

    pub(super) fn gp_toggle_stage(&mut self, gp: &mut GitPanel, i: usize) {
        let (Some(top), Some(c)) = (gp.top.clone(), gp.changes.get(i).cloned()) else { return };
        let mut paths = vec![c.path.clone()];
        if c.check_state() == CheckState::On {
            paths.extend(c.old.clone());
        }
        let r = if c.check_state() == CheckState::On { unstage(&top, &paths) } else { stage(&top, &paths) };
        if let Err(e) = r {
            gp.say(format!("git: {e}"), false);
        }
        self.gp_after_change(gp);
    }

    pub(super) fn gp_section(&mut self, gp: &mut GitPanel, g: Group) {
        let Some(top) = gp.top.clone() else { return };
        let paths = gp.group_paths(g);
        let r = if g == Group::Staged { unstage(&top, &paths) } else { stage(&top, &paths) };
        if let Err(e) = r {
            gp.say(format!("git: {e}"), false);
        }
        self.gp_after_change(gp);
    }

    pub(super) fn gp_discard(&mut self, gp: &mut GitPanel, i: usize) {
        let (Some(top), Some(c)) = (gp.top.clone(), gp.changes.get(i).cloned()) else { return };
        match discard(&top, &c) {
            Ok(()) => {
                let abs = top.join(&c.path);
                if let Some(k) = self.docs.iter().position(|d| d.path() == Some(abs.as_path())) {
                    self.hist_snapshot(k, crate::localhist::Source::Reload);
                    let _ = self.docs[k].reload_from_disk();
                    self.docs[k].git_stale = true;
                }
                gp.say(format!("{} descartado", c.path), true);
                self.say(format!("{} descartado", c.path));
            }
            Err(e) => gp.say(format!("discard: {e}"), false),
        }
        self.diff = None;
        self.gp_after_change(gp);
    }

    /// Check / uncheck "Amend": checking loads the last commit's message into the box (the draft
    /// is kept and comes back when unchecking).
    pub(super) fn gp_toggle_amend(&mut self, gp: &mut GitPanel) {
        let Some(top) = gp.top.clone() else { return };
        if gp.amend {
            gp.amend = false;
            gp.msg.set_text(&std::mem::take(&mut gp.amend_draft));
        } else {
            match gitsync::last_message(&top) {
                Some(m) => {
                    gp.amend_draft = gp.msg.text().to_string();
                    gp.msg.set_text(&m);
                    gp.amend = true;
                    gp.msg_focus = true;
                }
                None => gp.say("No commit to amend", false),
            }
        }
    }

    pub(super) fn gp_amend(&mut self, gp: &mut GitPanel, top: &Path) {
        // The commit being rewritten is already on the upstream when nothing is ahead of it.
        let published = gp.sync.upstream.is_some() && gp.sync.ahead == 0;
        match gitsync::commit_amend(top, gp.msg.text()) {
            Ok(line) => {
                gp.amend = false;
                gp.msg.set_text(&std::mem::take(&mut gp.amend_draft));
                let extra = if published { " (already published: the push requires force)" } else { "" };
                gp.say(format!("Commit amended{extra}"), true);
                if published {
                    gp.force = Some(ForceWhy::Amended);
                }
                self.say(format!("amend: {line}"));
                self.sync_docs_with_head();
                self.diff = None;
                gp.reset_history();
            }
            Err(e) => {
                gp.say(format!("git commit --amend: {e}"), false);
                self.say(format!("git commit --amend: {e}"));
            }
        }
        self.gp_after_change(gp);
    }

    pub(super) fn gp_commit(&mut self, gp: &mut GitPanel) {
        let Some(top) = gp.top.clone() else { return };
        if gp.msg.text().trim().is_empty() {
            gp.say("Write a commit message", false);
            gp.msg_focus = true;
            return;
        }
        if gp.amend {
            return self.gp_amend(gp, &top);
        }
        if gp.staged() == 0 {
            if gp.changes.is_empty() {
                return gp.say("Nothing to commit", false);
            }
            if !gp.include_all {
                return gp.say("Nothing staged: tick files or enable \"include all\"", false);
            }
            if let Err(e) = git_run(&top, &["add", "-A"]) {
                return gp.say(format!("git add: {e}"), false);
            }
        }
        match commit(&top, gp.msg.text()) {
            Ok(line) => {
                gp.msg.set_text("");
                gp.say(line.clone(), true);
                self.say(format!("commit: {line}"));
                self.sync_docs_with_head();
                self.diff = None;
                gp.reset_history();
            }
            Err(e) => {
                gp.say(format!("git commit: {e}"), false);
                self.say(format!("git commit: {e}"));
            }
        }
        self.gp_after_change(gp);
    }

    // ───────────── remote: fetch / pull / push ─────────────

    pub(super) fn gp_start(&mut self, gp: &mut GitPanel, op: Op) {
        let Some(top) = gp.top.clone() else { return };
        if gp.job.is_some() {
            return gp.say("Wait: an operation is already in progress", false);
        }
        gp.sync_note = None;
        gp.publish = None;
        gp.force = None;
        gp.strategy_ask = false;
        gp.job = Some(gitsync::spawn(&top, op, self.wake()));
        gp.spun = Instant::now();
    }

    pub(super) fn gp_pull(&mut self, gp: &mut GitPanel) {
        if gp.top.is_some() && gp.sync.upstream.is_none() {
            let b = self.branch.clone().unwrap_or_default();
            return self.gp_note(gp, format!("Branch “{b}” has no upstream: use Push to publish it first"), false);
        }
        let op = match PullStrategy::from_name(&self.config.git_pull_strategy) {
            Some(st) => Op::PullWith(st),
            None => Op::Pull,
        };
        self.gp_start(gp, op);
    }

    /// Remember `st` as the pull strategy and pull with it.
    pub(super) fn gp_pull_with(&mut self, gp: &mut GitPanel, st: PullStrategy) {
        self.config.git_pull_strategy = st.name().to_string();
        self.config.save();
        gp.strategy_ask = false;
        self.gp_start(gp, Op::PullWith(st));
    }

    pub(super) fn gp_push(&mut self, gp: &mut GitPanel) {
        if gp.sync.upstream.is_none() {
            return self.gp_ask_publish(gp);
        }
        if gp.force.is_some() {
            return;
        }
        self.gp_start(gp, Op::Push);
    }

    /// Branch without upstream: ask before publishing it (`push --set-upstream`).
    pub(super) fn gp_ask_publish(&mut self, gp: &mut GitPanel) {
        let Some(top) = gp.top.clone() else { return };
        let Some(branch) = self.branch.clone().filter(|b| b != "HEAD") else {
            return self.gp_note(gp, "Detached HEAD (no branch): create a branch before pushing".into(), false);
        };
        match gitsync::default_remote(&top, &branch) {
            Some(remote) => {
                gp.sync_note = None;
                gp.publish = Some((remote, branch));
            }
            None => self.gp_note(gp, "No remote configured (git remote add origin <url>)".into(), false),
        }
    }

    pub(super) fn gp_note(&mut self, gp: &mut GitPanel, text: String, ok: bool) {
        self.say(&text);
        gp.sync_note = Some((text, ok, Instant::now()));
    }

    pub(super) fn gp_job_done(&mut self, gp: &mut GitPanel, op: Op, res: Result<String, Fail>) {
        let res_copy = res.clone();
        match res {
            Ok(msg) => self.gp_note(gp, msg, true),
            Err(f) if f.kind == FailKind::NoUpstream && op == Op::Push => return self.gp_ask_publish(gp),
            // A pull that cannot fast-forward: let the user choose merge / rebase / ff-only.
            Err(f) if f.kind == FailKind::Diverged && matches!(op, Op::Pull | Op::PullWith(PullStrategy::FfOnly)) => {
                self.gp_note(gp, format!("{}: {}", op.label(), f.text), false);
                gp.strategy_ask = true;
            }
            // A rejected push with nothing new on the remote is rewritten history: offer the force.
            Err(f) if f.kind == FailKind::Rejected && op == Op::Push => {
                gp.refresh();
                let rewrote = gp.top.as_deref().is_some_and(gitsync::rewrote_published);
                if rewrote {
                    gp.sync_note = None;
                    gp.force = Some(ForceWhy::Rejected);
                    self.say(format!("{}: {}", op.label(), f.text));
                } else {
                    self.gp_note(
                        gp,
                        "Push: rejected, the remote has commits you do not have. Fetch and pull before pushing.".into(),
                        false,
                    );
                }
            }
            Err(f) => self.gp_note(gp, format!("{}: {}", op.label(), f.text), false),
        }
        self.worktree_job_done(&op, &res_copy, Some(&mut *gp));
        // A pull moves HEAD and files; everything the panel shows may have changed.
        self.sync_docs_with_head();
        self.refresh_tree();
        gp.reset_history();
        self.gp_after_change(gp);
    }

    /// Per-event hook (`App::tick`): collect the finished command and keep the elapsed counter alive.
    pub fn gitpanel_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some(mut gp) = self.gitpanel.take() else { return };
        if let Some(job) = gp.job.as_mut() {
            let before = job.progress.len();
            match job.poll() {
                Some(res) => {
                    let op = job.op.clone();
                    gp.job = None;
                    self.gp_job_done(&mut gp, op, res);
                    self.request_redraw();
                }
                None => {
                    let due = gp.spun + Duration::from_millis(500);
                    if job.progress.len() != before || due <= now {
                        gp.spun = now;
                        self.request_redraw();
                    }
                    want(gp.spun + Duration::from_millis(500));
                }
            }
        }
        self.gp_msg_autoscroll(&mut gp, now, want);
        if gp.closed {
            self.git_detach_job(&mut gp);
            self.focus = crate::app::Focus::Editor;
        } else {
            self.gitpanel = Some(gp);
        }
    }

    /// Dragging a selection above / below the message box keeps scrolling it (faster the
    /// farther the pointer is) and the selection follows.
    fn gp_msg_autoscroll(&mut self, gp: &mut GitPanel, now: Instant, want: &mut impl FnMut(Instant)) {
        let (mx, my) = self.mouse;
        let b = gp.msg_box;
        let dist = if !gp.msg.dragging() {
            0.0
        } else if my < b.y as f64 {
            my - b.y as f64
        } else if my > b.bottom() as f64 {
            my - b.bottom() as f64
        } else {
            0.0
        };
        if dist == 0.0 {
            gp.msg_auto = None;
            return;
        }
        let dt = gp.msg_auto.map_or(0.016, |t| now.saturating_duration_since(t).as_secs_f64().min(0.1));
        gp.msg_auto = Some(now);
        let speed = dist.signum() * (60.0 + dist.abs() * 10.0).min(2000.0);
        if gp.msg.scroll_by((speed * dt) as f32) {
            let (ox, oy) = gp.msg_origin();
            let tx = &mut self.tx;
            gp.msg.relayout(&mut |s: &str| tx.ui_char_xs(s, false, None));
            gp.msg.drag_to((mx - ox) as f32, (my - oy) as f32);
            self.request_redraw();
        }
        want(now + Duration::from_millis(16));
    }

    pub(super) fn gp_open_diff_change(&mut self, gp: &mut GitPanel, i: usize) {
        let (Some(top), Some(c)) = (gp.top.clone(), gp.changes.get(i).cloned()) else { return };
        let big = std::fs::metadata(top.join(&c.path)).is_ok_and(|m| m.len() > 3_000_000);
        if big {
            return gp.say("File too large for the side-by-side diff", false);
        }
        let name = split_name(&c.path).0.to_string();
        // The Staged entry opens on the staged side (its hunks can be unstaged), the others on the
        // not-staged side (their hunks can be staged). A staged + modified file offers both.
        let side = if c.group == Group::Staged { Side::Staged } else { Side::Unstaged };
        self.diff = DiffView::staging_auto(&top, &c.path, c.old.as_deref(), side, &name);
        if self.diff.is_none() {
            gp.say("No content differences to show", false);
        }
    }

    pub(super) fn gp_open_diff_commit(&mut self, gp: &mut GitPanel, j: usize) {
        let (Some(top), Some(ci)) = (gp.top.clone(), gp.open_commit) else { return };
        let (Some(f), Some(c)) = (gp.files.get(j).cloned(), gp.commits.get(ci).cloned()) else { return };
        let (old, new) = commit_pair(&top, &c.hash, &f);
        let rows = gitx::side_by_side(&old, &new);
        let name = split_name(&f.path).0.to_string();
        self.diff = Some(DiffView::plain(
            format!("{name} — {}  {}", c.short, c.subject),
            format!("{}^", c.short),
            c.short.clone(),
            rows,
        ));
        gp.open_file = Some(j);
    }

    pub(super) fn gp_toggle_commit(&mut self, gp: &mut GitPanel, i: usize) {
        if gp.open_commit == Some(i) {
            gp.open_commit = None;
            gp.files.clear();
            gp.open_file = None;
            return;
        }
        let (Some(top), Some(c)) = (gp.top.clone(), gp.commits.get(i).cloned()) else { return };
        gp.files = commit_files(&top, &c.hash);
        gp.open_commit = Some(i);
        gp.open_file = None;
        // Another commit may have been collapsed above this one: re-find it and bring the commit
        // together with its files into view.
        if let Some(k) = gp.rows().iter().position(|r| *r == Row::Commit(i)) {
            gp.cursor = k;
            let view = self.gitpanel_rect().h - gp.chrome_h();
            gp.reveal_span(k, k + gp.files.len(), view);
        }
        if !gp.files.is_empty() {
            self.gp_open_diff_commit(gp, 0);
        } else {
            self.diff = None;
        }
    }

    /// Activate a row (Enter / click on the body of a row).
    pub(super) fn gp_activate(&mut self, gp: &mut GitPanel, r: Row) {
        match r {
            Row::File(i) => self.gp_open_diff_change(gp, i),
            Row::Commit(i) => self.gp_toggle_commit(gp, i),
            Row::CFile(j) => self.gp_open_diff_commit(gp, j),
            Row::Header(_) => {}
        }
    }
}
