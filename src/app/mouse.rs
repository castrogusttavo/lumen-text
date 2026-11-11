use super::*;

impl App {
    // ── mouse ──

    pub(super) fn over_editor(&self) -> bool {
        self.layout.editor.contains(self.mouse.0, self.mouse.1)
    }

    pub fn mouse_down(&mut self, button: MouseButton) {
        let (x, y) = self.mouse;
        // The shortcuts page recording a combo takes Ctrl/Alt+click and the middle button.
        if button != MouseButton::Right
            && matches!(self.overlay, Overlay::Settings(_))
            && self.settings_record_mouse(if button == MouseButton::Middle { "middle" } else { "click" })
        {
            return self.request_redraw();
        }
        if self.hover_mouse_down(button) {
            return self.request_redraw();
        }
        self.hover = None;
        self.dismiss_tooltip();
        self.chord = None;
        self.jump = None; // a click gives up a pending "Jump to…"
        self.tests.focused = false; // a click in the Tests panel gives it back
        self.build.focused = false; // likewise for the Build panel
        self.aipanel_mouse_pre(); // and for the AI panel
        if button == MouseButton::Right {
            return self.right_click();
        }
        if self.regions_press(x, y) {
            return self.request_redraw();
        }
        if self.overlay_mouse(x, y) {
            return self.request_redraw();
        }
        if self.update_card_mouse(x, y) {
            return self.request_redraw();
        }
        if self.aipanel_mouse(x, y, if button == MouseButton::Middle { 1 } else { 0 }) {
            return self.request_redraw();
        }
        if self.gitpanel_mouse(x, y) {
            return self.request_redraw();
        }
        // The sidebar's right edge (resize) comes before its scrollbar, whose track overlaps it.
        if button == MouseButton::Left && self.sidebar_edge_press(x, y) {
            return self.request_redraw();
        }
        if self.topbar_click(x, y) || self.fswatch_click(x, y) || self.status_click(x, y) {
            return self.request_redraw();
        }
        if self.completion_click(x, y) {
            return self.request_redraw();
        }
        if self.layout.tabbar.contains(x, y) {
            let hit = self.layout.tabs.iter().find(|(r, _, _)| r.contains(x, y)).cloned();
            if let Some((_, close, i)) = hit {
                if button == MouseButton::Middle || close.contains(x, y) {
                    self.tab_close_click(i, button);
                } else {
                    self.activate(i);
                    if button == MouseButton::Left && self.overlay.is_none() {
                        self.tab_drag_press(i, x, y);
                    }
                }
            }
            return self.request_redraw();
        }
        if self.crumb_click(x, y) {
            return self.request_redraw();
        }
        if self.md_mouse_down(x, y, button) {
            return self.request_redraw();
        }
        if let Some(b) = self.sidebar_bar
            && self.sidebar_open
            && b.track.contains(x, y)
        {
            let grab = if b.thumb.contains(x, y) { y - b.thumb.y as f64 } else { b.thumb.h as f64 / 2.0 };
            self.drag_bar = Some((1, grab));
            self.sidebar_scroll_to(y, grab);
            return self.request_redraw();
        }
        if self.sidebar_open && self.layout.sidebar.contains(x, y) {
            self.focus = Focus::Sidebar;
            if y < (self.layout.sidebar.y + TAB_H) as f64 {
                if let Some((_, m)) = self.layout.sidebar_tabs.iter().find(|(r, _)| r.contains(x, y)).copied() {
                    self.sidebar_mode = m;
                    self.tree.scroll = 0.0;
                }
                return self.request_redraw();
            }
            let rel = y - (self.layout.sidebar.y + TAB_H) as f64 + self.tree.scroll;
            if self.sidebar_mode == SidebarMode::Outline {
                if rel >= 0.0 {
                    let i = (rel / ROW_H as f64) as usize;
                    if let Some(s) = self.docs.get(self.active).and_then(|d| d.symbols.get(i)).cloned() {
                        self.push_nav();
                        let d = &mut self.docs[self.active];
                        let p = d.pos_from_lsp(s.sel_l, s.sel_c);
                        d.ed.click(&mut d.buf, p, false);
                        d.reveal = true;
                        self.focus = Focus::Editor;
                    }
                }
                return self.request_redraw();
            }
            if rel >= 0.0 {
                let i = (rel / ROW_H as f64) as usize;
                if i < self.tree.rows.len() {
                    self.tree_press(i, x, y);
                }
            }
            return self.request_redraw();
        }
        if self.panel_mouse(x, y) {
            return self.request_redraw();
        }
        if self.merge_mouse(x, y) {
            return self.request_redraw();
        }
        if self.split_mouse_down(x, y) {
            return self.request_redraw();
        }
        if self.diff.is_some() && self.diff_area().contains(x, y) {
            // The diff's keys (n / p, Accept / Reject) need the keyboard off the terminal.
            self.focus = Focus::Editor;
            if button == MouseButton::Left {
                self.diff_press(x, y); // starts a line selection (line staging)
            }
            return self.request_redraw();
        }
        if self.conflict_click(x, y) {
            return self.request_redraw();
        }
        if !self.over_editor() {
            return;
        }
        self.focus = Focus::Editor;
        self.completion = None;
        if self.docs.is_empty() {
            return;
        }
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if self.img_mouse_down(x, y) {
            return self.request_redraw();
        }
        if button == MouseButton::Middle {
            // A user binding for the middle button runs instead; by default a middle drag is a column
            // selection and a click pastes the primary selection on release.
            let bound = self.keymap.lookup_mouse(ctrl, shift, alt, "middle").map(str::to_string);
            match bound.as_deref() {
                Some("PastePrimary") | None => self.drag_press_middle(),
                Some("none") => {}
                Some(id) => {
                    let d = &mut self.docs[self.active];
                    let p = d.pos_at(x, y);
                    d.ed.click(&mut d.buf, p, false);
                    self.run_mouse_action(id);
                }
            }
            return self.request_redraw();
        }
        let near = (x - self.last_click_pos.0).abs() < 5.0 && (y - self.last_click_pos.1).abs() < 5.0;
        self.clicks =
            if near && self.last_click.elapsed() < Duration::from_millis(400) { self.clicks % 3 + 1 } else { 1 };
        self.last_click = Instant::now();
        self.last_click_pos = (x, y);
        if self.hbar_press(x, y) {
            return self.request_redraw();
        }
        if let Some(b) = self.docs[self.active].sb
            && b.track.contains(x, y)
        {
            let grab = if b.thumb.contains(x, y) { y - b.thumb.y as f64 } else { b.thumb.h as f64 / 2.0 };
            self.drag_bar = Some((0, grab));
            self.docs[self.active].sb_drag = true;
            self.docs[self.active].scrollbar_drag(y, grab);
            return self.request_redraw();
        }
        if let Some(l) = self.docs[self.active].minimap_layout.as_ref()
            && l.rect.contains(x, y)
        {
            self.dragging_minimap = true;
            self.docs[self.active].minimap_scroll_to(y);
            return self.request_redraw();
        }
        // Code lens text: jump to the symbol and list its references / implementations.
        if let Some(idx) = self.docs[self.active].lens_hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, i)| *i) {
            let lens = self.docs[self.active].lenses[idx].clone();
            let d = &mut self.docs[self.active];
            let p = d.pos_from_lsp(lens.line, lens.col);
            d.ed.click(&mut d.buf, p, false);
            if lens.title.as_deref().is_some_and(|t| t.contains("implementation")) {
                self.goto_definition(crate::lsp::DefKind::Implementation);
            } else {
                self.find_references();
            }
            return self.request_redraw();
        }
        let clicks = self.clicks;
        // Modifier + single click: what keybindings.json binds to it (`alt+click` adds a cursor,
        // `ctrl+click` goes to the definition by default; see `keymap::MOUSE_DEFAULTS`).
        let bound = if clicks == 1 && (ctrl || alt) {
            self.keymap.lookup_mouse(ctrl, shift, alt, "click").filter(|id| *id != "none").map(str::to_string)
        } else {
            None
        };
        let d = &mut self.docs[self.active];
        if clicks == 1 && !alt && d.click_fold(x, y) {
            return self.request_redraw();
        }
        if clicks == 1 && !alt && self.gutter_breakpoint_click(x, y) {
            return self.request_redraw();
        }
        let d = &mut self.docs[self.active];
        let (p, hint) = d.hit_at(x, y);
        if bound.as_deref() == Some("AddCursor") {
            if shift {
                self.drag_press_box(p);
            } else {
                self.drag_press_block(p);
            }
            return self.request_redraw();
        }
        d.ed.clear_extra();
        if x < (d.rect.x + d.gutter_w()) as f64 && clicks == 1 {
            d.reveal = true;
            self.drag_press(p, 3, false);
            return self.request_redraw();
        }
        // Inlay hints: Ctrl+click on a label part with a location goes there; a double click on a
        // type hint inserts the annotation (one undo step). A plain click only places the cursor.
        if let Some(hit) = hint {
            if bound.as_deref() == Some("GotoDefinition") {
                d.ed.click(&mut d.buf, p, false);
                if let Some(l) = d.hint_link(hit) {
                    self.goto_location(&(l.uri, l.line, l.character));
                }
                return self.request_redraw();
            }
            if clicks == 2 && !ctrl && !shift && d.apply_hint_edits(hit) {
                d.reveal = true;
                return self.request_redraw();
            }
        }
        if let Some(id) = bound {
            // The caret goes to the click, then the bound command runs there.
            d.ed.click(&mut d.buf, p, false);
            self.run_mouse_action(&id);
            return self.request_redraw();
        }
        d.reveal = true;
        self.drag_press(p, clicks, shift);
        self.request_redraw();
    }

    /// Conventional pointer: arrow on chrome, hand on clickable things, I-beam over text,
    /// resize arrows on the split divider.
    pub(crate) fn cursor_for(&self) -> CursorIcon {
        let (x, y) = self.mouse;
        let l = &self.layout;
        if self.dragging_minimap {
            return CursorIcon::Pointer;
        }
        if self.dragging {
            return CursorIcon::Text;
        }
        // The update / about card floats above everything: arrow on it, hand on its buttons and link.
        if self.upd.card.is_some() && self.upd.card_rect.contains(x, y) {
            return if self.upd.rects.iter().any(|(r, _)| r.contains(x, y)) {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if self.over_region_bar(x, y) {
            return CursorIcon::Default;
        }
        if let Overlay::TopMenu(m) = &self.overlay {
            return if m.rows.iter().any(|(r, _)| r.contains(x, y)) {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if !self.overlay.is_none() {
            return if l.overlay_rows.iter().any(|(r, _)| r.contains(x, y)) {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if let Some(c) = self.hover_cursor(x, y) {
            return c;
        }
        if let Some(c) = self.aipanel_cursor(x, y) {
            return c;
        }
        if let Some(c) = self.gitpanel_cursor(x, y) {
            return c;
        }
        if self.tab_dragging() {
            return CursorIcon::Grabbing;
        }
        if self.sidebar_resizing() || self.sidebar_on_edge(x, y) {
            return CursorIcon::ColResize;
        }
        if self.drag_bar.is_some()
            || self.over_hbar(x, y)
            || self.sidebar_bar.is_some_and(|b| b.track.contains(x, y))
            || self.docs.get(self.active).and_then(|d| d.sb).is_some_and(|b| b.track.contains(x, y))
        {
            return CursorIcon::Default;
        }
        if let Some(c) = self.split_cursor(x, y) {
            return c;
        }
        if l.divider.w > 0 && Rect::new(l.divider.x - 4, l.divider.y, l.divider.w + 8, l.divider.h).contains(x, y) {
            return CursorIcon::ColResize;
        }
        if let Some(c) = self.md_cursor(x, y) {
            return c;
        }
        if let Some(c) = self.img_cursor(x, y) {
            return c;
        }
        if l.top_btns.iter().any(|(r, _)| r.contains(x, y))
            || l.status_btns.iter().any(|(r, _)| r.contains(x, y))
            || l.tabs.iter().any(|(r, _, _)| r.contains(x, y))
            || l.sidebar_tabs.iter().any(|(r, _)| r.contains(x, y))
            || l.crumbs.iter().any(|(r, _)| r.contains(x, y))
            || l.panel_tabs.iter().any(|(r, _)| r.contains(x, y))
            || self.terms_hit_at(x, y).is_some()
            || self.tests_clickable_at(x, y)
            || self.build_clickable_at(x, y)
            || self.fswatch_clickable_at(x, y)
            || l.completion_rows.iter().any(|(r, _)| r.contains(x, y))
            || self.diff_btn_at(x, y).is_some()
            || self.merge_btn_at(x, y)
        {
            return CursorIcon::Pointer;
        }
        if l.sidebar.contains(x, y) {
            let rel = y - (l.sidebar.y + TAB_H) as f64 + self.tree.scroll;
            let rows = if self.sidebar_mode == SidebarMode::Outline {
                self.docs.get(self.active).map_or(0, |d| d.symbols.len())
            } else {
                self.tree.rows.len()
            };
            return if y >= (l.sidebar.y + TAB_H) as f64 && rel >= 0.0 && ((rel / ROW_H as f64) as usize) < rows {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if let Some(d) = self.docs.get(self.active)
            && let Some(m) = d.minimap_layout.as_ref()
            && m.rect.contains(x, y)
        {
            return CursorIcon::Pointer;
        }
        if l.editor.contains(x, y) || self.split_in_other(x, y) {
            if self.mods.control_key()
                && self
                    .docs
                    .get(self.active)
                    .is_some_and(|d| d.hit_at(x, y).1.is_some_and(|h| d.hint_link(h).is_some()))
            {
                return CursorIcon::Pointer; // Ctrl over a hint with a target
            }
            // Gutter (line numbers, fold chevrons, breakpoints) is not text.
            let gutter_right = l.editor.x + 56;
            return if x < gutter_right as f64 && l.editor.contains(x, y) {
                CursorIcon::Default
            } else {
                CursorIcon::Text
            };
        }
        if l.panel.contains(x, y) {
            return CursorIcon::Text;
        }
        CursorIcon::Default
    }

    /// The clickable item under the pointer (rect + corner radius) for the hover tint.
    pub(super) fn hover_target(&self) -> Option<(Rect, i32)> {
        let (x, y) = self.mouse;
        let l = &self.layout;
        if !self.overlay.is_none()
            || self.dragging
            || self.dragging_minimap
            || self.tab_dragging()
            || self.sidebar_resizing()
        {
            return None;
        }
        if let Some((r, _)) = l.top_btns.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 8));
        }
        if let Some((r, _)) = l.status_btns.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 6));
        }
        if let Some((r, _, i)) = l.tabs.iter().find(|(r, _, _)| r.contains(x, y)) {
            if *i != self.active {
                return Some((Rect::new(r.x + 2, r.y + 5, r.w - 4, r.h - 10), 8));
            }
            return None;
        }
        if let Some((r, _)) = l.sidebar_tabs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((Rect::new(r.x, r.y + 8, r.w, r.h - 16), 6));
        }
        if let Some((r, _)) = l.crumbs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 5));
        }
        if let Some((r, _)) = l.panel_tabs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 6));
        }
        if let Some(h) = self.terms_hover_target() {
            return Some(h);
        }
        if let Some(h) = self.aipanel_hover_target() {
            return Some(h);
        }
        if let Some(h) = self.debug_hover_target() {
            return Some(h);
        }
        if let Some(h) = self.tests_hover_target() {
            return Some(h);
        }
        if let Some(h) = self.build_hover_target() {
            return Some(h);
        }
        if self.sidebar_open && l.sidebar.contains(x, y) && y >= (l.sidebar.y + TAB_H) as f64 {
            let rel = y - (l.sidebar.y + TAB_H) as f64 + self.tree.scroll;
            let rows = if self.sidebar_mode == SidebarMode::Outline {
                self.docs.get(self.active).map_or(0, |d| d.symbols.len())
            } else {
                self.tree.rows.len()
            };
            if rel >= 0.0 && ((rel / ROW_H as f64) as usize) < rows {
                let k = (rel / ROW_H as f64) as i32;
                let top = l.sidebar.y + TAB_H - self.tree.scroll as i32;
                return Some((Rect::new(l.sidebar.x + 8, top + k * ROW_H + 1, l.sidebar.w - 17, ROW_H - 2), 7));
            }
        }
        None
    }

    pub fn update_cursor(&mut self) {
        // Pointing at a palette row selects it (like macOS menus).
        let (mx, my) = self.mouse;
        if let Overlay::TopMenu(m) = &mut self.overlay
            && let Some((_, k)) = m.rows.iter().find(|(r, _)| r.contains(mx, my))
            && m.sel != *k
        {
            m.sel = *k;
            self.request_redraw();
        }
        if let Overlay::Picker(p) = &mut self.overlay
            && let Some((_, idx)) = self.layout.overlay_rows.iter().find(|(r, _)| r.contains(mx, my))
            && p.sel != *idx
        {
            p.sel = *idx;
            self.request_redraw();
        }
        let hot_ed = self.docs.get(self.active).and_then(|d| d.sb).is_some_and(|b| b.track.contains(mx, my));
        if self.hbar_hover() {
            self.request_redraw();
        }
        let hot_sb = self.sidebar_bar.is_some_and(|b| b.track.contains(mx, my));
        if hot_ed != self.docs.get(self.active).is_some_and(|d| d.sb_hot) {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.sb_hot = hot_ed;
            }
            self.request_redraw();
        }
        let hot_reg = self.over_region_bar(mx, my);
        if hot_reg != self.region_hot {
            self.region_hot = hot_reg;
            self.request_redraw();
        }
        if hot_sb != self.sidebar_bar_hot {
            self.sidebar_bar_hot = hot_sb;
            self.request_redraw();
        }
        let th = self
            .layout
            .tabs
            .iter()
            .find(|(r, _, _)| r.contains(mx, my) && !self.tab_dragging())
            .map(|(_, close, i)| (*i, close.contains(mx, my)));
        if th != self.tab_hover {
            self.tab_hover = th;
            self.request_redraw();
        }
        let hv = self.hover_target();
        if hv != self.hover_rect {
            self.hover_rect = hv;
            self.request_redraw();
        }
        self.update_tooltip();
        let want = self.cursor_for();
        if want != self.cursor_icon {
            self.cursor_icon = want;
            if let Some(g) = &self.gfx {
                g.window.set_cursor(Cursor::Icon(want));
            }
        }
    }

    /// (total, visible) of the sidebar list in pixels.
    pub(super) fn sidebar_extent(&self) -> (f64, f64) {
        let count = if self.sidebar_mode == SidebarMode::Outline {
            self.docs.get(self.active).map_or(0, |d| d.symbols.len())
        } else {
            self.tree.rows.len()
        };
        (count as f64 * ROW_H as f64, (self.layout.sidebar.h - TAB_H) as f64)
    }

    pub(super) fn sidebar_scroll_to(&mut self, y: f64, grab: f64) {
        if let Some(b) = self.sidebar_bar {
            let (total, visible) = self.sidebar_extent();
            self.tree.scroll = b.pos_for(y, grab, total, visible);
        }
    }

    pub fn mouse_move(&mut self) {
        let (x, y) = self.mouse;
        if self.hover_mouse_move() {
            return self.request_redraw();
        }
        if self.regions_drag() {
            return self.request_redraw();
        }
        if self.diff_drag_move(x, y) {
            return self.request_redraw();
        }
        if self.aipanel_move() {
            return self.request_redraw();
        }
        if self.term_drag_move() {
            self.request_redraw();
            if self.term_dragging() {
                return;
            }
        }
        if self.tree_drag_move() || self.sidebar_edge_move() || self.tab_drag_move() {
            return self.request_redraw();
        }
        if let Some((kind, grab)) = self.drag_bar {
            match kind {
                0 => {
                    if let Some(d) = self.docs.get_mut(self.active) {
                        d.scrollbar_drag(y, grab);
                    }
                }
                1 => self.sidebar_scroll_to(y, grab),
                _ => self.hbar_drag_move(kind, x, grab),
            }
            return self.request_redraw();
        }
        if self.ctxmenu_active() {
            if self.ctxmenu_hover() {
                self.request_redraw();
            }
            return;
        }
        if self.gitpanel_move() {
            self.request_redraw();
        }
        if self.split_drag(x, y) {
            return self.request_redraw();
        }
        if self.md_mouse_move(x, y) {
            return;
        }
        if self.img_mouse_move(x, y) {
            return;
        }
        if self.dragging_minimap {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.minimap_scroll_to(y);
            }
            return self.request_redraw();
        }
        if self.dragging {
            self.drag_update();
            return self.request_redraw();
        }
        if self.over_editor() && self.overlay.is_none() && !self.docs.is_empty() {
            let p = self.docs[self.active].pos_at(x, y);
            self.hover_pointer(Some(p));
        } else {
            self.hover_pointer(None);
        }
    }

    pub(super) fn sidebar_activate(&mut self) {
        let Some(row) = self.tree.rows.get(self.tree.sel).cloned() else { return };
        if row.is_dir {
            self.tree.toggle(self.tree.sel);
        } else {
            self.open_path(&row.path, None);
            self.focus = Focus::Editor;
        }
    }
}
