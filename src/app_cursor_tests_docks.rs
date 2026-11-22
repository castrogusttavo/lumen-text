//! Pointer sweeps of the right-hand dock (Git | AI) and the terminals: every hit region the dock
//! drew answers the shape of its kind, disabled controls say "not allowed", dead areas are arrows,
//! the left edge resizes (also while dragging it) and a modal hides all of it.

use crate::{
    app::App,
    app_cursor::{CursorReg, Z_BASE, Z_FLOAT},
    app_gitpanel::{Hit, cursor::git_hit_icon},
    app_unsaved::testkit::{lock, test_app, tmp},
    ui::{Canvas, Rect},
};
use winit::window::CursorIcon as C;

const W: i32 = 1400;
const H: i32 = 900;
const STEP: usize = 8;

fn frame(app: &mut App) {
    app.size = (W as u32, H as u32);
    let mut px = vec![0u32; (W * H) as usize];
    for _ in 0..3 {
        app.toasts = Default::default();
        app.render_into(&mut Canvas { px: &mut px, w: W, h: H }, 0.016);
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

fn is_resize(c: C) -> bool {
    matches!(c, C::ColResize | C::RowResize | C::EwResize | C::NsResize)
}

/// Probe a grid over `r`.
fn sweep_in(app: &mut App, r: Rect) -> Vec<(f64, f64, C, &'static str)> {
    let mut v = Vec::new();
    let mut y = r.y + 1;
    while y < r.bottom() {
        let mut x = r.x + 1;
        while x < r.right() {
            let (icon, label) = probe(app, x as f64, y as f64);
            v.push((x as f64, y as f64, icon, label));
            x += STEP as i32;
        }
        y += STEP as i32;
    }
    v
}

#[test]
fn the_git_dock_answers_per_control() {
    let _g = lock();
    let dir = tmp("cur-docks-git");
    let mut app = test_app(&dir);
    app.open_git_panel();
    frame(&mut app);
    let slot = app.slot_rect();
    // Left edge: resize, in the gap between the cards and just inside the dock.
    for dx in [-3, 0, 3] {
        let (icon, label) = probe(&mut app, (slot.x + dx) as f64, (slot.y + slot.h / 2) as f64);
        assert_eq!(icon, C::ColResize, "dock edge at dx={dx}: {label}");
    }
    // Every hit of the panel answers its kind (the last one drawn wins, like the click).
    let hits: Vec<(Rect, Hit)> = app.gitpanel.as_ref().map(|g| g.hits.clone()).unwrap_or_default();
    assert!(!hits.is_empty(), "the panel lays out controls");
    for (r, _) in &hits {
        if r.w < 4 || r.h < 4 {
            continue;
        }
        let (x, y) = center(*r);
        let top = app.gitpanel.as_ref().and_then(|g| g.hit_at(x, y)).expect("a hit under its own center");
        let (want, _, _) = git_hit_icon(top);
        let (icon, label) = probe(&mut app, x, y);
        assert_eq!(icon, want, "{top:?} at ({x},{y}): {label}");
    }
    // The switch above the panel is a hand.
    let sw: Vec<Rect> = app.ai.hits.iter().map(|(r, _)| *r).collect();
    assert!(!sw.is_empty(), "the Git | AI switch is laid out");
    for r in sw {
        let (x, y) = center(r);
        assert_eq!(probe(&mut app, x, y).0, C::Pointer, "switch {r:?}");
    }
    // Dead areas are arrows: no beam or hand without a hit, no resize but the edge.
    let gp_rect = app.gitpanel_rect();
    for (x, y, icon, label) in sweep_in(&mut app, gp_rect) {
        if x < (slot.x + 6) as f64 {
            continue;
        }
        let on_hit = hits.iter().any(|(r, _)| r.contains(x, y));
        if !on_hit {
            assert_eq!(icon, C::Default, "dead area ({x},{y}): {label}");
        }
        assert!(!is_resize(icon), "resize inside the dock at ({x},{y}): {label}");
    }
}

#[test]
fn the_ai_dock_answers_per_control_and_disabled_ones_are_not_allowed() {
    let _g = lock();
    let dir = tmp("cur-docks-ai");
    let mut app = test_app(&dir);
    app.ai.open = true;
    frame(&mut app);
    let slot = app.slot_rect();
    assert_eq!(probe(&mut app, slot.x as f64, (slot.y + 200) as f64).0, C::ColResize, "AI dock edge");
    let hits = app.ai.hits.clone();
    assert!(!hits.is_empty());
    for (r, h) in &hits {
        let (x, y) = center(*r);
        let top = app.ai.hit_at(x, y).map(|(_, h)| h).expect("a hit under its center");
        let want = if top == crate::app_aipanel::AiHit::Filter { C::Text } else { C::Pointer };
        let (icon, label) = probe(&mut app, x, y);
        assert_eq!(icon, want, "{h:?} at {r:?}: {label}");
    }
    // Stop and Restart have nothing to act on in a fresh dock: dimmed and "not allowed".
    assert!(!app.ai.disabled.is_empty(), "a fresh AI dock has dimmed buttons");
    for r in app.ai.disabled.clone() {
        let (x, y) = center(r);
        assert!(!hits.iter().any(|(h, _)| h.contains(x, y)), "a disabled control is not also a hit: {r:?}");
        assert_eq!(probe(&mut app, x, y).0, C::NotAllowed, "disabled {r:?}");
    }
    // No agent is running: nothing in the dock is text.
    let p = app.aipanel_rect();
    for (x, y, icon, label) in sweep_in(&mut app, p) {
        if x < (slot.x + 6) as f64 {
            continue;
        }
        assert_ne!(icon, C::Text, "no terminal yet at ({x},{y}): {label}");
        assert!(!is_resize(icon), "({x},{y}): {icon:?} {label}");
    }
}

#[test]
fn dragging_the_dock_edge_keeps_the_resize_cursor_everywhere() {
    let _g = lock();
    let dir = tmp("cur-docks-drag");
    let mut app = test_app(&dir);
    app.open_git_panel();
    frame(&mut app);
    let slot = app.slot_rect();
    app.mouse = (slot.x as f64, (slot.y + 100) as f64);
    assert!(app.aipanel_mouse(slot.x as f64, (slot.y + 100) as f64, 0) && app.ai.resizing);
    for (x, y) in [(5.0, 5.0), (700.0, 400.0), ((W - 3) as f64, (H - 3) as f64)] {
        assert_eq!(probe(&mut app, x, y).0, C::ColResize, "dragging, pointer at ({x},{y})");
    }
    app.aipanel_release();
    assert!(!app.ai.resizing);
}

#[test]
fn a_modal_hides_the_dock() {
    let _g = lock();
    for ai in [false, true] {
        let dir = tmp(if ai { "cur-docks-modal-ai" } else { "cur-docks-modal-git" });
        let mut app = test_app(&dir);
        if ai {
            app.ai.open = true;
        } else {
            app.open_git_panel();
        }
        frame(&mut app);
        app.open_commands();
        frame(&mut app);
        let rows = app.layout.overlay_rows.clone();
        let slot = app.slot_rect();
        for (x, y, icon, label) in sweep_in(&mut app, slot) {
            if rows.iter().any(|(r, _)| r.contains(x, y)) {
                continue;
            }
            assert_ne!(icon, C::Pointer, "dock hand leaks through the palette at ({x},{y}): {label}");
            assert_ne!(icon, C::NotAllowed, "({x},{y}): {label}");
            assert!(!is_resize(icon), "dock edge leaks through the palette at ({x},{y}): {label}");
        }
        let (icon, label) = probe(&mut app, (slot.x + 1) as f64, (slot.y + 100) as f64);
        assert_ne!(icon, C::ColResize, "edge under a modal: {label}");
    }
}

#[test]
fn the_closed_terminal_panel_is_not_text() {
    let _g = lock();
    let dir = tmp("cur-docks-term");
    let mut app = test_app(&dir);
    app.toggle_panel();
    app.panel.tab = crate::app_panel::PanelTab::Terminal;
    frame(&mut app);
    let p = app.layout.panel;
    assert!(p.h > 0, "the panel is on screen");
    if app.terms.panes.is_empty() {
        let (x, y) = center(Rect::new(p.x, p.y + 60, p.w, p.h - 60));
        let (icon, label) = probe(&mut app, x, y);
        assert_eq!(icon, C::Default, "the empty state has nothing to select: {label}");
    } else {
        for (r, _) in app.terms.pane_rects.clone() {
            let (x, y) = center(r);
            assert_eq!(probe(&mut app, x, y).0, C::Text, "terminal pane {r:?}");
        }
    }
}

#[test]
fn a_terminal_link_beats_the_text_beam() {
    let mut reg = CursorReg::default();
    let pane = Rect::new(0, 0, 400, 200);
    reg.push(pane, C::Text, Z_BASE - 1, "terminal");
    reg.push(Rect::new(40, 20, 120, 18), C::Pointer, Z_FLOAT, "terminal-link");
    assert_eq!(reg.topmost(50.0, 25.0, 0).map(|r| (r.icon, r.label)), Some((C::Pointer, "terminal-link")));
    assert_eq!(reg.topmost(300.0, 100.0, 0).map(|r| r.icon), Some(C::Text));
}

#[test]
fn git_hits_map_to_their_shapes() {
    assert_eq!(git_hit_icon(Hit::Msg).0, C::Text);
    assert_eq!(git_hit_icon(Hit::Filter).0, C::Text);
    assert_eq!(git_hit_icon(Hit::Refresh).0, C::Pointer);
    assert_eq!(git_hit_icon(Hit::Row(0)).0, C::Pointer);
    assert_eq!(git_hit_icon(Hit::MenuItem(usize::MAX)).0, C::Default, "menu padding is dead");
    assert_eq!(git_hit_icon(Hit::MenuItem(1)).0, C::Pointer);
}
