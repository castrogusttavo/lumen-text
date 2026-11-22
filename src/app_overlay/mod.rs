//! Overlays: command/file palette, find & replace bar, input prompts.

use crate::{
    app::{App, Focus, KeyIn},
    buffer::EditKind,
    field::TextField,
    fuzzy,
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

mod commands;
mod draw;
mod find;
mod groups;
mod input;
mod open;
pub mod preplace;
mod run;

pub use commands::*;
pub use groups::age_bucket;

#[derive(Clone)]
pub enum Action {
    Open(PathBuf, Option<(usize, usize)>),
    Cmd(Cmd),
    Goto(usize),
    /// Bottom bar pickers: indentation of the current document (see `app_topbar::indent_from_spec`).
    SetIndent(String),
    /// Language of the current document (`None` = plain text).
    SetLang(Option<String>),
    /// Encoding pickers of the current document (see `app_filefmt::encoding_action`).
    Encoding(String),
    WorkspaceEdit(Value),
    Run(String),
    TestRunner(crate::testrun::Runner),
    /// Tests panel target: a package.json script (its id, see `testrun::ScriptTarget`).
    TestTarget(String),
    /// Build panel target (its id, see `buildrun::Target`).
    BuildTarget(String),
    /// AI panel agent (its id, see `app_aipanel::AGENTS`).
    AiAgent(String),
    /// AI panel: a new session of this agent.
    AiNew(String),
    Theme(String),
    UndoBranch(usize),
    GitShow(PathBuf, String, String),
    /// Local history of a file: one snapshot (its file name) to compare, or `None` = list them.
    LocalHist(PathBuf, Option<String>),
    /// Branch / tag / release / cherry-pick pickers (see `app_gitpanel::actions`).
    Git(crate::app_gitpanel::GitPick),
    /// A macro saved in `macros.json` (see `macros`).
    Macro(String),
    /// Launch configuration picked for debugging (its id, see `debug_model::LaunchConfig`).
    DebugConfig(String),
    /// "Compare with branch or commit" pickers (see `app_gitcmp`).
    Compare(crate::app_gitcmp::CmpPick),
    /// Trust prompt of the project settings and the Python interpreter picker (see `app_projcfg`).
    Project(crate::app_projcfg::ProjPick),
    /// Resource panel rows and their Stop / Restart menus (see `resmon`).
    Res(crate::resmon::ResPick),
    /// Workspace folder pickers (see `app_workspace`).
    Workspace(crate::app_workspace::WsPick),
    /// Snippet pickers (see `app_snippets`).
    Snippet(crate::app_snippets::SnipPick),
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
    Themes,
    /// Short list of choices opened from the bottom bar (indentation, language).
    Choice,
    /// The Problems panel (see `app_problems`): grouped rows and a scope switch.
    Problems,
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
    /// Project search: whole words only (Alt+W).
    pub word: bool,
    /// Project search: the replace row (Ctrl+Shift+H) and its state.
    pub rep: Option<Box<preplace::ReplaceUi>>,
    /// Accepts free text too: what was typed becomes one more item, made by this (see
    /// [`typed_picker`]); `real` is the number of items that are not that one.
    pub typed: Option<std::rc::Rc<dyn Fn(&str) -> Action>>,
    pub real: usize,
    /// Go to picker (quick open kind): mode, preview and search state, see `app_goto`.
    pub goto: Option<Box<crate::app_goto::GotoState>>,
    /// Go to picker: where the file rows come from (`items` then holds only the rows shown).
    pub files: Option<Box<crate::app_goto::FileSource>>,
    /// Group of every item (parallel to `items`) for the headers of an unfiltered list; pickers
    /// without it derive theirs (see `groups`).
    pub group_by: Option<Vec<String>>,
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
    /// Branch / tag / release prompts (see `app_gitpanel::actions`).
    Git(crate::app_gitpanel::GitAsk),
    /// "Save as" for an untitled document; `rest` (more documents to save) and `then` (the
    /// action that was waiting for the save) continue after it.
    SaveAs {
        doc: usize,
        rest: Vec<usize>,
        then: Option<crate::app_unsaved::Pending>,
    },
    /// New name for a bottom-panel terminal (see `app_terms`).
    RenameTerminal {
        id: u32,
    },
    /// "Macro: Replay…": how many times.
    MacroCount,
    /// "Macro: Save last as…": the name in `macros.json`.
    MacroName,
    /// The name of AI panel session `id`.
    AiRename {
        id: u64,
    },
    /// Condition of the breakpoint on `line` (0-based) of `path`; empty clears it.
    BpCondition {
        path: PathBuf,
        line: usize,
    },
    /// Log message of a logpoint on `line` (0-based) of `path`; empty turns it back into a breakpoint.
    BpLog {
        path: PathBuf,
        line: usize,
    },
    /// "Remote: Open folder over SSH…": the address (see `app_remote`).
    RemoteOpen,
    /// "Workspace: Add folder…", "Open workspace file…" and "Save as…" (see `app_workspace`).
    WorkspaceAdd,
    WorkspaceOpen,
    WorkspaceSave,
    /// "Database: Open SQLite file…" (see `app_sqlview`).
    SqliteOpen,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub title: String,
    pub field: TextField,
    /// New file: icon of the typed name, resolved once typing pauses (`icon_due`).
    pub icon: Option<String>,
    pub icon_due: Option<Instant>,
    /// Inline name field (explorer rename / new file / new folder): why the name was refused, and
    /// when the refusal started (the field shakes for [`SHAKE`]).
    pub error: Option<String>,
    pub shake: Option<Instant>,
}

/// How long a refused name shakes the inline field.
pub const SHAKE: std::time::Duration = std::time::Duration::from_millis(320);

/// How long the new-file prompt waits after the last keystroke before resolving the type icon.
pub const PROMPT_ICON_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

impl Prompt {
    pub fn new(kind: PromptKind, title: String, field: TextField) -> Prompt {
        Prompt { kind, title, field, icon: None, icon_due: None, error: None, shake: None }
    }

    /// Is the field still shaking after a refused name?
    pub fn shaking(&self) -> bool {
        self.shake.is_some_and(|t| t.elapsed() < SHAKE)
    }

    /// Horizontal offset of the shaking field in logical px (0 when settled or motion is reduced).
    pub fn shake_dx(&self) -> i32 {
        match self.shake {
            Some(t) if !crate::motion::reduced() => shake_offset(t.elapsed().as_secs_f32() / SHAKE.as_secs_f32()),
            _ => 0,
        }
    }

    /// Explorer name prompts (rename, new file, new folder): drawn as a field in the tree.
    pub fn is_inline(&self) -> bool {
        matches!(self.kind, PromptKind::NewFile { .. } | PromptKind::NewDir { .. } | PromptKind::RenamePath { .. })
    }

    /// Icon shown in the input: a folder for "New Folder", the type of the typed name for
    /// "New File" (a plain file until the name is resolved), none for the other prompts.
    pub fn input_icon(&self) -> Option<&str> {
        match self.kind {
            PromptKind::NewDir { .. } => Some("file/folder"),
            PromptKind::NewFile { .. } => {
                Some(self.icon.as_deref().filter(|_| !self.field.text.trim().is_empty()).unwrap_or("file/file"))
            }
            _ => None,
        }
    }

    /// The name whose type the new-file icon shows: last component of what was typed ("src/a.rs" -> "a.rs").
    pub fn icon_name(&self) -> &str {
        let t = self.field.text.trim();
        t.rsplit(['/', '\\']).next().unwrap_or(t)
    }
}

/// Offset of a shake at progress `k` (0..1): three damped swings of at most 6 px.
pub fn shake_offset(k: f32) -> i32 {
    if !(0.0..1.0).contains(&k) {
        return 0;
    }
    ((k * std::f32::consts::TAU * 3.0).sin() * 6.0 * (1.0 - k)).round() as i32
}

/// Why `text` cannot be the name asked by an explorer prompt (None = fine). Looks at the disk only
/// for names that would collide.
pub fn name_error(kind: &PromptKind, text: &str) -> Option<&'static str> {
    let name = text.trim();
    if name.is_empty() {
        return Some("Type a name");
    }
    if name.contains('\0') || name.split(['/', '\\']).any(|c| c == "..") {
        return Some("Not a valid name");
    }
    match kind {
        PromptKind::RenamePath { from } => {
            let to = from.parent()?.join(name);
            (to != *from && to.exists()).then_some("Already exists")
        }
        PromptKind::NewDir { dir } => dir.join(name.trim_start_matches('/')).exists().then_some("Already exists"),
        _ => None,
    }
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
    /// Font family picker (see `fontpicker`).
    Font(Box<crate::fontpicker::FontPicker>),
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

/// A filtering picker that also takes what is typed (a ref, an expression...): a “Use …” item made
/// by `make` comes first, unless some item starts with the typed text.
pub fn typed_picker(title: &str, items: Vec<Item>, make: std::rc::Rc<dyn Fn(&str) -> Action>) -> Picker {
    let mut p = Picker::new(PickKind::Choice, title, items);
    p.typed = Some(make);
    p.refilter();
    p
}

impl Picker {
    pub(crate) fn new(kind: PickKind, title: &str, items: Vec<Item>) -> Self {
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
            word: false,
            rep: None,
            typed: None,
            real: 0,
            goto: None,
            files: None,
            group_by: None,
        };
        p.real = p.items.len();
        p.refilter();
        p
    }

    pub fn refilter(&mut self) {
        let text = self.field.text.trim().to_string();
        // Quick open is the Go to picker: a prefix (`@`, `#`, `:`) picks the mode, the rest filters.
        let (gmode, q) = if self.kind == PickKind::QuickOpen {
            let (m, rest) = crate::gotopick::parse(&text);
            (Some(m), rest.trim().to_string())
        } else {
            (None, text)
        };
        if let Some(make) = self.typed.clone() {
            self.items.truncate(self.real);
            if !q.is_empty() {
                let action = make(&q);
                self.items.push(Item { label: format!("Use “{q}”"), detail: "as typed".into(), action });
            }
        }
        let filter = match self.kind {
            // Project symbols arrive ranked by the merge; a line row is not a text match.
            PickKind::QuickOpen => {
                !matches!(gmode, Some(crate::gotopick::Mode::WorkspaceSymbols | crate::gotopick::Mode::Line))
            }
            PickKind::Commands
            | PickKind::Symbols
            | PickKind::CodeActions
            | PickKind::Themes
            | PickKind::Choice
            | PickKind::Problems => true,
            _ => false,
        };
        if gmode == Some(crate::gotopick::Mode::Files)
            && let Some(src) = &self.files
        {
            // The file index is ranked where it is; rows exist only for what is shown.
            let (items, filtered) = src.rows(&q);
            (self.items, self.filtered) = (items, filtered);
            self.real = self.items.len();
        } else if gmode == Some(crate::gotopick::Mode::WorkspaceSymbols) {
            let prepared = fuzzy::Query::new(&q);
            self.filtered = self
                .items
                .iter()
                .enumerate()
                .take(search::MAX_REPLACE_HITS)
                .map(|(i, it)| {
                    (
                        i,
                        if q.is_empty() {
                            Vec::new()
                        } else {
                            prepared.matched(&it.label).map(|x| x.1).unwrap_or_default()
                        },
                    )
                })
                .collect();
        } else if !filter || q.is_empty() {
            self.filtered = (0..self.items.len().min(search::MAX_REPLACE_HITS)).map(|i| (i, Vec::new())).collect();
        } else {
            // Several words narrow the list: every term must match somewhere in the label.
            let items = &self.items;
            self.filtered = fuzzy::rank(&q, items.len(), |i| Some(items[i].label.as_str()), 300);
        }
        if self.typed.is_some() && !q.is_empty() {
            // The typed item: first, unless an item starts with what was typed (then last).
            let t = self.real;
            let entry = match self.filtered.iter().position(|(i, _)| *i == t) {
                Some(k) => self.filtered.remove(k),
                None => (t, Vec::new()),
            };
            if self.filtered.iter().any(|(i, _)| self.items[*i].label.starts_with(&q)) {
                self.filtered.push(entry);
            } else {
                self.filtered.insert(0, entry);
            }
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

    pub(crate) fn selected(&self) -> Option<Item> {
        self.filtered.get(self.sel).map(|(i, _)| self.items[*i].clone())
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn shake_swings_and_settles() {
        assert_eq!(shake_offset(0.0), 0);
        assert_eq!(shake_offset(1.0), 0);
        assert_eq!(shake_offset(-0.5), 0);
        assert!((0..40).map(|i| shake_offset(i as f32 / 40.0).abs()).max().unwrap() >= 3);
        assert!((0..40).all(|i| shake_offset(i as f32 / 40.0).abs() <= 6));
    }

    #[test]
    fn names_are_checked_before_the_disk_is_touched() {
        let dir = std::env::temp_dir().join(format!("lumen-name-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("taken")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let new_dir = PromptKind::NewDir { dir: dir.clone() };
        let new_file = PromptKind::NewFile { dir: dir.clone() };
        let rename = PromptKind::RenamePath { from: dir.join("a.txt") };
        assert_eq!(name_error(&new_file, "  "), Some("Type a name"));
        assert_eq!(name_error(&new_file, "../x"), Some("Not a valid name"));
        assert_eq!(name_error(&new_file, "src/ok.rs"), None);
        assert_eq!(name_error(&new_file, "a.txt"), None, "an existing file is opened, not refused");
        assert_eq!(name_error(&new_dir, "taken"), Some("Already exists"));
        assert_eq!(name_error(&new_dir, "fresh"), None);
        assert_eq!(name_error(&rename, "taken"), Some("Already exists"));
        assert_eq!(name_error(&rename, "a.txt"), None, "same name: nothing to do");
        assert_eq!(name_error(&rename, "b.txt"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
