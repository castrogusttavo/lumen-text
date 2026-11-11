//! Font family picker: a searchable list of the installed families, each row drawn in its own
//! font, with a free-text fallback ("use this name"). Opened from the Fonts settings and the
//! palette. The list comes from `fontcfg::installed_families` (computed once, on first use); a row's
//! face is only loaded when the row is drawn, and everything is dropped again when the picker closes.

use crate::{
    app::{App, KeyIn},
    app_overlay::Overlay,
    field::TextField,
    fontcfg::{self, Family},
    settingsui::Page,
    ui::{Canvas, Rect},
};
use serde_json::json;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, NamedKey};

pub const ROWS: usize = 9;
const ROW_H: i32 = 38;
const INPUT_H: i32 = 50;
const TOGGLE_H: i32 = 32;
const FOOT_H: i32 = 30;
/// Buffer picker only: a line of code in the selected family with the buffer font features and
/// letter spacing applied.
const PREVIEW_H: i32 = 48;
const PREVIEW_TEXT: &str = "0O o1lI| {[()]} => != -> <= === www";

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// Back to the system default family.
    Default,
    /// The typed text, used as a family name as it is.
    Custom(String),
    /// Index into `installed_families()`.
    Family(usize),
}

/// What a key or click asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    None,
    Close,
    /// Store this family ("" = default) and close.
    Pick(String),
}

pub struct FontPicker {
    /// Settings key the choice is stored under: `buffer_font_family` or `ui_font_family`.
    pub key: &'static str,
    pub field: TextField,
    /// Show proportional fonts too (the buffer picker lists monospace ones by default).
    pub show_all: bool,
    pub rows: Vec<Row>,
    pub sel: usize,
    pub scroll: usize,
    /// The family in use now (marked in the list).
    pub current: String,
    /// Settings section to reopen after closing, when the picker was opened from the settings.
    pub back_to: Option<usize>,
}

impl FontPicker {
    pub fn new(key: &'static str, current: &str, families: &[Family]) -> Self {
        let mut p = Self {
            key,
            field: TextField::default(),
            show_all: key == "ui_font_family",
            rows: Vec::new(),
            sel: 0,
            scroll: 0,
            current: current.to_string(),
            back_to: None,
        };
        p.refilter(families);
        // Start on the family in use.
        if let Some(i) =
            p.rows.iter().position(|r| matches!(r, Row::Family(f) if families[*f].name.eq_ignore_ascii_case(current)))
        {
            p.sel = i;
            p.scroll = i.saturating_sub(ROWS / 2);
        }
        p
    }

    pub fn is_buffer(&self) -> bool {
        self.key == "buffer_font_family"
    }

    /// Rebuild `rows` for the current query and filter, keeping the selection on the same entry.
    pub fn refilter(&mut self, families: &[Family]) {
        let q = self.field.text.trim().to_string();
        let mut rows = Vec::new();
        if q.is_empty() {
            rows.push(Row::Default);
        }
        let exact = families.iter().any(|f| f.name.eq_ignore_ascii_case(&q));
        if !q.is_empty() && !exact {
            rows.push(Row::Custom(fontcfg::clean_family(&q)));
        }
        for (i, f) in families.iter().enumerate() {
            if (self.show_all || f.mono) && fontcfg::matches_query(&f.name, &q) {
                rows.push(Row::Family(i));
            }
        }
        // A query that matches nothing among monospace fonts looks at every family before giving up.
        if rows.iter().all(|r| !matches!(r, Row::Family(_))) && !self.show_all && !q.is_empty() {
            for (i, f) in families.iter().enumerate() {
                if fontcfg::matches_query(&f.name, &q) {
                    rows.push(Row::Family(i));
                }
            }
        }
        self.rows = rows;
        self.sel = 0;
        self.scroll = 0;
        // With a typed name that matches exactly or by prefix, start on that family instead of the
        // "use this name" row.
        if !q.is_empty()
            && let Some(i) = self.rows.iter().position(|r| matches!(r, Row::Family(_)))
            && exact
        {
            self.sel = i;
        }
    }

    pub fn move_sel(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.sel = (self.sel as isize + d).clamp(0, self.rows.len() as isize - 1) as usize;
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + ROWS {
            self.scroll = self.sel + 1 - ROWS;
        }
    }

    pub fn scroll_by(&mut self, d: isize) {
        let max = self.rows.len().saturating_sub(ROWS) as isize;
        self.scroll = (self.scroll as isize + d).clamp(0, max) as usize;
    }

    /// The family a row stands for ("" = system default).
    pub fn value_of(&self, row: usize, families: &[Family]) -> Option<String> {
        Some(match self.rows.get(row)? {
            Row::Default => String::new(),
            Row::Custom(s) => s.clone(),
            Row::Family(i) => families[*i].name.clone(),
        })
    }

    pub fn toggle_all(&mut self, families: &[Family]) {
        self.show_all = !self.show_all;
        self.refilter(families);
    }

    /// Editing keys. `ctrl`/`alt` as the user holds them; `text` is what the key types.
    pub fn key(&mut self, key: &Key, text: Option<&str>, ctrl: bool, alt: bool, families: &[Family]) -> Outcome {
        match key {
            Key::Named(NamedKey::Escape) => return Outcome::Close,
            Key::Named(NamedKey::Enter) => {
                return match self.value_of(self.sel, families) {
                    Some(v) => Outcome::Pick(v),
                    None => Outcome::Close,
                };
            }
            Key::Named(NamedKey::ArrowDown) => self.move_sel(1),
            Key::Named(NamedKey::ArrowUp) => self.move_sel(-1),
            Key::Named(NamedKey::PageDown) => self.move_sel(ROWS as isize),
            Key::Named(NamedKey::PageUp) => self.move_sel(-(ROWS as isize)),
            Key::Named(NamedKey::Tab) => self.move_sel(1),
            Key::Named(NamedKey::ArrowLeft) => self.field.left(),
            Key::Named(NamedKey::ArrowRight) => self.field.right(),
            Key::Named(NamedKey::Home) => self.field.home(),
            Key::Named(NamedKey::End) => self.field.end(),
            Key::Named(NamedKey::Backspace) => {
                let ch = if ctrl { self.field.delete_word() } else { self.field.backspace() };
                if ch {
                    self.refilter(families);
                }
            }
            Key::Named(NamedKey::Delete) => {
                if self.field.delete() {
                    self.refilter(families);
                }
            }
            Key::Character(c) if alt && c.eq_ignore_ascii_case("a") => self.toggle_all(families),
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => self.field.select_all(),
            _ if !ctrl && !alt => {
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    self.field.insert(t);
                    self.refilter(families);
                }
            }
            _ => {}
        }
        Outcome::None
    }
}

impl App {
    /// Open the picker for `key` (`buffer_font_family` or `ui_font_family`). `from_settings` makes
    /// closing it reopen the settings on the section it came from.
    pub fn open_font_picker(&mut self, key: &'static str, from_settings: bool) {
        let current = if key == "ui_font_family" {
            self.config.ui_font_family.clone()
        } else {
            self.config.buffer_font_family.clone()
        };
        let back_to = match (&self.overlay, from_settings) {
            (Overlay::Settings(m), true) => Some(m.settings.section),
            _ => None,
        };
        let mut p = FontPicker::new(key, &current, fontcfg::installed_families());
        p.back_to = back_to;
        self.overlay = Overlay::Font(Box::new(p));
        self.request_redraw();
    }

    fn close_font_picker(&mut self) {
        self.tx.clear_previews();
        let back = match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Font(p) => p.back_to,
            _ => None,
        };
        if let Some(section) = back {
            self.open_settings(Page::Settings);
            if let Overlay::Settings(m) = &mut self.overlay {
                m.settings.set_section(section);
            }
        }
        self.request_redraw();
    }

    fn font_pick(&mut self, family: String) {
        let key = match &self.overlay {
            Overlay::Font(p) => p.key,
            _ => return,
        };
        self.apply_setting(key, &json!(family));
        self.close_font_picker();
    }

    pub fn fontpicker_key(&mut self, ev: &KeyIn) {
        let (ctrl, alt) = (self.mods.control_key(), self.mods.alt_key());
        let paste = ctrl && matches!(&ev.key, Key::Character(c) if c.eq_ignore_ascii_case("v"));
        let clip = if paste { Some(self.clip.get().replace('\n', " ")) } else { None };
        let Overlay::Font(p) = &mut self.overlay else { return };
        let fams = fontcfg::installed_families();
        let out = match clip {
            Some(t) => {
                p.field.insert(&t);
                p.refilter(fams);
                Outcome::None
            }
            None => p.key(&ev.key, ev.text.as_deref(), ctrl, alt, fams),
        };
        match out {
            Outcome::None => {}
            Outcome::Close => self.close_font_picker(),
            Outcome::Pick(f) => self.font_pick(f),
        }
        self.request_redraw();
    }

    fn fontpicker_rect(&self) -> Rect {
        let w = self.size.0 as i32;
        let pw = (w - 40).clamp(300, 560);
        let extra = match &self.overlay {
            Overlay::Font(p) if p.is_buffer() => TOGGLE_H + PREVIEW_H,
            _ => 0,
        };
        Rect::new((w - pw) / 2, 70, pw, INPUT_H + extra + ROWS as i32 * ROW_H + FOOT_H + 12)
    }

    /// Geometry of the list: its top-left y and the toggle row (buffer picker only).
    fn fontpicker_parts(&self) -> (Rect, Option<Rect>, Rect) {
        let r = self.fontpicker_rect();
        let buffer = matches!(&self.overlay, Overlay::Font(p) if p.is_buffer());
        let toggle = buffer.then(|| Rect::new(r.x + 8, r.y + INPUT_H, r.w - 16, TOGGLE_H));
        let top = r.y + INPUT_H + if buffer { TOGGLE_H } else { 0 } + 6;
        (r, toggle, Rect::new(r.x, top, r.w, ROWS as i32 * ROW_H))
    }

    pub fn fontpicker_mouse(&mut self, x: f64, y: f64) -> bool {
        let (r, toggle, list) = self.fontpicker_parts();
        if !r.contains(x, y) {
            self.close_font_picker();
            return true;
        }
        let fams = fontcfg::installed_families();
        if toggle.is_some_and(|t| t.contains(x, y)) {
            if let Overlay::Font(p) = &mut self.overlay {
                p.toggle_all(fams);
            }
            self.request_redraw();
            return true;
        }
        if list.contains(x, y) {
            let pick = match &mut self.overlay {
                Overlay::Font(p) => {
                    let row = p.scroll + ((y - list.y as f64) / ROW_H as f64) as usize;
                    p.value_of(row, fams)
                }
                _ => None,
            };
            if let Some(v) = pick {
                self.font_pick(v);
            }
        }
        true
    }

    pub fn fontpicker_wheel(&mut self, delta: MouseScrollDelta) {
        let d = crate::scroller::wheel_rows_int(delta, ROW_H as f64);
        if let Overlay::Font(p) = &mut self.overlay {
            p.scroll_by(d);
        }
        self.request_redraw();
    }

    pub fn draw_fontpicker(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let (r, toggle, list) = self.fontpicker_parts();
        let fams = fontcfg::installed_families();
        // Take what the drawing needs so `self.tx` can be borrowed while drawing.
        let (rows, scroll, sel, text, cursor, sel_all, show_all, is_buffer, current) = match &self.overlay {
            Overlay::Font(p) => {
                let vis: Vec<Row> = p.rows.iter().skip(p.scroll).take(ROWS).cloned().collect();
                (
                    vis,
                    p.scroll,
                    p.sel,
                    p.field.text.clone(),
                    p.field.cursor,
                    p.field.sel_all,
                    p.show_all,
                    p.is_buffer(),
                    p.current.clone(),
                )
            }
            _ => return,
        };
        let total = match &self.overlay {
            Overlay::Font(p) => p.rows.len(),
            _ => 0,
        };
        cv.shadow(r, 12);
        cv.card(r, 12, c.widget_bg, c.border);
        let title = if is_buffer { "Buffer font family" } else { "Interface font family" };
        self.tx.icon(cv, "ui/font", r.x + 18, r.y + (INPUT_H - 16) / 2, 16, dim, r);
        let (shown, col) =
            if text.is_empty() { (format!("{title}: search or type a name"), dim) } else { (text.clone(), fg) };
        if sel_all {
            let w = self.tx.ui_width(&text, false);
            cv.fill_round(Rect::new(r.x + 44, r.y + (INPUT_H - 24) / 2, w + 4, 24), 4, c.accent.with_alpha(0x50));
        }
        self.tx.draw_ui(cv, r.x + 46, r.y + (INPUT_H - 18) / 2, &shown, col, r, false);
        let prefix: String = text.chars().take(cursor).collect();
        let cx = r.x + 46 + self.tx.ui_width(&prefix, false);
        cv.fill_clip(Rect::new(cx, r.y + (INPUT_H - 20) / 2, 1, 20), fg, r);
        cv.fill(Rect::new(r.x + 16, r.y + INPUT_H - 1, r.w - 32, 1), c.border);
        if let Some(t) = toggle {
            let b = Rect::new(t.x + 8, t.y + (TOGGLE_H - 14) / 2, 14, 14);
            cv.card(b, 4, if show_all { c.accent } else { c.input_bg }, if show_all { c.accent } else { c.border });
            if show_all {
                self.tx.icon(cv, "ui/check", b.x + 1, b.y + 1, 12, c.on_accent, b);
            }
            self.tx.draw_ui(cv, b.right() + 8, t.y + (TOGGLE_H - 18) / 2, "Show all fonts (Alt+A)", dim, t, false);
        }
        for (k, row) in rows.iter().enumerate() {
            let y = list.y + k as i32 * ROW_H;
            let rr = Rect::new(r.x + 8, y, r.w - 16, ROW_H);
            if scroll + k == sel {
                cv.fill_round_clip(rr, 10, c.list_active_bg, list);
            }
            let ty = y + (ROW_H - 20) / 2;
            match row {
                Row::Default => {
                    let d = if is_buffer { "System monospace (default)" } else { "System sans-serif (default)" };
                    self.tx.draw_ui(cv, rr.x + 14, ty, d, fg, rr, false);
                    if current.is_empty() {
                        self.tx.icon(cv, "ui/check", rr.right() - 28, y + (ROW_H - 14) / 2, 14, c.accent_text, rr);
                    }
                }
                Row::Custom(name) => {
                    self.tx.draw_ui(cv, rr.x + 14, ty, &format!("Use \u{201c}{name}\u{201d}"), fg, rr, false);
                    self.tx.draw_ui(cv, rr.right() - 120, ty, "custom name", dim, rr, false);
                }
                Row::Family(i) => {
                    let f = &fams[*i];
                    let ok = self.tx.preview_ready(&f.name, &f.file);
                    if ok {
                        self.tx.draw_family(cv, rr.x + 14, ty - 1, &f.name, &f.name, fg, rr, 16.0);
                    } else {
                        self.tx.draw_ui(cv, rr.x + 14, ty, &f.name, fg, rr, false);
                    }
                    if f.mono {
                        let tag = "mono";
                        let tw = self.tx.ui_width(tag, false);
                        self.tx.draw_ui(cv, rr.right() - tw - 40, ty, tag, dim, rr, false);
                    }
                    if f.name.eq_ignore_ascii_case(&current) {
                        self.tx.icon(cv, "ui/check", rr.right() - 28, y + (ROW_H - 14) / 2, 14, c.accent_text, rr);
                    }
                }
            }
        }
        if total == 0 {
            self.tx.draw_ui(cv, r.x + 16, list.y + 8, "No fonts found", dim, r, false);
        }
        if is_buffer {
            // The selected family (else the one in use) as code, features and spacing applied.
            let family = match rows.get(sel.wrapping_sub(scroll)) {
                Some(Row::Family(i)) => Some(&fams[*i]),
                _ => fams.iter().find(|f| f.name.eq_ignore_ascii_case(&current)),
            };
            let pr = Rect::new(r.x + 8, list.bottom() + 3, r.w - 16, PREVIEW_H);
            cv.fill(Rect::new(pr.x + 8, pr.y, pr.w - 16, 1), c.border);
            let v = &self.config.view;
            let mut how = crate::fontcfg::features_text(&v.buffer_font_features);
            if v.buffer_letter_spacing != 0.0 {
                how = format!("{how}  spacing {} px", v.buffer_letter_spacing).trim().to_string();
            }
            if how.is_empty() {
                how = "no font features".into();
            }
            let hw = self.tx.ui_width(&how, false);
            self.tx.draw_ui(cv, pr.right() - hw - 8, pr.y + 4, &how, dim, pr, false);
            match family {
                Some(f) if self.tx.preview_ready(&f.name, &f.file) => {
                    let clip = Rect::new(pr.x, pr.y, pr.w - hw - 24, pr.h);
                    self.tx.draw_family_buffer(cv, pr.x + 14, pr.y + 18, &f.name, PREVIEW_TEXT, fg, clip, 16.0);
                }
                _ => self.tx.draw_ui(cv, pr.x + 14, pr.y + 20, "Pick a family to preview it", dim, pr, false),
            }
        }
        let foot = format!("{total} fonts   Enter picks   Esc closes");
        self.tx.draw_ui(cv, r.x + 16, r.bottom() - FOOT_H + 4, &foot, dim, r, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fams() -> Vec<Family> {
        let f = |n: &str, mono: bool| Family { name: n.into(), mono, file: PathBuf::from("/x.ttf") };
        vec![f("Fira Code", true), f("Inter", false), f("JetBrains Mono", true), f("Noto Serif", false)]
    }

    fn type_text(p: &mut FontPicker, s: &str, f: &[Family]) {
        for ch in s.chars() {
            p.key(&Key::Character(ch.to_string().into()), Some(&ch.to_string()), false, false, f);
        }
    }

    #[test]
    fn buffer_picker_lists_monospace_first_and_show_all_adds_the_rest() {
        let f = fams();
        let mut p = FontPicker::new("buffer_font_family", "", &f);
        assert_eq!(p.rows, [Row::Default, Row::Family(0), Row::Family(2)]);
        p.toggle_all(&f);
        assert_eq!(p.rows.len(), 5, "default + four families");
        let u = FontPicker::new("ui_font_family", "", &f);
        assert_eq!(u.rows.len(), 5, "the UI picker shows everything");
    }

    #[test]
    fn typing_filters_and_offers_the_typed_name() {
        let f = fams();
        let mut p = FontPicker::new("buffer_font_family", "", &f);
        type_text(&mut p, "jet", &f);
        assert_eq!(p.rows, [Row::Custom("jet".into()), Row::Family(2)]);
        let out = p.key(&Key::Named(NamedKey::ArrowDown), None, false, false, &f);
        assert_eq!(out, Outcome::None);
        assert_eq!(p.key(&Key::Named(NamedKey::Enter), None, false, false, &f), Outcome::Pick("JetBrains Mono".into()));
        // Typing a full name selects that family directly.
        let mut p = FontPicker::new("buffer_font_family", "", &f);
        type_text(&mut p, "Fira Code", &f);
        assert_eq!(p.value_of(p.sel, &f), Some("Fira Code".into()));
        // A name nobody has is accepted as typed.
        let mut p = FontPicker::new("buffer_font_family", "", &f);
        type_text(&mut p, "My Font", &f);
        assert_eq!(p.key(&Key::Named(NamedKey::Enter), None, false, false, &f), Outcome::Pick("My Font".into()));
    }

    #[test]
    fn a_query_with_no_monospace_hit_looks_at_every_family() {
        let f = fams();
        let mut p = FontPicker::new("buffer_font_family", "", &f);
        type_text(&mut p, "inter", &f);
        assert!(p.rows.contains(&Row::Family(1)), "{:?}", p.rows);
    }

    #[test]
    fn default_row_and_escape() {
        let f = fams();
        let mut p = FontPicker::new("ui_font_family", "", &f);
        assert_eq!(p.key(&Key::Named(NamedKey::Enter), None, false, false, &f), Outcome::Pick(String::new()));
        assert_eq!(p.key(&Key::Named(NamedKey::Escape), None, false, false, &f), Outcome::Close);
    }

    #[test]
    fn opens_on_the_current_family_and_scrolls_with_the_selection() {
        let f: Vec<Family> = (0..40)
            .map(|i| Family { name: format!("Mono {i:02}"), mono: true, file: PathBuf::from("/x.ttf") })
            .collect();
        let p = FontPicker::new("buffer_font_family", "mono 30", &f);
        assert_eq!(p.value_of(p.sel, &f), Some("Mono 30".into()));
        assert!(p.sel >= p.scroll && p.sel < p.scroll + ROWS);
        let mut p = FontPicker::new("buffer_font_family", "", &f);
        for _ in 0..20 {
            p.move_sel(1);
        }
        assert!(p.sel >= p.scroll && p.sel < p.scroll + ROWS, "selection {} scroll {}", p.sel, p.scroll);
        p.scroll_by(1000);
        assert_eq!(p.scroll, p.rows.len() - ROWS);
    }
}
