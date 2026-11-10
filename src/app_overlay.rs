//! Overlays: command/file palette, find & replace bar, input prompts.

use crate::{
    app::{App, Focus, KeyIn},
    buffer::EditKind,
    field::TextField,
    fuzzy::fuzzy,
    search::{self, Hit},
    ui::{Canvas, Rect},
};
use regex::RegexBuilder;
use serde_json::Value;
use std::{path::PathBuf, sync::atomic::Ordering, thread, time::Instant};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Save,
    CloseTab,
    NextTab,
    PrevTab,
    ToggleSidebar,
    FocusSidebar,
    QuickOpen,
    Commands,
    ProjectSearch,
    Find,
    Replace,
    GotoLine,
    GotoSymbol,
    Format,
    Rename,
    ShowHover,
    GotoDefinition,
    GotoTypeDefinition,
    GotoImplementation,
    FindReferences,
    CodeActions,
    ToggleComment,
    Undo,
    Redo,
    NextDiagnostic,
    PrevDiagnostic,
    ReloadFile,
    RestartLsp,
    ToggleAutoImport,
    RunTask,
    RepeatTask,
    StopTask,
    TestFile,
    TestAtCursor,
    ToggleOutput,
    NewFile,
    NewDir,
    RenamePath,
    DeletePath,
    GitChanges,
    GitPanel,
    GitStage,
    GitUnstage,
    GitStageAll,
    GitDiscard,
    GitCommit,
    GitStageHunk,
    GitRevertHunk,
    GitDiffFile,
    GitFileHistory,
    MarkdownPreview,
    SplitRight,
    SplitDown,
    CloseSplit,
    ClosePane,
    FocusOtherPane,
    FocusPrevPane,
    FocusLeftPane,
    FocusRightPane,
    FocusPane3,
    FocusPane4,
    MoveToNextPane,
    MoveToPrevPane,
    ConflictOurs,
    ConflictTheirs,
    ConflictBoth,
    WorkspaceSymbols,
    Problems,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleNarrow,
    NavBack,
    NavForward,
    OpenTerminal,
    CloseTerminal,
    FollowSystemTheme,
    ExpandSelection,
    ToggleOutline,
    ToggleCodeLens,
    ToggleInlayHints,
    ToggleGitBlame,
    ToggleMinimap,
    ToggleWordWrap,
    ProjectMenu,
    BranchMenu,
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
    OpenSettings,
    CreateTheme,
    ImportZedThemes,
    OpenKeybindings,
    DebugStart,
    DebugStop,
    ToggleBreakpoint,
    ClearBreakpoints,
    StepOver,
    StepInto,
    StepOut,
    ShrinkSelection,
    IncomingCalls,
    OutgoingCalls,
    CycleTsgoSource,
    UndoBranches,
    OrganizeImports,
    ToggleFormatOnSave,
    PickTheme,
    CheckUpdate,
    RestartUpdate,
    About,
    Quit,
}

impl Cmd {
    /// The command whose variant name is `id` (used by keybindings.json).
    pub fn from_id(id: &str) -> Option<Cmd> {
        COMMANDS.iter().map(|(_, _, c)| *c).find(|c| format!("{c:?}") == id)
    }
}

pub const COMMANDS: &[(&str, &str, Cmd)] = &[
    ("File: Save", "Ctrl+S", Cmd::Save),
    ("File: Close tab", "Ctrl+W", Cmd::CloseTab),
    ("File: Reload from disk", "", Cmd::ReloadFile),
    ("File: Open…", "Ctrl+P", Cmd::QuickOpen),
    ("Palette: Commands", "Ctrl+Shift+P", Cmd::Commands),
    ("View: Toggle sidebar", "Ctrl+B", Cmd::ToggleSidebar),
    ("View: Focus explorer", "Ctrl+Shift+E", Cmd::FocusSidebar),
    ("View: Next tab", "Ctrl+Tab", Cmd::NextTab),
    ("View: Previous tab", "Ctrl+Shift+Tab", Cmd::PrevTab),
    ("Search: In file", "Ctrl+F", Cmd::Find),
    ("Search: Replace", "Ctrl+H", Cmd::Replace),
    ("Search: In project", "Ctrl+Shift+F", Cmd::ProjectSearch),
    ("Go to: Line…", "Ctrl+G", Cmd::GotoLine),
    ("Go to: Symbol in file", "Ctrl+Shift+O", Cmd::GotoSymbol),
    ("Go to: Definition", "F12", Cmd::GotoDefinition),
    ("Go to: Type definition", "", Cmd::GotoTypeDefinition),
    ("Go to: Implementation", "", Cmd::GotoImplementation),
    ("Go to: References", "Shift+F12", Cmd::FindReferences),
    ("Go to: Next problem", "F8", Cmd::NextDiagnostic),
    ("Go to: Previous problem", "Shift+F8", Cmd::PrevDiagnostic),
    ("Code: Rename symbol", "F2", Cmd::Rename),
    ("Code: Show hover info at cursor", "Ctrl+K I", Cmd::ShowHover),
    ("Code: Format document", "Shift+Alt+F", Cmd::Format),
    ("Code: Quick actions", "Ctrl+.", Cmd::CodeActions),
    ("Code: Toggle comment", "Ctrl+/", Cmd::ToggleComment),
    ("Edit: Undo", "Ctrl+Z", Cmd::Undo),
    ("Edit: Redo", "Ctrl+Shift+Z", Cmd::Redo),
    ("LSP: Restart servers", "", Cmd::RestartLsp),
    ("LSP: Toggle auto-import in completion", "", Cmd::ToggleAutoImport),
    ("Tasks: Run…", "", Cmd::RunTask),
    ("Tasks: Repeat last", "Ctrl+K R", Cmd::RepeatTask),
    ("Tasks: Stop", "Ctrl+K X", Cmd::StopTask),
    ("Tests: Run current file", "Ctrl+K F", Cmd::TestFile),
    ("Tests: Run test at cursor", "Ctrl+K T", Cmd::TestAtCursor),
    ("View: Toggle output panel", "Ctrl+J", Cmd::ToggleOutput),
    ("Code: Organize imports", "", Cmd::OrganizeImports),
    ("Code: Toggle format on save", "", Cmd::ToggleFormatOnSave),
    ("Appearance: Choose theme…", "", Cmd::PickTheme),
    ("File: New file…", "A (explorer)", Cmd::NewFile),
    ("File: New folder…", "Shift+A (explorer)", Cmd::NewDir),
    ("File: Rename…", "F2 (explorer)", Cmd::RenamePath),
    ("File: Delete…", "Delete (explorer)", Cmd::DeletePath),
    ("Git: Changed files", "", Cmd::GitChanges),
    ("Git: Open panel", "Ctrl+Shift+G", Cmd::GitPanel),
    ("Git: Stage file", "", Cmd::GitStage),
    ("Git: Unstage file", "", Cmd::GitUnstage),
    ("Git: Stage all", "", Cmd::GitStageAll),
    ("Git: Discard file changes…", "", Cmd::GitDiscard),
    ("Git: Commit…", "", Cmd::GitCommit),
    ("Git: Stage hunk", "", Cmd::GitStageHunk),
    ("Git: Revert hunk", "", Cmd::GitRevertHunk),
    ("Git: View file diff (side by side)", "", Cmd::GitDiffFile),
    ("Git: File history", "", Cmd::GitFileHistory),
    ("Markdown: Open reader beside", "Ctrl+Shift+V", Cmd::MarkdownPreview),
    ("View: Split editor right", "Ctrl+\\", Cmd::SplitRight),
    ("View: Split editor down", "Ctrl+K \\", Cmd::SplitDown),
    ("View: Close split (merge all)", "", Cmd::CloseSplit),
    ("View: Close focused pane", "Ctrl+K Shift+W", Cmd::ClosePane),
    ("View: Focus next pane", "Ctrl+K Right", Cmd::FocusOtherPane),
    ("View: Focus previous pane", "Ctrl+K Left", Cmd::FocusPrevPane),
    ("View: Focus pane 1", "Ctrl+1", Cmd::FocusLeftPane),
    ("View: Focus pane 2", "Ctrl+2", Cmd::FocusRightPane),
    ("View: Focus pane 3", "Ctrl+3", Cmd::FocusPane3),
    ("View: Focus pane 4", "Ctrl+4", Cmd::FocusPane4),
    ("View: Move tab to next pane", "Ctrl+K Shift+Right", Cmd::MoveToNextPane),
    ("View: Move tab to previous pane", "Ctrl+K Shift+Left", Cmd::MoveToPrevPane),
    ("Git: Conflict — accept current", "", Cmd::ConflictOurs),
    ("Git: Conflict — accept incoming", "", Cmd::ConflictTheirs),
    ("Git: Conflict — accept both", "", Cmd::ConflictBoth),
    ("Go to: Symbol in project", "Ctrl+T", Cmd::WorkspaceSymbols),
    ("View: Problems in open files", "", Cmd::Problems),
    ("View: Zoom in", "Ctrl+=", Cmd::ZoomIn),
    ("View: Zoom out", "Ctrl+-", Cmd::ZoomOut),
    ("View: Reset zoom", "Ctrl+0", Cmd::ZoomReset),
    ("LSP: Toggle lean mode (open files only) / full project", "", Cmd::ToggleNarrow),
    ("Go to: Back", "Alt+←", Cmd::NavBack),
    ("Go to: Forward", "Alt+→", Cmd::NavForward),
    ("Terminal: Open / toggle focus", "Ctrl+`", Cmd::OpenTerminal),
    ("Terminal: Kill", "", Cmd::CloseTerminal),
    ("Appearance: Follow system light/dark theme", "", Cmd::FollowSystemTheme),
    ("View: Toggle Files / Outline", "", Cmd::ToggleOutline),
    ("View: Toggle code lens (reference counts)", "", Cmd::ToggleCodeLens),
    ("View: Toggle inlay hints (types and parameters)", "", Cmd::ToggleInlayHints),
    ("Git: Toggle git blame", "", Cmd::ToggleGitBlame),
    ("View: Toggle minimap", "", Cmd::ToggleMinimap),
    ("View: Toggle word wrap", "", Cmd::ToggleWordWrap),
    ("Project: Switch project…", "", Cmd::ProjectMenu),
    ("Git: Switch branch…", "", Cmd::BranchMenu),
    ("Fold: Fold region", "Ctrl+Shift+[", Cmd::Fold),
    ("Fold: Unfold region", "Ctrl+Shift+]", Cmd::Unfold),
    ("Fold: Fold all", "", Cmd::FoldAll),
    ("Fold: Unfold all", "", Cmd::UnfoldAll),
    ("Preferences: Settings", "Ctrl+,", Cmd::OpenSettings),
    ("Theme: Create custom theme (JSON template)", "", Cmd::CreateTheme),
    ("Theme: Import Zed themes (min-theme etc.)", "", Cmd::ImportZedThemes),
    ("Preferences: Keyboard shortcuts", "", Cmd::OpenKeybindings),
    ("Debug: Start / continue", "F5", Cmd::DebugStart),
    ("Debug: Stop", "Shift+F5", Cmd::DebugStop),
    ("Debug: Toggle breakpoint", "F9", Cmd::ToggleBreakpoint),
    ("Debug: Step over", "F10", Cmd::StepOver),
    ("Debug: Step into", "F11", Cmd::StepInto),
    ("Debug: Step out", "Shift+F11", Cmd::StepOut),
    ("Debug: Remove all breakpoints", "", Cmd::ClearBreakpoints),
    ("Edit: Expand selection", "Shift+Alt+→", Cmd::ExpandSelection),
    ("Edit: Shrink selection", "Shift+Alt+←", Cmd::ShrinkSelection),
    ("Go to: Callers (call hierarchy)", "", Cmd::IncomingCalls),
    ("Go to: Callees (call hierarchy)", "", Cmd::OutgoingCalls),
    ("LSP: Cycle tsgo version (auto / project / bundled)", "", Cmd::CycleTsgoSource),
    ("Edit: View undo branches (undo tree)", "", Cmd::UndoBranches),
    ("Help: Check for updates", "", Cmd::CheckUpdate),
    ("Help: Restart to update", "", Cmd::RestartUpdate),
    ("Help: About Lumen", "", Cmd::About),
    ("Quit", "Ctrl+Q", Cmd::Quit),
];

#[derive(Clone)]
pub enum Action {
    Open(PathBuf, Option<(usize, usize)>),
    Cmd(Cmd),
    Goto(usize),
    /// Bottom bar pickers: indentation of the current document (see `app_topbar::indent_from_spec`).
    SetIndent(String),
    /// Language of the current document (`None` = plain text).
    SetLang(Option<String>),
    WorkspaceEdit(Value),
    Run(String),
    Theme(String),
    UndoBranch(usize),
    GitShow(PathBuf, String, String),
}

#[derive(Clone)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub action: Action,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    QuickOpen,
    Commands,
    ProjectSearch,
    Locations,
    Symbols,
    CodeActions,
    WorkspaceSymbols,
    Themes,
    /// Short list of choices opened from the bottom bar (indentation, language).
    Choice,
}

pub struct Picker {
    pub kind: PickKind,
    pub title: String,
    pub field: TextField,
    pub items: Vec<Item>,
    pub filtered: Vec<(usize, Vec<usize>)>, // (item index, matched label positions)
    pub sel: usize,
    pub scroll: usize,
    pub case: bool,
    pub regex: bool,
}

pub enum PromptKind {
    Rename {
        line: usize,
        col: usize,
    },
    GotoLine,
    NewFile {
        dir: PathBuf,
    },
    NewDir {
        dir: PathBuf,
    },
    RenamePath {
        from: PathBuf,
    },
    DeletePath {
        path: PathBuf,
    },
    GitDiscard {
        path: PathBuf,
    },
    GitCommit,
    /// "Save as" for an untitled document; `rest` (more documents to save) and `then` (the
    /// action that was waiting for the save) continue after it.
    SaveAs {
        doc: usize,
        rest: Vec<usize>,
        then: Option<crate::app_unsaved::Pending>,
    },
}

pub struct Prompt {
    pub kind: PromptKind,
    pub title: String,
    pub field: TextField,
}

pub enum Overlay {
    None,
    Picker(Picker),
    Prompt(Prompt),
    Settings(Box<crate::settingsui::Modal>),
    /// Top bar dropdowns (project switcher, branch list).
    TopMenu(Box<crate::app_topbar::TopMenu>),
    /// Right-click menu (explorer rows, tabs) and its delete confirmation.
    Ctx(Box<crate::app_ctxmenu::CtxMenu>),
}

impl Overlay {
    pub fn is_none(&self) -> bool {
        matches!(self, Overlay::None)
    }
}

pub struct FindBar {
    pub field: TextField,
    pub replace: TextField,
    pub show_replace: bool,
    pub focused: bool,
    pub in_replace: bool,
    pub case: bool,
    pub regex: bool,
    pub matches: Vec<(usize, usize)>,
    pub current: Option<usize>,
    pub valid: bool,
    sig: (usize, i32, String, bool, bool),
}

const PICKER_ROWS: usize = 10;
const ROW: i32 = 34;
const INPUT_H: i32 = 54;

/// A picker over a short list of choices with row `sel` highlighted (and scrolled into view).
pub fn choice_picker(title: &str, items: Vec<Item>, sel: usize) -> Picker {
    let mut p = Picker::new(PickKind::Choice, title, items);
    p.sel = sel.min(p.filtered.len().saturating_sub(1));
    p.keep_visible();
    p
}

impl Picker {
    fn new(kind: PickKind, title: &str, items: Vec<Item>) -> Self {
        let mut p = Picker {
            kind,
            title: title.to_string(),
            field: TextField::default(),
            items,
            filtered: Vec::new(),
            sel: 0,
            scroll: 0,
            case: false,
            regex: false,
        };
        p.refilter();
        p
    }

    pub fn refilter(&mut self) {
        let q = self.field.text.trim().to_string();
        let filter = matches!(
            self.kind,
            PickKind::QuickOpen
                | PickKind::Commands
                | PickKind::Symbols
                | PickKind::CodeActions
                | PickKind::Themes
                | PickKind::Choice
        );
        if !filter || q.is_empty() {
            self.filtered = (0..self.items.len().min(2000)).map(|i| (i, Vec::new())).collect();
        } else {
            // Several words narrow the list: every term must match somewhere in the label.
            let terms: Vec<&str> = q.split_whitespace().collect();
            let mut scored: Vec<(i32, usize, Vec<usize>)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    let (mut total, mut pos) = (0, Vec::new());
                    for t in &terms {
                        let (s, p) = fuzzy(t, &it.label)?;
                        total += s;
                        pos.extend(p);
                    }
                    pos.sort_unstable();
                    pos.dedup();
                    Some((total, i, pos))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().take(300).map(|(_, i, p)| (i, p)).collect();
        }
        self.sel = self.sel.min(self.filtered.len().saturating_sub(1));
        self.scroll = 0;
        self.keep_visible();
    }

    fn keep_visible(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + PICKER_ROWS {
            self.scroll = self.sel + 1 - PICKER_ROWS;
        }
    }

    fn move_sel(&mut self, d: isize) {
        let n = self.filtered.len() as isize;
        if n > 0 {
            self.sel = (self.sel as isize + d).rem_euclid(n) as usize;
            self.keep_visible();
        }
    }

    fn selected(&self) -> Option<Item> {
        self.filtered.get(self.sel).map(|(i, _)| self.items[*i].clone())
    }
}

impl App {
    // ───────────── opening ─────────────

    pub fn rel(&self, p: &std::path::Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().into_owned()
    }

    pub fn open_quick_open(&mut self) {
        if self.file_index.is_none() {
            self.file_index = Some(search::list_files(&self.root, 50_000));
        }
        let files = self.file_index.clone().unwrap_or_default();
        let open: Vec<PathBuf> = self.docs.iter().filter_map(|d| d.path().map(PathBuf::from)).collect();
        let mut items: Vec<Item> = Vec::new();
        for p in open.iter().rev().chain(files.iter().filter(|f| !open.contains(f))) {
            items.push(Item { label: self.rel(p), detail: String::new(), action: Action::Open(p.clone(), None) });
        }
        self.overlay = Overlay::Picker(Picker::new(PickKind::QuickOpen, "Open file", items));
    }

    pub fn open_commands(&mut self) {
        let items = COMMANDS
            .iter()
            .map(|(n, k, c)| Item { label: n.to_string(), detail: k.to_string(), action: Action::Cmd(*c) })
            .collect();
        self.overlay = Overlay::Picker(Picker::new(PickKind::Commands, "Commands", items));
    }

    pub fn open_project_search(&mut self) {
        let mut p = Picker::new(PickKind::ProjectSearch, "Search in project", Vec::new());
        if let Some(d) = self.docs.get(self.active) {
            let sel = d.ed.selected_text(&d.buf);
            if !sel.is_empty() && !sel.contains('\n') {
                p.field = TextField::with(&sel);
            }
        }
        let has_query = !p.field.text.is_empty();
        self.overlay = Overlay::Picker(p);
        if has_query {
            self.start_search();
        }
    }

    pub fn open_locations(&mut self, title: &str, items: Vec<Item>) {
        self.overlay = Overlay::Picker(Picker::new(PickKind::Locations, title, items));
    }

    pub fn open_symbols_with(&mut self, items: Vec<Item>) {
        self.overlay = Overlay::Picker(Picker::new(PickKind::Symbols, "Go to symbol", items));
    }

    pub fn open_code_actions_with(&mut self, items: Vec<Item>) {
        self.overlay = Overlay::Picker(Picker::new(PickKind::CodeActions, "Quick actions", items));
    }

    /// Bundled themes plus the user's (`~/.config/lumen-text/themes`, incl. converted Zed themes).
    pub fn theme_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.assets.join("themes"))
            .map(|rd| {
                rd.flatten().filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned())).collect()
            })
            .unwrap_or_default();
        names.extend(crate::zedtheme::list_user_themes().into_iter().map(|(n, _)| n));
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names
    }

    pub fn theme_path(&self, name: &str) -> std::path::PathBuf {
        if let Some((_, p)) = crate::zedtheme::list_user_themes().into_iter().find(|(n, _)| n == name) {
            return p;
        }
        self.assets.join("themes").join(format!("{name}.json"))
    }

    /// Converts the Zed themes installed on this machine (min-theme etc.) into our format.
    pub fn import_zed_themes(&mut self) -> usize {
        let Some(out) = crate::zedtheme::user_theme_dir() else { return 0 };
        crate::zedtheme::install_zed_themes(&crate::zedtheme::default_zed_theme_dirs(), &out).len()
    }

    pub fn open_theme_picker(&mut self) {
        self.import_zed_themes();
        let user: std::collections::HashSet<String> =
            crate::zedtheme::list_user_themes().into_iter().map(|(n, _)| n).collect();
        let current = self.theme.name.clone();
        let names = self.theme_names();
        let sel = names.iter().position(|n| *n == current).unwrap_or(0);
        let items = names
            .into_iter()
            .map(|n| Item {
                detail: if user.contains(&n) { "your theme".into() } else { String::new() },
                label: n.clone(),
                action: Action::Theme(n),
            })
            .collect();
        self.theme_orig = Some(current);
        let mut p = Picker::new(PickKind::Themes, "Choose theme (arrow keys to preview)", items);
        p.sel = sel;
        self.overlay = Overlay::Picker(p);
    }

    /// While the theme picker is open, show the highlighted theme live.
    pub fn preview_theme_selection(&mut self) {
        let Overlay::Picker(p) = &self.overlay else { return };
        if p.kind != PickKind::Themes {
            return;
        }
        if let Some(Item { action: Action::Theme(n), .. }) = p.selected()
            && n != self.theme.name
        {
            self.set_theme_inner(&n, false);
        }
    }

    /// Esc / click outside the theme picker: put the original theme back.
    pub fn restore_theme_preview(&mut self) {
        if let Some(orig) = self.theme_orig.take()
            && orig != self.theme.name
        {
            self.set_theme_inner(&orig, false);
        }
    }

    pub fn create_custom_theme(&mut self) {
        let Some(dir) = crate::zedtheme::user_theme_dir() else { return };
        match crate::zedtheme::create_custom_theme_file(&dir, "My theme") {
            Ok(path) => {
                self.say(format!("template created: {} (save to apply, then pick the theme)", path.display()));
                self.open_path(&path, None);
            }
            Err(e) => self.say(format!("could not create the theme: {e}")),
        }
    }

    pub fn pump_eslint(&mut self) {
        let mut results = Vec::new();
        while let Ok(r) = self.eslint_rx.try_recv() {
            results.push(r);
        }
        for (path, res) in results {
            self.eslint_busy.remove(&path);
            let Some(i) = self.docs.iter().position(|d| d.path() == Some(path.as_path())) else { continue };
            match res {
                Ok(msgs) => {
                    let d = &mut self.docs[i];
                    let diags: Vec<crate::doc::Diag> = msgs
                        .iter()
                        .map(|m| {
                            let a = d.pos_from_lsp(m.line.saturating_sub(1), m.col.saturating_sub(1));
                            let b = d.pos_from_lsp(m.end_line.saturating_sub(1), m.end_col.saturating_sub(1));
                            let (sl, sc) = d.buf.line_col(a);
                            let (el, ec) = d.buf.line_col(b.max(a));
                            crate::doc::Diag {
                                sl,
                                sc,
                                el,
                                ec,
                                severity: if m.severity >= 2 { 1 } else { 2 },
                                message: format!("{} (eslint)", m.message),
                                code: m.rule.clone(),
                            }
                        })
                        .collect();
                    d.set_eslint_diags(diags);
                }
                Err(e) => {
                    self.docs[i].set_eslint_diags(Vec::new());
                    self.say(format!("eslint: {e}"));
                }
            }
            self.request_redraw();
        }
    }

    /// Files handed over by another `lumen-text` invocation (single-instance mode).
    pub fn pump_inbox(&mut self) {
        let mut batches: Vec<Vec<String>> = Vec::new();
        if let Some(rx) = &self.inbox {
            while let Ok(b) = rx.try_recv() {
                batches.push(b);
            }
        }
        if batches.is_empty() {
            return;
        }
        for arg in batches.into_iter().flatten() {
            let (p, at) = crate::parse_target(&arg);
            if !p.is_dir() {
                self.open_path(&p, at);
            }
        }
        if let Some(g) = &self.gfx {
            g.window.focus_window();
        }
    }

    pub fn open_undo_branches(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let items: Vec<Item> = d
            .buf
            .undo_branches()
            .into_iter()
            .map(|(n, desc)| Item { label: desc, detail: String::new(), action: Action::UndoBranch(n) })
            .collect();
        if items.is_empty() {
            return self.say("no alternative branches from here (use Ctrl+Z and edit to create one)");
        }
        self.open_locations("Undo branches", items);
    }

    pub fn set_theme(&mut self, name: &str) {
        self.config.appearance = "fixed".into();
        self.set_theme_inner(name, true);
    }

    pub fn set_theme_inner(&mut self, name: &str, persist: bool) {
        let path = self.theme_path(name);
        match crate::theme::Theme::load(&path) {
            Ok(t) => {
                self.theme = t;
                for d in &mut self.docs {
                    if let Some(h) = d.hl.as_mut() {
                        h.theme_changed();
                    }
                }
                self.config.theme = Some(name.to_string());
                if persist {
                    self.config.save();
                }
                self.say(format!("theme: {name}"));
            }
            Err(e) => self.say(format!("invalid theme: {e}")),
        }
    }

    pub fn open_workspace_symbols(&mut self) {
        if self.with_server_pub().is_none() {
            return self.say("project symbols need the language server (open a TS/JS file)");
        }
        self.overlay =
            Overlay::Picker(Picker::new(PickKind::WorkspaceSymbols, "Symbol in project (type to search)", Vec::new()));
    }

    pub fn open_problems(&mut self) {
        let mut items = Vec::new();
        for d in &self.docs {
            let Some(p) = d.path() else { continue };
            for g in d.diags.iter().filter(|g| g.severity <= 2) {
                let icon = if g.severity == 1 { "\u{f057}" } else { "\u{f071}" };
                items.push(Item {
                    label: format!("{icon} {}", g.message.lines().next().unwrap_or("")),
                    detail: format!("{}:{}", self.rel(p), g.sl + 1),
                    action: Action::Open(p.to_path_buf(), Some((g.sl, g.sc))),
                });
            }
        }
        if items.is_empty() {
            return self.say("no problems in open files");
        }
        let title = format!("{} problem(s)", items.len());
        self.open_locations(&title, items);
    }

    pub fn open_goto_line(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        self.overlay = Overlay::Prompt(Prompt {
            kind: PromptKind::GotoLine,
            title: "Go to line (line[:column])".into(),
            field: TextField::default(),
        });
    }

    pub fn open_prompt_rename(&mut self, name: &str, line: usize, col: usize) {
        self.overlay = Overlay::Prompt(Prompt {
            kind: PromptKind::Rename { line, col },
            title: "New symbol name".into(),
            field: TextField::with(name),
        });
    }

    pub fn start_search(&mut self) {
        let Overlay::Picker(p) = &mut self.overlay else { return };
        if p.kind == PickKind::WorkspaceSymbols {
            let q = p.field.text.trim().to_string();
            return self.request_workspace_symbols(q);
        }
        if p.kind != PickKind::ProjectSearch {
            return;
        }
        p.items.clear();
        p.filtered.clear();
        p.sel = 0;
        let query = p.field.text.clone();
        let (case, regex) = (p.case, p.regex);
        let my_gen = self.search_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if query.is_empty() {
            return;
        }
        let (root, generation, tx, wake) =
            (self.root.clone(), self.search_gen.clone(), self.search_tx.clone(), self.wake());
        thread::spawn(move || search::search(&root, &query, case, regex, generation, my_gen, tx, || wake()));
    }

    pub fn pump_background(&mut self) {
        self.pump_lsp();
        self.pump_task();
        self.pump_term();
        self.pump_inbox();
        self.pump_eslint();
        self.pump_debug();
        self.pump_watcher();
        self.pump_update();
        let cur = self.search_gen.load(Ordering::SeqCst);
        let mut incoming: Vec<Hit> = Vec::new();
        while let Ok((g, hits)) = self.search_rx.try_recv() {
            if g == cur {
                incoming.extend(hits);
            }
        }
        if incoming.is_empty() {
            return;
        }
        let items: Vec<Item> = incoming
            .into_iter()
            .map(|h| Item {
                label: h.text.clone(),
                detail: format!("{}:{}", self.rel(&h.path), h.line + 1),
                action: Action::Open(h.path.clone(), Some((h.line, h.col))),
            })
            .collect();
        if let Overlay::Picker(p) = &mut self.overlay
            && p.kind == PickKind::ProjectSearch
        {
            p.items.extend(items);
            p.refilter();
        }
    }

    // ───────────── commands ─────────────

    pub fn run_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Save => self.save_active(),
            Cmd::CloseTab => {
                let a = self.active;
                self.close_tab(a, false)
            }
            Cmd::NextTab | Cmd::PrevTab => {
                let len = self.docs.len();
                if len > 0 {
                    let n = if c == Cmd::NextTab { (self.active + 1) % len } else { (self.active + len - 1) % len };
                    self.activate(n);
                }
            }
            Cmd::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Cmd::FocusSidebar => {
                self.sidebar_open = true;
                self.focus = Focus::Sidebar;
            }
            Cmd::QuickOpen => self.open_quick_open(),
            Cmd::Commands => self.open_commands(),
            Cmd::ProjectSearch => self.open_project_search(),
            Cmd::Find => self.open_find(false),
            Cmd::Replace => self.open_find(true),
            Cmd::GotoLine => self.open_goto_line(),
            Cmd::GotoSymbol => self.open_symbols(),
            Cmd::Format => self.format_document(),
            Cmd::Rename => self.start_rename(),
            Cmd::ShowHover => self.request_hover_at_cursor(),
            Cmd::GotoDefinition => self.goto_definition(crate::lsp::DefKind::Definition),
            Cmd::GotoTypeDefinition => self.goto_definition(crate::lsp::DefKind::TypeDefinition),
            Cmd::GotoImplementation => self.goto_definition(crate::lsp::DefKind::Implementation),
            Cmd::FindReferences => self.find_references(),
            Cmd::CodeActions => self.code_actions(),
            Cmd::ToggleComment => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.ed.toggle_comment(&mut d.buf);
                    d.reveal = true;
                }
            }
            Cmd::Undo | Cmd::Redo => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    if c == Cmd::Undo {
                        d.ed.undo(&mut d.buf)
                    } else {
                        d.ed.redo(&mut d.buf)
                    }
                    d.reveal = true;
                }
            }
            Cmd::NextDiagnostic => self.next_diagnostic(true),
            Cmd::PrevDiagnostic => self.next_diagnostic(false),
            Cmd::ReloadFile => self.reload_active(),
            Cmd::RestartLsp => self.restart_lsp(),
            Cmd::ToggleAutoImport => self.toggle_auto_import(),
            Cmd::RunTask => self.open_task_picker(),
            Cmd::RepeatTask => self.repeat_task(),
            Cmd::StopTask => self.stop_task(),
            Cmd::TestFile => self.run_tests(false),
            Cmd::TestAtCursor => self.run_tests(true),
            Cmd::ToggleOutput => self.toggle_panel(),
            Cmd::NewFile => self.prompt_new_file(false),
            Cmd::NewDir => self.prompt_new_file(true),
            Cmd::RenamePath => self.prompt_rename_path(),
            Cmd::DeletePath => self.prompt_delete_path(),
            Cmd::GitChanges => self.open_git_changes(),
            Cmd::GitPanel => self.toggle_git_panel(),
            Cmd::GitStage => self.git_stage(true),
            Cmd::GitUnstage => self.git_stage(false),
            Cmd::GitStageAll => self.git_stage_all(),
            Cmd::GitDiscard => self.prompt_git_discard(),
            Cmd::GitCommit => self.prompt_git_commit(),
            Cmd::GitStageHunk => self.git_stage_hunk(),
            Cmd::GitRevertHunk => self.git_revert_hunk(),
            Cmd::GitDiffFile => self.open_diff_head(),
            Cmd::GitFileHistory => self.open_file_history(),
            Cmd::MarkdownPreview => self.toggle_md_reader(),
            Cmd::SplitRight => self.split_right(),
            Cmd::SplitDown => self.split_down(),
            Cmd::CloseSplit => self.split_close(),
            Cmd::ClosePane => self.close_focused_pane(),
            Cmd::FocusOtherPane => self.focus_next_pane(1),
            Cmd::FocusPrevPane => self.focus_next_pane(-1),
            Cmd::FocusLeftPane => self.focus_pane_n(0),
            Cmd::FocusRightPane => self.focus_pane_n(1),
            Cmd::FocusPane3 => self.focus_pane_n(2),
            Cmd::FocusPane4 => self.focus_pane_n(3),
            Cmd::MoveToNextPane => self.move_to_pane(1),
            Cmd::MoveToPrevPane => self.move_to_pane(-1),
            Cmd::ConflictOurs => self.resolve_conflict(crate::gitx::Pick::Ours),
            Cmd::ConflictTheirs => self.resolve_conflict(crate::gitx::Pick::Theirs),
            Cmd::ConflictBoth => self.resolve_conflict(crate::gitx::Pick::Both),
            Cmd::WorkspaceSymbols => self.open_workspace_symbols(),
            Cmd::Problems => self.open_problems(),
            Cmd::ZoomIn => self.zoom(1.0),
            Cmd::ZoomOut => self.zoom(-1.0),
            Cmd::ZoomReset => self.zoom(0.0),
            Cmd::ToggleNarrow => self.toggle_narrow(),
            Cmd::NavBack => self.navigate(true),
            Cmd::NavForward => self.navigate(false),
            Cmd::OpenTerminal => self.toggle_terminal_focus(),
            Cmd::CloseTerminal => self.close_terminal(),
            Cmd::DebugStart
            | Cmd::DebugStop
            | Cmd::ToggleBreakpoint
            | Cmd::ClearBreakpoints
            | Cmd::StepOver
            | Cmd::StepInto
            | Cmd::StepOut => self.run_debug_cmd(c),
            Cmd::CreateTheme => self.create_custom_theme(),
            Cmd::ImportZedThemes => {
                let n = self.import_zed_themes();
                self.say(format!("{n} Zed theme(s) imported"));
            }
            Cmd::OpenSettings => self.open_settings(crate::settingsui::Page::Settings),
            Cmd::OpenKeybindings => self.open_settings(crate::settingsui::Page::Keybinds),
            Cmd::ToggleMinimap => {
                for d in &mut self.docs {
                    d.minimap = !d.minimap;
                }
                self.minimap_default = self.docs.first().is_none_or(|d| d.minimap);
            }
            Cmd::ProjectMenu => self.open_topmenu_cmd(crate::app_topbar::MenuKind::Project),
            Cmd::BranchMenu => self.open_topmenu_cmd(crate::app_topbar::MenuKind::Branch),
            Cmd::ToggleWordWrap if self.split.is_some() => self.split_toggle_wrap(),
            Cmd::ToggleWordWrap => {
                self.config.word_wrap = !self.config.word_wrap;
                self.config.save();
                for d in &mut self.docs {
                    d.set_word_wrap(self.config.word_wrap);
                }
                let on = self.config.word_wrap;
                self.say(if on { "word wrap: on" } else { "word wrap: off" });
            }
            Cmd::Fold | Cmd::Unfold | Cmd::FoldAll | Cmd::UnfoldAll => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    let did = match c {
                        Cmd::Fold => d.fold_at_cursor(),
                        Cmd::Unfold => d.unfold_at_cursor(),
                        Cmd::FoldAll => {
                            d.fold_all();
                            true
                        }
                        _ => {
                            d.unfold_all();
                            true
                        }
                    };
                    if !did {
                        self.say(if c == Cmd::Fold { "no region to fold here" } else { "nothing folded here" });
                    }
                }
            }
            Cmd::ToggleCodeLens => {
                self.config.code_lens = !self.config.code_lens;
                self.config.save();
                for d in &mut self.docs {
                    d.lenses.clear();
                    d.lens_due = Some(Instant::now());
                }
                for l in &mut self.lsps {
                    l.notify("workspace/didChangeConfiguration", serde_json::json!({"settings": {}}));
                }
                let on = self.config.code_lens;
                self.say(if on { "code lens: on" } else { "code lens: off" });
            }
            Cmd::ToggleInlayHints => {
                self.config.inlay_hints = !self.config.inlay_hints;
                self.config.save();
                self.refresh_inlay_hints();
                let on = self.config.inlay_hints;
                self.say(if on { "inlay hints: on" } else { "inlay hints: off" });
            }
            Cmd::ToggleGitBlame => self.toggle_git_blame(),
            Cmd::ToggleOutline => {
                self.sidebar_open = true;
                self.sidebar_mode = if self.sidebar_mode == crate::app::SidebarMode::Files {
                    crate::app::SidebarMode::Outline
                } else {
                    crate::app::SidebarMode::Files
                };
                self.tree.scroll = 0.0;
            }
            Cmd::ExpandSelection => self.expand_selection(),
            Cmd::ShrinkSelection => self.shrink_selection(),
            Cmd::IncomingCalls => self.call_hierarchy(true),
            Cmd::OutgoingCalls => self.call_hierarchy(false),
            Cmd::FollowSystemTheme => {
                self.config.appearance =
                    if self.config.appearance == "system" { "fixed".into() } else { "system".into() };
                self.config.save();
                let on = self.config.appearance == "system";
                self.say(if on { "theme: following the system" } else { "theme: fixed" });
                self.follow_system_theme();
            }
            Cmd::CycleTsgoSource => self.cycle_tsgo_source(),
            Cmd::UndoBranches => self.open_undo_branches(),
            Cmd::OrganizeImports => self.organize_imports(),
            Cmd::ToggleFormatOnSave => self.toggle_format_on_save(),
            Cmd::PickTheme => self.open_theme_picker(),
            Cmd::CheckUpdate => self.update_check_manual(),
            Cmd::RestartUpdate => self.update_restart_cmd(),
            Cmd::About => self.update_show_about(),
            Cmd::Quit => {
                if self.try_quit() {
                    std::process::exit(0);
                }
            }
        }
    }

    /// Run a command reached through the keymap (user file or `keymap::DEFAULTS`). Returns true
    /// when the app should quit. Text-editing commands behave like their hard-wired keys did:
    /// extra cursors collapse and the edit is synced/notified like any other keystroke.
    pub fn run_key_cmd(&mut self, c: Cmd) -> bool {
        match c {
            Cmd::Quit => return self.try_quit(),
            Cmd::Undo | Cmd::Redo | Cmd::ToggleComment => {
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.ed.clear_extra();
                }
                self.run_cmd(c);
                return self.after_key(None);
            }
            _ => self.run_cmd(c),
        }
        self.request_redraw();
        false
    }

    /// Run the command a keybinding names (`"none"` does nothing; an unknown id says so).
    pub fn run_bound(&mut self, id: &str) -> bool {
        match Cmd::from_id(id) {
            Some(c) => self.run_key_cmd(c),
            None => {
                if id != "none" {
                    self.say(format!("shortcut points to an unknown command: {id}"));
                }
                false
            }
        }
    }

    pub fn reload_active(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(PathBuf::from) else { return };
        if d.buf.is_dirty() {
            self.say("Unsaved changes — save or close the tab before reloading");
            return;
        }
        let idx = self.active;
        self.close_tab(idx, true);
        self.open_path(&path, None);
        self.say("Reloaded from disk");
    }

    // ───────────── overlay input ─────────────

    pub fn overlay_key(&mut self, ev: &KeyIn) -> bool {
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_key(ev);
            return false;
        }
        if matches!(self.overlay, Overlay::TopMenu(_)) {
            self.topmenu_key(ev);
            return false;
        }
        if matches!(self.overlay, Overlay::Ctx(_)) {
            self.ctxmenu_key(ev);
            return false;
        }
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        let key = ev.key.clone();

        // Shared text-field editing.
        enum Next {
            Close,
            Accept,
            Changed,
            Nothing,
        }
        let mut paste: Option<String> = None;
        if ctrl && let Key::Character(c) = &key {
            match c.to_lowercase().as_str() {
                "v" => paste = Some(self.clip.get()),
                "a" => {
                    match &mut self.overlay {
                        Overlay::Picker(p) => p.field.select_all(),
                        Overlay::Prompt(p) => p.field.select_all(),
                        _ => {}
                    }
                    self.request_redraw();
                    return false;
                }
                _ => {}
            }
        }

        // Take the field out while editing so the picker can be borrowed independently.
        let mut field = match &mut self.overlay {
            Overlay::Picker(p) => std::mem::take(&mut p.field),
            Overlay::Prompt(p) => std::mem::take(&mut p.field),
            Overlay::None | Overlay::Settings(_) | Overlay::TopMenu(_) | Overlay::Ctx(_) => return false,
        };
        let sel_move = |app: &mut App, d: isize| {
            if let Overlay::Picker(p) = &mut app.overlay {
                p.move_sel(d)
            }
        };
        let next = match &key {
            Key::Named(NamedKey::Escape) => Next::Close,
            Key::Named(NamedKey::Enter) => Next::Accept,
            Key::Named(NamedKey::ArrowDown) => {
                sel_move(self, 1);
                Next::Nothing
            }
            Key::Named(NamedKey::ArrowUp) => {
                sel_move(self, -1);
                Next::Nothing
            }
            Key::Named(NamedKey::PageDown) => {
                sel_move(self, PICKER_ROWS as isize);
                Next::Nothing
            }
            Key::Named(NamedKey::PageUp) => {
                sel_move(self, -(PICKER_ROWS as isize));
                Next::Nothing
            }
            Key::Named(NamedKey::Tab) => {
                sel_move(self, if shift { -1 } else { 1 });
                Next::Nothing
            }
            Key::Named(NamedKey::Backspace) => {
                let ch = if ctrl { field.delete_word() } else { field.backspace() };
                if ch { Next::Changed } else { Next::Nothing }
            }
            Key::Named(NamedKey::Delete) => {
                if field.delete() {
                    Next::Changed
                } else {
                    Next::Nothing
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                field.left();
                Next::Nothing
            }
            Key::Named(NamedKey::ArrowRight) => {
                field.right();
                Next::Nothing
            }
            Key::Named(NamedKey::Home) => {
                field.home();
                Next::Nothing
            }
            Key::Named(NamedKey::End) => {
                field.end();
                Next::Nothing
            }
            Key::Character(c) if alt && (c.as_str() == "c" || c.as_str() == "r") => {
                if let Overlay::Picker(p) = &mut self.overlay {
                    if c.as_str() == "c" { p.case = !p.case } else { p.regex = !p.regex }
                }
                Next::Changed
            }
            _ => {
                if let Some(t) = paste.take() {
                    field.insert(&t.replace('\n', " "));
                    Next::Changed
                } else if !ctrl && !alt {
                    match &ev.text {
                        Some(t) if !t.is_empty() => {
                            field.insert(t);
                            Next::Changed
                        }
                        _ => Next::Nothing,
                    }
                } else {
                    Next::Nothing
                }
            }
        };
        match &mut self.overlay {
            Overlay::Picker(p) => p.field = field,
            Overlay::Prompt(p) => p.field = field,
            Overlay::None | Overlay::Settings(_) | Overlay::TopMenu(_) | Overlay::Ctx(_) => {}
        }

        match next {
            Next::Close => {
                self.overlay = Overlay::None;
                self.restore_theme_preview();
            }
            Next::Accept => self.overlay_accept(),
            Next::Changed => {
                let mut schedule = false;
                if let Overlay::Picker(p) = &mut self.overlay {
                    if p.kind == PickKind::ProjectSearch || p.kind == PickKind::WorkspaceSymbols {
                        schedule = true;
                    } else {
                        p.refilter();
                    }
                }
                if schedule {
                    self.search_due = Some(std::time::Instant::now() + std::time::Duration::from_millis(180));
                }
            }
            Next::Nothing => {}
        }
        self.preview_theme_selection();
        self.request_redraw();
        false
    }

    pub fn overlay_accept(&mut self) {
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        match overlay {
            Overlay::Settings(_) | Overlay::TopMenu(_) | Overlay::Ctx(_) => {}
            Overlay::Picker(p) => {
                if let Some(item) = p.selected() {
                    self.run_action(item.action);
                } else {
                    self.overlay = Overlay::Picker(p);
                }
            }
            Overlay::Prompt(p) => match p.kind {
                PromptKind::GotoLine => {
                    let mut it = p.field.text.trim().split(':');
                    let line = it.next().and_then(|s| s.trim().parse::<usize>().ok());
                    let col = it.next().and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1);
                    if let (Some(l), Some(d)) = (line, self.docs.get_mut(self.active)) {
                        let pos = d.buf.pos(l.saturating_sub(1), col.saturating_sub(1));
                        d.ed.click(&mut d.buf, pos, false);
                        d.reveal = true;
                    }
                }
                PromptKind::Rename { line, col } => self.send_rename(line, col, p.field.text.trim().to_string()),
                PromptKind::NewFile { dir } => self.create_path(&dir, &p.field.text, false),
                PromptKind::NewDir { dir } => self.create_path(&dir, &p.field.text, true),
                PromptKind::RenamePath { from } => self.rename_path(&from, &p.field.text),
                PromptKind::DeletePath { path } => {
                    if matches!(p.field.text.trim().to_lowercase().as_str(), "sim" | "s" | "y" | "yes") {
                        self.delete_path(&path)
                    } else {
                        self.say("delete cancelled")
                    }
                }
                PromptKind::GitDiscard { path } => {
                    if matches!(p.field.text.trim().to_lowercase().as_str(), "sim" | "s" | "y" | "yes") {
                        self.git_discard(&path)
                    } else {
                        self.say("cancelado")
                    }
                }
                PromptKind::GitCommit => self.git_commit(&p.field.text),
                PromptKind::SaveAs { doc, rest, then } => self.save_as_answer(doc, rest, then, p.field.text),
            },
            Overlay::None => {}
        }
    }

    pub fn run_action(&mut self, a: Action) {
        match a {
            Action::Open(path, at) => {
                self.push_nav();
                self.open_path(&path, at);
                self.focus = Focus::Editor;
            }
            Action::Cmd(c) => self.run_cmd(c),
            Action::Goto(p) => {
                self.push_nav();
                if let Some(d) = self.docs.get_mut(self.active) {
                    d.ed.click(&mut d.buf, p, false);
                    d.reveal = true;
                }
            }
            Action::SetIndent(spec) => self.set_doc_indent(&spec),
            Action::SetLang(id) => self.set_doc_language(id),
            Action::WorkspaceEdit(v) => {
                if v.get("__resolve").is_some() {
                    self.resolve_code_action(&v["__resolve"]);
                } else {
                    self.apply_workspace_edit(&v)
                }
            }
            Action::Run(cmd) => {
                let title = cmd.clone();
                self.run_task(&cmd, &title)
            }
            Action::Theme(name) => {
                self.theme_orig = None;
                self.set_theme(&name)
            }
            Action::GitShow(path, commit, subject) => self.open_diff_commit(&path, &commit, &subject),
            Action::UndoBranch(n) => {
                if let Some(d) = self.docs.get_mut(self.active)
                    && let Some(sel) = d.buf.redo_branch(n)
                {
                    d.ed.sel = sel;
                    d.ed.clear_extra();
                    d.reveal = true;
                }
            }
        }
    }

    fn picker_rect(&self) -> Rect {
        let (w, _) = self.size;
        let pw = (w as i32 - 40).clamp(300, 760);
        Rect::new((w as i32 - pw) / 2, 70, pw, 0)
    }

    pub fn overlay_mouse(&mut self, x: f64, y: f64) -> bool {
        if self.overlay.is_none() {
            return false;
        }
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_mouse(x, y);
            return true;
        }
        if matches!(self.overlay, Overlay::TopMenu(_)) {
            return self.topmenu_mouse(x, y);
        }
        if matches!(self.overlay, Overlay::Ctx(_)) {
            return self.ctxmenu_mouse(x, y);
        }
        let rows = self.layout.overlay_rows.clone();
        if let Some((_, idx)) = rows.iter().find(|(r, _)| r.contains(x, y)) {
            if let Overlay::Picker(p) = &mut self.overlay {
                p.sel = *idx;
            }
            self.overlay_accept();
            return true;
        }
        let r = self.picker_rect();
        if x < r.x as f64
            || x > r.right() as f64
            || y < r.y as f64
            || y > (r.y + INPUT_H + PICKER_ROWS as i32 * ROW + 12) as f64
        {
            self.overlay = Overlay::None;
            self.restore_theme_preview();
        }
        true
    }

    pub fn overlay_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_wheel(delta);
            return true;
        }
        if matches!(self.overlay, Overlay::TopMenu(_)) {
            self.topmenu_wheel(delta);
            return true;
        }
        if matches!(self.overlay, Overlay::Ctx(_)) {
            return true;
        }
        let Overlay::Picker(p) = &mut self.overlay else { return !self.overlay.is_none() };
        let d = match delta {
            MouseScrollDelta::LineDelta(_, y) => -(y as isize) * 3,
            MouseScrollDelta::PixelDelta(pp) => -(pp.y / ROW as f64) as isize,
        };
        let max = p.filtered.len().saturating_sub(PICKER_ROWS) as isize;
        p.scroll = (p.scroll as isize + d).clamp(0, max) as usize;
        true
    }

    // ───────────── find / replace ─────────────

    pub fn open_find(&mut self, replace: bool) {
        if self.docs.is_empty() {
            return;
        }
        let seed = {
            let d = &self.docs[self.active];
            let s = d.ed.selected_text(&d.buf);
            (!s.is_empty() && !s.contains('\n')).then_some(s)
        };
        let f = self.find.get_or_insert_with(|| FindBar {
            field: TextField::default(),
            replace: TextField::default(),
            show_replace: false,
            focused: true,
            in_replace: false,
            case: false,
            regex: false,
            matches: Vec::new(),
            current: None,
            valid: true,
            sig: (usize::MAX, -1, String::new(), false, false),
        });
        f.focused = true;
        f.show_replace |= replace;
        f.in_replace = replace;
        if let Some(s) = seed {
            f.field = TextField::with(&s);
        } else {
            f.field.end();
        }
    }

    /// Recompute matches when the query, options, document or text changed.
    pub fn refresh_find(&mut self) {
        let active = self.active;
        let Some(f) = self.find.as_mut() else { return };
        let Some(d) = self.docs.get(active) else { return };
        let sig = (active, d.version, f.field.text.clone(), f.case, f.regex);
        if f.sig == sig {
            return;
        }
        let query_changed = f.sig.2 != sig.2 || f.sig.3 != sig.3 || f.sig.4 != sig.4 || f.sig.0 != sig.0;
        f.sig = sig;
        f.matches.clear();
        f.valid = true;
        if f.field.text.is_empty() {
            f.current = None;
            return;
        }
        let pat = if f.regex { f.field.text.clone() } else { regex::escape(&f.field.text) };
        let Ok(re) = RegexBuilder::new(&pat).case_insensitive(!f.case).build() else {
            f.valid = false;
            f.current = None;
            return;
        };
        'outer: for l in 0..d.buf.line_count() {
            let line = d.buf.line(l);
            let ls = d.buf.line_start(l);
            for m in re.find_iter(&line) {
                if m.start() == m.end() {
                    continue;
                }
                let a = ls + line[..m.start()].chars().count();
                f.matches.push((a, a + line[m.start()..m.end()].chars().count()));
                if f.matches.len() >= 10_000 {
                    break 'outer;
                }
            }
        }
        if query_changed {
            let head = d.ed.sel.start();
            f.current =
                f.matches.iter().position(|&(a, _)| a >= head).or(if f.matches.is_empty() { None } else { Some(0) });
        } else if let Some(c) = f.current {
            f.current = if f.matches.is_empty() { None } else { Some(c.min(f.matches.len() - 1)) };
        }
    }

    fn find_jump(&mut self, forward: bool) {
        self.refresh_find();
        let Some(f) = self.find.as_mut() else { return };
        if f.matches.is_empty() {
            return;
        }
        let n = f.matches.len();
        let cur = match f.current {
            Some(c) => {
                if forward {
                    (c + 1) % n
                } else {
                    (c + n - 1) % n
                }
            }
            None => 0,
        };
        f.current = Some(cur);
        let (a, b) = f.matches[cur];
        if let Some(d) = self.docs.get_mut(self.active) {
            d.ed.sel = crate::buffer::Sel { anchor: a, head: b };
            d.reveal = true;
        }
    }

    fn replace_one(&mut self) {
        self.refresh_find();
        let Some(f) = self.find.as_ref() else { return };
        let (Some(c), rep) = (f.current, f.replace.text.clone()) else { return };
        let (a, b) = f.matches[c];
        if let Some(d) = self.docs.get_mut(self.active) {
            d.buf.close_group();
            let before = d.ed.sel;
            d.buf.replace(a, b, &rep, EditKind::Other, before);
            d.ed.sel = crate::buffer::Sel::caret(a + rep.chars().count());
            d.buf.set_after(d.ed.sel);
            d.buf.close_group();
            d.sync_edits();
        }
        self.refresh_find();
        self.find_jump(true);
    }

    fn replace_all(&mut self) {
        self.refresh_find();
        let Some(f) = self.find.as_ref() else { return };
        if f.matches.is_empty() {
            return;
        }
        let (matches, rep) = (f.matches.clone(), f.replace.text.clone());
        let n = matches.len();
        if let Some(d) = self.docs.get_mut(self.active) {
            let before = d.ed.sel;
            d.buf.begin_compound(before);
            for &(a, b) in matches.iter().rev() {
                d.buf.replace(a, b, &rep, EditKind::Other, before);
            }
            d.buf.end_compound(before);
            d.ed.sel = crate::buffer::Sel::caret(d.ed.sel.head.min(d.buf.len_chars()));
            d.sync_edits();
        }
        self.say(format!("{n} replacements"));
    }

    /// Keys while the find bar has focus. Returns true when consumed.
    pub fn find_key(&mut self, ev: &KeyIn) -> bool {
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        let key = ev.key.clone();
        let mut paste = None;
        if ctrl && let Key::Character(c) = &key {
            match c.to_lowercase().as_str() {
                "v" => paste = Some(self.clip.get()),
                "a" => {
                    if let Some(f) = self.find.as_mut() {
                        if f.in_replace { f.replace.select_all() } else { f.field.select_all() }
                    }
                    return false;
                }
                "f" | "h" => return false, // handled globally (re-focus)
                _ => {}
            }
        }
        if let Key::Named(NamedKey::Enter) = &key {
            if alt && ctrl {
                self.replace_all();
            } else if self.find.as_ref().is_some_and(|f| f.in_replace) {
                self.replace_one();
            } else {
                self.find_jump(!shift);
            }
            return true;
        }
        if let Key::Named(NamedKey::F3) = &key {
            self.find_jump(!shift);
            return true;
        }
        let Some(f) = self.find.as_mut() else { return false };
        match &key {
            Key::Named(NamedKey::Escape) => {
                self.find = None;
            }
            Key::Named(NamedKey::Tab) => {
                if f.show_replace {
                    f.in_replace = !f.in_replace;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.find_jump(true);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.find_jump(false);
            }
            Key::Character(c) if alt && (c.as_str() == "c" || c.as_str() == "r") => {
                if c.as_str() == "c" {
                    f.case = !f.case
                } else {
                    f.regex = !f.regex
                }
            }
            _ => {
                let field = if f.in_replace { &mut f.replace } else { &mut f.field };
                match &key {
                    Key::Named(NamedKey::Backspace) => {
                        if ctrl {
                            field.delete_word()
                        } else {
                            field.backspace()
                        };
                    }
                    Key::Named(NamedKey::Delete) => {
                        field.delete();
                    }
                    Key::Named(NamedKey::ArrowLeft) => field.left(),
                    Key::Named(NamedKey::ArrowRight) => field.right(),
                    Key::Named(NamedKey::Home) => field.home(),
                    Key::Named(NamedKey::End) => field.end(),
                    _ => {
                        if let Some(t) = paste {
                            field.insert(&t.replace('\n', " "));
                        } else if !ctrl && !alt {
                            if let Some(t) = &ev.text {
                                field.insert(t);
                            }
                        } else {
                            return false;
                        }
                    }
                }
                // Typing in the search field jumps to the first match from the cursor.
                self.refresh_find();
                let jump = self.find.as_ref().is_some_and(|f| !f.in_replace);
                if jump
                    && let Some(f) = self.find.as_ref()
                    && let Some(c) = f.current
                {
                    let (a, b) = f.matches[c];
                    if let Some(d) = self.docs.get_mut(self.active) {
                        d.ed.sel = crate::buffer::Sel { anchor: a, head: b };
                        d.reveal = true;
                    }
                }
            }
        }
        true
    }

    // ───────────── drawing ─────────────

    pub fn draw_find(&mut self, cv: &mut Canvas) {
        self.refresh_find();
        let Some(f) = &self.find else { return };
        let c = self.theme.chrome;
        let ed = self.layout.editor;
        let w = (ed.w - 40).clamp(280, 540);
        let h = if f.show_replace { 78 } else { 42 };
        let r = Rect::new(ed.right() - w - 14, ed.y + 8, w, h);
        cv.shadow(r, 10);
        cv.card(r, 10, c.widget_bg, c.border);
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        let field_w = w - 168;
        let field = |cv: &mut Canvas,
                     tx: &mut crate::ui::TextCtx,
                     y: i32,
                     text: &str,
                     hint: &str,
                     cursor: usize,
                     focus: bool,
                     bad: bool,
                     selall: bool| {
            let b = Rect::new(r.x + 8, y, field_w, 28);
            cv.card(b, 7, c.input_bg, if focus { c.accent } else { c.border });
            let (shown, col) = if text.is_empty() { (hint, dim) } else { (text, if bad { c.error } else { fg }) };
            if selall {
                let w = tx.ui_width(text, false);
                cv.fill_round(Rect::new(b.x + 7, b.y + 3, w + 4, 22), 4, c.accent.with_alpha(0x50));
            }
            tx.draw_ui(cv, b.x + 9, b.y + 5, shown, col, b.inset(1), false);
            if focus {
                let prefix: String = text.chars().take(cursor).collect();
                let x = b.x + 9 + tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, b.y + 6, 1, 16), fg, b);
            }
        };
        field(
            cv,
            &mut self.tx,
            r.y + 7,
            &f.field.text,
            "Search",
            f.field.cursor,
            f.focused && !f.in_replace,
            !f.valid,
            f.field.sel_all,
        );
        let count = match (f.matches.len(), f.current) {
            (0, _) => {
                if f.field.text.is_empty() {
                    String::new()
                } else {
                    "no results".to_string()
                }
            }
            (n, Some(c)) => format!("{} de {}", c + 1, n),
            (n, None) => format!("{n}"),
        };
        let mut x = r.x + 8 + field_w + 10;
        let ty = r.y + 7 + 5;
        let toggle = |on: bool| if on { c.accent } else { dim };
        self.tx.draw_ui(cv, x, ty, "Aa", toggle(f.case), r, true);
        x += 28;
        self.tx.draw_ui(cv, x, ty, ".*", toggle(f.regex), r, true);
        x += 26;
        self.tx.draw_ui(cv, x, ty, &count, dim, r, false);
        if f.show_replace {
            field(
                cv,
                &mut self.tx,
                r.y + 43,
                &f.replace.text,
                "Substituir",
                f.replace.cursor,
                f.focused && f.in_replace,
                false,
                f.replace.sel_all,
            );
            self.tx.draw_ui(cv, r.x + 8 + field_w + 10, r.y + 43 + 5, "Enter · Ctrl+Alt+Enter all", dim, r, false);
        }
    }

    pub fn draw_overlay(&mut self, cv: &mut Canvas) {
        self.layout.overlay_rows.clear();
        let c = self.theme.chrome;
        let fg = self.theme.foreground;
        let dim = c.tab_inactive_fg;
        let base = self.picker_rect();
        self.regions.modal_from = self.regions.list.len();
        // (list area, total rows, scroll row) of the picker: its scrollbar is added after the borrow ends.
        let mut picker_bar: Option<(Rect, f64, f64)> = None;

        match &self.overlay {
            Overlay::None => {}
            Overlay::Settings(_) => self.draw_settings(cv),
            Overlay::TopMenu(_) => self.draw_topmenu(cv),
            Overlay::Ctx(_) => self.draw_ctxmenu(cv),
            Overlay::Prompt(p) => {
                let r = Rect::new(base.x, base.y, base.w, 86);
                cv.shadow(r, 12);
                cv.card(r, 12, c.widget_bg, c.border);
                self.tx.draw_ui(cv, r.x + 16, r.y + 12, &p.title, dim, r, false);
                let input = Rect::new(r.x + 12, r.y + 38, r.w - 24, 34);
                cv.card(input, 8, c.input_bg, c.accent);
                if p.field.sel_all {
                    let w = self.tx.ui_width(&p.field.text, false);
                    cv.fill_round(Rect::new(input.x + 8, input.y + 6, w + 4, 22), 4, c.accent.with_alpha(0x50));
                }
                self.tx.draw_ui(cv, input.x + 10, input.y + 8, &p.field.text, fg, input.inset(1), false);
                let prefix: String = p.field.text.chars().take(p.field.cursor).collect();
                let x = input.x + 10 + self.tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, input.y + 8, 1, 18), fg, input);
            }
            Overlay::Picker(p) => {
                let shown = p.filtered.len().min(PICKER_ROWS);
                let list_h = if shown == 0 { 44 } else { shown as i32 * ROW + 14 };
                let r = Rect::new(base.x, base.y, base.w, INPUT_H + list_h);
                cv.shadow(r, 16);
                cv.card(r, 16, c.widget_bg, c.border);
                // Input row.
                self.tx.icon(cv, "hugeicons/search", r.x + 18, r.y + (INPUT_H - 16) / 2, 16, dim, r);
                let (shown_text, col) =
                    if p.field.text.is_empty() { (p.title.as_str(), dim) } else { (p.field.text.as_str(), fg) };
                if p.field.sel_all {
                    let w = self.tx.ui_width(&p.field.text, false);
                    cv.fill_round(
                        Rect::new(r.x + 44, r.y + (INPUT_H - 24) / 2, w + 4, 24),
                        4,
                        c.accent.with_alpha(0x50),
                    );
                }
                self.tx.draw_ui(cv, r.x + 46, r.y + (INPUT_H - 18) / 2, shown_text, col, r, false);
                if p.kind == PickKind::ProjectSearch {
                    let tag = format!("{}{}", if p.case { "Aa  " } else { "" }, if p.regex { ".*" } else { "" });
                    let tw = self.tx.ui_width(&tag, true);
                    self.tx.draw_ui(cv, r.right() - tw - 16, r.y + (INPUT_H - 18) / 2, &tag, c.accent_text, r, true);
                }
                let prefix: String = p.field.text.chars().take(p.field.cursor).collect();
                let x = r.x + 46 + self.tx.ui_width(&prefix, false);
                cv.fill_clip(Rect::new(x, r.y + (INPUT_H - 20) / 2, 1, 20), fg, r);
                cv.fill(Rect::new(r.x + 16, r.y + INPUT_H - 1, r.w - 32, 1), c.border);

                let mut rows = Vec::new();
                // Mid-glide the list sits a fraction of a row past `p.scroll`: rows shift up by `off`
                // px, one more row shows at the bottom and everything is clipped to the list.
                let off = self.region_off(crate::scrollreg::Reg::Picker, ROW);
                let lclip = Rect::new(r.x, r.y + INPUT_H, r.w, list_h);
                for (k, (idx, pos)) in
                    p.filtered.iter().enumerate().skip(p.scroll).take(PICKER_ROWS + (off > 0) as usize)
                {
                    let it = &p.items[*idx];
                    let y = r.y + INPUT_H + 6 + (k - p.scroll) as i32 * ROW - off;
                    let row_full = Rect::new(r.x + 8, y, r.w - 16, ROW);
                    let rr = row_full.intersect(&lclip);
                    if k == p.sel {
                        cv.fill_round_clip(row_full, 10, c.list_active_bg, lclip);
                    }
                    let avail = rr.w - 28;
                    let detail_w = if matches!(p.kind, PickKind::Commands | PickKind::Themes | PickKind::Choice)
                        && !it.detail.is_empty()
                    {
                        self.tx.ui_width(&it.detail, false) + 20
                    } else {
                        0
                    };
                    // Label, shortened from the left when it does not fit (paths), matches highlighted.
                    let label: Vec<char> = it.label.chars().collect();
                    let mut skip = 0;
                    let budget = avail - detail_w;
                    let max_label =
                        if it.detail.is_empty() || p.kind == PickKind::Commands { budget } else { budget * 3 / 5 };
                    while skip < label.len()
                        && self.tx.ui_width(&label[skip..].iter().collect::<String>(), false) > max_label
                    {
                        skip += (label.len() - skip).div_ceil(8).max(1);
                    }
                    let dir_end = if p.kind == PickKind::QuickOpen {
                        label.iter().rposition(|ch| *ch == '/').map_or(0, |i| i + 1)
                    } else {
                        0
                    };
                    // Match colour: the accent when it has a hue of its own, else the plain text colour.
                    let chroma = c.accent_text.0.max(c.accent_text.1).max(c.accent_text.2)
                        - c.accent_text.0.min(c.accent_text.1).min(c.accent_text.2);
                    let mcol = if chroma >= 40 { c.accent_text } else { fg };
                    let mut pieces: Vec<(String, crate::theme::Rgba)> = Vec::new();
                    if skip > 0 {
                        pieces.push(("…".to_string(), dim));
                    }
                    for (i, ch) in label.iter().enumerate().skip(skip) {
                        let col = if pos.contains(&i) {
                            mcol
                        } else if i < dir_end {
                            dim
                        } else {
                            fg
                        };
                        match pieces.last_mut() {
                            Some((s, pc)) if *pc == col => s.push(*ch),
                            _ => pieces.push((ch.to_string(), col)),
                        }
                    }
                    let ly = y + (ROW - 18) / 2;
                    self.tx.draw_ui_pieces(cv, rr.x + 14, ly, &pieces, rr, false);
                    // Matched letters: an underline in the text colour on top of the accent colour, so
                    // they stand out on the selected / hovered row without relying on colour alone.
                    let mut i = skip;
                    while i < label.len() {
                        if !pos.contains(&i) {
                            i += 1;
                            continue;
                        }
                        let mut j = i;
                        while j < label.len() && pos.contains(&j) {
                            j += 1;
                        }
                        let pre: String = std::iter::repeat_n('…', (skip > 0) as usize)
                            .chain(label[skip..i].iter().copied())
                            .collect();
                        let run: String = label[i..j].iter().collect();
                        let x0 = rr.x + 14 + self.tx.ui_width(&pre, false);
                        cv.fill_clip(Rect::new(x0, ly + 17, self.tx.ui_width(&run, false), 1), mcol, rr);
                        i = j;
                    }
                    if !it.detail.is_empty() {
                        if p.kind == PickKind::Commands {
                            let dx = rr.right() - detail_w + 8;
                            self.tx.draw_ui(cv, dx, ly, &it.detail, dim, rr, false);
                        } else {
                            let used =
                                self.tx.ui_width(&pieces.iter().map(|(s, _)| s.as_str()).collect::<String>(), false);
                            let dx = rr.x + 14 + used + 16;
                            let room = rr.right() - dx - 10;
                            let mut det = it.detail.clone();
                            while self.tx.ui_width(&det, false) > room && det.chars().count() > 3 {
                                det.remove(0);
                            }
                            self.tx.draw_ui(cv, dx, ly, &det, dim, rr, false);
                        }
                    }
                    rows.push((rr, k));
                }
                self.layout.overlay_rows = rows;
                if p.filtered.len() > PICKER_ROWS {
                    picker_bar =
                        Some((Rect::new(r.x, r.y + INPUT_H, r.w, list_h), p.filtered.len() as f64, p.scroll as f64));
                }
                if p.filtered.is_empty() {
                    let msg = if p.kind == PickKind::ProjectSearch && p.field.text.is_empty() {
                        "Type to search the project"
                    } else {
                        "No results"
                    };
                    self.tx.draw_ui(cv, r.x + 16, r.y + INPUT_H + 10, msg, dim, r, false);
                }
            }
        }
        if let Some((area, total, pos)) = picker_bar {
            self.region_bar(
                cv,
                crate::scrollreg::Reg::Picker,
                area.inset(0),
                total,
                PICKER_ROWS as f64,
                pos,
                ROW as f64,
                true,
            );
        }
    }
}
