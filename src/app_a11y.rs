//! Screen reader glue: the AccessKit adapter of the window and the `App` side of [`crate::a11y`].
//!
//! The adapter is created with the window (it must exist before the window is shown) unless the
//! `accessibility` setting is "off". It costs nothing until an assistive technology asks for the
//! tree: the platform tells us through `request_initial_tree` (any thread), which only raises a
//! flag and wakes the event loop. While the flag is down [`App::a11y_sync`] returns at once, so
//! no snapshot and no tree is ever built; while it is up, a snapshot is made after each batch of
//! events and the tree is rebuilt only when the snapshot differs from the last one sent.

use crate::{
    a11y::{self, Area, Snapshot},
    app::{App, Focus, SidebarMode},
    app_overlay::Overlay,
    app_panel::PanelTab,
    app_topbar::{MenuKind, StatusBtn},
    doc::{Diag, Doc},
    settingsui::{Page, SettingKind},
};
use accesskit::{ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

/// What the platform threads tell the app.
#[derive(Default)]
pub struct Shared {
    /// An assistive technology is connected and wants a tree.
    on: AtomicBool,
    /// Bumped on every (re)connection: the next tree must be sent whole and announce nothing.
    epoch: AtomicU64,
}

struct Activate {
    shared: Arc<Shared>,
    wake: crate::lsp::Wake,
}

impl ActivationHandler for Activate {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.shared.epoch.fetch_add(1, Ordering::AcqRel);
        self.shared.on.store(true, Ordering::Release);
        // The tree is built on the main thread, on its next turn.
        (self.wake)();
        None
    }
}

/// Actions asked by the assistive technology (focus, click, scroll) are not handled yet: the
/// tree is for reading, and the keyboard drives the editor.
struct IgnoreActions;

impl ActionHandler for IgnoreActions {
    fn do_action(&mut self, _request: ActionRequest) {}
}

struct Deactivate {
    shared: Arc<Shared>,
}

impl DeactivationHandler for Deactivate {
    fn deactivate_accessibility(&mut self) {
        self.shared.on.store(false, Ordering::Release);
    }
}

#[derive(Default)]
pub struct A11y {
    adapter: Option<accesskit_winit::Adapter>,
    shared: Arc<Shared>,
    /// The snapshot the platform has, and the epoch it was made in.
    last: Option<Snapshot>,
    seen: u64,
    /// Text of the announcer live region.
    said: String,
}

impl A11y {
    /// Connect the window to the platform's accessibility API. Call before the window is shown.
    pub fn attach(&mut self, el: &ActiveEventLoop, window: &Window, wake: crate::lsp::Wake) {
        self.adapter = Some(accesskit_winit::Adapter::with_direct_handlers(
            el,
            window,
            Activate { shared: self.shared.clone(), wake },
            IgnoreActions,
            Deactivate { shared: self.shared.clone() },
        ));
    }

    /// Window events the adapter needs (focus, size, position).
    pub fn event(&mut self, window: &Window, ev: &WindowEvent) {
        if let Some(a) = self.adapter.as_mut() {
            a.process_event(window, ev);
        }
    }

    /// Is the tree wanted right now? `mode` is the `accessibility` setting.
    pub fn wanted(&self, mode: &str) -> bool {
        self.adapter.is_some() && mode != "off" && (mode == "on" || self.shared.on.load(Ordering::Acquire))
    }

    pub fn attached(&self) -> bool {
        self.adapter.is_some()
    }
}

/// Longest part of a line put in a text run.
fn clip(mut line: String) -> String {
    if line.len() > a11y::MAX_LINE_CHARS {
        line = line.chars().take(a11y::MAX_LINE_CHARS).collect();
    }
    line
}

/// The diagnostic that applies at (`line`, `col` in chars): one covering the caret, else the most
/// severe one on the line.
fn diag_at(d: &Doc, line: usize, col: usize) -> Option<String> {
    let u = crate::ide::utf16_col(&d.buf.line(line), col);
    let on_line: Vec<&Diag> = d.diags.iter().filter(|g| g.sl <= line && line <= g.el).collect();
    let covers = |g: &Diag| (line, u) >= (g.sl, g.sc) && (line, u) <= (g.el, g.ec);
    let best = on_line
        .iter()
        .copied()
        .filter(|g| covers(g))
        .min_by_key(|g| g.severity)
        .or_else(|| on_line.iter().copied().min_by_key(|g| g.severity))?;
    Some(a11y::diag_words(best.severity, &best.message))
}

fn top_menu_title(kind: MenuKind) -> String {
    match kind {
        MenuKind::Project => "Switch project".to_string(),
        MenuKind::Branch => "Switch branch".to_string(),
        MenuKind::Browse => "Open folder".to_string(),
        MenuKind::Menu(i) => {
            format!("{} menu", crate::app_menubar::MENUS.get(i as usize).map_or("Menu", |m| m.0))
        }
        MenuKind::Act(_) => "Run target".to_string(),
    }
}

/// The settings window: its pages (sections) and the rows of the current page, a window around
/// the selected row.
fn settings_view(m: &crate::settingsui::Modal) -> a11y::DialogView {
    if m.page == Page::Keybinds {
        return a11y::DialogView {
            title: "Keyboard shortcuts".into(),
            about: Some("Settings page".into()),
            ..Default::default()
        };
    }
    let page = &m.settings;
    let searching = !page.search.text.trim().is_empty();
    let sections = page
        .sections()
        .iter()
        .enumerate()
        .map(|(i, name)| a11y::Chip::tab(*name, "", !searching && i == page.section))
        .collect();
    let rows = page.visible();
    let sel = page.sel.min(rows.len().saturating_sub(1));
    let (a, b) = a11y::list_window(rows.len(), sel, a11y::MAX_ITEMS);
    let controls = rows[a..b]
        .iter()
        .enumerate()
        .map(|(k, &def)| {
            let d = &page.defs[def];
            let v = page.current(def);
            let (kind, value) = match &d.kind {
                SettingKind::Bool => {
                    let on = v.as_bool().unwrap_or(false);
                    (a11y::ControlKind::Switch(on), if on { "on" } else { "off" }.to_string())
                }
                SettingKind::Choice(_) => (a11y::ControlKind::Choice, v.as_str().unwrap_or_default().to_string()),
                SettingKind::Number { min, max, .. } => {
                    let n = v.as_f64().unwrap_or(*min);
                    (a11y::ControlKind::Number { min: *min, max: *max, value: n }, v.to_string())
                }
                SettingKind::Text => (a11y::ControlKind::Text, v.as_str().unwrap_or_default().to_string()),
            };
            a11y::Control {
                label: d.label.to_string(),
                help: d.help.to_string(),
                value,
                kind,
                modified: page.is_modified(def),
                selected: a + k == sel,
            }
        })
        .collect();
    a11y::DialogView {
        title: "Settings".into(),
        about: searching.then(|| format!("{} results for \"{}\"", rows.len(), page.search.text.trim())),
        sections,
        controls,
        ..Default::default()
    }
}

fn panel_tab_name(t: PanelTab) -> &'static str {
    match t {
        PanelTab::Output => "Output",
        PanelTab::Problems => "Problems",
        PanelTab::Terminal => "Terminal",
        PanelTab::Debug => "Debug",
        PanelTab::Tests => "Tests",
        PanelTab::Build => "Build",
        PanelTab::Docker => "Docker",
    }
}

/// Reading order of a row of controls: top to bottom, left to right.
fn by_x<T>(mut v: Vec<(crate::ui::Rect, T)>) -> Vec<T> {
    v.sort_by_key(|(r, _)| (r.y, r.x));
    v.into_iter().map(|(_, k)| k).collect()
}

impl App {
    /// The settings changed (or the app starts): tell the user when a change needs a restart.
    pub(crate) fn a11y_setting_changed(&mut self) {
        let mode = self.config.accessibility.as_str();
        if mode != "off" && !self.a11y.attached() && self.gfx.is_some() {
            self.say("accessibility: restart Lumen to connect the screen reader");
        }
        self.a11y.last = None;
    }

    /// Send the screen reader what changed on screen. Does nothing (and costs nothing) while no
    /// assistive technology is connected.
    pub fn a11y_sync(&mut self) {
        if !self.a11y.wanted(&self.config.accessibility) {
            return;
        }
        let epoch = self.a11y.shared.epoch.load(Ordering::Acquire);
        let fresh = epoch != self.a11y.seen;
        let snap = self.a11y_snapshot();
        if !fresh && self.a11y.last.as_ref() == Some(&snap) {
            return;
        }
        let prev = if fresh { None } else { self.a11y.last.as_ref() };
        let said = a11y::announcement(prev, &snap);
        if !said.is_empty() {
            self.a11y.said = said;
        }
        let text = self.a11y.said.clone();
        if let Some(ad) = self.a11y.adapter.as_mut() {
            ad.update_if_active(|| a11y::build(&snap, &text));
        }
        self.a11y.seen = epoch;
        self.a11y.last = Some(snap);
    }

    /// The words of a status bar chip: its name and what it shows now. Mirrors the tooltips; the
    /// memory read-out keeps to its name (its tooltip refreshes the numbers).
    fn status_words(&self, k: StatusBtn) -> (String, String) {
        let (e, w) = self.diag_counts();
        let doc = self.docs.get(self.active);
        let tip = |t: crate::app_tooltip::TipText| (t.title, t.detail.unwrap_or_default());
        match k {
            StatusBtn::Problems => {
                let plural = |n: usize, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
                let detail = if e + w == 0 {
                    "No problems".to_string()
                } else {
                    format!("{}, {}", plural(e, "error"), plural(w, "warning"))
                };
                ("Problems".to_string(), detail)
            }
            StatusBtn::Ram => ("Memory in use".to_string(), String::new()),
            StatusBtn::Position => {
                let at = doc
                    .map(|d| d.buf.line_col(d.ed.sel.head))
                    .map(|(l, c)| format!("Line {}, column {}", l + 1, c + 1));
                ("Go to line".to_string(), at.unwrap_or_default())
            }
            StatusBtn::Indent => {
                let now = doc.map(|d| {
                    if d.ed.indent.contains('\t') { "Tab".to_string() } else { format!("{} spaces", d.ed.indent.len()) }
                });
                ("Indentation".to_string(), now.unwrap_or_default())
            }
            StatusBtn::Language => tip(self.language_tip()),
            StatusBtn::Encoding => {
                ("Encoding".to_string(), doc.map(|d| d.buf.encoding.label().to_string()).unwrap_or_default())
            }
            StatusBtn::Update => {
                let (t, d) = self.update_indicator_tip();
                (t, d.unwrap_or_default())
            }
            StatusBtn::Build => ("Build".to_string(), self.build_status_tip()),
            StatusBtn::Ai => ("AI agent".to_string(), self.ai.badge().map(|(b, _)| b).unwrap_or_default()),
            StatusBtn::Inline => ("Inline completion".to_string(), self.inline_status_tip()),
            StatusBtn::Project => tip(self.projcfg_tip()),
            StatusBtn::Pr => ("Pull request".to_string(), self.pr_tip().unwrap_or_default()),
            StatusBtn::Branch | StatusBtn::Sync | StatusBtn::Lsp | StatusBtn::Eol | StatusBtn::Selection => {
                tip(self.statusbar_tip(k))
            }
        }
    }

    /// Menu bar, activity rail, dock and panel tabs, status bar chips and toasts of this frame, from
    /// the hit lists the last draw left in the layout.
    fn a11y_chrome(&self, s: &mut Snapshot) {
        let l = &self.layout;
        let menus: Vec<(crate::ui::Rect, u8)> = l
            .top_btns
            .iter()
            .filter_map(|(r, k)| if let MenuKind::Menu(i) = k { Some((*r, *i)) } else { None })
            .collect();
        s.menu_bar = by_x(menus)
            .into_iter()
            .filter_map(|i| crate::app_menubar::MENUS.get(i as usize))
            .map(|m| {
                let mut c = a11y::Chip::button(m.0, "");
                c.popup = true;
                c
            })
            .collect();
        s.rail = by_x(l.rail_btns.iter().map(|(r, k)| (*r, *k)).collect())
            .into_iter()
            .map(|b| a11y::Chip::button(b.title(), b.command().and_then(|c| self.shortcut_text(c)).unwrap_or_default()))
            .collect();
        if self.gitpanel.is_some() || self.ai.open {
            s.docks =
                vec![a11y::Chip::tab("Git", "", self.gitpanel.is_some()), a11y::Chip::tab("AI", "", self.ai.open)];
        }
        if self.panel.open {
            s.panel_tabs = by_x(l.panel_tabs.iter().map(|(r, t)| (*r, *t)).collect())
                .into_iter()
                .map(|t| {
                    let detail = match (t, self.diag_counts()) {
                        (PanelTab::Problems, (e, w)) if e + w > 0 => format!("{e} errors, {w} warnings"),
                        _ => String::new(),
                    };
                    a11y::Chip::tab(panel_tab_name(t), detail, t == self.panel.tab)
                })
                .collect();
        }
        let chips: Vec<(crate::ui::Rect, StatusBtn)> = l
            .status_btns
            .iter()
            .filter(|(r, _)| r.y >= crate::app::TOP_H)
            .chain(l.status_info.iter())
            .copied()
            .collect();
        s.status = by_x(chips)
            .into_iter()
            .map(|k| {
                let (label, detail) = self.status_words(k);
                a11y::Chip::button(label, detail)
            })
            .collect();
        s.toasts = self.toasts.spoken().into_iter().map(|(text, urgent)| a11y::ToastView { text, urgent }).collect();
    }

    /// What is on screen, as plain data (see [`a11y::Snapshot`]); only the parts a screen reader
    /// needs: the viewport of the editor, a window of the explorer and of the open list, the
    /// terminal's rows.
    pub fn a11y_snapshot(&self) -> Snapshot {
        let root =
            self.root.file_name().map_or_else(|| self.root.display().to_string(), |n| n.to_string_lossy().into_owned());
        let mut s = Snapshot { title: format!("lumen-text, {root}"), ..Default::default() };
        s.recording = self.macros.recording.is_some();
        s.toast = self
            .status
            .as_ref()
            .filter(|(_, t)| t.elapsed() < std::time::Duration::from_secs(6))
            .map(|(m, _)| m.clone());
        s.tabs = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| a11y::TabView { name: d.name.clone(), dirty: d.buf.is_dirty(), active: i == self.active })
            .collect();

        if let Some(d) = self.docs.get(self.active) {
            let (cl, cc) = d.buf.line_col(d.ed.sel.head);
            let (al, ac) = d.buf.line_col(d.ed.sel.anchor);
            let total = d.buf.line_count();
            let (first_line, _) = d.top_pos();
            let (a, b) = a11y::editor_window(total, first_line, d.visible_lines() as usize, cl, al);
            s.editor = Some(a11y::EditorView {
                name: d.name.clone(),
                read_only: d.buf.read_only,
                line_count: total,
                first: a,
                lines: (a..b).map(|l| clip(d.buf.line(l))).collect(),
                caret: (cl, cc),
                anchor: (al, ac),
                diag: diag_at(d, cl, cc),
            });
        }

        let rows = &self.tree.rows;
        if self.sidebar_open && self.sidebar_mode == SidebarMode::Files && !rows.is_empty() {
            let sel = self.tree.sel.min(rows.len() - 1);
            let (a, b) = a11y::list_window(rows.len(), sel, a11y::MAX_FILES);
            s.files = rows[a..b]
                .iter()
                .enumerate()
                .map(|(k, r)| a11y::FileRow {
                    name: r.name.clone(),
                    depth: r.depth,
                    is_dir: r.is_dir,
                    expanded: r.expanded,
                    selected: self.tree.row_selected(a + k),
                })
                .collect();
            s.files_sel = Some(sel - a);
        }

        if let Some(f) = &self.find {
            s.find = Some(a11y::FindView {
                query: f.field.text.clone(),
                replace: f.show_replace.then(|| f.replace.text.clone()),
                matches: f.matches.len(),
                current: f.current,
            });
        }

        self.a11y_chrome(&mut s);

        s.area = match self.focus {
            Focus::Editor => Area::Editor,
            Focus::Sidebar => Area::Files,
            Focus::Terminal => Area::Terminal,
        };
        if self.find.as_ref().is_some_and(|f| f.focused) && s.area == Area::Editor {
            s.area = Area::Find;
        }
        match &self.overlay {
            Overlay::None => {}
            Overlay::Picker(p) => {
                let n = p.filtered.len();
                let sel = p.sel.min(n.saturating_sub(1));
                let (a, b) = a11y::list_window(n, sel, a11y::MAX_ITEMS);
                let items = p.filtered[a..b]
                    .iter()
                    .filter_map(|(i, _)| p.items.get(*i))
                    .map(|it| a11y::DialogItem { label: it.label.clone(), detail: it.detail.clone() })
                    .collect();
                s.dialog = Some(a11y::DialogView {
                    title: if p.title.is_empty() { "Search".to_string() } else { p.title.clone() },
                    input: Some(p.field.text.clone()),
                    items,
                    sel: (n > 0).then(|| sel - a),
                });
                s.area = Area::Palette;
            }
            Overlay::Settings(m) => {
                s.dialog = Some(settings_view(m));
                s.area = Area::Dialog;
            }
            Overlay::Ctx(m) => {
                let items: Vec<a11y::DialogItem> = m
                    .entries
                    .iter()
                    .filter_map(|e| match e {
                        crate::app_ctxmenu::Entry::Item { label, key, .. } => {
                            Some(a11y::DialogItem { label: (*label).to_string(), detail: (*key).to_string() })
                        }
                        crate::app_ctxmenu::Entry::Sep => None,
                    })
                    .collect();
                let sel = m.sel.and_then(|i| match m.entries.get(i) {
                    Some(crate::app_ctxmenu::Entry::Item { .. }) => Some(
                        m.entries[..i].iter().filter(|e| matches!(e, crate::app_ctxmenu::Entry::Item { .. })).count(),
                    ),
                    _ => None,
                });
                s.dialog = Some(a11y::DialogView { title: "Context menu".into(), items, sel, ..Default::default() });
                s.area = if sel.is_some() { Area::Palette } else { Area::Dialog };
            }
            other => {
                let (title, input) = match other {
                    Overlay::Prompt(p) => (p.title.clone(), Some(p.field.text.clone())),
                    Overlay::TopMenu(m) => (top_menu_title(m.kind), Some(m.field.text.clone())),
                    Overlay::Font(_) => ("Font picker".to_string(), None),
                    _ => ("Dialog".to_string(), None),
                };
                s.dialog = Some(a11y::DialogView { title, input, ..Default::default() });
                s.area = Area::Dialog;
            }
        }
        // The welcome tour is a dialog of its own, outside the overlay stack.
        if let (Some(o), true) = (&self.onboarding, self.overlay.is_none()) {
            s.dialog = Some(a11y::DialogView {
                title: "Welcome to Lumen".into(),
                about: Some(format!("Step {} of {}", o.step + 1, crate::app_onboarding::STEPS)),
                ..Default::default()
            });
            s.area = Area::Dialog;
        }

        if self.panel.open && matches!(self.panel.tab, PanelTab::Terminal) {
            let active = self.terms.active;
            if let Some((n, t)) = self.terms.tabs.iter().enumerate().find(|(_, t)| t.id == active) {
                let (g, off) = (&t.term.grid, t.term.scroll_offset);
                let rows = (0..g.rows.min(a11y::MAX_TERM_ROWS))
                    .map(|r| {
                        let row: String = (0..g.cols)
                            .map(|c| g.view_cell(r, c, off).ch)
                            .filter(|ch| *ch != crate::terminal::WIDE_TAIL)
                            .collect();
                        row.trim_end().to_string()
                    })
                    .collect();
                s.terminal = Some(a11y::TermView { title: t.title(n + 1), rows, cursor: (g.cy, g.cx) });
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    #[test]
    fn the_setting_decides_whether_the_tree_is_wanted() {
        let a = A11y::default();
        // No adapter (headless, or the setting was off at start): never.
        for mode in ["auto", "on", "off"] {
            assert!(!a.wanted(mode), "{mode}");
        }
    }

    #[test]
    fn the_snapshot_describes_the_open_file_and_the_caret() {
        let _g = lock();
        let dir = tmp("a11y-snap");
        let mut app = test_app(&dir);
        std::fs::write(dir.join("two.txt"), "alpha\nbeta gamma\n").unwrap();
        app.open_path(&dir.join("two.txt"), None);
        let d = &mut app.docs[app.active];
        d.ed.click(&mut d.buf, 8, false);
        let s = app.a11y_snapshot();
        assert_eq!(s.area, Area::Editor);
        assert_eq!(s.tabs.len(), 2);
        assert!(s.tabs.iter().filter(|t| t.active).count() == 1);
        let e = s.editor.clone().expect("editor");
        assert_eq!(e.name, "two.txt");
        assert_eq!(e.lines[e.caret.0 - e.first], "beta gamma");
        assert_eq!(e.caret, (1, 2));
        assert!(!s.files.is_empty(), "the explorer is open");
        // The tree builds from it with the focus on the editor.
        let u = a11y::build(&s, "");
        assert_eq!(u.focus, a11y::EDITOR);
    }

    #[test]
    fn inactive_means_no_work_and_a_connection_sends_a_whole_tree() {
        let _g = lock();
        let dir = tmp("a11y-sync");
        let mut app = test_app(&dir);
        // Without an adapter the sync is a no-op and remembers nothing.
        app.a11y_sync();
        assert!(app.a11y.last.is_none());
    }

    #[test]
    fn a_dialog_and_the_find_bar_show_up_in_the_snapshot() {
        let _g = lock();
        let dir = tmp("a11y-dlg");
        let mut app = test_app(&dir);
        app.open_find(false);
        let s = app.a11y_snapshot();
        assert_eq!(s.area, Area::Find);
        assert!(s.find.is_some());
    }
    fn frame(app: &mut App) {
        let (w, h) = (1400, 900);
        app.size = (w as u32, h as u32);
        let mut px = vec![0u32; (w * h) as usize];
        for _ in 0..2 {
            app.render_into(&mut crate::ui::Canvas { px: &mut px, w, h }, 0.016);
        }
    }

    fn names(chips: &[a11y::Chip]) -> Vec<&str> {
        chips.iter().map(|c| c.label.as_str()).collect()
    }

    fn assert_tree_is_sound(s: &Snapshot) {
        let u = a11y::build(s, "");
        let mut ids: Vec<u64> = u.nodes.iter().map(|(i, _)| i.0).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate node ids");
        for (_, nd) in &u.nodes {
            for c in nd.children() {
                assert!(u.nodes.iter().any(|(i, _)| i == c), "missing child {c:?}");
            }
        }
        assert!(u.nodes.iter().any(|(i, _)| *i == u.focus), "focus {:?} is not in the tree", u.focus);
    }

    #[test]
    fn the_chrome_of_a_plain_editor_is_in_the_snapshot() {
        let _g = lock();
        let dir = tmp("a11y-chrome");
        let mut app = test_app(&dir);
        frame(&mut app);
        let s = app.a11y_snapshot();
        assert_eq!(names(&s.menu_bar).first().copied(), Some("File"), "menu bar: {:?}", names(&s.menu_bar));
        assert!(s.menu_bar.iter().all(|c| c.popup));
        assert!(names(&s.rail).contains(&"Explorer") && names(&s.rail).contains(&"Settings"), "{:?}", names(&s.rail));
        assert!(names(&s.status).contains(&"Go to line"), "status chips: {:?}", names(&s.status));
        assert!(names(&s.status).contains(&"Problems"));
        assert!(s.panel_tabs.is_empty() && s.docks.is_empty(), "closed panels expose nothing");
        assert_tree_is_sound(&s);
    }

    #[test]
    fn panel_tabs_and_dock_tabs_follow_what_is_open() {
        let _g = lock();
        let dir = tmp("a11y-panels");
        let mut app = test_app(&dir);
        app.toggle_panel();
        app.panel.tab = PanelTab::Problems;
        frame(&mut app);
        let s = app.a11y_snapshot();
        assert!(names(&s.panel_tabs).contains(&"Problems"), "{:?}", names(&s.panel_tabs));
        let sel: Vec<&str> =
            s.panel_tabs.iter().filter(|c| c.selected == Some(true)).map(|c| c.label.as_str()).collect();
        assert_eq!(sel, ["Problems"], "exactly the current tab is selected");
        app.open_git_panel();
        frame(&mut app);
        let s = app.a11y_snapshot();
        assert_eq!(names(&s.docks), ["Git", "AI"]);
        assert_eq!(s.docks.iter().map(|c| c.selected).collect::<Vec<_>>(), [Some(true), Some(false)]);
        assert_tree_is_sound(&s);
    }

    #[test]
    fn the_settings_window_is_a_modal_dialog_with_labelled_controls() {
        let _g = lock();
        let dir = tmp("a11y-settings");
        let mut app = test_app(&dir);
        frame(&mut app);
        app.open_settings(crate::settingsui::Page::Settings);
        frame(&mut app);
        let s = app.a11y_snapshot();
        let d = s.dialog.clone().expect("the settings are a dialog");
        assert_eq!(d.title, "Settings");
        assert_eq!(s.area, Area::Dialog);
        assert!(!d.sections.is_empty() && d.sections.iter().filter(|c| c.selected == Some(true)).count() == 1);
        assert!(!d.controls.is_empty(), "the rows of the page are exposed");
        assert!(d.controls.iter().all(|c| !c.label.is_empty()), "every control has a label");
        assert_eq!(d.controls.iter().filter(|c| c.selected).count(), 1, "one row has the focus");
        assert!(d.controls.iter().any(|c| matches!(c.kind, a11y::ControlKind::Switch(_))));
        assert_tree_is_sound(&s);
    }

    #[test]
    fn a_context_menu_lists_its_items_and_the_tour_is_a_dialog() {
        let _g = lock();
        let dir = tmp("a11y-ctx");
        let mut app = test_app(&dir);
        frame(&mut app);
        app.open_ctxmenu(crate::app_ctxmenu::Target::Tab(0), (300, 200));
        let s = app.a11y_snapshot();
        let d = s.dialog.clone().expect("a dialog");
        assert_eq!(d.title, "Context menu");
        assert!(d.items.len() > 2 && d.items.iter().all(|i| !i.label.is_empty()), "{:?}", d.items);
        assert_tree_is_sound(&s);
        app.overlay = Overlay::None;
        app.onboarding_open();
        let s = app.a11y_snapshot();
        let d = s.dialog.clone().expect("the tour is a dialog");
        assert_eq!(d.title, "Welcome to Lumen");
        assert!(d.about.as_deref().is_some_and(|a| a.starts_with("Step 1 of ")), "{:?}", d.about);
        assert_tree_is_sound(&s);
    }

    #[test]
    fn toasts_reach_the_live_region() {
        let _g = lock();
        let dir = tmp("a11y-toast");
        let mut app = test_app(&dir);
        app.toasts = Default::default();
        app.notify(crate::ui_toast::ToastKind::Error, "Push failed", None);
        app.notify(crate::ui_toast::ToastKind::Success, "Saved", None);
        let s = app.a11y_snapshot();
        assert!(s.toasts.iter().any(|t| t.text == "Push failed" && t.urgent), "{:?}", s.toasts);
        assert!(s.toasts.iter().any(|t| t.text == "Saved" && !t.urgent));
    }
}
