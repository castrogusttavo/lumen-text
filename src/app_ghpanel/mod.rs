//! GitHub page, next to the Git page (the "Git | GitHub | AI" switch on top): the pull requests,
//! issues, workflow runs and notifications of the project's repository, through the `gh` CLI
//! (`crate::ghub`), after Athas' `github_panel`.
//!
//! * a list column (section tabs, repository, filter box + filter menu, groups with counts) and
//!   the detail beside it (a narrow window shows one or the other, with a Back button);
//! * a pull request: status, review and merge badges, description, labels, reviewers, activity
//!   (comments and reviews), a comment box, Files (with the diff of a file), Commits, Checks;
//!   Merge (merge / squash / rebase), Approve, Request changes, Close / Reopen, Checkout;
//! * an issue: description, activity, comment, Close / Close as not planned / Reopen;
//! * a workflow run: jobs and steps with status and duration, the log of a job (filter, failed
//!   steps only, errors and warnings marked), Re-run (all / failed), Cancel;
//! * notifications: unread first, a click opens the pull request / issue or marks it read.
//!
//! Every call runs on a worker thread (`state::GhPanel::spawn`); lists and details are cached per
//! filter / number, shown at once when the page opens again and refreshed in the background.
//! Without `gh`, without a login or without a GitHub remote the page says so and how to fix it.
//!
//! Modules: `state` (model, workers), `md` (the Markdown of descriptions and comments, plain
//! code), `items` (what the detail shows, plain code), `draw` / `draw_detail` / `draw_log` and
//! `input` (App glue).

use crate::{
    app::{App, KeyIn},
    app_gitpanel::fit,
    field::TextField,
    ghub::{self, IssueFilter, PrFilter},
    ghub_model::{self as model, RunFilter, Tone},
    textarea::{KeyAct, TextArea},
    theme::Rgba,
    ui::{Canvas, Rect},
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::{
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

mod draw;
mod draw_detail;
mod draw_log;
mod input;
mod items;
mod md;
mod state;
#[cfg(test)]
mod tests;

pub use state::*;

/// Seconds since the epoch.
pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
