//! Shared frame of every modal surface (command palette, quick open, prompts, settings, sheets).
//!
//! * One entrance for all of them: a softly fading backdrop and a panel that rises
//!   [`RISE`] px while it fades in (`dur::SLOW`), driven by [`Fx`] and `anim::Anim`.
//! * The entrance works on any drawing code without touching it: the region is copied before the
//!   surface draws ([`Canvas::snap_region`]) and mixed back with the finished picture
//!   ([`Canvas::blend_over_snap`]) by the entrance progress, shifted down by the rise.
//! * Keycap chips ([`keycaps`], [`draw_keycaps`]) and the footer hint row of a panel.
//! * Semantics: Esc closes, Enter accepts (see `app_overlay::input`); the footer says so.

use crate::{
    anim::{self, Anim, dur},
    app::App,
    app_overlay::Overlay,
    ds::{self, Elev},
    theme::{Chrome, Rgba},
    ui::{Canvas, Rect, TextCtx},
};

/// How far below its place a panel starts, in logical px.
pub const RISE: i32 = ds::S2;
/// Strength of the backdrop dim (alpha, 0..255) in a dark / light theme.
const DIM_DARK: f32 = 110.0;
const DIM_LIGHT: f32 = 64.0;
/// Height of a keycap chip, in logical px.
pub const KEY_H: i32 = 20;
const KEY_PAD: i32 = 6;
const KEY_GAP: i32 = ds::S1 / 2;
/// Height of the footer hint row of a panel.
pub const FOOT_H: i32 = 30;
/// A selection that jumps further than this many rows does not glide (typing re-ranks the list).
const GLIDE_MAX: f32 = 8.0;

/// Which modal surface is on screen (0 = none); a change restarts the entrance.
pub fn surface_id(o: &Overlay) -> u8 {
    match o {
        Overlay::None => 0,
        Overlay::Picker(p) => 16 + p.kind as u8,
        // Explorer name fields (rename, new file / folder) sit in the tree: no dim, no panel.
        Overlay::Prompt(p) if p.is_inline() => 8,
        Overlay::Prompt(_) => 2,
        Overlay::Settings(_) => 3,
        Overlay::TopMenu(_) => 4,
        Overlay::Ctx(m) => {
            if m.is_sheet() {
                6
            } else {
                5
            }
        }
        Overlay::Font(_) => 7,
    }
}

/// Surfaces that dim the window behind them (popover menus do not).
pub fn has_backdrop(id: u8) -> bool {
    matches!(id, 2 | 3 | 6 | 7) || id >= 16
}

/// Entrance state of the modal layer.
pub struct Fx {
    id: u8,
    /// Panel entrance, 0..1.
    pub open: Anim,
    /// Backdrop dim, 0..1 (also fades out after the surface closed).
    pub dim: Anim,
    /// Highlight of the picker list: the (fractional) row it sits on.
    pub sel: Anim,
}

impl Default for Fx {
    fn default() -> Self {
        Self { id: 0, open: Anim::new(1.0, dur::SLOW), dim: Anim::new(0.0, dur::BASE), sel: Anim::new(0.0, dur::FAST) }
    }
}

impl Fx {
    /// Follow the surface on screen and advance the animations by `dt`; true while anything moves.
    pub fn frame(&mut self, id: u8, dt: f64) -> bool {
        if id != self.id {
            self.id = id;
            if id != 0 {
                self.open.snap(0.0);
                self.open.to(1.0);
                self.sel.snap(0.0);
            }
            self.dim.to(if has_backdrop(id) { 1.0 } else { 0.0 });
        }
        let mut busy = self.open.step(dt);
        busy |= self.dim.step(dt);
        busy |= self.sel.step(dt);
        busy
    }

    /// Aim the list highlight at row `row`: glides, unless the jump is long.
    pub fn glide_to(&mut self, row: usize) {
        let t = row as f32;
        if (t - self.sel.target()).abs() > GLIDE_MAX {
            self.sel.snap(t);
        } else {
            self.sel.to(t);
        }
    }

    /// Entrance progress 0..1 of the panel (1 = settled).
    pub fn progress(&self) -> f32 {
        self.open.value.clamp(0.0, 1.0)
    }

    /// Dim strength 0..1.
    pub fn dim_k(&self) -> f32 {
        self.dim.value.clamp(0.0, 1.0)
    }
}

/// Pixels of a region of the canvas, kept to mix with what is drawn over it afterwards.
pub struct Snap {
    r: Rect,
    px: Vec<u32>,
}

fn mix_px(a: u32, b: u32, k: u32) -> u32 {
    let ch = |s: u32| ((((a >> s) & 0xff) * (256 - k) + ((b >> s) & 0xff) * k) >> 8) & 0xff;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

impl Canvas<'_> {
    /// Copy the pixels of `r` (logical px, clipped to the canvas).
    pub fn snap_region(&self, r: Rect) -> Snap {
        let d = self.to_dev(r).intersect(&Rect::new(0, 0, self.w, self.h));
        let mut px = Vec::with_capacity((d.w * d.h) as usize);
        for y in d.y..d.bottom() {
            let s = (y * self.w + d.x) as usize;
            px.extend_from_slice(&self.px[s..s + d.w as usize]);
        }
        Snap { r: d, px }
    }

    /// Mix what was drawn over the region since `snap` with the saved pixels: `k` = 1 shows the
    /// new picture, 0 the old one; the new picture sits `dy` logical px lower than it was drawn.
    pub fn blend_over_snap(&mut self, snap: &Snap, k: f32, dy: i32) {
        let d = snap.r;
        if d.w <= 0 || d.h <= 0 {
            return;
        }
        let dy = crate::ui::sc(dy);
        let kk = (k.clamp(0.0, 1.0) * 256.0) as u32;
        let mut drawn = Vec::with_capacity(snap.px.len());
        for y in d.y..d.bottom() {
            let s = (y * self.w + d.x) as usize;
            drawn.extend_from_slice(&self.px[s..s + d.w as usize]);
        }
        for row in 0..d.h {
            let dst = ((d.y + row) * self.w + d.x) as usize;
            let from = row - dy;
            for col in 0..d.w {
                let i = (row * d.w + col) as usize;
                let before = snap.px[i];
                let after = if (0..d.h).contains(&from) { drawn[(from * d.w + col) as usize] } else { before };
                self.px[dst + col as usize] = if before == after { before } else { mix_px(before, after, kk) };
            }
        }
    }
}

/// The chips of a shortcut: "Ctrl+Shift+P" -> ["Ctrl", "Shift", "P"], "Ctrl+K Ctrl+S" -> four chips,
/// a trailing "+" is the key itself ("Ctrl++").
pub fn keycaps(combo: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chord in combo.split_whitespace() {
        let mut rest = chord;
        while !rest.is_empty() {
            if let Some(p) = rest.strip_prefix('+') {
                // A "+" that is a key: the last one of "Ctrl++".
                if p.is_empty() {
                    out.push("+".to_string());
                }
                rest = p;
                continue;
            }
            let end = rest.find('+').unwrap_or(rest.len());
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

/// Width taken by the chips `keys`.
pub fn keycaps_width(tx: &mut TextCtx, keys: &[String]) -> i32 {
    let w: i32 = keys.iter().map(|k| (tx.ui_width(k, false) + 2 * KEY_PAD).max(KEY_H)).sum();
    w + KEY_GAP * keys.len().saturating_sub(1) as i32
}

/// Draw the chips with their top-left corner at (`x`, `y`), faded by `alpha`; returns their width.
pub fn draw_keycaps(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    c: &Chrome,
    x: i32,
    y: i32,
    keys: &[String],
    alpha: f32,
    clip: Rect,
) -> i32 {
    let mut cx = x;
    for k in keys {
        let w = (tx.ui_width(k, false) + 2 * KEY_PAD).max(KEY_H);
        let chip = Rect::new(cx, y, w, KEY_H);
        cv.card(chip, ds::R_CTRL, anim::fade(c.input_bg, alpha), anim::fade(c.border, alpha));
        let tw = tx.ui_width(k, false);
        tx.draw_ui(cv, chip.x + (w - tw) / 2, chip.y + 1, k, anim::fade(c.tab_inactive_fg, alpha), clip, false);
        cx += w + KEY_GAP;
    }
    (cx - x - KEY_GAP).max(0)
}

impl App {
    /// Per frame: follow the open surface, aim the list highlight, advance the entrance.
    pub fn modal_frame(&mut self, dt: f64) -> bool {
        let id = surface_id(&self.overlay);
        let row = match &self.overlay {
            Overlay::Picker(p) => Some(p.sel),
            _ => None,
        };
        let fresh = id != self.modal.id;
        let busy = self.modal.frame(id, dt);
        if let Some(row) = row {
            if fresh {
                self.modal.sel.snap(row as f32);
            } else {
                self.modal.glide_to(row);
            }
        }
        // A refused explorer name shakes its field for a moment.
        let shaking = matches!(&self.overlay, Overlay::Prompt(p) if p.shaking());
        busy | self.modal.sel.moving() | shaking
    }

    /// Dim the window behind a modal (also while the dim fades out after it closed).
    pub fn draw_backdrop(&self, cv: &mut Canvas) {
        let k = self.modal.dim_k();
        if k <= 0.004 {
            return;
        }
        let top = if self.theme.background.luminance() < 0.5 { DIM_DARK } else { DIM_LIGHT };
        cv.fill(cv.bounds(), Rgba(0, 0, 0, (top * k) as u8));
    }

    /// Start of a panel entrance: the pixels of the band the modals live in, while it is moving.
    pub fn reveal_begin(&self, cv: &Canvas) -> Option<Snap> {
        if self.modal.progress() >= 0.999 || matches!(surface_id(&self.overlay), 0 | 4 | 5 | 8) {
            return None;
        }
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        let w = sw.min(960);
        Some(cv.snap_region(Rect::new((sw - w) / 2, 0, w, sh)))
    }

    /// End of a panel entrance: fade and rise from what was behind it.
    pub fn reveal_end(&self, cv: &mut Canvas, snap: Snap) {
        let k = self.modal.progress();
        cv.blend_over_snap(&snap, k, ((1.0 - k) * RISE as f32).round() as i32);
    }

    /// Shadow and rounded card of a floating panel at elevation `e`; returns the inner rect.
    pub fn modal_panel(&self, cv: &mut Canvas, r: Rect, e: Elev) -> Rect {
        let c = self.theme.chrome;
        let ring = anim::lerp_rgba(c.border, self.theme.foreground, e.lift() as f32 / 128.0);
        cv.shadow(r, ds::R_FLOAT);
        cv.card(r, ds::R_FLOAT, c.widget_bg, ring);
        r.inset(1)
    }
}

/// Look of a sheet button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnStyle {
    /// The answer to give: accent fill.
    Primary,
    /// The other choices: quiet, bordered.
    Secondary,
    /// Destructive: error text, error fill when focused.
    Danger,
}

/// One button of a confirmation sheet. `focused` is the keyboard default (Enter), `hot` the pointer.
pub fn draw_button(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    c: &Chrome,
    r: Rect,
    label: &str,
    style: BtnStyle,
    focused: bool,
    hot: bool,
) {
    let fg = c.tab_active_fg;
    let text = match style {
        BtnStyle::Primary => {
            cv.card(r, ds::R_ROW, if hot { c.accent.mix(fg, 0.14) } else { c.accent }, c.accent);
            c.on_accent
        }
        BtnStyle::Secondary => {
            cv.card(r, ds::R_ROW, c.widget_bg, c.border);
            if hot || focused {
                cv.fill_round(r.inset(1), ds::R_ROW - 1, c.list_hover_bg);
            }
            fg
        }
        BtnStyle::Danger => {
            cv.card(r, ds::R_ROW, c.widget_bg, c.error.with_alpha(0x90));
            if focused {
                cv.fill_round(r.inset(1), ds::R_ROW - 1, c.error);
                if c.error.luminance() > 0.55 { Rgba(0, 0, 0, 255) } else { Rgba(255, 255, 255, 255) }
            } else {
                if hot {
                    cv.fill_round(r.inset(1), ds::R_ROW - 1, c.error.with_alpha(0x28));
                }
                c.error
            }
        }
    };
    if focused {
        cv.ring(r.inset(-2), ds::R_ROW + 2, c.accent_text.with_alpha(0xa0));
    }
    let tw = tx.ui_width(label, true);
    tx.draw_ui(cv, r.x + (r.w - tw) / 2, r.y + (r.h - 18) / 2, label, text, r, true);
}

/// Footer hint row of a panel `r` (its last [`FOOT_H`] px): "key  action" pairs, left aligned.
pub fn draw_footer(cv: &mut Canvas, tx: &mut TextCtx, c: &Chrome, r: Rect, hints: &[(&str, &str)]) {
    let row = Rect::new(r.x, r.bottom() - FOOT_H, r.w, FOOT_H);
    cv.fill(Rect::new(row.x + ds::S4, row.y, row.w - 2 * ds::S4, 1), c.border);
    let mut x = row.x + ds::S4;
    let y = row.y + (FOOT_H - KEY_H) / 2 + 1;
    for (key, what) in hints {
        let keys = keycaps(key);
        x += draw_keycaps(cv, tx, c, x, y, &keys, 1.0, row) + ds::S2;
        tx.draw_ui(cv, x, y + 1, what, c.tab_inactive_fg, row, false);
        x += tx.ui_width(what, false) + ds::S4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_split_into_chips() {
        assert_eq!(keycaps("Ctrl+Shift+P"), ["Ctrl", "Shift", "P"]);
        assert_eq!(keycaps("Ctrl++"), ["Ctrl", "+"]);
        assert_eq!(keycaps("Ctrl+K Ctrl+S"), ["Ctrl", "K", "Ctrl", "S"]);
        assert_eq!(keycaps("Esc"), ["Esc"]);
        assert!(keycaps("").is_empty());
    }

    #[test]
    fn entrance_restarts_per_surface_and_settles() {
        let mut fx = Fx::default();
        assert!(!fx.frame(0, 0.016), "nothing open, nothing moves");
        assert!(fx.frame(16, 0.016));
        assert!(fx.progress() < 1.0 && fx.dim_k() > 0.0);
        let mut n = 0;
        while fx.frame(16, 0.016) {
            n += 1;
            assert!(n < 200, "did not settle");
        }
        assert_eq!(fx.progress(), 1.0);
        assert_eq!(fx.dim_k(), 1.0);
        // Closing fades the dim out and leaves the panel alone.
        fx.frame(0, 0.016);
        while fx.frame(0, 0.016) {}
        assert_eq!(fx.dim_k(), 0.0);
        // A popover does not dim.
        fx.frame(5, 0.016);
        while fx.frame(5, 0.016) {}
        assert_eq!(fx.dim_k(), 0.0);
    }

    #[test]
    fn highlight_glides_short_jumps_and_snaps_long_ones() {
        let mut fx = Fx::default();
        fx.glide_to(2);
        assert_eq!(fx.sel.value, 0.0, "a short jump glides");
        assert!(fx.sel.moving());
        fx.glide_to(40);
        assert_eq!(fx.sel.value, 40.0, "a long jump lands at once");
    }

    #[test]
    fn mixing_pixels_is_a_linear_blend() {
        assert_eq!(mix_px(0x000000, 0xff8040, 0), 0x000000);
        assert_eq!(mix_px(0x000000, 0xff8040, 256) & 0xffffff, 0xff8040 & 0xffffff);
        assert_eq!(mix_px(0x00ff00, 0x00ff00, 128), 0x00ff00);
    }

    #[test]
    fn blend_shows_the_old_picture_at_zero_and_the_new_at_one() {
        let mut px = vec![0x102030u32; 16 * 16];
        let mut cv = Canvas { px: &mut px, w: 16, h: 16 };
        let snap = cv.snap_region(Rect::new(0, 0, 16, 16));
        cv.fill(Rect::new(4, 4, 8, 8), Rgba(255, 255, 255, 255));
        cv.blend_over_snap(&snap, 1.0, 0);
        assert_eq!(cv.px[8 * 16 + 8], 0xffffff);
        let mut px2 = vec![0x102030u32; 16 * 16];
        let mut cv2 = Canvas { px: &mut px2, w: 16, h: 16 };
        let snap2 = cv2.snap_region(Rect::new(0, 0, 16, 16));
        cv2.fill(Rect::new(4, 4, 8, 8), Rgba(255, 255, 255, 255));
        cv2.blend_over_snap(&snap2, 0.0, 0);
        assert_eq!(cv2.px[8 * 16 + 8], 0x102030);
    }
}
