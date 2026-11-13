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
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let sb = if self.sidebar_open { self.sidebar_w() } else { 0 };
        // Zen mode and the View toggles can hide the top bar, tab bar, breadcrumbs and status bar.
        let (top_h, tab_h, bread_h, status_h) = self.chrome_heights();
        self.layout.topbar = Rect::new(0, 0, w, top_h);
        self.layout.sidebar = Rect::new(0, top_h, sb, h - status_h - top_h);
        let rw = self.gitpanel_w();
        self.layout.tabbar = Rect::new(sb, top_h, w - sb - rw, tab_h);
        self.layout.bread = Rect::new(sb, top_h + tab_h, (w - sb - rw).max(0), bread_h);
        let avail = (h - top_h - tab_h - bread_h - status_h).max(0);
        let panel_h = if self.panel.open { self.panel.height.min(avail / 2).max(0) } else { 0 };
        let gap = if panel_h > 0 { CARD_M } else { 0 };
        let card_h = (avail - CARD_M - panel_h - gap).max(0);
        self.layout.card = Rect::new(sb + CARD_M, top_h + tab_h + bread_h, (w - sb - 2 * CARD_M - rw).max(0), card_h);
        self.layout.card = self.zen_card(self.layout.card);
        self.layout.editor = self.layout.card;
        self.layout.panel =
            Rect::new(sb + CARD_M, top_h + tab_h + bread_h + card_h + gap, (w - sb - 2 * CARD_M - rw).max(0), panel_h);
        self.layout.status = Rect::new(0, h - status_h, w, status_h);
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
        self.compute_layout();

        g.surface.resize(w, h).ok();
        let mut busy = false;
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
        let t = &self.theme;
        let c = t.chrome;
        cv.clear(t.background);
        crate::perf::lap!("clear");
        self.regions.clear();
        let mut busy = self.input_frame(dt);

        // Editor card: the document draws inside, the border ring goes on top so the
        // rounded corners stay intact.
        let editor = self.layout.editor;
        let find_ranges: Vec<(usize, usize)> = self.find.as_ref().map(|f| f.matches.clone()).unwrap_or_default();
        let find_cur = self.find.as_ref().and_then(|f| f.current);
        // Zen mode hides the minimaps for this frame only (each document keeps its own switch).
        let minimaps = self.zen_hide_minimaps();
        busy |= self.draw_split_other(cv, dt);
        crate::perf::lap!("split_other");
        // Inline suggestion: follow the latest keystroke before the document draws its ghost text.
        self.inline_sync();
        if let Some(d) = self.docs.get_mut(self.active) {
            d.cell_w = self.tx.cell_w;
            busy |= d.draw(
                cv,
                &mut self.tx,
                &self.theme,
                editor,
                &DrawOpts { dt, find: &find_ranges, find_cur, focused: true },
            );
        } else {
            cv.fill(editor, self.theme.background);
            let hint = [
                ("Ctrl+P", "open file"),
                ("Ctrl+Shift+P", "commands"),
                ("Ctrl+Shift+F", "search in project"),
                ("Ctrl+B", "show/hide the sidebar"),
            ];
            let x = editor.x + editor.w / 2 - 150;
            let mut y = editor.y + editor.h / 2 - 56;
            for (k, v) in hint {
                self.tx.draw_ui(cv, x, y, k, c.accent_text, editor, true);
                self.tx.draw_ui(cv, x + 120, y, v, c.tab_inactive_fg, editor, false);
                y += 28;
            }
        }

        self.draw_jump(cv);
        self.zen_restore_minimaps(minimaps);
        crate::perf::lap!("doc.caret_end");
        busy |= self.draw_md(cv);
        self.draw_merge(cv);
        crate::perf::lap!("md");
        // Hidden bars (zen mode, View toggles) draw nothing and keep no stale hit rects.
        if self.layout.topbar.h > 0 {
            self.draw_topbar(cv);
        } else {
            self.layout.top_btns.clear();
        }
        crate::perf::lap!("topbar");
        if self.layout.tabbar.h > 0 {
            self.draw_tabs(cv);
        } else {
            self.layout.tabs.clear();
        }
        crate::perf::lap!("tabs");
        self.draw_breadcrumbs(cv);
        self.draw_diff(cv);
        crate::perf::lap!("breadcrumbs_diff");
        if self.sidebar_open {
            self.draw_sidebar(cv);
        }
        crate::perf::lap!("sidebar");
        self.draw_panel(cv);
        self.draw_gitpanel(cv);
        self.draw_aipanel(cv);
        if self.sidebar_open {
            self.draw_sidebar_edge(cv);
        }
        self.draw_tree_drag(cv);
        self.draw_tab_drag(cv);
        crate::perf::lap!("panels");
        if self.layout.status.h > 0 {
            self.draw_status(cv);
        } else {
            self.layout.status_btns.clear();
            self.layout.status_info.clear();
        }
        crate::perf::lap!("status");
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
        self.draw_tooltip(cv);
        self.draw_toast(cv);
        crate::perf::lap!("overlays");
        busy | self.scroll_activity()
    }

    pub(super) fn draw_tabs(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let bar = self.layout.tabbar;
        cv.fill(bar, c.tabbar_bg);
        self.layout.tabs.clear();
        let pane_docs = self.split_pane_docs();
        // Geometry of every tab in strip coordinates, then scroll so the active one is visible.
        let order = crate::app_ctxmenu::tab_order(&self.docs);
        let mut geo: Vec<(usize, i32, i32)> = Vec::with_capacity(order.len());
        let mut x = 6;
        for &i in &order {
            let w = self.tx.ui_width(&self.docs[i].name, false).min(TAB_NAME_MAX)
                + 34
                + 34
                + 11 * pane_docs.iter().filter(|p| p.0 == i).count() as i32;
            geo.push((i, x, w));
            x += w;
        }
        let view = (bar.w - 12).max(1) as f64;
        self.tabs_max = ((x - 6) as f64 - view).max(0.0);
        self.inp.tabs.geo.clone_from(&geo);
        let dragging = self.tab_dragging();
        if self.tabs_seen != Some(self.active) {
            self.tabs_seen = Some(self.active);
            if let Some(&(_, tx0, tw)) = geo.iter().find(|(i, _, _)| *i == self.active) {
                let (l, r) = (tx0 as f64 - 6.0, (tx0 + tw) as f64 + 6.0);
                if l < self.tabs_scroll {
                    self.tabs_scroll = l;
                } else if r > self.tabs_scroll + view + 6.0 {
                    self.tabs_scroll = r - view - 6.0;
                }
            }
        }
        self.tabs_scroll = self.tabs_scroll.clamp(0.0, self.tabs_max);
        let off = self.tabs_scroll.round() as i32;
        let strip = Rect::new(bar.x + 2, bar.y, bar.w - 4, bar.h);
        for (i, tx0, w) in geo {
            let d = &self.docs[i];
            let r = Rect::new(bar.x + tx0 - off, bar.y, w, bar.h);
            if r.right() < strip.x || r.x > strip.right() {
                continue;
            }
            let active = i == self.active;
            let fg = if active { c.tab_active_fg } else { c.tab_inactive_fg };
            let ty = r.y + (bar.h - 18) / 2;
            let lead = 34;
            if active {
                cv.fill_round_clip(Rect::new(r.x + 2, r.y + 5, r.w - 4, bar.h - 10), 8, c.tab_active_bg, strip);
            }
            let fname = self.tx.icons.file_icon(&d.name);
            self.tx.icon(cv, &fname, r.x + 12, r.y + (bar.h - 16) / 2, 16, fg, strip);
            let (label, name_w) = self.tab_label(i, active);
            let d = &self.docs[i];
            self.tx.draw_ui(cv, r.x + lead, ty, &label, fg, strip, active);
            let close = Rect::new(r.right() - 28, r.y, 28, bar.h);
            let (mx, my) = self.mouse;
            let over_tab = strip.contains(mx, my) && r.contains(mx, my) && self.overlay.is_none() && !dragging;
            let over_close = over_tab && close.contains(mx, my);
            if over_tab {
                // Hovering a tab (focused or not) reveals its close button; it gets its own
                // background when the pointer is on it.
                let cb = Rect::new(close.x + 4, r.y + (bar.h - 20) / 2, 20, 20);
                if over_close {
                    cv.fill_round_clip(cb, 6, self.theme.foreground.with_alpha(0x38), strip);
                }
                let ic = if over_close { c.tab_active_fg } else { c.tab_inactive_fg };
                self.tx.icon(cv, "ui/close", close.x + 8, r.y + (bar.h - 12) / 2, 12, ic, strip);
            } else if d.buf.is_dirty() {
                // Like Zed: the unsaved dot takes the close button's place.
                cv.fill_round_clip(Rect::new(close.x + 10, r.y + bar.h / 2 - 4, 8, 8), 4, c.accent, strip);
            } else if d.pinned {
                self.tx.icon(cv, "ui/pin", close.x + 8, r.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, strip);
            } else if active {
                self.tx.icon(cv, "ui/close", close.x + 8, r.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, strip);
            }
            // Split editor: an underline and the pane number for every pane that shows this tab, in
            // the accent color for the focused pane and gray for the others.
            for (k, &(_, focused)) in pane_docs.iter().enumerate().filter(|(_, p)| p.0 == i) {
                let (col, label) = if focused {
                    (c.accent, c.accent_text)
                } else {
                    (c.tab_inactive_fg.with_alpha(0x80), c.tab_inactive_fg)
                };
                let seg = (r.w - 28) / pane_docs.iter().filter(|p| p.0 == i).count() as i32;
                let nth = pane_docs.iter().take(k).filter(|p| p.0 == i).count() as i32;
                cv.fill_clip(Rect::new(r.x + 14 + nth * seg, bar.bottom() - 5, (seg - 2).max(4), 2), col, strip);
                self.tx.draw_ui(
                    cv,
                    r.x + lead + name_w + 6 + nth * 11,
                    ty + 1,
                    &(k + 1).to_string(),
                    label,
                    strip,
                    focused,
                );
            }
            self.layout.tabs.push((r.intersect(&strip), close.intersect(&strip), i));
        }
        // Soft edge fades hint that more tabs hide beyond the strip.
        let bg = self.theme.chrome.tabbar_bg;
        if self.tabs_scroll > 0.5 {
            for k in 0..14 {
                cv.fill_clip(
                    Rect::new(strip.x + k, bar.y, 1, bar.h),
                    bg.with_alpha((0xd0 - k * 14).max(0) as u8),
                    strip,
                );
            }
        }
        if self.tabs_scroll < self.tabs_max - 0.5 {
            for k in 0..14 {
                cv.fill_clip(
                    Rect::new(strip.right() - 1 - k, bar.y, 1, bar.h),
                    bg.with_alpha((0xd0 - k * 14).max(0) as u8),
                    strip,
                );
            }
        }
    }

    /// A tab's name as drawn (cut with "…" past `TAB_NAME_MAX`) and its width.
    pub(crate) fn tab_label(&mut self, doc: usize, bold: bool) -> (String, i32) {
        let name = &self.docs[doc].name;
        let full = self.tx.ui_width(name, bold);
        if full <= TAB_NAME_MAX {
            return (name.clone(), full);
        }
        let label = crate::ui::fit_ellipsis(&mut self.tx, name, TAB_NAME_MAX, bold);
        let w = self.tx.ui_width(&label, bold);
        (label, w)
    }

    pub(super) fn draw_sidebar(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        cv.fill(r, c.sidebar_bg);
        cv.fill(Rect::new(r.right() - 1, r.y, 1, r.h), c.border);
        self.layout.sidebar_tabs.clear();
        self.sidebar_bar = None;
        // One quiet title: the structure outline moved to the command palette.
        self.tx.draw_ui(
            cv,
            r.x + 16,
            r.y + (TAB_H - 18) / 2 + 2,
            if self.sidebar_mode == SidebarMode::Outline { "Outline" } else { "Files" },
            c.tab_inactive_fg,
            r,
            true,
        );
        self.draw_explorer_refresh(cv);
        if self.sidebar_mode == SidebarMode::Outline {
            self.draw_outline(cv);
            return self.draw_sidebar_bar(cv);
        }
        let top = r.y + TAB_H;
        let body = Rect::new(r.x, top, r.w - 1, r.h - TAB_H);
        let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
        let off = -((self.tree.scroll as i32) % ROW_H);
        let hx = -(self.tree.hscroll as i32);
        let sidebar_focused = self.focus == Focus::Sidebar;
        let active_path = self.docs.get(self.active).and_then(|d| d.path()).map(Path::to_path_buf);
        let rows = ((body.h / ROW_H) + 2) as usize;
        for (k, row) in self.tree.rows.iter().enumerate().skip(first).take(rows) {
            let y = top + off + ((k - first) as i32) * ROW_H;
            let rr = Rect::new(r.x + 8, y + 1, r.w - 17, ROW_H - 2);
            let is_sel = self.tree.row_selected(k);
            let is_active = active_path.as_deref() == Some(row.path.as_path());
            if is_sel && sidebar_focused {
                cv.fill_clip(Rect::new(rr.x, rr.y, rr.w, rr.h), c.list_active_bg, body);
                let clip = body.intersect(&rr);
                cv.fill_round(clip, 7, c.list_active_bg);
            } else if is_active {
                let clip = body.intersect(&rr);
                cv.fill_round(clip, 7, c.list_hover_bg);
            }
            let indent = 18 + row.depth as i32 * 14;
            // Zed-style: a chevron for folders, a type icon for files (tinted, so it follows the theme).
            let cut = self.file_clip.is_cut(&row.path);
            let icon_color = if is_active { c.tab_active_fg } else { c.tab_inactive_fg };
            let dim = cut || row.ignored;
            let icon_color = if dim { icon_color.with_alpha(0x60).over(c.sidebar_bg) } else { icon_color };
            let iy = y + (ROW_H - 16) / 2;
            if row.is_dir {
                let chev = if row.expanded { "ui/chevron_down" } else { "ui/chevron_right" };
                self.tx.icon(cv, chev, r.x + hx + indent - 4, y + (ROW_H - 14) / 2, 14, icon_color, body);
                let folder = if row.expanded { "file/folder_open" } else { "file/folder" };
                self.tx.icon(cv, folder, r.x + hx + indent + 14, iy, 16, icon_color, body);
            } else {
                let name = self.tx.icons.file_icon(&row.name);
                self.tx.icon(cv, &name, r.x + hx + indent + 14, iy, 16, icon_color, body);
            }
            let badge = self.git_status.get(&row.path).copied();
            let fg = if is_active { c.tab_active_fg } else { c.sidebar_fg };
            let fg = if dim { fg.with_alpha(0x60).over(c.sidebar_bg) } else { fg };
            // A name that does not fit before the git dot ends in "…"; scrolling right reveals it.
            let text_x = r.x + hx + indent + 38;
            // Line coverage of the file, when a report is loaded, just before the git dot.
            let cov = if row.is_dir { None } else { self.coverage_pct_of(&row.path) };
            let cov_label = cov.map(|p| format!("{p:.0}%"));
            let cov_w = cov_label.as_deref().map_or(0, |l| self.tx.ui_width_sized(l, false, 11.0) + 6);
            let room = crate::app_sidebar::name_room(r, text_x, false) - cov_w;
            let name = crate::ui::fit_ellipsis(&mut self.tx, &row.name, room, false);
            self.tx.draw_ui(cv, text_x, y + (ROW_H - 18) / 2, &name, fg, body, false);
            if let (Some(p), Some(l)) = (cov, cov_label.as_deref()) {
                let col = self.coverage_color(p).with_alpha(0xc0).over(c.sidebar_bg);
                self.tx.draw_ui_sized(cv, r.right() - 30 - cov_w, y + (ROW_H - 15) / 2, l, col, body, false, 11.0);
            }
            if let Some(b) = badge {
                // A quiet dot instead of a colored letter: modified / new / deleted.
                let dot = match b {
                    'D' => c.git_deleted,
                    'M' | 'R' => c.git_modified,
                    _ => c.git_added.with_alpha(0xa0),
                };
                cv.fill_round(body.intersect(&Rect::new(r.right() - 24, y + ROW_H / 2 - 3, 6, 6)), 3, dot);
            }
        }
        self.draw_sidebar_bar(cv);
    }

    pub(super) fn draw_sidebar_bar(&mut self, cv: &mut Canvas) {
        let r = self.layout.sidebar;
        let (total, visible) = self.sidebar_extent();
        let track = Rect::new(r.right() - 15, r.y + TAB_H, 14, r.h - TAB_H);
        self.sidebar_bar = crate::ui::VBar::new(track, total, visible, self.tree.scroll);
        if let Some(b) = self.sidebar_bar {
            b.draw(cv, self.theme.foreground, self.sidebar_bar_hot, self.drag_bar.is_some_and(|(k, _)| k == 1));
        }
        self.draw_sidebar_hbar(cv);
    }

    pub(super) fn draw_outline(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        let top = r.y + TAB_H;
        let body = Rect::new(r.x, top, r.w - 1, r.h - TAB_H);
        let Some(d) = self.docs.get(self.active) else { return };
        if d.symbols.is_empty() {
            self.tx.draw_ui(
                cv,
                r.x + 18,
                top + 10,
                "No outline (needs the TS/JS server)",
                c.tab_inactive_fg,
                body,
                false,
            );
            return;
        }
        let (cur_line, _) = d.buf.line_col(d.ed.head());
        let chain: Vec<usize> = d.symbol_chain(cur_line).iter().map(|s| s.sel_l).collect();
        let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
        let off = -((self.tree.scroll as i32) % ROW_H);
        let hx = -(self.tree.hscroll as i32);
        let rows = (body.h / ROW_H + 2) as usize;
        let syms: Vec<crate::doc::Sym> = d.symbols.iter().skip(first).take(rows).cloned().collect();
        for (k, s) in syms.iter().enumerate() {
            let y = top + off + k as i32 * ROW_H;
            let rr = Rect::new(r.x + 8, y + 1, r.w - 17, ROW_H - 2);
            if chain.contains(&s.sel_l) {
                cv.fill_round(body.intersect(&rr), 7, c.list_hover_bg);
            }
            let x = r.x + hx + 18 + s.depth as i32 * 14;
            let letter = match s.kind {
                5 => "C",
                6 => "m",
                7 => "p",
                8 => "f",
                9 => "c",
                10 => "E",
                11 => "I",
                12 => "ƒ",
                13 => "v",
                14 => "k",
                2 => "M",
                22 => "e",
                23 => "S",
                26 => "T",
                _ => "·",
            };
            self.tx.draw_ui(cv, x, y + (ROW_H - 18) / 2, letter, c.accent_text, body, true);
            let room = crate::app_sidebar::name_room(r, x + 18, true);
            let name = crate::ui::fit_ellipsis(&mut self.tx, &s.name, room, false);
            self.tx.draw_ui(cv, x + 18, y + (ROW_H - 18) / 2, &name, c.sidebar_fg, body, false);
        }
    }

    pub(super) fn draw_breadcrumbs(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.bread;
        self.layout.crumbs.clear();
        if r.h == 0 {
            return; // breadcrumbs switched off
        }
        let Some(d) = self.docs.get(self.active) else { return };
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

    pub fn memory_text(&mut self) -> String {
        if self.mem_cache.0.elapsed() > Duration::from_secs(2) {
            let own = mem::snapshot().map_or(0, |m| m.pss_kb) as f64 / 1024.0;
            let mut s = format!("RAM {own:.0} MB");
            s.push_str(&self.lspx_ram_suffix());
            self.mem_cache = (Instant::now(), s);
        }
        self.mem_cache.1.clone()
    }

    pub(super) fn update_title(&mut self, window: &Window, ms: f32) {
        let title = match self.docs.get(self.active) {
            Some(d) => format!("{}{} — lumen-text — {:.1} ms", if d.buf.is_dirty() { "● " } else { "" }, d.name, ms),
            None => "lumen-text".to_string(),
        };
        if title != self.last_title {
            window.set_title(&title);
            self.last_title = title;
        }
    }
}
