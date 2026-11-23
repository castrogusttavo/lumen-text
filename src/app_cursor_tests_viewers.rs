//! Pointer sweeps of the viewers: the picture (open / closed hand), the Markdown preview (hand on
//! links, I-beam on text), the merge editor (hand on every button, never a resize arrow) and a
//! viewer under a modal (the backdrop answers an arrow).

use crate::{
    app::App,
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
        app.render_into(&mut Canvas { px: &mut px, w: W, h: H }, 0.016);
        app.pump_background();
        std::thread::sleep(std::time::Duration::from_millis(5));
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

fn git(dir: &std::path::Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn a_picture_is_a_grab_target_and_a_drag_is_grabbing() {
    let _g = lock();
    let dir = tmp("cur-img");
    let mut app = test_app(&dir);
    let path = dir.join("pic.png");
    let px: Vec<u32> = (0..64 * 64).map(|i| if (i / 8 + i / 512) % 2 == 0 { 0xff8800 } else { 0x2244ff }).collect();
    crate::script::write_png(&path, &px, 64, 64).unwrap();
    let i = app.open_path(&path, None).expect("opens");
    app.active = i;
    assert!(app.docs[i].showing_image());
    frame(&mut app);
    let v = app.docs[i].img.as_ref().expect("picture");
    assert!(v.dev_img.w > 0, "the picture was decoded and laid out");
    let (x, y) = {
        let s = crate::ui::scale() as f64;
        let (cx, cy) = center(v.dev_img);
        (cx / s, cy / s)
    };
    let (icon, label) = probe(&mut app, x, y);
    assert_eq!(icon, C::Grab, "over the picture: {label}");
    // The pane around it is never the editor's I-beam.
    let pane = app.docs[i].img.as_ref().unwrap().rect;
    let (icon, label) = probe(&mut app, (pane.x + 3) as f64, (pane.bottom() - 3) as f64);
    assert_ne!(icon, C::Text, "around the picture: {label}");
    // A press starts the pan: closed hand, also when the pointer leaves the picture.
    app.mouse = (x, y);
    assert!(app.img_mouse_down(x, y));
    assert_eq!(probe(&mut app, x, y).0, C::Grabbing);
    assert_eq!(probe(&mut app, x + 120.0, y + 40.0).0, C::Grabbing, "a drag owns the pointer");
    app.img_release();
    assert_eq!(probe(&mut app, x, y).0, C::Grab, "released");
}

#[test]
fn the_markdown_preview_has_hands_on_links_and_beams_on_text() {
    let _g = lock();
    let dir = tmp("cur-md");
    let mut app = test_app(&dir);
    let path = dir.join("doc.md");
    std::fs::write(&path, "# Title\n\nSome text with a [link](https://example.com) inside.\n\n- [ ] a task\n").unwrap();
    let i = app.open_path(&path, None).expect("opens");
    app.active = i;
    app.toggle_md_reader();
    frame(&mut app);
    let r = app.layout.md;
    assert!(r.w > 0 && r.h > 0, "the preview takes the right half");
    let (mut hands, mut beams) = (0, 0);
    let mut y = r.y + 2;
    while y < r.bottom() {
        let mut x = r.x + 2;
        while x < r.right() {
            let (icon, label) = probe(&mut app, x as f64, y as f64);
            assert!(!is_resize(icon) && icon != C::Wait && icon != C::NotAllowed, "({x},{y}) {icon:?} {label}");
            match icon {
                C::Pointer => hands += 1,
                C::Text => beams += 1,
                _ => {}
            }
            x += 6;
        }
        y += 6;
    }
    assert!(hands > 0, "the link and the checkbox give a hand");
    assert!(beams > hands, "the rest of the preview is text");
}

#[test]
fn the_merge_editor_buttons_are_hands() {
    let _g = lock();
    let d = tmp("cur-merge");
    if !git(&d, &["init", "-q", "-b", "main"]) {
        return; // no git on this machine: nothing to sweep
    }
    std::fs::write(d.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    assert!(git(&d, &["add", "f.txt"]) && git(&d, &["commit", "-q", "-m", "base"]));
    assert!(git(&d, &["checkout", "-q", "-b", "feat"]));
    std::fs::write(d.join("f.txt"), "one\nTWO feat\nthree\n").unwrap();
    assert!(git(&d, &["commit", "-q", "-am", "feat"]));
    assert!(git(&d, &["checkout", "-q", "main"]));
    std::fs::write(d.join("f.txt"), "one\nTWO main\nthree\n").unwrap();
    assert!(git(&d, &["commit", "-q", "-am", "main"]));
    assert!(!git(&d, &["merge", "feat"]), "the merge conflicts");
    let mut app = test_app(&d);
    app.merge_open(&d.join("f.txt"));
    frame(&mut app);
    let btns = app.merge.as_ref().expect("merge editor").btns.clone();
    assert!(btns.len() >= 4, "close, resolve, next, previous and the checkboxes: {}", btns.len());
    for (r, b) in btns {
        if matches!(b, crate::app_merge::MergeBtn::Goto(_)) {
            continue;
        }
        let (x, y) = center(r);
        let (icon, label) = probe(&mut app, x, y);
        // "Mark resolved" is disabled while markers remain: not-allowed, with its reason.
        let want = if b == crate::app_merge::MergeBtn::Resolve { C::NotAllowed } else { C::Pointer };
        assert_eq!(icon, want, "merge button {b:?} {r:?}: {label}");
    }
    // The panes themselves never show a resize arrow.
    let area = app.merge.as_ref().unwrap().area;
    for y in (area.y..area.bottom()).step_by(12) {
        for x in (area.x..area.right()).step_by(12) {
            let (icon, label) = probe(&mut app, x as f64, y as f64);
            assert!(!is_resize(icon), "({x},{y}) {icon:?} {label}");
        }
    }
    // A palette over the viewer hides its hands.
    let (x, y) = center(app.merge.as_ref().unwrap().btns[0].0);
    app.open_commands();
    frame(&mut app);
    let rows = app.layout.overlay_rows.clone();
    if !rows.iter().any(|(r, _)| r.contains(x, y)) {
        assert_ne!(probe(&mut app, x, y).0, C::Pointer, "the covered button leaks a hand");
    }
    drop(app);
    let _ = std::fs::remove_dir_all(&d);
}
