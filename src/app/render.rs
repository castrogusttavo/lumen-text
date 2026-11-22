use super::*;

impl App {
    // ── layout & rendering ──

    /// Re-derive the UI scale (window scale factor, `LUMEN_SCALE` / `--scale`, or the `ui_scale`
    /// setting) and re-measure the fonts when it changed.
    pub fn apply_scale(&mut self) {
        let s = self.dpi.effective(self.config.ui_scale);
        if (s - crate::ui::scale()).abs() > 1e-4 {
            self.tx.set_scale(s);
            for d in &mut self.docs {
                d.reveal = true;
            }
        }
    }

    pub(crate) fn compute_layout(&mut self) {
        self.shell_sync();
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        // Zen mode and the View toggles can hide the top bar, tab bar and breadcrumbs.
        let (top_h, tab_h, bread_h) = self.chrome_heights();
        self.layout.topbar = Rect::new(0, 0, w, top_h);
        let lx = self.rail_w();
        self.layout.rail = Rect::new(0, top_h, lx, h - top_h);
        let sidebar_w = self.sidebar_w();
        let gap = self.shell_gap();
        // The panel animates its own height (open, close, maximize); the shell only places it.
        let avail = (h - gap - top_h - tab_h - bread_h).max(0);
        let panel_h = self.panel_slide_h(avail);
        let sh = &self.layout.shell;
        let plan = crate::app_shell::Plan {
            win: (w, h),
            top: top_h,
            rail: lx,
            side_w: sidebar_w,
            dock_w: self.slot_rect().w,
            tab_h,
            bread_h,
            panel_h,
            gap,
            side_t: sh.side.value,
            dock_t: sh.dock.value,
        };
        let s = crate::app_shell::solve(&plan);
        self.layout.sidebar = if self.sidebar_open { s.sidebar } else { Rect::new(lx, top_h, 0, s.sidebar.h) };
        self.layout.card_outer = s.outer;
        self.layout.tabbar = s.tabbar;
        self.layout.bread = s.bread;
        self.layout.card = self.zen_card(s.card);
        self.layout.editor = self.layout.card;
        self.layout.panel = s.panel;
        // No bottom bar: a zero-height edge keeps what is anchored above it (cards, panels) in place.
        self.layout.status = Rect::new(0, h, w, 0);
        self.split_layout();
    }

    /// Draw one frame into any canvas (used by the headless script runner).
    pub fn render_into(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        self.compute_layout();
        self.draw_all(cv, dt)
    }

    pub(super) fn render(&mut self) {
        let Some(mut g) = self.gfx.take() else { return };
        let size = g.window.inner_size();
        self.size = crate::ui::logical_size(size.width, size.height);
        let (Some(w), Some(h)) = (std::num::NonZeroU32::new(size.width), std::num::NonZeroU32::new(size.height)) else {
            self.gfx = Some(g);
            return;
        };
        let since = self.last_frame.elapsed().as_secs_f64();
        let dt = if since > 0.1 { 0.016 } else { since.min(0.05) };
        let t0 = Instant::now();
        // Shell motion (dock / sidebar slide, hover tints): live after the first frame.
        self.layout.shell.live = self.first_frame_done;
        self.shell_sync();
        let shell_busy = self.layout.shell.step(dt);
        self.compute_layout();

        g.surface.resize(w, h).ok();
        let mut busy = shell_busy;
        {
            let Ok(mut buffer) = g.surface.buffer_mut() else {
                self.gfx = Some(g);
                return;
            };
            let mut cv = Canvas { px: &mut buffer, w: size.width as i32, h: size.height as i32 };
            busy |= self.draw_all(&mut cv, dt);
            let _ = buffer.present();
        }
        self.last_frame = Instant::now();
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.update_title(&g.window, ms);
        self.gfx = Some(g);
        if !self.first_frame_done {
            self.first_frame_done = true;
            crate::startup::first_frame();
            self.post_frame_init();
            if crate::startup::profiling() {
                self.exit_requested = true;
            }
        }
        if busy {
            self.request_redraw();
        }
    }

    pub(super) fn draw_all(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        self.refresh_find();
        cv.clear(crate::app_shell::canvas_color(&self.theme));
        self.draw_card_shadow(cv);
        crate::perf::lap!("clear");
        self.regions.clear();
        let mut busy = self.input_frame(dt);
        busy |= self.modal_frame(dt);
        busy |= self.toast_frame(dt);

        // Editor card: the document draws inside, the border ring goes on top so the
        // rounded corners stay intact.
        let editor = self.layout.editor;
        // Zen mode hides the minimaps for this frame only (each document keeps its own switch).
        let minimaps = self.zen_hide_minimaps();
        busy |= self.draw_split_other(cv, dt);
        crate::perf::lap!("split_other");
        // Inline suggestion: follow the latest keystroke before the document draws its ghost text.
        self.inline_sync();
        // The find bar's matches are borrowed for the draw, not copied every frame.
        let (find_ranges, find_cur): (&[(usize, usize)], Option<usize>) = match &self.find {
            Some(f) => (f.matches.as_slice(), f.current),
            None => (&[], None),
        };
        if self.docs.get(self.active).is_some() {
            self.layout.welcome.clear();
        }
        if let Some(d) = self.docs.get_mut(self.active) {
            d.cell_w = self.tx.cell_w;
            busy |= d.draw(
                cv,
                &mut self.tx,
                &self.theme,
                editor,
                &DrawOpts { dt, find: find_ranges, find_cur, focused: true },
            );
        } else {
            self.draw_welcome(cv, editor);
        }

        busy |= self.draw_multibuf(cv, editor);
        self.draw_jump(cv);
        self.zen_restore_minimaps(minimaps);
        crate::perf::lap!("doc.caret_end");
        busy |= self.draw_md(cv);
        self.draw_merge(cv);
        crate::perf::lap!("md");
        // Hidden bars (zen mode, View toggles) draw nothing and keep no stale hit rects.
        self.layout.status_btns.clear();
        self.layout.status_info.clear();
        if self.layout.topbar.h > 0 {
            self.draw_topbar(cv);
        } else {
            self.layout.top_btns.clear();
        }
        crate::perf::lap!("topbar");
        if self.layout.tabbar.h > 0 {
            busy |= self.draw_tabs(cv, dt);
            self.draw_quick(cv);
        } else {
            self.layout.tabs.clear();
            self.layout.quick = Rect::default();
        }
        crate::perf::lap!("tabs");
        self.draw_breadcrumbs(cv);
        self.draw_diff(cv);
        busy |= self.draw_diag_chip(cv, dt);
        crate::perf::lap!("breadcrumbs_diff");
        if self.sidebar_open {
            self.draw_sidebar(cv);
        }
        // After the sidebar: while it slides in from under the rail, the rail stays on top.
        self.draw_rail(cv);
        crate::perf::lap!("sidebar");
        self.draw_panel(cv);
        busy |= self.draw_gitpanel(cv, dt);
        busy |= self.draw_aipanel(cv, dt);
        if self.sidebar_open {
            self.draw_sidebar_edge(cv);
        }
        self.draw_cards(cv);
        self.draw_tree_drag(cv);
        self.draw_tab_drag(cv);
        crate::perf::lap!("panels");
        // Re-aim the hover tint at what is under the pointer in this frame's layout: a closed or
        // switched tab, a deleted row or a toggled panel must not leave the old tint behind (a
        // "shadow") until the mouse moves again.
        self.hover_rect = self.hover_target();
        if let Some((r, rad)) = self.hover_rect {
            let f = self.theme.foreground;
            cv.fill_round(r, rad, f.with_alpha(if self.theme.background.luminance() < 0.5 { 0x16 } else { 0x12 }));
        }
        self.draw_find(cv);
        self.draw_completion(cv);
        self.draw_hover(cv);
        self.draw_update_card(cv);
        self.draw_overlay(cv);
        self.draw_onboarding(cv);
        self.draw_tooltip(cv);
        self.draw_toast(cv);
        self.draw_toasts(cv);
        crate::perf::lap!("overlays");
        busy | self.scroll_activity() | self.layout.shell.moving() | crate::termfx::take_moving()
    }

    /// The name a tab shows: the file name, with the parent folders in front when another open
    /// file has the same name (`workspace/route.ts` next to `project/route.ts`).
    pub(crate) fn tab_display_name(&self, doc: usize) -> String {
        let name = &self.docs[doc].name;
        // Only a name shared with another tab needs the work.
        if !self.docs.iter().enumerate().any(|(i, d)| i != doc && d.name == *name && d.path().is_some()) {
            return name.clone();
        }
        let items: Vec<(&str, Option<&Path>)> = self.docs.iter().map(|d| (d.name.as_str(), d.path())).collect();
        crate::tabnames::disambiguate(&items).swap_remove(doc)
    }

    /// A tab's name as drawn (cut with "…" past `TAB_NAME_MAX`) and its width.
    pub(crate) fn tab_label(&mut self, doc: usize, bold: bool) -> (String, i32) {
        let name = self.tab_display_name(doc);
        let full = self.tx.ui_width(&name, bold);
        if full <= TAB_NAME_MAX {
            return (name, full);
        }
        let label = crate::ui::fit_ellipsis(&mut self.tx, &name, TAB_NAME_MAX, bold);
        let w = self.tx.ui_width(&label, bold);
        (label, w)
    }

    pub(super) fn draw_sidebar(&mut self, cv: &mut Canvas) {
        self.draw_sidebar_ui(cv);
    }

    pub(crate) fn draw_sidebar_bar(&mut self, cv: &mut Canvas) {
        let r = self.layout.sidebar;
        let (total, visible) = self.sidebar_extent();
        let track = Rect::new(r.right() - 15, r.y + TAB_H, 14, r.h - TAB_H);
        self.sidebar_bar = crate::ui::VBar::new(track, total, visible, self.tree.scroll);
        if let Some(b) = self.sidebar_bar {
            b.draw(cv, self.theme.foreground, self.sidebar_bar_hot, self.drag_bar.is_some_and(|(k, _)| k == 1));
        }
        self.draw_sidebar_hbar(cv);
    }

    pub(super) fn draw_breadcrumbs(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.bread;
        self.layout.crumbs.clear();
        if r.h == 0 {
            return; // zen mode, or no file and breadcrumbs switched off
        }
        // Problems, cursor, indentation, language and encoding at the right end; the crumbs
        // (when switched on) get the room to their left.
        let edge = self.draw_bread_readouts(cv, r);
        if !self.config.breadcrumbs {
            return;
        }
        let Some(d) = self.docs.get(self.active) else { return };
        let r = Rect::new(r.x, r.y, (edge - 8 - r.x).max(0), r.h);
        let (dim, fg) = (c.tab_inactive_fg, c.tab_active_fg);
        let rel = d.path().map(|p| self.rel(p)).unwrap_or_else(|| d.name.clone());
        let (line, _) = d.buf.line_col(d.ed.head());
        let mut chain: Vec<(String, usize, usize)> =
            d.symbol_chain(line).iter().map(|s| (s.name.clone(), s.sel_l, s.sel_c)).collect();
        if chain.is_empty() && d.buf.line_count() > crate::bigfile::BIG_LINES {
            // Big file with no (or a trimmed) outline: derive the chain from indentation.
            chain = d.big.chain(&d.buf, line, d.version).into_iter().map(|s| (s.name, s.sel_l, s.sel_c)).collect();
        }
        let parts: Vec<String> = rel.split('/').map(str::to_string).collect();
        let y = r.y + (r.h - 18) / 2;
        let mut x = r.x + 16;
        let sep_w = self.tx.ui_width(">", false) + 12;
        for (i, p) in parts.iter().enumerate() {
            let w = self.tx.ui_width(p, false);
            let last = i + 1 == parts.len();
            self.tx.draw_ui(cv, x, y, p, if last && chain.is_empty() { fg } else { dim }, r, false);
            self.layout
                .crumbs
                .push((Rect::new(x - 3, r.y, w + 6, r.h), Crumb::Path(self.root.join(parts[..=i].join("/")))));
            x += w;
            if !last || !chain.is_empty() {
                self.tx.draw_ui(cv, x + 6, y, ">", dim.with_alpha(0x90), r, false);
                x += sep_w;
            }
            if x > r.right() - 40 {
                return;
            }
        }
        for (i, (name, l, col)) in chain.iter().enumerate() {
            let w = self.tx.ui_width(name, false);
            self.tx.draw_ui(cv, x, y, name, if i + 1 == chain.len() { fg } else { dim }, r, false);
            self.layout.crumbs.push((Rect::new(x - 3, r.y, w + 6, r.h), Crumb::Sym(*l, *col)));
            x += w;
            if i + 1 < chain.len() {
                self.tx.draw_ui(cv, x + 6, y, ">", dim.with_alpha(0x90), r, false);
                x += sep_w;
            }
            if x > r.right() - 40 {
                return;
            }
        }
    }

    pub(super) fn crumb_click(&mut self, x: f64, y: f64) -> bool {
        let Some((_, cr)) = self.layout.crumbs.iter().find(|(r, _)| r.contains(x, y)).cloned() else {
            return self.layout.bread.contains(x, y);
        };
        match cr {
            Crumb::Path(p) => {
                self.sidebar_open = true;
                self.sidebar_mode = SidebarMode::Files;
                self.tree.reveal(&p);
                self.focus = Focus::Sidebar;
            }
            Crumb::Sym(l, c) => {
                self.push_nav();
                if let Some(d) = self.docs.get_mut(self.active) {
                    let p = d.pos_from_lsp(l, c);
                    d.ed.click(&mut d.buf, p, false);
                    d.reveal = true;
                }
                self.focus = Focus::Editor;
            }
        }
        true
    }

    /// The memory readout of the bottom bar. The numbers come from the background sampler (see
    /// `mem::watch`): nothing here reads /proc unless no sample was published yet. The text is
    /// rebuilt when a new sample arrived, and every 2 s for what is not memory (crashed servers).
    pub fn memory_text(&mut self) -> String {
        let generation = mem::generation();
        if self.mem_cache.0.elapsed() > Duration::from_secs(2) || self.res.mem_generation != generation {
            let own = mem::own_pss_kb().unwrap_or(0) as f64 / 1024.0;
            let mut s = format!("RAM {own:.0} MB");
            s.push_str(&self.lspx_ram_suffix());
            if self.res.over {
                s.push_str(" · over budget");
            }
            self.mem_cache = (Instant::now(), s);
            self.res.mem_generation = generation;
        }
        self.mem_cache.1.clone()
    }

    pub(super) fn update_title(&mut self, window: &Window, _ms: f32) {
        // No frame time here: it changed the title (a compositor round trip) on almost every frame.
        let title = match self.docs.get(self.active) {
            Some(d) => format!(
                "{}{} — lumen-text",
                if d.buf.is_dirty() || d.mb.as_ref().is_some_and(|m| m.is_dirty()) { "● " } else { "" },
                d.name
            ),
            None => "lumen-text".to_string(),
        };
        if title != self.last_title {
            window.set_title(&title);
            self.last_title = title;
        }
    }
}
