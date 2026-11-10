//! Application core: window, layout, tabs, sidebar, status bar and input routing.
//! LSP glue lives in `app_lsp.rs`, palette/find/prompt overlays in `app_overlay.rs`.

use crate::{
    app_overlay::{FindBar, Overlay},
    app_panel::Panel,
    buffer::{Buffer, EditKind},
    clipboard::Clipboard,
    doc::{Doc, DrawOpts},
    git,
    highlight::{Highlighter, SharedTokenizer, comment_prefix, language_for, make_tokenizer},
    lsp::Lsp,
    mem,
    search::Hit,
    theme::Theme,
    tree::Tree,
    ui::{Canvas, Dpi, Rect, TextCtx},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, atomic::AtomicUsize, mpsc},
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Cursor, CursorIcon, Window, WindowId},
};

mod docs;
mod keys;
mod mouse;
mod render;
mod tick;

pub const TAB_H: i32 = 40;
pub use crate::app_topbar::TOP_H;
pub const STATUS_H: i32 = 28;
pub const SIDEBAR_W: i32 = 256;
pub const ROW_H: i32 = 28;
pub const BREAD_H: i32 = 26;
pub const CARD_M: i32 = 0; // flat layout: no gap around the editor
#[allow(dead_code)]
pub const CARD_R: i32 = 0;

pub enum UserEvent {
    Wake,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Sidebar,
    Terminal,
}

/// Keyboard input decoupled from winit's `KeyEvent` so it can also be scripted.
pub struct KeyIn {
    pub key: Key,
    pub text: Option<String>,
}

/// What a breadcrumb segment jumps to.
#[derive(Clone, Debug)]
pub enum Crumb {
    Path(PathBuf),
    Sym(usize, usize), // line, UTF-16 column
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarMode {
    Files,
    Outline,
}

pub struct Gfx {
    pub window: Rc<Window>,
    pub surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

#[derive(Default)]
pub struct Layout {
    pub topbar: Rect,
    pub top_btns: Vec<(Rect, crate::app_topbar::MenuKind)>,
    /// The brand mark in the right corner of the top bar (click: About).
    pub top_logo: Rect,
    pub status_btns: Vec<(Rect, crate::app_topbar::StatusBtn)>,
    /// Non-clickable bottom bar readouts (memory, position, indentation, language): tooltips only.
    pub status_info: Vec<(Rect, crate::app_topbar::StatusBtn)>,
    pub tabbar: Rect,
    pub tabs: Vec<(Rect, Rect, usize)>, // (tab, close button, doc index)
    pub sidebar: Rect,
    pub editor: Rect,
    pub md: Rect,
    /// Split editor: the unfocused pane and the divider (empty when not split).
    pub other: Rect,
    pub divider: Rect,
    pub card: Rect,
    pub status: Rect,
    pub panel: Rect,
    pub bread: Rect,
    pub crumbs: Vec<(Rect, Crumb)>,
    pub sidebar_tabs: Vec<(Rect, SidebarMode)>,
    pub debug_frames: Vec<(Rect, usize)>,
    pub panel_tabs: Vec<(Rect, crate::app_panel::PanelTab)>,
    pub overlay_rows: Vec<(Rect, usize)>,
    pub completion_rows: Vec<(Rect, usize)>,
}

pub struct App {
    pub wake_fn: crate::lsp::Wake,
    pub theme: Theme,
    pub tx: TextCtx,
    pub gfx: Option<Gfx>,
    /// Window size in *logical* pixels (device size / UI scale); all layout works in these.
    pub size: (u32, u32),
    /// Device pixel ratio from the window + the `--scale` / `LUMEN_SCALE` override.
    pub dpi: Dpi,
    pub clip: Clipboard,
    pub assets: PathBuf,

    pub docs: Vec<Doc>,
    pub active: usize,
    pub tokenizers: HashMap<String, SharedTokenizer>,

    pub root: PathBuf,
    pub tree: Tree,
    pub sidebar_open: bool,
    pub sidebar_mode: SidebarMode,
    pub focus: Focus,
    pub branch: Option<String>,
    pub git_status: HashMap<PathBuf, char>,

    pub lsps: Vec<Lsp>,
    pub lsp_warned: bool,
    pub lsp_guard: crate::lsp_guard::Guard,
    /// Formatter file types already reported as unsupported (one message per type and session).
    pub fmt_told: std::collections::HashSet<String>,
    pub config: crate::config::Config,

    pub overlay: Overlay,
    /// Files cut/copied from the explorer context menu.
    pub file_clip: crate::app_ctxmenu::FileClip,
    pub find: Option<FindBar>,
    pub hover: Option<crate::app_lsp::HoverTip>,
    pub hover_probe: Option<(Instant, usize, bool)>, // (since, char pos, requested)
    pub hv: crate::app_hover::HoverCtl,
    pub completion: Option<crate::app_lsp::Completion>,
    pub comp_due: Option<(Instant, Option<char>)>,
    pub comp_t0: Option<Instant>,
    pub file_index: Option<Vec<PathBuf>>,
    pub panel: Panel,
    pub term: Option<crate::terminal::Term>,

    pub search_gen: Arc<AtomicUsize>,
    pub search_tx: mpsc::Sender<(usize, Vec<Hit>)>,
    pub search_rx: mpsc::Receiver<(usize, Vec<Hit>)>,
    pub search_due: Option<Instant>,

    pub layout: Layout,
    pub last_frame: Instant,
    pub mods: ModifiersState,
    pub mouse: (f64, f64),
    pub dragging: bool,
    /// Wheel / drag-selection state (app_input.rs).
    pub inp: crate::app_input::InputState,
    pub clicks: u32,
    pub last_click: Instant,
    pub last_click_pos: (f64, f64),
    /// First stroke of a pending chord (normalized, e.g. "ctrl+k"); see `keymap::DEFAULTS`.
    pub chord: Option<String>,
    /// Git work that outlives the panel: detached command, background counts, toast.
    pub gitbg: crate::app_gitbg::GitBg,
    /// Language / indentation / tab width the user picked per file (kept in the session).
    pub picks: std::collections::HashMap<PathBuf, crate::app_topbar::DocPick>,

    pub status: Option<(String, Instant)>,
    pub quit_armed: bool,
    pub close_armed: Option<usize>,
    pub last_title: String,
    pub mem_cache: (Instant, String),
    pub last_disk_check: Instant,
    pub nav_back: Vec<(PathBuf, usize)>,
    pub nav_fwd: Vec<(PathBuf, usize)>,
    pub blame_for: Option<(usize, usize, Instant)>, // (doc, line, since)
    pub biome_disabled: bool,
    pub eslint_tx: std::sync::mpsc::Sender<(PathBuf, Result<Vec<crate::eslint::Msg>, String>)>,
    pub eslint_rx: std::sync::mpsc::Receiver<(PathBuf, Result<Vec<crate::eslint::Msg>, String>)>,
    pub eslint_busy: std::collections::HashSet<PathBuf>,
    pub tests: crate::app_tests::TestsState,
    pub dbg: Option<crate::app_debug::Debugger>,
    pub diff: Option<crate::app_gitx::DiffView>,
    /// Git panel docked on the right (Changes / History).
    pub gitpanel: Option<Box<crate::app_gitpanel::GitPanel>>,
    pub split: Option<Box<crate::app_split::Split>>,
    pub breakpoints: std::collections::BTreeMap<PathBuf, Vec<usize>>,
    pub dbg_last_stop: Option<(String, usize)>,
    pub pending_native: Option<PathBuf>,
    pub inbox: Option<std::sync::mpsc::Receiver<Vec<String>>>,
    pub watcher: Option<crate::watcher::Watcher>,
    /// When set, re-pull diagnostics of every open TS document (a dependency changed on disk).
    pub lsp_refresh_at: Option<Instant>,
    /// Project roots whose whole directory tree is watched for the language server.
    pub lsp_watched: Vec<PathBuf>,
    pub dragging_minimap: bool,
    pub cursor_icon: CursorIcon,
    pub hover_rect: Option<(Rect, i32)>,
    /// Tooltip of the button under the pointer (shown after a short delay).
    pub tooltip: Option<crate::app_tooltip::Tooltip>,
    /// Scrollbar being dragged: (0 = editor, 1 = sidebar, pointer offset inside the thumb).
    pub drag_bar: Option<(u8, f64)>,
    /// Scrollable panes registered while drawing (wheel + draggable bars).
    pub regions: crate::scrollreg::Regions,
    pub region_hot: bool,
    /// Horizontal scroll of the tab strip (px), its maximum, and the tab last made visible.
    pub tabs_scroll: f64,
    pub tabs_max: f64,
    pub tabs_seen: Option<usize>,
    /// Tab under the pointer and whether the pointer is on its close button (for redraws).
    pub tab_hover: Option<(usize, bool)>,
    pub sidebar_bar: Option<crate::ui::VBar>,
    pub sidebar_bar_hot: bool,
    pub last_trim: Instant,
    pub minimap_default: bool,
    pub theme_orig: Option<String>,
    pub last_tree_refresh: Instant,
    pub keymap: crate::keymap::Keymap,
    pub preedit: String,
    pub actions_expected: usize,
    pub actions_items: Vec<crate::app_overlay::Item>,
    /// Auto-update state (background checks, update card).
    pub upd: crate::app_update::UpdateUi,
}

impl App {
    pub fn new(
        files: Vec<(PathBuf, Option<(usize, usize)>)>,
        root: PathBuf,
        theme: Theme,
        wake_fn: crate::lsp::Wake,
        config: crate::config::Config,
    ) -> Self {
        let mut tx = TextCtx::new(None);
        crate::minimap::set_style(crate::minimap::Style::from_name(&config.minimap_style));
        crate::editor::set_default_tab_w(config.tab_width);
        crate::scroller::set_speed(config.scroll_speed);
        if (config.font_size - 14.0).abs() > f32::EPSILON {
            tx.set_zoom(config.font_size);
        }
        mem::report("fonts ready");
        let assets = crate::config::assets_dir();
        let (stx, srx) = mpsc::channel();
        let tree = Tree::new(root.clone());
        let mut app = Self {
            wake_fn,
            theme,
            tx,
            gfx: None,
            size: (1100, 720),
            dpi: Dpi::from_env(),
            clip: Clipboard::default(),
            assets,
            docs: Vec::new(),
            active: 0,
            tokenizers: HashMap::new(),
            branch: git::branch(&root),
            git_status: HashMap::new(),
            root,
            tree,
            sidebar_open: true,
            sidebar_mode: SidebarMode::Files,
            focus: Focus::Editor,
            lsps: Vec::new(),
            lsp_warned: false,
            lsp_guard: crate::lsp_guard::Guard::new(),
            fmt_told: Default::default(),
            config,
            overlay: Overlay::None,
            file_clip: Default::default(),
            find: None,
            hover: None,
            hover_probe: None,
            hv: Default::default(),
            completion: None,
            comp_due: None,
            comp_t0: None,
            file_index: None,
            panel: Panel::new(),
            term: None,
            search_gen: Arc::new(AtomicUsize::new(0)),
            search_tx: stx,
            search_rx: srx,
            search_due: None,
            layout: Layout::default(),
            last_frame: Instant::now(),
            mods: ModifiersState::empty(),
            mouse: (0.0, 0.0),
            dragging: false,
            inp: Default::default(),
            clicks: 0,
            last_click: Instant::now(),
            last_click_pos: (0.0, 0.0),
            chord: None,
            gitbg: Default::default(),
            picks: Default::default(),
            status: None,
            quit_armed: false,
            close_armed: None,
            last_title: String::new(),
            mem_cache: (Instant::now() - Duration::from_secs(10), String::new()),
            last_disk_check: Instant::now(),
            nav_back: Vec::new(),
            nav_fwd: Vec::new(),
            blame_for: None,
            biome_disabled: false,
            eslint_tx: mpsc::channel().0,
            eslint_rx: mpsc::channel().1,
            eslint_busy: Default::default(),
            tests: crate::app_tests::TestsState::default(),
            dbg: None,
            diff: None,
            gitpanel: None,
            split: None,
            breakpoints: Default::default(),
            dbg_last_stop: None,
            pending_native: None,
            inbox: None,
            watcher: None,
            lsp_refresh_at: None,
            lsp_watched: Vec::new(),
            dragging_minimap: false,
            cursor_icon: CursorIcon::Default,
            hover_rect: None,
            tooltip: None,
            drag_bar: None,
            regions: Default::default(),
            region_hot: false,
            tabs_scroll: 0.0,
            tabs_max: 0.0,
            tabs_seen: None,
            tab_hover: None,
            sidebar_bar: None,
            sidebar_bar_hot: false,
            last_trim: Instant::now(),
            minimap_default: true,
            theme_orig: None,
            last_tree_refresh: Instant::now(),
            keymap: crate::keymap::Keymap::load(),
            preedit: String::new(),
            actions_expected: 0,
            actions_items: Vec::new(),
            upd: crate::app_update::UpdateUi::new(),
        };
        app.refresh_git_status();
        app.watcher = crate::watcher::Watcher::new(app.wake()).ok();
        app.watch_dirs();
        let (etx, erx) = mpsc::channel();
        app.eslint_tx = etx;
        app.eslint_rx = erx;
        let had_args = !files.is_empty();
        for (p, at) in files {
            app.open_path(&p, at);
        }
        if !had_args {
            app.restore_session();
        }
        if std::env::var_os("LUMEN_HEADLESS").is_none() {
            crate::app_topbar::remember_project(&app.root);
        }
        if app.docs.is_empty() {
            app.say("Ctrl+P opens a file · Ctrl+Shift+P commands");
        }
        app
    }

    /// Text from an input method (or any source that is text, not keys): goes through the same
    /// routing as typing so overlays, the find bar and the terminal all receive it.
    pub fn commit_text(&mut self, text: &str) {
        let saved = self.mods;
        self.mods = ModifiersState::empty();
        for ch in text.chars().filter(|c| !c.is_control()) {
            let s = ch.to_string();
            let ki = KeyIn { key: Key::Character(s.clone().into()), text: Some(s) };
            self.handle_key(&ki);
        }
        self.mods = saved;
        self.request_redraw();
    }

    /// Re-read the desktop's light/dark preference when in "system" appearance.
    pub fn follow_system_theme(&mut self) {
        if self.config.appearance != "system" {
            return;
        }
        let want = self.config.resolve_theme(crate::config::detect_dark());
        if self.config.theme.as_deref() != Some(want.as_str()) {
            self.set_theme_inner(&want, false);
        }
    }

    pub fn wake(&self) -> crate::lsp::Wake {
        self.wake_fn.clone()
    }
}

/// macOS: Cmd drives the editor's Ctrl shortcuts (Cmd+C/V/X/Z/S/P/F...). Elsewhere modifiers pass through.
fn map_mods(s: ModifiersState) -> ModifiersState {
    if cfg!(target_os = "macos") && s.super_key() { (s & !ModifiersState::SUPER) | ModifiersState::CONTROL } else { s }
}

/// The brand mark (256 px, embedded) as the window icon; decoded once at start.
fn window_icon() -> Option<winit::window::Icon> {
    static PNG: &[u8] = include_bytes!("../../packaging/icons/png/lumen-text-256.png");
    let pix = crate::mdimg::decode_png(PNG)?;
    let rgba: Vec<u8> =
        pix.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]).collect();
    winit::window::Icon::from_rgba(rgba, pix.w, pix.h).ok()
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("lumen-text")
            .with_window_icon(window_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0));
        // Wayland app_id / X11 class must match lumen-text.desktop so the launcher shows our icon.
        #[cfg(target_os = "linux")]
        {
            use winit::platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11};
            attrs = WindowAttributesExtWayland::with_name(attrs, "lumen-text", "lumen-text");
            attrs = WindowAttributesExtX11::with_name(attrs, "lumen-text", "lumen-text");
        }
        let window = Rc::new(el.create_window(attrs).expect("window"));
        window.set_cursor(Cursor::Icon(CursorIcon::Default));
        window.set_ime_allowed(true);
        let ctx = softbuffer::Context::new(window.clone()).expect("softbuffer ctx");
        let surface = softbuffer::Surface::new(&ctx, window.clone()).expect("surface");
        self.dpi.device = window.scale_factor();
        self.gfx = Some(Gfx { window, surface });
        self.apply_scale();
        mem::report("window created");
    }

    fn user_event(&mut self, _: &ActiveEventLoop, _: UserEvent) {
        self.pump_background();
        self.request_redraw();
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let next = self.tick();
        el.set_control_flow(match next {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => {
                if self.try_quit() {
                    el.exit();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dpi.device = scale_factor;
                self.apply_scale();
                if let Some(g) = &self.gfx {
                    let s = g.window.inner_size();
                    self.size = crate::ui::logical_size(s.width, s.height);
                }
                self.request_redraw()
            }
            WindowEvent::Resized(s) => {
                self.size = crate::ui::logical_size(s.width, s.height);
                self.compute_layout();
                for d in &mut self.docs {
                    d.reveal = true;
                }
                self.request_redraw()
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                self.preedit.clear();
                self.commit_text(&text);
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(text, _)) => {
                self.preedit = text;
                self.request_redraw();
            }
            WindowEvent::Focused(false) => self.git_bg_focus(false),
            WindowEvent::Focused(true) => {
                self.git_bg_focus(true);
                self.follow_system_theme();
                self.last_disk_check = Instant::now() - Duration::from_secs(10);
                self.refresh_git_status();
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = map_mods(m.state()),
            WindowEvent::CursorLeft { .. } => self.tooltip_leave(),
            WindowEvent::CursorMoved { position, .. } => {
                // winit reports device pixels; everything inside the app is logical.
                let s = crate::ui::scale() as f64;
                self.mouse = (position.x / s, position.y / s);
                self.mouse_move();
                self.update_cursor();
            }
            WindowEvent::MouseInput { state, button, .. } => match (state, button) {
                (ElementState::Pressed, MouseButton::Left | MouseButton::Middle | MouseButton::Right) => {
                    self.mouse_down(button)
                }
                (ElementState::Released, _) => self.mouse_up(),
                _ => {}
            },
            WindowEvent::MouseWheel { delta, phase, .. } => {
                let s = crate::ui::scale() as f64;
                let delta = match delta {
                    MouseScrollDelta::PixelDelta(p) => {
                        MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(p.x / s, p.y / s))
                    }
                    d => d,
                };
                if phase == winit::event::TouchPhase::Ended { self.on_wheel_end() } else { self.on_wheel(delta) }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && self.on_key(&event) => {
                el.exit();
            }
            _ => {}
        }
    }
}

pub fn run(app: App, el: EventLoop<UserEvent>) {
    let mut app = app;
    el.run_app(&mut app).expect("run");
}
