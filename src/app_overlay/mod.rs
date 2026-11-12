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

mod commands;
mod draw;
mod find;
mod input;
mod open;
pub mod preplace;
mod run;

pub use commands::*;

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
    Theme(String),
    UndoBranch(usize),
    GitShow(PathBuf, String, String),
    /// Local history of a file: one snapshot (its file name) to compare, or `None` = list them.
    LocalHist(PathBuf, Option<String>),
    /// Branch / tag / release / cherry-pick pickers (see `app_gitpanel::actions`).
    Git(crate::app_gitpanel::GitPick),
    /// A macro saved in `macros.json` (see `macros`).
    Macro(String),
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
    /// Project search: whole words only (Alt+W).
    pub word: bool,
    /// Project search: the replace row (Ctrl+Shift+H) and its state.
    pub rep: Option<Box<preplace::ReplaceUi>>,
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
}

pub struct Prompt {
    pub kind: PromptKind,
    pub title: String,
    pub field: TextField,
    /// New file: icon of the typed name, resolved once typing pauses (`icon_due`).
    pub icon: Option<String>,
    pub icon_due: Option<Instant>,
}

/// How long the new-file prompt waits after the last keystroke before resolving the type icon.
pub const PROMPT_ICON_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

impl Prompt {
    pub fn new(kind: PromptKind, title: String, field: TextField) -> Prompt {
        Prompt { kind, title, field, icon: None, icon_due: None }
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
            word: false,
            rep: None,
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
            self.filtered = (0..self.items.len().min(search::MAX_REPLACE_HITS)).map(|i| (i, Vec::new())).collect();
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
