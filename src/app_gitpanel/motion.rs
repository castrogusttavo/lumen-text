//! Motion and small pure helpers of the Git panel: the sliding pill of the Changes / History tabs,
//! the fade-in of the body after a tab switch, the focus rings of the two text boxes, the sweep of
//! the progress bar of a running fetch / pull / push, and the guidance on the commit subject.
//! Everything is advanced by the frame time and reported through `moving` so an idle panel costs nothing.

use super::Tab;
use crate::anim::{Anim, Hot, Tween, dur};

/// Conventional soft limit of a commit subject line.
pub const SUBJECT_SOFT: usize = 50;
/// Hard limit past which most tools (and GitHub) truncate the subject.
pub const SUBJECT_HARD: usize = 72;

/// How the length of the subject reads: fine, getting long, past the limit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubjectTone {
    Ok,
    Long,
    Over,
}

/// Characters of the first line of `msg` (what the log shows as the subject).
pub fn subject_len(msg: &str) -> usize {
    msg.lines().next().map_or(0, |l| l.trim_end().chars().count())
}

pub fn subject_tone(len: usize) -> SubjectTone {
    if len > SUBJECT_HARD {
        SubjectTone::Over
    } else if len > SUBJECT_SOFT {
        SubjectTone::Long
    } else {
        SubjectTone::Ok
    }
}

/// An indeterminate bar: given the elapsed seconds and the track width, the (offset, width) of the
/// moving segment. It enters from the left edge, crosses and leaves on the right, in 1.4 s loops.
pub fn sweep(elapsed: f32, track: i32) -> (i32, i32) {
    let seg = (track / 3).max(8);
    let phase = (elapsed / 1.4).fract();
    let x = ((track + seg) as f32 * crate::anim::ease_in_out_cubic(phase)) as i32 - seg;
    let left = x.max(0);
    let right = (x + seg).min(track);
    (left, (right - left).max(0))
}

/// Alpha factor (0.45..1) of something that breathes while work is pending.
pub fn pulse(elapsed: f32) -> f32 {
    0.725 + 0.275 * (elapsed * std::f32::consts::TAU / 1.2).sin()
}

/// All animated state of the panel.
#[derive(Clone, Copy, Debug)]
pub struct GpMotion {
    /// Left edge and width of the active tab's pill.
    pub tab_x: Anim,
    pub tab_w: Anim,
    placed: bool,
    shown: Tab,
    veil: Tween,
    pub msg_ring: Hot,
    pub filter_ring: Hot,
}

impl Default for GpMotion {
    fn default() -> Self {
        let mut veil = Tween::new(dur::BASE);
        veil.finish();
        Self {
            tab_x: Anim::new(0.0, dur::BASE),
            tab_w: Anim::new(0.0, dur::BASE),
            placed: false,
            shown: Tab::Changes,
            veil,
            msg_ring: Hot::default(),
            filter_ring: Hot::default(),
        }
    }
}

impl GpMotion {
    /// Where the active tab's pill belongs now; the first call places it without motion.
    pub fn place_tab(&mut self, x: i32, w: i32) {
        if self.placed {
            self.tab_x.to(x as f32);
            self.tab_w.to(w as f32);
        } else {
            self.tab_x.snap(x as f32);
            self.tab_w.snap(w as f32);
            self.placed = true;
        }
    }

    /// The tab on show; a change restarts the fade-in of the body.
    pub fn show(&mut self, tab: Tab) {
        if tab != self.shown {
            self.shown = tab;
            self.veil.start();
        }
    }

    /// Alpha (0..255) of the veil laid over the body while it fades in after a tab switch.
    pub fn veil_alpha(&self) -> u8 {
        if self.veil.running() { (255.0 * 0.85 * (1.0 - self.veil.eased())).round() as u8 } else { 0 }
    }

    pub fn step(&mut self, dt: f64) -> bool {
        let mut busy = self.tab_x.step(dt);
        busy |= self.tab_w.step(dt);
        busy |= self.veil.step(dt);
        busy |= self.msg_ring.step(dt);
        busy |= self.filter_ring.step(dt);
        busy
    }

    pub fn moving(&self) -> bool {
        self.tab_x.moving()
            || self.tab_w.moving()
            || self.veil.running()
            || self.msg_ring.0.moving()
            || self.filter_ring.0.moving()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_counts_first_line_only() {
        assert_eq!(subject_len(""), 0);
        assert_eq!(subject_len("fix: bug  \n\nlong body here"), 8);
        assert_eq!(subject_len("ação"), 4);
    }

    #[test]
    fn subject_tone_thresholds() {
        assert_eq!(subject_tone(SUBJECT_SOFT), SubjectTone::Ok);
        assert_eq!(subject_tone(SUBJECT_SOFT + 1), SubjectTone::Long);
        assert_eq!(subject_tone(SUBJECT_HARD), SubjectTone::Long);
        assert_eq!(subject_tone(SUBJECT_HARD + 1), SubjectTone::Over);
    }

    #[test]
    fn sweep_stays_inside_the_track() {
        for i in 0..=140 {
            let (x, w) = sweep(i as f32 / 100.0, 200);
            assert!(x >= 0 && w >= 0 && x + w <= 200, "{x} {w}");
        }
        assert_eq!(sweep(0.0, 200).1, 0);
    }

    #[test]
    fn pulse_stays_in_range() {
        for i in 0..100 {
            let p = pulse(i as f32 / 50.0);
            assert!((0.44..=1.01).contains(&p));
        }
    }

    #[test]
    fn tab_pill_snaps_first_then_glides() {
        let mut m = GpMotion::default();
        m.place_tab(10, 60);
        assert!(!m.moving());
        m.place_tab(80, 70);
        assert!(m.moving());
        while m.step(1.0 / 60.0) {}
        assert_eq!(m.tab_x.value, 80.0);
        assert_eq!(m.tab_w.value, 70.0);
    }

    #[test]
    fn switching_tabs_fades_the_body_in() {
        let mut m = GpMotion::default();
        assert_eq!(m.veil_alpha(), 0);
        m.show(Tab::Changes);
        assert_eq!(m.veil_alpha(), 0);
        m.show(Tab::History);
        assert!(m.veil_alpha() > 200);
        while m.step(1.0 / 60.0) {}
        assert_eq!(m.veil_alpha(), 0);
    }
}
