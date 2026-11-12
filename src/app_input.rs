//! Pointer input that is more than a click: wheel / touchpad scrolling (editor, sidebar, panel),
//! drag selection (by characters, words, lines, blocks) with auto-scroll, the horizontal scrollbars
//! and the release logic. The selection rules are pure functions over `Doc` (`DragSel`), so they are
//! unit-tested without a window.

use crate::{
    app::{App, ROW_H},
    buffer::Sel,
    doc::Doc,
    doc_scroll::{H_STEP, WheelIn},
    editor::Editor,
    scroller::{self, Scroller, WHEEL_ROWS},
    ui::{HBar, Rect, line_h},
};
use winit::event::MouseScrollDelta;

// ───────────────────────────── drag selection ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragKind {
    Char,
    Word,
    Line,
    Block,
}

/// A selection being dragged. The anchor is a point (`lo == hi`) for characters and the initial
/// word / line for the other modes.
#[derive(Clone, Debug)]
pub struct DragSel {
    pub kind: DragKind,
    pub lo: usize,
    pub hi: usize,
    /// Pressed inside an existing selection: nothing changes until the pointer moves to another
    /// character; a plain release then collapses the selection (a click).
    pub pending: bool,
    /// Block mode: anchor (line, visual column) and the cursors that were there before.
    pub block: (usize, usize),
    pub base: Vec<Sel>,
    /// Middle button: a column selection once dragged, else (with `pending`) a paste on release.
    pub middle: bool,
}

impl DragSel {
    /// Mouse press at char `p`. `clicks`: 1 single, 2 double (word), 3+ triple (line).
    pub fn press(d: &mut Doc, p: usize, clicks: u32, shift: bool) -> DragSel {
        let mut ds = DragSel {
            kind: DragKind::Char,
            lo: p,
            hi: p,
            pending: false,
            block: (0, 0),
            base: Vec::new(),
            middle: false,
        };
        match clicks {
            1 => {
                let s = d.ed.sel;
                if shift {
                    ds.lo = s.anchor;
                    ds.hi = s.anchor;
                    d.ed.click(&mut d.buf, p, true);
                } else if !s.is_empty() && p >= s.start() && p < s.end() {
                    ds.pending = true;
                } else {
                    d.ed.click(&mut d.buf, p, false);
                }
            }
            2 => {
                if let Some((ws, we)) = Self::word_span(d, p) {
                    d.ed.select_range(&mut d.buf, ws, we);
                }
                ds.kind = DragKind::Word;
                (ds.lo, ds.hi) = (d.ed.sel.start(), d.ed.sel.end());
            }
            _ => {
                let (ls, le) = Self::line_span(d, p);
                d.ed.select_range(&mut d.buf, ls, le);
                ds.kind = DragKind::Line;
                (ds.lo, ds.hi) = (d.ed.sel.start(), d.ed.sel.end());
            }
        }
        ds
    }

    /// The line holding `p` for line selection; a folded header takes its hidden lines along
    /// (the row on screen stands for all of them).
    fn line_span(d: &Doc, p: usize) -> (usize, usize) {
        let (s, e) = Editor::line_range(&d.buf, p);
        let (l, _) = d.buf.line_col(p);
        let (_, last) = d.fold_extend(l, l);
        if last > l {
            let n = d.buf.line_count();
            return (s, if last + 1 < n { d.buf.line_start(last + 1) } else { d.buf.len_chars() });
        }
        (s, e)
    }

    /// The word under `p`; on an empty line there is no word, so the range is just that point
    /// (not the previous line's break).
    fn word_span(d: &Doc, p: usize) -> Option<(usize, usize)> {
        let (l, _) = d.buf.line_col(p);
        if d.buf.line_len(l) == 0 {
            return Some((p, p));
        }
        Editor::word_range(&d.buf, p)
    }

    /// Alt+press: a new cursor at `p` that becomes a block selection when dragged.
    pub fn press_block(d: &mut Doc, p: usize, vcol: usize) -> DragSel {
        let base = d.ed.all_sels();
        d.ed.add_cursor_at(p);
        let (line, _) = d.buf.line_col(p);
        DragSel { kind: DragKind::Block, lo: p, hi: p, pending: false, block: (line, vcol), base, middle: false }
    }

    /// Shift+Alt+press at `pt` = (line, visual column): a column selection from the current box's
    /// corner (or the primary cursor) to the pointer, replacing the other cursors; dragging resizes it.
    pub fn press_box(d: &mut Doc, p: usize, pt: (usize, usize)) -> DragSel {
        let anchor = d.ed.box_anchor(&d.buf);
        d.ed.block_select(&d.buf, anchor, pt, &[], d.view.folds.hidden_ranges());
        DragSel { kind: DragKind::Block, lo: p, hi: p, pending: false, block: anchor, base: Vec::new(), middle: false }
    }

    /// Middle press at char `p` / point `pt`: nothing changes yet. Dragging to another column or
    /// line makes a column selection anchored here; a release without moving pastes (see `mouse_up`).
    pub fn press_middle(p: usize, pt: (usize, usize)) -> DragSel {
        DragSel { kind: DragKind::Block, lo: p, hi: p, pending: true, block: pt, base: Vec::new(), middle: true }
    }

    /// The pointer is now at char `p` (visual column `vcol`, unclamped, for blocks).
    pub fn update(&mut self, d: &mut Doc, p: usize, vcol: usize) {
        match self.kind {
            DragKind::Char => {
                if self.pending {
                    if p == self.lo {
                        return;
                    }
                    self.pending = false;
                    d.ed.select_range(&mut d.buf, self.lo, p);
                } else {
                    d.ed.click(&mut d.buf, p, true);
                }
            }
            DragKind::Word => {
                if let Some((ws, we)) = Self::word_span(d, p) {
                    if ws < self.lo {
                        d.ed.select_range(&mut d.buf, self.hi, ws);
                    } else {
                        d.ed.select_range(&mut d.buf, self.lo, we.max(self.hi));
                    }
                }
            }
            DragKind::Line => {
                let (ls, le) = Self::line_span(d, p);
                if ls < self.lo {
                    d.ed.select_range(&mut d.buf, self.hi, ls);
                } else {
                    d.ed.select_range(&mut d.buf, self.lo, le.max(self.hi));
                }
            }
            DragKind::Block => {
                let (line, _) = d.buf.line_col(p);
                if self.pending {
                    if (line, vcol) == self.block {
                        return;
                    }
                    self.pending = false;
                }
                d.ed.block_select(&d.buf, self.block, (line, vcol), &self.base, d.view.folds.hidden_ranges());
            }
        }
    }

    /// Button released: a press inside a selection that never became a drag was a click.
    pub fn release(self, d: &mut Doc) {
        if self.pending {
            d.ed.click(&mut d.buf, self.lo, false);
        }
    }
}

// ───────────────────────────── app state ─────────────────────────────

/// Input state that lives in `App` (one field there).
#[derive(Default)]
pub struct InputState {
    pub drag: Option<DragSel>,
    pub tree_v: Scroller,
    pub tree_h: Scroller,
    pub panel_v: Scroller,
    /// Horizontal bar of the sidebar (last drawn frame) and its hover state.
    pub tree_hb: Option<HBar>,
    pub tree_hb_hot: bool,
    /// Widest sidebar row in px (content width), computed once per frame.
    pub tree_cw: f64,
    /// Dragging the sidebar's right edge (resize).
    pub sb_edge: crate::app_sidebar::SidebarEdge,
    /// Tab strip geometry and a tab being dragged to a new place.
    pub tabs: crate::app_tabdrag::TabStrip,
    /// Touchpad pixels of a bound wheel combo (Ctrl+wheel zoom) not yet turned into a step.
    pub wheel_acc: f64,
}

const HBAR_H: i32 = 12;
/// Touchpad pixels per step of a bound wheel combo (a mouse wheel notch is one step).
const WHEEL_STEP_PX: f64 = 50.0;

/// Steps of a bound wheel combo for one event (positive = up): a notch is one, touchpad pixels
/// add up in `acc` until they make a whole step (a reversal starts over).
pub fn wheel_steps(delta: MouseScrollDelta, acc: &mut f64) -> i32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => {
            let n = y.round() as i32;
            if n == 0 && y != 0.0 { y.signum() as i32 } else { n.clamp(-5, 5) }
        }
        MouseScrollDelta::PixelDelta(p) => {
            if *acc != 0.0 && acc.signum() != p.y.signum() {
                *acc = 0.0;
            }
            *acc += p.y;
            let n = (*acc / WHEEL_STEP_PX).trunc();
            *acc -= n * WHEEL_STEP_PX;
            n as i32
        }
    }
}

impl App {
    /// The wheel event reduced to (dx, dy) positive = down / right. Shift turns vertical into
    /// horizontal; a touchpad swipe that is mostly on one axis stays on it.
    pub fn wheel_in(&self, delta: MouseScrollDelta) -> WheelIn {
        let (mut dx, mut dy, pixels) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (-(x as f64), -(y as f64), false),
            MouseScrollDelta::PixelDelta(p) => (-p.x, -p.y, true),
        };
        if self.mods.shift_key() && dx == 0.0 {
            (dx, dy) = (dy, 0.0);
        } else if pixels {
            if dy.abs() > dx.abs() * 2.0 {
                dx = 0.0;
            } else if dx.abs() > dy.abs() * 2.0 {
                dy = 0.0;
            }
        }
        WheelIn { dx, dy, pixels }
    }

    // ── mouse bindings (keybindings.json mouse combos, see `keymap::MOUSE_DEFAULTS`) ──

    /// Back / forward side buttons: what the keymap binds to `mouse4` / `mouse5` (NavBack /
    /// NavForward by default), or a combo for the shortcuts page while it records one.
    pub fn mouse_side_button(&mut self, button: winit::event::MouseButton) {
        let name = if button == winit::event::MouseButton::Back { "mouse4" } else { "mouse5" };
        if self.settings_record_mouse(name) {
            return self.request_redraw();
        }
        if !self.overlay.is_none() {
            return;
        }
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if let Some(id) = self.keymap.lookup_mouse(ctrl, shift, alt, name).map(str::to_string) {
            self.run_mouse_action(&id);
        }
    }

    /// Run a mouse binding's target: a mouse-only action (`PastePrimary` pastes the primary
    /// selection at the caret; `AddCursor` only means something on a click in the text, see
    /// `mouse_down`) or any palette command id.
    pub fn run_mouse_action(&mut self, id: &str) {
        match id {
            "none" | "AddCursor" => {}
            "PastePrimary" => {
                let text = self.clip.get_primary();
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.ed.paste(&mut d.buf, &text);
                    d.reveal = true;
                }
            }
            _ => {
                self.run_bound(id);
            }
        }
        self.request_redraw();
    }

    /// The wheel with a bound combo (`ctrl+wheelup` / `ctrl+wheeldown` zoom by default): runs the
    /// command once per notch. True when the event was taken (it does not scroll then).
    fn wheel_binding(&mut self, delta: MouseScrollDelta) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64,
            MouseScrollDelta::PixelDelta(p) => p.y,
        };
        if dy == 0.0 {
            return false;
        }
        if (ctrl || alt) && self.settings_record_mouse(if dy > 0.0 { "wheelup" } else { "wheeldown" }) {
            return true;
        }
        let km = &self.keymap;
        let up = km.lookup_mouse(ctrl, shift, alt, "wheelup").filter(|id| *id != "none").map(str::to_string);
        let down = km.lookup_mouse(ctrl, shift, alt, "wheeldown").filter(|id| *id != "none").map(str::to_string);
        if up.is_none() && down.is_none() {
            self.inp.wheel_acc = 0.0;
            return false;
        }
        let steps = wheel_steps(delta, &mut self.inp.wheel_acc);
        if let Some(id) = if steps > 0 { up } else { down } {
            for _ in 0..steps.unsigned_abs() {
                self.run_mouse_action(&id);
            }
        }
        true
    }

    pub fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let (x, y) = self.mouse;
        if self.wheel_binding(delta) {
            return self.request_redraw();
        }
        if self.hover_wheel(delta) {
            return self.request_redraw();
        }
        if self.regions_wheel(delta) {
            return self.request_redraw();
        }
        if self.aipanel_wheel(delta) {
            return self.request_redraw();
        }
        if self.gitpanel_wheel(delta) {
            return self.request_redraw();
        }
        if self.layout.tabbar.contains(x, y) {
            // A notch is 3 tabs' worth (24 px "rows"); the dominant axis wins.
            let (dx, dy) = scroller::wheel_px_xy(delta, 24.0);
            let px = if dx.abs() > dy.abs() { dx } else { dy };
            self.tabs_scroll = (self.tabs_scroll + px).clamp(0.0, self.tabs_max);
            return self.request_redraw();
        }
        let w = self.wheel_in(delta);
        if self.sidebar_open && self.layout.sidebar.contains(x, y) {
            self.sidebar_wheel(w);
            return self.request_redraw();
        }
        if self.overlay_wheel(delta) {
            return self.request_redraw();
        }
        if self.panel_wheel(delta) {
            return self.request_redraw();
        }
        if self.diff_wheel(delta) {
            return self.request_redraw();
        }
        if self.md_wheel(delta) || self.split_wheel(delta) {
            return self.request_redraw();
        }
        self.hover = None;
        if let Some(d) = self.docs.get_mut(self.active) {
            d.wheel(w, scroller::now());
        }
        self.request_redraw();
    }

    /// Touchpad fingers lifted: let the scroll coast.
    pub fn on_wheel_end(&mut self) {
        let (x, y) = self.mouse;
        let now = scroller::now();
        if self.hover_wheel_end(now) || self.regions_fling() {
            return self.request_redraw();
        }
        if self.sidebar_open && self.layout.sidebar.contains(x, y) {
            self.inp.tree_v.adopt(self.tree.scroll);
            self.inp.tree_v.fling(now);
            self.inp.tree_h.adopt(self.tree.hscroll);
            self.inp.tree_h.fling(now);
        } else if let Some(d) = self.docs.get_mut(self.active)
            && self.layout.editor.contains(x, y)
            && self.overlay.is_none()
        {
            d.wheel_end(now);
        }
        self.request_redraw();
    }

    // ── sidebar ──

    fn sidebar_rows(&self) -> usize {
        if self.sidebar_mode == crate::app::SidebarMode::Outline {
            self.docs.get(self.active).map_or(0, |d| d.symbols.len())
        } else {
            self.tree.rows.len()
        }
    }

    fn sidebar_max_v(&self) -> f64 {
        (self.sidebar_rows() as f64 * ROW_H as f64 - (self.layout.sidebar.h - crate::app::TAB_H) as f64).max(0.0)
    }

    fn sidebar_max_h(&self) -> f64 {
        (self.inp.tree_cw - (self.layout.sidebar.w - 1) as f64).max(0.0)
    }

    /// Widest sidebar row: x of the text + its width + room for the git dot. Estimated from the
    /// character counts, with the one widest candidate measured exactly.
    fn sidebar_measure(&mut self) -> f64 {
        let avg = (self.tx.ui_width("0123456789abcdefghij", false) as f64 / 20.0).max(4.0);
        let (mut best, mut best_est) = (None::<(String, f64)>, 0.0f64);
        if self.sidebar_mode == crate::app::SidebarMode::Outline {
            if let Some(d) = self.docs.get(self.active) {
                for s in &d.symbols {
                    let start = 36.0 + s.depth as f64 * 14.0;
                    let est = start + s.name.chars().count() as f64 * avg;
                    if est > best_est {
                        best_est = est;
                        best = Some((s.name.clone(), start));
                    }
                }
            }
        } else {
            for r in &self.tree.rows {
                let start = 56.0 + r.depth as f64 * 14.0;
                let est = start + r.name.chars().count() as f64 * avg;
                if est > best_est {
                    best_est = est;
                    best = Some((r.name.clone(), start));
                }
            }
        }
        match best {
            Some((name, start)) => start + self.tx.ui_width(&name, false) as f64 + 30.0,
            None => 0.0,
        }
    }

    fn sidebar_wheel(&mut self, w: WheelIn) {
        let now = scroller::now();
        if w.dy != 0.0 {
            let max = self.sidebar_max_v();
            self.inp.tree_v.adopt(self.tree.scroll);
            if w.pixels {
                self.inp.tree_v.pixels(w.dy, max, now);
            } else {
                self.inp.tree_v.wheel(w.dy, ROW_H as f64 * WHEEL_ROWS, max, now);
            }
            self.tree.scroll = self.inp.tree_v.pos;
        }
        if w.dx != 0.0 {
            let max = self.sidebar_max_h();
            self.inp.tree_h.adopt(self.tree.hscroll);
            if w.pixels {
                self.inp.tree_h.pixels(w.dx, max, now);
            } else {
                self.inp.tree_h.wheel(w.dx, H_STEP, max, now);
            }
            self.tree.hscroll = self.inp.tree_h.pos;
        }
    }

    /// Wheel over the output panel (same dynamics as the editor).
    pub fn panel_wheel_apply(&mut self, delta: MouseScrollDelta, max: f64) {
        let w = self.wheel_in(delta);
        self.inp.panel_v.adopt(self.panel.scroll);
        if w.pixels {
            self.inp.panel_v.pixels(w.dy, max, scroller::now());
        } else {
            self.inp.panel_v.wheel(w.dy, line_h() as f64 * WHEEL_ROWS, max, scroller::now());
        }
        self.panel.scroll = self.inp.panel_v.pos;
        self.panel.follow = self.panel.scroll >= max - 1.0;
    }

    /// Per-frame input work (called at the start of drawing): scroll animations of the sidebar and
    /// the panel, and auto-scroll while a selection is dragged outside the text. True while busy.
    pub fn input_frame(&mut self, dt: f64) -> bool {
        let mut busy = self.drag_autoscroll(dt);
        busy |= self.tab_drag_frame(dt);
        busy |= self.regions_frame(dt);
        busy |= self.hover_frame(dt);
        if self.sidebar_open {
            self.inp.tree_cw = self.sidebar_measure();
            let (max_v, max_h) = (self.sidebar_max_v(), self.sidebar_max_h());
            self.inp.tree_v.adopt(self.tree.scroll);
            busy |= self.inp.tree_v.step(dt, max_v);
            self.tree.scroll = self.inp.tree_v.pos;
            if self.tree.scroll > max_v {
                self.tree.scroll = max_v;
                self.inp.tree_v.jump(max_v, max_v);
            }
            self.inp.tree_h.adopt(self.tree.hscroll);
            busy |= self.inp.tree_h.step(dt, max_h);
            self.tree.hscroll = self.inp.tree_h.pos;
            if self.tree.hscroll > max_h {
                self.tree.hscroll = max_h;
                self.inp.tree_h.jump(max_h, max_h);
            }
        }
        if self.panel.open {
            let max = self.panel.max_scroll(self.layout.panel.h - crate::app_panel::PANEL_HEADER);
            self.inp.panel_v.adopt(self.panel.scroll);
            if self.inp.panel_v.step(dt, max) {
                busy = true;
                self.panel.follow = self.inp.panel_v.pos >= max - 1.0;
            }
            self.panel.scroll = self.inp.panel_v.pos;
        }
        busy
    }

    /// Thin horizontal bar along the bottom of the sidebar when a row is wider than the sidebar.
    pub fn draw_sidebar_hbar(&mut self, cv: &mut crate::ui::Canvas) {
        let r = self.layout.sidebar;
        let (vw, max) = ((r.w - 1) as f64, self.sidebar_max_h());
        let track = Rect::new(r.x, r.bottom() - HBAR_H, (r.w - 1 - 14).max(0), HBAR_H);
        self.inp.tree_hb = if max > 0.5 { HBar::new(track, vw + max, vw, self.tree.hscroll) } else { None };
        if let Some(b) = self.inp.tree_hb {
            b.draw(cv, self.theme.foreground, self.inp.tree_hb_hot, self.drag_bar.is_some_and(|(k, _)| k == 3));
        }
    }

    /// Press on a horizontal scrollbar (editor or sidebar). True when it was on one.
    pub fn hbar_press(&mut self, x: f64, y: f64) -> bool {
        let grab_of = |b: &HBar| if b.thumb.contains(x, y) { x - b.thumb.x as f64 } else { b.thumb.w as f64 / 2.0 };
        if let Some(b) = self.docs.get(self.active).and_then(|d| d.hb)
            && b.track.contains(x, y)
        {
            let grab = grab_of(&b);
            self.drag_bar = Some((2, grab));
            if let Some(d) = self.docs.get_mut(self.active) {
                d.hb_drag = true;
                d.hbar_drag(x, grab);
            }
            return true;
        }
        if let Some(b) = self.inp.tree_hb.filter(|_| self.sidebar_open)
            && b.track.contains(x, y)
        {
            let grab = grab_of(&b);
            self.drag_bar = Some((3, grab));
            self.sidebar_hdrag(x, grab);
            return true;
        }
        false
    }

    pub fn sidebar_hdrag(&mut self, x: f64, grab: f64) {
        if let Some(b) = self.inp.tree_hb {
            let max = self.sidebar_max_h();
            let vw = (self.layout.sidebar.w - 1) as f64;
            self.tree.hscroll = b.pos_for(x, grab, vw + max, vw).clamp(0.0, max);
        }
    }

    /// Pointer drag of a scrollbar of kind 2 (editor, horizontal) or 3 (sidebar, horizontal).
    pub fn hbar_drag_move(&mut self, kind: u8, x: f64, grab: f64) {
        if kind == 2 {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.hbar_drag(x, grab);
            }
        } else {
            self.sidebar_hdrag(x, grab);
        }
    }

    /// Hover state of the horizontal bars (called with the cursor update). True when it changed.
    pub fn hbar_hover(&mut self) -> bool {
        let (x, y) = self.mouse;
        let mut changed = false;
        let hot_ed = self.docs.get(self.active).and_then(|d| d.hb).is_some_and(|b| b.track.contains(x, y));
        if let Some(d) = self.docs.get_mut(self.active)
            && d.hb_hot != hot_ed
        {
            d.hb_hot = hot_ed;
            changed = true;
        }
        let hot_sb = self.inp.tree_hb.is_some_and(|b| b.track.contains(x, y));
        if hot_sb != self.inp.tree_hb_hot {
            self.inp.tree_hb_hot = hot_sb;
            changed = true;
        }
        changed
    }

    pub fn over_hbar(&self, x: f64, y: f64) -> bool {
        self.docs.get(self.active).and_then(|d| d.hb).is_some_and(|b| b.track.contains(x, y))
            || self.inp.tree_hb.is_some_and(|b| b.track.contains(x, y))
    }

    // ── drag selection glue ──

    /// Visual column under window x for the active document (not clamped to the line length).
    /// Mouse press in the text (single / double / triple click, shift extends).
    pub fn drag_press(&mut self, p: usize, clicks: u32, shift: bool) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        self.inp.drag = Some(DragSel::press(d, p, clicks, shift));
        self.dragging = true;
    }

    /// Alt+press: add a cursor, and a column selection if the pointer is dragged.
    pub fn drag_press_block(&mut self, p: usize) {
        let (x, y) = self.mouse;
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let (_, vcol) = d.box_point_at(x, y);
        self.inp.drag = Some(DragSel::press_block(d, p, vcol));
        self.dragging = true;
    }

    /// Shift+Alt+press: column selection from the cursor to the pointer (resized by dragging).
    pub fn drag_press_box(&mut self, p: usize) {
        let (x, y) = self.mouse;
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let pt = d.box_point_at(x, y);
        self.inp.drag = Some(DragSel::press_box(d, p, pt));
        self.dragging = true;
    }

    /// Middle press in the text: a column selection when dragged, a paste of the primary
    /// selection when released in place.
    pub fn drag_press_middle(&mut self) {
        let (x, y) = self.mouse;
        let Some(d) = self.docs.get(self.active) else { return };
        let p = d.pos_at(x, y);
        let pt = d.box_point_at(x, y);
        self.inp.drag = Some(DragSel::press_middle(p, pt));
        self.dragging = true;
    }

    /// Ctrl+Shift+Alt+arrows: grow / shrink the column selection of the focused document.
    pub fn column_select(&mut self, dl: isize, dc: isize) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        d.column_select(dl, dc);
        d.reveal = true;
    }

    /// Extend the dragged selection to the pointer (clamped into the text area).
    pub fn drag_update(&mut self) {
        if !self.dragging {
            return;
        }
        let Some(mut ds) = self.inp.drag.take() else { return };
        let (x, y) = self.mouse;
        if let Some(d) = self.docs.get_mut(self.active) {
            let bx = (d.rect.x + d.gutter_w()) as f64;
            let px = x.clamp(bx, ((d.rect.right() - 1) as f64).max(bx));
            let py = y.clamp(d.rect.y as f64, ((d.rect.bottom() - 1) as f64).max(d.rect.y as f64));
            let p = d.pos_at(px, py);
            let (_, vcol) = d.box_point_at(px, py);
            ds.update(d, p, vcol);
        }
        self.inp.drag = Some(ds);
    }

    /// While the pointer is outside the text the view keeps moving, faster the farther it is
    /// (vertical and horizontal), and the selection follows. True while the view moved.
    pub fn drag_autoscroll(&mut self, dt: f64) -> bool {
        if !self.dragging || self.inp.drag.is_none() {
            return false;
        }
        let (x, y) = self.mouse;
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        let r = d.rect;
        let bx = (r.x + d.gutter_w()) as f64;
        let dy = if y < r.y as f64 {
            y - r.y as f64
        } else if y > r.bottom() as f64 {
            y - r.bottom() as f64
        } else {
            0.0
        };
        let dx = if x < bx {
            x - bx
        } else if x > r.right() as f64 {
            x - r.right() as f64
        } else {
            0.0
        };
        if dx == 0.0 && dy == 0.0 {
            return false;
        }
        let speed = |dist: f64| dist.signum() * (60.0 + dist.abs() * 10.0).min(4000.0);
        let before = (d.top, d.left);
        if dy != 0.0 {
            d.scroll_direct(speed(dy) * dt / line_h() as f64);
        }
        if dx != 0.0 && d.view.cols == 0 {
            let to = d.left + speed(dx) * dt;
            d.scroll_left_to(to);
        }
        let moved = (d.top, d.left) != before;
        self.drag_update();
        moved
    }

    /// Button released (anywhere): ends scrollbar drags and selection drags.
    pub fn mouse_up(&mut self) {
        self.hover_mouse_up();
        self.gitpanel_release();
        self.aipanel_release();
        self.term_release();
        self.sidebar_edge_release();
        self.tab_drag_release();
        self.tree_drag_release();
        self.diff_release();
        self.dragging_minimap = false;
        if self.regions.drag.take().is_some() {
            self.request_redraw();
        }
        if self.drag_bar.take().is_some() {
            for d in &mut self.docs {
                d.sb_drag = false;
                d.hb_drag = false;
            }
            self.request_redraw();
        }
        self.split_release();
        if self.dragging {
            if let Some(ds) = self.inp.drag.take() {
                if ds.middle && ds.pending {
                    // A middle click that never moved: paste the primary selection there.
                    let text = self.clip.get_primary();
                    if let Some(d) = self.docs.get_mut(self.active) {
                        d.ed.clear_extra();
                        d.ed.click(&mut d.buf, ds.lo, false);
                        d.ed.paste(&mut d.buf, &text);
                        d.reveal = true;
                    }
                } else if let Some(d) = self.docs.get_mut(self.active) {
                    ds.release(d);
                }
                self.request_redraw();
            }
            if let Some(d) = self.docs.get(self.active) {
                let sel = d.ed.selected_text(&d.buf);
                if !sel.is_empty() {
                    self.clip.set_primary(&sel);
                }
            }
        }
        self.inp.drag = None;
        self.dragging = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    fn doc(text: &str) -> Doc {
        let mut d = Doc::new(Buffer::from_str(text), None, None, "//", 8.0);
        d.rect = Rect::new(0, 0, 600, 300);
        d
    }

    const TXT: &str = "alpha beta gamma\nsecond line here\nthird\n";

    fn sel(d: &Doc) -> (usize, usize) {
        (d.ed.sel.anchor, d.ed.sel.head)
    }

    #[test]
    fn single_click_drag_extends_from_the_press_point() {
        let mut d = doc(TXT);
        let mut ds = DragSel::press(&mut d, 3, 1, false);
        assert_eq!(sel(&d), (3, 3));
        ds.update(&mut d, 9, 9);
        assert_eq!(sel(&d), (3, 9));
        ds.update(&mut d, 1, 1); // dragging back past the anchor flips the selection
        assert_eq!(sel(&d), (3, 1));
    }

    #[test]
    fn dragging_after_a_selection_exists_selects_more() {
        // the reported bug: select a word, then press shift+click or drag again to grow it
        let mut d = doc(TXT);
        let mut ds = DragSel::press(&mut d, 2, 2, false); // double click on "alpha"
        assert_eq!(sel(&d), (0, 5));
        ds.update(&mut d, 8, 8); // drag into "beta"
        assert_eq!(sel(&d), (0, 10), "the selection grows word by word");
        ds.release(&mut d);
        // shift+click extends the existing selection from its anchor
        let _ = DragSel::press(&mut d, 25, 1, true);
        assert_eq!(d.ed.sel.anchor, 0);
        assert_eq!(d.ed.sel.head, 25);
    }

    #[test]
    fn shift_click_then_drag_keeps_the_original_anchor() {
        let mut d = doc(TXT);
        d.ed.click(&mut d.buf, 6, false);
        let mut ds = DragSel::press(&mut d, 10, 1, true);
        assert_eq!(sel(&d), (6, 10));
        ds.update(&mut d, 14, 14);
        assert_eq!(sel(&d), (6, 14));
    }

    #[test]
    fn double_click_drag_extends_by_words_in_both_directions() {
        let mut d = doc(TXT);
        let mut ds = DragSel::press(&mut d, 8, 2, false); // "beta" = 6..10
        assert_eq!(sel(&d), (6, 10));
        ds.update(&mut d, 13, 13); // inside "gamma" (11..16)
        assert_eq!(sel(&d), (6, 16));
        ds.update(&mut d, 2, 2); // back into "alpha" (0..5): anchor flips to the end of "beta"
        assert_eq!(sel(&d), (10, 0));
        ds.update(&mut d, 7, 7); // inside the first word again: exactly that word
        assert_eq!(sel(&d), (6, 10));
    }

    #[test]
    fn triple_click_drag_extends_by_lines() {
        let mut d = doc(TXT);
        let l1 = d.buf.line_start(1);
        let l2 = d.buf.line_start(2);
        let mut ds = DragSel::press(&mut d, l1 + 3, 3, false);
        assert_eq!(sel(&d), (l1, l2));
        ds.update(&mut d, l2 + 2, 0); // down to the third line
        assert_eq!(sel(&d), (l1, d.buf.len_chars()));
        ds.update(&mut d, 4, 4); // up to the first line: flips
        assert_eq!(sel(&d), (l2, 0));
    }

    #[test]
    fn press_inside_a_selection_only_clears_on_release() {
        let mut d = doc(TXT);
        d.ed.select_range(&mut d.buf, 0, 10);
        let ds = DragSel::press(&mut d, 4, 1, false);
        assert_eq!(sel(&d), (0, 10), "the selection survives the press");
        ds.release(&mut d);
        assert_eq!(sel(&d), (4, 4), "a plain click collapses it on release");
    }

    #[test]
    fn press_inside_a_selection_and_drag_starts_a_new_selection() {
        let mut d = doc(TXT);
        d.ed.select_range(&mut d.buf, 0, 10);
        let mut ds = DragSel::press(&mut d, 4, 1, false);
        ds.update(&mut d, 4, 4);
        assert_eq!(sel(&d), (0, 10), "same character: still a pending click");
        ds.update(&mut d, 12, 12);
        assert_eq!(sel(&d), (4, 12));
        ds.release(&mut d);
        assert_eq!(sel(&d), (4, 12), "released after a drag: nothing collapses");
    }

    #[test]
    fn press_outside_a_selection_replaces_it() {
        let mut d = doc(TXT);
        d.ed.select_range(&mut d.buf, 0, 10);
        let _ = DragSel::press(&mut d, 14, 1, false);
        assert_eq!(sel(&d), (14, 14));
    }

    #[test]
    fn alt_drag_makes_a_column_selection() {
        let mut d = doc("abcdef\nabcdef\nabcdef\nab\n");
        let p = d.buf.pos(0, 1);
        let mut ds = DragSel::press_block(&mut d, p, 1);
        assert_eq!(d.ed.extra.len(), 1, "alt+click alone adds a cursor");
        let pend = d.buf.pos(2, 4);
        ds.update(&mut d, pend, 4);
        let all = d.ed.all_sels();
        // base cursor (0,0) + three block lines; the block on line 0 is the same char range as...
        // the pre-existing primary caret at 0 and block selections 1..4 on lines 0, 1, 2
        let lines: Vec<(usize, usize, usize)> = all
            .iter()
            .map(|s| (d.buf.line_col(s.start()).0, d.buf.line_col(s.start()).1, d.buf.line_col(s.end()).1))
            .collect();
        assert!(lines.contains(&(0, 1, 4)), "{lines:?}");
        assert!(lines.contains(&(1, 1, 4)), "{lines:?}");
        assert!(lines.contains(&(2, 1, 4)), "{lines:?}");
        // a short line clamps to its end
        let pend = d.buf.pos(3, 2);
        ds.update(&mut d, pend, 4);
        let lines: Vec<(usize, usize, usize)> =
            d.ed.all_sels()
                .iter()
                .map(|s| (d.buf.line_col(s.start()).0, d.buf.line_col(s.start()).1, d.buf.line_col(s.end()).1))
                .collect();
        assert!(lines.contains(&(3, 1, 2)), "{lines:?}");
    }

    fn box_lines(d: &Doc) -> Vec<(usize, usize, usize)> {
        d.ed.all_sels()
            .iter()
            .map(|s| (d.buf.line_col(s.start()).0, d.buf.line_col(s.start()).1, d.buf.line_col(s.end()).1))
            .collect()
    }

    #[test]
    fn shift_alt_press_boxes_from_the_cursor_and_keeps_extending_it() {
        let mut d = doc("abcdef\nabcdef\nabcdef\n");
        d.ed.click(&mut d.buf, 1, false); // (0, 1)
        let other = d.buf.pos(2, 0);
        d.ed.extra.push(Sel::caret(other)); // an unrelated extra cursor goes away
        let p = d.buf.pos(2, 4);
        let mut ds = DragSel::press_box(&mut d, p, (2, 4));
        assert_eq!(box_lines(&d), vec![(0, 1, 4), (1, 1, 4), (2, 1, 4)]);
        let to = d.buf.pos(1, 2);
        ds.update(&mut d, to, 2);
        assert_eq!(box_lines(&d), vec![(0, 1, 2), (1, 1, 2)], "dragging resizes from the same corner");
        ds.release(&mut d);
        // Another Shift+Alt+click grows the same box.
        let p = d.buf.pos(2, 5);
        let _ = DragSel::press_box(&mut d, p, (2, 5));
        assert_eq!(box_lines(&d), vec![(0, 1, 5), (1, 1, 5), (2, 1, 5)]);
    }

    #[test]
    fn middle_drag_makes_a_column_selection_and_a_still_click_stays_a_paste() {
        let mut d = doc("abcdef\nabcdef\nabcdef\n");
        d.ed.click(&mut d.buf, 2, false);
        let p = d.buf.pos(0, 1);
        let mut ds = DragSel::press_middle(p, (0, 1));
        assert_eq!(sel(&d), (2, 2), "nothing changes on press");
        ds.update(&mut d, p, 1);
        assert!(ds.pending && ds.middle, "same point: still a click (pasted on release)");
        assert_eq!(sel(&d), (2, 2));
        let to = d.buf.pos(2, 3);
        ds.update(&mut d, to, 3);
        assert!(!ds.pending);
        assert_eq!(box_lines(&d), vec![(0, 1, 3), (1, 1, 3), (2, 1, 3)], "anchored at the press point");
    }

    #[test]
    fn middle_click_pastes_the_primary_selection_and_middle_drag_does_not() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp, untitled};
        let _g = lock();
        let dir = tmp("middlebox");
        let mut app = test_app(&dir);
        let i = untitled(&mut app, "abcdef\nabcdef\nabcdef\n");
        app.active = i;
        app.clip.set_primary("XY");
        app.inp.drag = Some(DragSel::press_middle(3, (0, 3)));
        app.dragging = true;
        app.mouse_up();
        assert_eq!(app.docs[i].buf.to_string(), "abcXYdef\nabcdef\nabcdef\n");
        // A drag that became a box pastes nothing.
        let mut ds = DragSel::press_middle(1, (0, 1));
        let d = &mut app.docs[i];
        let to = d.buf.pos(1, 2);
        ds.update(d, to, 2);
        app.inp.drag = Some(ds);
        app.dragging = true;
        app.mouse_up();
        assert_eq!(app.docs[i].buf.to_string(), "abcXYdef\nabcdef\nabcdef\n");
        assert_eq!(app.docs[i].ed.all_sels().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn folded_doc() -> Doc {
        // 0 fn a() {  1     x();  2 }  3 fn b() {  4     y();  5 }  6 tail
        let mut d = doc("fn a() {\n    x();\n}\nfn b() {\n    y();\n}\ntail\n");
        let p = d.buf.pos(3, 0);
        d.ed.click(&mut d.buf, p, false);
        assert!(d.fold_at_cursor()); // fold `fn b` (lines 3..4)
        d.view_prepare(80);
        d
    }

    #[test]
    fn line_selection_takes_a_folded_headers_hidden_lines() {
        let mut d = folded_doc();
        let (l3, l5) = (d.buf.line_start(3), d.buf.line_start(5));
        // triple click on the folded header selects it with its body
        let ds = DragSel::press(&mut d, l3 + 2, 3, false);
        assert_eq!(sel(&d), (l3, l5), "header + hidden line, not just the header");
        assert_eq!((ds.lo, ds.hi), (l3, l5));
        // a plain line still selects one line
        let _ = DragSel::press(&mut d, 2, 3, false);
        assert_eq!(sel(&d), (0, d.buf.line_start(1)));
    }

    #[test]
    fn line_drag_over_a_folded_header_includes_its_body_in_both_directions() {
        let mut d = folded_doc();
        let (l0, l3, l5, l6) = (d.buf.line_start(0), d.buf.line_start(3), d.buf.line_start(5), d.buf.line_start(6));
        // gutter-style line drag from line 0 down to the folded header: its body is selected too
        let mut ds = DragSel::press(&mut d, 1, 3, false);
        ds.update(&mut d, l3 + 3, 3);
        assert_eq!(sel(&d), (l0, l5));
        // and on to the next visible line
        ds.update(&mut d, l6 + 1, 1);
        assert_eq!(sel(&d), (l0, d.buf.len_chars()));
        // from the folded header upwards: the anchor block is the whole fold
        let mut ds = DragSel::press(&mut d, l3 + 1, 3, false);
        ds.update(&mut d, 1, 1);
        assert_eq!(sel(&d), (l5, l0));
    }

    #[test]
    fn word_drag_on_empty_lines_does_not_grab_the_previous_break() {
        let mut d = doc("one two\n\nthree\n");
        let blank = d.buf.line_start(1);
        let mut ds = DragSel::press(&mut d, 5, 2, false); // "two" = 4..7
        assert_eq!(sel(&d), (4, 7));
        ds.update(&mut d, blank, 0);
        assert_eq!(sel(&d), (4, blank), "extends to the empty line itself");
        let mut ds = DragSel::press(&mut d, 14, 2, false); // "three" = 9..14
        assert_eq!(sel(&d), (9, 14));
        ds.update(&mut d, blank, 0);
        assert_eq!(sel(&d), (14, blank), "backwards: the head sits on the empty line, not before its break");
    }

    #[test]
    fn drag_works_with_folds_and_wrap_positions() {
        // positions come from pos_at, so the selection logic only sees char indexes: a wrapped doc
        // gives the same result
        let mut d = doc(&"word ".repeat(80));
        d.view.cols = 20;
        let mut ds = DragSel::press(&mut d, 3, 1, false);
        ds.update(&mut d, 200, 200);
        assert_eq!(sel(&d), (3, 200));
    }

    #[test]
    fn bound_wheel_steps_per_notch_and_per_touchpad_distance() {
        let mut acc = 0.0;
        assert_eq!(wheel_steps(MouseScrollDelta::LineDelta(0.0, 1.0), &mut acc), 1);
        assert_eq!(wheel_steps(MouseScrollDelta::LineDelta(0.0, -1.0), &mut acc), -1);
        assert_eq!(wheel_steps(MouseScrollDelta::LineDelta(0.0, 0.3), &mut acc), 1, "a partial notch is a step");
        assert_eq!(wheel_steps(MouseScrollDelta::LineDelta(0.0, 40.0), &mut acc), 5, "capped");
        let px = |y: f64| MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, y));
        assert_eq!(wheel_steps(px(20.0), &mut acc), 0);
        assert_eq!(wheel_steps(px(20.0), &mut acc), 0);
        assert_eq!(wheel_steps(px(20.0), &mut acc), 1, "60 px make one step, 10 px stay");
        assert!((acc - 10.0).abs() < 1e-9);
        assert_eq!(wheel_steps(px(-30.0), &mut acc), 0, "a reversal starts over");
        assert_eq!(wheel_steps(px(-30.0), &mut acc), -1);
    }

    #[test]
    fn mouse_bindings_dispatch() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("mousebind");
        let mut app = test_app(&dir);
        // Ctrl+wheel zooms (one step per notch), the plain wheel does not.
        let size = crate::ui::font_size();
        app.mods = winit::keyboard::ModifiersState::CONTROL;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
        assert_eq!(crate::ui::font_size(), size + 1.0, "Ctrl+wheel up zooms in");
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -1.0));
        assert_eq!(crate::ui::font_size(), size);
        app.mods = winit::keyboard::ModifiersState::empty();
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
        assert_eq!(crate::ui::font_size(), size, "the plain wheel scrolls");
        // A user binding for the side button runs its command; "none" switches Ctrl+wheel off.
        app.keymap = crate::keymap::Keymap::from_json(r#"{"mouse4": "ToggleSidebar", "ctrl+wheelup": "none"}"#);
        let open = app.sidebar_open;
        app.mouse_side_button(winit::event::MouseButton::Back);
        assert_eq!(app.sidebar_open, !open);
        app.mods = winit::keyboard::ModifiersState::CONTROL;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
        assert_eq!(crate::ui::font_size(), size, "disabled");
        app.mods = winit::keyboard::ModifiersState::empty();
        // The primary selection lands at the caret.
        app.clip.set_primary("XY");
        let d = &mut app.docs[0];
        d.ed.click(&mut d.buf, 0, false);
        app.active = 0;
        app.run_mouse_action("PastePrimary");
        let text = app.docs[0].buf.to_string();
        assert!(text.starts_with("XYseed"), "{text}");
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
