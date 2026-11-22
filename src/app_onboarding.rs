//! First-run onboarding: a five-step, full-window flow (welcome, theme with a live preview,
//! keymap, font size / UI scale, open a project) shown when no `config.json` exists yet, and again
//! from the palette ("Help: Welcome tour"). Choices apply live through `apply_setting`; finishing
//! stores them in `config.json`, Esc / Skip puts everything back and stores the untouched settings
//! so the tour does not return on its own. Drawing lives in `app_onboarding_draw`.

use crate::{
    anim::{Anim, dur},
    app::App,
    app_overlay::Cmd,
    ui::Rect,
};
use serde_json::json;
use std::{
    cell::Cell,
    path::{Path, PathBuf},
};
use winit::keyboard::{Key, NamedKey};

pub(crate) const STEPS: usize = 5;
pub(crate) const SCALES: [(&str, f32); 5] =
    [("Auto", 0.0), ("100%", 1.0), ("125%", 1.25), ("150%", 1.5), ("200%", 2.0)];
pub(crate) const KEYMAPS: [(&str, &str, &str, &str); 3] = [
    ("vscode", "VS Code", "The familiar defaults", "ui/editor_vs_code"),
    ("emacs", "Emacs", "Ctrl+F/B/N/P, kill and yank, Ctrl+X chords", "ui/editor_emacs"),
    ("vim", "Vim", "Modal editing with a status pill", "ui/editor_cursor"),
];
pub(crate) const FONT_MIN: f32 = 8.0;
pub(crate) const FONT_MAX: f32 = 32.0;
/// Recent projects offered on the last step.
const RECENT_MAX: usize = 3;
/// Seconds the opening fade and the first staggered entrance take.
const ENTER_SECS: f32 = 1.0;
/// Rows of the theme list before the window height is known (first frame).
const THEME_ROWS_GUESS: usize = 6;

/// First launch: the settings file does not exist yet (no path to look at counts as not first).
pub fn is_first_run(path: Option<&Path>) -> bool {
    path.is_some_and(|p| !p.exists())
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hit {
    Theme(usize),
    Keymap(&'static str),
    Size(f32),
    Scale(f32),
    Goto(usize),
    Back,
    Next,
    Done,
    Skip,
    OpenFolder,
    Recent(usize),
}

impl Hit {
    /// Rows that get the gliding hover highlight (buttons brighten instead).
    pub(crate) fn glides(&self) -> bool {
        matches!(self, Hit::Theme(_) | Hit::Keymap(_) | Hit::Recent(_) | Hit::OpenFolder)
    }
}

/// What the settings were before the tour (Esc puts them back).
struct Orig {
    theme: String,
    appearance: String,
    keymap: String,
    size: f32,
    scale: f32,
}

/// A hover highlight that glides from row to row and fades out when the pointer leaves.
pub(crate) struct Glide {
    r: [Anim; 4],
    a: Anim,
}

impl Glide {
    fn new() -> Self {
        let m = || Anim::new(0.0, dur::BASE);
        Self { r: [m(), m(), m(), m()], a: Anim::new(0.0, dur::FAST) }
    }

    pub(crate) fn aim(&mut self, t: Option<Rect>) {
        match t {
            Some(t) => {
                let v = [t.x, t.y, t.w, t.h].map(|v| v as f32);
                let hidden = self.a.value < 0.05;
                for (a, v) in self.r.iter_mut().zip(v) {
                    if hidden {
                        a.snap(v);
                    } else {
                        a.to(v);
                    }
                }
                self.a.to(1.0);
            }
            None => self.a.to(0.0),
        }
    }

    fn step(&mut self, dt: f64) -> bool {
        let mut b = self.a.step(dt);
        for a in &mut self.r {
            b |= a.step(dt);
        }
        b
    }

    /// Current rect and opacity while visible.
    pub(crate) fn now(&self) -> Option<(Rect, f32)> {
        (self.a.value > 0.01).then(|| {
            let [x, y, w, h] = self.r.each_ref().map(|a| a.value.round() as i32);
            (Rect::new(x, y, w, h), self.a.value.clamp(0.0, 1.0))
        })
    }
}

pub struct Onboarding {
    pub(crate) step: usize,
    pub(crate) names: Vec<String>,
    pub(crate) sel: usize,
    orig: Orig,
    pub(crate) rects: Vec<(Rect, Hit)>,
    /// Fractional step shown (slides between integers).
    pub(crate) slide: Anim,
    /// Selection highlight position (row index) of the theme, keymap and last-step lists.
    pub(crate) selg: [Anim; 3],
    /// First visible theme row (fractional while the list scrolls).
    pub(crate) scroll: Anim,
    pub(crate) glide: Glide,
    /// Seconds since the tour opened (drives the first staggered entrance).
    pub(crate) t: f32,
    /// Font/scale step: 0 = font size, 1 = interface scale.
    pub(crate) focus: usize,
    /// Last step: the highlighted choice (0 = open a folder, then recents, then start empty).
    pub(crate) pick: usize,
    pub(crate) recents: Vec<PathBuf>,
    hover: Cell<Option<usize>>,
}

/// First row of the visible window of a scrolling list that keeps `sel` in view.
pub(crate) fn theme_window(sel: usize, len: usize, rows: usize) -> usize {
    sel.saturating_sub(2).min(len.saturating_sub(rows))
}

/// The preset next to `cur` in `dir` (-1 / +1): from the closest one when `cur` is not a preset.
pub(crate) fn next_scale(cur: f32, dir: i32) -> f32 {
    let at = SCALES
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1.1 - cur).abs().total_cmp(&(b.1.1 - cur).abs()))
        .map_or(0, |(i, _)| i);
    SCALES[(at as i32 + dir).clamp(0, SCALES.len() as i32 - 1) as usize].1
}

impl Onboarding {
    fn new(names: Vec<String>, sel: usize, orig: Orig, recents: Vec<PathBuf>) -> Self {
        Self {
            step: 0,
            names,
            sel,
            orig,
            rects: Vec::new(),
            slide: Anim::new(0.0, dur::STAGE),
            selg: [Anim::new(0.0, dur::BASE); 3],
            scroll: Anim::new(0.0, dur::BASE),
            glide: Glide::new(),
            t: 0.0,
            focus: 0,
            pick: 0,
            recents,
            hover: Cell::new(None),
        }
    }

    /// Number of choices on the last step.
    pub(crate) fn pick_count(&self) -> usize {
        2 + self.recents.len()
    }

    fn pick_hit(&self, i: usize) -> Hit {
        if i == 0 {
            Hit::OpenFolder
        } else if i <= self.recents.len() {
            Hit::Recent(i - 1)
        } else {
            Hit::Done
        }
    }

    fn go(&mut self, step: usize) {
        self.step = step.min(STEPS - 1);
        self.slide.to(self.step as f32);
        self.focus = 0;
    }

    /// Aim the selection highlights (`snap` jumps instead of gliding: first frame).
    pub(crate) fn retarget(&mut self, keymap: &str, rows: usize, snap: bool) {
        let km = KEYMAPS.iter().position(|m| m.0 == keymap).unwrap_or(0);
        let win = theme_window(self.sel, self.names.len(), rows);
        let goals = [self.sel as f32, km as f32, self.pick as f32];
        for (a, g) in self.selg.iter_mut().zip(goals) {
            if snap {
                a.snap(g);
            } else {
                a.to(g);
            }
        }
        if snap {
            self.scroll.snap(win as f32);
        } else {
            self.scroll.to(win as f32);
        }
    }

    /// Advance every animation; true while something still moves.
    pub(crate) fn advance(&mut self, dt: f64) -> bool {
        self.t += dt as f32;
        let mut b = self.t < ENTER_SECS;
        b |= self.slide.step(dt);
        for a in &mut self.selg {
            b |= a.step(dt);
        }
        b |= self.scroll.step(dt);
        b |= self.glide.step(dt);
        b
    }
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
        let recents = crate::app_welcome::recent_projects(&self.root, RECENT_MAX);
        let mut st = Onboarding::new(names, sel, orig, recents);
        st.retarget(&self.config.keymap_preset, THEME_ROWS_GUESS, true);
        self.onboarding = Some(Box::new(st));
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
            Hit::Size(v) => {
                st.focus = 0;
                self.apply_setting("buffer_font_size", &json!(v));
            }
            Hit::Scale(v) => {
                st.focus = 1;
                self.apply_setting("ui_scale", &json!(v));
            }
            Hit::Goto(i) => st.go(i),
            Hit::Back => st.go(st.step.saturating_sub(1)),
            Hit::Next => st.go(st.step + 1),
            Hit::Done => self.onboarding_finish(true),
            Hit::Skip => self.onboarding_finish(false),
            Hit::OpenFolder => {
                self.onboarding_finish(true);
                self.run_cmd(Cmd::ProjectMenu);
            }
            Hit::Recent(i) => {
                let p = st.recents.get(i).cloned();
                self.onboarding_finish(true);
                if let Some(p) = p {
                    self.switch_project(p);
                }
            }
        }
        self.request_redraw();
    }

    /// Mouse press: the flow is modal, so it always consumes the click.
    pub fn onboarding_mouse(&mut self, x: f64, y: f64) -> bool {
        let Some(st) = &self.onboarding else { return false };
        let hit = st.rects.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
        if let Some(h) = hit {
            self.onboarding_apply(h);
        }
        true
    }

    /// `Some(over a control)` while the tour is open, `None` otherwise (for the pointer shape).
    /// A change of the hovered control also asks for a frame, so the highlight follows the pointer.
    pub fn onboarding_cursor_hand(&self) -> Option<bool> {
        let st = self.onboarding.as_ref()?;
        let h = st.rects.iter().position(|(r, _)| r.contains(self.mouse.0, self.mouse.1));
        if st.hover.get() != h {
            st.hover.set(h);
            self.request_redraw();
        }
        Some(h.is_some())
    }

    /// Keys while the flow is open: all of them are consumed.
    pub fn onboarding_key(&mut self, key: &Key) -> bool {
        let Some(st) = &self.onboarding else { return false };
        let (step, sel, last) = (st.step, st.sel, st.names.len().saturating_sub(1));
        let (focus, pick, picks) = (st.focus, st.pick, st.pick_count());
        let enter = st.pick_hit(pick);
        if let Key::Character(s) = key {
            let n = s.as_str().parse::<usize>().ok().filter(|n| (1..=KEYMAPS.len()).contains(n));
            if let (2, Some(n)) = (step, n) {
                self.onboarding_apply(Hit::Keymap(KEYMAPS[n - 1].0));
            }
            return true;
        }
        let Key::Named(k) = key else { return true };
        let dir: i32 = match k {
            NamedKey::ArrowUp => -1,
            NamedKey::ArrowDown => 1,
            _ => 0,
        };
        match k {
            NamedKey::Escape => self.onboarding_finish(false),
            NamedKey::Enter => self.onboarding_apply(if step + 1 < STEPS { Hit::Next } else { enter }),
            NamedKey::ArrowLeft => self.onboarding_apply(Hit::Back),
            NamedKey::ArrowRight if step + 1 < STEPS => self.onboarding_apply(Hit::Next),
            NamedKey::Tab => {
                if let Some(st) = self.onboarding.as_mut().filter(|st| st.step == 3) {
                    st.focus ^= 1;
                }
            }
            NamedKey::ArrowUp | NamedKey::ArrowDown => match step {
                1 => self.onboarding_apply(Hit::Theme((sel as i32 + dir).clamp(0, last as i32) as usize)),
                2 => {
                    let cur = KEYMAPS.iter().position(|m| m.0 == self.config.keymap_preset).unwrap_or(0);
                    let i = (cur as i32 + dir).clamp(0, KEYMAPS.len() as i32 - 1) as usize;
                    self.onboarding_apply(Hit::Keymap(KEYMAPS[i].0));
                }
                3 if focus == 1 => self.onboarding_apply(Hit::Scale(next_scale(self.config.ui_scale, dir))),
                3 => {
                    let v = (self.config.buffer_font_size - dir as f32).clamp(FONT_MIN, FONT_MAX);
                    self.onboarding_apply(Hit::Size(v));
                }
                4 => {
                    if let Some(st) = self.onboarding.as_mut() {
                        st.pick = (pick as i32 + dir).clamp(0, picks as i32 - 1) as usize;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        true
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

    #[test]
    fn theme_window_keeps_selection_visible() {
        assert_eq!(theme_window(0, 30, 6), 0);
        assert_eq!(theme_window(10, 30, 6), 8);
        assert_eq!(theme_window(29, 30, 6), 24);
        assert_eq!(theme_window(1, 3, 6), 0);
    }

    #[test]
    fn scale_steps_through_presets() {
        assert_eq!(next_scale(0.0, 1), 1.0);
        assert_eq!(next_scale(1.0, -1), 0.0);
        assert_eq!(next_scale(0.0, -1), 0.0);
        assert_eq!(next_scale(2.0, 1), 2.0);
        // Between presets: starts from the closest one.
        assert_eq!(next_scale(1.3, 1), 1.5);
    }

    fn sample(recents: usize) -> Onboarding {
        let orig =
            Orig { theme: "a".into(), appearance: String::new(), keymap: "vscode".into(), size: 14.0, scale: 0.0 };
        Onboarding::new(vec!["a".into()], 0, orig, (0..recents).map(|i| PathBuf::from(format!("/p{i}"))).collect())
    }

    #[test]
    fn steps_clamp_and_slide_retargets() {
        let mut o = sample(0);
        o.go(99);
        assert_eq!(o.step, STEPS - 1);
        assert_eq!(o.slide.target(), (STEPS - 1) as f32);
        o.go(2);
        assert_eq!(o.step, 2);
    }

    #[test]
    fn last_step_choices_map_to_actions() {
        let o = sample(2);
        assert_eq!(o.pick_count(), 4);
        assert_eq!(o.pick_hit(0), Hit::OpenFolder);
        assert_eq!(o.pick_hit(1), Hit::Recent(0));
        assert_eq!(o.pick_hit(2), Hit::Recent(1));
        assert_eq!(o.pick_hit(3), Hit::Done);
    }

    #[test]
    fn glide_snaps_when_hidden_then_glides() {
        let mut g = Glide::new();
        assert!(g.now().is_none());
        g.aim(Some(Rect::new(0, 100, 50, 20)));
        assert_eq!(g.r[1].value, 100.0);
        g.a.snap(1.0);
        g.aim(Some(Rect::new(0, 200, 50, 20)));
        g.step(1.0 / 60.0);
        let (r, _) = g.now().unwrap();
        assert!(r.y > 100 && r.y < 200, "glides instead of jumping: {}", r.y);
        g.aim(None);
        for _ in 0..60 {
            g.step(1.0 / 60.0);
        }
        assert!(g.now().is_none());
    }
}
