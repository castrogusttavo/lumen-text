//! The "reduce motion" switch. `Anim::step` and `Tween::step` consult `reduced()` and snap to
//! their target, so every animated surface honours the setting without per-widget code. Code that
//! animates by other means (shimmer, pulses, slides) should check `reduced()` and draw the final
//! state instead.

use std::sync::atomic::{AtomicBool, Ordering};

static SETTING: AtomicBool = AtomicBool::new(false);

/// Record the `reduce_motion` config value (called at startup and when the setting changes).
pub fn set(on: bool) {
    SETTING.store(on, Ordering::Relaxed);
}

/// Is `LUMEN_REDUCE_MOTION` set to a truthy value ("1" or "true")?
pub fn env_on(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("1") | Some("true"))
}

/// True when animations should snap: the config setting or `LUMEN_REDUCE_MOTION=1`.
pub fn reduced() -> bool {
    SETTING.load(Ordering::Relaxed) || env_on(std::env::var("LUMEN_REDUCE_MOTION").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values() {
        assert!(env_on(Some("1")) && env_on(Some("true")) && env_on(Some(" 1 ")));
        assert!(!env_on(Some("0")) && !env_on(Some("")) && !env_on(None));
    }

    #[test]
    fn off_by_default() {
        // The global is never flipped by tests (they run in parallel with the animation tests).
        assert!(!SETTING.load(Ordering::Relaxed));
    }
}
