//! Git panel docked on the right: two tabs.
//!   * "Changes": changed files grouped as Staged / Unstaged / Untracked (checkbox =
//!     stage/unstage, click = side-by-side diff against HEAD, trash = discard with a second click
//!     to confirm), Ctrl / Shift + click to select files, a multi-line commit message box
//!     (Ctrl+Enter commits), and the stashes (stash all / the selected files, apply, pop, drop,
//!     and their files with the side-by-side diff).
//!   * "History": the commit graph (lanes, merges, branch / tag / HEAD labels) of every branch,
//!     streamed a page at a time from a worker thread and drawn row by row for the visible part
//!     only; a click on a commit expands its files and shows the first file's diff, a right click
//!     opens the commit menu (checkout, branch / tag here, cherry-pick, revert, copy hash).
//!
//! Modules: `logic`, `refs`, `stash` and `graph` (parsers, git calls, lane assignment) are plain
//! code covered by unit tests against real temporary repositories; the message box is a
//! `crate::textarea::TextArea`; `state` and `history` are the panel model; `ops`, `input`,
//! `draw`, `draw_graph`, `actions` and `stash_ui` are the App glue.

use crate::{
    app::{App, KeyIn},
    app_gitx::DiffView,
    field::TextField,
    git,
    gitsync::{self, Fail, FailKind, Job, Op, PullStrategy, SyncInfo},
    gitx::{self, Side},
    textarea::{KeyAct, TextArea},
    theme::Rgba,
    ui::{Canvas, Rect, TextCtx},
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

mod actions;
mod draw;
mod draw_graph;
mod graph;
mod history;
mod input;
mod logic;
mod ops;
mod refs;
mod stash;
mod stash_ui;
mod state;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_graph;

pub use actions::*;
pub use graph::*;
pub use history::*;
pub use logic::*;
pub use refs::*;
pub use stash::*;
pub use stash_ui::*;
pub use state::*;
