//! The floating surface of popovers, menus, completion and modals: one shadow, one fill
//! (`raised`), one rim (`border_glow`, brighter with elevation).

use crate::{
    anim::lerp_rgba,
    ds::Elev,
    theme::Chrome,
    ui::{Canvas, Rect},
};

impl Canvas<'_> {
    /// Shadow plus a `rad`-cornered card at elevation `e`.
    pub fn float_card(&mut self, r: Rect, rad: i32, c: &Chrome, e: Elev) {
        let ring = lerp_rgba(c.border_glow, c.tab_active_fg, e.lift() as f32 / 128.0);
        self.shadow_tint(r, rad, c.shadow);
        self.card(r, rad, c.raised, ring);
    }
}
