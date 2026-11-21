//! First-run onboarding: a short guided card (theme, keymap preset, font size / UI scale) shown
//! when no `config.json` exists yet, and again from the palette ("Help: Welcome tour"). Choices
//! apply live through `apply_setting`; Done stores them in `config.json`, Esc puts everything back
//! and stores the untouched settings so the tour does not return on its own.

use crate::{
    app::App,
    app_gitpanel::fit,
    ui::{Canvas, Rect},
};
use serde_json::json;
use std::path::Path;
use winit::keyboard::{Key, NamedKey};

const STEPS: usize = 3;
const ROW_H: i32 = 30;
const THEME_ROWS: usize = 6;
const SCALES: [(&str, f32); 5] = [("Auto", 0.0), ("100%", 1.0), ("125%", 1.25), ("150%", 1.5), ("200%", 2.0)];
const KEYMAPS: [(&str, &str, &str); 3] = [
    ("vscode", "VS Code", "The familiar defaults"),
    ("emacs", "Emacs", "Ctrl+F/B/N/P, kill and yank, Ctrl+X chords"),
    ("vim", "Vim", "Modal editing with a status pill"),
];
const FONT_MIN: f32 = 8.0;
const FONT_MAX: f32 = 32.0;

/// First launch: the settings file does not exist yet (no path to look at counts as not first).
pub fn is_first_run(path: Option<&Path>) -> bool {
    path.is_some_and(|p| !p.exists())
}

#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Theme(usize),
    Keymap(&'static str),
    Size(f32),
    Scale(f32),
    Back,
    Next,
    Done,
    Skip,
}

/// What the settings were before the tour (Esc puts them back).
struct Orig {
    theme: String,
    appearance: String,
    keymap: String,
    size: f32,
    scale: f32,
}

pub struct Onboarding {
    step: usize,
    names: Vec<String>,
    sel: usize,
    orig: Orig,
    rects: Vec<(Rect, Hit)>,
}

impl App {
    /// Start the tour (first launch or "Help: Welcome tour").
    pub fn onboarding_open(&mut self) {
        let names = self.theme_names();
        let sel = names.iter().position(|n| *n == self.theme.name).unwrap_or(0);
        let c = &self.config;
        let orig = Orig {
            theme: self.theme.name.clone(),
            appearance: c.appearance.clone(),
            keymap: c.keymap_preset.clone(),
            size: c.buffer_font_size,
            scale: c.ui_scale,
        };
        self.onboarding = Some(Box::new(Onboarding { step: 0, names, sel, orig, rects: Vec::new() }));
        self.request_redraw();
    }

    /// Close the tour. `keep` stores the choices; otherwise the original settings come back.
    fn onboarding_finish(&mut self, keep: bool) {
        let Some(st) = self.onboarding.take() else { return };
        if !keep {
            let o = st.orig;
            self.apply_setting("theme", &json!(o.theme));
            self.apply_setting("appearance", &json!(o.appearance));
            self.apply_setting("keymap_preset", &json!(o.keymap));
            self.apply_setting("buffer_font_size", &json!(o.size));
            self.apply_setting("ui_scale", &json!(o.scale));
        }
        // The path the settings modal stores through: writes config.json, so the tour is done.
        self.config.save();
        self.request_redraw();
    }

    fn onboarding_apply(&mut self, hit: Hit) {
        let Some(st) = self.onboarding.as_mut() else { return };
        match hit {
            Hit::Theme(i) => {
                st.sel = i;
                if let Some(n) = st.names.get(i).cloned() {
                    self.apply_setting("theme", &json!(n));
                }
            }
            Hit::Keymap(p) => self.apply_setting("keymap_preset", &json!(p)),
            Hit::Size(v) => self.apply_setting("buffer_font_size", &json!(v)),
            Hit::Scale(v) => self.apply_setting("ui_scale", &json!(v)),
            Hit::Back => st.step = st.step.saturating_sub(1),
            Hit::Next => st.step = (st.step + 1).min(STEPS - 1),
            Hit::Done => self.onboarding_finish(true),
            Hit::Skip => self.onboarding_finish(false),
        }
        self.request_redraw();
    }

    /// Mouse press: the card is modal, so it always consumes the click.
    pub fn onboarding_mouse(&mut self, x: f64, y: f64) -> bool {
        let Some(st) = &self.onboarding else { return false };
        let hit = st.rects.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
        if let Some(h) = hit {
            self.onboarding_apply(h);
        }
        true
    }

    /// `Some(over a button)` while the tour is open, `None` otherwise (for the pointer shape).
    pub fn onboarding_cursor_hand(&self) -> Option<bool> {
        let st = self.onboarding.as_ref()?;
        Some(st.rects.iter().any(|(r, _)| r.contains(self.mouse.0, self.mouse.1)))
    }

    /// Keys while the card is open: all of them are consumed.
    pub fn onboarding_key(&mut self, key: &Key) -> bool {
        let Some(st) = &self.onboarding else { return false };
        let (step, sel, last) = (st.step, st.sel, st.names.len().saturating_sub(1));
        let Key::Named(k) = key else { return true };
        let dir: i32 = match k {
            NamedKey::ArrowUp => -1,
            NamedKey::ArrowDown => 1,
            _ => 0,
        };
        match k {
            NamedKey::Escape => self.onboarding_finish(false),
            NamedKey::Enter => self.onboarding_apply(if step + 1 == STEPS { Hit::Done } else { Hit::Next }),
            NamedKey::ArrowLeft => self.onboarding_apply(Hit::Back),
            NamedKey::ArrowRight => self.onboarding_apply(Hit::Next),
            NamedKey::ArrowUp | NamedKey::ArrowDown => match step {
                0 => self.onboarding_apply(Hit::Theme((sel as i32 + dir).clamp(0, last as i32) as usize)),
                1 => {
                    let cur = KEYMAPS.iter().position(|m| m.0 == self.config.keymap_preset).unwrap_or(0);
                    let i = (cur as i32 + dir).clamp(0, KEYMAPS.len() as i32 - 1) as usize;
                    self.onboarding_apply(Hit::Keymap(KEYMAPS[i].0));
                }
                _ => {
                    let v = (self.config.buffer_font_size - dir as f32).clamp(FONT_MIN, FONT_MAX);
                    self.onboarding_apply(Hit::Size(v));
                }
            },
            _ => {}
        }
        true
    }

    fn onboarding_button(&mut self, cv: &mut Canvas, r: Rect, label: &str, primary: bool) {
        let c = self.theme.chrome;
        let hot = r.contains(self.mouse.0, self.mouse.1);
        if primary {
            cv.fill_round(r, 8, if hot { c.accent.shift(14) } else { c.accent });
        } else {
            cv.card(r, 8, if hot { c.list_hover_bg } else { c.widget_bg }, c.border);
        }
        let tw = self.tx.ui_width(label, primary);
        let fg = if primary { c.on_accent } else { self.theme.foreground };
        self.tx.draw_ui(cv, r.x + (r.w - tw) / 2, r.y + (r.h - 18) / 2, label, fg, r, primary);
    }

    pub fn draw_onboarding(&mut self, cv: &mut Canvas) {
        let Some(mut st) = self.onboarding.take() else { return };
        st.rects.clear();
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        cv.fill(Rect::new(0, 0, sw, sh), self.theme.background.with_alpha(0x90));
        let w = (sw - 24).clamp(260, 520);
        let h = (sh - 24).clamp(200, 340);
        let r = Rect::new((sw - w) / 2, ((sh - h) / 2).max(0), w, h);
        cv.shadow(r, 12);
        cv.card(r, 12, c.widget_bg, c.border);
        let inner = w - 40;
        let x0 = r.x + 20;

        self.tx.draw_ui(cv, x0, r.y + 16, "Welcome to Lumen", fg, r, true);
        let count = format!("Step {} of {STEPS}", st.step + 1);
        let cw = self.tx.ui_width(&count, false);
        self.tx.draw_ui(cv, r.right() - 20 - cw, r.y + 16, &count, dim, r, false);
        let (head, hint) = match st.step {
            0 => ("Pick a theme", "Up / Down to preview, click to choose"),
            1 => ("Choose a keymap", "You can change it later in Settings"),
            _ => ("Size things to your eyes", "Editor font size and interface scale"),
        };
        self.tx.draw_ui(cv, x0, r.y + 46, head, fg, r, true);
        let hint = fit(&mut self.tx, hint, inner, false);
        self.tx.draw_ui(cv, x0, r.y + 68, &hint, dim, r, false);

        let top = r.y + 98;
        let row_rect = |i: usize| Rect::new(x0 - 6, top + i as i32 * ROW_H, inner + 12, ROW_H - 2);
        let mark = |mouse: (f64, f64), cv: &mut Canvas, rr: Rect, on: bool| {
            if on {
                cv.fill_round(rr, 6, c.accent.with_alpha(0x30));
            } else if rr.contains(mouse.0, mouse.1) {
                cv.fill_round(rr, 6, c.list_hover_bg);
            }
        };
        match st.step {
            0 => {
                let start = st.sel.saturating_sub(2).min(st.names.len().saturating_sub(THEME_ROWS));
                for (k, i) in (start..st.names.len().min(start + THEME_ROWS)).enumerate() {
                    let rr = row_rect(k);
                    mark(self.mouse, cv, rr, i == st.sel);
                    let name = fit(&mut self.tx, &st.names[i], inner - 8, i == st.sel);
                    self.tx.draw_ui(cv, rr.x + 8, rr.y + 5, &name, fg, rr, i == st.sel);
                    st.rects.push((rr, Hit::Theme(i)));
                }
            }
            1 => {
                for (k, (id, label, desc)) in KEYMAPS.iter().enumerate() {
                    let rr = row_rect(k);
                    let on = self.config.keymap_preset == *id;
                    mark(self.mouse, cv, rr, on);
                    self.tx.draw_ui(cv, rr.x + 8, rr.y + 5, label, fg, rr, on);
                    let lw = self.tx.ui_width(label, on) + 20;
                    let d = fit(&mut self.tx, desc, (inner - lw - 8).max(10), false);
                    self.tx.draw_ui(cv, rr.x + 8 + lw, rr.y + 5, &d, dim, rr, false);
                    st.rects.push((rr, Hit::Keymap(*id)));
                }
            }
            _ => {
                self.tx.draw_ui(cv, x0, top + 6, "Editor font size", fg, r, false);
                let size = self.config.buffer_font_size;
                let plus = Rect::new(r.right() - 20 - 30, top, 30, 28);
                let minus = Rect::new(plus.x - 70 - 30, top, 30, 28);
                let val = format!("{size:.0}px");
                let vw = self.tx.ui_width(&val, true);
                self.tx.draw_ui(cv, minus.right() + (70 - vw) / 2, top + 5, &val, fg, r, true);
                self.onboarding_button(cv, minus, "-", false);
                self.onboarding_button(cv, plus, "+", false);
                st.rects.push((minus, Hit::Size((size - 1.0).max(FONT_MIN))));
                st.rects.push((plus, Hit::Size((size + 1.0).min(FONT_MAX))));
                self.tx.draw_ui(cv, x0, top + 56, "Interface scale", fg, r, false);
                let bw = (inner / SCALES.len() as i32).min(80);
                for (k, (label, v)) in SCALES.iter().enumerate() {
                    let b = Rect::new(x0 + k as i32 * (bw + 4), top + 82, bw, 28);
                    let on = (self.config.ui_scale - *v).abs() < 0.01;
                    self.onboarding_button(cv, b, label, on);
                    st.rects.push((b, Hit::Scale(*v)));
                }
            }
        }

        // Footer: Skip on the left, Back / Next (or Done) on the right.
        let by = r.bottom() - 16 - 30;
        let skip_w = self.tx.ui_width("Skip", false);
        let skip = Rect::new(x0, by, skip_w + 8, 30);
        let hot = skip.contains(self.mouse.0, self.mouse.1);
        self.tx.draw_ui(cv, skip.x + 4, by + 6, "Skip", if hot { fg } else { dim }, skip, false);
        st.rects.push((skip, Hit::Skip));
        let (nl, nh) = if st.step + 1 == STEPS { ("Done", Hit::Done) } else { ("Next", Hit::Next) };
        let nw = self.tx.ui_width(nl, true) + 30;
        let next = Rect::new(r.right() - 20 - nw, by, nw, 30);
        self.onboarding_button(cv, next, nl, true);
        st.rects.push((next, nh));
        if st.step > 0 {
            let bw = self.tx.ui_width("Back", false) + 30;
            let back = Rect::new(next.x - 8 - bw, by, bw, 30);
            self.onboarding_button(cv, back, "Back", false);
            st.rects.push((back, Hit::Back));
        }
        self.onboarding = Some(st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_means_no_config_file_yet() {
        let dir = std::env::temp_dir().join(format!("lumen-onb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        assert!(is_first_run(Some(&p)));
        std::fs::write(&p, "{}").unwrap();
        assert!(!is_first_run(Some(&p)));
        assert!(!is_first_run(None));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
