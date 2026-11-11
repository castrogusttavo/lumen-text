//! Editor display settings read while drawing: line numbers, bookmark and breakpoint marks in the
//! gutter, and the scrollbar mode (always / auto-hide / never).
//!
//! They live in one packed word (like the zoom level) so any drawing code can ask without being
//! handed the config. In tests each thread has its own copy, so parallel tests never see each other.

use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scrollbar {
    /// Thin bars always visible (the classic look).
    Show,
    /// Fade in on scroll or hover, fade out after a moment.
    Auto,
    /// Never drawn, never grabbed; the wheel and keys still scroll. The default setting.
    Never,
}

impl Scrollbar {
    pub fn from_name(s: &str) -> Scrollbar {
        match s {
            "auto" => Scrollbar::Auto,
            "show" => Scrollbar::Show,
            _ => Scrollbar::Never,
        }
    }
}

/// Width of the gutter's mark column (breakpoint dots, test marks, the debug arrow) when the line
/// numbers are hidden; with numbers the marks sit over their left padding.
pub const NUMLESS_MARKS_W: i32 = 28;
/// Clicks within this many px of the gutter's left edge toggle a breakpoint.
pub const BP_CLICK_W: i32 = 14;

// Bits of the word; all zero = everything shown, scrollbars always on. (The `scrollbar` setting
// defaults to "never"; `apply` writes it at startup, so the zero state is only what tests start from.)
const NO_NUMBERS: u32 = 1;
const NO_BOOKMARKS: u32 = 2;
const NO_BREAKPOINTS: u32 = 4;
const BAR_SHIFT: u32 = 4; // two bits: 0 show, 1 auto, 2 never
const WS_SHIFT: u32 = 6; // two bits: 0 none, 1 boundary, 2 trailing, 3 all

#[cfg(not(test))]
mod store {
    use std::sync::atomic::{AtomicU32, Ordering};
    static WORD: AtomicU32 = AtomicU32::new(0);
    static TOUCH_MS: AtomicU32 = AtomicU32::new(0);
    pub fn get() -> u32 {
        WORD.load(Ordering::Relaxed)
    }
    pub fn set(v: u32) {
        WORD.store(v, Ordering::Relaxed)
    }
    pub fn touch_ms() -> u32 {
        TOUCH_MS.load(Ordering::Relaxed)
    }
    pub fn set_touch_ms(v: u32) {
        TOUCH_MS.store(v, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod store {
    use std::cell::Cell;
    thread_local! {
        static WORD: Cell<u32> = const { Cell::new(0) };
        static TOUCH_MS: Cell<u32> = const { Cell::new(0) };
    }
    pub fn get() -> u32 {
        WORD.with(Cell::get)
    }
    pub fn set(v: u32) {
        WORD.with(|c| c.set(v))
    }
    pub fn touch_ms() -> u32 {
        TOUCH_MS.with(Cell::get)
    }
    pub fn set_touch_ms(v: u32) {
        TOUCH_MS.with(|c| c.set(v))
    }
}

fn set_bit(bit: u32, on: bool) {
    let w = store::get();
    store::set(if on { w | bit } else { w & !bit });
}

pub fn show_line_numbers() -> bool {
    store::get() & NO_NUMBERS == 0
}
pub fn set_line_numbers(on: bool) {
    set_bit(NO_NUMBERS, !on)
}
pub fn show_bookmarks() -> bool {
    store::get() & NO_BOOKMARKS == 0
}
pub fn set_bookmarks(on: bool) {
    set_bit(NO_BOOKMARKS, !on)
}
pub fn show_breakpoints() -> bool {
    store::get() & NO_BREAKPOINTS == 0
}
pub fn set_breakpoints(on: bool) {
    set_bit(NO_BREAKPOINTS, !on)
}

pub fn scrollbar() -> Scrollbar {
    match (store::get() >> BAR_SHIFT) & 3 {
        1 => Scrollbar::Auto,
        2 => Scrollbar::Never,
        _ => Scrollbar::Show,
    }
}
pub fn set_scrollbar(m: Scrollbar) {
    let v = match m {
        Scrollbar::Show => 0,
        Scrollbar::Auto => 1,
        Scrollbar::Never => 2,
    };
    store::set((store::get() & !(3 << BAR_SHIFT)) | (v << BAR_SHIFT));
}

/// Which whitespace the editor draws (`render_whitespace`).
pub fn whitespace() -> crate::whitespace::Render {
    use crate::whitespace::Render;
    match (store::get() >> WS_SHIFT) & 3 {
        1 => Render::Boundary,
        2 => Render::Trailing,
        3 => Render::All,
        _ => Render::Off,
    }
}
pub fn set_whitespace(m: crate::whitespace::Render) {
    use crate::whitespace::Render;
    let v = match m {
        Render::Off => 0,
        Render::Boundary => 1,
        Render::Trailing => 2,
        Render::All => 3,
    };
    store::set((store::get() & !(3 << WS_SHIFT)) | (v << WS_SHIFT));
}

/// Load every display setting from the config.
pub fn apply(c: &crate::config::Config) {
    set_line_numbers(c.show_line_numbers);
    set_bookmarks(c.show_bookmarks);
    set_breakpoints(c.show_breakpoints);
    set_scrollbar(Scrollbar::from_name(&c.scrollbar));
    set_whitespace(crate::whitespace::Render::from_name(&c.render_whitespace));
}

// ── auto-hide scrollbars ──

/// Scrolling keeps bars at full strength this long, then they fade out over [`FADE_MS`].
pub const HOLD_MS: u32 = 900;
pub const FADE_MS: u32 = 400;

fn now_ms() -> u32 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis().min(u32::MAX as u128 - 1) as u32 + 1
}

/// True when the scroll `signature` (hash of the visible panes' positions) differs from the last
/// one seen; the very first signature never counts.
pub fn scroll_changed(sig: u64) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SIG: AtomicU64 = AtomicU64::new(0);
    let old = SIG.swap(sig, Ordering::Relaxed);
    old != 0 && old != sig
}

/// Something scrolled: show the bars (auto mode).
pub fn touch() {
    store::set_touch_ms(now_ms());
}

/// Bar strength 0..=1 `elapsed` ms after the last scroll activity.
pub fn fade(elapsed: u32) -> f32 {
    if elapsed <= HOLD_MS { 1.0 } else { (1.0 - (elapsed - HOLD_MS) as f32 / FADE_MS as f32).clamp(0.0, 1.0) }
}

/// How strong bars are drawn right now: 1 in `show`, 0 in `never`, and in `auto` 1 while the
/// pointer is on the bar (`near`) or just after scrolling, fading to 0 afterwards.
pub fn bar_strength(near: bool) -> f32 {
    match scrollbar() {
        Scrollbar::Show => 1.0,
        Scrollbar::Never => 0.0,
        Scrollbar::Auto if near => 1.0,
        Scrollbar::Auto => {
            let t = store::touch_ms();
            if t == 0 { 0.0 } else { fade(now_ms().saturating_sub(t)) }
        }
    }
}

/// Bars are mid-fade: keep redrawing frames until they are gone.
pub fn fading() -> bool {
    if scrollbar() != Scrollbar::Auto {
        return false;
    }
    let t = store::touch_ms();
    t != 0 && now_ms().saturating_sub(t) < HOLD_MS + FADE_MS + 50
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_show_everything() {
        assert!(show_line_numbers() && show_bookmarks() && show_breakpoints());
        assert_eq!(scrollbar(), Scrollbar::Show);
    }

    #[test]
    fn flags_are_independent() {
        set_line_numbers(false);
        assert!(!show_line_numbers() && show_bookmarks() && show_breakpoints());
        set_breakpoints(false);
        set_bookmarks(false);
        set_scrollbar(Scrollbar::Never);
        assert!(!show_line_numbers() && !show_bookmarks() && !show_breakpoints());
        assert_eq!(scrollbar(), Scrollbar::Never);
        set_line_numbers(true);
        set_scrollbar(Scrollbar::Auto);
        assert!(show_line_numbers() && !show_bookmarks());
        assert_eq!(scrollbar(), Scrollbar::Auto);
    }

    #[test]
    fn whitespace_mode_shares_the_word_without_touching_the_rest() {
        use crate::whitespace::Render;
        assert_eq!(whitespace(), Render::Off, "nothing drawn by default");
        set_scrollbar(Scrollbar::Auto);
        for m in [Render::All, Render::Trailing, Render::Boundary, Render::Off] {
            set_whitespace(m);
            assert_eq!(whitespace(), m);
            assert_eq!(scrollbar(), Scrollbar::Auto);
            assert!(show_line_numbers());
        }
    }

    #[test]
    fn scrollbar_names() {
        assert_eq!(Scrollbar::from_name("auto"), Scrollbar::Auto);
        assert_eq!(Scrollbar::from_name("never"), Scrollbar::Never);
        assert_eq!(Scrollbar::from_name("show"), Scrollbar::Show);
        assert_eq!(Scrollbar::from_name("???"), Scrollbar::Never, "unknown = the default");
    }

    #[test]
    fn the_default_config_hides_scrollbars() {
        apply(&crate::config::Config::default());
        assert_eq!(scrollbar(), Scrollbar::Never);
        assert!(crate::ui::VBar::new(crate::ui::Rect::new(0, 0, 14, 200), 1000.0, 100.0, 0.0).is_none());
    }

    #[test]
    fn fade_curve() {
        assert_eq!(fade(0), 1.0);
        assert_eq!(fade(HOLD_MS), 1.0);
        assert!((fade(HOLD_MS + FADE_MS / 2) - 0.5).abs() < 0.01);
        assert_eq!(fade(HOLD_MS + FADE_MS), 0.0);
        assert_eq!(fade(60_000), 0.0);
    }

    #[test]
    fn strength_per_mode() {
        set_scrollbar(Scrollbar::Show);
        assert_eq!(bar_strength(false), 1.0);
        set_scrollbar(Scrollbar::Never);
        assert_eq!(bar_strength(true), 0.0, "never wins even under the pointer");
        set_scrollbar(Scrollbar::Auto);
        assert_eq!(bar_strength(false), 0.0, "hidden until something scrolls");
        assert_eq!(bar_strength(true), 1.0, "hover shows it");
        touch();
        assert_eq!(bar_strength(false), 1.0, "just scrolled");
        assert!(fading());
    }

    #[test]
    fn hidden_bars_take_no_part_in_hit_testing() {
        use crate::ui::{HBar, Rect, VBar};
        let t = Rect::new(0, 0, 14, 200);
        assert!(VBar::new(t, 1000.0, 100.0, 0.0).is_some());
        set_scrollbar(Scrollbar::Auto);
        assert!(VBar::new(t, 1000.0, 100.0, 0.0).is_some(), "auto keeps the bar (and its drag) available");
        set_scrollbar(Scrollbar::Never);
        assert!(VBar::new(t, 1000.0, 100.0, 0.0).is_none());
        assert!(HBar::new(Rect::new(0, 0, 200, 12), 1000.0, 100.0, 0.0).is_none());
    }
}
