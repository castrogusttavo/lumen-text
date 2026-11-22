//! Headless pointer sweeps: put the app in a state, draw a frame, probe a grid of mouse positions
//! and check the invariants of `docs/CURSOR_CHECKLIST.md` (hand on controls, I-beam on text,
//! resize arrows on dividers, arrow on dead areas, and nothing under an open modal leaks through).

use crate::{
    app::App,
    app_cursor::{CursorReg, Z_BASE, Z_MODAL, debug_title},
    app_unsaved::testkit::{lock, test_app, tmp},
    ui::{Canvas, Rect},
};
use winit::window::CursorIcon as C;

const W: i32 = 1400;
const H: i32 = 900;
const STEP: usize = 16;

/// Draw two frames (the second one settles the animations that start on the first).
fn frame(app: &mut App) {
    app.size = (W as u32, H as u32);
    let mut px = vec![0u32; (W * H) as usize];
    for _ in 0..2 {
        // A startup toast would answer Pointer wherever it lands.
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

/// Every probe of a grid over the window: (x, y, icon, label).
fn sweep(app: &mut App) -> Vec<(f64, f64, C, &'static str)> {
    let mut v = Vec::new();
    for y in (1..H).step_by(STEP) {
        for x in (1..W).step_by(STEP) {
            let (icon, label) = probe(app, x as f64, y as f64);
            v.push((x as f64, y as f64, icon, label));
        }
    }
    v
}

fn is_resize(c: C) -> bool {
    matches!(c, C::ColResize | C::RowResize | C::EwResize | C::NsResize)
}

/// Controls of the chrome that the layout lists give a hand at their center.
fn assert_chrome_is_pointer(app: &mut App) {
    let l = &app.layout;
    let mut rects: Vec<(Rect, &str)> = Vec::new();
    rects.extend(l.top_btns.iter().map(|(r, _)| (*r, "top_btns")));
    rects.extend(l.status_btns.iter().map(|(r, _)| (*r, "status_btns")));
    rects.extend(l.rail_btns.iter().map(|(r, _)| (*r, "rail_btns")));
    rects.extend(l.tabs.iter().map(|(r, _, _)| (*r, "tabs")));
    rects.extend(l.sidebar_tabs.iter().map(|(r, _)| (*r, "sidebar_tabs")));
    rects.extend(l.panel_tabs.iter().map(|(r, _)| (*r, "panel_tabs")));
    for (r, what) in rects {
        let (x, y) = center(r);
        let (icon, label) = probe(app, x, y);
        assert_eq!(icon, C::Pointer, "{what} {r:?} gives {icon:?} ({label})");
    }
}

#[test]
fn editor_state_pointer_everywhere() {
    let _g = lock();
    let dir = tmp("cur-editor");
    let mut app = test_app(&dir);
    frame(&mut app);
    assert!(!app.layout.tabs.is_empty(), "the seed file has a tab");
    assert_chrome_is_pointer(&mut app);
    // The text area is text; the gutter is not.
    let ed = app.layout.editor;
    let (x, y) = center(ed);
    let (icon, label) = probe(&mut app, x, y);
    assert_eq!(icon, C::Text, "editor text: {label}");
    let (icon, label) = probe(&mut app, (ed.x + 10) as f64, y);
    assert_eq!(icon, C::Default, "gutter: {label}");
    // Closing a tab is a click too.
    let close = app.layout.tabs[0].1;
    let (x, y) = center(close);
    assert_eq!(probe(&mut app, x, y).0, C::Pointer, "tab close button");
    // The sidebar edge resizes; its empty tail is an arrow.
    if app.sidebar_open && app.layout.sidebar.w > 0 {
        let s = app.layout.sidebar;
        let (icon, label) = probe(&mut app, (s.right() + 2) as f64, (s.y + s.h / 2) as f64);
        assert_eq!(icon, C::ColResize, "sidebar edge: {label}");
        let (icon, label) = probe(&mut app, (s.x + 12) as f64, (s.bottom() - 6) as f64);
        assert_eq!(icon, C::Default, "sidebar dead area: {label}");
    }
    // No wait cursor and no not-allowed anywhere in a plain editor.
    for (x, y, icon, label) in sweep(&mut app) {
        assert!(!matches!(icon, C::Wait | C::NotAllowed | C::Progress), "({x},{y}) {icon:?} {label}");
    }
}

#[test]
fn welcome_dead_areas_are_arrows_and_rows_are_hands() {
    let _g = lock();
    let dir = tmp("cur-welcome");
    let mut app = test_app(&dir);
    app.docs.clear();
    app.active = 0;
    frame(&mut app);
    for (r, _) in app.layout.welcome.clone() {
        let (x, y) = center(r);
        let (icon, label) = probe(&mut app, x, y);
        assert_eq!(icon, C::Pointer, "welcome row {r:?}: {label}");
    }
    let rows = app.layout.welcome.clone();
    for (x, y, icon, label) in sweep(&mut app) {
        let on_row = rows.iter().any(|(r, _)| r.contains(x, y));
        if app.layout.editor.contains(x, y) && !on_row {
            assert_ne!(icon, C::Text, "welcome screen has no text area at ({x},{y}): {label}");
        }
    }
}

#[test]
fn git_dock_edge_resizes_and_covers_the_editor() {
    let _g = lock();
    let dir = tmp("cur-gitdock");
    let mut app = test_app(&dir);
    app.open_git_panel();
    frame(&mut app);
    let slot = app.slot_rect();
    let (icon, label) = probe(&mut app, slot.x as f64, (slot.y + 200) as f64);
    assert_eq!(icon, C::ColResize, "dock edge: {label}");
    // Inside the dock the editor's I-beam never shows.
    for (x, y, icon, label) in sweep(&mut app) {
        if slot.contains(x, y) && x > (slot.x + 8) as f64 {
            assert!(!is_resize(icon), "({x},{y}) inside the dock: {icon:?} {label}");
        }
    }
    // The editor beside it is still text.
    let (x, y) = center(app.layout.editor);
    assert_eq!(probe(&mut app, x, y).0, C::Text);
}

#[test]
fn ai_dock_edge_resizes() {
    let _g = lock();
    let dir = tmp("cur-aidock");
    let mut app = test_app(&dir);
    app.ai.open = true;
    frame(&mut app);
    let slot = app.slot_rect();
    let (icon, label) = probe(&mut app, slot.x as f64, (slot.y + 200) as f64);
    assert_eq!(icon, C::ColResize, "AI dock edge: {label}");
}

#[test]
fn bottom_panel_tabs_are_hands_and_output_is_text() {
    let _g = lock();
    let dir = tmp("cur-panel");
    let mut app = test_app(&dir);
    app.toggle_panel();
    frame(&mut app);
    assert!(app.layout.panel.h > 0, "the panel is on screen");
    assert!(!app.layout.panel_tabs.is_empty(), "its tabs are laid out");
    assert_chrome_is_pointer(&mut app);
}

#[test]
fn an_open_palette_shadows_the_chrome_below() {
    let _g = lock();
    let dir = tmp("cur-palette");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.open_commands();
    frame(&mut app);
    assert!(!app.layout.overlay_rows.is_empty(), "the palette lists commands");
    let rows = app.layout.overlay_rows.clone();
    let mut hand_on_rows = 0;
    for (x, y, icon, label) in sweep(&mut app) {
        let on_row = rows.iter().any(|(r, _)| r.contains(x, y));
        if on_row {
            assert_eq!(icon, C::Pointer, "palette row at ({x},{y}): {label}");
            hand_on_rows += 1;
        } else {
            assert_ne!(icon, C::Pointer, "the hand of covered UI leaks at ({x},{y}): {label}");
            assert!(!is_resize(icon), "resize arrow under a modal at ({x},{y}): {label}");
        }
    }
    assert!(hand_on_rows > 0);
    // The dead areas of the backdrop are the plain arrow, with the reason in the label.
    let (icon, label) = probe(&mut app, 3.0, (H - 3) as f64);
    assert_eq!((icon, label), (C::Default, "cascade:modal-backdrop"));
}

#[test]
fn a_context_menu_shadows_the_chrome_below() {
    let _g = lock();
    let dir = tmp("cur-ctx");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.open_ctxmenu(crate::app_ctxmenu::Target::Tab(0), (300, 200));
    frame(&mut app);
    let rows = app.layout.overlay_rows.clone();
    assert!(!rows.is_empty(), "the menu has items");
    for (r, _) in &rows {
        let (x, y) = center(*r);
        assert_eq!(probe(&mut app, x, y).0, C::Pointer, "menu item {r:?}");
    }
    for (x, y, icon, label) in sweep(&mut app) {
        if !rows.iter().any(|(r, _)| r.contains(x, y)) {
            assert_ne!(icon, C::Pointer, "leak at ({x},{y}): {label}");
        }
    }
}

#[test]
fn the_settings_modal_owns_the_whole_window() {
    let _g = lock();
    let dir = tmp("cur-settings");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.open_settings(crate::settingsui::Page::Settings);
    frame(&mut app);
    // Along the window's border: the rail, title bar, status bar and dock edge all sit under the
    // dimmed backdrop.
    let mut border = Vec::new();
    for i in (1..H).step_by(STEP) {
        border.push((3.0, i as f64));
        border.push(((W - 3) as f64, i as f64));
    }
    for i in (1..W).step_by(STEP) {
        border.push((i as f64, 3.0));
        border.push((i as f64, (H - 3) as f64));
    }
    for (x, y) in border {
        let (icon, label) = probe(&mut app, x, y);
        assert_eq!(icon, C::Default, "backdrop at ({x},{y}): {label}");
    }
    // The chrome's own buttons do not leak through either.
    let l = &app.layout;
    let mut chrome: Vec<Rect> = l.top_btns.iter().map(|(r, _)| *r).collect();
    chrome.extend(l.status_btns.iter().map(|(r, _)| *r));
    chrome.extend(l.tabs.iter().map(|(r, _, _)| *r));
    for r in chrome {
        let (x, y) = center(r);
        if x > 3.0 && y > 3.0 {
            let (icon, label) = probe(&mut app, x, y);
            assert!(!is_resize(icon), "{r:?}: {icon:?} {label}");
            assert!(!label.contains("button") && label != "tab", "{r:?} leaks region {label}");
        }
    }
}

#[test]
fn the_onboarding_tour_hides_the_chrome() {
    let _g = lock();
    let dir = tmp("cur-onboarding");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.onboarding_open();
    frame(&mut app);
    let rects: Vec<Rect> =
        app.onboarding.as_ref().map(|o| o.rects.iter().map(|(r, _)| *r).collect()).unwrap_or_default();
    for (x, y, icon, label) in sweep(&mut app) {
        let on_ctl = rects.iter().any(|r| r.contains(x, y));
        assert_eq!(icon == C::Pointer, on_ctl, "({x},{y}) {icon:?} {label}");
    }
}

#[test]
fn the_find_bar_has_a_field_and_buttons() {
    let _g = lock();
    let dir = tmp("cur-find");
    let mut app = test_app(&dir);
    frame(&mut app);
    app.open_find(false);
    frame(&mut app);
    let all = sweep(&mut app);
    // The sweep is coarse (16 px): the bar's field is wide, its buttons are 24 px or so.
    assert!(all.iter().any(|(_, _, i, l)| *l == "cascade:find" && *i == C::Text), "an I-beam on the field");
    assert!(all.iter().any(|(_, _, i, l)| *l == "cascade:find" && *i == C::Pointer), "a hand on its buttons");
}

#[test]
fn drags_own_the_pointer() {
    let _g = lock();
    let dir = tmp("cur-drag");
    let mut app = test_app(&dir);
    frame(&mut app);
    let (x, y) = center(app.layout.tabs[0].0);
    app.dragging = true;
    assert_eq!(probe(&mut app, x, y).0, C::Text, "a selection drag keeps the I-beam over the tab strip");
    app.dragging = false;
    app.dragging_minimap = true;
    assert_eq!(probe(&mut app, 5.0, 5.0).0, C::Pointer);
}

#[test]
fn registry_topmost_and_floor() {
    let mut reg = CursorReg::default();
    let r = Rect::new(0, 0, 100, 100);
    reg.push(r, C::Pointer, Z_BASE, "base");
    reg.push(Rect::new(10, 10, 20, 20), C::Text, Z_BASE + 5, "field");
    reg.push(Rect::new(0, 0, 0, 10), C::Pointer, Z_MODAL, "empty rect is dropped");
    assert_eq!(reg.list.len(), 2);
    assert_eq!(reg.topmost(15.0, 15.0, 0).map(|r| r.label), Some("field"), "higher z wins");
    assert_eq!(reg.topmost(50.0, 50.0, 0).map(|r| r.label), Some("base"));
    assert!(reg.topmost(50.0, 50.0, Z_MODAL).is_none(), "a modal floor hides the chrome");
    assert!(reg.topmost(150.0, 50.0, 0).is_none());
    // Equal levels: the region registered last (drawn last) wins.
    reg.push(r, C::Grab, Z_BASE, "later");
    assert_eq!(reg.topmost(50.0, 50.0, 0).map(|r| r.label), Some("later"));
}

#[test]
fn debug_title_names_icon_and_region() {
    assert_eq!(debug_title("a.rs — lumen-text", C::Pointer, "tab"), "a.rs — lumen-text  cursor=Pointer region=tab");
}
