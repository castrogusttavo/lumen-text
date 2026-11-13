//! The hover popup, VS Code / Zed style: rendered Markdown (highlighted code, JSDoc sections),
//! the full declaration of interfaces / classes / enums / type aliases, diagnostics with their
//! TypeScript code, a scrollable body that the pointer may enter, and action links in the footer.
//!
//! Pipeline (nothing blocks the UI):
//! 1. `textDocument/hover` answers -> [`App::hover_response`] parses the Markdown ([`crate::hover_md`]),
//!    builds a [`HoverTip`] and shows it at once;
//! 2. in parallel `definition` / `typeDefinition` (see [`HoverWhat`]) are asked; when they answer,
//!    [`App::hover_extra_response`] cuts the declaration out of the source file (open buffer or
//!    disk) and the tip is recomposed in place (scroll position kept);
//! 3. tips are cached per (document version, anchor range), so moving the pointer inside the same
//!    symbol never asks again and never blinks.
//!
//! Layout ([`layout`]) is pure text-metrics work done once per (tip, width) and drawn clipped to
//! the scroll viewport; geometry ([`place`]) keeps the popup inside the editor area.

use crate::{
    app::{App, KeyIn},
    doc::{Diag, uri_to_path},
    hover_md::{self as md, Blk, JsTag, Kind, ListItem, Span},
    lsp::{self, DefKind, Pending},
    scroller::{self, Scroller},
    theme::{Rgba, Theme},
    ui::{Canvas, Rect, TextCtx, VBar, font_size, line_h},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use winit::{
    event::{MouseButton, MouseScrollDelta},
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

mod draw;
mod input;
mod layout;
mod response;
mod select;
#[cfg(test)]
mod tests;
mod text;
mod tip;

pub use layout::*;
pub use select::*;
pub use text::*;

const PAD_X: i32 = 12;
const PAD_Y: i32 = 8;
const GAP: i32 = 7;
/// Prose line height: 19 px at the default UI line height, scaled by the `ui_line_height` setting.
fn prose_lh() -> i32 {
    (19.0 * crate::fontcfg::prose_scale()).round() as i32
}
const LABEL_LH: i32 = 15;
const CODE_PAD: i32 = 7;
const BAR_W: i32 = 12;
const FOOT_ROW: i32 = 22;
const HIDE_DELAY: Duration = Duration::from_millis(280);
/// Declarations longer than this are cut with a "mais N linhas" line.
pub const DECL_LINES: usize = 40;
const CACHE_MAX: usize = 24;

// ───────────────────────────── state ─────────────────────────────

/// What a follow-up request of the hover pipeline was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverWhat {
    Def,
    TypeDef,
    Module,
    KeyType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Definition,
    TypeDefinition,
    References,
    CopyType,
    QuickFix,
    /// Ask the agent of the AI panel to explain / fix the diagnostic.
    ExplainAi,
    FixAi,
}

impl Act {
    fn label(self) -> &'static str {
        match self {
            Act::Definition => "Go to definition",
            Act::TypeDefinition => "Go to type definition",
            Act::References => "References",
            Act::CopyType => "Copy type",
            Act::QuickFix => "Quick actions (Ctrl+.)",
            Act::ExplainAi => "Explain with AI",
            Act::FixAi => "Fix with AI",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Link(String),
    Act(Act),
}

#[derive(Clone, Debug, Default)]
pub struct DiagInfo {
    pub sev: u8,
    pub msg: String,
    pub code: String,
    pub url: Option<String>,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Debug)]
pub struct Decl {
    pub text: String,
    pub more: usize,
    pub kind: Kind,
}

#[derive(Clone, Debug)]
pub struct Origin {
    pub path: std::path::PathBuf,
    pub label: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ModInfo {
    pub spec: String,
    pub file: Option<String>,
    pub pkg: Option<String>,
}

/// Everything learned after the first answer.
#[derive(Clone, Debug, Default)]
pub struct Extra {
    pub diags: Vec<DiagInfo>,
    pub decl: Option<Decl>,
    /// `(label, declaration)`: "Type User", "Class Repo".
    pub type_decl: Option<(String, Decl)>,
    pub overloads: Vec<String>,
    pub value: Option<String>,
    /// Source of a declaration whose hover type was cut with `...`.
    pub snippet: Option<(String, usize)>,
    pub module: Option<ModInfo>,
    pub origin: Option<Origin>,
    /// Hover Markdown of the property a literal value belongs to.
    pub key_md: Option<String>,
    /// `("\"Ana\"", "string · 3 caracteres")` for a hovered literal.
    pub literal: Option<(String, String)>,
}

/// One laid-out run of a line.
#[derive(Clone, Debug)]
pub struct Seg {
    pub x: i32,
    pub w: i32,
    pub text: String,
    pub color: Rgba,
    pub bold: bool,
    /// Synthetic slant (the UI font has no italic face).
    pub italic: bool,
    pub mono: bool,
    pub strike: bool,
    pub link: Option<usize>,
    pub size: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct LLine {
    pub y: i32,
    pub h: i32,
    pub segs: Vec<Seg>,
    /// Monospace runs (code) drawn at `x0`.
    pub x0: i32,
    pub runs: Vec<(String, Rgba, u8)>,
    pub plain: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DecoKind {
    Fill(Rgba, i32),
    Rule(Rgba),
}

#[derive(Clone, Copy, Debug)]
pub struct Deco {
    pub x: i32,
    pub y: i32,
    /// 0 = up to the right edge of the content.
    pub w: i32,
    pub h: i32,
    pub kind: DecoKind,
}

#[derive(Clone, Debug, Default)]
pub struct Laid {
    pub w: i32,
    pub h: i32,
    pub lines: Vec<LLine>,
    pub decos: Vec<Deco>,
    pub urls: Vec<String>,
    pub links: Vec<(Rect, usize)>,
}

#[derive(Clone)]
pub struct HoverTip {
    /// Screen position (top-left) of the first char of the anchor.
    pub at: (i32, i32),
    pub anchor: (usize, usize),
    pub serial: u64,
    pub uri: String,
    pub ver: i32,
    pub kind: Kind,
    pub name: String,
    /// The first code block of the language server's answer ("Copy type").
    pub sig: String,
    head: Vec<Blk>,
    doc: md::JsDoc,
    pub extra: Extra,
    pub blocks: Vec<Blk>,
    blocks_ver: u64,
    laid: Option<(LayKey, Laid)>,
    pub scroll: f64,
    /// Glide of the wheel / touchpad over `scroll`.
    scr: Scroller,
    /// Selected text: (anchor, head) as (line, char) points, in any order.
    pub sel: Option<(SelPt, SelPt)>,
    /// Per-char geometry of the lines the selection touches (valid for the layout key).
    cells: (Option<LayKey>, std::collections::HashMap<usize, Vec<CharBox>>),
    pub side_below: Option<bool>,
    // geometry of the last draw (screen coordinates)
    pub rect: Rect,
    pub view: Rect,
    pub bar: Option<VBar>,
    pub hits: Vec<(Rect, Hit)>,
    pub has_lsp: bool,
}

type LayKey = (i32, u32, u32, u32, u64, u32);

/// A point of the text selection: (index into `Laid::lines`, char boundary inside that line).
pub type SelPt = (usize, usize);

/// One visible char of a laid-out line with its horizontal extent (relative to the content origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharBox {
    pub ch: char,
    pub x0: i32,
    pub x1: i32,
}

#[derive(Default)]
pub struct HoverCtl {
    pub serial: u64,
    pub hide_at: Option<Instant>,
    cache: Vec<HoverTip>,
    drag_bar: Option<f64>,
    drag_sel: Option<SelPt>,
    hot: Option<Hit>,
}
