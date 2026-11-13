//! Headless driver: runs the same input and render paths as the GUI without a window.
//! Used for automated checks and screenshots:
//!   lumen-text --keys "ctrl+p;type:app;enter;wait:500;snap:after" --snap /tmp/out --size 1000x700 <files>

use crate::{
    app::{App, KeyIn},
    ui::Canvas,
};
use std::{
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};
use winit::{
    event::MouseButton,
    keyboard::{Key, ModifiersState, NamedKey, SmolStr},
};

pub struct Opts {
    pub keys: String,
    pub snap_dir: PathBuf,
    pub size: (u32, u32),
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    !data.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8))
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &d in data {
        a = (a + d as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Minimal PNG writer (RGB, stored deflate blocks).
pub fn write_png(path: &Path, px: &[u32], w: u32, h: u32) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((w * h * 3 + h) as usize);
    for y in 0..h as usize {
        raw.push(0);
        for x in 0..w as usize {
            let p = px[y * w as usize + x];
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, out)
}

fn state_line(app: &App) -> String {
    let mut extra = app.regions_state();
    if let Some(h) = &app.hover {
        extra.push_str(&format!(" hover_scroll={:.1}", h.scroll));
    }
    let base = state_line_doc(app);
    if extra.is_empty() { base } else { format!("{base} {}", extra.trim()) }
}

fn state_line_doc(app: &App) -> String {
    let d = app.docs.get(app.active);
    match d {
        Some(d) => format!(
            "top={:.2} target={:.2} left={:.1} sel={}..{} extra={} tree=({:.1},{:.1}) panel={:.1}",
            d.top,
            d.target,
            d.left,
            d.ed.sel.anchor,
            d.ed.sel.head,
            d.ed.extra.len(),
            app.tree.scroll,
            app.tree.hscroll,
            app.panel.scroll
        ),
        None => format!("no doc tree=({:.1},{:.1})", app.tree.scroll, app.tree.hscroll),
    }
}

fn settle(app: &mut App, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        app.pump_background();
        app.tick();
        sleep(Duration::from_millis(8));
    }
}

/// Render one frame into a scratch canvas (popups learn their rectangles from drawing).
fn frame(app: &mut App, w: u32, h: u32) {
    let mut px = vec![0u32; (w * h) as usize];
    let mut cv = Canvas { px: &mut px, w: w as i32, h: h as i32 };
    app.render_into(&mut cv, 0.016);
}

/// Named keys of the `--keys` syntax (the first name of a key is the one `named_token` gives).
const NAMED: &[(&str, NamedKey)] = &[
    ("enter", NamedKey::Enter),
    ("esc", NamedKey::Escape),
    ("escape", NamedKey::Escape),
    ("tab", NamedKey::Tab),
    ("backspace", NamedKey::Backspace),
    ("delete", NamedKey::Delete),
    ("up", NamedKey::ArrowUp),
    ("down", NamedKey::ArrowDown),
    ("left", NamedKey::ArrowLeft),
    ("right", NamedKey::ArrowRight),
    ("home", NamedKey::Home),
    ("end", NamedKey::End),
    ("pageup", NamedKey::PageUp),
    ("pagedown", NamedKey::PageDown),
    ("space", NamedKey::Space),
    ("insert", NamedKey::Insert),
    ("f1", NamedKey::F1),
    ("f2", NamedKey::F2),
    ("f3", NamedKey::F3),
    ("f4", NamedKey::F4),
    ("f5", NamedKey::F5),
    ("f6", NamedKey::F6),
    ("f7", NamedKey::F7),
    ("f8", NamedKey::F8),
    ("f9", NamedKey::F9),
    ("f10", NamedKey::F10),
    ("f11", NamedKey::F11),
    ("f12", NamedKey::F12),
];

fn named(name: &str) -> Option<NamedKey> {
    NAMED.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// The `--keys` name of a named key ("enter", "f5"), if the syntax has one.
pub fn named_token(key: NamedKey) -> Option<&'static str> {
    NAMED.iter().find(|(_, k)| *k == key).map(|(n, _)| *n)
}

/// Send `--keys` key tokens (combos and `type:text`, separated by `;`) straight to the app, without
/// the rendering and settling of [`run`]. Tests drive the editor through it.
#[cfg(test)]
pub fn send_keys(app: &mut App, keys: &str) {
    for tok in keys.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        for (mods, ki) in parse_token(tok) {
            app.mods = mods;
            app.handle_key(&ki);
        }
        app.mods = ModifiersState::empty();
    }
}

/// The key presses of one token: a combo, or one press per character of `type:text`.
pub fn parse_token(tok: &str) -> Vec<(ModifiersState, KeyIn)> {
    match tok.strip_prefix("type:") {
        Some(text) => text
            .chars()
            .map(|ch| {
                let ki = if ch == ' ' {
                    KeyIn { key: Key::Named(NamedKey::Space), text: Some(" ".into()) }
                } else {
                    KeyIn { key: Key::Character(SmolStr::new(ch.to_string())), text: Some(ch.to_string()) }
                };
                (ModifiersState::empty(), ki)
            })
            .collect(),
        None => parse_combo(tok).into_iter().collect(),
    }
}

/// "ctrl+shift+p" -> (mods, key)
pub fn parse_combo(tok: &str) -> Option<(ModifiersState, KeyIn)> {
    let mut mods = ModifiersState::empty();
    let mut parts: Vec<&str> = tok.split('+').collect();
    // A literal '+' key ends with "++" and splits into an empty last part.
    let last = if tok.ends_with("++") || tok == "+" {
        parts.truncate(parts.len().saturating_sub(2));
        "+".to_string()
    } else {
        parts.pop()?.to_string()
    };
    for m in parts {
        match m {
            "ctrl" => mods |= ModifiersState::CONTROL,
            "shift" => mods |= ModifiersState::SHIFT,
            "alt" => mods |= ModifiersState::ALT,
            _ => return None,
        }
    }
    let (key, text) = match named(&last) {
        Some(n) => (Key::Named(n), if n == NamedKey::Space { Some(" ".to_string()) } else { None }),
        None if last.chars().count() == 1 => {
            let plain = !mods.contains(ModifiersState::CONTROL) && !mods.contains(ModifiersState::ALT);
            (Key::Character(SmolStr::new(&last)), plain.then(|| last.clone()))
        }
        None => return None,
    };
    Some((mods, KeyIn { key, text }))
}

pub fn run(mut app: App, opts: Opts) {
    // `opts.size` is logical (what the layout sees); the canvas and the PNGs are device pixels.
    let (lw, lh) = opts.size;
    let sf = crate::ui::scale();
    let (w, h) = (((lw as f32 * sf).round() as u32).max(1), ((lh as f32 * sf).round() as u32).max(1));
    app.size = (lw, lh);
    std::fs::create_dir_all(&opts.snap_dir).ok();
    // Startup budget hook (`perf::tests::startup_*`): draw the first frame at once and announce it, so the
    // parent can time process start to first frame instead of the fixed settle below.
    if std::env::var_os("LUMEN_PERF_MARK").is_some() || crate::startup::profiling() {
        frame(&mut app, w, h);
        eprintln!("[start] first frame");
    }
    if !app.first_frame_done {
        app.first_frame_done = true;
        crate::startup::first_frame();
    }
    app.post_frame_init();
    settle(&mut app, 1500);

    for tok in opts.keys.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        if let Some(ms) = tok.strip_prefix("wait:") {
            settle(&mut app, ms.parse().unwrap_or(100));
            frame(&mut app, w, h);
        } else if let Some(name) = tok.strip_prefix("snap:") {
            let mut px = vec![0u32; (w * h) as usize];
            for _ in 0..120 {
                let mut cv = Canvas { px: &mut px, w: w as i32, h: h as i32 };
                if !app.render_into(&mut cv, 0.016) {
                    break;
                }
                app.pump_background();
            }
            let path = opts.snap_dir.join(format!("{name}.png"));
            match write_png(&path, &px, w, h) {
                Ok(()) => {
                    eprintln!("[snap] {}", path.display());
                    crate::mem::report(&format!("after snap:{name}"));
                }
                Err(e) => eprintln!("[snap] failed: {e}"),
            }
        } else if tok == "full" {
            // Render frames until nothing is pending (tokenizing, animations); report the time.
            let mut px = vec![0u32; (w * h) as usize];
            let t0 = Instant::now();
            let mut frames = 0;
            loop {
                let mut cv = Canvas { px: &mut px, w: w as i32, h: h as i32 };
                let busy = app.render_into(&mut cv, 0.016);
                frames += 1;
                if !busy || frames > 200_000 {
                    break;
                }
            }
            eprintln!("[full] {frames} frames in {:?}", t0.elapsed());
        } else if let Some(cmd) = tok.strip_prefix("sh:") {
            #[cfg(unix)]
            let _ = std::process::Command::new("sh").arg("-c").arg(cmd).status();
            #[cfg(windows)]
            let _ = std::process::Command::new("cmd").args(["/C", cmd]).status();
        } else if let Some(text) = tok.strip_prefix("type:") {
            app.mods = ModifiersState::empty();
            for ch in text.chars() {
                let ki = if ch == ' ' {
                    KeyIn { key: Key::Named(NamedKey::Space), text: Some(" ".into()) }
                } else {
                    KeyIn { key: Key::Character(SmolStr::new(ch.to_string())), text: Some(ch.to_string()) }
                };
                app.handle_key(&ki);
                settle(&mut app, 6);
            }
            settle(&mut app, 30);
        } else if let Some(m) = tok.strip_prefix("mods:") {
            // Modifiers that stay held until the next key combo (for wheel / mouse tokens).
            let mut mods = ModifiersState::empty();
            for part in m.split('+') {
                match part.trim() {
                    "ctrl" => mods |= ModifiersState::CONTROL,
                    "shift" => mods |= ModifiersState::SHIFT,
                    "alt" => mods |= ModifiersState::ALT,
                    _ => {}
                }
            }
            app.mods = mods;
        } else if let Some(v) = tok.strip_prefix("wheel:").or_else(|| tok.strip_prefix("pwheel:")) {
            // wheel:dx,dy[,x,y] in notches (pwheel: in pixels); positive = down / right.
            let n: Vec<f64> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
            if n.len() >= 4 {
                app.mouse = (n[2], n[3]);
            }
            let (dx, dy) = (n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0));
            let delta = if tok.starts_with("pwheel:") {
                winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(-dx, -dy))
            } else {
                winit::event::MouseScrollDelta::LineDelta(-dx as f32, -dy as f32)
            };
            app.on_wheel(delta);
        } else if tok == "wheel-end" {
            app.on_wheel_end();
        } else if tok == "still" || tok.starts_with("frames:") {
            // Render 16 ms frames until nothing animates (`still`) or N frames; report how many.
            let limit = tok.strip_prefix("frames:").and_then(|n| n.parse().ok()).unwrap_or(2000usize);
            let mut px = vec![0u32; (w * h) as usize];
            let mut n = 0;
            while n < limit {
                let mut cv = Canvas { px: &mut px, w: w as i32, h: h as i32 };
                let busy = app.render_into(&mut cv, 0.016);
                n += 1;
                if !busy && tok == "still" {
                    break;
                }
            }
            eprintln!("[{}] frames={n} {}", tok.split(':').next().unwrap_or(tok), state_line(&app));
        } else if tok == "state" {
            eprintln!("[state] {}", state_line(&app));
        } else if let Some(xy) = tok.strip_prefix("down:").or_else(|| tok.strip_prefix("move:")) {
            // down:x,y presses the left button and keeps dragging; move:x,y moves the pointer.
            let mut it = xy.split(',').filter_map(|v| v.trim().parse::<f64>().ok());
            if let (Some(x), Some(y)) = (it.next(), it.next()) {
                app.mouse = (x, y);
                if tok.starts_with("down:") {
                    app.mouse_down(MouseButton::Left);
                } else {
                    app.mouse_move();
                    app.update_cursor();
                }
            }
        } else if tok == "up" {
            app.mouse_up();
        } else if let Some(xy) = tok.strip_prefix("rclick:") {
            // Right click: opens the context menu of the tab / explorer row under the pointer.
            let mut it = xy.split(',').filter_map(|v| v.trim().parse::<f64>().ok());
            if let (Some(x), Some(y)) = (it.next(), it.next()) {
                app.mouse = (x, y);
                app.mouse_down(MouseButton::Right);
            }
        } else if let Some(args) = tok.strip_prefix("wheel:") {
            // `wheel:x,y,notches`: mouse wheel at a point (positive = scroll up, like a real wheel).
            let v: Vec<f64> = args.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            if let [x, y, n] = v[..] {
                app.mouse = (x, y);
                app.on_wheel(winit::event::MouseScrollDelta::LineDelta(0.0, n as f32));
                settle(&mut app, 20);
            }
        } else if let Some(args) = tok.strip_prefix("drag:") {
            // `drag:x,y,x2,y2`: press at (x, y), move to (x2, y2), release (scrollbar thumbs, dividers).
            let v: Vec<f64> = args.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            if let [x, y, x2, y2] = v[..] {
                app.mouse = (x, y);
                app.mouse_down(MouseButton::Left);
                app.mouse = (x2, y2);
                app.mouse_move();
                app.regions.drag = None;
                app.drag_bar = None;
                app.dragging = false;
                app.tree.drag = None;
                settle(&mut app, 20);
            }
        } else if let Some(xy) = tok.strip_prefix("click:").or_else(|| tok.strip_prefix("hover:")) {
            let mut it = xy.split(',').filter_map(|v| v.trim().parse::<f64>().ok());
            if let (Some(x), Some(y)) = (it.next(), it.next()) {
                app.mouse = (x, y);
                if tok.starts_with("click:") {
                    frame(&mut app, w, h);
                    app.mouse_down(MouseButton::Left);
                    app.dragging = false;
                    app.tree_drag_release(); // a click on an explorer file opens it on release
                } else {
                    // Layout and hit rects only exist after a frame; draw one so `hover:` works first thing.
                    let mut px = vec![0u32; (w * h) as usize];
                    app.render_into(&mut Canvas { px: &mut px, w: w as i32, h: h as i32 }, 0.016);
                    app.mouse_move();
                    app.update_cursor();
                    settle(&mut app, 700);
                    frame(&mut app, w, h);
                }
            }
        } else if let Some((mods, ki)) = parse_combo(tok) {
            app.mods = mods;
            let quit = app.handle_key(&ki);
            app.mods = ModifiersState::empty();
            settle(&mut app, 40);
            if quit {
                break;
            }
        } else {
            eprintln!("[script] unknown token: {tok}");
        }
    }
    eprintln!("[script] done; docs={} active={}", app.docs.len(), app.active);
}
