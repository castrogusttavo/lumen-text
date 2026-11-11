//! Image tabs in the app: zoom / pan input, the SVG "Preview | Source" switch, the bottom bar
//! read-out and the animation timer. The picture itself is drawn by `imgview`.

use crate::{
    app::{App, Focus, KeyIn},
    imgview::ImgView,
    ui::{Canvas, Rect},
};
use std::time::Instant;
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

/// Pan step of the arrow keys and of one wheel notch (logical px).
const PAN_STEP: f64 = 40.0;

impl App {
    /// The picture of the active tab, when it shows one.
    fn img_shown(&self) -> Option<&ImgView> {
        self.docs.get(self.active).filter(|d| d.showing_image()).and_then(|d| d.img.as_deref())
    }

    fn img_shown_mut(&mut self) -> Option<&mut ImgView> {
        self.docs.get_mut(self.active).filter(|d| d.showing_image()).and_then(|d| d.img.as_deref_mut())
    }

    /// The active tab is an SVG (its `Ctrl+Shift+V` is "preview / source", not "paste").
    pub fn active_is_svg(&self) -> bool {
        self.docs.get(self.active).and_then(|d| d.img.as_ref()).is_some_and(|v| v.kind == crate::imgview::Kind::Svg)
    }

    /// SVG tab: flip between the picture and the source. False when the active tab is no SVG.
    pub fn img_toggle_source(&mut self) -> bool {
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        if !d.img_toggle() {
            return false;
        }
        let msg = if d.showing_image() { "SVG: preview" } else { "SVG: source" };
        self.say(msg);
        self.request_redraw();
        true
    }

    /// `Ctrl+=` / `Ctrl+-` / `Ctrl+0` on an image tab zoom the picture (not the font).
    pub fn img_zoom_cmd(&mut self, dir: i32) -> bool {
        let Some(v) = self.img_shown_mut() else { return false };
        v.zoom_step(dir);
        let fit = v.is_fit();
        if dir == 0 {
            self.say(if fit { "zoom: fit to window" } else { "zoom: 100%" });
        }
        self.request_redraw();
        true
    }

    /// Wheel over the picture: `Ctrl` zooms around the pointer, otherwise it pans.
    pub fn img_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let (ctrl, shift) = (self.mods.control_key(), self.mods.shift_key());
        let Some(v) = self.img_shown_mut().filter(|v| v.rect.contains(x, y)) else { return false };
        if ctrl {
            let factor = match delta {
                MouseScrollDelta::LineDelta(_, dy) => 1.15f32.powf(dy),
                MouseScrollDelta::PixelDelta(p) => (p.y as f32 / 300.0).exp(),
            };
            v.zoom_at(factor, x, y);
        } else {
            let (mut dx, mut dy) = crate::scroller::wheel_px_xy(delta, PAN_STEP);
            if shift && dx == 0.0 {
                (dx, dy) = (dy, 0.0);
            }
            v.pan_by(-dx, -dy);
        }
        true
    }

    /// Press on an image tab: the SVG switch, or the start of a pan drag. True when taken.
    pub fn img_mouse_down(&mut self, x: f64, y: f64) -> bool {
        let Some(d) = self.docs.get(self.active) else { return false };
        let Some(v) = d.img.as_deref() else { return false };
        if let Some(&(_, preview)) = v.toggle.iter().find(|(r, _)| r.contains(x, y)) {
            if preview != v.preview {
                self.img_toggle_source();
            }
            return true;
        }
        let Some(v) = self.img_shown_mut().filter(|v| v.rect.contains(x, y)) else { return false };
        v.begin_drag(x, y);
        self.focus = Focus::Editor;
        true
    }

    /// Pointer moves over an image tab: drags the picture. True when the image tab took the move
    /// (no text hover there).
    pub fn img_mouse_move(&mut self, x: f64, y: f64) -> bool {
        let Some(v) = self.img_shown_mut() else { return false };
        if v.drag.is_some() {
            v.drag_to(x, y);
            self.request_redraw();
            return true;
        }
        v.rect.contains(x, y)
    }

    pub fn img_release(&mut self) {
        for v in self.docs.iter_mut().filter_map(|d| d.img.as_deref_mut()) {
            v.drag = None;
        }
    }

    /// Hand over the switch, open / closed hand over the picture.
    pub fn img_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let v = self.docs.get(self.active)?.img.as_deref()?;
        if v.toggle.iter().any(|(r, _)| r.contains(x, y)) {
            return Some(CursorIcon::Pointer);
        }
        let v = self.img_shown()?;
        if v.drag.is_some() {
            return Some(CursorIcon::Grabbing);
        }
        if !v.rect.contains(x, y) {
            return None;
        }
        let s = crate::ui::scale() as f64;
        let on_picture = v.dev_img.contains(x * s, y * s);
        Some(if on_picture { CursorIcon::Grab } else { CursorIcon::Default })
    }

    /// Keys on a tab showing a picture: arrows pan, `+` / `-` / `0` zoom; nothing else edits the
    /// (hidden) text. True when the key was for the picture.
    pub fn img_key(&mut self, ev: &KeyIn) -> bool {
        let Some(v) = self.img_shown_mut() else { return false };
        match &ev.key {
            Key::Named(NamedKey::ArrowLeft) => v.pan_by(PAN_STEP, 0.0),
            Key::Named(NamedKey::ArrowRight) => v.pan_by(-PAN_STEP, 0.0),
            Key::Named(NamedKey::ArrowUp) => v.pan_by(0.0, PAN_STEP),
            Key::Named(NamedKey::ArrowDown) => v.pan_by(0.0, -PAN_STEP),
            Key::Character(c) if matches!(c.as_str(), "+" | "=") => v.zoom_step(1),
            Key::Character(c) if c.as_str() == "-" => v.zoom_step(-1),
            Key::Character(c) if c.as_str() == "0" => v.zoom_step(0),
            _ => {}
        }
        true
    }

    /// Animated pictures on screen ask for a redraw when their next frame is due; still ones (and
    /// tabs not on screen) cost nothing.
    pub fn img_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let mut shown: Vec<usize> = vec![self.active];
        if let Some(sp) = self.split.as_ref() {
            shown.extend(sp.panes.iter().map(|p| p.doc));
        }
        let mut redraw = false;
        for i in shown {
            let Some(v) = self.docs.get(i).filter(|d| d.showing_image()).and_then(|d| d.img.as_deref()) else {
                continue;
            };
            if let Some(due) = v.anim_due(now) {
                if due <= now {
                    redraw = true;
                } else {
                    want(due);
                }
            }
        }
        if redraw {
            self.request_redraw();
        }
    }

    /// Bottom bar read-out of an image tab ("1920 × 1080 px · 245.3 KB · 37%"); returns the next x.
    pub fn draw_img_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        let Some(text) = self.img_shown().map(ImgView::status_text) else { return x };
        let r = self.layout.status;
        let w = self.tx.ui_width(&text, false);
        let dim = self.theme.chrome.tab_inactive_fg;
        self.tx.draw_ui(cv, x + 4, r.y + (r.h - 18) / 2, &text, dim, Rect::new(x, r.y, w + 12, r.h), false);
        x + w + 16
    }

    /// Text of the image read-out (tests and tooltips).
    #[cfg(test)]
    pub fn img_status(&self) -> Option<String> {
        self.img_shown().map(ImgView::status_text)
    }
}

#[cfg(test)]
mod tests {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use crate::ui::Canvas;

    /// 4 x 2 RGBA PNG: top row opaque red, opaque green, half-transparent blue, transparent; bottom
    /// row opaque white, opaque black, transparent, opaque red.
    const PNG_4X2: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x04, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x7f, 0xa8, 0x7d, 0x63, 0x00, 0x00, 0x00,
        0x1c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x0c, 0x19, 0xfe, 0x37, 0x30,
        0x80, 0xc0, 0x7f, 0x20, 0x00, 0x51, 0x0c, 0x10, 0xe2, 0x3f, 0x00, 0xde, 0x4e, 0x0c, 0x75, 0xab, 0x24, 0xf5,
        0x81, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn frame(app: &mut crate::app::App, px: &mut [u32], w: i32, h: i32) -> bool {
        app.render_into(&mut Canvas { px, w, h }, 0.016)
    }

    /// Render until the picture is decoded (the worker thread answers quickly).
    fn settle(app: &mut crate::app::App, px: &mut [u32], w: i32, h: i32) {
        for _ in 0..400 {
            let busy = frame(app, px, w, h);
            let pending = app.docs.get(app.active).and_then(|d| d.img.as_ref()).is_some_and(|v| v.pending());
            if !busy && !pending {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the picture never settled");
    }

    #[test]
    fn a_png_opens_as_a_picture_zooms_pans_and_lets_go_of_its_pixels() {
        let _g = lock();
        let dir = tmp("img-tab");
        let mut app = test_app(&dir);
        let path = dir.join("pixels.png");
        std::fs::write(&path, PNG_4X2).unwrap();
        let i = app.open_path(&path, None).expect("opens");
        app.active = i;
        assert!(app.docs[i].showing_image(), "a PNG shows as a picture");
        assert_eq!(app.docs[i].buf.len_chars(), 0, "no binary garbage in a text buffer");
        let (w, h) = (900, 500);
        app.size = (w as u32, h as u32);
        let mut px = vec![0u32; (w * h) as usize];
        settle(&mut app, &mut px, w, h);
        let st = app.img_status().unwrap();
        assert!(st.starts_with("4 × 2 px · ") && st.ends_with("· 100%"), "{st}");

        // Zoom in far enough to see every pixel as a block (100 → 125 → 150 → 200 → 300 → 400 → 600 %).
        for _ in 0..6 {
            assert!(app.img_zoom_cmd(1));
        }
        settle(&mut app, &mut px, w, h);
        let v = app.docs[i].img.as_ref().unwrap();
        let (r, z) = (v.dev_img, v.shown_zoom);
        assert_eq!(z, 6.0);
        let at = |px: &[u32], fx: f64, fy: f64| {
            let x = r.x + (r.w as f64 * fx) as i32;
            let y = r.y + (r.h as f64 * fy) as i32;
            px[(y * w + x) as usize]
        };
        assert_eq!(at(&px, 0.125, 0.25), 0xff0000, "opaque red");
        assert_eq!(at(&px, 0.375, 0.25), 0x00ff00, "opaque green");
        assert_eq!(at(&px, 0.125, 0.75), 0xffffff, "opaque white");
        assert_eq!(at(&px, 0.375, 0.75), 0x000000, "opaque black");
        assert_eq!(at(&px, 0.875, 0.75), 0xff0000, "opaque red");
        // Transparent pixel: the checkerboard (dark theme greys), never the editor background.
        let t = at(&px, 0.875, 0.25);
        assert!(t == 0x3a3a3a || t == 0x2c2c2c, "{t:06x}");
        // Half-transparent blue over the checkerboard: blue wins, the grey shows through.
        let b = at(&px, 0.625, 0.25);
        assert!((b & 0xff) > 0x80 && (b >> 16) < 0x40, "{b:06x}");
        assert!(app.img_status().unwrap().ends_with("· 600%"));

        // Typing does not edit the hidden text; arrows pan (the picture is smaller than the pane: it stays centred).
        app.commit_text("x");
        assert_eq!(app.docs[i].buf.len_chars(), 0);
        // Ctrl+0: back to fit (100 % for a picture this small).
        assert!(app.img_zoom_cmd(0));
        assert!(app.img_zoom_cmd(0));
        settle(&mut app, &mut px, w, h);
        assert!(app.img_status().unwrap().ends_with("· 100%"));
        assert!(app.docs[i].img.as_ref().unwrap().pixel_bytes() > 0);

        // Saving never writes the empty text over the picture.
        app.save_active();
        assert_eq!(std::fs::read(&path).unwrap(), PNG_4X2);

        // Session (kept in the scratch folder): the tab comes back as a picture.
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.join(".state")) };
        app.save_session();
        app.close_tab(i, true);
        assert!(app.docs.iter().all(|d| d.img.is_none()), "closing the tab drops the picture");
        app.restore_session();
        let j = app.docs.iter().position(|d| d.path() == Some(path.as_path())).expect("restored");
        assert!(app.docs[j].showing_image());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_svg_has_a_preview_and_an_editable_source() {
        let _g = lock();
        let dir = tmp("img-svg");
        let mut app = test_app(&dir);
        let path = dir.join("square.svg");
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000"/></svg>"##;
        std::fs::write(&path, svg).unwrap();
        let i = app.open_path(&path, None).unwrap();
        app.active = i;
        assert!(app.docs[i].showing_image());
        assert_eq!(app.docs[i].buf.to_string(), svg, "the source is kept as text");
        let (w, h) = (900, 500);
        app.size = (w as u32, h as u32);
        let mut px = vec![0u32; (w * h) as usize];
        settle(&mut app, &mut px, w, h);
        let r = app.docs[i].img.as_ref().unwrap().dev_img;
        assert_eq!(px[((r.y + r.h / 2) * w + r.x + r.w / 2) as usize], 0xff0000, "rasterized");
        assert!(app.img_status().unwrap().starts_with("10 × 10 px"));
        // Typing in the preview does not touch the source.
        app.commit_text("x");
        assert_eq!(app.docs[i].buf.to_string(), svg);
        // The switch: source is the normal (editable) text editor.
        assert!(app.img_toggle_source());
        assert!(!app.docs[i].showing_image() && !app.docs[i].buf.read_only);
        frame(&mut app, &mut px, w, h);
        let sw = app.docs[i].img.as_ref().unwrap().toggle.clone();
        assert_eq!(sw.len(), 2, "Preview | Source buttons");
        // A click on "Preview" goes back.
        let (pr, _) = sw.iter().find(|(_, p)| *p).copied().unwrap();
        assert!(app.img_mouse_down(pr.x as f64 + 2.0, pr.y as f64 + 2.0));
        assert!(app.docs[i].showing_image() && app.docs[i].buf.read_only);
        // A PNG has no source to show.
        let png = dir.join("p.png");
        std::fs::write(&png, PNG_4X2).unwrap();
        let k = app.open_path(&png, None).unwrap();
        app.active = k;
        assert!(!app.img_toggle_source());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
