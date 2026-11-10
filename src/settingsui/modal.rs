use super::*;

// ───────────────────────────── modal shell ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Settings,
    Keybinds,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Hit {
    Close,
    Tab(Page),
    Section(usize),
    Search,
    SetRow(usize),     // def index
    Toggle(usize),     // def
    Seg(usize, usize), // def, option
    DropOpen(usize),   // def
    DropItem(usize),   // option
    Step(usize, i32),  // def, dir
    EditText(usize),   // def
    SetReset(usize),   // def
    KbRow(usize),      // row k
    KbReset(usize),    // row k
    KbResetAll,
    KbConfirm,
    KbCancel,
}

pub struct Modal {
    pub page: Page,
    pub settings: SettingsPage,
    pub keybinds: KeybindsPage,
    pub(super) hits: Vec<(Rect, Hit)>,
    pub(super) rect: Rect,
    /// The scrolling list of the page drawn last: (its rectangle, content height in px). The app
    /// paints and drags the thin scrollbar (`ui::VBar`) from this.
    pub(super) list: Option<(Rect, i32)>,
}

pub(super) const HEAD_H: i32 = 48;
pub(super) const SIDE_W: i32 = 176;
pub(super) const FOOT_H: i32 = 46;

impl Modal {
    pub fn new(settings: SettingsPage, keybinds: KeybindsPage, page: Page) -> Self {
        Self { page, settings, keybinds, hits: vec![], rect: Rect::default(), list: None }
    }

    /// (list area, content height, visible height, scroll) in px for the scrollbar; `None` when
    /// everything fits or a dropdown covers the list.
    pub fn bar_info(&self) -> Option<(Rect, f64, f64, f64)> {
        let (area, content) = self.list?;
        if self.page == Page::Settings && self.settings.dropdown_open() {
            return None;
        }
        let scroll = if self.page == Page::Settings { self.settings.scroll } else { self.keybinds.scroll };
        Some((area, content as f64, area.h as f64, scroll))
    }

    /// Scroll position (px) of the current page's list.
    pub fn scroll(&self) -> f64 {
        if self.page == Page::Settings { self.settings.scroll } else { self.keybinds.scroll }
    }

    /// Scrollbar drag: move the list of the current page to `v` px.
    pub fn set_scroll(&mut self, v: f64) {
        match self.page {
            Page::Settings => self.settings.scroll = v.clamp(0.0, self.settings.max_scroll()),
            Page::Keybinds => self.keybinds.scroll = v.clamp(0.0, self.keybinds.max_scroll()),
        }
    }

    /// Modal rectangle for a window: min(window-80, 860) x min(window-80, 560), centered.
    pub fn rect_for(window: Rect) -> Rect {
        let w = (window.w - 80).clamp(120, 860);
        let h = (window.h - 80).clamp(120, 560);
        Rect::new(window.x + (window.w - w) / 2, window.y + (window.h - h) / 2, w, h)
    }

    /// Rectangle of the last drawn frame.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn switch_page(&mut self) {
        self.settings.cancel();
        self.keybinds.cancel();
        self.page = if self.page == Page::Settings { Page::Keybinds } else { Page::Settings };
    }

    pub fn key(&mut self, key: &Key, text: Option<&str>, ctrl: bool, shift: bool, alt: bool) -> Event {
        if self.page == Page::Keybinds && self.keybinds.is_capturing() {
            return self.keybinds.record_key(ctrl, shift, alt, key);
        }
        let k = classify(key);
        if ctrl && k == K::Tab {
            self.switch_page();
            return Event::None;
        }
        if k == K::Esc {
            let handled = match self.page {
                Page::Settings => self.settings.cancel(),
                Page::Keybinds => self.keybinds.cancel(),
            };
            return if handled { Event::None } else { Event::Close };
        }
        if ctrl && matches!(&k, K::Ch(ch) if ch.eq_ignore_ascii_case("a")) {
            match self.page {
                Page::Settings => match self.settings.edit.as_mut() {
                    Some((_, f)) => f.select_all(),
                    None => self.settings.search.select_all(),
                },
                Page::Keybinds => self.keybinds.filter.select_all(),
            }
            return Event::None;
        }
        match self.page {
            Page::Settings => self.settings.key(&k, text, ctrl, shift, alt),
            Page::Keybinds => self.keybinds.key(&k, text, ctrl, shift, alt),
        }
    }

    /// Scroll by `dy` pixels (positive = the list scrolls down).
    pub fn wheel(&mut self, dy: f64) {
        match self.page {
            Page::Settings => self.settings.wheel(dy),
            Page::Keybinds => self.keybinds.wheel(dy),
        }
    }

    /// Click at window coordinates; uses the rectangles of the last `draw`.
    pub fn mouse_down(&mut self, x: f64, y: f64) -> Event {
        if !self.rect.contains(x, y) {
            return Event::Close;
        }
        let hit = self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
        // An open dropdown swallows any click that is not on one of its items.
        if self.page == Page::Settings && self.settings.dropdown_open() {
            if let Some(Hit::DropItem(o)) = hit {
                let def = self.settings.drop.as_ref().map(|d| d.def);
                self.settings.drop = None;
                return def.map_or(Event::None, |d| self.settings.pick(d, o));
            }
            self.settings.drop = None;
            return Event::None;
        }
        if self.page == Page::Settings && self.settings.is_editing() && !matches!(hit, Some(Hit::EditText(_))) {
            return self.settings.commit_edit();
        }
        if self.page == Page::Keybinds
            && self.keybinds.is_capturing()
            && !matches!(hit, Some(Hit::KbConfirm) | Some(Hit::KbCancel))
        {
            self.keybinds.cancel();
            if !matches!(hit, Some(Hit::KbRow(_))) {
                return Event::None;
            }
        }
        let Some(hit) = hit else { return Event::None };
        match hit {
            Hit::Close => Event::Close,
            Hit::Tab(p) => {
                if p != self.page {
                    self.switch_page();
                }
                Event::None
            }
            Hit::Search | Hit::DropItem(_) => Event::None,
            Hit::Section(i) => {
                self.settings.search = TextField::default();
                self.settings.set_section(i);
                Event::None
            }
            Hit::SetRow(d) => {
                self.settings.select_def(d);
                Event::None
            }
            Hit::Toggle(d) => {
                self.settings.select_def(d);
                self.settings.toggle(d)
            }
            Hit::Seg(d, o) => {
                self.settings.select_def(d);
                self.settings.pick(d, o)
            }
            Hit::DropOpen(d) => {
                self.settings.select_def(d);
                self.settings.open_drop(d);
                Event::None
            }
            Hit::Step(d, dir) => {
                self.settings.select_def(d);
                self.settings.step(d, dir)
            }
            Hit::EditText(d) => {
                self.settings.select_def(d);
                if !self.settings.is_editing() {
                    self.settings.start_edit(d);
                }
                Event::None
            }
            Hit::SetReset(d) => {
                self.settings.select_def(d);
                self.settings.reset_def(d)
            }
            Hit::KbRow(k) => {
                self.keybinds.select_row(k);
                self.keybinds.begin_record();
                Event::None
            }
            Hit::KbReset(k) => {
                self.keybinds.select_row(k);
                self.keybinds.reset_row(k)
            }
            Hit::KbResetAll => {
                self.keybinds.arm_reset_all();
                Event::None
            }
            Hit::KbConfirm => self.keybinds.confirm(),
            Hit::KbCancel => {
                self.keybinds.cancel();
                Event::None
            }
        }
    }

    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, window: Rect) {
        let c = theme.chrome;
        let fg = theme.foreground;
        let dim = c.tab_inactive_fg;
        let r = Self::rect_for(window);
        self.rect = r;
        self.hits.clear();
        cv.shadow(r, 14);
        cv.card(r, 14, c.widget_bg, c.border);

        // Header: page tabs + close button.
        let mut x = r.x + 18;
        for (page, icon, label) in
            [(Page::Settings, "settings", "Settings"), (Page::Keybinds, "keyboard", "Keyboard shortcuts")]
        {
            let active = self.page == page;
            let tw = tx.ui_width(label, active);
            let tab = Rect::new(x, r.y + 9, 16 + 8 + tw + 28, 30);
            if active {
                cv.fill_round(tab, 8, c.list_active_bg);
            }
            tx.icon(cv, &format!("hugeicons/{icon}"), tab.x + 12, tab.y + 7, 16, if active { fg } else { dim }, tab);
            tx.draw_ui(cv, tab.x + 36, tab.y + 6, label, if active { fg } else { dim }, tab, active);
            self.hits.push((tab, Hit::Tab(page)));
            x = tab.right() + 6;
        }
        let close = Rect::new(r.right() - 18 - 28, r.y + 10, 28, 28);
        tx.icon(cv, "hugeicons/close", close.x + 6, close.y + 6, 16, dim, close);
        self.hits.push((close, Hit::Close));
        let hint = "Ctrl+Tab switches";
        let hw = tx.ui_width(hint, false);
        if close.x - hw - 20 > x {
            tx.draw_ui(cv, close.x - hw - 12, r.y + 15, hint, dim.with_alpha(160), r, false);
        }
        cv.fill(Rect::new(r.x + 1, r.y + HEAD_H, r.w - 2, 1), c.border);

        let body = Rect::new(r.x, r.y + HEAD_H + 1, r.w, r.h - HEAD_H - 1);
        self.list = match self.page {
            Page::Settings => draw_settings(&mut self.settings, cv, tx, theme, body, &mut self.hits),
            Page::Keybinds => draw_keybinds(&mut self.keybinds, cv, tx, theme, body, &mut self.hits),
        };
    }
}
