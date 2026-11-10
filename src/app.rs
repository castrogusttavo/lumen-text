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

    // ── documents ──

    pub fn say(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// Like [`say`](Self::say), but the message stays visible `extra_secs` longer (warnings).
    pub fn say_for(&mut self, msg: impl Into<String>, extra_secs: u64) {
        self.status = Some((msg.into(), Instant::now() + Duration::from_secs(extra_secs)));
    }

    pub fn request_redraw(&self) {
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    pub(crate) fn tokenizer_for(&mut self, lang: &str) -> Option<SharedTokenizer> {
        let reg = crate::textmate::Registry::new(&self.assets).ok()?;
        let scope = reg.scope_for_language(lang)?;
        if let Some(t) = self.tokenizers.get(&scope) {
            return Some(t.clone());
        }
        let t = make_tokenizer(&self.assets, &scope)?;
        self.tokenizers.insert(scope, t.clone());
        Some(t)
    }

    /// Open (or activate) a file. Returns its index.
    pub fn open_path(&mut self, path: &Path, at: Option<(usize, usize)>) -> Option<usize> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let idx = if let Some(i) = self.docs.iter().position(|d| d.path() == Some(path.as_path())) {
            i
        } else {
            let buf = match Buffer::open(&path) {
                Ok(b) => b,
                Err(e) => {
                    self.say(format!("Could not open {}: {e}", path.display()));
                    return None;
                }
            };
            let lang = language_for(&path.to_string_lossy());
            let hl = lang.and_then(|l| self.tokenizer_for(l)).map(Highlighter::new);
            let comment = comment_prefix(&path.to_string_lossy());
            let mut doc = Doc::new(buf, hl, lang, comment, self.tx.cell_w);
            doc.bps = self.breakpoints.get(&path).cloned().unwrap_or_default();
            doc.minimap = self.config.minimap;
            doc.indent_guides = self.config.indent_guides;
            doc.sticky = self.config.sticky_scroll;
            doc.lens_refs = self.config.code_lens_refs;
            doc.lens_impls = self.config.code_lens_impls;
            doc.set_word_wrap(self.config.word_wrap);
            doc.git_base = git::head_text(&path);
            self.docs.push(doc);
            self.docs.len() - 1
        };
        self.activate(idx);
        if let Some((line, col)) = at
            && let Some(d) = self.docs.get_mut(idx)
        {
            let p = d.buf.pos(line, col);
            d.ed.click(&mut d.buf, p, false);
            d.reveal = true;
        }
        self.tree.reveal(&path);
        Some(idx)
    }

    fn here(&self) -> Option<(PathBuf, usize)> {
        let d = self.docs.get(self.active)?;
        Some((d.path()?.to_path_buf(), d.ed.head()))
    }

    /// Remember where we are before a jump (definition, search result, symbol…).
    pub fn push_nav(&mut self) {
        if let Some(h) = self.here() {
            if self.nav_back.last() != Some(&h) {
                self.nav_back.push(h);
                if self.nav_back.len() > 100 {
                    self.nav_back.remove(0);
                }
            }
            self.nav_fwd.clear();
        }
    }

    fn jump_to(&mut self, path: &Path, at: usize) {
        if let Some(i) = self.open_path(path, None) {
            let d = &mut self.docs[i];
            let at = at.min(d.buf.len_chars());
            d.ed.click(&mut d.buf, at, false);
            d.reveal = true;
        }
    }

    pub fn navigate(&mut self, back: bool) {
        let target = if back { self.nav_back.pop() } else { self.nav_fwd.pop() };
        let Some((path, at)) = target else {
            return self.say(if back { "no previous position" } else { "no next position" });
        };
        if let Some(h) = self.here() {
            if back { self.nav_fwd.push(h) } else { self.nav_back.push(h) }
        }
        self.jump_to(&path, at);
    }

    /// Watch the project root, every expanded folder of the tree, and the folders of open files.
    pub fn watch_dirs(&mut self) {
        let Some(w) = self.watcher.as_mut() else { return };
        let _ = w.watch_dir(&self.root);
        for r in &self.tree.rows {
            if r.is_dir && r.expanded {
                let _ = w.watch_dir(&r.path);
            }
        }
        for d in &self.docs {
            if let Some(p) = d.path().and_then(Path::parent) {
                let _ = w.watch_dir(p);
            }
        }
        // A running language server needs to hear about every source file changing on disk, also in
        // folders that are collapsed in the tree.
        for l in &self.lsps {
            if l.name == "tsgo" && !self.lsp_watched.contains(&l.root) {
                self.lsp_watched.push(l.root.clone());
                crate::watcher::watch_tree(w, &l.root, 4000);
            }
        }
    }

    /// React to file-system events: reload changed files right away and refresh the tree.
    pub fn pump_watcher(&mut self) {
        let mut evs = Vec::new();
        if let Some(w) = &self.watcher {
            while let Ok(e) = w.rx.try_recv() {
                evs.push(e);
            }
        }
        if evs.is_empty() {
            return;
        }
        self.lsp_fs_events(&evs);
        self.last_disk_check = Instant::now() - Duration::from_secs(60);
        if self.last_tree_refresh.elapsed() > Duration::from_millis(400) {
            self.last_tree_refresh = Instant::now();
            self.tree.rebuild();
            self.file_index = None;
            self.refresh_git_status();
            self.watch_dirs();
        }
        self.request_redraw();
    }

    pub fn activate(&mut self, idx: usize) {
        if idx < self.docs.len() {
            self.active = idx;
            self.hover = None;
            self.completion = None;
            self.close_armed = None;
            self.request_redraw();
        }
    }

    pub fn close_tab(&mut self, idx: usize, force: bool) {
        let Some(d) = self.docs.get(idx) else { return };
        if d.buf.is_dirty() && !force && self.close_armed != Some(idx) {
            self.close_armed = Some(idx);
            let n = d.name.clone();
            self.say(format!("{n} has unsaved changes — close again to discard"));
            return;
        }
        self.lsp_did_close(idx);
        self.docs.remove(idx);
        self.split_doc_removed(idx);
        self.close_armed = None;
        if self.active >= self.docs.len() {
            self.active = self.docs.len().saturating_sub(1);
        } else if idx < self.active {
            self.active -= 1;
        }
        self.request_redraw();
    }

    pub fn save_active(&mut self) {
        if self.docs.get(self.active).is_some_and(|d| d.path().is_none()) {
            // Untitled document: nowhere to write yet, ask for a path.
            return self.ask_save_as(self.active, Vec::new(), None, String::new());
        }
        let warn = self.format_for_save(self.active);
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let name = d.name.clone();
        match d.buf.save() {
            Ok(()) => {
                d.git_stale = true;
                d.disk_mtime = d.stat_mtime();
                d.conflict = false;
                let idx = self.active;
                self.lsp_did_save(idx);
                self.refresh_git_status();
                match warn {
                    Some(w) => self.say_for(format!("Saved {name}, but {w}"), 8),
                    None => self.say(format!("Saved {name}")),
                }
                self.tests_on_save();
            }
            Err(e) => self.say(format!("Error saving {name}: {e}")),
        }
    }

    // ── timers ──

    /// Run whatever is due; returns the next deadline, if any.
    pub fn tick(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut want = |t: Instant| next = Some(next.map_or(t, |n| n.min(t)));

        self.lsp_tick(now, &mut want);
        if self.last_trim.elapsed() > Duration::from_secs(4) {
            self.last_trim = Instant::now();
            crate::app_lsp::release_memory();
        }
        want(self.last_trim + Duration::from_secs(4));
        self.check_disk(now, &mut want);
        self.gitpanel_tick(now, &mut want);
        self.update_tick(now, &mut want);
        self.git_bg_tick(now, &mut want);
        self.md_tick(now, &mut want);
        self.tests_tick(now, &mut want);

        // Git gutter against the buffer, debounced.
        let active = self.active;
        for (i, d) in self.docs.iter_mut().enumerate() {
            if !d.git_stale {
                continue;
            }
            let due = d.changed_at.map_or(now, |t| t + Duration::from_millis(350));
            if due > now && i == active {
                want(due);
                continue;
            }
            if i != active && d.changed_at.is_some() {
                // background tabs refresh when activated
            }
            d.git_stale = false;
            d.git = match (&d.git_base, d.buf.line_count() < 200_000) {
                (Some(base), true) => git::marks(base, &d.buf.to_string()),
                _ => Vec::new(),
            };
        }

        // Inline blame: follows the cursor line, computed after the cursor rests a moment.
        if let Some(d) = self.docs.get_mut(self.active) {
            let (line, _) = d.buf.line_col(d.ed.head());
            if d.blame.as_ref().is_some_and(|(l, _)| *l != line) || d.buf.is_dirty() || !self.config.git_blame {
                d.blame = None;
            }
            let tracked = self.config.git_blame && d.git_base.is_some() && !d.buf.is_dirty();
            if tracked && d.blame.is_none() {
                match self.blame_for {
                    Some((i, l, since)) if i == self.active && l == line => {
                        let due = since + Duration::from_millis(280);
                        if due <= now {
                            if let Some(p) = d.path().map(Path::to_path_buf) {
                                d.blame = git::blame_line(&p, line).map(|t| (line, t));
                            }
                            self.blame_for = None;
                            self.request_redraw();
                        } else {
                            want(due);
                        }
                    }
                    _ => {
                        self.blame_for = Some((self.active, line, now));
                        want(now + Duration::from_millis(280));
                    }
                }
            }
        }

        // Merge-conflict markers, a moment after the text settles.
        for d in &mut self.docs {
            let Some(due) = d.conflicts_due else { continue };
            if due > now {
                want(due);
                continue;
            }
            d.conflicts_due = None;
            let text = d.buf.to_string();
            d.conflicts = if text.contains("<<<<<<<") { crate::gitx::parse_conflicts(&text) } else { Vec::new() };
        }

        // ESLint on a worker thread, a moment after typing stops.
        for i in 0..self.docs.len() {
            let Some(due) = self.docs[i].eslint_due else { continue };
            if due > now {
                want(due);
                continue;
            }
            self.docs[i].eslint_due = None;
            let Some(path) = self.docs[i].path().map(Path::to_path_buf) else { continue };
            if !matches!(self.docs[i].lang, Some("typescript" | "tsx" | "javascript" | "jsx"))
                || self.eslint_busy.contains(&path)
            {
                continue;
            }
            let Some((root, bin)) = crate::eslint::detect(&path) else { continue };
            self.eslint_busy.insert(path.clone());
            let text = self.docs[i].buf.to_string();
            let (tx, wake) = (self.eslint_tx.clone(), self.wake());
            std::thread::spawn(move || {
                let res = crate::eslint::run(&root, &bin, &path, &text);
                let _ = tx.send((path, res));
                wake();
            });
        }

        if let Some(t) = self.search_due {
            if t <= now {
                self.search_due = None;
                self.start_search();
            } else {
                want(t);
            }
        }
        self.tooltip_tick(now, &mut want);
        self.hover_tick(now, &mut want);
        if let Some((since, pos, requested)) = self.hover_probe {
            let due = since + Duration::from_millis(450);
            if !requested {
                if due <= now {
                    self.hover_probe = Some((since, pos, true));
                    self.request_hover(pos);
                } else {
                    want(due);
                }
            }
        }
        next
    }

    /// Pick up edits made by other programs (git checkout, formatters, other editors).
    fn check_disk(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        // The inotify watcher reports changes immediately; this is only a safety net.
        const EVERY: Duration = Duration::from_millis(6000);
        let due = self.last_disk_check + EVERY;
        if due > now {
            want(due);
            return;
        }
        self.last_disk_check = now;
        want(now + EVERY);
        let mut notes: Vec<String> = Vec::new();
        for d in &mut self.docs {
            let (Some(old), Some(cur)) = (d.disk_mtime, d.stat_mtime()) else { continue };
            if old == cur {
                continue;
            }
            if d.buf.is_dirty() {
                d.disk_mtime = Some(cur);
                if !d.conflict {
                    d.conflict = true;
                    notes.push(format!("{} changed on disk and you have unsaved changes (Ctrl+S overwrites)", d.name));
                }
            } else if d.reload_from_disk().is_ok() {
                notes.push(format!("{} reloaded (changed on disk)", d.name));
            }
        }
        if let Some(n) = notes.pop() {
            self.say(n);
            self.request_redraw();
        }
    }

    // ── session ──

    fn session_path(&self) -> Option<PathBuf> {
        let base = crate::paths::state_home()?;
        let h = self
            .root
            .to_string_lossy()
            .bytes()
            .fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
        Some(base.join("lumen-text").join(format!("session-{h:016x}.json")))
    }

    pub fn save_session(&self) {
        let Some(path) = self.session_path() else { return };
        let files: Vec<serde_json::Value> = self
            .docs
            .iter()
            .filter_map(|d| {
                let (l, c) = d.buf.line_col(d.ed.head());
                let mut entry = serde_json::json!({"path": d.path()?.to_string_lossy(), "line": l, "col": c, "top": d.target, "pinned": d.pinned, "md": d.md.is_some()});
                self.picks_to_json(d.path()?, &mut entry);
                Some(entry)
            })
            .collect();
        // Index of each document's entry in `files` (documents without a path are not saved).
        let mut file_ix = vec![None; self.docs.len()];
        for (k, i) in self.docs.iter().enumerate().filter(|(_, d)| d.path().is_some()).map(|(i, _)| i).enumerate() {
            file_ix[i] = Some(k);
        }
        let mut v = serde_json::json!({"active": self.active, "files": files});
        if let Some(sp) = self.split_session(&file_ix) {
            v["split"] = sp;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, v.to_string());
    }

    pub fn restore_session(&mut self) {
        let Some(path) = self.session_path() else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let mut opened: Vec<Option<usize>> = Vec::new();
        for f in v["files"].as_array().into_iter().flatten() {
            opened.push(None);
            let Some(p) = f["path"].as_str() else { continue };
            if !Path::new(p).exists() {
                continue;
            }
            let at = Some((f["line"].as_u64().unwrap_or(0) as usize, f["col"].as_u64().unwrap_or(0) as usize));
            let i = self.open_path(Path::new(p), at);
            *opened.last_mut().unwrap() = i;
            if let Some(i) = i {
                let top = f["top"].as_f64().unwrap_or(0.0);
                self.docs[i].pinned = f["pinned"].as_bool().unwrap_or(false);
                self.docs[i].md_restore(f["md"].as_bool().unwrap_or(false));
                self.apply_picks_json(i, f);
                self.docs[i].target = top;
                self.docs[i].top = top;
                self.docs[i].reveal = false;
            }
        }
        let active = v["active"].as_u64().unwrap_or(0) as usize;
        if active < self.docs.len() {
            self.activate(active);
        }
        if v["split"].is_object() {
            self.split_restore(&v["split"], &opened);
        }
    }

    // ── layout & rendering ──

    /// Re-derive the UI scale (window scale factor, `LUMEN_SCALE` / `--scale`, or the `ui_scale`
    /// setting) and re-measure the fonts when it changed.
    pub fn apply_scale(&mut self) {
        let s = self.dpi.effective(self.config.ui_scale);
        if (s - crate::ui::scale()).abs() > 1e-4 {
            self.tx.set_scale(s);
            for d in &mut self.docs {
                d.reveal = true;
            }
        }
    }

    fn compute_layout(&mut self) {
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let sb = if self.sidebar_open { SIDEBAR_W } else { 0 };
        self.layout.topbar = Rect::new(0, 0, w, TOP_H);
        self.layout.sidebar = Rect::new(0, TOP_H, sb, h - STATUS_H - TOP_H);
        let rw = self.gitpanel_w();
        self.layout.tabbar = Rect::new(sb, TOP_H, w - sb - rw, TAB_H);
        let bread_h = if self.config.breadcrumbs { BREAD_H } else { 0 };
        self.layout.bread = Rect::new(sb, TOP_H + TAB_H, (w - sb - rw).max(0), bread_h);
        let avail = (h - TOP_H - TAB_H - bread_h - STATUS_H).max(0);
        let panel_h = if self.panel.open { self.panel.height.min(avail / 2).max(0) } else { 0 };
        let gap = if panel_h > 0 { CARD_M } else { 0 };
        let card_h = (avail - CARD_M - panel_h - gap).max(0);
        self.layout.card = Rect::new(sb + CARD_M, TOP_H + TAB_H + bread_h, (w - sb - 2 * CARD_M - rw).max(0), card_h);
        self.layout.editor = self.layout.card;
        self.layout.panel =
            Rect::new(sb + CARD_M, TOP_H + TAB_H + bread_h + card_h + gap, (w - sb - 2 * CARD_M - rw).max(0), panel_h);
        self.layout.status = Rect::new(0, h - STATUS_H, w, STATUS_H);
        self.split_layout();
    }

    /// Draw one frame into any canvas (used by the headless script runner).
    pub fn render_into(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        self.compute_layout();
        self.draw_all(cv, dt)
    }

    fn render(&mut self) {
        let Some(mut g) = self.gfx.take() else { return };
        let size = g.window.inner_size();
        self.size = crate::ui::logical_size(size.width, size.height);
        let (Some(w), Some(h)) = (std::num::NonZeroU32::new(size.width), std::num::NonZeroU32::new(size.height)) else {
            self.gfx = Some(g);
            return;
        };
        let since = self.last_frame.elapsed().as_secs_f64();
        let dt = if since > 0.1 { 0.016 } else { since.min(0.05) };
        let t0 = Instant::now();
        self.compute_layout();

        g.surface.resize(w, h).ok();
        let mut busy = false;
        {
            let Ok(mut buffer) = g.surface.buffer_mut() else {
                self.gfx = Some(g);
                return;
            };
            let mut cv = Canvas { px: &mut buffer, w: size.width as i32, h: size.height as i32 };
            busy |= self.draw_all(&mut cv, dt);
            let _ = buffer.present();
        }
        self.last_frame = Instant::now();
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.update_title(&g.window, ms);
        self.gfx = Some(g);
        if busy {
            self.request_redraw();
        }
    }

    fn draw_all(&mut self, cv: &mut Canvas, dt: f64) -> bool {
        self.refresh_find();
        let t = &self.theme;
        let c = t.chrome;
        cv.clear(t.background);
        self.regions.clear();
        let mut busy = self.input_frame(dt);

        // Editor card: the document draws inside, the border ring goes on top so the
        // rounded corners stay intact.
        let editor = self.layout.editor;
        let find_ranges: Vec<(usize, usize)> = self.find.as_ref().map(|f| f.matches.clone()).unwrap_or_default();
        let find_cur = self.find.as_ref().and_then(|f| f.current);
        busy |= self.draw_split_other(cv, dt);
        if let Some(d) = self.docs.get_mut(self.active) {
            d.cell_w = self.tx.cell_w;
            busy |= d.draw(
                cv,
                &mut self.tx,
                &self.theme,
                editor,
                &DrawOpts { dt, find: &find_ranges, find_cur, focused: true },
            );
        } else {
            cv.fill(editor, self.theme.background);
            let hint = [
                ("Ctrl+P", "open file"),
                ("Ctrl+Shift+P", "commands"),
                ("Ctrl+Shift+F", "search in project"),
                ("Ctrl+B", "show/hide the sidebar"),
            ];
            let x = editor.x + editor.w / 2 - 150;
            let mut y = editor.y + editor.h / 2 - 56;
            for (k, v) in hint {
                self.tx.draw_ui(cv, x, y, k, c.accent_text, editor, true);
                self.tx.draw_ui(cv, x + 120, y, v, c.tab_inactive_fg, editor, false);
                y += 28;
            }
        }

        busy |= self.draw_md(cv);
        self.draw_topbar(cv);
        self.draw_tabs(cv);
        self.draw_breadcrumbs(cv);
        self.draw_diff(cv);
        if self.sidebar_open {
            self.draw_sidebar(cv);
        }
        self.draw_panel(cv);
        self.draw_gitpanel(cv);
        self.draw_tree_drag(cv);
        self.draw_status(cv);
        if let Some((r, rad)) = self.hover_rect {
            let f = self.theme.foreground;
            cv.fill_round(r, rad, f.with_alpha(if self.theme.background.luminance() < 0.5 { 0x16 } else { 0x12 }));
        }
        self.draw_find(cv);
        self.draw_completion(cv);
        self.draw_hover(cv);
        self.draw_update_card(cv);
        self.draw_overlay(cv);
        self.draw_tooltip(cv);
        self.draw_toast(cv);
        busy
    }

    fn draw_tabs(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let bar = self.layout.tabbar;
        cv.fill(bar, c.tabbar_bg);
        self.layout.tabs.clear();
        let pane_docs = self.split_pane_docs();
        // Geometry of every tab in strip coordinates, then scroll so the active one is visible.
        let order = crate::app_ctxmenu::tab_order(&self.docs);
        let mut geo: Vec<(usize, i32, i32)> = Vec::with_capacity(order.len());
        let mut x = 6;
        for &i in &order {
            let w = self.tx.ui_width(&self.docs[i].name, false)
                + 34
                + 34
                + 11 * pane_docs.iter().filter(|p| p.0 == i).count() as i32;
            geo.push((i, x, w));
            x += w;
        }
        let view = (bar.w - 12).max(1) as f64;
        self.tabs_max = ((x - 6) as f64 - view).max(0.0);
        if self.tabs_seen != Some(self.active) {
            self.tabs_seen = Some(self.active);
            if let Some(&(_, tx0, tw)) = geo.iter().find(|(i, _, _)| *i == self.active) {
                let (l, r) = (tx0 as f64 - 6.0, (tx0 + tw) as f64 + 6.0);
                if l < self.tabs_scroll {
                    self.tabs_scroll = l;
                } else if r > self.tabs_scroll + view + 6.0 {
                    self.tabs_scroll = r - view - 6.0;
                }
            }
        }
        self.tabs_scroll = self.tabs_scroll.clamp(0.0, self.tabs_max);
        let off = self.tabs_scroll.round() as i32;
        let strip = Rect::new(bar.x + 2, bar.y, bar.w - 4, bar.h);
        for (i, tx0, w) in geo {
            let d = &self.docs[i];
            let r = Rect::new(bar.x + tx0 - off, bar.y, w, bar.h);
            if r.right() < strip.x || r.x > strip.right() {
                continue;
            }
            let active = i == self.active;
            let fg = if active { c.tab_active_fg } else { c.tab_inactive_fg };
            let ty = r.y + (bar.h - 18) / 2;
            let lead = 34;
            if active {
                cv.fill_round_clip(Rect::new(r.x + 2, r.y + 5, r.w - 4, bar.h - 10), 8, c.tab_active_bg, strip);
            }
            let fname = self.tx.icons.file_icon(&d.name);
            self.tx.icon(cv, &fname, r.x + 12, r.y + (bar.h - 16) / 2, 16, fg, strip);
            self.tx.draw_ui(cv, r.x + lead, ty, &d.name, fg, strip, active);
            let close = Rect::new(r.right() - 28, r.y, 28, bar.h);
            let (mx, my) = self.mouse;
            let over_tab = strip.contains(mx, my) && r.contains(mx, my) && self.overlay.is_none();
            let over_close = over_tab && close.contains(mx, my);
            if over_tab {
                // Hovering a tab (focused or not) reveals its close button; it gets its own
                // background when the pointer is on it.
                let cb = Rect::new(close.x + 4, r.y + (bar.h - 20) / 2, 20, 20);
                if over_close {
                    cv.fill_round_clip(cb, 6, self.theme.foreground.with_alpha(0x38), strip);
                }
                let ic = if over_close { c.tab_active_fg } else { c.tab_inactive_fg };
                self.tx.icon(cv, "ui/close", close.x + 8, r.y + (bar.h - 12) / 2, 12, ic, strip);
            } else if d.buf.is_dirty() {
                // Like Zed: the unsaved dot takes the close button's place.
                cv.fill_round_clip(Rect::new(close.x + 10, r.y + bar.h / 2 - 4, 8, 8), 4, c.accent, strip);
            } else if d.pinned {
                self.tx.icon(cv, "ui/pin", close.x + 8, r.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, strip);
            } else if active {
                self.tx.icon(cv, "ui/close", close.x + 8, r.y + (bar.h - 12) / 2, 12, c.tab_inactive_fg, strip);
            }
            // Split editor: an underline and the pane number for every pane that shows this tab, in
            // the accent color for the focused pane and gray for the others.
            for (k, &(_, focused)) in pane_docs.iter().enumerate().filter(|(_, p)| p.0 == i) {
                let (col, label) = if focused {
                    (c.accent, c.accent_text)
                } else {
                    (c.tab_inactive_fg.with_alpha(0x80), c.tab_inactive_fg)
                };
                let seg = (r.w - 28) / pane_docs.iter().filter(|p| p.0 == i).count() as i32;
                let nth = pane_docs.iter().take(k).filter(|p| p.0 == i).count() as i32;
                cv.fill_clip(Rect::new(r.x + 14 + nth * seg, bar.bottom() - 5, (seg - 2).max(4), 2), col, strip);
                let name_w = self.tx.ui_width(&d.name, active);
                self.tx.draw_ui(
                    cv,
                    r.x + lead + name_w + 6 + nth * 11,
                    ty + 1,
                    &(k + 1).to_string(),
                    label,
                    strip,
                    focused,
                );
            }
            self.layout.tabs.push((r.intersect(&strip), close.intersect(&strip), i));
        }
        // Soft edge fades hint that more tabs hide beyond the strip.
        let bg = self.theme.chrome.tabbar_bg;
        if self.tabs_scroll > 0.5 {
            for k in 0..14 {
                cv.fill_clip(
                    Rect::new(strip.x + k, bar.y, 1, bar.h),
                    bg.with_alpha((0xd0 - k * 14).max(0) as u8),
                    strip,
                );
            }
        }
        if self.tabs_scroll < self.tabs_max - 0.5 {
            for k in 0..14 {
                cv.fill_clip(
                    Rect::new(strip.right() - 1 - k, bar.y, 1, bar.h),
                    bg.with_alpha((0xd0 - k * 14).max(0) as u8),
                    strip,
                );
            }
        }
    }

    fn draw_sidebar(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        cv.fill(r, c.sidebar_bg);
        cv.fill(Rect::new(r.right() - 1, r.y, 1, r.h), c.border);
        self.layout.sidebar_tabs.clear();
        self.sidebar_bar = None;
        // One quiet title: the structure outline moved to the command palette.
        self.tx.draw_ui(
            cv,
            r.x + 16,
            r.y + (TAB_H - 18) / 2 + 2,
            if self.sidebar_mode == SidebarMode::Outline { "Outline" } else { "Files" },
            c.tab_inactive_fg,
            r,
            true,
        );
        if self.sidebar_mode == SidebarMode::Outline {
            self.draw_outline(cv);
            return self.draw_sidebar_bar(cv);
        }
        let top = r.y + TAB_H;
        let body = Rect::new(r.x, top, r.w - 1, r.h - TAB_H);
        let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
        let off = -((self.tree.scroll as i32) % ROW_H);
        let hx = -(self.tree.hscroll as i32);
        let sidebar_focused = self.focus == Focus::Sidebar;
        let active_path = self.docs.get(self.active).and_then(|d| d.path()).map(Path::to_path_buf);
        let rows = ((body.h / ROW_H) + 2) as usize;
        for (k, row) in self.tree.rows.iter().enumerate().skip(first).take(rows) {
            let y = top + off + ((k - first) as i32) * ROW_H;
            let rr = Rect::new(r.x + 8, y + 1, r.w - 17, ROW_H - 2);
            let is_sel = self.tree.row_selected(k);
            let is_active = active_path.as_deref() == Some(row.path.as_path());
            if is_sel && sidebar_focused {
                cv.fill_clip(Rect::new(rr.x, rr.y, rr.w, rr.h), c.list_active_bg, body);
                let clip = body.intersect(&rr);
                cv.fill_round(clip, 7, c.list_active_bg);
            } else if is_active {
                let clip = body.intersect(&rr);
                cv.fill_round(clip, 7, c.list_hover_bg);
            }
            let indent = 18 + row.depth as i32 * 14;
            // Zed-style: a chevron for folders, a type icon for files (tinted, so it follows the theme).
            let cut = self.file_clip.is_cut(&row.path);
            let icon_color = if is_active { c.tab_active_fg } else { c.tab_inactive_fg };
            let icon_color = if cut { icon_color.with_alpha(0x60).over(c.sidebar_bg) } else { icon_color };
            let iy = y + (ROW_H - 16) / 2;
            if row.is_dir {
                let chev = if row.expanded { "ui/chevron_down" } else { "ui/chevron_right" };
                self.tx.icon(cv, chev, r.x + hx + indent - 4, y + (ROW_H - 14) / 2, 14, icon_color, body);
                let folder = if row.expanded { "file/folder_open" } else { "file/folder" };
                self.tx.icon(cv, folder, r.x + hx + indent + 14, iy, 16, icon_color, body);
            } else {
                let name = self.tx.icons.file_icon(&row.name);
                self.tx.icon(cv, &name, r.x + hx + indent + 14, iy, 16, icon_color, body);
            }
            let badge = self.git_status.get(&row.path).copied();
            let fg = if is_active { c.tab_active_fg } else { c.sidebar_fg };
            let fg = if cut { fg.with_alpha(0x60).over(c.sidebar_bg) } else { fg };
            self.tx.draw_ui(cv, r.x + hx + indent + 38, y + (ROW_H - 18) / 2, &row.name, fg, body, false);
            if let Some(b) = badge {
                // A quiet dot instead of a colored letter: modified / new / deleted.
                let dot = match b {
                    'D' => c.git_deleted,
                    'M' | 'R' => c.git_modified,
                    _ => c.git_added.with_alpha(0xa0),
                };
                cv.fill_round(body.intersect(&Rect::new(r.right() - 24, y + ROW_H / 2 - 3, 6, 6)), 3, dot);
            }
        }
        self.draw_sidebar_bar(cv);
    }

    fn draw_sidebar_bar(&mut self, cv: &mut Canvas) {
        let r = self.layout.sidebar;
        let (total, visible) = self.sidebar_extent();
        let track = Rect::new(r.right() - 15, r.y + TAB_H, 14, r.h - TAB_H);
        self.sidebar_bar = crate::ui::VBar::new(track, total, visible, self.tree.scroll);
        if let Some(b) = self.sidebar_bar {
            b.draw(cv, self.theme.foreground, self.sidebar_bar_hot, self.drag_bar.is_some_and(|(k, _)| k == 1));
        }
        self.draw_sidebar_hbar(cv);
    }

    fn draw_outline(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        let top = r.y + TAB_H;
        let body = Rect::new(r.x, top, r.w - 1, r.h - TAB_H);
        let Some(d) = self.docs.get(self.active) else { return };
        if d.symbols.is_empty() {
            self.tx.draw_ui(
                cv,
                r.x + 18,
                top + 10,
                "No outline (needs the TS/JS server)",
                c.tab_inactive_fg,
                body,
                false,
            );
            return;
        }
        let (cur_line, _) = d.buf.line_col(d.ed.head());
        let chain: Vec<usize> = d.symbol_chain(cur_line).iter().map(|s| s.sel_l).collect();
        let first = (self.tree.scroll / ROW_H as f64).floor().max(0.0) as usize;
        let off = -((self.tree.scroll as i32) % ROW_H);
        let hx = -(self.tree.hscroll as i32);
        let rows = (body.h / ROW_H + 2) as usize;
        let syms: Vec<crate::doc::Sym> = d.symbols.iter().skip(first).take(rows).cloned().collect();
        for (k, s) in syms.iter().enumerate() {
            let y = top + off + k as i32 * ROW_H;
            let rr = Rect::new(r.x + 8, y + 1, r.w - 17, ROW_H - 2);
            if chain.contains(&s.sel_l) {
                cv.fill_round(body.intersect(&rr), 7, c.list_hover_bg);
            }
            let x = r.x + hx + 18 + s.depth as i32 * 14;
            let letter = match s.kind {
                5 => "C",
                6 => "m",
                7 => "p",
                8 => "f",
                9 => "c",
                10 => "E",
                11 => "I",
                12 => "ƒ",
                13 => "v",
                14 => "k",
                2 => "M",
                22 => "e",
                23 => "S",
                26 => "T",
                _ => "·",
            };
            self.tx.draw_ui(cv, x, y + (ROW_H - 18) / 2, letter, c.accent_text, body, true);
            self.tx.draw_ui(cv, x + 18, y + (ROW_H - 18) / 2, &s.name, c.sidebar_fg, body, false);
        }
    }

    fn draw_breadcrumbs(&mut self, cv: &mut Canvas) {
        let c = self.theme.chrome;
        let r = self.layout.bread;
        self.layout.crumbs.clear();
        if r.h == 0 {
            return; // breadcrumbs switched off
        }
        let Some(d) = self.docs.get(self.active) else { return };
        let (dim, fg) = (c.tab_inactive_fg, c.tab_active_fg);
        let rel = d.path().map(|p| self.rel(p)).unwrap_or_else(|| d.name.clone());
        let (line, _) = d.buf.line_col(d.ed.head());
        let mut chain: Vec<(String, usize, usize)> =
            d.symbol_chain(line).iter().map(|s| (s.name.clone(), s.sel_l, s.sel_c)).collect();
        if chain.is_empty() && d.buf.line_count() > crate::bigfile::BIG_LINES {
            // Big file with no (or a trimmed) outline: derive the chain from indentation.
            chain = d.big.chain(&d.buf, line, d.version).into_iter().map(|s| (s.name, s.sel_l, s.sel_c)).collect();
        }
        let parts: Vec<String> = rel.split('/').map(str::to_string).collect();
        let y = r.y + (r.h - 18) / 2;
        let mut x = r.x + 16;
        let sep_w = self.tx.ui_width(">", false) + 12;
        for (i, p) in parts.iter().enumerate() {
            let w = self.tx.ui_width(p, false);
            let last = i + 1 == parts.len();
            self.tx.draw_ui(cv, x, y, p, if last && chain.is_empty() { fg } else { dim }, r, false);
            self.layout
                .crumbs
                .push((Rect::new(x - 3, r.y, w + 6, r.h), Crumb::Path(self.root.join(parts[..=i].join("/")))));
            x += w;
            if !last || !chain.is_empty() {
                self.tx.draw_ui(cv, x + 6, y, ">", dim.with_alpha(0x90), r, false);
                x += sep_w;
            }
            if x > r.right() - 40 {
                return;
            }
        }
        for (i, (name, l, col)) in chain.iter().enumerate() {
            let w = self.tx.ui_width(name, false);
            self.tx.draw_ui(cv, x, y, name, if i + 1 == chain.len() { fg } else { dim }, r, false);
            self.layout.crumbs.push((Rect::new(x - 3, r.y, w + 6, r.h), Crumb::Sym(*l, *col)));
            x += w;
            if i + 1 < chain.len() {
                self.tx.draw_ui(cv, x + 6, y, ">", dim.with_alpha(0x90), r, false);
                x += sep_w;
            }
            if x > r.right() - 40 {
                return;
            }
        }
    }

    fn crumb_click(&mut self, x: f64, y: f64) -> bool {
        let Some((_, cr)) = self.layout.crumbs.iter().find(|(r, _)| r.contains(x, y)).cloned() else {
            return self.layout.bread.contains(x, y);
        };
        match cr {
            Crumb::Path(p) => {
                self.sidebar_open = true;
                self.sidebar_mode = SidebarMode::Files;
                self.tree.reveal(&p);
                self.focus = Focus::Sidebar;
            }
            Crumb::Sym(l, c) => {
                self.push_nav();
                if let Some(d) = self.docs.get_mut(self.active) {
                    let p = d.pos_from_lsp(l, c);
                    d.ed.click(&mut d.buf, p, false);
                    d.reveal = true;
                }
                self.focus = Focus::Editor;
            }
        }
        true
    }

    pub fn memory_text(&mut self) -> String {
        if self.mem_cache.0.elapsed() > Duration::from_secs(2) {
            let own = mem::snapshot().map_or(0, |m| m.pss_kb) as f64 / 1024.0;
            let mut s = format!("RAM {own:.0} MB");
            for l in &self.lsps {
                if let Some(kb) = mem::pss_of(l.pid()) {
                    s.push_str(&format!(" + {} {:.0} MB", l.name, kb as f64 / 1024.0));
                }
            }
            self.mem_cache = (Instant::now(), s);
        }
        self.mem_cache.1.clone()
    }

    fn update_title(&mut self, window: &Window, ms: f32) {
        let title = match self.docs.get(self.active) {
            Some(d) => format!("{}{} — lumen-text — {:.1} ms", if d.buf.is_dirty() { "● " } else { "" }, d.name, ms),
            None => "lumen-text".to_string(),
        };
        if title != self.last_title {
            window.set_title(&title);
            self.last_title = title;
        }
    }

    // ── mouse ──

    fn over_editor(&self) -> bool {
        self.layout.editor.contains(self.mouse.0, self.mouse.1)
    }

    pub fn mouse_down(&mut self, button: MouseButton) {
        let (x, y) = self.mouse;
        if self.hover_mouse_down(button) {
            return self.request_redraw();
        }
        self.hover = None;
        self.dismiss_tooltip();
        self.chord = None;
        if button == MouseButton::Right {
            return self.right_click();
        }
        if self.regions_press(x, y) {
            return self.request_redraw();
        }
        if self.overlay_mouse(x, y) {
            return self.request_redraw();
        }
        if self.update_card_mouse(x, y) {
            return self.request_redraw();
        }
        if self.gitpanel_mouse(x, y) {
            return self.request_redraw();
        }
        if self.topbar_click(x, y) || self.status_click(x, y) {
            return self.request_redraw();
        }
        if self.completion_click(x, y) {
            return self.request_redraw();
        }
        if self.layout.tabbar.contains(x, y) {
            let hit = self.layout.tabs.iter().find(|(r, _, _)| r.contains(x, y)).cloned();
            if let Some((_, close, i)) = hit {
                if button == MouseButton::Middle || close.contains(x, y) {
                    self.tab_close_click(i, button);
                } else {
                    self.activate(i);
                }
            }
            return self.request_redraw();
        }
        if self.crumb_click(x, y) {
            return self.request_redraw();
        }
        if self.md_mouse_down(x, y, button) {
            return self.request_redraw();
        }
        if let Some(b) = self.sidebar_bar
            && self.sidebar_open
            && b.track.contains(x, y)
        {
            let grab = if b.thumb.contains(x, y) { y - b.thumb.y as f64 } else { b.thumb.h as f64 / 2.0 };
            self.drag_bar = Some((1, grab));
            self.sidebar_scroll_to(y, grab);
            return self.request_redraw();
        }
        if self.sidebar_open && self.layout.sidebar.contains(x, y) {
            self.focus = Focus::Sidebar;
            if y < (self.layout.sidebar.y + TAB_H) as f64 {
                if let Some((_, m)) = self.layout.sidebar_tabs.iter().find(|(r, _)| r.contains(x, y)).copied() {
                    self.sidebar_mode = m;
                    self.tree.scroll = 0.0;
                }
                return self.request_redraw();
            }
            let rel = y - (self.layout.sidebar.y + TAB_H) as f64 + self.tree.scroll;
            if self.sidebar_mode == SidebarMode::Outline {
                if rel >= 0.0 {
                    let i = (rel / ROW_H as f64) as usize;
                    if let Some(s) = self.docs.get(self.active).and_then(|d| d.symbols.get(i)).cloned() {
                        self.push_nav();
                        let d = &mut self.docs[self.active];
                        let p = d.pos_from_lsp(s.sel_l, s.sel_c);
                        d.ed.click(&mut d.buf, p, false);
                        d.reveal = true;
                        self.focus = Focus::Editor;
                    }
                }
                return self.request_redraw();
            }
            if rel >= 0.0 {
                let i = (rel / ROW_H as f64) as usize;
                if i < self.tree.rows.len() {
                    self.tree_press(i, x, y);
                }
            }
            return self.request_redraw();
        }
        if self.panel_mouse(x, y) {
            return self.request_redraw();
        }
        if self.split_mouse_down(x, y) {
            return self.request_redraw();
        }
        if self.diff.is_some() && self.diff_area().contains(x, y) {
            return;
        }
        if self.conflict_click(x, y) {
            return self.request_redraw();
        }
        if !self.over_editor() {
            return;
        }
        self.focus = Focus::Editor;
        self.completion = None;
        if self.docs.is_empty() {
            return;
        }
        if button == MouseButton::Middle {
            // Middle click: paste the primary selection at the pointer.
            let text = self.clip.get_primary();
            let d = &mut self.docs[self.active];
            let p = d.pos_at(x, y);
            d.ed.click(&mut d.buf, p, false);
            d.ed.paste(&mut d.buf, &text);
            d.reveal = true;
            return self.request_redraw();
        }
        let ctrl = self.mods.control_key();
        let near = (x - self.last_click_pos.0).abs() < 5.0 && (y - self.last_click_pos.1).abs() < 5.0;
        self.clicks =
            if near && self.last_click.elapsed() < Duration::from_millis(400) { self.clicks % 3 + 1 } else { 1 };
        self.last_click = Instant::now();
        self.last_click_pos = (x, y);
        if self.hbar_press(x, y) {
            return self.request_redraw();
        }
        if let Some(b) = self.docs[self.active].sb
            && b.track.contains(x, y)
        {
            let grab = if b.thumb.contains(x, y) { y - b.thumb.y as f64 } else { b.thumb.h as f64 / 2.0 };
            self.drag_bar = Some((0, grab));
            self.docs[self.active].sb_drag = true;
            self.docs[self.active].scrollbar_drag(y, grab);
            return self.request_redraw();
        }
        if let Some(l) = self.docs[self.active].minimap_layout.as_ref()
            && l.rect.contains(x, y)
        {
            self.dragging_minimap = true;
            self.docs[self.active].minimap_scroll_to(y);
            return self.request_redraw();
        }
        // Code lens text: jump to the symbol and list its references / implementations.
        if let Some(idx) = self.docs[self.active].lens_hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, i)| *i) {
            let lens = self.docs[self.active].lenses[idx].clone();
            let d = &mut self.docs[self.active];
            let p = d.pos_from_lsp(lens.line, lens.col);
            d.ed.click(&mut d.buf, p, false);
            if lens.title.as_deref().is_some_and(|t| t.contains("implementation")) {
                self.goto_definition(crate::lsp::DefKind::Implementation);
            } else {
                self.find_references();
            }
            return self.request_redraw();
        }
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        let clicks = self.clicks;
        let d = &mut self.docs[self.active];
        if clicks == 1 && !alt && d.click_fold(x, y) {
            return self.request_redraw();
        }
        let (p, hint) = d.hit_at(x, y);
        if alt && clicks == 1 {
            self.drag_press_block(p);
            return self.request_redraw();
        }
        d.ed.clear_extra();
        if x < (d.rect.x + d.gutter_w()) as f64 && clicks == 1 {
            d.reveal = true;
            self.drag_press(p, 3, false);
            return self.request_redraw();
        }
        // Inlay hints: Ctrl+click on a label part with a location goes there; a double click on a
        // type hint inserts the annotation (one undo step). A plain click only places the cursor.
        if let Some(hit) = hint {
            if ctrl && clicks == 1 {
                d.ed.click(&mut d.buf, p, false);
                if let Some(l) = d.hint_link(hit) {
                    self.goto_location(&(l.uri, l.line, l.character));
                }
                return self.request_redraw();
            }
            if clicks == 2 && !ctrl && !shift && d.apply_hint_edits(hit) {
                d.reveal = true;
                return self.request_redraw();
            }
        }
        if ctrl && clicks == 1 {
            d.ed.click(&mut d.buf, p, false);
            self.goto_definition(crate::lsp::DefKind::Definition);
            return self.request_redraw();
        }
        d.reveal = true;
        self.drag_press(p, clicks, shift);
        self.request_redraw();
    }

    /// Conventional pointer: arrow on chrome, hand on clickable things, I-beam over text,
    /// resize arrows on the split divider.
    pub(crate) fn cursor_for(&self) -> CursorIcon {
        let (x, y) = self.mouse;
        let l = &self.layout;
        if self.dragging_minimap {
            return CursorIcon::Pointer;
        }
        if self.dragging {
            return CursorIcon::Text;
        }
        // The update / about card floats above everything: arrow on it, hand on its buttons and link.
        if self.upd.card.is_some() && self.upd.card_rect.contains(x, y) {
            return if self.upd.rects.iter().any(|(r, _)| r.contains(x, y)) {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if self.over_region_bar(x, y) {
            return CursorIcon::Default;
        }
        if let Overlay::TopMenu(m) = &self.overlay {
            return if m.rows.iter().any(|(r, _)| r.contains(x, y)) {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if !self.overlay.is_none() {
            return if l.overlay_rows.iter().any(|(r, _)| r.contains(x, y)) {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if let Some(c) = self.hover_cursor(x, y) {
            return c;
        }
        if let Some(c) = self.gitpanel_cursor(x, y) {
            return c;
        }
        if self.drag_bar.is_some()
            || self.over_hbar(x, y)
            || self.sidebar_bar.is_some_and(|b| b.track.contains(x, y))
            || self.docs.get(self.active).and_then(|d| d.sb).is_some_and(|b| b.track.contains(x, y))
        {
            return CursorIcon::Default;
        }
        if let Some(c) = self.split_cursor(x, y) {
            return c;
        }
        if l.divider.w > 0 && Rect::new(l.divider.x - 4, l.divider.y, l.divider.w + 8, l.divider.h).contains(x, y) {
            return CursorIcon::ColResize;
        }
        if l.top_btns.iter().any(|(r, _)| r.contains(x, y))
            || l.top_logo.contains(x, y)
            || l.status_btns.iter().any(|(r, _)| r.contains(x, y))
            || l.tabs.iter().any(|(r, _, _)| r.contains(x, y))
            || l.sidebar_tabs.iter().any(|(r, _)| r.contains(x, y))
            || l.crumbs.iter().any(|(r, _)| r.contains(x, y))
            || l.panel_tabs.iter().any(|(r, _)| r.contains(x, y))
            || self.tests_clickable_at(x, y)
            || l.completion_rows.iter().any(|(r, _)| r.contains(x, y))
        {
            return CursorIcon::Pointer;
        }
        if l.sidebar.contains(x, y) {
            let rel = y - (l.sidebar.y + TAB_H) as f64 + self.tree.scroll;
            let rows = if self.sidebar_mode == SidebarMode::Outline {
                self.docs.get(self.active).map_or(0, |d| d.symbols.len())
            } else {
                self.tree.rows.len()
            };
            return if y >= (l.sidebar.y + TAB_H) as f64 && rel >= 0.0 && ((rel / ROW_H as f64) as usize) < rows {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
        }
        if let Some(d) = self.docs.get(self.active)
            && let Some(m) = d.minimap_layout.as_ref()
            && m.rect.contains(x, y)
        {
            return CursorIcon::Pointer;
        }
        if l.editor.contains(x, y) || self.split_in_other(x, y) {
            if self.mods.control_key()
                && self
                    .docs
                    .get(self.active)
                    .is_some_and(|d| d.hit_at(x, y).1.is_some_and(|h| d.hint_link(h).is_some()))
            {
                return CursorIcon::Pointer; // Ctrl over a hint with a target
            }
            // Gutter (line numbers, fold chevrons, breakpoints) is not text.
            let gutter_right = l.editor.x + 56;
            return if x < gutter_right as f64 && l.editor.contains(x, y) {
                CursorIcon::Default
            } else {
                CursorIcon::Text
            };
        }
        if l.panel.contains(x, y) {
            return CursorIcon::Text;
        }
        CursorIcon::Default
    }

    /// The clickable item under the pointer (rect + corner radius) for the hover tint.
    fn hover_target(&self) -> Option<(Rect, i32)> {
        let (x, y) = self.mouse;
        let l = &self.layout;
        if !self.overlay.is_none() || self.dragging || self.dragging_minimap {
            return None;
        }
        if let Some((r, _)) = l.top_btns.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 8));
        }
        if let Some((r, _)) = l.status_btns.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 6));
        }
        if let Some((r, _, i)) = l.tabs.iter().find(|(r, _, _)| r.contains(x, y)) {
            if *i != self.active {
                return Some((Rect::new(r.x + 2, r.y + 5, r.w - 4, r.h - 10), 8));
            }
            return None;
        }
        if let Some((r, _)) = l.sidebar_tabs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((Rect::new(r.x, r.y + 8, r.w, r.h - 16), 6));
        }
        if let Some((r, _)) = l.crumbs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 5));
        }
        if let Some((r, _)) = l.panel_tabs.iter().find(|(r, _)| r.contains(x, y)) {
            return Some((*r, 6));
        }
        if let Some(h) = self.debug_hover_target() {
            return Some(h);
        }
        if let Some(h) = self.tests_hover_target() {
            return Some(h);
        }
        if self.sidebar_open && l.sidebar.contains(x, y) && y >= (l.sidebar.y + TAB_H) as f64 {
            let rel = y - (l.sidebar.y + TAB_H) as f64 + self.tree.scroll;
            let rows = if self.sidebar_mode == SidebarMode::Outline {
                self.docs.get(self.active).map_or(0, |d| d.symbols.len())
            } else {
                self.tree.rows.len()
            };
            if rel >= 0.0 && ((rel / ROW_H as f64) as usize) < rows {
                let k = (rel / ROW_H as f64) as i32;
                let top = l.sidebar.y + TAB_H - self.tree.scroll as i32;
                return Some((Rect::new(l.sidebar.x + 8, top + k * ROW_H + 1, l.sidebar.w - 17, ROW_H - 2), 7));
            }
        }
        None
    }

    pub fn update_cursor(&mut self) {
        // Pointing at a palette row selects it (like macOS menus).
        let (mx, my) = self.mouse;
        if let Overlay::TopMenu(m) = &mut self.overlay
            && let Some((_, k)) = m.rows.iter().find(|(r, _)| r.contains(mx, my))
            && m.sel != *k
        {
            m.sel = *k;
            self.request_redraw();
        }
        if let Overlay::Picker(p) = &mut self.overlay
            && let Some((_, idx)) = self.layout.overlay_rows.iter().find(|(r, _)| r.contains(mx, my))
            && p.sel != *idx
        {
            p.sel = *idx;
            self.request_redraw();
        }
        let hot_ed = self.docs.get(self.active).and_then(|d| d.sb).is_some_and(|b| b.track.contains(mx, my));
        if self.hbar_hover() {
            self.request_redraw();
        }
        let hot_sb = self.sidebar_bar.is_some_and(|b| b.track.contains(mx, my));
        if hot_ed != self.docs.get(self.active).is_some_and(|d| d.sb_hot) {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.sb_hot = hot_ed;
            }
            self.request_redraw();
        }
        let hot_reg = self.over_region_bar(mx, my);
        if hot_reg != self.region_hot {
            self.region_hot = hot_reg;
            self.request_redraw();
        }
        if hot_sb != self.sidebar_bar_hot {
            self.sidebar_bar_hot = hot_sb;
            self.request_redraw();
        }
        let th = self
            .layout
            .tabs
            .iter()
            .find(|(r, _, _)| r.contains(mx, my))
            .map(|(_, close, i)| (*i, close.contains(mx, my)));
        if th != self.tab_hover {
            self.tab_hover = th;
            self.request_redraw();
        }
        let hv = self.hover_target();
        if hv != self.hover_rect {
            self.hover_rect = hv;
            self.request_redraw();
        }
        self.update_tooltip();
        let want = self.cursor_for();
        if want != self.cursor_icon {
            self.cursor_icon = want;
            if let Some(g) = &self.gfx {
                g.window.set_cursor(Cursor::Icon(want));
            }
        }
    }

    /// (total, visible) of the sidebar list in pixels.
    fn sidebar_extent(&self) -> (f64, f64) {
        let count = if self.sidebar_mode == SidebarMode::Outline {
            self.docs.get(self.active).map_or(0, |d| d.symbols.len())
        } else {
            self.tree.rows.len()
        };
        (count as f64 * ROW_H as f64, (self.layout.sidebar.h - TAB_H) as f64)
    }

    fn sidebar_scroll_to(&mut self, y: f64, grab: f64) {
        if let Some(b) = self.sidebar_bar {
            let (total, visible) = self.sidebar_extent();
            self.tree.scroll = b.pos_for(y, grab, total, visible);
        }
    }

    pub fn mouse_move(&mut self) {
        let (x, y) = self.mouse;
        if self.hover_mouse_move() {
            return self.request_redraw();
        }
        if self.regions_drag() {
            return self.request_redraw();
        }
        if self.tree_drag_move() {
            return self.request_redraw();
        }
        if let Some((kind, grab)) = self.drag_bar {
            match kind {
                0 => {
                    if let Some(d) = self.docs.get_mut(self.active) {
                        d.scrollbar_drag(y, grab);
                    }
                }
                1 => self.sidebar_scroll_to(y, grab),
                _ => self.hbar_drag_move(kind, x, grab),
            }
            return self.request_redraw();
        }
        if self.ctxmenu_active() {
            if self.ctxmenu_hover() {
                self.request_redraw();
            }
            return;
        }
        if self.gitpanel_move() {
            self.request_redraw();
        }
        if self.split_drag(x, y) {
            return self.request_redraw();
        }
        if self.dragging_minimap {
            if let Some(d) = self.docs.get_mut(self.active) {
                d.minimap_scroll_to(y);
            }
            return self.request_redraw();
        }
        if self.dragging {
            self.drag_update();
            return self.request_redraw();
        }
        if self.over_editor() && self.overlay.is_none() && !self.docs.is_empty() {
            let p = self.docs[self.active].pos_at(x, y);
            self.hover_pointer(Some(p));
        } else {
            self.hover_pointer(None);
        }
    }

    fn sidebar_activate(&mut self) {
        let Some(row) = self.tree.rows.get(self.tree.sel).cloned() else { return };
        if row.is_dir {
            self.tree.toggle(self.tree.sel);
        } else {
            self.open_path(&row.path, None);
            self.focus = Focus::Editor;
        }
    }

    // ── keyboard ──

    pub fn on_key(&mut self, ev: &KeyEvent) -> bool {
        let ki = KeyIn { key: ev.logical_key.clone(), text: ev.text.as_ref().map(|t| t.to_string()) };
        self.handle_key(&ki)
    }

    pub fn handle_key(&mut self, ev: &KeyIn) -> bool {
        self.compute_layout();
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        if self.hover_key(ev) {
            return false;
        }
        self.hover = None;
        self.dismiss_tooltip();
        let key = ev.key.clone();

        if !self.overlay.is_none() {
            return self.overlay_key(ev);
        }
        if self.update_card_key(&key) {
            self.request_redraw();
            return false;
        }
        if self.gitpanel_key(ev) {
            self.request_redraw();
            return false;
        }
        if self.find.as_ref().is_some_and(|f| f.focused) && self.find_key(ev) {
            self.request_redraw();
            return false;
        }

        if self.focus == Focus::Terminal && self.term_key(ev) {
            self.request_redraw();
            return false;
        }

        // User key bindings win over the built-in shortcuts.
        if self.diff.is_some() && self.diff_pane_focused() && self.diff_key(&key) {
            self.request_redraw();
            return false;
        }
        // Second stroke of a chord (`Ctrl+K` then a letter): modifier presses alone keep it pending.
        if let Some(prefix) = self.chord.clone() {
            if matches!(
                &key,
                Key::Named(
                    NamedKey::Shift
                        | NamedKey::Control
                        | NamedKey::Alt
                        | NamedKey::AltGraph
                        | NamedKey::Super
                        | NamedKey::Meta
                        | NamedKey::CapsLock
                )
            ) {
                return false;
            }
            self.chord = None;
            if let Some(id) = self.keymap.lookup_chord(&prefix, ctrl, shift, alt, &key).map(str::to_string) {
                let quit = self.run_bound(&id);
                self.request_redraw();
                return quit;
            }
            self.request_redraw();
            return false; // an unbound second stroke only cancels the chord
        }
        // The explorer's own keys (A, Shift+A, F2, Delete) win over the global table while it has the focus.
        if self.focus == Focus::Sidebar
            && let Some(id) = self.keymap.lookup_explorer(ctrl, shift, alt, &key).map(str::to_string)
        {
            let quit = self.run_bound(&id);
            self.request_redraw();
            return quit;
        }
        // One table: keybindings.json first ("none" disables), then keymap::DEFAULTS.
        if let Some(id) = self.keymap.lookup(ctrl, shift, alt, &key).map(str::to_string) {
            let cmd = crate::app_overlay::Cmd::from_id(&id);
            // Ctrl+Shift+V is also "paste" outside Markdown files: let it through.
            let paste = cmd == Some(crate::app_overlay::Cmd::MarkdownPreview)
                && !self.active_is_markdown()
                && ctrl
                && shift
                && !alt;
            if !paste {
                let quit = self.run_bound(&id);
                self.request_redraw();
                return quit;
            }
        }
        // First stroke of a chord.
        if let Some(c) = self.keymap.chord_start(ctrl, shift, alt, &key) {
            self.chord = Some(c);
            self.request_redraw();
            return false;
        }

        if (ctrl || alt)
            && let Some(r) = self.shortcut(&key, ctrl, alt)
        {
            self.request_redraw();
            return r;
        }
        if let Key::Named(n) = &key
            && let Some(r) = self.named_shortcut(*n, ctrl, shift, alt)
        {
            self.request_redraw();
            return r;
        }

        if self.completion_key(&key) {
            self.request_redraw();
            return false;
        }

        if self.focus == Focus::Sidebar && self.sidebar_key(&key) {
            self.request_redraw();
            return false;
        }

        let page = self.docs.get(self.active).map_or(20.0, |d| (d.visible_lines() - 2.0).max(1.0));
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        let mut typed = None;
        d.reveal = true;
        let multi = !d.ed.extra.is_empty();

        if ctrl && let Key::Character(c) = &key {
            let k = c.to_lowercase();
            // Commands that understand several cursors; anything else collapses to one.
            let multi_aware =
                matches!((k.as_str(), shift), ("d", false) | ("l", true) | ("c", _) | ("x", _) | ("v", _));
            if multi && !multi_aware {
                d.ed.clear_extra();
            }
            match (k.as_str(), shift) {
                ("a", _) => d.ed.select_all(&mut d.buf),
                ("d", false) => d.ed.add_next_occurrence(&mut d.buf),
                ("l", true) => d.ed.select_all_occurrences(&mut d.buf),
                ("c", _) => {
                    let t = d.copy_cmd();
                    self.clip.set(&t);
                }
                ("x", _) => {
                    if multi {
                        let t = d.ed.copy_text_multi(&d.buf);
                        self.clip.set(&t);
                        d.ed.edit_multi(&mut d.buf, EditKind::Other, |e, b| e.backspace(b));
                    } else {
                        let t = d.cut_cmd();
                        self.clip.set(&t);
                    }
                }
                ("v", _) => {
                    let t = self.clip.get();
                    let d = &mut self.docs[self.active];
                    d.ed.paste_multi(&mut d.buf, &t);
                }
                ("d", true) => d.duplicate_cmd(),
                ("k", true) => d.delete_line_cmd(),
                ("]", _) => d.ed.tab(&mut d.buf, false),
                ("[", _) => d.ed.tab(&mut d.buf, true),
                _ => {}
            }
            return self.after_key(None);
        }

        match &key {
            Key::Named(n) => match n {
                NamedKey::ArrowUp if ctrl && alt => d.ed.add_cursor_vertical(&mut d.buf, -1),
                NamedKey::ArrowDown if ctrl && alt => d.ed.add_cursor_vertical(&mut d.buf, 1),
                NamedKey::ArrowLeft => d.ed.move_multi(&mut d.buf, |e, b| e.left(b, shift, ctrl)),
                NamedKey::ArrowRight => d.ed.move_multi(&mut d.buf, |e, b| e.right(b, shift, ctrl)),
                NamedKey::ArrowUp if alt => {
                    d.ed.clear_extra();
                    d.move_lines_cmd(false)
                }
                NamedKey::ArrowDown if alt => {
                    d.ed.clear_extra();
                    d.move_lines_cmd(true)
                }
                NamedKey::ArrowUp if ctrl => {
                    d.reveal = false;
                    d.scroll_by(-1.0)
                }
                NamedKey::ArrowDown if ctrl => {
                    d.reveal = false;
                    d.scroll_by(1.0)
                }
                NamedKey::ArrowUp => d.move_vertical(shift, -1),
                NamedKey::ArrowDown => d.move_vertical(shift, 1),
                NamedKey::PageUp => {
                    d.ed.clear_extra();
                    d.move_vertical(shift, -(page as isize));
                    d.glide_by(-page);
                }
                NamedKey::PageDown => {
                    d.ed.clear_extra();
                    d.move_vertical(shift, page as isize);
                    d.glide_by(page);
                }
                NamedKey::Home if ctrl => {
                    d.ed.clear_extra();
                    d.scr.glide_next = true;
                    d.ed.doc_start(&mut d.buf, shift)
                }
                NamedKey::End if ctrl => {
                    d.ed.clear_extra();
                    d.scr.glide_next = true;
                    d.ed.doc_end(&mut d.buf, shift)
                }
                // With word wrap the first press goes to the start / end of the visual row (every cursor).
                NamedKey::Home => d.home_cmd(shift),
                NamedKey::End => d.end_cmd(shift),
                NamedKey::Backspace => d.ed.edit_multi(&mut d.buf, EditKind::Backspace, |e, b| {
                    if ctrl { e.delete_word(b, false) } else { e.backspace(b) }
                }),
                NamedKey::Delete => d.ed.edit_multi(&mut d.buf, EditKind::Delete, |e, b| {
                    if ctrl { e.delete_word(b, true) } else { e.delete(b) }
                }),
                NamedKey::Enter => d.ed.edit_multi(&mut d.buf, EditKind::Other, |e, b| e.newline(b)),
                NamedKey::Tab if !shift && d.ed.extra.is_empty() && d.snippet_next() => {}
                NamedKey::Tab
                    if !shift && d.ed.extra.is_empty() && d.ed.sel.is_empty() && self.completion.is_none() && {
                        // Snippet prefix right before the cursor?
                        let head = d.ed.head();
                        let mut st = head;
                        while st > 0 && d.buf.char_at(st - 1).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            st -= 1;
                        }
                        let word = d.buf.slice(st, head);
                        match crate::snippets::find(d.lang, &word) {
                            Some(sn) => {
                                d.insert_snippet(st, head, sn.body);
                                true
                            }
                            None => false,
                        }
                    } => {}
                NamedKey::Tab
                    if !shift && d.ed.extra.is_empty() && d.ed.sel.is_empty() && self.completion.is_none() && {
                        // Emmet abbreviation right before the cursor (HTML/Vue/Svelte/JSX/TSX)?
                        let flavor = match d.lang {
                            Some("html" | "vue" | "svelte") => Some((crate::emmet::Flavor::Html, false)),
                            Some("tsx" | "jsx") => Some((crate::emmet::Flavor::Jsx, true)),
                            _ => None,
                        };
                        match flavor {
                            Some((fl, jsx_code)) => {
                                let head = d.ed.head();
                                let (l, col) = d.buf.line_col(head);
                                let prefix: String = d.buf.line(l).chars().take(col).collect();
                                match crate::emmet::extract_abbr(&prefix) {
                                    Some((start_col, abbr))
                                        if crate::snippets::emmet_allowed(
                                            &abbr,
                                            &prefix.chars().take(start_col).collect::<String>(),
                                            jsx_code,
                                        ) =>
                                    {
                                        match crate::emmet::expand(&abbr, fl) {
                                            Some(body) => {
                                                let start = d.buf.line_start(l) + start_col;
                                                d.insert_snippet(start, head, &body);
                                                true
                                            }
                                            None => false,
                                        }
                                    }
                                    _ => false,
                                }
                            }
                            None => false,
                        }
                    } => {}
                NamedKey::Tab => d.ed.edit_multi(&mut d.buf, EditKind::Other, |e, b| e.tab(b, shift)),
                NamedKey::Escape => {
                    d.snippet = None;
                    if d.ed.extra.is_empty() {
                        let h = d.ed.head();
                        d.ed.click(&mut d.buf, h, false);
                        self.find = None;
                        // Nothing else to dismiss: Esc also forgets a pending file cut (the
                        // dimmed explorer rows), whichever pane has the focus.
                        self.cancel_cut();
                    } else {
                        d.ed.clear_extra();
                    }
                }
                _ => typed = self.type_text(ev),
            },
            _ => typed = self.type_text(ev),
        }
        self.after_key(typed)
    }

    fn type_text(&mut self, ev: &KeyIn) -> Option<char> {
        let alt = self.mods.alt_key();
        let d = self.docs.get_mut(self.active)?;
        if alt || self.mods.control_key() || self.mods.super_key() {
            d.reveal = false;
            return None;
        }
        let Some(t) = &ev.text else {
            d.reveal = false;
            return None;
        };
        let mut last = None;
        for ch in t.chars().filter(|c| !c.is_control()) {
            d.ed.edit_multi(&mut d.buf, EditKind::Typing, |e, b| e.type_char(b, ch));
            last = Some(ch);
        }
        if last.is_none() {
            d.reveal = false;
        }
        last
    }

    pub fn after_key(&mut self, typed: Option<char>) -> bool {
        self.quit_armed = false;
        self.close_armed = None;
        if let Some(d) = self.docs.get_mut(self.active) {
            d.sync_edits();
        }
        self.on_typed(typed);
        self.request_redraw();
        false
    }

    /// Named keys that are not `Cmd`s (the rest goes through `keymap::DEFAULTS` in `handle_key`).
    fn named_shortcut(&mut self, n: NamedKey, ctrl: bool, shift: bool, _alt: bool) -> Option<bool> {
        match n {
            NamedKey::Space if ctrl && shift => {
                self.request_signature();
                Some(false)
            }
            NamedKey::Space if ctrl => {
                self.request_completion(None);
                Some(false)
            }
            _ => None,
        }
    }

    /// Hard-wired shortcuts that are not `Cmd`s (everything else is dispatched through
    /// `keymap::DEFAULTS` in `handle_key`): Alt+digit tab jumps.
    fn shortcut(&mut self, key: &Key, ctrl: bool, alt: bool) -> Option<bool> {
        let Key::Character(c) = key else { return None };
        let c = c.to_lowercase();
        if alt && !ctrl {
            let n = c.chars().next().and_then(|ch| ch.to_digit(10)).filter(|&n| n >= 1)?;
            self.activate(crate::app_ctxmenu::tab_order(&self.docs).get(n as usize - 1).copied().unwrap_or(usize::MAX));
            return Some(false);
        }
        None
    }

    fn sidebar_key(&mut self, key: &Key) -> bool {
        match key {
            Key::Named(NamedKey::ArrowDown) => self.tree.step(1, self.mods.shift_key()),
            Key::Named(NamedKey::ArrowUp) => self.tree.step(-1, self.mods.shift_key()),
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Enter) => self.sidebar_activate(),
            Key::Character(c)
                if self.mods.control_key() && !self.mods.alt_key() && self.explorer_ctrl_key(c.as_str()) => {}
            Key::Named(NamedKey::ArrowLeft) => self.tree.collapse_or_parent(),
            Key::Named(NamedKey::Escape) => {
                // First Esc forgets a pending cut (the dimmed rows); the next one leaves the explorer.
                if !self.explorer_escape() {
                    self.focus = Focus::Editor;
                }
            }
            _ => return false,
        }
        // Keep the selection in view.
        let sel_y = self.tree.sel as f64 * ROW_H as f64;
        let view = (self.layout.sidebar.h - TAB_H).max(ROW_H) as f64;
        if sel_y < self.tree.scroll {
            self.tree.scroll = sel_y;
        } else if sel_y + ROW_H as f64 > self.tree.scroll + view {
            self.tree.scroll = sel_y + ROW_H as f64 - view;
        }
        true
    }

    /// Change the font size by `delta` px (0 resets), keeping the cursor in view.
    pub fn zoom(&mut self, delta: f32) {
        let size = if delta == 0.0 { self.tx.reset_zoom() } else { self.tx.set_zoom(crate::ui::font_size() + delta) };
        self.config.font_size = size;
        self.config.save();
        for d in &mut self.docs {
            d.cell_w = self.tx.cell_w;
            d.reveal = true;
        }
        self.say(format!("zoom: {size:.0} px"));
    }

    pub fn try_quit(&mut self) -> bool {
        let dirty: Vec<&str> = self.docs.iter().filter(|d| d.buf.is_dirty()).map(|d| d.name.as_str()).collect();
        if !dirty.is_empty() && !self.quit_armed {
            self.quit_armed = true;
            let list = dirty.join(", ");
            self.say(format!("Unsaved: {list} — Ctrl+S saves, Ctrl+Q again discards"));
            self.request_redraw();
            return false;
        }
        mem::report("exit");
        self.save_session();
        self.stop_task();
        for l in &mut self.lsps {
            l.shutdown();
        }
        true
    }
}

/// The brand mark (256 px, embedded) as the window icon; decoded once at start.
/// macOS: Cmd drives the editor's Ctrl shortcuts (Cmd+C/V/X/Z/S/P/F...). Elsewhere modifiers pass through.
fn map_mods(s: ModifiersState) -> ModifiersState {
    if cfg!(target_os = "macos") && s.super_key() { (s & !ModifiersState::SUPER) | ModifiersState::CONTROL } else { s }
}

fn window_icon() -> Option<winit::window::Icon> {
    static PNG: &[u8] = include_bytes!("../packaging/icons/png/lumen-text-256.png");
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
