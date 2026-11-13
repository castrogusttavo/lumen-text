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
    doc::{Diag, Doc},
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
            other => {
                let (title, input) = match other {
                    Overlay::Prompt(p) => (p.title.clone(), Some(p.field.text.clone())),
                    Overlay::Settings(_) => ("Settings".to_string(), None),
                    Overlay::TopMenu(_) => ("Menu".to_string(), None),
                    Overlay::Ctx(_) => ("Context menu".to_string(), None),
                    Overlay::Font(_) => ("Font picker".to_string(), None),
                    _ => ("Dialog".to_string(), None),
                };
                s.dialog = Some(a11y::DialogView { title, input, ..Default::default() });
                s.area = Area::Dialog;
            }
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
}
