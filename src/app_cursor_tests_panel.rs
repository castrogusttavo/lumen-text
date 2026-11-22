//! Pointer sweeps of the bottom panel: its top resize edge, the Problems rows, the Debug toolbar
//! (disabled buttons answer not-allowed), the Docker buttons, and the modal shadow over all of it.

use crate::{
    app::App,
    app_panel::PanelTab,
    app_unsaved::testkit::{lock, test_app, tmp},
    ui::{Canvas, Rect},
};
use winit::window::CursorIcon as C;

const W: i32 = 1400;
const H: i32 = 900;

/// Draw until the panel's slide has settled.
fn settle(app: &mut App) {
    app.size = (W as u32, H as u32);
    let mut px = vec![0u32; (W * H) as usize];
    for _ in 0..60 {
        app.toasts = Default::default();
        app.render_into(&mut Canvas { px: &mut px, w: W, h: H }, 0.05);
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

fn open_panel(tag: &str, tab: PanelTab) -> App {
    let dir = tmp(tag);
    let mut app = test_app(&dir);
    app.toggle_panel();
    app.panel.tab = tab;
    settle(&mut app);
    assert!(app.layout.panel.h > 0, "the panel is on screen");
    app
}

#[test]
fn the_top_edge_resizes_and_a_drag_keeps_the_arrow() {
    let _g = lock();
    let mut app = open_panel("cur-panel-edge", PanelTab::Output);
    let p = app.layout.panel;
    let (x, y) = ((p.x + p.w / 2) as f64, p.y as f64);
    assert_eq!(probe(&mut app, x, y), (C::RowResize, "panel-edge"));
    // The tab strip below the grab band is still a hand.
    let (tx, ty) = center(app.layout.panel_tabs[0].0);
    assert_eq!(probe(&mut app, tx, ty).0, C::Pointer);
    // Mid-drag the row-resize follows the pointer anywhere.
    app.panel.edge.grab = Some(0.0);
    assert_eq!(probe(&mut app, 100.0, 100.0).0, C::RowResize);
    app.panel.edge.grab = None;
    assert_ne!(probe(&mut app, 100.0, 100.0).0, C::RowResize);
}

#[test]
fn problems_rows_are_hands_and_the_rest_is_an_arrow() {
    let _g = lock();
    let mut app = open_panel("cur-panel-problems", PanelTab::Problems);
    let rects: Vec<Rect> = app.problems_hit_rects().collect();
    for r in &rects {
        let (x, y) = center(*r);
        assert_eq!(probe(&mut app, x, y).0, C::Pointer, "problems hit {r:?}");
        assert!(app.problems_clickable_at(x, y));
    }
    let b = app.layout.panel;
    let (icon, label) = probe(&mut app, (b.x + b.w / 2) as f64, (b.bottom() - 6) as f64);
    assert_eq!(icon, C::Default, "empty tail: {label}");
}

#[test]
fn disabled_debug_tools_are_not_allowed_and_enabled_ones_are_hands() {
    let _g = lock();
    let mut app = open_panel("cur-panel-debug", PanelTab::Debug);
    let hits = app.layout.debug_hits.clone();
    let mut tools = 0;
    for (r, h) in hits {
        if let crate::app_debug::DebugHit::Tool(t) = h {
            tools += 1;
            let want = if app.tool_enabled(t) { C::Pointer } else { C::NotAllowed };
            let (x, y) = center(r);
            assert_eq!(probe(&mut app, x, y).0, want, "tool {t:?}");
        }
    }
    assert!(tools > 0, "the toolbar is drawn");
}

#[test]
fn docker_buttons_are_hands_and_its_dead_area_is_an_arrow() {
    let _g = lock();
    let mut app = open_panel("cur-panel-docker", PanelTab::Docker);
    let rects: Vec<Rect> = app.docker_hit_rects().collect();
    assert!(!rects.is_empty(), "the refresh button is drawn");
    for r in &rects {
        let (x, y) = center(*r);
        assert_eq!(probe(&mut app, x, y).0, C::Pointer, "docker button {r:?}");
    }
    let b = app.layout.panel;
    let (icon, label) = probe(&mut app, (b.x + b.w / 2) as f64, (b.bottom() - 6) as f64);
    assert_eq!(icon, C::Default, "dead area: {label}");
}

#[test]
fn a_modal_shadows_the_panel_tabs() {
    let _g = lock();
    for tab in [PanelTab::Problems, PanelTab::Debug, PanelTab::Docker] {
        let mut app = open_panel("cur-panel-modal", tab);
        app.open_commands();
        settle(&mut app);
        let rows = app.layout.overlay_rows.clone();
        let p = app.layout.panel;
        for y in (p.y - 6..p.bottom()).step_by(8) {
            for x in (p.x..p.right()).step_by(24) {
                if rows.iter().any(|(r, _)| r.contains(x as f64, y as f64)) {
                    continue;
                }
                let (icon, label) = probe(&mut app, x as f64, y as f64);
                assert!(
                    !matches!(icon, C::Pointer | C::RowResize | C::NotAllowed),
                    "{tab:?} leaks {icon:?} at ({x},{y}): {label}"
                );
            }
        }
    }
}
