//! Git panel docked on the right: two tabs.
//!   * "Changes": changed files grouped as Staged / Unstaged / Untracked (checkbox =
//!     stage/unstage, click = side-by-side diff against HEAD, trash = discard with a second click
//!     to confirm) and a multi-line commit message box (Ctrl+Enter commits).
//!   * "History": `git log` loaded a page at a time while scrolling; a click on a commit
//!     expands its files and shows the first file's diff.
//!
//! Modules: `logic` (parsers, git calls) and `msg` (message editing) are plain code covered by unit tests
//! against real temporary repositories; `state` is the panel model; `ops`, `input` and `draw` are the App glue.

use crate::{
    app::{App, KeyIn},
    app_gitx::DiffView,
    field::TextField,
    git,
    gitsync::{self, Fail, FailKind, Job, Op, PullStrategy, SyncInfo},
    gitx::{self, Side},
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

mod draw;
mod input;
mod logic;
mod msg;
mod ops;
mod state;
#[cfg(test)]
mod tests;

pub use logic::*;
pub use msg::*;
pub use state::*;
