//! The Lumen mark (the lamp, no background): embedded PNG, scaled once per size and cached, drawn
//! in its own colours (it is not a tinted UI icon).

use crate::{
    mdimg::{self, Pix},
    ui::{Canvas, Rect, sc},
};
use std::sync::Mutex;

static PNG: &[u8] = include_bytes!("../assets/brand/mark.png");
/// Width over height of the mark.
const ASPECT: f32 = 790.0 / 895.0;

static CACHE: Mutex<Option<Pix>> = Mutex::new(None);

/// Logical width of the mark drawn `h` logical pixels tall.
pub fn width_for(h: i32) -> i32 {
    ((h as f32) * ASPECT).round() as i32
}

/// Draw the mark `h` logical pixels tall with its top-left at logical (`x`, `y`), clipped to `clip`
/// (logical pixels).
pub fn draw_mark(cv: &mut Canvas, x: i32, y: i32, h: i32, clip: Rect) {
    let ph = sc(h).max(1) as u32;
    let pw = (ph as f32 * ASPECT).round().max(1.0) as u32;
    let Ok(mut g) = CACHE.lock() else { return };
    if g.as_ref().is_none_or(|p| p.h != ph) {
        *g = mdimg::decode_png(PNG).map(|p| mdimg::scale_to(p, pw, ph));
    }
    if let Some(p) = g.as_ref() {
        let dev_clip = Rect::new(sc(clip.x), sc(clip.y), sc(clip.w), sc(clip.h));
        mdimg::blit(cv, sc(x), sc(y), p, dev_clip);
    }
}
