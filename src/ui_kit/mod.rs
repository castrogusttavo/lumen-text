//! The shared UI kit: one implementation per visual concept, so every surface of the product
//! wears the same clothes. Colors come from `theme::Chrome`, geometry from `ds`, motion from
//! `anim` / `motion`.
//!
//! * [`chip`]: tinted pills (kinds, counters, badges).
//! * [`keycap`]: shortcut keys drawn as small rounded caps.
//! * [`button`]: primary / secondary / danger buttons.
//! * [`empty`]: the friendly empty state (icon tile, title, hint, one action).
//! * [`shimmer`]: loading skeletons and the shared animation clock.
//! * [`card`]: the floating surface of popovers, menus and modals.

pub mod button;
pub mod card;
pub mod chip;
pub mod empty;
pub mod keycap;
pub mod shimmer;
