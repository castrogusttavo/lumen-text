use super::*;

impl App {
    // ───────────── pointer, keys, timers ─────────────

    /// The pointer moved over the editor text (`None` = elsewhere): keeps the popup alive while the
    /// pointer is on its symbol or inside it, hides it shortly after it leaves, arms a new probe.
    pub fn hover_pointer(&mut self, p: Option<usize>) {
        let (x, y) = self.mouse;
        if let Some(h) = &self.hover {
            let inside = h.rect.contains(x, y);
            let on_symbol =
                p.is_some_and(|p| h.anchor.0 <= p && p <= h.anchor.1.max(h.anchor.0 + 1) && self.over_anchor_line(h));
            if inside || on_symbol {
                self.hv.hide_at = None;
                self.hover_probe = None;
                return;
            }
            if self.hv.hide_at.is_none() {
                self.hv.hide_at = Some(Instant::now() + HIDE_DELAY);
            }
        }
        match p {
            Some(p) => {
                let changed = self.hover_probe.is_none_or(|(_, q, _)| q != p);
                if changed {
                    self.hover_probe = Some((Instant::now(), p, false));
                }
            }
            None => self.hover_probe = None,
        }
    }

    pub(super) fn over_anchor_line(&self, h: &HoverTip) -> bool {
        let Some(d) = self.docs.get(self.active) else { return false };
        d.screen_pos(h.anchor.0).is_some_and(|(_, y)| {
            let lh = line_h() as f64;
            self.mouse.1 >= y as f64 && self.mouse.1 < y as f64 + lh
        })
    }

    pub fn hover_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if let Some(t) = self.hv.hide_at {
            if t <= now {
                self.hv.hide_at = None;
                self.hover = None;
                self.request_redraw();
            } else {
                want(t);
            }
        }
    }

    /// Mouse moved: true when the popup consumed it (dragging, or the pointer is over it).
    pub fn hover_mouse_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let Some(h) = &mut self.hover else { return false };
        if let Some(grab) = self.hv.drag_bar {
            if let (Some(l), Some(b)) = (h.laid.as_ref().map(|l| &l.1), h.bar) {
                h.scroll = b.pos_for(y, grab, l.h as f64, h.view.h as f64);
            }
            return true;
        }
        if let Some(anchor) = self.hv.drag_sel {
            let top = h.view.y as f64;
            let bot = h.view.bottom() as f64;
            if y < top {
                h.scroll = (h.scroll - 14.0).max(0.0);
            } else if y > bot {
                h.scroll += 14.0;
            }
            clamp_scroll(h);
            if let Some(head) = pos_at(h, &mut self.tx, x, y.clamp(top, bot - 1.0)) {
                h.sel = (head != anchor).then_some((anchor, head));
            }
            return true;
        }
        if !h.rect.contains(x, y) || self.dragging {
            if self.hv.hot.take().is_some() {
                self.request_redraw();
            }
            return false;
        }
        let hot = h.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone());
        if hot != self.hv.hot {
            self.hv.hot = hot;
            self.request_redraw();
        }
        self.hv.hide_at = None;
        self.hover_probe = None;
        true
    }

    pub fn hover_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let h = self.hover.as_ref()?;
        if !h.rect.contains(x, y) {
            return None;
        }
        if h.hits.iter().any(|(r, _)| r.contains(x, y)) || h.bar.is_some_and(|b| b.track.contains(x, y)) {
            return Some(CursorIcon::Pointer);
        }
        Some(if h.view.contains(x, y) { CursorIcon::Text } else { CursorIcon::Default })
    }

    /// Press: handled when inside the popup (links, actions, scrollbar, line selection); outside
    /// it closes the popup and lets the click through.
    pub fn hover_mouse_down(&mut self, button: MouseButton) -> bool {
        let (x, y) = self.mouse;
        let Some(h) = &mut self.hover else { return false };
        if !h.rect.contains(x, y) || button == MouseButton::Right {
            self.hover = None;
            return false;
        }
        if let Some(b) = h.bar.filter(|b| b.track.contains(x, y)) {
            let grab = if b.thumb.contains(x, y) { y - b.thumb.y as f64 } else { b.thumb.h as f64 / 2.0 };
            self.hv.drag_bar = Some(grab);
            if let Some(l) = h.laid.as_ref().map(|l| &l.1) {
                h.scroll = b.pos_for(y, grab, l.h as f64, h.view.h as f64);
            }
            return true;
        }
        if let Some((_, hit)) = h.hits.iter().find(|(r, _)| r.contains(x, y)).cloned() {
            self.hover_activate(hit);
            return true;
        }
        if h.view.contains(x, y) {
            // A press puts the caret between two chars; dragging extends the selection from there.
            h.sel = None;
            if let Some(pt) = pos_at(h, &mut self.tx, x, y) {
                self.hv.drag_sel = Some(pt);
            }
        }
        true
    }

    pub fn hover_mouse_up(&mut self) {
        self.hv.drag_bar = None;
        self.hv.drag_sel = None;
    }

    pub fn hover_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let w = self.wheel_in(delta);
        let Some(h) = &mut self.hover else { return false };
        if !h.rect.contains(x, y) {
            return false;
        }
        let d = if w.dy != 0.0 { w.dy } else { w.dx };
        clamp_scroll(h);
        let max = max_scroll(h);
        h.scr.adopt(h.scroll);
        if w.pixels {
            h.scr.pixels(d, max, scroller::now());
        } else {
            h.scr.wheel(d, 3.0 * PROSE_LH as f64, max, scroller::now());
        }
        h.scroll = h.scr.pos;
        true
    }

    /// Per-frame glide of the wheel scroll. True while the popup is still moving.
    pub fn hover_frame(&mut self, dt: f64) -> bool {
        let Some(h) = &mut self.hover else { return false };
        let max = max_scroll(h);
        h.scr.adopt(h.scroll);
        let moving = h.scr.step(dt, max);
        h.scroll = h.scr.pos;
        moving
    }

    /// Touchpad fingers lifted over the popup: coast.
    pub fn hover_wheel_end(&mut self, now: f64) -> bool {
        let (x, y) = self.mouse;
        let Some(h) = &mut self.hover else { return false };
        if !h.rect.contains(x, y) {
            return false;
        }
        h.scr.adopt(h.scroll);
        h.scr.fling(now);
        true
    }

    /// Esc closes; Ctrl+C copies the selected lines. Returns true when the key was used.
    pub fn hover_key(&mut self, ev: &KeyIn) -> bool {
        let ctrl = self.mods.control_key();
        let Some(h) = &mut self.hover else { return false };
        match &ev.key {
            Key::Named(NamedKey::Escape) => {
                self.hover = None;
                self.request_redraw();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                let Some(text) = sel_text(h, &mut self.tx) else { return false };
                self.clip.set(&text);
                self.say("copied");
                true
            }
            _ => false,
        }
    }

    pub(super) fn hover_activate(&mut self, hit: Hit) {
        match hit {
            Hit::Act(a) => self.hover_run(a),
            Hit::Link(u) => self.hover_open_link(&u),
        }
    }

    /// Run a footer action on the hovered symbol (the caret moves onto it first: the existing
    /// commands work on the caret).
    pub fn hover_run(&mut self, act: Act) {
        let Some(h) = self.hover.take() else { return };
        let target = if act == Act::QuickFix {
            h.extra
                .diags
                .first()
                .and_then(|d| self.docs.get(self.active).map(|doc| doc.buf.pos(d.line, d.col)))
                .unwrap_or(h.anchor.0)
        } else {
            h.anchor.0
        };
        if act == Act::CopyType {
            let text = h.type_text();
            self.clip.set(&text);
            self.say("type copied");
            return;
        }
        if let Some(d) = self.docs.get_mut(self.active) {
            let p = target.min(d.buf.len_chars());
            d.ed.click(&mut d.buf, p, false);
        }
        match act {
            Act::Definition => self.goto_definition(DefKind::Definition),
            Act::TypeDefinition => self.goto_definition(DefKind::TypeDefinition),
            Act::References => self.find_references(),
            Act::QuickFix => self.code_actions(),
            Act::CopyType => {}
        }
        self.request_redraw();
    }

    pub(super) fn hover_open_link(&mut self, url: &str) {
        let tip = self.hover.clone();
        if let Some(r) = url.strip_prefix("lumen:goto:") {
            let (p, frag) = r.rsplit_once('#').unwrap_or((r, "0,0"));
            let mut it = frag.split(',').filter_map(|v| v.parse::<usize>().ok());
            let (l, c) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
            self.hover = None;
            self.push_nav();
            self.open_path(std::path::Path::new(p), Some((l, c)));
        } else if url == "lumen:more" {
            self.hover_run(Act::Definition);
        } else if url == "lumen:more-type" {
            self.hover_run(Act::TypeDefinition);
        } else if let Some(rest) = url.strip_prefix("file://") {
            // `file:///x.ts#18,13-18,17`: 1-based line and column
            let (p, frag) = rest.split_once('#').unwrap_or((rest, ""));
            let nums: Vec<usize> = frag.split(|c: char| !c.is_ascii_digit()).filter_map(|v| v.parse().ok()).collect();
            let (l, c) = (
                nums.first().copied().unwrap_or(1).saturating_sub(1),
                nums.get(1).copied().unwrap_or(1).saturating_sub(1),
            );
            if let Some(path) = uri_to_path(&format!("file://{p}")) {
                self.hover = None;
                self.push_nav();
                self.open_path(&path, Some((l, c)));
            }
        } else if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
            open_external(url);
        }
        let _ = tip;
    }
}
