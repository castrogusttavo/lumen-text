//! Pointer and placement sweeps for the overlays area: palette, context menu, font picker, the
//! update card and the toasts that float over all of them.

use crate::{
    app::App,
    app_unsaved::testkit::{lock, test_app, tmp},
    ui::{Canvas, Rect},
};
use winit::window::CursorIcon as C;

const W: i32 = 1400;
const H: i32 = 900;
const STEP: usize = 8;

/// Two frames, keeping whatever toasts the test pushed.
fn frame(app: &mut App) {
    app.size = (W as u32, H as u32);
    let mut px = vec![0u32; (W * H) as usize];
    for _ in 0..2 {
        app.render_into(&mut Canvas { px: &mut px, w: W, h: H }, 0.016);
    }
}

fn probe(app: &mut App, x: f64, y: f64) -> (C, &'static str) {
    app.mouse = (x, y);
    app.cursor_probe()
}

fn center(r: Rect) -> (f64, f64) {
    (r.x as f64 + r.w as f64 / 2.0, r.y as f64 + r.h as f64 / 2.0)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    let i = a.intersect(&b);
    i.w > 0 && i.h > 0
}

#[test]
fn toasts_never_cover_the_controls_of_a_menu() {
    let _g = lock();
    let dir = tmp("cur-ov-toast");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.toasts = Default::default();
    // A menu opened in the bottom right corner, where the toasts usually sit.
    app.open_ctxmenu(crate::app_ctxmenu::Target::Tab(0), (W - 200, H - 320));
    frame(&mut app);
    for i in 0..3 {
        app.notify(crate::ui_toast::ToastKind::Info, format!("message number {i}"), None);
    }
    frame(&mut app);
    let rows: Vec<Rect> = app.layout.overlay_rows.iter().map(|(r, _)| *r).collect();
    let cards = app.toasts.cards();
    assert!(!rows.is_empty() && !cards.is_empty());
    for c in &cards {
        for r in &rows {
            assert!(!overlaps(*c, *r), "toast {c:?} covers menu row {r:?}");
        }
        // (They slide in sideways, so only the vertical extent is checked.)
        assert!(c.y >= 0 && c.bottom() <= H, "toast {c:?} leaves the window");
    }
}

#[test]
fn toasts_keep_their_corner_when_nothing_is_in_the_way() {
    let _g = lock();
    let dir = tmp("cur-ov-toast-free");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.toasts = Default::default();
    app.notify(crate::ui_toast::ToastKind::Info, "saved", None);
    frame(&mut app);
    let c = app.toasts.cards()[0];
    assert!(c.right() > W - 40 && c.bottom() > H - 40, "bottom right: {c:?}");
}

#[test]
fn the_font_picker_has_an_input_rows_and_a_dead_backdrop() {
    let _g = lock();
    let dir = tmp("cur-ov-font");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.open_font_picker("ui_font_family", false);
    frame(&mut app);
    app.toasts = Default::default();
    let mut all: Vec<(f64, f64, C, &'static str)> = Vec::new();
    for y in (1..H).step_by(STEP) {
        for x in (1..W).step_by(STEP) {
            let (i, l) = probe(&mut app, x as f64, y as f64);
            all.push((x as f64, y as f64, i, l));
        }
    }
    assert!(all.iter().any(|(_, _, i, l)| *l == "font-input" && *i == C::Text), "an I-beam on the search field");
    assert!(all.iter().any(|(_, _, i, l)| *l == "font-row" && *i == C::Pointer), "a hand on the rows");
    for (x, y, icon, label) in &all {
        if !label.starts_with("font-") {
            assert!(!matches!(icon, C::Pointer | C::ColResize | C::RowResize), "leak at ({x},{y}): {icon:?} {label}");
        }
    }
    assert_eq!(probe(&mut app, 3.0, (H - 3) as f64).0, C::Default);
}

#[test]
fn the_update_card_does_not_leak_through_a_modal() {
    let _g = lock();
    let dir = tmp("cur-ov-update");
    let mut app = test_app(&dir);
    app.upd.card = Some(crate::app_update::Card::About);
    frame(&mut app);
    app.toasts = Default::default();
    let card = app.upd.card_rect;
    assert!(card.h > 0, "the card is on screen");
    let btn = app.upd.rects.last().map(|(r, _)| *r).expect("the card has buttons");
    let (x, y) = center(btn);
    assert_eq!(probe(&mut app, x, y).0, C::Pointer, "its button is a hand while nothing covers it");
    app.open_commands();
    frame(&mut app);
    app.toasts = Default::default();
    let under_rows = app.layout.overlay_rows.iter().any(|(r, _)| r.contains(x, y));
    if !under_rows {
        assert_ne!(probe(&mut app, x, y).0, C::Pointer, "the covered card shows no hand under the backdrop");
    }
}

#[test]
fn the_palette_field_is_an_ibeam() {
    let _g = lock();
    let dir = tmp("cur-ov-palette");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.open_commands();
    frame(&mut app);
    app.toasts = Default::default();
    let mut ibeam = 0;
    for y in (1..H).step_by(STEP) {
        for x in (1..W).step_by(STEP) {
            let (icon, label) = probe(&mut app, x as f64, y as f64);
            if icon == C::Text {
                assert_eq!(label, "cascade:overlay-input", "({x},{y})");
                ibeam += 1;
            }
        }
    }
    assert!(ibeam > 0, "the query field shows the I-beam");
}
