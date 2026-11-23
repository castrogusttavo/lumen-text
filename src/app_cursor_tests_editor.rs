//! Pointer sweeps of the editor surface: gutter, find bar, completion popup and the marks drawn
//! inside the text (see `docs/CURSOR_CHECKLIST.md`, "Editor card").

use crate::{
    app::App,
    app_lsp::{CItem, Completion},
    app_unsaved::testkit::{lock, test_app, tmp},
    ui::{Canvas, Rect},
};
use winit::window::CursorIcon as C;

const W: i32 = 1400;
const H: i32 = 900;

fn frame(app: &mut App) {
    app.size = (W as u32, H as u32);
    let mut px = vec![0u32; (W * H) as usize];
    for _ in 0..3 {
        app.toasts = Default::default();
        app.render_into(&mut Canvas { px: &mut px, w: W, h: H }, 0.5);
    }
    app.toasts = Default::default();
}

fn probe(app: &mut App, x: f64, y: f64) -> (C, &'static str) {
    app.mouse = (x, y);
    app.cursor_probe()
}

fn center(r: Rect) -> (f64, f64) {
    (r.x as f64 + r.w as f64 / 2.0, r.y as f64 + r.h as f64 / 2.0)
}

#[test]
fn the_gutter_is_an_arrow_and_the_text_a_beam() {
    let _g = lock();
    let dir = tmp("cur-ed-gutter");
    let mut app = test_app(&dir);
    frame(&mut app);
    let d = &app.docs[app.active];
    let (gutter, _) = d.zone_rects().expect("a text tab has zones");
    assert!(gutter.w > 0 && gutter.w == d.gutter_w());
    let (gx, gy) = (gutter.x as f64 + 4.0, gutter.y as f64 + 40.0);
    let (tx, ty) = ((gutter.right() + 30) as f64, gutter.y as f64 + 40.0);
    assert_eq!(probe(&mut app, gx, gy), (C::Default, "gutter"));
    assert_eq!(probe(&mut app, tx, ty).0, C::Text);
    // A modal hides the zone: still no hand, no beam.
    app.open_commands();
    frame(&mut app);
    let (icon, _) = probe(&mut app, tx, ty);
    assert_eq!(icon, C::Default, "the backdrop is an arrow over the text");
}

#[test]
fn the_minimap_is_a_hand_when_it_shows() {
    let _g = lock();
    let dir = tmp("cur-ed-minimap");
    let mut app = test_app(&dir);
    frame(&mut app);
    let Some((_, Some(m))) = app.docs[app.active].zone_rects() else { return };
    let (x, y) = center(m);
    assert_eq!(probe(&mut app, x, y), (C::Pointer, "minimap"));
}

#[test]
fn the_find_bar_has_a_zone_per_control() {
    let _g = lock();
    let dir = tmp("cur-ed-find");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.open_find(true);
    if let Some(f) = app.find.as_mut() {
        f.field.text = "zzz-no-match".into();
        f.field.cursor = 12;
    }
    frame(&mut app);
    assert!(app.find.as_ref().is_some_and(|f| f.matches.is_empty()), "nothing matches");
    let g = crate::app_overlay::find::find_geo(app.layout.editor, true);
    let (x, y) = center(g.field);
    assert_eq!(probe(&mut app, x, y), (C::Text, "find-field"));
    let (x, y) = center(g.rfield);
    assert_eq!(probe(&mut app, x, y), (C::Text, "find-field"));
    for b in [g.case, g.regex, g.close] {
        let (x, y) = center(b);
        assert_eq!(probe(&mut app, x, y), (C::Pointer, "find-button"));
    }
    // Nothing to step to or replace: those buttons say so.
    for b in [g.prev, g.next, g.replace, g.all] {
        let (x, y) = center(b);
        assert_eq!(probe(&mut app, x, y), (C::NotAllowed, "find-button"), "{b:?}");
    }
    // The card's dead area is an arrow, never the editor's beam beneath.
    assert_eq!(probe(&mut app, (g.card.x + 2) as f64, (g.card.y + 2) as f64), (C::Default, "find-card"));
    // With a match the stepping buttons are hands.
    if let Some(f) = app.find.as_mut() {
        f.field.text = "e".into();
        f.field.cursor = 1;
    }
    frame(&mut app);
    assert!(app.find.as_ref().is_some_and(|f| !f.matches.is_empty()), "the seed has an e");
    let (x, y) = center(g.next);
    assert_eq!(probe(&mut app, x, y), (C::Pointer, "find-button"));
    // No wait cursor anywhere over the bar.
    for yy in (g.card.y..g.card.bottom()).step_by(4) {
        for xx in (g.card.x..g.card.right()).step_by(4) {
            let (icon, label) = probe(&mut app, xx as f64, yy as f64);
            assert!(!matches!(icon, C::Wait | C::Progress), "({xx},{yy}) {label}");
            assert_ne!(label, "editor-text", "the bar covers the text at ({xx},{yy})");
        }
    }
}

#[test]
fn the_completion_card_is_an_arrow_and_its_rows_hands() {
    let _g = lock();
    let dir = tmp("cur-ed-completion");
    let mut app = test_app(&dir);
    frame(&mut app);
    let item = |l: &str| CItem {
        label: l.into(),
        detail: String::new(),
        insert: l.into(),
        kind: 6,
        sort: l.into(),
        range: None,
        raw: serde_json::json!({}),
    };
    app.docs[app.active].ed.sel = crate::buffer::Sel::caret(0);
    app.completion = Some(Completion {
        items: vec![item("alpha"), item("beta"), item("gamma")],
        filtered: vec![0, 1, 2],
        sel: 0,
        scroll: 0,
        start: 0,
        seed_len: 0,
        fade: Default::default(),
        vis: 10,
        rect: Rect::default(),
        docs_rect: Rect::default(),
    });
    frame(&mut app);
    let Some(c) = app.completion.as_ref() else { panic!("the popup survived the frame") };
    let card = c.rect;
    assert!(card.w > 0, "the card was drawn");
    assert!(c.vis >= 1);
    let rows = app.layout.completion_rows.clone();
    assert_eq!(rows.len(), 3);
    for (r, _) in &rows {
        let (x, y) = center(*r);
        assert_eq!(probe(&mut app, x, y), (C::Pointer, "completion-row"));
    }
    assert_eq!(probe(&mut app, (card.x + 1) as f64, (card.y + 1) as f64), (C::Default, "completion-card"));
}
