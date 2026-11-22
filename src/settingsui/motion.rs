//! Motion, hover and small pure helpers of the settings modal: the sliding indicators, the
//! animated switches, which hit region the pointer rests on (for hover tints, the cursor and the
//! delayed tooltip), the slider math and the search-match ranges.
//!
//! Everything is advanced by the frame time measured between two `Modal::draw` calls and reported
//! through `Motion::busy`, so the app only keeps redrawing while something still moves.
use super::*;
use crate::anim::{Anim, dur};
use std::path::PathBuf;
use std::time::Instant;

/// Seconds the pointer must rest on a control before its tooltip shows.
pub(super) const TIP_DELAY: f32 = 0.4;

pub(super) struct Motion {
    last: Option<Instant>,
    /// Seconds since the previous frame (clamped), valid during one `draw`.
    pub dt: f64,
    /// "Reduce motion": every animated value jumps to its target.
    pub reduce: bool,
    /// Set while any value is still moving; the app reads it to schedule the next frame.
    pub busy: bool,
    /// Top of the active left-nav entry, in rows (fractional while sliding).
    pub nav: Option<Anim>,
    /// Opacity of the nav indicator (it fades while a search lists results from every page).
    pub nav_a: Anim,
    /// Header tab indicator: x and width in px.
    pub tab_x: Option<Anim>,
    pub tab_w: Option<Anim>,
    pub toggles: HashMap<usize, Anim>,
    pub segs: HashMap<usize, Anim>,
    /// The hit region under the pointer and for how long it has been resting there.
    pub hover: Option<Hit>,
    pub hover_t: f32,
    pub mouse: (i32, i32),
    /// Slider being dragged (def index).
    pub drag: Option<usize>,
    /// Slider tracks of the last frame (def -> track rectangle).
    pub tracks: HashMap<usize, Rect>,
    /// Phase of the "recording" ring pulse.
    pub pulse: f32,
    /// Where the bundled themes live (for the dropdown's colour swatches).
    pub theme_dir: Option<PathBuf>,
    swatches: HashMap<String, Option<[Rgba; 3]>>,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            last: None,
            dt: 0.016,
            reduce: false,
            busy: false,
            nav: None,
            nav_a: Anim::new(1.0, dur::BASE),
            tab_x: None,
            tab_w: None,
            toggles: HashMap::new(),
            segs: HashMap::new(),
            hover: None,
            hover_t: 0.0,
            mouse: (0, 0),
            drag: None,
            tracks: HashMap::new(),
            pulse: 0.0,
            theme_dir: None,
            swatches: HashMap::new(),
        }
    }
}

/// Advance `a` toward `target`; true while it still moves. With reduced motion it jumps.
pub(super) fn drive(a: &mut Anim, target: f32, reduce: bool, dt: f64) -> bool {
    if reduce {
        a.snap(target);
        return false;
    }
    a.to(target);
    a.step(dt)
}

impl Motion {
    /// Start of a frame: measure `dt`, clear the busy flag and the per-frame slider tracks.
    pub fn begin_frame(&mut self) {
        let now = Instant::now();
        let since = self.last.map_or(0.016, |l| now.duration_since(l).as_secs_f64());
        self.last = Some(now);
        self.dt = if since > 0.1 { 0.016 } else { since.min(0.05) };
        self.busy = false;
        self.tracks.clear();
        if self.hover.is_some() {
            self.hover_t += self.dt as f32;
        }
    }

    /// End of a frame: a tooltip still waiting for its delay needs one more frame later.
    pub fn end_frame(&mut self, has_tip: bool) {
        if has_tip && self.hover_t < TIP_DELAY {
            self.busy = true;
        }
    }

    pub fn is_hot(&self, h: &Hit) -> bool {
        self.hover.as_ref() == Some(h)
    }

    /// True when the pointer is on any control of the row `def` (row tint).
    pub fn row_hot(&self, def: usize) -> bool {
        self.hover.as_ref().and_then(hit_def) == Some(def)
    }

    /// Animated 0..1 position of a switch.
    pub fn toggle(&mut self, def: usize, on: bool) -> f32 {
        let (dt, reduce) = (self.dt, self.reduce);
        let target = if on { 1.0 } else { 0.0 };
        let a = self.toggles.entry(def).or_insert_with(|| Anim::new(target, dur::BASE));
        let moving = drive(a, target, reduce, dt);
        let v = a.value;
        self.busy |= moving;
        v
    }

    /// Animated (fractional) index of the active segment of a segmented control.
    pub fn seg(&mut self, def: usize, idx: usize) -> f32 {
        let (dt, reduce) = (self.dt, self.reduce);
        let target = idx as f32;
        let a = self.segs.entry(def).or_insert_with(|| Anim::new(target, dur::BASE));
        let moving = drive(a, target, reduce, dt);
        let v = a.value;
        self.busy |= moving;
        v
    }

    /// Animated value that starts on its first target (no slide-in on the first frame).
    pub fn glide(slot: &mut Option<Anim>, target: f32, reduce: bool, dt: f64, busy: &mut bool) -> f32 {
        let a = slot.get_or_insert_with(|| Anim::new(target, dur::BASE));
        *busy |= drive(a, target, reduce, dt);
        a.value
    }

    /// The three swatch colours (background, foreground, accent) of a theme, loaded once.
    pub fn swatch(&mut self, name: &str) -> Option<[Rgba; 3]> {
        if let Some(s) = self.swatches.get(name) {
            return *s;
        }
        let mut path = self.theme_dir.as_ref().map(|d| d.join(format!("{name}.json"))).filter(|p| p.exists());
        if path.is_none() {
            path = crate::zedtheme::list_user_themes().into_iter().find(|(n, _)| n == name).map(|(_, p)| p);
        }
        let sw = path.and_then(|p| Theme::load(&p).ok()).map(|t| [t.background, t.foreground, t.chrome.accent]);
        self.swatches.insert(name.to_string(), sw);
        sw
    }
}

/// The setting row a hit region belongs to.
pub(super) fn hit_def(h: &Hit) -> Option<usize> {
    match h {
        Hit::SetRow(d)
        | Hit::Toggle(d)
        | Hit::Seg(d, _)
        | Hit::DropOpen(d)
        | Hit::Step(d, _)
        | Hit::EditText(d)
        | Hit::SetReset(d)
        | Hit::Slide(d) => Some(*d),
        _ => None,
    }
}

/// Mouse cursor over a hit region: I-beam on text inputs, hand on everything clickable.
pub(super) fn cursor_of(h: &Hit) -> winit::window::CursorIcon {
    use winit::window::CursorIcon;
    match h {
        Hit::Search | Hit::EditText(_) => CursorIcon::Text,
        Hit::Block | Hit::SetRow(_) => CursorIcon::Default,
        _ => CursorIcon::Pointer,
    }
}

/// Value of a slider for the pointer at fraction `frac` (0..1) of its track, snapped to `step`.
pub(super) fn slider_value(min: f64, max: f64, step: f64, frac: f64) -> f64 {
    let raw = min + frac.clamp(0.0, 1.0) * (max - min);
    if step <= 0.0 {
        return raw.clamp(min, max);
    }
    (min + ((raw - min) / step).round() * step).clamp(min, max)
}

/// Fraction (0..1) of `v` along `min..=max`.
pub(super) fn slider_frac(min: f64, max: f64, v: f64) -> f32 {
    if max <= min { 0.0 } else { ((v - min) / (max - min)).clamp(0.0, 1.0) as f32 }
}

/// A number row is drawn as a slider with a value chip when its step is fractional (scale,
/// speed, line height); whole-number rows keep the `- n +` stepper.
pub(super) fn is_slider(min: f64, max: f64, step: f64) -> bool {
    step > 0.0 && step < 1.0 && (max - min) / step <= 200.0
}

/// Position (x, w) of a fractional segment index between two neighbouring segments.
pub(super) fn seg_span(segs: &[(i32, i32)], f: f32) -> Option<(i32, i32)> {
    let last = segs.len().checked_sub(1)?;
    let f = f.clamp(0.0, last as f32);
    let i = (f.floor() as usize).min(last);
    let j = (i + 1).min(last);
    let t = f - i as f32;
    let l = |a: i32, b: i32| (a as f32 + (b - a) as f32 * t).round() as i32;
    Some((l(segs[i].0, segs[j].0), l(segs[i].1, segs[j].1)))
}

/// Byte ranges of `text` matched by any whitespace-separated term of `q` (case-insensitive for
/// ASCII), merged and sorted. Used to highlight what a search matched.
pub(super) fn match_ranges(text: &str, q: &str) -> Vec<(usize, usize)> {
    let hay = text.to_ascii_lowercase();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for term in q.to_ascii_lowercase().split_whitespace() {
        let mut from = 0;
        while let Some(i) = hay[from..].find(term) {
            let s = from + i;
            out.push((s, s + term.len()));
            from = s + term.len();
        }
    }
    out.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in out {
        match merged.last_mut() {
            Some(l) if s <= l.1 => l.1 = l.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// Rows that choose a colour theme (their options get a live colour swatch).
pub(super) fn is_theme_key(key: &str) -> bool {
    matches!(key, "theme" | "theme_light" | "theme_dark")
}

/// Description of a hit region for the tooltip: (text, shortcut combo).
pub(super) fn tip_text(h: &Hit, settings: &SettingsPage) -> Option<(String, Option<&'static str>)> {
    use crate::projcfg::Scope;
    Some(match h {
        Hit::Close => ("Close".to_string(), Some("escape")),
        Hit::Tab(Page::Settings) => ("Settings".to_string(), Some("ctrl+tab")),
        Hit::Tab(Page::Keybinds) => ("Keyboard shortcuts".to_string(), Some("ctrl+tab")),
        Hit::Scope(Scope::User) => ("Edit your settings, used in every project".to_string(), Some("alt+u")),
        Hit::Scope(Scope::Project) => ("Edit this project's settings".to_string(), Some("alt+p")),
        Hit::SetReset(d) => (format!("Reset to default ({})", settings.default_label(*d)), None),
        Hit::KbReset(_) => ("Restore the default shortcut".to_string(), Some("ctrl+r")),
        Hit::KbResetAll => ("Restore every shortcut to its default".to_string(), Some("ctrl+shift+r")),
        Hit::ClearSearch | Hit::KbClearFilter => ("Clear".to_string(), None),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_snaps_to_the_step_and_stays_in_range() {
        assert_eq!(slider_value(0.0, 4.0, 0.25, 0.5), 2.0);
        assert_eq!(slider_value(0.0, 4.0, 0.25, 0.51), 2.0);
        assert_eq!(slider_value(0.0, 4.0, 0.25, -3.0), 0.0);
        assert_eq!(slider_value(0.0, 4.0, 0.25, 9.0), 4.0);
        assert!((slider_value(0.1, 5.0, 0.1, 0.5) - 2.55).abs() < 0.11);
        assert!((slider_frac(0.0, 4.0, 1.0) - 0.25).abs() < 1e-6);
        assert_eq!(slider_frac(2.0, 2.0, 2.0), 0.0);
    }

    #[test]
    fn fractional_steps_become_sliders_whole_ones_stay_steppers() {
        assert!(is_slider(0.0, 4.0, 0.25));
        assert!(is_slider(1.0, 2.5, 0.05));
        assert!(!is_slider(8.0, 40.0, 1.0));
        assert!(!is_slider(100.0, 900.0, 100.0));
        assert!(!is_slider(0.0, 4.0, 0.0));
    }

    #[test]
    fn segment_pill_interpolates_between_neighbours() {
        let segs = [(10, 40), (50, 60), (110, 80)];
        assert_eq!(seg_span(&segs, 0.0), Some((10, 40)));
        assert_eq!(seg_span(&segs, 1.0), Some((50, 60)));
        assert_eq!(seg_span(&segs, 0.5), Some((30, 50)));
        assert_eq!(seg_span(&segs, 9.0), Some((110, 80)));
        assert_eq!(seg_span(&[], 0.0), None);
    }

    #[test]
    fn search_matches_are_found_merged_and_case_insensitive() {
        assert_eq!(match_ranges("Format on save", "FORMAT"), vec![(0, 6)]);
        assert_eq!(match_ranges("Format on save", "on sa"), vec![(7, 9), (10, 12)]);
        assert_eq!(match_ranges("aaa", "aa a"), vec![(0, 3)]);
        assert!(match_ranges("Format", "zzz").is_empty());
        assert!(match_ranges("Format", "   ").is_empty());
        // Non-ASCII text never splits a character.
        let s = "Zoom é aqui";
        for (a, b) in match_ranges(s, "é aq") {
            assert!(s.get(a..b).is_some());
        }
    }

    #[test]
    fn reduced_motion_jumps_and_normal_motion_settles() {
        let mut a = Anim::new(0.0, dur::BASE);
        assert!(drive(&mut a, 1.0, false, 0.016));
        assert!(a.value > 0.0 && a.value < 1.0);
        for _ in 0..200 {
            drive(&mut a, 1.0, false, 0.016);
        }
        assert_eq!(a.value, 1.0);
        assert!(!drive(&mut a, 0.0, true, 0.016));
        assert_eq!(a.value, 0.0);
    }

    #[test]
    fn cursor_is_a_hand_on_controls_and_a_beam_on_inputs() {
        use winit::window::CursorIcon;
        assert_eq!(cursor_of(&Hit::Toggle(0)), CursorIcon::Pointer);
        assert_eq!(cursor_of(&Hit::Slide(0)), CursorIcon::Pointer);
        assert_eq!(cursor_of(&Hit::Search), CursorIcon::Text);
        assert_eq!(cursor_of(&Hit::Block), CursorIcon::Default);
    }
}
