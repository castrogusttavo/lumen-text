//! The pull request side of the Git panel: the workers (`pr`) behind it, the "Create PR" form, the
//! "COMMITTED ON BRANCH" rows of the Changes list and the Checks tab (checks, review comments).

use super::*;

/// Most lines of one comment paragraph shown in the Checks tab.
const COMMENT_LINES: usize = 12;

// ───────────── the panel's side: workers ─────────────

impl GitPanel {
    /// Read the branch's commits on a worker.
    pub fn request_branch(&mut self) {
        let Some(top) = self.top.clone() else { return };
        self.branch_at = Some(Instant::now());
        let (tx, wake) = (self.pr_ch.0.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let _ = tx.send(PrMsg::Branch(read_branch(&top)));
            if let Some(w) = wake {
                w();
            }
        });
    }

    /// Ask for the branch's pull request on a worker.
    pub fn request_detail(&mut self) {
        let Some(top) = self.top.clone() else { return };
        if self.detail.loading {
            return;
        }
        self.detail.loading = true;
        let (tx, wake) = (self.pr_ch.0.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let _ = tx.send(PrMsg::Detail(read_pr(&top)));
            if let Some(w) = wake {
                w();
            }
        });
    }

    /// Open the form with a title and description made from the branch's commits.
    pub fn open_form(&mut self) {
        if self.form.is_some() {
            return;
        }
        self.tab = Tab::Changes;
        let mut f = PrForm {
            title: TextArea::default(),
            body: TextArea::default(),
            base: self.branch.as_ref().map_or_else(|| "main".to_string(), |b| b.base.clone()),
            draft: false,
            focus: FormField::Title,
            creating: false,
            error: None,
            title_box: Rect::default(),
            body_box: Rect::default(),
        };
        if let Some(b) = &self.branch {
            let (t, body) = pr_defaults(b);
            f.title.set_text(&t);
            f.body.set_text(&body);
        }
        self.form = Some(f);
        self.request_branch();
        self.msg_focus = false;
        self.scroll[0] = 0.0;
    }

    /// The "Generate" button: the title and description again, from the commits.
    pub fn generate_pr(&mut self) {
        let Some(b) = self.branch.clone() else { return };
        let (t, body) = pr_defaults(&b);
        if let Some(f) = self.form.as_mut() {
            f.title.set_text(&t);
            f.body.set_text(&body);
            f.error = None;
        }
    }

    /// Create the pull request on a worker (publishing the branch first when needed).
    pub fn submit_form(&mut self, draft: bool) {
        let Some(top) = self.top.clone() else { return };
        let Some(f) = self.form.as_mut() else { return };
        if f.creating {
            return;
        }
        let title = f.title.text().trim().to_string();
        if title.is_empty() {
            f.error = Some("A title is needed".to_string());
            return;
        }
        f.creating = true;
        f.error = None;
        let (body, base) = (f.body.text().trim().to_string(), f.base.clone());
        let (tx, wake) = (self.pr_ch.0.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let _ = tx.send(PrMsg::Created(create_pr(&top, &title, &body, &base, draft)));
            if let Some(w) = wake {
                w();
            }
        });
    }

    /// Fold in what the workers sent; true when the panel should be drawn again.
    pub fn pump_pr(&mut self) -> bool {
        let mut changed = false;
        while let Ok(m) = self.pr_ch.1.try_recv() {
            changed = true;
            match m {
                PrMsg::Branch(b) => {
                    // A form opened before the commits were known gets its title now.
                    if let Some(f) = self.form.as_mut()
                        && f.title.text().is_empty()
                        && f.body.text().is_empty()
                    {
                        let (t, body) = pr_defaults(&b);
                        f.title.set_text(&t);
                        f.body.set_text(&body);
                        f.base = b.base.clone();
                    }
                    self.branch = Some(b);
                }
                PrMsg::Detail(r) => {
                    self.detail.loading = false;
                    self.detail.loaded = true;
                    self.detail.at = Some(Instant::now());
                    match r {
                        Ok(info) => {
                            self.detail.info = info;
                            self.detail.err = None;
                        }
                        Err(e) => self.detail.err = Some(e),
                    }
                }
                PrMsg::Created(Ok(url)) => {
                    self.form = None;
                    self.detail.loaded = false;
                    self.say(
                        if url.is_empty() {
                            "Pull request created".to_string()
                        } else {
                            format!("Pull request created: {url}")
                        },
                        true,
                    );
                    self.pending_url = (!url.is_empty()).then_some(url);
                    self.tab = Tab::Checks;
                    self.request_detail();
                }
                PrMsg::Created(Err(e)) => {
                    if let Some(f) = self.form.as_mut() {
                        f.creating = false;
                        f.error = Some(e);
                    }
                }
            }
        }
        changed
    }
}

/// Plain text of a Markdown comment: markers and HTML comments dropped, paragraphs kept.
pub fn clean_markdown(body: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_html_comment = false;
    for l in body.lines() {
        let t = l.trim();
        if in_html_comment {
            in_html_comment = !t.contains("-->");
            continue;
        }
        if t.starts_with("<!--") {
            in_html_comment = !t.contains("-->");
            continue;
        }
        if t.starts_with('<') && t.ends_with('>') && !t.contains(' ') {
            continue;
        }
        let t = t.trim_start_matches('#').trim_start().replace("**", "").replace('`', "");
        out.push(t);
    }
    out.join("\n").trim().to_string()
}

/// Room between the "New PR" title and the buttons at the right of the form header.
fn g_left_room(area: Rect, title_w: i32) -> i32 {
    (area.w - 2 * COMPOSER_MARGIN - 26 - title_w - 10 - 120).max(0)
}

// ───────────── drawing ─────────────

impl App {
    /// While the Checks tab is on screen the pull request is asked for again every so often (sooner
    /// while checks are still running), so the results appear without a click.
    pub fn gp_pr_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some(gp) = self.gitpanel.as_mut() else { return };
        if gp.tab != Tab::Checks || !gp.detail.loaded || gp.detail.loading {
            return;
        }
        let every = if gp.detail.info.as_ref().is_some_and(|i| i.pending() > 0) { 15 } else { 60 };
        let Some(due) = gp.detail.at.map(|t| t + Duration::from_secs(every)) else { return };
        if due <= now {
            gp.request_detail();
        } else {
            want(due);
        }
    }

    /// The "Create PR" form that replaces the commit box: header with Generate, title, description,
    /// the base branch and the Create button (draft behind its chevron).
    pub(super) fn gp_draw_form(&mut self, cv: &mut Canvas, gp: &mut GitPanel, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let Some(mut f) = gp.form.take() else { return };
        let m = COMPOSER_MARGIN;
        let w = area.w - 2 * m;
        let x = area.x + m;
        let mut y = area.y + 6;
        // Header: icon + "New PR", Cancel and Generate at the right.
        self.tx.icon(cv, "ui/git_branch", x + 2, y + 8, 16, dim, area);
        self.tx.draw_ui(cv, x + 26, y + 6, "New PR", strong, area, true);
        if let Some(b) = gp.branch.as_ref().filter(|b| !b.commits.is_empty()) {
            let (add, del) = b.totals();
            let n = b.commits.len();
            let info = format!("{n} commit{} · +{add} -{del}", if n == 1 { "" } else { "s" });
            let nw = self.tx.ui_width("New PR", true);
            let room = g_left_room(area, nw);
            let t = fit(&mut self.tx, &info, room, false);
            self.tx.draw_ui_sized(cv, x + 26 + nw + 10, y + 8, &t, dim.with_alpha(0xb0), area, false, 12.0);
        }
        let gen_label = "Generate";
        let gw = self.tx.ui_width(gen_label, false) + 34;
        let g = Rect::new(area.right() - m - gw, y + 2, gw, 26);
        if self.hv(g) {
            cv.fill_round(g, 8, strong.with_alpha(0x18));
        }
        self.tx.icon(cv, "ui/sparkle", g.x + 8, g.y + 5, 16, dim, g);
        self.tx.draw_ui(cv, g.x + 28, g.y + 4, gen_label, dim, g, false);
        gp.hits.push((g, Hit::PrGenerate));
        let cancel = Rect::new(g.x - 30, y + 2, 26, 26);
        self.gp_icon_btn(cv, gp, cancel, "ui/close", Hit::PrCancel);
        y += 36;

        // Title: one line.
        let tb = Rect::new(x, y, w, 40);
        let ring = |focus: bool| if focus { c.accent.with_alpha(0xa0) } else { c.border.with_alpha(0xa0) };
        cv.fill_round(tb, 10, c.input_bg);
        cv.ring(tb, 10, ring(f.focus == FormField::Title && gp.focused));
        f.title_box = tb;
        {
            let tx = &mut self.tx;
            f.title.layout((tb.w - 24) as f32, &mut |s: &str| tx.ui_char_xs(s, false, None));
            f.title.set_view(18.0, 18.0);
        }
        self.gp_paint_area(
            cv,
            &f.title,
            Rect::new(tb.x + 12, tb.y + 11, tb.w - 24, 18),
            "Title",
            f.focus == FormField::Title && gp.focused,
        );
        gp.hits.push((tb, Hit::PrTitle));
        y += 44 + 8;

        // Description: a few lines.
        let bb = Rect::new(x, y, w, 96);
        cv.fill_round(bb, 10, c.input_bg);
        cv.ring(bb, 10, ring(f.focus == FormField::Body && gp.focused));
        f.body_box = bb;
        {
            let tx = &mut self.tx;
            f.body.layout((bb.w - 24) as f32, &mut |s: &str| tx.ui_char_xs(s, false, None));
            f.body.set_view(18.0, (bb.h - 20) as f32);
        }
        self.gp_paint_area(
            cv,
            &f.body,
            Rect::new(bb.x + 12, bb.y + 10, bb.w - 24, bb.h - 20),
            "Description",
            f.focus == FormField::Body && gp.focused,
        );
        gp.hits.push((bb, Hit::PrBody));
        y += 96 + 8;

        // Base branch.
        self.tx.draw_ui_sized(cv, x + 4, y + 5, "Base branch", dim, area, false, 13.0);
        let bw = self.tx.ui_width(&f.base, true);
        self.tx.draw_ui(cv, area.right() - m - 4 - bw, y + 4, &f.base, strong, area, true);
        gp.hits.push((Rect::new(x, y, w, 28), Hit::PrBase));
        y += 28 + 8;

        // Create (or "Creating…"), with the draft option behind the chevron.
        let blk = Rect::new(x, y, w, 38);
        let cw = 34;
        let main = Rect::new(blk.x, blk.y, blk.w - cw, blk.h);
        let chev = Rect::new(main.right(), blk.y, cw, blk.h);
        let ready = !f.title.text().trim().is_empty() && !f.creating;
        cv.fill_round(blk, 10, if ready { c.accent } else { c.accent.with_alpha(0x60) });
        let over = c.on_accent.with_alpha(0x2a);
        if ready && self.hv(main) {
            cv.fill_round_clip(blk, 10, over, main);
        }
        if ready && (self.hv(chev) || gp.menu == Some(GpMenu::Pr)) {
            cv.fill_round_clip(blk, 10, over, chev);
        }
        let col = if ready { c.on_accent } else { c.on_accent.with_alpha(0xc0) };
        cv.fill(Rect::new(main.right(), blk.y + 10, 1, 18), col.with_alpha(0x50));
        let label = if f.creating {
            "Creating…"
        } else if f.draft {
            "Create draft pull request"
        } else {
            "Create pull request"
        };
        let lw = self.tx.ui_width(label, true);
        let lead = if f.creating { 22 } else { 0 };
        let lx = main.x + (main.w - lw - lead) / 2;
        if f.creating {
            crate::app_tests::spinner_t(cv, lx + 7, blk.y + blk.h / 2, 7, col, c.accent, spin_clock(), main);
            self.request_redraw();
        }
        self.tx.draw_ui(cv, lx + lead, blk.y + 9, label, col, main, true);
        self.tx.icon(cv, "ui/chevron_down", chev.x + 9, blk.y + 11, 16, col, chev);
        if ready {
            gp.hits.push((main, if f.draft { Hit::PrCreateDraft } else { Hit::PrCreate }));
            gp.hits.push((chev, Hit::Menu(GpMenu::Pr)));
        }
        y += 38 + 4;
        if let Some(e) = f.error.clone() {
            let t = fit(&mut self.tx, &e, w, false);
            self.tx.draw_ui_sized(cv, x + 2, y, &t, c.error, area, false, 12.0);
        }
        gp.form = Some(f);
    }

    /// The text of a form box: placeholder, selection, lines, caret.
    fn gp_paint_area(&mut self, cv: &mut Canvas, ta: &TextArea, r: Rect, placeholder: &str, focused: bool) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let lh = 18;
        if ta.text().is_empty() {
            self.tx.draw_ui(cv, r.x, r.y, placeholder, dim, r, false);
        }
        let scroll = ta.scroll.round() as i32;
        let row_y = |k: usize| r.y + k as i32 * lh - scroll;
        let first = (scroll / lh).max(0) as usize;
        let last = ((scroll + r.h) / lh + 1).max(0) as usize;
        for (k, x0, x1) in ta.sel_spans() {
            if (first..=last).contains(&k) {
                let (a, z) = (x0.round() as i32, x1.round() as i32);
                cv.fill_clip(Rect::new(r.x + a, row_y(k), (z - a).max(1), lh), self.theme.selection, r);
            }
        }
        for (k, l) in ta.lines().iter().enumerate().take(last + 1).skip(first) {
            let t = ta.line_text(l);
            self.tx.draw_ui(cv, r.x, row_y(k), &t, strong, r, false);
        }
        if focused {
            let (k, x) = ta.caret();
            cv.fill_clip(Rect::new(r.x + (x.round() as i32).min(r.w + 4), row_y(k) + 1, 1, lh - 2), strong, r);
        }
    }

    /// The header of the committed list: "COMMITTED ON BRANCH n".
    pub(super) fn gp_draw_branch_head(&mut self, cv: &mut Canvas, gp: &mut GitPanel, y: i32, body: Rect) {
        let (strong, dim) = (self.theme.chrome.tab_active_fg, self.theme.chrome.tab_inactive_fg);
        let n = gp.branch.as_ref().map_or(0, |b| b.commits.len());
        let vis = Self::gp_vis(body, y, SEC_H);
        let row = Rect::new(body.x, y, body.w, SEC_H);
        if self.hv(row) && self.hv(vis) {
            cv.fill_round_clip(Rect::new(body.x + 6, y, body.w - 12, SEC_H), 8, strong.with_alpha(0x0c), body);
        }
        gp.hits.push((row.intersect(&vis), Hit::BranchHead));
        let ty = y + (SEC_H - 16) / 2 + 1;
        let chev = if gp.branch_folded { "ui/chevron_right" } else { "ui/chevron_down" };
        self.tx.icon(cv, chev, body.x + 10, y + (SEC_H - 16) / 2, 16, dim, body);
        let title = "COMMITTED ON BRANCH";
        self.tx.draw_ui_sized(cv, body.x + 30, ty, title, dim, body, true, 12.0);
        let tw = self.tx.ui_width_sized(title, true, 12.0);
        self.tx.draw_ui_sized(cv, body.x + 30 + tw + 8, ty, &n.to_string(), dim.with_alpha(0xb0), body, false, 12.0);
    }

    /// One commit of the branch: subject on the left, its short hash at the right.
    pub(super) fn gp_draw_branch_commit(&mut self, cv: &mut Canvas, gp: &mut GitPanel, i: usize, y: i32, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let Some(cm) = gp.branch.as_ref().and_then(|b| b.commits.get(i)).cloned() else { return };
        let h = Self::row_height_branch_commit();
        let vis = Self::gp_vis(body, y, h);
        let r = Rect::new(body.x + 6, y, body.w - 12, h);
        if self.hv(r) && self.hv(vis) {
            cv.fill_round_clip(r, 8, strong.with_alpha(0x0c), body);
        }
        gp.hits.push((r.intersect(&vis), Hit::BranchCommit(i)));
        // A commit with files shows a chevron: click folds them.
        let icon = if cm.files.is_empty() {
            "ui/git_commit"
        } else if gp.branch_commits_folded.contains(&i) {
            "ui/chevron_right"
        } else {
            "ui/chevron_down"
        };
        self.tx.icon(cv, icon, r.x + 8, y + (h - 16) / 2, 16, dim, body);
        let hw = self.tx.ui_width_sized(&cm.hash, false, 12.0);
        let subj = fit(&mut self.tx, &cm.subject, r.w - 34 - hw - 16, false);
        self.tx.draw_ui(cv, r.x + 32, y + (h - 18) / 2, &subj, strong, body, false);
        self.tx.draw_ui_sized(cv, r.right() - 8 - hw, y + (h - 14) / 2 + 1, &cm.hash, dim, body, false, 12.0);
    }

    fn row_height_branch_commit() -> i32 {
        34
    }

    /// One file of a branch commit, indented under it: icon, name, its folder dim, +N -N, status letter.
    pub(super) fn gp_draw_branch_file(
        &mut self,
        cv: &mut Canvas,
        gp: &GitPanel,
        i: usize,
        j: usize,
        y: i32,
        body: Rect,
    ) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let Some(f) = gp.branch.as_ref().and_then(|b| b.commits.get(i)).and_then(|cm| cm.files.get(j)) else { return };
        let h = FILE_H;
        let r = Rect::new(body.x + 6, y, body.w - 12, h);
        if self.hv(r) {
            cv.fill_round_clip(r, 8, strong.with_alpha(0x0c), body);
        }
        let (name, dir) = split_name(&f.path);
        let ix = r.x + 28;
        let ty = y + (h - 18) / 2;
        let icon = self.tx.icons.file_icon(name);
        self.tx.icon(cv, &icon, ix, y + (h - 16) / 2, 16, dim, body);
        // Right side: status letter, then +N -N.
        let letter = f.code.to_string();
        let lw = self.tx.ui_width(&letter, true);
        let tint = code_color(&c, f.code);
        self.tx.draw_ui(cv, r.right() - 8 - lw, ty, &letter, tint, body, true);
        let (a, d) = (format!("+{}", f.add), format!("-{}", f.del));
        let (aw, dw) = (self.tx.ui_width_sized(&a, false, 11.0), self.tx.ui_width_sized(&d, false, 11.0));
        let sx = r.right() - 8 - lw - 10 - dw;
        self.tx.draw_ui_sized(cv, sx, ty + 2, &d, c.error.with_alpha(0x99), body, false, 11.0);
        self.tx.draw_ui_sized(cv, sx - 6 - aw, ty + 2, &a, c.git_added.with_alpha(0x99), body, false, 11.0);
        let room = (sx - 6 - aw - 8) - (ix + 22);
        let nm = fit(&mut self.tx, name, room, false);
        self.tx.draw_ui(cv, ix + 22, ty, &nm, strong, body, false);
        let nw = self.tx.ui_width(&nm, false);
        let left = room - nw - 8;
        if !dir.is_empty() && left > 30 {
            let d = fit(&mut self.tx, dir, left, false);
            self.tx.draw_ui_sized(cv, ix + 22 + nw + 8, ty + 1, &d, dim.with_alpha(0xa0), body, false, 12.0);
        }
    }

    // ───────────── the Checks tab ─────────────

    /// The branch's pull request: its number and state, the checks, the comments.
    pub(super) fn gp_draw_checks(&mut self, cv: &mut Canvas, gp: &mut GitPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        cv.fill(Rect::new(body.x, body.y, body.w, 1), c.border);
        // Ask on the first look (`gp_pr_tick` asks again every so often).
        if !gp.detail.loading && !gp.detail.loaded {
            gp.request_detail();
        }
        let center = |app: &mut App, cv: &mut Canvas, y: i32, text: &str, col: Rgba, bold: bool| {
            let w = app.tx.ui_width(text, bold);
            app.tx.draw_ui(cv, body.x + (body.w - w) / 2, y, text, col, body, bold);
        };
        if !gp.detail.loaded {
            center(self, cv, body.y + 40, "Looking for the pull request…", dim, false);
            return;
        }
        if let Some(e) = gp.detail.err.clone().filter(|_| gp.detail.info.is_none()) {
            let msg = if e == "gh not found" { "GitHub CLI (gh) not found".to_string() } else { e.clone() };
            center(self, cv, body.y + 40, &msg, strong, true);
            let hint = if e == "gh not found" {
                "Install gh and run gh auth login."
            } else {
                "Is this branch pushed to GitHub?"
            };
            center(self, cv, body.y + 62, hint, dim, false);
            return;
        }
        let Some(pr) = gp.detail.info.clone() else {
            center(self, cv, body.y + 48, "No pull request for this branch", strong, true);
            center(self, cv, body.y + 70, "Create one from the Changes tab.", dim, false);
            let label = "Create PR";
            let bw = self.tx.ui_width(label, true) + 34;
            let b = Rect::new(body.x + (body.w - bw) / 2, body.y + 100, bw, 32);
            let hot = self.hv(b);
            cv.fill_round(b, 10, if hot { c.accent.shift(14) } else { c.accent });
            self.tx.icon(cv, "ui/git_branch", b.x + 10, b.y + 8, 16, c.on_accent, b);
            self.tx.draw_ui(cv, b.x + 32, b.y + 6, label, c.on_accent, b, true);
            gp.hits.push((b, Hit::PrOpen));
            return;
        };
        // Content height first, so the scroll can be clamped.
        let inner_w = body.w - 2 * 12;
        let mut layout: Vec<(i32, Block)> = Vec::new();
        let mut y = 8;
        layout.push((y, Block::Head));
        y += 40;
        if !pr.checks.is_empty() {
            layout.push((y, Block::Summary));
            y += 34;
            for k in 0..pr.checks.len() {
                layout.push((y, Block::Check(k)));
                y += 30;
            }
            y += 8;
        }
        layout.push((y, Block::CommentsHead));
        y += 40;
        for (k, cm) in pr.comments.iter().enumerate() {
            let h = self.gp_comment_height(cm, inner_w - 24);
            layout.push((y, Block::Comment(k, h)));
            y += h + 10;
        }
        if pr.comments.is_empty() {
            layout.push((y, Block::NoComments));
            y += 30;
        }
        let total = y + 16;
        gp.max_scroll_pr = (total - body.h).max(0) as f64;
        gp.scroll_pr = gp.scroll_pr.clamp(0.0, gp.max_scroll_pr);
        let top = body.y - gp.scroll_pr as i32;
        for (ry, b) in layout {
            let y = top + ry;
            match b {
                Block::Head => self.gp_pr_head(cv, gp, &pr, y, body),
                Block::Summary => self.gp_pr_summary(cv, &pr, y, body),
                Block::Check(k) => self.gp_pr_check(cv, &pr.checks[k], y, body),
                Block::CommentsHead => self.gp_pr_comments_head(cv, gp, pr.comments.len(), y, body),
                Block::Comment(k, h) => {
                    if y + h >= body.y && y <= body.bottom() {
                        self.gp_pr_comment(cv, &pr.comments[k], y, h, body);
                    }
                }
                Block::NoComments => {
                    self.tx.draw_ui(cv, body.x + 16, y + 4, "No comments yet", dim, body, false);
                }
            }
        }
        let (pos, visible) = (gp.scroll_pr, body.h as f64);
        self.region_bar(cv, crate::scrollreg::Reg::GitChanges, body, total as f64, visible, pos, 1.0, false);
    }

    fn gp_pr_head(&mut self, cv: &mut Canvas, gp: &mut GitPanel, pr: &PrInfo, y: i32, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        self.tx.icon(cv, "ui/git_branch", body.x + 14, y + 8, 16, c.git_added, body);
        let num = format!("#{}", pr.number);
        self.tx.draw_ui(cv, body.x + 36, y + 7, &num, strong, body, true);
        let nw = self.tx.ui_width(&num, true);
        let (label, tone) = match (pr.state.as_str(), pr.draft) {
            ("MERGED", _) => ("MERGED", Rgba(0xa3, 0x71, 0xf7, 0xff)),
            ("CLOSED", _) => ("CLOSED", c.error),
            (_, true) => ("DRAFT", dim),
            _ => ("OPEN", c.git_added),
        };
        let lw = self.tx.ui_width_sized(label, true, 11.0);
        let badge = Rect::new(body.x + 36 + nw + 10, y + 7, lw + 16, 20);
        cv.fill_round(badge, 6, tone.with_alpha(0x22));
        cv.ring(badge, 6, tone.with_alpha(0x70));
        self.tx.draw_ui_sized(cv, badge.x + 8, badge.y + 3, label, tone, badge, true, 11.0);
        // Click: open it on GitHub.
        let hit = Rect::new(body.x + 8, y, body.w - 16, 34);
        gp.hits.push((hit, Hit::PrBase));
        let t = fit(&mut self.tx, &pr.title, body.w - 28, false);
        self.tx.draw_ui_sized(cv, body.x + 16, y + 28 - 2, &t, dim, body, false, 12.0);
        let _ = c;
    }

    fn gp_pr_summary(&mut self, cv: &mut Canvas, pr: &PrInfo, y: i32, body: Rect) {
        let c = self.theme.chrome;
        let (pass, fail, pend) = (pr.passing(), pr.failing(), pr.pending());
        let (text, col, icon) = if fail > 0 {
            (format!("{fail} failing"), c.error, "ui/x_circle")
        } else if pend > 0 {
            (format!("{pend} pending"), c.warning, "ui/circle")
        } else {
            (format!("{pass} passing"), c.git_added, "ui/check")
        };
        self.gp_status_dot(cv, body.x + 14, y + 8, col, icon, body);
        self.tx.draw_ui(cv, body.x + 38, y + 7, &text, col, body, false);
        cv.fill(Rect::new(body.x + 12, y + 30, body.w - 24, 1), c.border.with_alpha(0x80));
    }

    /// A small ring with a mark inside: a check's state.
    fn gp_status_dot(&mut self, cv: &mut Canvas, x: i32, y: i32, col: Rgba, icon: &str, clip: Rect) {
        cv.ring(Rect::new(x, y, 18, 18), 9, col.with_alpha(0xc0));
        if icon != "ui/circle" {
            self.tx.icon(cv, icon, x + 3, y + 3, 12, col, clip);
        } else {
            cv.fill_round(Rect::new(x + 6, y + 6, 6, 6), 3, col);
        }
    }

    fn gp_pr_check(&mut self, cv: &mut Canvas, ch: &Check, y: i32, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (col, icon) = match ch.state {
            RunState::Pass => (c.git_added, "ui/check"),
            RunState::Fail => (c.error, "ui/x_circle"),
            RunState::Pending => (c.warning, "ui/circle"),
            RunState::Skipped => (dim, "ui/dash"),
        };
        let r = Rect::new(body.x + 6, y, body.w - 12, 30);
        if self.hv(r) {
            cv.fill_round_clip(r, 8, strong.with_alpha(0x0c), body);
        }
        self.gp_status_dot(cv, body.x + 14, y + 6, col, icon, body);
        let dur = if ch.secs > 0 { duration(ch.secs) } else { String::new() };
        let dw = self.tx.ui_width_sized(&dur, false, 12.0);
        let name = fit(&mut self.tx, &ch.name, r.w - 40 - dw - 16, false);
        self.tx.draw_ui(cv, body.x + 38, y + 6, &name, strong, body, false);
        self.tx.draw_ui_sized(cv, r.right() - 8 - dw, y + 8, &dur, dim, body, false, 12.0);
    }

    fn gp_pr_comments_head(&mut self, cv: &mut Canvas, gp: &mut GitPanel, n: usize, y: i32, body: Rect) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        cv.fill(Rect::new(body.x + 12, y + 2, body.w - 24, 1), c.border.with_alpha(0x80));
        self.tx.icon(cv, "ui/chat", body.x + 14, y + 14, 16, dim, body);
        self.tx.draw_ui_sized(cv, body.x + 38, y + 13, "COMMENTS", dim, body, true, 12.0);
        let tw = self.tx.ui_width_sized("COMMENTS", true, 12.0);
        let cnt = n.to_string();
        let nw = self.tx.ui_width_sized(&cnt, true, 12.0);
        let pill = Rect::new(body.x + 38 + tw + 8, y + 11, (nw + 12).max(20), 18);
        cv.fill_round(pill, 9, c.tab_active_fg.with_alpha(0x1c));
        self.tx.draw_ui_sized(cv, pill.x + (pill.w - nw) / 2, pill.y + 2, &cnt, dim, pill, true, 12.0);
        // The sparkle hands the comments to the agent.
        if n > 0 {
            let b = Rect::new(body.right() - 12 - 28, y + 7, 28, 28);
            self.gp_icon_btn(cv, gp, b, "ui/sparkle", Hit::PrFix);
        }
    }

    /// Wrapped lines of a comment's body at width `w`.
    fn gp_comment_lines(&mut self, text: &str, w: i32) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for para in clean_markdown(text).split('\n') {
            if para.trim().is_empty() {
                if !out.is_empty() && out.last().is_some_and(|l| !l.is_empty()) {
                    out.push(String::new());
                }
                continue;
            }
            out.extend(wrap_lines(&mut self.tx, para, w, COMMENT_LINES));
        }
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }

    fn gp_comment_height(&mut self, cm: &Comment, w: i32) -> i32 {
        let mut h = 14 + 22 + 8;
        h += self.gp_comment_lines(&cm.body, w).len() as i32 * 18 + 12;
        for r in &cm.replies {
            h += 12 + 22 + 6 + self.gp_comment_lines(&r.body, w - 36).len() as i32 * 18 + 6;
        }
        h
    }

    /// A comment card: author line (avatar, name, age, BOT, file:line), the body, then the replies.
    fn gp_pr_comment(&mut self, cv: &mut Canvas, cm: &Comment, y: i32, h: i32, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let card = Rect::new(body.x + 12, y, body.w - 24, h);
        cv.card(card, 12, c.widget_bg.with_alpha(0xa0), c.border);
        let mut cy = y + 12;
        self.gp_comment_author(cv, cm, card.x + 12, cy, card, 28);
        cy += 30;
        let lines = self.gp_comment_lines(&cm.body, card.w - 24);
        let mut first = true;
        for l in &lines {
            let (col, bold) = if first && !l.is_empty() { (strong, true) } else { (strong.with_alpha(0xd8), false) };
            self.tx.draw_ui(cv, card.x + 12, cy, l, col, card, bold);
            first = false;
            cy += 18;
        }
        cy += 10;
        for r in &cm.replies {
            cv.fill(Rect::new(card.x + 1, cy - 4, card.w - 2, 1), c.border.with_alpha(0x90));
            cy += 8;
            // A reply is in the same place as its thread: no file:line again.
            let reply = Comment { place: String::new(), replies: Vec::new(), ..r.clone() };
            self.gp_comment_author(cv, &reply, card.x + 12, cy, card, 22);
            cy += 28;
            for l in self.gp_comment_lines(&r.body, card.w - 24 - 36) {
                self.tx.draw_ui(cv, card.x + 12 + 30, cy, &l, strong.with_alpha(0xd0), card, false);
                cy += 18;
            }
            cy += 6;
        }
        let _ = dim;
    }

    fn gp_comment_author(&mut self, cv: &mut Canvas, cm: &Comment, x: i32, y: i32, clip: Rect, av: i32) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let disc = Rect::new(x, y, av, av);
        let tint = if cm.bot { c.warning } else { c.accent };
        cv.fill_round(disc, av / 2, tint.with_alpha(0x30));
        if cm.bot {
            self.tx.icon(cv, "ui/sparkle", x + (av - 14) / 2, y + (av - 14) / 2, 14, tint, clip);
        } else {
            let initial: String = cm.author.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
            let iw = self.tx.ui_width_sized(&initial, true, 12.0);
            self.tx.draw_ui_sized(cv, x + (av - iw) / 2, y + (av - 15) / 2, &initial, tint, disc, true, 12.0);
        }
        let mut tx0 = x + av + 8;
        self.tx.draw_ui(cv, tx0, y + (av - 18) / 2, &cm.author, strong, clip, true);
        tx0 += self.tx.ui_width(&cm.author, true) + 8;
        let age = ago(clock_secs().saturating_sub(cm.at));
        self.tx.draw_ui_sized(cv, tx0, y + (av - 15) / 2, &age, dim, clip, false, 12.0);
        tx0 += self.tx.ui_width_sized(&age, false, 12.0) + 8;
        if cm.bot {
            let bw = self.tx.ui_width_sized("BOT", true, 10.0) + 12;
            let b = Rect::new(tx0, y + (av - 18) / 2, bw, 18);
            cv.ring(b, 5, dim.with_alpha(0x80));
            self.tx.draw_ui_sized(cv, b.x + 6, b.y + 3, "BOT", dim, b, true, 10.0);
            tx0 += bw + 8;
        }
        if !cm.place.is_empty() {
            let room = clip.right() - 12 - tx0;
            if room > 30 {
                let p = fit(&mut self.tx, &cm.place, room, false);
                self.tx.draw_ui_sized(cv, tx0, y + (av - 15) / 2, &p, dim.with_alpha(0xc0), clip, false, 12.0);
            }
        }
    }
}

enum Block {
    Head,
    Summary,
    Check(usize),
    CommentsHead,
    /// Comment `k` and its height.
    Comment(usize, i32),
    NoComments,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_loses_its_markers_but_keeps_the_paragraphs() {
        let md = "<!-- internal -->\n**Potential issue**\n\n## Fix\nUse `setCountry` here.\n<details>";
        assert_eq!(clean_markdown(md), "Potential issue\n\nFix\nUse setCountry here.");
    }
}
