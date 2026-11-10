//! Tests for the font and editor-display settings: layout math at other line heights, the gutter
//! without numbers, live application through `apply_setting`, bookmarks in the session, the
//! breakpoint click and the language-server switch.

use crate::{
    app::App,
    app_unsaved::testkit,
    buffer::Buffer,
    display::{self, Scrollbar},
    doc::Doc,
    fontcfg,
    ui::{Canvas, Rect, TextCtx, VBar, line_h},
};
use serde_json::json;

fn doc(text: &str) -> Doc {
    let mut d = Doc::new(Buffer::from_str(text), None, None, "//", 8.0);
    d.rect = Rect::new(0, 0, 800, 400);
    d
}

fn sample_text() -> String {
    (0..60).map(|i| format!("line {i} with some text to click on {}\n", "x".repeat(i % 7))).collect()
}

/// (font size, line height setting) pairs: the default, tight, loose, a pixel height, big zoom.
const CASES: &[(f32, f32)] =
    &[(14.0, 1.57), (14.0, 1.0), (14.0, 2.0), (14.0, 2.5), (14.0, 30.0), (10.0, 1.3), (22.0, 1.8)];

#[test]
fn hit_test_round_trips_at_every_line_height_and_zoom() {
    for &(size, lh) in CASES {
        fontcfg::store_buffer_size(size);
        fontcfg::store_buffer_line_height(lh);
        let row_h = line_h() as f64;
        assert_eq!(row_h, fontcfg::line_px(size, lh) as f64);
        for numbers in [true, false] {
            display::set_line_numbers(numbers);
            let mut d = doc(&sample_text());
            d.cell_w = 8.4;
            for top in [0.0, 3.0, 7.5] {
                d.top = top;
                for line in [top.floor() as usize + 1, top.floor() as usize + 4] {
                    for col in [0usize, 3, 10, 20] {
                        let c = d.buf.pos(line, col.min(d.buf.line_len(line)));
                        let (x, y) = d.screen_pos(c).expect("visible");
                        // Cell centre: the click lands on the char whatever the row height.
                        let hit = d.pos_at(x as f64 + 1.0, y as f64 + row_h / 2.0);
                        assert_eq!(hit, c, "size {size} lh {lh} numbers {numbers} top {top} line {line} col {col}");
                    }
                }
            }
        }
    }
    display::set_line_numbers(true);
}

#[test]
fn hit_test_round_trips_with_word_wrap_at_other_line_heights() {
    for &(size, lh) in CASES {
        fontcfg::store_buffer_size(size);
        fontcfg::store_buffer_line_height(lh);
        let long = "abc ".repeat(40);
        let mut d = doc(&format!("{long}\nnext\n"));
        d.set_word_wrap(true);
        d.cell_w = 8.0;
        d.view_prepare(30);
        let row_h = line_h() as f64;
        for col in [0usize, 5, 29, 31, 60, 100, 150] {
            let c = d.buf.pos(0, col);
            let (row, vcol) = d.caret_geom(c);
            let x = (d.rect.x + d.gutter_w()) as f64 + vcol as f64 * 8.0;
            let y = d.rect.y as f64 + row as f64 * row_h + row_h / 2.0;
            assert_eq!(d.pos_at(x, y), c, "size {size} lh {lh} col {col}");
        }
    }
}

#[test]
fn visible_rows_and_scroll_range_follow_the_row_height() {
    for &(size, lh) in CASES {
        fontcfg::store_buffer_size(size);
        fontcfg::store_buffer_line_height(lh);
        let d = doc(&sample_text());
        let rows = (400.0 / line_h()).floor().max(1.0) as f64;
        assert_eq!(d.visible_lines(), rows, "size {size} lh {lh}");
        assert_eq!(d.max_top(), (d.total_rows() as f64 - rows).max(0.0));
    }
}

#[test]
fn gutter_without_numbers_is_narrow_but_keeps_the_fold_chevron_working() {
    let text = "fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn b() {}\n";
    let mut d = doc(text);
    d.cell_w = 8.4;
    let with = d.gutter_w();
    display::set_line_numbers(false);
    let without = d.gutter_w();
    assert!(without < with, "{without} vs {with}");
    assert_eq!(without, display::NUMLESS_MARKS_W + crate::docview::FOLD_W);
    assert!(d.is_foldable(0));
    // The chevron column is the FOLD_W px just left of the text, wherever the gutter ends.
    let lh = line_h() as f64;
    let x = (d.rect.x + without) as f64 - 4.0;
    assert!(d.click_fold(x, lh / 2.0), "chevron click folds the block");
    assert!(d.view.folds.is_fold_start(0).is_some());
    assert!(d.click_fold(x, lh / 2.0), "and unfolds it");
    // The text starts right after the narrower gutter.
    let c = d.buf.pos(1, 4);
    let (sx, _) = d.screen_pos(c).unwrap();
    assert_eq!(sx, without + (4.0f32 * 8.4).round() as i32);
    display::set_line_numbers(true);
}

#[test]
fn ligatures_keep_the_cell_grid() {
    let mut tx = TextCtx::new(None);
    let spec = |ligs| fontcfg::FontSpec { ligatures: ligs, ..fontcfg::FontSpec::buffer_default() };
    for text in ["=> != === -> <= >= ++ :: //", "<-- --> |> <| ## .. ||", "www fi fl ffi"] {
        let n = text.chars().count() as f32;
        tx.set_buffer_font(&spec(false));
        let plain = tx.buffer_text_width(text);
        tx.set_buffer_font(&spec(true));
        let ligs = tx.buffer_text_width(text);
        let cell = tx.cell_w;
        assert!((plain - n * cell).abs() < 0.6 * n, "plain {plain} vs {} for {text:?}", n * cell);
        // A ligature takes the cells of the characters it replaces: the line is as wide either way.
        assert!((ligs - plain).abs() < 0.6 * n, "ligatures {ligs} vs plain {plain} for {text:?}");
    }
}

#[test]
fn scrollbar_modes_change_what_is_drawn() {
    let draw = |hot: bool| {
        let mut px = vec![0u32; 40 * 120];
        let mut cv = Canvas { px: &mut px, w: 40, h: 120 };
        let track = Rect::new(20, 0, 14, 120);
        if let Some(b) = VBar::new(track, 1000.0, 100.0, 0.0) {
            b.draw(&mut cv, crate::theme::Rgba(255, 255, 255, 255), hot, false);
        }
        px.iter().filter(|p| **p != 0).count()
    };
    display::set_scrollbar(Scrollbar::Show);
    assert!(draw(false) > 0, "show: always drawn");
    display::set_scrollbar(Scrollbar::Never);
    assert_eq!(draw(true), 0, "never: nothing, even when pointed at");
    display::set_scrollbar(Scrollbar::Auto);
    assert_eq!(draw(false), 0, "auto: hidden at rest");
    assert!(draw(true) > 0, "auto: shown under the pointer");
    display::touch();
    assert!(draw(false) > 0, "auto: shown right after scrolling");
}

// ── App level ──

fn app(tag: &str) -> (testkit::Guard, App, std::path::PathBuf) {
    let g = testkit::lock();
    let dir = testkit::tmp(tag);
    let app = testkit::test_app(&dir);
    (g, app, dir)
}

#[test]
fn live_font_settings_change_the_grid_and_are_saved() {
    let (_g, mut app, dir) = app("fonts-live");
    let (lh0, cw0) = (line_h(), app.tx.cell_w);
    assert_eq!(lh0, 22.0, "defaults unchanged");
    app.apply_setting("buffer_line_height", &json!(2.0));
    assert_eq!(line_h(), 28.0);
    app.apply_setting("buffer_font_size", &json!(20));
    assert_eq!(crate::ui::font_size(), 20.0);
    assert!(app.tx.cell_w > cw0, "bigger font, wider cell");
    assert_eq!(app.docs[app.active].cell_w, app.tx.cell_w, "documents get the new cell width");
    app.apply_setting("buffer_font_weight", &json!(640));
    assert_eq!(app.config.buffer_font_weight, 600, "weights snap to hundreds");
    app.apply_setting("buffer_line_height", &json!(99999));
    assert_eq!(app.config.buffer_line_height, 80.0, "huge values mean pixels, clamped");
    app.apply_setting("ui_font_size", &json!(15));
    assert_eq!(crate::ui::ui_size(), 15.0);
    app.apply_setting("buffer_font_ligatures", &json!(true));
    assert!(app.config.buffer_font_ligatures);
    let saved = crate::config::Config::load_from(&dir.join(".config/lumen-text/config.json"));
    assert_eq!(saved.buffer_font_size, 20.0);
    assert_eq!(saved.ui_font_size, 15.0);
    assert!(saved.buffer_font_ligatures);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_family_falls_back_and_says_so() {
    let (_g, mut app, dir) = app("fonts-missing");
    let cw0 = app.tx.cell_w;
    app.apply_setting("buffer_font_family", &json!("Definitely Not An Installed Font"));
    assert!(app.status.as_ref().is_some_and(|(m, _)| m.contains("font not found")), "{:?}", app.status);
    assert_eq!(app.tx.cell_w, cw0, "the default font is still in use");
    app.apply_setting("ui_font_family", &json!("Also Not Installed"));
    assert!(app.tx.ui_width("Hello", false) > 0);
    // An empty name is the default again, without a complaint.
    app.status = None;
    app.apply_setting("buffer_font_family", &json!(""));
    assert!(app.status.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_installed_family_loads_and_unloading_restores_the_default() {
    let (_g, mut app, dir) = app("fonts-installed");
    let fams = fontcfg::installed_families();
    let Some(other) = fams.iter().find(|f| f.mono && f.name != "Adwaita Mono") else {
        eprintln!("[fonts] no installed monospace family to test with");
        return;
    };
    let faces0 = app.tx.fonts.db().len();
    app.apply_setting("buffer_font_family", &json!(other.name));
    assert!(app.status.is_none(), "{} should load: {:?}", other.name, app.status);
    let faces1 = app.tx.fonts.db().len();
    assert!(faces1 > faces0, "its files were added to the database");
    // Picking another family replaces the faces instead of piling them up.
    app.apply_setting("buffer_font_family", &json!(other.name));
    assert_eq!(app.tx.fonts.db().len(), faces1, "no growth when the same family is applied again");
    app.apply_setting("buffer_font_family", &json!(""));
    assert_eq!(app.tx.fonts.db().len(), faces0, "back to the default: its faces are dropped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preview_faces_are_dropped_when_the_picker_closes() {
    let (_g, mut app, dir) = app("fonts-preview");
    let fams = fontcfg::installed_families();
    let Some(f) = fams.iter().find(|f| f.mono) else { return };
    let before = app.tx.fonts.db().len();
    assert!(app.tx.preview_ready(&f.name, &f.file) || std::fs::metadata(&f.file).is_ok_and(|m| m.len() > 12 << 20));
    let mut px = vec![0u32; 200 * 40];
    let mut cv = Canvas { px: &mut px, w: 200, h: 40 };
    app.tx.draw_family(
        &mut cv,
        2,
        2,
        &f.name,
        &f.name,
        crate::theme::Rgba(255, 255, 255, 255),
        Rect::new(0, 0, 200, 40),
        16.0,
    );
    app.tx.clear_previews();
    assert_eq!(app.tx.fonts.db().len(), before, "the picker leaves no faces behind");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn display_settings_apply_live() {
    let (_g, mut app, dir) = app("fonts-display");
    let w0 = app.docs[app.active].gutter_w();
    app.apply_setting("show_line_numbers", &json!(false));
    assert!(app.docs[app.active].gutter_w() < w0);
    assert!(!app.config.show_line_numbers);
    app.apply_setting("show_line_numbers", &json!(true));
    assert_eq!(app.docs[app.active].gutter_w(), w0);
    app.apply_setting("scrollbar", &json!("never"));
    assert_eq!(display::scrollbar(), Scrollbar::Never);
    app.apply_setting("scrollbar", &json!("bogus"));
    assert_eq!(app.config.scrollbar, "show");
    app.apply_setting("show_bookmarks", &json!(false));
    app.apply_setting("show_breakpoints", &json!(false));
    assert!(!display::show_bookmarks() && !display::show_breakpoints());
    let saved = crate::config::Config::load_from(&dir.join(".config/lumen-text/config.json"));
    assert!(!saved.show_bookmarks && !saved.show_breakpoints);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rendering_works_at_odd_line_heights_and_without_numbers() {
    let (_g, mut app, dir) = app("fonts-render");
    app.size = (700, 400);
    let (w, h) = (700, 400);
    let mut px = vec![0u32; (w * h) as usize];
    for (lh, numbers) in [(1.0, true), (2.5, true), (1.57, false), (31.0, false)] {
        app.apply_setting("buffer_line_height", &json!(lh));
        app.apply_setting("show_line_numbers", &json!(numbers));
        app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
        assert!(px.iter().any(|p| *p != px[0]), "something was drawn at lh {lh}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bookmarks_toggle_navigate_and_survive_the_session() {
    let (_g, mut app, dir) = app("fonts-marks");
    let path = dir.join("m.txt");
    std::fs::write(&path, (0..30).map(|i| format!("row {i}\n")).collect::<String>()).unwrap();
    let i = app.open_path(&path, None).unwrap();
    app.active = i;
    let go = |app: &mut App, l: usize| {
        let d = &mut app.docs[app.active];
        let p = d.buf.line_start(l);
        d.ed.click(&mut d.buf, p, false);
    };
    let line = |app: &App| {
        let d = &app.docs[app.active];
        d.buf.line_col(d.ed.head()).0
    };
    for l in [4, 12, 20] {
        go(&mut app, l);
        app.toggle_bookmark();
    }
    assert_eq!(app.docs[i].marks, [4, 12, 20]);
    go(&mut app, 0);
    app.goto_bookmark(true);
    assert_eq!(line(&app), 4);
    app.goto_bookmark(true);
    assert_eq!(line(&app), 12);
    app.goto_bookmark(false);
    app.goto_bookmark(false);
    assert_eq!(line(&app), 20, "wraps to the last");
    // An inserted line above moves every mark down.
    go(&mut app, 0);
    {
        let d = &mut app.docs[i];
        d.ed.insert_text(&mut d.buf, "new\n");
        d.sync_edits();
    }
    assert_eq!(app.docs[i].marks, [5, 13, 21]);
    app.save_session();
    app.docs[i].marks.clear();
    app.restore_session();
    let j = app.docs.iter().position(|d| d.path() == Some(&path)).unwrap();
    assert_eq!(app.docs[j].marks, [5, 13, 21], "restored from the session");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gutter_click_toggles_a_breakpoint_unless_they_are_hidden() {
    let (_g, mut app, dir) = app("fonts-bp");
    let path = dir.join("b.txt");
    std::fs::write(&path, (0..30).map(|i| format!("row {i}\n")).collect::<String>()).unwrap();
    let i = app.open_path(&path, None).unwrap();
    app.active = i;
    app.docs[i].rect = Rect::new(100, 50, 600, 400);
    let y = 50.0 + 3.0 * line_h() as f64 + 4.0;
    assert!(app.gutter_breakpoint_click(104.0, y));
    assert_eq!(app.breakpoints.get(&path).map(|v| v.as_slice()), Some(&[3usize][..]));
    assert!(app.gutter_breakpoint_click(104.0, y), "a second click removes it");
    assert!(app.breakpoints.get(&path).is_none_or(|v| v.is_empty()));
    assert!(!app.gutter_breakpoint_click(300.0, y), "clicks in the text are not gutter clicks");
    app.apply_setting("show_breakpoints", &json!(false));
    assert!(!app.gutter_breakpoint_click(104.0, y), "hidden: no dots, no gutter click");
    assert!(app.breakpoints.get(&path).is_none_or(|v| v.is_empty()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lsp_master_switch() {
    let (_g, mut app, dir) = app("fonts-lsp");
    assert!(app.lsp_enabled());
    let path = dir.join("a.ts");
    std::fs::write(&path, "export const a = 1;\n").unwrap();
    let i = app.open_path(&path, None).unwrap();
    app.apply_setting("enable_lsp", &json!(false));
    assert!(!app.lsp_enabled() && !app.config.lsp_enabled());
    assert!(app.ensure_lsp(i).is_none(), "no server starts while the switch is off");
    assert!(app.lsps.is_empty());
    assert!(!app.docs[i].lsp_open);
    app.apply_setting("enable_lsp", &json!(true));
    assert!(app.lsp_enabled());
    assert!(crate::config::Config::load_from(&dir.join(".config/lumen-text/config.json")).enable_lsp);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_page_lists_the_new_rows_and_font_rows_open_the_picker() {
    use crate::settingsui::{Event, SettingsPage, default_settings, default_values};
    let defs = default_settings();
    for key in [
        "buffer_font_family",
        "buffer_font_size",
        "buffer_font_weight",
        "buffer_line_height",
        "buffer_font_ligatures",
        "ui_font_family",
        "ui_font_size",
        "ui_font_weight",
        "ui_line_height",
        "ui_font_ligatures",
        "show_line_numbers",
        "show_bookmarks",
        "show_breakpoints",
        "scrollbar",
        "enable_lsp",
    ] {
        assert!(defs.iter().any(|d| d.key == key), "{key} has a settings row");
        assert!(default_values().get(key).is_some(), "{key} has a default");
    }
    assert!(!defs.iter().any(|d| d.key == "font_size"), "the old key is gone from the UI");
    let sections: Vec<&str> = defs.iter().map(|d| d.section).collect();
    assert!(sections.contains(&"Fonts") && sections.contains(&"Editor display"));
    let mut p = SettingsPage::new(defs, crate::settings_ext::values(&crate::config::Config::default()));
    p.set_defaults(default_values());
    let d = p.defs.iter().position(|d| d.key == "buffer_font_family").unwrap();
    assert_eq!(p.activate(d), Event::PickFont { key: "buffer_font_family".into() });
    let n = p.defs.iter().position(|d| d.key == "buffer_line_height").unwrap();
    assert!(!p.is_modified(n), "the default line height is not shown as modified");
    let sz = p.defs.iter().position(|d| d.key == "buffer_font_size").unwrap();
    assert!(!p.is_modified(sz), "14 (stored as 14.0) is the default size");
    p.set_number(sz, 15.0);
    p.set_number(sz, 14.0);
    assert!(!p.is_modified(sz), "stepping back to the default clears the reset mark");
}
