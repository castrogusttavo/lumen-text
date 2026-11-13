#![allow(clippy::too_many_arguments, clippy::type_complexity)]
// Release builds on Windows are GUI-subsystem programs (no console window); `winconsole` re-attaches
// the parent console for command-line use.
#![cfg_attr(all(windows, not(debug_assertions), not(test)), windows_subsystem = "windows")]
mod app;
mod app_aipanel;
mod app_ask;
mod app_assist;
mod app_backup;
mod app_build;
mod app_coverage;
mod app_ctxmenu;
mod app_debug;
mod app_display;
mod app_elevated;
mod app_explorer;
mod app_filefmt;
mod app_files;
mod app_format;
mod app_fswatch;
mod app_git;
mod app_gitbg;
mod app_gitcmp;
mod app_gitext;
mod app_gitpanel;
mod app_gitx;
mod app_gitx_lines;
mod app_goto;
mod app_hover;
mod app_http;
mod app_ignored;
mod app_img;
mod app_inline;
mod app_input;
mod app_jump;
mod app_localhist;
mod app_lsp;
mod app_lspx;
mod app_md;
mod app_merge;
mod app_overlay;
mod app_panel;
mod app_problems;
mod app_projcfg;
mod app_remote;
mod app_reopen;
mod app_resmon;
mod app_settings;
mod app_sidebar;
mod app_snippets;
mod app_split;
mod app_structural;
mod app_tabdrag;
mod app_termio;
mod app_terms;
mod app_tests;
mod app_tooltip;
mod app_topbar;
mod app_unsaved;
mod app_update;
mod app_view;
mod app_window;
mod app_workspace;
mod app_worktree;
mod assist;
mod backup;
mod bidi;
mod bigfile;
mod bookmarks;
mod buffer;
mod buildrun;
mod cli;
mod clipboard;
mod config;
mod coverage;
mod dap;
mod dap_adapters;
#[cfg(test)]
mod dap_tests;
mod debug_model;
mod display;
mod doc;
mod doc_scroll;
mod docview;
mod editor;
mod editorconfig;
mod emmet;
mod encoding;
mod eslint;
mod field;
mod fontcfg;
mod fontpicker;
#[cfg(test)]
mod fonts_tests;
mod format;
mod fstype;
mod fuzzy;
mod ghpr;
mod git;
mod gitblame;
mod gitcmp;
mod gitpatch;
mod gitrebase;
mod gitsync;
mod gitx;
mod gotopick;
mod highlight;
mod hover_md;
mod httpclient;
mod httpfile;
mod icon_pack;
mod icons;
mod ide;
mod ignore_filter;
mod imgview;
mod inlay;
mod inline;
mod instance;
mod keymap;
mod kittykeys;
mod localhist;
mod lsp;
mod lsp_guard;
#[cfg(test)]
mod lsp_matrix;
mod lsp_stream;
mod lsplog;
mod lspmerge;
mod lspreg;
mod lspsup;
mod macros;
mod mdimg;
mod mdview;
mod mem;
mod merge3;
mod minimap;
mod notify;
mod paths;
mod perf;
mod poller;
mod problems;
mod projcfg;
mod pty;
mod pyenv;
mod remote;
mod replace;
mod resmon;
mod rowmap;
mod script;
mod scroller;
mod scrollreg;
mod search;
mod settings_ext;
mod settingsui;
mod shellint;
mod snippet_store;
mod snippet_syntax;
mod snippets;
mod startup;
mod structural;
mod tasks;
mod terminal;
mod termlink;
mod termnav;
mod termview;
mod testrun;
mod textarea;
mod textmate;
mod theme;
mod tree;
mod ui;
mod undo_store;
mod update;
mod watcher;
mod whitespace;
mod winconsole;
mod workspace;
mod worktree;
mod zedtheme;

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

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(code.clamp(0, 255) as u8)
}

fn main() -> ExitCode {
    startup::begin();
    winconsole::init(env::args().skip(1).any(|a| a.starts_with('-')));
    // Few malloc arenas: with background threads (tokenizer, LSP readers) glibc otherwise keeps
    // hundreds of MB of freed memory per thread arena.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 4 * 1024 * 1024);
    }
    let mut cli = cli::parse(env::args().skip(1));
    if cli.profile_startup {
        startup::set_profile(true);
        startup::set_deferred(true);
        // No window needed to measure: run the headless path (first frame, then exit).
        if cli.keys.is_none() {
            cli.keys = Some("wait:0".into());
        }
    } else {
        startup::set_deferred(true);
    }
    startup::mark("cli parsed");
    match cli.early {
        Some(cli::Early::Version) => {
            println!("lumen-text {}", version());
            return ExitCode::SUCCESS;
        }
        Some(cli::Early::InstallUser) => return exit_code(update::cli_install_user()),
        Some(cli::Early::CheckUpdate) => return exit_code(update::cli_check(false)),
        Some(cli::Early::Update) => return exit_code(update::cli_check(true)),
        None => {}
    }
    let (args, keys, snap, size, scale_arg) =
        (cli.args.clone(), cli.keys.clone(), cli.snap.clone(), cli.size, cli.scale);
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
    startup::mark("config loaded");
    let theme = match theme::Theme::load(&theme_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("theme error ({}): {e}", theme_path.display());
            return ExitCode::FAILURE;
        }
    };

    startup::mark("theme loaded");
    let mut files = Vec::new();
    let mut root: Option<PathBuf> = None;
    // A `.lumen-workspace` / `.code-workspace` file: its first folder is the project, the rest
    // are extra folders of the window (see `workspace`).
    let mut ws_file: Option<(PathBuf, Vec<PathBuf>)> = None;
    for a in &args {
        let (p, at) = parse_target(a);
        if workspace::is_workspace_file(&p) && p.is_file() {
            match workspace::load(&p) {
                Ok(f) => {
                    root = Some(f[0].clone());
                    ws_file = Some((p, f.into_iter().skip(1).collect()));
                }
                Err(e) => eprintln!("workspace {}: {e}", p.display()),
            }
        } else if p.is_dir() {
            root = Some(paths::canonical(&p).unwrap_or(p));
        } else {
            files.push((p, at));
        }
    }
    let mut root = root
        .or_else(|| files.first().and_then(|(p, _)| paths::canonical(p).ok()).and_then(|p| lsp::project_root(&p)))
        .or_else(|| {
            // Launched without a target: reopen the project used last time.
            if args.is_empty() && keys.is_none() {
                app_topbar::load_recents().into_iter().find(|p| p.is_dir())
            } else {
                None
            }
        })
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    if keys.is_some() {
        unsafe { env::set_var("LUMEN_HEADLESS", "1") };
    }
    if let Some(keys) = keys {
        // Headless: same input/render code as the GUI, no window.
        let wake: lsp::Wake = std::sync::Arc::new(|| {});
        let mut app = app::App::new(files, root, theme, wake, cfg.clone());
        if let Some((f, extra)) = ws_file.take() {
            app.ws_attach(extra, Some(f));
        }
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
    // Files of a project that is open go to its window; an open folder raises its window (see
    // `instance`). Otherwise this process becomes a new window.
    let (inbox, inst) = match instance::claim(&args, cli.win.forces_new() || ws_file.is_some(), wake.clone()) {
        instance::Role::Secondary => return ExitCode::SUCCESS,
        instance::Role::Primary(rx, inst) => (rx, inst),
    };
    // Started without targets and no window running: reopen every window of the last run.
    let reopen = args.is_empty() && !cli.win.forces_new();
    let mut slot = cli.win.slot;
    if reopen
        && let Some((first, rest)) =
            app_window::set_path().and_then(|p| app_window::restore_plan(&app_window::load_set(&p)))
    {
        root = first.root;
        slot = Some(first.slot);
        for a in rest {
            let _ = app_window::spawn_window(&a);
        }
    }
    let slot = slot.unwrap_or_else(|| inst.as_ref().map_or(0, |i| instance::free_slot(&i.others, &root)));
    // "New window" and a moved tab start without the project's saved tabs.
    let restore = cli.win.slot.is_some() || (!cli.win.new_window && cli.win.take.is_none());
    app_window::set_launch(app_window::Launch { slot, restore });
    std::thread::spawn(update::refresh_user_integration);
    // Learn the user's shell PATH now so the first task or test run does not wait for it.
    std::thread::spawn(|| {
        tasks::login_path();
    });
    startup::mark("instance claimed");
    let mut app = app::App::new(files, root, theme, wake, cfg.clone());
    if let Some((f, extra)) = ws_file.take() {
        app.ws_attach(extra, Some(f));
    }
    if scale_arg.is_some() {
        app.dpi.forced = scale_arg;
    }
    app.inbox = Some(inbox);
    app.win_attach(inst, !reopen && cli.win.slot.is_none());
    match &cli.win.take {
        Some(t) => {
            app.take_handoff(t);
        }
        None => app.take_orphans(),
    }
    mem::report("app ready");
    startup::mark("app ready");
    app::run(app, el);
    ExitCode::SUCCESS
}
