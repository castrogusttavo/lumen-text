//! Motion of the AI panel's chrome (hover glow, the sliding tab indicator, the status pulse, the
//! "starting" skeleton, the focus glow of the terminal frame and the staggered entrance of the
//! empty state). Plain state advanced by the frame time and unit-tested; `draw` only reads it.

use super::AiHit;
use crate::anim::{Anim, Hot, dur, stagger};

/// Seconds of one pulse of a working status dot.
const PULSE_PERIOD: f32 = 1.6;
/// Seconds between one skeleton bar and the next in the shimmer wave.
const WAVE_SPEED: f32 = 4.0;
/// Delay between two items of the empty state's entrance.
const ENTER_GAP: f32 = 0.05;
/// After this long every item of the entrance has settled (the redraws stop).
const ENTER_TOTAL: f32 = 1.0;

pub struct Motion {
    clock: f32,
    /// Hover amount of every button the pointer is on or just left.
    hot: Vec<(AiHit, Hot)>,
    /// The active tab's left edge and width: slides when another tab becomes active.
    ind_x: Anim,
    ind_w: Anim,
    ind_for: Option<u64>,
    /// 1 while the agent boots with nothing on screen yet.
    skeleton: Anim,
    /// 1 while the terminal has the keyboard.
    focus: Anim,
    /// Seconds since the empty state appeared (0 while it is not shown).
    empty_t: f32,
    /// Something pulses this frame: keep redrawing.
    live: bool,
}

impl Default for Motion {
    fn default() -> Self {
        Motion {
            clock: 0.0,
            hot: Vec::new(),
            ind_x: Anim::new(0.0, dur::BASE),
            ind_w: Anim::new(0.0, dur::BASE),
            ind_for: None,
            skeleton: Anim::new(0.0, dur::SLOW),
            focus: Anim::new(0.0, dur::BASE),
            empty_t: 0.0,
            live: false,
        }
    }
}

impl Motion {
    /// Advance everything by `dt`. `pulse`: a status dot is pulsing; `empty`: the empty state shows.
    pub fn step(&mut self, dt: f64, pulse: bool, empty: bool) {
        let dt = dt.min(0.05);
        self.clock = (self.clock + dt as f32) % 1000.0;
        for (_, h) in &mut self.hot {
            h.step(dt);
        }
        self.hot.retain(|(_, h)| h.0.moving() || h.v() > 0.0);
        self.ind_x.step(dt);
        self.ind_w.step(dt);
        self.skeleton.step(dt);
        self.focus.step(dt);
        self.live = pulse;
        self.empty_t = if empty { self.empty_t + dt as f32 } else { 0.0 };
    }

    /// Is anything still moving (the caller ORs this into the frame's `busy`)?
    pub fn moving(&self) -> bool {
        self.live
            || self.hot.iter().any(|(_, h)| h.0.moving())
            || self.ind_x.moving()
            || self.ind_w.moving()
            || self.skeleton.moving()
            || self.skeleton.value > 0.001
            || self.focus.moving()
            || (self.empty_t > 0.0 && self.empty_t < ENTER_TOTAL)
    }

    /// The pointer is on `h` (or on nothing).
    pub fn hover(&mut self, h: Option<AiHit>) {
        for (k, hot) in &mut self.hot {
            hot.set(Some(*k) == h);
        }
        if let Some(h) = h
            && !self.hot.iter().any(|(k, _)| *k == h)
        {
            let mut hot = Hot::default();
            hot.set(true);
            self.hot.push((h, hot));
        }
    }

    /// Hover amount 0..1 of `h`.
    pub fn hot(&self, h: AiHit) -> f32 {
        self.hot.iter().find(|(k, _)| *k == h).map_or(0.0, |(_, v)| v.v())
    }

    /// Where the active tab's indicator is drawn this frame, given where the tab of session `id` is
    /// laid out. Follows the layout at once (panel resize, squeezed tabs) and glides only when the
    /// active session changes.
    pub fn indicator(&mut self, id: u64, x: i32, w: i32) -> (i32, i32) {
        let (xf, wf) = (x as f32, w as f32);
        let still = !self.ind_x.moving() && !self.ind_w.moving();
        if self.ind_for.is_none_or(|f| f == id && still) {
            self.ind_x.snap(xf);
            self.ind_w.snap(wf);
        } else {
            self.ind_x.to(xf);
            self.ind_w.to(wf);
        }
        self.ind_for = Some(id);
        (self.ind_x.value.round() as i32, self.ind_w.value.round() as i32)
    }

    pub fn set_skeleton(&mut self, on: bool) {
        self.skeleton.to(if on { 1.0 } else { 0.0 });
    }

    pub fn skeleton(&self) -> f32 {
        self.skeleton.value.clamp(0.0, 1.0)
    }

    pub fn set_focus(&mut self, on: bool) {
        self.focus.to(if on { 1.0 } else { 0.0 });
    }

    pub fn focus(&self) -> f32 {
        self.focus.value.clamp(0.0, 1.0)
    }

    /// Phase 0..1 of the status pulse.
    pub fn pulse(&self) -> f32 {
        (self.clock % PULSE_PERIOD) / PULSE_PERIOD
    }

    /// Brightness 0..1 of skeleton bar `i`: a wave running down the bars.
    pub fn wave(&self, i: usize) -> f32 {
        0.5 + 0.5 * (self.clock * WAVE_SPEED - i as f32 * 0.8).sin()
    }

    /// Entrance progress 0..1 of item `i` of the empty state.
    pub fn enter(&self, i: usize) -> f32 {
        stagger(self.empty_t, i, ENTER_GAP, dur::SLOW)
    }
}

/// Alpha of a hover tint: grows with the hover amount `k`, and jumps higher while the button is held.
pub fn tint_alpha(k: f32, down: bool, max: u8) -> u8 {
    let k = if down { 1.7 } else { k.clamp(0.0, 1.0) };
    (max as f32 * k).round().min(255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(m: &mut Motion, frames: usize) {
        for _ in 0..frames {
            m.step(1.0 / 60.0, false, false);
        }
    }

    #[test]
    fn hover_fades_in_then_out_and_is_dropped() {
        let mut m = Motion::default();
        m.hover(Some(AiHit::New));
        run(&mut m, 30);
        assert!(m.hot(AiHit::New) > 0.95);
        assert!(!m.moving(), "a held hover is at rest");
        m.hover(None);
        run(&mut m, 60);
        assert_eq!(m.hot(AiHit::New), 0.0);
        assert!(m.hot.is_empty(), "settled entries are pruned");
    }

    #[test]
    fn hover_moves_from_one_button_to_the_next() {
        let mut m = Motion::default();
        m.hover(Some(AiHit::New));
        run(&mut m, 30);
        m.hover(Some(AiHit::Stop));
        run(&mut m, 3);
        assert!(m.hot(AiHit::New) < 1.0 && m.hot(AiHit::Stop) > 0.0);
    }

    #[test]
    fn indicator_follows_layout_but_glides_between_tabs() {
        let mut m = Motion::default();
        assert_eq!(m.indicator(1, 10, 100), (10, 100));
        // Same tab, new geometry (panel resized): immediate.
        assert_eq!(m.indicator(1, 20, 90), (20, 90));
        // Another tab becomes active: not there yet, then arrives.
        let (x, _) = m.indicator(2, 200, 100);
        assert!(x < 200);
        let mut last = x;
        for _ in 0..90 {
            m.step(1.0 / 60.0, false, false);
            last = m.indicator(2, 200, 100).0;
        }
        assert_eq!(last, 200);
        assert!(!m.moving());
    }

    #[test]
    fn skeleton_fades_and_keeps_redrawing_until_gone() {
        let mut m = Motion::default();
        m.set_skeleton(true);
        assert!(m.moving());
        run(&mut m, 60);
        assert!(m.skeleton() > 0.99);
        assert!(m.moving(), "the shimmer needs frames while it shows");
        m.set_skeleton(false);
        run(&mut m, 90);
        assert!(!m.moving());
    }

    #[test]
    fn empty_state_enters_in_order_and_then_rests() {
        let mut m = Motion::default();
        m.step(0.1, false, true);
        assert!(m.enter(0) > m.enter(4));
        assert!(m.moving());
        for _ in 0..80 {
            m.step(1.0 / 60.0, false, true);
        }
        assert_eq!(m.enter(8), 1.0);
        assert!(!m.moving());
        m.step(0.016, false, false);
        assert_eq!(m.enter(0), 0.0, "leaving the empty state rewinds the entrance");
    }

    #[test]
    fn pulse_and_wave_stay_in_range() {
        let mut m = Motion::default();
        for _ in 0..200 {
            m.step(0.05, true, false);
            assert!((0.0..1.0).contains(&m.pulse()));
            assert!((0.0..=1.0).contains(&m.wave(3)));
        }
        assert!(m.moving());
    }

    #[test]
    fn tint_grows_with_hover_and_press() {
        assert_eq!(tint_alpha(0.0, false, 0x20), 0);
        assert_eq!(tint_alpha(1.0, false, 0x20), 0x20);
        assert!(tint_alpha(1.0, true, 0x20) > 0x20);
        assert_eq!(tint_alpha(1.0, true, 0xff), 0xff);
    }
}
