//! A native chat tab: the conversation with one agent CLI driven through its structured mode
//! (`backend`), the model it fills (`chat`) and the little state the view needs (composer text,
//! scroll, laid-out rows). The drawing and the input live in `chat_ui`. Nothing here draws or
//! polls: the CLI's reader threads wake the event loop, and the rows are laid out again only when
//! the conversation or the width changed, so an idle chat costs nothing.

use super::{
    backend::{Backend, Kind, Spawn},
    chat::{Chat, PermMode, Phase},
    menu::Menu,
    slash::Cmd,
    state::Status,
};
use crate::{textarea::TextArea, ui::Rect};
use std::time::Instant;

/// What can be clicked in a chat.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatHit {
    Input,
    Send,
    Stop,
    /// The permission-mode pill (Claude Code): click cycles it.
    Mode,
    /// Allow / Deny the permission request `n` of `chat.perms`.
    Allow(usize),
    Deny(usize),
    /// The model label: opens the model dropdown.
    Model,
    /// The effort label: opens the effort dropdown.
    Effort,
    /// Row `n` of the open dropdown.
    DropItem(usize),
    /// Row `n` of the `/` or `@` menu above the composer.
    Pick(usize),
}

/// What a laid-out row of the transcript is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    /// The user's message, in a bubble on the right.
    User,
    /// The agent's words.
    Text,
    /// A tool call: icon, verb, summary, status.
    Tool,
    Thinking,
    Error,
    Note,
    /// A permission request with its Allow / Deny.
    Perm,
    /// "Working for 4s" while the turn runs.
    Working,
}

/// One block of the transcript with its lines wrapped for the width it was laid out at.
#[derive(Clone, Debug)]
pub struct Row {
    pub kind: RowKind,
    pub lines: Vec<String>,
    /// For a tool: the tool call's index in `chat.messages` flattened order (its status is read live).
    pub tool: Option<String>,
    pub y: i32,
    pub h: i32,
}

/// Which setting a dropdown changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropKind {
    Model,
    Effort,
    Mode,
}

#[derive(Clone, Debug)]
pub struct DropItem {
    pub id: String,
    pub label: String,
    pub detail: String,
    /// The value in use.
    pub on: bool,
}

/// A small list that opens right over its button in the composer's toolbar.
#[derive(Clone, Debug)]
pub struct Dropdown {
    pub kind: DropKind,
    pub items: Vec<DropItem>,
    pub sel: usize,
}

/// The reasoning levels an agent takes (`--effort` of Claude Code, `model_reasoning_effort` of Codex).
pub fn efforts(kind: Kind) -> Vec<(&'static str, &'static str)> {
    match kind {
        Kind::Claude => vec![
            ("low", "Fastest, the least reasoning"),
            ("medium", "Balanced"),
            ("high", "More reasoning"),
            ("xhigh", "Extra deep reasoning"),
            ("max", "The most reasoning"),
        ],
        Kind::Codex => vec![("low", "Fastest"), ("medium", "Balanced"), ("high", "The most reasoning")],
        Kind::Gemini => Vec::new(),
    }
}

pub struct ChatTab {
    pub chat: Chat,
    pub backend: Backend,
    pub input: TextArea,
    /// Keys go to the composer.
    pub focused: bool,
    /// Pixels scrolled from the top of the transcript, and whether it sticks to the newest row.
    pub scroll: f64,
    pub follow: bool,
    pub max_scroll: f64,
    pub rows: Vec<Row>,
    /// What `rows` was laid out for: the chat revision and the width.
    pub laid: Option<(u64, i32)>,
    pub total_h: i32,
    pub hits: Vec<(Rect, ChatHit)>,
    /// The composer box and the transcript area as drawn last.
    pub input_box: Rect,
    pub view: Rect,
    pub started: Instant,
    /// The `/` or `@` menu open above the composer.
    pub menu: Option<Menu>,
    /// The slash commands of this agent, and how many the agent had reported when it was built.
    pub catalog: Vec<Cmd>,
    catalog_for: usize,
    /// The project's files (relative paths), listed the first time a `@` is typed.
    pub files: Option<Vec<String>>,
    /// The toolbar's dropdown while one is open, and where its buttons were drawn (model, effort,
    /// mode) so it opens over the one clicked.
    pub dropdown: Option<Dropdown>,
    pub anchors: [Rect; 3],
}

impl ChatTab {
    pub fn new(kind: Kind, sp: Spawn, wake: super::backend::Wake) -> ChatTab {
        ChatTab {
            chat: Chat::new(),
            backend: Backend::new(kind, sp, wake),
            input: TextArea::default(),
            focused: true,
            scroll: 0.0,
            follow: true,
            max_scroll: 0.0,
            rows: Vec::new(),
            laid: None,
            total_h: 0,
            hits: Vec::new(),
            input_box: Rect::default(),
            view: Rect::default(),
            started: Instant::now(),
            menu: None,
            catalog: Vec::new(),
            catalog_for: usize::MAX,
            files: None,
            dropdown: None,
            anchors: [Rect::default(); 3],
        }
    }

    /// The commands of this agent (Lumen's, the project's, the ones the agent reported); built
    /// again when the agent reports more.
    pub fn commands(&mut self, root: &std::path::Path) -> &[Cmd] {
        if self.catalog_for != self.chat.commands.len() {
            let home = crate::paths::home();
            let found = super::slash::discover(root, home.as_deref());
            let meta = super::slash::Meta {
                commands: self.chat.commands.clone(),
                terminal_only: self.chat.terminal_only.clone(),
            };
            self.catalog = super::slash::catalog(self.backend.kind, found, &meta);
            self.catalog_for = self.chat.commands.len();
        }
        &self.catalog
    }

    /// The model in use: the one the agent reported, else the one picked.
    pub fn model_now(&self) -> Option<String> {
        self.chat.model.clone().or_else(|| self.backend.model.clone())
    }

    /// Open the dropdown of `kind` (or close it when it is the one open).
    pub fn open_dropdown(&mut self, kind: DropKind) {
        if self.dropdown.as_ref().is_some_and(|d| d.kind == kind) {
            self.dropdown = None;
            return;
        }
        let items: Vec<DropItem> = match kind {
            DropKind::Model => {
                let cur = self.model_now();
                super::slash::models(self.backend.kind)
                    .into_iter()
                    .map(|(id, desc)| DropItem {
                        id: id.to_string(),
                        label: id.to_string(),
                        detail: desc.to_string(),
                        on: cur.as_deref().is_some_and(|c| c == id || c.contains(id)),
                    })
                    .collect()
            }
            DropKind::Effort => efforts(self.backend.kind)
                .into_iter()
                .map(|(id, desc)| DropItem {
                    id: id.to_string(),
                    label: id.to_string(),
                    detail: desc.to_string(),
                    on: self.backend.effort.as_deref() == Some(id),
                })
                .collect(),
            DropKind::Mode => PermMode::ALL
                .into_iter()
                .map(|m| DropItem {
                    id: m.id().to_string(),
                    label: m.label().to_string(),
                    detail: m.detail().to_string(),
                    on: self.backend.mode == m,
                })
                .collect(),
        };
        if items.is_empty() {
            return;
        }
        let sel = items.iter().position(|i| i.on).unwrap_or(0);
        self.dropdown = Some(Dropdown { kind, items, sel });
    }

    /// Apply row `i` of the open dropdown and close it.
    pub fn pick_dropdown(&mut self, i: usize) {
        let Some(d) = self.dropdown.take() else { return };
        let Some(item) = d.items.get(i) else { return };
        match d.kind {
            DropKind::Model => self.backend.set_model(&mut self.chat, &item.id),
            DropKind::Effort => self.backend.set_effort(&mut self.chat, &item.id),
            DropKind::Mode => {
                if let Some(m) = PermMode::from_id(&item.id) {
                    self.backend.set_mode(&mut self.chat, m);
                }
            }
        }
    }

    /// Fold what the CLI printed into the conversation; true when something changed.
    pub fn pump(&mut self) -> bool {
        self.backend.pump(&mut self.chat)
    }

    /// Time-based work (an interrupt the CLI did not obey); when to look again.
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.backend.tick(&mut self.chat, now)
    }

    /// The tab's status, from the turn: working, asking for a permission, or idle.
    pub fn phase_status(&self, worked: bool) -> Status {
        match self.chat.phase {
            Phase::Working => Status::Working,
            Phase::Waiting => Status::Waiting,
            Phase::Idle if worked => Status::Done,
            Phase::Idle => Status::Waiting,
        }
    }

    /// Send the composer's text (a message while the agent works waits its turn: only when idle).
    pub fn submit(&mut self) -> Option<String> {
        let text = self.input.text().trim().to_string();
        if text.is_empty() || self.chat.working() {
            return None;
        }
        self.input.set_text("");
        self.follow = true;
        self.backend.send(&mut self.chat, &text);
        Some(text)
    }

    pub fn hit_at(&self, x: f64, y: f64) -> Option<ChatHit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }
}
