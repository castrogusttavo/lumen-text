//! Shell and navigation sweeps: the rail, title bar, menu bar, status bar and sidebar answer the
//! right pointer shape, an open menu switches on hover, and the sidebar card keeps its shape and
//! its pixels while it slides shut (see `docs/CURSOR_CHECKLIST.md`, "Window chrome" and "Sidebar").

use crate::{
    app::App,
    app_overlay::Overlay,
    app_topbar::MenuKind,
    app_unsaved::testkit::{lock, test_app, tmp},
    ui::{Canvas, Rect},
};
use winit::window::CursorIcon as C;

const W: i32 = 1400;
const H: i32 = 900;

/// Draw one frame of `dt` seconds and return its pixels.
fn shot(app: &mut App, dt: f64) -> Vec<u32> {
    app.size = (W as u32, H as u32);
    let mut px = vec![0u32; (W * H) as usize];
    app.toasts = Default::default();
    app.render_into(&mut Canvas { px: &mut px, w: W, h: H }, dt);
    app.toasts = Default::default();
    px
}

/// Two frames, so what starts animating on the first one has settled in the layout.
fn frame(app: &mut App) -> Vec<u32> {
    shot(app, 0.016);
    shot(app, 0.016)
}

fn probe(app: &mut App, x: f64, y: f64) -> (C, &'static str) {
    app.mouse = (x, y);
    app.cursor_probe()
}

fn center(r: Rect) -> (f64, f64) {
    (r.x as f64 + r.w as f64 / 2.0, r.y as f64 + r.h as f64 / 2.0)
}

fn at(px: &[u32], x: i32, y: i32) -> u32 {
    px[(y * W + x) as usize]
}

#[test]
fn rail_buttons_are_hands_and_the_gaps_are_arrows() {
    let _g = lock();
    let dir = tmp("cur-shell-rail");
    let mut app = test_app(&dir);
    frame(&mut app);
    let btns = app.layout.rail_btns.clone();
    assert!(btns.len() >= 2, "the rail has buttons");
    for (r, _) in &btns {
        let (x, y) = center(*r);
        let (icon, label) = probe(&mut app, x, y);
        assert_eq!((icon, label), (C::Pointer, "rail-button"), "{r:?}");
    }
    let gap = btns[0].0.bottom() + 1;
    let (icon, label) = probe(&mut app, center(btns[0].0).0, gap as f64);
    assert_eq!((icon, label), (C::Default, "rail-dead"), "between two rail buttons");
}

#[test]
fn title_bar_controls_are_hands_and_its_empty_area_is_an_arrow() {
    let _g = lock();
    let dir = tmp("cur-shell-top");
    let mut app = test_app(&dir);
    frame(&mut app);
    assert!(!app.layout.top_btns.is_empty());
    for (r, _) in app.layout.top_btns.clone() {
        let (x, y) = center(r);
        assert_eq!(probe(&mut app, x, y).0, C::Pointer, "title-bar control {r:?}");
    }
    // The very top-left pixel row is padding.
    assert_eq!(probe(&mut app, 2.0, 1.0).0, C::Default);
}

#[test]
fn status_bar_gaps_are_arrows_and_its_buttons_hands() {
    let _g = lock();
    let dir = tmp("cur-shell-status");
    let mut app = test_app(&dir);
    frame(&mut app);
    let st = app.layout.status;
    assert!(st.h > 0, "the status bar is shown");
    for (r, _) in app.layout.status_btns.clone() {
        let (x, y) = center(r);
        assert_eq!(probe(&mut app, x, y).0, C::Pointer, "status button {r:?}");
    }
    let (icon, label) = probe(&mut app, (st.x + 2) as f64, (st.y + st.h / 2) as f64);
    assert_eq!((icon, label), (C::Default, "status-dead"));
}

#[test]
fn the_sidebar_edge_resizes_and_its_header_controls_have_their_shapes() {
    let _g = lock();
    let dir = tmp("cur-shell-side");
    let mut app = test_app(&dir);
    app.sidebar_open = true;
    frame(&mut app);
    let s = app.layout.sidebar;
    assert!(s.w > 0);
    let (icon, label) = probe(&mut app, (s.right() - 2) as f64, (s.y + s.h / 2) as f64);
    assert_eq!((icon, label), (C::ColResize, "sidebar-edge"));
    if let Some(r) = app.tree.fx.hits.search {
        let (x, y) = center(r);
        assert_eq!(probe(&mut app, x, y).0, C::Pointer, "search button");
    }
}

#[test]
fn an_open_menu_follows_the_pointer_over_the_other_titles() {
    let _g = lock();
    let dir = tmp("cur-shell-menu");
    let mut app = test_app(&dir);
    frame(&mut app);
    let title = |app: &App, i: u8| app.layout.top_btns.iter().find(|(_, k)| *k == MenuKind::Menu(i)).map(|(r, _)| *r);
    let (Some(first), Some(second)) = (title(&app, 0), title(&app, 1)) else {
        panic!("the menu bar is laid out in a {W} px window");
    };
    app.open_topmenu(MenuKind::Menu(0), first);
    frame(&mut app);
    // Over the same title, or over a button that is not a menu title: nothing changes.
    app.mouse = center(first);
    assert!(!app.topmenu_hover());
    if let Some((project, _)) = app.layout.top_btns.iter().find(|(_, k)| *k == MenuKind::Project).copied() {
        app.mouse = center(project);
        assert!(!app.topmenu_hover(), "only menu titles switch the menu");
    }
    // Over the next title: its menu opens, anchored under it.
    app.mouse = center(second);
    assert!(app.topmenu_hover());
    let Overlay::TopMenu(m) = &app.overlay else { panic!("a menu stays open") };
    assert_eq!(m.kind, MenuKind::Menu(1));
    assert_eq!(m.anchor, second);
    // The rows of the open menu answer the hand, the rest of the window the arrow.
    frame(&mut app);
    let rows = match &app.overlay {
        Overlay::TopMenu(m) => m.rows.clone(),
        _ => Vec::new(),
    };
    assert!(!rows.is_empty(), "the menu lists commands");
    let (x, y) = center(rows[0].0);
    assert_eq!(probe(&mut app, x, y).0, C::Pointer);
    assert_eq!(probe(&mut app, 3.0, (H - 3) as f64).0, C::Default);
}

#[test]
fn hovering_a_menu_row_selects_it() {
    let _g = lock();
    let dir = tmp("cur-shell-menu-row");
    let mut app = test_app(&dir);
    frame(&mut app);
    let first = app.layout.top_btns.iter().find(|(_, k)| *k == MenuKind::Menu(0)).map(|(r, _)| *r).unwrap();
    app.open_topmenu(MenuKind::Menu(0), first);
    frame(&mut app);
    let rows = match &app.overlay {
        Overlay::TopMenu(m) => m.rows.clone(),
        _ => Vec::new(),
    };
    assert!(rows.len() >= 2);
    app.mouse = center(rows[1].0);
    app.topmenu_hover();
    let Overlay::TopMenu(m) = &app.overlay else { panic!("menu open") };
    assert_eq!(m.sel, rows[1].1);
}

#[test]
fn the_sidebar_slides_shut_with_round_corners_and_without_hit_areas() {
    let _g = lock();
    let dir = tmp("cur-shell-slide");
    let mut app = test_app(&dir);
    app.sidebar_open = true;
    frame(&mut app);
    let c = app.theme.chrome;
    // The canvas as the softbuffer pixel format holds it (the margin beside the editor card carries
    // its shadow, so it is not a reference).
    let canvas_px = (u32::from(c.canvas.0) << 16) | (u32::from(c.canvas.1) << 8) | u32::from(c.canvas.2);
    app.sidebar_open = false;
    app.layout.shell.live = true;
    let rail_w = app.rail_w();
    let mut saw_mid = false;
    for _ in 0..40 {
        app.layout.shell.step(0.016);
        let px = shot(&mut app, 0.016);
        let t = app.layout.shell.side.value;
        assert_eq!(app.layout.sidebar.w, 0, "a closing sidebar takes no clicks");
        assert!(app.layout.sidebar_tabs.is_empty());
        if t > 0.2 && t < 0.9 {
            saw_mid = true;
            let side = app.layout.shell.side_rect;
            // The corner of the card still reads as canvas: it stays rounded while it moves.
            let (cx, cy) = (side.right() - 1, side.y);
            assert_eq!(at(&px, cx, cy), canvas_px, "square corner at t={t}");
            if c.sidebar_bg != c.canvas {
                // And the card is really painted (it used to vanish on the first frame).
                assert_ne!(at(&px, rail_w + 6, 300), canvas_px, "sidebar missing at t={t}");
            }
        }
    }
    assert!(saw_mid, "the slide passes through intermediate positions");
    assert!(app.layout.shell.side.value < 0.01, "and ends closed");
}

#[test]
fn a_held_button_shows_the_pressed_look() {
    let _g = lock();
    let dir = tmp("cur-shell-pressed");
    let mut app = test_app(&dir);
    frame(&mut app);
    let b = app.layout.rail_btns[0].0;
    let (x, y) = (b.x + 2, b.y + b.h / 2);
    app.mouse = (x as f64, y as f64);
    let up = at(&shot(&mut app, 0.016), x, y);
    app.layout.shell.down = true;
    let down = at(&shot(&mut app, 0.016), x, y);
    assert_ne!(up, down, "pressed overlay over the rail button");
    // Pressing elsewhere leaves it alone.
    app.mouse = (700.0, 450.0);
    let elsewhere = at(&shot(&mut app, 0.016), x, y);
    assert_eq!(elsewhere, up);
}
