mod app;
mod app_ctxmenu;
mod app_explorer;
mod app_debug;
mod app_files;
mod app_gitx;
mod app_hover;
mod app_gitpanel;
mod app_git;
mod app_lsp;
mod app_md;
mod app_input;
mod app_overlay;
mod app_panel;
mod buffer;
mod config;
mod clipboard;
mod dap;
mod doc;
mod doc_scroll;
mod docview;
mod emmet;
mod eslint;
mod editor;
mod field;
mod format;
mod fuzzy;
mod git;
mod gitsync;
mod gitx;
mod highlight;
mod hover_md;
mod icons;
mod instance;
mod keymap;
mod lsp;
mod mdimg;
mod mdview;
mod mem;
mod minimap;
mod perf;
mod script;
mod snippets;
mod rowmap;
mod scrollreg;
mod scroller;
mod search;
mod tasks;
mod inlay;
mod watcher;
mod zedtheme;
mod settingsui;
mod app_settings;
mod app_split;
mod app_tooltip;
mod app_topbar;
mod app_unsaved;
mod terminal;
mod textmate;
mod theme;
mod tree;
mod ui;

use std::{env, path::PathBuf, process::ExitCode};
use winit::event_loop::EventLoop;

/// `file`, `file:line` or `file:line:col` (1-based).
pub(crate) fn parse_target(arg: &str) -> (PathBuf, Option<(usize, usize)>) {
    let p = PathBuf::from(arg);
    if p.exists() {
        return (p, None);
    }
    let mut parts = arg.rsplitn(3, ':').collect::<Vec<_>>();
    parts.reverse();
    match parts.as_slice() {
        [f, l] => match l.parse::<usize>() {
            Ok(l) => (PathBuf::from(f), Some((l.saturating_sub(1), 0))),
            Err(_) => (p, None),
        },
        [f, l, c] => match (l.parse::<usize>(), c.parse::<usize>()) {
            (Ok(l), Ok(c)) => (PathBuf::from(f), Some((l.saturating_sub(1), c.saturating_sub(1)))),
            _ => (p, None),
        },
        _ => (p, None),
    }
}

/// Release builds are stamped by CI with `LUMEN_VERSION` (CalVer `YYYY.M.D.N`); local builds use Cargo's.
fn version() -> &'static str {
    option_env!("LUMEN_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn main() -> ExitCode {
    // Few malloc arenas: with background threads (tokenizer, LSP readers) glibc otherwise keeps
    // hundreds of MB of freed memory per thread arena.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 4 * 1024 * 1024);
    }
    let mut args: Vec<String> = Vec::new();
    let (mut keys, mut snap, mut size) = (None::<String>, PathBuf::from("/tmp/lumen-snap"), (1000u32, 700u32));
    let mut scale_arg = None::<f32>;
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!("lumen-text {}", version());
                return ExitCode::SUCCESS;
            }
            "--keys" => keys = it.next(),
            "--scale" => scale_arg = it.next().as_deref().and_then(ui::parse_scale),
            "--snap" => snap = PathBuf::from(it.next().unwrap_or_default()),
            "--size" => {
                if let Some((w, h)) = it.next().as_deref().and_then(|s| s.split_once('x')) {
                    size = (w.parse().unwrap_or(1000), h.parse().unwrap_or(700));
                }
            }
            _ if a.starts_with("--") => {}
            _ => args.push(a),
        }
    }
    let cfg = config::Config::load();
    theme::set_theme_chrome(cfg.own_chrome());
    let theme_path = env::var("LUMEN_THEME").map(PathBuf::from).unwrap_or_else(|_| {
        let name = cfg.resolve_theme(if cfg.appearance == "system" { config::detect_dark() } else { None });
        match zedtheme::list_user_themes().into_iter().find(|(n, _)| *n == name) {
            Some((_, p)) => p,
            None => config::assets_dir().join("themes").join(format!("{name}.json")),
        }
    });
    mem::report("startup");
    let theme = match theme::Theme::load(&theme_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("theme error ({}): {e}", theme_path.display());
            return ExitCode::FAILURE;
        }
    };

    let mut files = Vec::new();
    let mut root: Option<PathBuf> = None;
    for a in &args {
        let (p, at) = parse_target(a);
        if p.is_dir() {
            root = Some(p.canonicalize().unwrap_or(p));
        } else {
            files.push((p, at));
        }
    }
    let root = root
        .or_else(|| files.first().and_then(|(p, _)| p.canonicalize().ok()).and_then(|p| lsp::project_root(&p)))
        .or_else(|| {
            // Launched without a target: reopen the project used last time.
            if args.is_empty() && keys.is_none() { app_topbar::load_recents().into_iter().find(|p| p.is_dir()) } else { None }
        })
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    if keys.is_some() {
        unsafe { env::set_var("LUMEN_HEADLESS", "1") };
    }
    if let Some(keys) = keys {
        // Headless: same input/render code as the GUI, no window.
        let wake: lsp::Wake = std::sync::Arc::new(|| {});
        let mut app = app::App::new(files, root, theme, wake, cfg.clone());
        // `--size` is the logical window size; the snapshots are `size * scale` device pixels.
        if scale_arg.is_some() {
            app.dpi.forced = scale_arg;
        }
        app.apply_scale();
        script::run(app, script::Opts { keys, snap_dir: snap, size });
        return ExitCode::SUCCESS;
    }
    let el = EventLoop::<app::UserEvent>::with_user_event().build().expect("event loop");
    let proxy = el.create_proxy();
    let wake: lsp::Wake = std::sync::Arc::new(move || {
        let _ = proxy.send_event(app::UserEvent::Wake);
    });
    // One window per user: hand the files to a running instance and leave.
    let inbox = match instance::claim(&args, wake.clone()) {
        instance::Role::Secondary => return ExitCode::SUCCESS,
        instance::Role::Primary(rx) => rx,
    };
    let mut app = app::App::new(files, root, theme, wake, cfg.clone());
    if scale_arg.is_some() {
        app.dpi.forced = scale_arg;
    }
    app.inbox = Some(inbox);
    mem::report("app ready");
    app::run(app, el);
    ExitCode::SUCCESS
}
