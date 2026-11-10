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

const PAD_X: i32 = 12;
const PAD_Y: i32 = 8;
const GAP: i32 = 7;
const PROSE_LH: i32 = 19;
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
}

impl Act {
    fn label(self) -> &'static str {
        match self {
            Act::Definition => "Go to definition",
            Act::TypeDefinition => "Go to type definition",
            Act::References => "References",
            Act::CopyType => "Copy type",
            Act::QuickFix => "Quick actions (Ctrl+.)",
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

type LayKey = (i32, u32, u32, u32, u64);

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

// ───────────────────────────── composing ─────────────────────────────

impl HoverTip {
    fn new(md_text: &str, anchor: (usize, usize), at: (i32, i32)) -> HoverTip {
        let blocks = md::parse_markdown(md_text);
        let mut head = Vec::new();
        let mut rest = Vec::new();
        for b in blocks {
            if rest.is_empty() && matches!(b, Blk::Code { .. }) {
                head.push(b);
            } else {
                rest.push(b);
            }
        }
        let sig = head
            .iter()
            .filter_map(|b| if let Blk::Code { text, .. } = b { Some(text.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n");
        let (kind, name) = md::detect_kind(&sig);
        let mut t = HoverTip {
            at,
            anchor,
            serial: 0,
            uri: String::new(),
            ver: 0,
            kind,
            name,
            sig,
            head,
            doc: md::group_jsdoc(rest),
            extra: Extra::default(),
            blocks: vec![],
            blocks_ver: 0,
            laid: None,
            scroll: 0.0,
            scr: Scroller::new(0.3),
            sel: None,
            cells: (None, Default::default()),
            side_below: None,
            rect: Rect::default(),
            view: Rect::default(),
            bar: None,
            hits: vec![],
            has_lsp: !md_text.trim().is_empty(),
        };
        t.compose();
        t
    }

    /// A tip with only diagnostics (no language server answer).
    pub fn diag_only(diags: Vec<DiagInfo>, anchor: (usize, usize), at: (i32, i32)) -> HoverTip {
        let mut t = HoverTip::new("", anchor, at);
        t.extra.diags = diags;
        t.compose();
        t
    }

    /// A tip from Markdown the app made itself (signature help).
    pub fn from_markdown(text: &str, at: (i32, i32), pos: usize) -> HoverTip {
        HoverTip::new(text, (pos, pos), at)
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Plain text for "Copy type".
    pub fn type_text(&self) -> String {
        if let Some(d) = &self.extra.decl
            && matches!(d.kind, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias)
        {
            return d.text.clone();
        }
        self.sig.clone()
    }

    pub fn compose(&mut self) {
        let x = &self.extra;
        let mut out: Vec<Blk> = Vec::new();
        for d in &x.diags {
            out.push(Blk::Diag { sev: d.sev, msg: d.msg.clone(), code: d.code.clone(), url: d.url.clone() });
        }
        let dep = self.doc.tags.iter().find(|t| t.tag == "deprecated");
        if let Some(t) = dep {
            out.push(Blk::Note { sev: 2, title: "Deprecated".into(), body: t.body.clone() });
        }
        // The literal under the pointer, with the type of the property it belongs to.
        let key = x.key_md.as_deref().map(md::parse_markdown).unwrap_or_default();
        let key_doc = md::group_jsdoc(key.iter().filter(|b| !matches!(b, Blk::Code { .. })).cloned().collect());
        for b in key.iter().filter(|b| matches!(b, Blk::Code { .. })) {
            out.push(b.clone());
        }
        out.extend(key_doc.desc);
        if let Some((lit, what)) = &x.literal {
            out.push(Blk::Kv {
                key: "Value".into(),
                val: vec![Span::code(lit.clone()), Span::plain(format!("  {what}"))],
            });
        }

        // Signature: the full declaration when we have it.
        let full =
            x.decl.as_ref().filter(|d| matches!(d.kind, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias));
        if let Some(d) = full {
            out.push(Blk::Code { text: d.text.clone(), lang: "ts".into() });
            if d.more > 0 {
                out.push(Blk::Dim(vec![
                    Span { text: format!("… {} more lines — ", d.more), ..Default::default() },
                    Span { text: "view definition".into(), link: Some("lumen:more".into()), ..Default::default() },
                ]));
            }
        } else {
            for b in &self.head {
                match b {
                    Blk::Code { text, lang } if text.lines().count() > 30 => {
                        let (t, more) = md::truncate_lines(text, 30);
                        out.push(Blk::Code { text: t, lang: lang.clone() });
                        out.push(Blk::Dim(vec![Span::plain(format!("… {more} more lines"))]));
                    }
                    b => out.push(b.clone()),
                }
            }
        }
        if !x.overloads.is_empty() {
            out.push(Blk::Label(format!("Overloads ({})", x.overloads.len())));
            let shown: Vec<&str> = x.overloads.iter().take(8).map(String::as_str).collect();
            out.push(Blk::Code { text: shown.join("\n"), lang: "ts".into() });
            if x.overloads.len() > 8 {
                out.push(Blk::Dim(vec![Span::plain(format!("… and {} more", x.overloads.len() - 8))]));
            }
        }
        if let Some(v) = &x.value
            && !self.sig.trim_end().ends_with(v.as_str())
        {
            out.push(Blk::Kv { key: "Value".into(), val: vec![Span::code(v.clone())] });
        }
        if let Some((code, more)) = &x.snippet {
            out.push(Blk::Label("Declaration".into()));
            out.push(Blk::Code { text: code.clone(), lang: "ts".into() });
            if *more > 0 {
                out.push(Blk::Dim(vec![Span::plain(format!("… {more} more lines"))]));
            }
        }
        if let Some(m) = &x.module {
            out.push(Blk::Kv { key: "Module".into(), val: vec![Span::code(m.spec.clone())] });
            if let Some(f) = &m.file {
                out.push(Blk::Kv {
                    key: "File".into(),
                    val: vec![Span { text: f.clone(), link: x.origin.as_ref().map(origin_link), ..Default::default() }],
                });
            }
            if let Some(p) = &m.pkg {
                out.push(Blk::Kv { key: "Package".into(), val: vec![Span::plain(p.clone())] });
            }
        }

        out.extend(self.doc.desc.iter().cloned());

        // Parameters: the signature's, with the @param descriptions merged in.
        let fnlike = matches!(self.kind, Kind::Function | Kind::Method | Kind::Constructor);
        let sig = if fnlike { md::parse_sig(&self.sig) } else { None };
        let params: Vec<&JsTag> = self
            .doc
            .tags
            .iter()
            .filter(|t| matches!(t.tag.as_str(), "param" | "arg" | "argument" | "prop" | "property"))
            .collect();
        let mut items: Vec<Blk> = Vec::new();
        let mut used = vec![false; params.len()];
        if let Some(s) = &sig {
            for p in &s.params {
                let bare = p.name.trim_start_matches("...");
                let k = params.iter().position(|t| {
                    t.name.as_deref().map(|n| n.trim_start_matches("...").trim_matches(['[', ']'])) == Some(bare)
                });
                let mut body = vec![];
                let mut extra = vec![];
                if let Some(k) = k {
                    used[k] = true;
                    body = params[k].body.clone();
                    extra = params[k].extra.clone();
                }
                let mut head = p.name.clone();
                if p.optional && p.default.is_none() {
                    head.push('?');
                }
                if !p.ty.is_empty() {
                    head.push_str(": ");
                    head.push_str(&p.ty);
                }
                if let Some(d) = &p.default {
                    head.push_str(" = ");
                    head.push_str(d);
                }
                items.push(Blk::Item { head: Some(head), body, extra });
            }
        }
        for (k, t) in params.iter().enumerate() {
            if !used[k] {
                items.push(Blk::Item { head: t.name.clone(), body: t.body.clone(), extra: t.extra.clone() });
            }
        }
        if !items.is_empty() {
            out.push(Blk::Label("Parameters".into()));
            out.extend(items);
        }
        let tmpl: Vec<&JsTag> =
            self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "template" | "typeParam" | "typeparam")).collect();
        if !tmpl.is_empty() {
            out.push(Blk::Label("Type parameters".into()));
            for t in tmpl {
                out.push(Blk::Item { head: t.name.clone(), body: t.body.clone(), extra: t.extra.clone() });
            }
        }
        let rets: Vec<&JsTag> =
            self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "returns" | "return")).collect();
        let ret_ty = sig
            .as_ref()
            .and_then(|s| s.ret.clone())
            .filter(|r| (r != "void" || !rets.is_empty()) && self.kind != Kind::Constructor);
        if !rets.is_empty() || ret_ty.is_some() {
            out.push(Blk::Label("Returns".into()));
            if rets.is_empty() {
                out.push(Blk::Item { head: ret_ty.clone(), body: vec![], extra: vec![] });
            }
            for t in rets {
                out.push(Blk::Item { head: ret_ty.clone(), body: t.body.clone(), extra: t.extra.clone() });
            }
        }
        for t in self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "throws" | "exception")) {
            out.push(Blk::Label("Throws".into()));
            out.push(Blk::Item { head: t.name.clone(), body: t.body.clone(), extra: t.extra.clone() });
        }
        let ex: Vec<&JsTag> = self.doc.tags.iter().filter(|t| t.tag == "example").collect();
        if !ex.is_empty() {
            out.push(Blk::Label(if ex.len() > 1 { "Examples".into() } else { "Example".to_string() }));
            for t in ex {
                if !t.body.is_empty() {
                    // The title of an example (`@example Basic usage`) reads as a caption: italic.
                    out.push(Blk::Para(
                        t.body
                            .iter()
                            .cloned()
                            .map(|mut s| {
                                s.italic = true;
                                s
                            })
                            .collect(),
                    ));
                }
                out.extend(t.extra.iter().cloned());
            }
        }
        let see: Vec<&JsTag> = self.doc.tags.iter().filter(|t| matches!(t.tag.as_str(), "see" | "link")).collect();
        if !see.is_empty() {
            out.push(Blk::Label("See also".into()));
            for t in see {
                out.push(Blk::Para(t.body.clone()));
            }
        }
        for t in self.doc.tags.iter().filter(|t| {
            !matches!(
                t.tag.as_str(),
                "param"
                    | "arg"
                    | "argument"
                    | "prop"
                    | "property"
                    | "template"
                    | "typeParam"
                    | "typeparam"
                    | "returns"
                    | "return"
                    | "throws"
                    | "exception"
                    | "example"
                    | "see"
                    | "link"
                    | "deprecated"
            )
        }) {
            let mut val = t.body.clone();
            if let Some(n) = &t.name {
                val.insert(0, Span::code(n.clone()));
            }
            out.push(Blk::Kv { key: format!("@{}", t.tag), val });
            out.extend(t.extra.iter().cloned());
        }

        // The type of a variable / property / parameter, expanded.
        if let Some((name, d)) = &x.type_decl {
            out.push(Blk::Label(name.clone()));
            out.push(Blk::Code { text: d.text.clone(), lang: "ts".into() });
            if d.more > 0 {
                out.push(Blk::Dim(vec![
                    Span { text: format!("… {} more lines — ", d.more), ..Default::default() },
                    Span {
                        text: "view type definition".into(),
                        link: Some("lumen:more-type".into()),
                        ..Default::default()
                    },
                ]));
            }
        }
        if let Some(o) = &x.origin
            && x.module.is_none()
        {
            out.push(Blk::Kv {
                key: "Defined in".into(),
                val: vec![Span {
                    text: format!("{}:{}", o.label, o.line + 1),
                    link: Some(origin_link(o)),
                    ..Default::default()
                }],
            });
        }
        self.blocks = out;
        self.blocks_ver += 1;
        self.laid = None;
    }

    /// Footer actions that make sense for this tip.
    pub fn actions(&self) -> Vec<Act> {
        let mut v = Vec::new();
        if !self.extra.diags.is_empty() {
            v.push(Act::QuickFix);
        }
        if self.has_lsp {
            v.push(Act::Definition);
            if self.kind.wants_type_def() {
                v.push(Act::TypeDefinition);
            }
            v.push(Act::References);
            if !self.sig.is_empty() {
                v.push(Act::CopyType);
            }
        }
        v
    }
}

fn origin_link(o: &Origin) -> String {
    format!("lumen:goto:{}#{},{}", o.path.display(), o.line, o.col)
}

// ───────────────────────────── layout ─────────────────────────────

pub struct Pal {
    pub fg: Rgba,
    pub dim: Rgba,
    pub link: Rgba,
    pub code_bg: Rgba,
    pub chip_bg: Rgba,
    pub err: Rgba,
    pub warn: Rgba,
    pub info: Rgba,
    pub rule: Rgba,
}

impl Pal {
    pub fn new(t: &Theme) -> Pal {
        let c = &t.chrome;
        let dark = t.background.luminance() < 0.5;
        let fg = t.foreground;
        Pal {
            fg,
            dim: fg.with_alpha(if dark { 0xb4 } else { 0xa8 }),
            link: c.accent_text,
            code_bg: fg.with_alpha(if dark { 0x12 } else { 0x0d }),
            chip_bg: fg.with_alpha(if dark { 0x1e } else { 0x14 }),
            err: c.error,
            warn: c.warning,
            info: c.info,
            rule: fg.with_alpha(0x26),
        }
    }
    fn sev(&self, s: u8) -> Rgba {
        match s {
            1 => self.err,
            2 => self.warn,
            _ => self.info,
        }
    }
}

type HlFn<'a> = dyn FnMut(&str, &str) -> Option<Vec<Vec<(usize, usize, Rgba, u8)>>> + 'a;

pub struct Env<'a> {
    pub tx: &'a mut TextCtx,
    pub pal: Pal,
    pub hl: &'a mut HlFn<'a>,
    space: i32,
    cell: f32,
}

impl<'a> Env<'a> {
    pub fn new(tx: &'a mut TextCtx, pal: Pal, hl: &'a mut HlFn<'a>) -> Env<'a> {
        let space = (tx.ui_width("a a", false) - tx.ui_width("aa", false)).max(3);
        let cell = tx.cell_w;
        Env { tx, pal, hl, space, cell }
    }
    fn cells(&self, n: usize) -> i32 {
        (n as f32 * self.cell).round() as i32
    }
}

struct Builder {
    y: i32,
    lines: Vec<LLine>,
    decos: Vec<Deco>,
    urls: Vec<String>,
    links: Vec<(Rect, usize)>,
    right: i32,
}

impl Builder {
    fn url(&mut self, u: &str) -> usize {
        if let Some(i) = self.urls.iter().position(|x| x == u) {
            return i;
        }
        self.urls.push(u.to_string());
        self.urls.len() - 1
    }
}

#[derive(Clone)]
struct Tok {
    text: String,
    sp: bool,
    brk: bool,
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: Option<String>,
}

fn tokenize(spans: &[Span], force_bold: bool) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut pending = false;
    for s in spans {
        let mk = |text: String, sp: bool, brk: bool| Tok {
            text,
            sp,
            brk,
            bold: s.bold || force_bold,
            italic: s.italic,
            code: s.code,
            strike: s.strike,
            link: s.link.clone(),
        };
        if s.code {
            for (k, part) in s.text.split('\n').enumerate() {
                if k > 0 {
                    out.push(mk(String::new(), false, true));
                    pending = false;
                }
                if !part.is_empty() {
                    out.push(mk(part.to_string(), pending, false));
                    pending = false;
                }
            }
            continue;
        }
        let mut word = String::new();
        let flush = |word: &mut String, pending: &mut bool, out: &mut Vec<Tok>| {
            if !word.is_empty() {
                out.push(mk(std::mem::take(word), *pending, false));
                *pending = false;
            }
        };
        for c in s.text.chars() {
            if c == '\n' {
                flush(&mut word, &mut pending, &mut out);
                out.push(mk(String::new(), false, true));
                pending = false;
            } else if c.is_whitespace() {
                flush(&mut word, &mut pending, &mut out);
                pending = true;
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut pending, &mut out);
    }
    out
}

/// Word-wrap `spans` into lines of at most `w` px starting at `x0`.
fn flow(env: &mut Env, b: &mut Builder, spans: &[Span], x0: i32, w: i32, color: Rgba, force_bold: bool) {
    let toks = tokenize(spans, force_bold);
    let w = w.max(40);
    struct P {
        t: Tok,
        w: i32,
    }
    let mut rows: Vec<Vec<P>> = vec![vec![]];
    let mut cur = 0i32;
    let sp = env.space;
    for t in toks {
        if t.brk {
            rows.push(vec![]);
            cur = 0;
            continue;
        }
        let measure = |env: &mut Env, t: &Tok| -> i32 {
            if t.code { env.cells(t.text.chars().count()) + 6 } else { env.tx.ui_width(&t.text, t.bold) }
        };
        let tw = measure(env, &t);
        let row = rows.last_mut().unwrap();
        let lead = if t.sp && !row.is_empty() { sp } else { 0 };
        if tw > w {
            // too long for a line: break it into chunks
            let n = t.text.chars().count().max(1);
            let per = (((w - 6) as f32 / (tw as f32 / n as f32)) as usize).max(4);
            let chars: Vec<char> = t.text.chars().collect();
            let mut first = true;
            for ch in chars.chunks(per) {
                let piece = Tok { text: ch.iter().collect(), sp: first && t.sp, ..t.clone() };
                let pw = measure(env, &piece);
                if !rows.last().unwrap().is_empty() {
                    rows.push(vec![]);
                }
                rows.last_mut().unwrap().push(P { t: piece, w: pw });
                first = false;
            }
            cur = rows.last().unwrap().iter().map(|p| p.w).sum();
            continue;
        }
        if !row.is_empty() && cur + lead + tw > w {
            rows.push(vec![]);
            cur = 0;
            rows.last_mut().unwrap().push(P { t: Tok { sp: false, ..t }, w: tw });
            cur += tw;
        } else {
            cur += lead + tw;
            row.push(P { t, w: tw });
        }
    }
    if rows.last().is_some_and(|r| r.is_empty()) && rows.len() > 1 {
        rows.pop();
    }
    let slant = env.tx.italic_works();
    for row in rows {
        let y = b.y;
        let mut segs: Vec<Seg> = Vec::new();
        let mut plain = String::new();
        let mut x = x0;
        let mut i = 0;
        while i < row.len() {
            let t = &row[i].t;
            if t.sp && i > 0 {
                x += sp;
                plain.push(' ');
            }
            let lk = t.link.clone();
            if t.code {
                let wd = row[i].w;
                let li = lk.as_deref().map(|u| b.url(u));
                b.decos.push(Deco { x, y: y + 1, w: wd, h: PROSE_LH - 2, kind: DecoKind::Fill(env.pal.chip_bg, 3) });
                segs.push(Seg {
                    x: x + 3,
                    w: wd - 6,
                    text: t.text.clone(),
                    color: if li.is_some() { env.pal.link } else { env.pal.fg },
                    bold: false,
                    italic: false,
                    mono: true,
                    strike: t.strike,
                    link: li,
                    size: None,
                });
                if let Some(li) = li {
                    b.links.push((Rect::new(x, y, wd, PROSE_LH), li));
                }
                plain.push_str(&t.text);
                x += wd;
                i += 1;
                continue;
            }
            // merge following plain tokens with the same style
            let mut text = t.text.clone();
            let mut j = i + 1;
            while j < row.len() {
                let n = &row[j].t;
                if n.code || n.bold != t.bold || n.italic != t.italic || n.strike != t.strike || n.link != t.link {
                    break;
                }
                text.push(if n.sp { ' ' } else { '\u{200b}' });
                text.push_str(&n.text);
                j += 1;
            }
            let text = text.replace('\u{200b}', "");
            let wd = env.tx.ui_width(&text, t.bold);
            let li = lk.as_deref().map(|u| b.url(u));
            let c = if li.is_some() {
                env.pal.link
            } else if t.italic && !slant {
                // Fallback only: the slant did not render, so italics are shown a little lighter.
                color.with_alpha((color.3 as u32 * 220 / 255) as u8)
            } else {
                color
            };
            plain.push_str(&text);
            segs.push(Seg {
                x,
                w: wd,
                text,
                color: c,
                bold: t.bold,
                italic: t.italic && slant,
                mono: false,
                strike: t.strike,
                link: li,
                size: None,
            });
            if let Some(li) = li {
                b.links.push((Rect::new(x, y, wd, PROSE_LH), li));
            }
            x += wd;
            i = j;
        }
        b.right = b.right.max(x);
        b.lines.push(LLine { y, h: PROSE_LH, segs, x0: 0, runs: vec![], plain });
        b.y += PROSE_LH;
    }
}

/// One char with its style, for wrapping code.
type CharRun = (char, Rgba, u8);

fn code_chars(text: &str, spans: Option<&Vec<(usize, usize, Rgba, u8)>>, fg: Rgba) -> Vec<CharRun> {
    let mut out = Vec::with_capacity(text.len());
    let mut k = 0;
    for (bi, ch) in text.char_indices() {
        let mut col = (fg, 0u8);
        if let Some(sp) = spans {
            while k < sp.len() && sp[k].1 <= bi {
                k += 1;
            }
            if let Some(s) = sp.get(k)
                && s.0 <= bi
                && bi < s.1
            {
                col = (s.2, s.3);
            }
        }
        out.push((ch, col.0, col.1));
    }
    out
}

fn runs_of(chars: &[CharRun]) -> Vec<(String, Rgba, u8)> {
    let mut out: Vec<(String, Rgba, u8)> = Vec::new();
    for &(ch, c, f) in chars {
        match out.last_mut() {
            Some(l) if l.1 == c && l.2 == f => l.0.push(ch),
            _ => out.push((ch.to_string(), c, f)),
        }
    }
    out
}

/// Wrap a code line at `cols` chars, preferring to break after `,` / before a space.
pub fn wrap_code(chars: Vec<CharRun>, cols: usize) -> Vec<Vec<CharRun>> {
    let cols = cols.max(12);
    if chars.len() <= cols {
        return vec![chars];
    }
    let indent = chars.iter().take_while(|c| c.0 == ' ').count();
    let cont = (indent + 2).min(cols / 3);
    let mut out = Vec::new();
    let mut rest = chars;
    let mut first = true;
    loop {
        let room = if first { cols } else { cols - cont };
        if rest.len() <= room {
            out.push(rest);
            break;
        }
        let lo = room / 2;
        let cut = (lo..room)
            .rev()
            .find(|&i| rest[i - 1].0 == ',' && rest[i].0 == ' ')
            .map(|i| i + 1)
            .or_else(|| (lo..room).rev().find(|&i| rest[i].0 == ' ').map(|i| i + 1))
            .unwrap_or(room);
        let mut tail = rest.split_off(cut.min(rest.len()));
        while rest.last().is_some_and(|c| c.0 == ' ') {
            rest.pop();
        }
        out.push(rest);
        // continuation indent
        let fg = tail.first().map(|c| (c.1, c.2)).unwrap_or((Rgba(0, 0, 0, 255), 0));
        let mut line: Vec<CharRun> = std::iter::repeat_n((' ', fg.0, 0), cont).collect();
        while tail.first().is_some_and(|c| c.0 == ' ') {
            tail.remove(0);
        }
        line.extend(tail);
        rest = line;
        first = false;
    }
    out
}

fn code_block(env: &mut Env, b: &mut Builder, text: &str, lang: &str, x0: i32, w: i32, bg: bool) {
    let text = text.replace('\t', "  ");
    let inner_x = x0 + if bg { CODE_PAD } else { 0 };
    let inner_w = w - (inner_x - x0) - if bg { CODE_PAD } else { 0 };
    let cols = (inner_w as f32 / env.cell) as usize;
    let hl = (env.hl)(lang, &text);
    let fg = env.pal.fg;
    let lh = line_h() as i32;
    let top = b.y;
    if bg {
        b.y += 5;
    }
    for (li, line) in text.split('\n').enumerate() {
        let chars = code_chars(line, hl.as_ref().and_then(|h| h.get(li)), fg);
        for part in wrap_code(chars, cols) {
            let plain: String = part.iter().map(|c| c.0).collect();
            b.right = b.right.max(inner_x + env.cells(part.len()) + if bg { CODE_PAD } else { 0 });
            b.lines.push(LLine { y: b.y, h: lh, segs: vec![], x0: inner_x, runs: runs_of(&part), plain });
            b.y += lh;
        }
    }
    if bg {
        b.y += 5;
        b.decos.push(Deco { x: x0, y: top, w: 0, h: b.y - top, kind: DecoKind::Fill(env.pal.code_bg, 6) });
    }
}

fn label(env: &mut Env, b: &mut Builder, text: &str, x0: i32) {
    // "TYPE ButtonProps" keeps the identifier as written; the fixed titles are all caps
    let up = match text.split_once(' ') {
        Some((first, rest)) if matches!(first, "Type" | "Class") => format!("{} {rest}", first.to_uppercase()),
        _ => text.to_uppercase(),
    };
    let wd = env.tx.ui_width_sized(&up, true, 10.5);
    b.right = b.right.max(x0 + wd);
    b.lines.push(LLine {
        y: b.y,
        h: LABEL_LH,
        segs: vec![Seg {
            x: x0,
            w: wd,
            text: up.clone(),
            color: env.pal.dim,
            bold: true,
            italic: false,
            mono: false,
            strike: false,
            link: None,
            size: Some(10.5),
        }],
        x0: 0,
        runs: vec![],
        plain: text.to_string(),
    });
    b.y += LABEL_LH;
}

fn block(env: &mut Env, b: &mut Builder, blk: &Blk, x0: i32, w: i32) {
    let fg = env.pal.fg;
    match blk {
        Blk::Code { text, lang } => code_block(env, b, text, if lang.is_empty() { "ts" } else { lang }, x0, w, true),
        Blk::Para(sp) => flow(env, b, sp, x0, w - x0, fg, false),
        Blk::List(items) => {
            for ListItem { marker, depth, spans } in items {
                let ind = x0 + *depth as i32 * 14;
                let y = b.y;
                let mw = env.tx.ui_width(marker, false);
                b.lines.push(LLine {
                    y,
                    h: PROSE_LH,
                    segs: vec![Seg {
                        x: ind + 12 - mw,
                        w: mw,
                        text: marker.clone(),
                        color: env.pal.dim,
                        bold: false,
                        italic: false,
                        mono: false,
                        strike: false,
                        link: None,
                        size: None,
                    }],
                    x0: 0,
                    runs: vec![],
                    plain: String::new(),
                });
                let n0 = b.lines.len();
                flow(env, b, spans, ind + 16, w - ind - 16 + x0, fg, false);
                // the marker line shares its row with the first text line
                if b.lines.len() > n0 {
                    let first = b.lines.remove(n0);
                    let m = b.lines.remove(n0 - 1);
                    let mut f = first;
                    f.segs.splice(0..0, m.segs);
                    f.plain = format!("{} {}", marker, f.plain);
                    b.lines.insert(n0 - 1, f);
                }
            }
        }
        Blk::Quote(sp) => {
            let top = b.y;
            flow(env, b, sp, x0 + 10, w - x0 - 10, env.pal.dim, false);
            b.decos.push(Deco {
                x: x0,
                y: top + 1,
                w: 3,
                h: b.y - top - 2,
                kind: DecoKind::Fill(env.pal.rule.with_alpha(0x66), 1),
            });
        }
        Blk::Rule => {
            b.y += 3;
            b.decos.push(Deco { x: x0, y: b.y, w: 0, h: 1, kind: DecoKind::Rule(env.pal.rule) });
            b.y += 4;
        }
        Blk::Label(t) => label(env, b, t, x0),
        Blk::Item { head, body, extra } => {
            if let Some(h) = head {
                let cols_w = w - x0;
                let before = b.lines.len();
                code_block_param(env, b, h, x0, cols_w);
                let _ = before;
            }
            let ind = x0 + 14;
            if !body.is_empty() {
                flow(env, b, body, ind, w - ind + x0, env.pal.fg, false);
            }
            for e in extra {
                b.y += 3;
                block(env, b, e, ind, w);
            }
        }
        Blk::Diag { sev, msg, code, url } => {
            let col = env.pal.sev(*sev);
            let top = b.y;
            b.y += 4;
            let mut spans = vec![Span::plain(msg.clone())];
            if !code.is_empty() {
                spans.push(Span::plain(" "));
                spans.push(Span {
                    text: if code.chars().all(|c| c.is_ascii_digit()) { format!("TS{code}") } else { code.clone() },
                    link: url.clone(),
                    ..Default::default()
                });
            }
            flow(env, b, &spans, x0 + 10, w - x0 - 10 - 4, fg, false);
            b.y += 4;
            b.decos.push(Deco { x: x0, y: top, w: 0, h: b.y - top, kind: DecoKind::Fill(col.with_alpha(0x1c), 6) });
            b.decos.push(Deco { x: x0, y: top + 3, w: 3, h: b.y - top - 6, kind: DecoKind::Fill(col, 1) });
        }
        Blk::Note { sev, title, body } => {
            let col = env.pal.sev(*sev);
            let top = b.y;
            b.y += 4;
            let mut spans = vec![Span { text: title.clone(), bold: true, ..Default::default() }];
            if !body.is_empty() {
                spans.push(Span::plain(" — "));
                spans.extend(body.iter().cloned());
            }
            flow(env, b, &spans, x0 + 10, w - x0 - 14, fg, false);
            b.y += 4;
            b.decos.push(Deco { x: x0, y: top, w: 0, h: b.y - top, kind: DecoKind::Fill(col.with_alpha(0x1c), 6) });
            b.decos.push(Deco { x: x0, y: top + 3, w: 3, h: b.y - top - 6, kind: DecoKind::Fill(col, 1) });
        }
        Blk::Dim(sp) => flow(env, b, sp, x0, w - x0, env.pal.dim, false),
        Blk::Kv { key, val } => {
            let kw = (env.tx.ui_width_sized(key, true, 11.5) + 14).max(60);
            let y = b.y;
            let n0 = b.lines.len();
            flow(env, b, val, x0 + kw, w - x0 - kw, fg, false);
            let kwid = env.tx.ui_width_sized(key, true, 11.5);
            let seg = Seg {
                x: x0,
                w: kwid,
                text: key.clone(),
                color: env.pal.dim,
                bold: true,
                italic: false,
                mono: false,
                strike: false,
                link: None,
                size: Some(11.5),
            };
            if b.lines.len() > n0 {
                b.lines[n0].segs.insert(0, seg);
                b.lines[n0].plain = format!("{key}: {}", b.lines[n0].plain);
            } else {
                b.lines.push(LLine { y, h: PROSE_LH, segs: vec![seg], x0: 0, runs: vec![], plain: key.clone() });
                b.y += PROSE_LH;
            }
        }
    }
}

/// `name: type` of a parameter / return value, highlighted as TypeScript, no background.
fn code_block_param(env: &mut Env, b: &mut Builder, head: &str, x0: i32, w: i32) {
    code_block(env, b, head, "ts", x0, w, false);
}

pub fn layout(blocks: &[Blk], w: i32, env: &mut Env) -> Laid {
    let mut b = Builder { y: 0, lines: vec![], decos: vec![], urls: vec![], links: vec![], right: 0 };
    let mut prev_label = false;
    for (i, blk) in blocks.iter().enumerate() {
        if i > 0 {
            let is_item = matches!(blk, Blk::Item { .. });
            b.y += if prev_label {
                3
            } else if matches!(blk, Blk::Label(_)) {
                GAP + 2
            } else if is_item {
                5
            } else {
                GAP
            };
        }
        block(env, &mut b, blk, 0, w);
        prev_label = matches!(blk, Blk::Label(_));
    }
    Laid { w: b.right.min(w), h: b.y, lines: b.lines, decos: b.decos, urls: b.urls, links: b.links }
}

// ───────────────────────────── geometry ─────────────────────────────

/// Popup rectangle for a body of `want_h` px anchored at line `(ax, ay)` (line height `lh`):
/// above the line when it fits (or when there is more room above), else below; always inside
/// `bounds`. `side` is the side chosen before (kept unless it stops fitting and the other is
/// much better), the returned bool is "below".
pub fn place(anchor: (i32, i32), lh: i32, bounds: Rect, w: i32, want_h: i32, side: Option<bool>) -> (Rect, bool) {
    let margin = 4;
    let above = anchor.1 - bounds.y - margin - 3;
    let below_room = bounds.bottom() - (anchor.1 + lh) - margin - 3;
    let h_cap = (bounds.h - 2 * margin).max(60);
    let want = want_h.min(h_cap);
    let below = match side {
        Some(true) if below_room >= want.min(120) || below_room >= above => true,
        Some(false) if above >= want.min(120) || above >= below_room => false,
        _ => {
            if above >= want {
                false
            } else if below_room >= want {
                true
            } else {
                below_room > above
            }
        }
    };
    let avail = if below { below_room } else { above };
    let h = want.min(avail.max(60)).min(h_cap);
    let w = w.min(bounds.w - 2 * margin).max(60);
    let y = if below { anchor.1 + lh + 3 } else { anchor.1 - h - 3 };
    let y = y.clamp(bounds.y + margin, (bounds.bottom() - h - margin).max(bounds.y + margin));
    let x = anchor.0.clamp(bounds.x + margin, (bounds.right() - w - margin).max(bounds.x + margin));
    (Rect::new(x, y, w, h), below)
}

fn link_target(url: &str) -> Hit {
    Hit::Link(url.to_string())
}

// ───────────────────────────── App integration ─────────────────────────────

fn scope_for(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "" | "ts" | "typescript" | "tsx" | "typescriptreact" => "source.ts",
        "js" | "javascript" | "jsx" | "mjs" | "cjs" => "source.js",
        "json" => "source.json",
        "jsonc" => "source.json.comments",
        "css" => "source.css",
        "html" => "text.html.basic",
        "sh" | "bash" | "shell" | "shellscript" | "zsh" => "source.shell",
        _ => return None,
    })
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Byte offset of (line, utf16 column) in `text`.
pub fn byte_off(text: &str, line: usize, col16: usize) -> Option<usize> {
    let mut start = 0usize;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let mut n = 0;
    let mut off = start;
    for ch in text[start..].chars() {
        if n >= col16 || ch == '\n' {
            break;
        }
        n += ch.len_utf16();
        off += ch.len_utf8();
    }
    Some(off)
}

/// Is this file a TypeScript lib file (`lib.dom.d.ts`...), whose types are too big to inline?
fn is_lib_dts(path: &std::path::Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("lib.") && n.ends_with(".d.ts"))
}

/// `@scope/name@1.2.3` for a file inside node_modules.
fn package_of(path: &std::path::Path) -> Option<String> {
    let s = path.to_string_lossy();
    let i = s.rfind("/node_modules/")?;
    let rest = &s[i + "/node_modules/".len()..];
    let mut it = rest.split('/');
    let first = it.next()?;
    let name = if first.starts_with('@') { format!("{first}/{}", it.next()?) } else { first.to_string() };
    let pj = std::path::Path::new(&s[..i + "/node_modules/".len()]).join(&name).join("package.json");
    let ver = std::fs::read_to_string(pj)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["version"].as_str().map(str::to_string));
    Some(match ver {
        Some(v) => format!("{name}@{v}"),
        None => name,
    })
}

fn hover_markdown(v: &Value) -> String {
    fn one(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => {
                let val = o.get("value").and_then(Value::as_str).unwrap_or("");
                match o.get("language").and_then(Value::as_str) {
                    Some(l) => format!("```{l}\n{val}\n```"),
                    None => val.to_string(),
                }
            }
            Value::Array(a) => a.iter().map(one).filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join("\n\n"),
            _ => String::new(),
        }
    }
    one(&v["contents"]).trim().to_string()
}

/// A literal (string / number / boolean / null) at char column `col` of `line`: (start, end) columns.
fn literal_at(line: &str, col: usize) -> Option<(usize, usize)> {
    let cs: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '"' || c == '\'' || c == '`' {
            let s = i;
            i += 1;
            while i < cs.len() && cs[i] != c {
                if cs[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            let e = (i + 1).min(cs.len());
            if (s..e).contains(&col) {
                return Some((s, e));
            }
            i = e;
        } else if c == '/' && cs.get(i + 1) == Some(&'/') {
            return None;
        } else if is_ident(c) {
            let s = i;
            while i < cs.len() && (is_ident(cs[i]) || (cs[i] == '.' && cs[s].is_ascii_digit())) {
                i += 1;
            }
            let word: String = cs[s..i].iter().collect();
            let lit = matches!(word.as_str(), "true" | "false" | "null")
                || word.chars().next().is_some_and(|c| c.is_ascii_digit());
            if lit && (s..i).contains(&col) {
                return Some((s, i));
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Offset of the name in `interface NAME` / `type NAME` / `class NAME` / `enum NAME` in `text`.
fn find_type_decl(text: &str, name: &str) -> Option<(usize, Kind)> {
    let re = regex::Regex::new(&format!(r"(?m)^[ \t]*(?:export\s+)?(?:declare\s+)?(?:default\s+)?(?:abstract\s+)?(?:const\s+)?(?:interface|type|class|enum)\s+({})\b", regex::escape(name))).ok()?;
    let m = re.captures(text)?.get(1)?;
    let k = md::decl_kind_at(text, m.start())
        .filter(|k| matches!(k, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias))?;
    Some((m.start(), k))
}

fn describe_literal(text: &str) -> String {
    let t = text.trim();
    match t.chars().next() {
        Some('"' | '\'') => format!("string · {} caracteres", t.chars().count().saturating_sub(2)),
        Some('`') => format!("template · {} caracteres", t.chars().count().saturating_sub(2)),
        Some(c) if c.is_ascii_digit() => {
            if t.ends_with('n') {
                "bigint".to_string()
            } else {
                "number".to_string()
            }
        }
        _ if t == "null" => "null".to_string(),
        _ => "boolean".to_string(),
    }
}

impl App {
    /// Diagnostics at char `at` of document `i`.
    pub fn diag_infos_at(&self, i: usize, at: usize) -> Vec<DiagInfo> {
        let d = &self.docs[i];
        let (l, c) = d.buf.line_col(at);
        d.diags
            .iter()
            .filter(|g: &&Diag| {
                (g.sl, g.sc) <= (l, c) && (l, c) < (g.el, g.ec.max(g.sc + 1))
                    || (g.sl == l && g.sc <= c && c <= g.ec.max(g.sc + 1) && g.el == l)
            })
            .map(|g| DiagInfo {
                sev: g.severity,
                msg: g.message.clone(),
                url: (!g.code.is_empty() && g.code.chars().all(|c| c.is_ascii_digit()))
                    .then(|| format!("https://typescript.tv/errors/#TS{}", g.code)),
                code: g.code.clone(),
                line: g.sl,
                col: g.sc,
            })
            .collect()
    }

    /// Show a tip: replaces the current one, cancels the hide timer, remembers it in the cache.
    fn hover_show(&mut self, mut tip: HoverTip) {
        self.hv.serial += 1;
        tip.serial = self.hv.serial;
        self.hv.hide_at = None;
        self.hv.hot = None;
        if tip.has_lsp {
            self.hv.cache.retain(|c| !(c.uri == tip.uri && c.anchor == tip.anchor));
            self.hv.cache.push(tip.clone());
            if self.hv.cache.len() > CACHE_MAX {
                self.hv.cache.remove(0);
            }
        }
        self.hover = Some(tip);
        self.request_redraw();
    }

    /// A tip kept from an earlier hover over the same symbol (same document version).
    pub fn hover_from_cache(&mut self, at: usize) -> bool {
        let i = self.active;
        let Some(d) = self.docs.get(i) else { return false };
        let (uri, ver) = (d.uri().unwrap_or_default(), d.version);
        let Some(mut tip) = self
            .hv
            .cache
            .iter()
            .find(|c| c.uri == uri && c.ver == ver && c.anchor.0 <= at && at <= c.anchor.1)
            .cloned()
        else {
            return false;
        };
        let Some(pos) = d.screen_pos(tip.anchor.0) else { return false };
        tip.at = pos;
        tip.scroll = 0.0;
        tip.sel = None;
        tip.side_below = None;
        tip.extra.diags = self.diag_infos_at(i, at);
        tip.compose();
        self.hv.serial += 1;
        tip.serial = self.hv.serial;
        self.hv.hide_at = None;
        self.hover = Some(tip);
        self.request_redraw();
        true
    }

    pub fn hover_show_diags(&mut self, i: usize, at: usize) {
        let diags = self.diag_infos_at(i, at);
        if diags.is_empty() {
            return;
        }
        if let Some(pos) = self.docs[i].screen_pos(at) {
            let mut tip = HoverTip::diag_only(diags, (at, at), pos);
            tip.uri = self.docs[i].uri().unwrap_or_default();
            self.hover_show(tip);
        }
    }

    pub fn hover_set_markdown(&mut self, text: &str) {
        let i = self.active;
        let Some(d) = self.docs.get(i) else { return };
        let at = d.ed.head();
        if let Some(pos) = d.screen_pos(at) {
            let mut tip = HoverTip::from_markdown(text, pos, at);
            tip.has_lsp = false;
            tip.uri = d.uri().unwrap_or_default();
            self.hover_show(tip);
        }
    }

    /// The `textDocument/hover` answer.
    pub fn hover_response(&mut self, uri: &str, at: usize, v: &Value) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        if i != self.active {
            return;
        }
        let md_text = hover_markdown(v);
        let diags = self.diag_infos_at(i, at);
        let d = &self.docs[i];
        let range = v["range"].as_object().map(|_| {
            let r = &v["range"];
            let f = |p: &Value| {
                d.pos_from_lsp(p["line"].as_u64().unwrap_or(0) as usize, p["character"].as_u64().unwrap_or(0) as usize)
            };
            (f(&r["start"]), f(&r["end"]))
        });
        let anchor = range.unwrap_or_else(|| self.word_range(i, at));
        let (ver, uri_s) = (d.version, d.uri().unwrap_or_default());
        if md_text.is_empty() {
            // Nothing from the server: a literal value, or only diagnostics.
            if let Some(lit) = self.literal_hover(i, at, diags.clone()) {
                return lit;
            }
            if diags.is_empty() {
                return;
            }
        }
        let Some(pos) = self.docs[i].screen_pos(anchor.0).or_else(|| self.docs[i].screen_pos(at)) else { return };
        let mut tip = HoverTip::new(&md_text, anchor, pos);
        tip.uri = uri_s;
        tip.ver = ver;
        tip.extra.diags = diags;
        // import / require statements: module and resolved file
        let spec = self.import_at(i, at);
        let is_module = tip.kind == Kind::Module;
        if let Some((s, _, _)) = &spec
            && (is_module || md_text.contains("(alias)") || tip.kind == Kind::Other)
        {
            tip.extra.module = Some(ModInfo { spec: s.clone(), file: None, pkg: None });
        }
        tip.compose();
        self.hover_show(tip);
        if !md_text.is_empty() {
            self.hover_follow_ups(i, anchor.0, spec);
        }
    }

    /// (start, end) char range of the identifier at `at`.
    fn word_range(&self, i: usize, at: usize) -> (usize, usize) {
        let d = &self.docs[i];
        let (l, c) = d.buf.line_col(at);
        let line: Vec<char> = d.buf.line(l).chars().collect();
        let (mut s, mut e) = (c.min(line.len()), c.min(line.len()));
        while s > 0 && is_ident(line[s - 1]) {
            s -= 1;
        }
        while e < line.len() && is_ident(line[e]) {
            e += 1;
        }
        (d.buf.pos(l, s), d.buf.pos(l, e))
    }

    /// (specifier, line, char column of the specifier) when `at` is inside an import statement.
    fn import_at(&self, i: usize, at: usize) -> Option<(String, usize, usize)> {
        let d = &self.docs[i];
        if !matches!(d.lang, Some("typescript" | "tsx" | "javascript" | "jsx")) {
            return None;
        }
        let (l, _) = d.buf.line_col(at);
        let lo = l.saturating_sub(60);
        let hi = (l + 61).min(d.buf.line_count());
        let lines: Vec<String> = (lo..hi).map(|k| d.buf.line(k)).collect();
        let (s, ln, col) = md::import_spec(&lines, l - lo)?;
        Some((s, ln + lo, col))
    }

    fn hover_follow_ups(&mut self, i: usize, anchor: usize, spec: Option<(String, usize, usize)>) {
        let Some(tip) = &self.hover else { return };
        let (serial, kind, uri) = (tip.serial, tip.kind, tip.uri.clone());
        let Some((_, li)) = self.hover_server(i) else { return };
        let ask = |app: &mut App, what: HoverWhat, params: Value, method: &str| {
            app.lsps[li].request(method, params, Pending::HoverExtra { uri: uri.clone(), at: anchor, serial, what });
        };
        if (kind == Kind::Module || (spec.is_some() && self.hover.as_ref().is_some_and(|t| t.extra.module.is_some())))
            && let Some((_, line, col)) = &spec
        {
            let d = &self.docs[i];
            let p = d.buf.pos(*line, *col + 1);
            let params = self.hover_pos_params(i, p);
            ask(self, HoverWhat::Module, params, "textDocument/definition");
        }
        if kind.wants_def() && kind != Kind::Module {
            let params = self.hover_pos_params(i, anchor);
            ask(self, HoverWhat::Def, params, "textDocument/definition");
        }
        if kind.wants_type_def() {
            let params = self.hover_pos_params(i, anchor);
            ask(self, HoverWhat::TypeDef, params, "textDocument/typeDefinition");
        }
    }

    fn hover_server(&mut self, i: usize) -> Option<(usize, usize)> {
        let li = self.doc_lsp(i)?;
        self.docs[i].lsp_open.then_some((i, li))
    }

    fn hover_pos_params(&self, i: usize, at: usize) -> Value {
        let d = &self.docs[i];
        let (l, c) = d.lsp_pos(at);
        json!({"textDocument": {"uri": d.uri().unwrap_or_default()}, "position": {"line": l, "character": c}})
    }

    /// Text of a file the server pointed at: the open buffer when there is one, else the disk.
    fn read_source(&self, uri: &str) -> Option<(String, std::path::PathBuf)> {
        let path = uri_to_path(uri)?;
        if let Some(d) = self.docs.iter().find(|d| d.path() == Some(path.as_path())) {
            return Some((d.buf.to_string(), path));
        }
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() > 4_000_000 {
            return None;
        }
        Some((std::fs::read_to_string(&path).ok()?, path))
    }

    /// Answer of a follow-up request (definition, type definition, module resolution, key hover).
    pub fn hover_extra_response(&mut self, uri: &str, at: usize, serial: u64, what: HoverWhat, v: &Value) {
        let live = self.hover.as_ref().is_some_and(|t| t.serial == serial && t.uri == uri && t.anchor.0 == at);
        if !live {
            return;
        }
        let mut changed = false;
        match what {
            HoverWhat::KeyType => {
                let m = hover_markdown(v);
                if !m.is_empty()
                    && let Some(t) = &mut self.hover
                {
                    t.extra.key_md = Some(m);
                    changed = true;
                }
            }
            HoverWhat::Module => {
                if let Some((turi, ..)) = lsp::locations(v).into_iter().next()
                    && let Some(path) = uri_to_path(&turi)
                {
                    let label = self.rel(&path);
                    let pkg = package_of(&path);
                    if let Some(t) = &mut self.hover {
                        if let Some(m) = &mut t.extra.module {
                            m.file = Some(label.clone());
                            m.pkg = pkg;
                        }
                        t.extra.origin = Some(Origin { path, label, line: 0, col: 0 });
                        changed = true;
                    }
                }
            }
            HoverWhat::Def => {
                if let Some((turi, line, col)) = lsp::locations(v).into_iter().next() {
                    changed = self.apply_definition(&turi, line, col);
                }
            }
            HoverWhat::TypeDef => {
                if let Some((turi, line, col)) = lsp::locations(v).into_iter().next() {
                    changed = self.apply_type_definition(&turi, line, col);
                }
            }
        }
        if changed {
            if let Some(t) = &mut self.hover {
                t.compose();
                let c = t.clone();
                if let Some(slot) = self.hv.cache.iter_mut().find(|x| x.serial == serial) {
                    *slot = c;
                }
            }
            self.request_redraw();
        }
    }

    fn apply_definition(&mut self, turi: &str, line: usize, col: usize) -> bool {
        let Some((text, path)) = self.read_source(turi) else { return false };
        let Some(off) = byte_off(&text, line, col) else { return false };
        let label = self.rel(&path);
        let Some(t) = &self.hover else { return false };
        let (kind, name) = (t.kind, t.name.clone());
        let at_kind = md::decl_kind_at(&text, off);
        let mut decl = None;
        let mut overloads = Vec::new();
        let mut value = None;
        let mut class_decl = None;
        let mut snippet = None;
        match kind {
            Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias => {
                let k = at_kind
                    .filter(|k| matches!(k, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias))
                    .unwrap_or(kind);
                if let Some((text, more)) = md::render_decl(&text, off, k, DECL_LINES) {
                    decl = Some(Decl { text, more, kind: k });
                }
            }
            Kind::Function | Kind::Method => {
                overloads = md::find_overloads(&text, &name, off);
                // `function Button(props: ButtonProps)`: show ButtonProps when it is declared in that file
                let sig = self.hover.as_ref().and_then(|t| md::parse_sig(&t.sig));
                for p in sig.iter().flat_map(|s| s.params.iter()) {
                    let ty = p.ty.trim().trim_end_matches("[]");
                    let base = ty.split('<').next().unwrap_or("");
                    if base.is_empty() || !base.chars().all(is_ident) || !base.starts_with(|c: char| c.is_uppercase()) {
                        continue;
                    }
                    if let Some((o, k)) = find_type_decl(&text, base)
                        && let Some((body, more)) = md::render_decl(&text, o, k, DECL_LINES)
                    {
                        class_decl = Some((format!("Type {base}"), Decl { text: body, more, kind: k }));
                        break;
                    }
                }
            }
            Kind::Constructor if at_kind == Some(Kind::Class) => {
                let cname: String = text[off..].chars().take_while(|c| is_ident(*c)).collect();
                if let Some((body, more)) = md::render_decl(&text, off, Kind::Class, DECL_LINES) {
                    class_decl = Some((format!("Class {cname}"), Decl { text: body, more, kind: Kind::Class }));
                }
            }
            Kind::Variable if at_kind == Some(Kind::Variable) => {
                value = md::short_value(&text, off);
                if value.is_none() && self.hover.as_ref().is_some_and(|t| t.sig.contains("...")) {
                    snippet = md::render_decl(&text, off, Kind::Variable, 14);
                }
            }
            _ => {}
        }
        let is_current = self.docs.get(self.active).and_then(|d| d.path()).is_some_and(|p| p == path);
        let Some(t) = &mut self.hover else { return false };
        let ok =
            decl.is_some() || !overloads.is_empty() || value.is_some() || class_decl.is_some() || snippet.is_some();
        t.extra.snippet = snippet;
        t.extra.type_decl = class_decl.or(t.extra.type_decl.take());
        t.extra.decl = decl;
        t.extra.overloads = overloads;
        t.extra.value = value;
        // `line` is 0-based; shown as line + 1. Not shown for symbols declared right here.
        if !is_current || at_kind.is_none() {
            t.extra.origin = Some(Origin { path, label, line, col });
        }
        ok || t.extra.origin.is_some()
    }

    fn apply_type_definition(&mut self, turi: &str, line: usize, col: usize) -> bool {
        let Some((text, path)) = self.read_source(turi) else { return false };
        if is_lib_dts(&path) {
            return false;
        }
        let Some(off) = byte_off(&text, line, col) else { return false };
        let Some(k) = md::decl_kind_at(&text, off)
            .filter(|k| matches!(k, Kind::Interface | Kind::Class | Kind::Enum | Kind::TypeAlias))
        else {
            return false;
        };
        let name: String = text[off..].chars().take_while(|c| is_ident(*c)).collect();
        let Some(t) = &self.hover else { return false };
        if name.is_empty() || !t.sig.contains(&name) || t.name == name {
            return false;
        }
        let Some((body, more)) = md::render_decl(&text, off, k, DECL_LINES) else { return false };
        let Some(t) = &mut self.hover else { return false };
        t.extra.type_decl = Some((format!("Type {name}"), Decl { text: body, more, kind: k }));
        true
    }

    /// Hover over a literal value: its kind, and the type of the property it is assigned to.
    fn literal_hover(&mut self, i: usize, at: usize, diags: Vec<DiagInfo>) -> Option<()> {
        let d = &self.docs[i];
        if !matches!(d.lang, Some("typescript" | "tsx" | "javascript" | "jsx")) {
            return None;
        }
        let (l, c) = d.buf.line_col(at);
        let line = d.buf.line(l);
        let (s, e) = literal_at(&line, c)?;
        let cs: Vec<char> = line.chars().collect();
        let text: String = cs[s..e].iter().collect();
        let (a, b) = (d.buf.pos(l, s), d.buf.pos(l, e));
        let pos = d.screen_pos(a)?;
        // `key: value`: the key sits before a colon
        let mut k = s;
        while k > 0 && cs[k - 1] == ' ' {
            k -= 1;
        }
        let mut key_at = None;
        if k > 0 && (cs[k - 1] == ':' || (cs[k - 1] == '=' && !(k > 1 && matches!(cs[k - 2], '=' | '!' | '<' | '>')))) {
            k -= 1;
            while k > 0 && cs[k - 1] == ' ' {
                k -= 1;
            }
            let end = k;
            if k > 0 && (cs[k - 1] == '"' || cs[k - 1] == '\'') {
                let q = cs[k - 1];
                k -= 1;
                while k > 0 && cs[k - 1] != q {
                    k -= 1;
                }
                k = k.saturating_sub(1);
                if end > k {
                    key_at = Some(d.buf.pos(l, k + 1));
                }
            } else {
                while k > 0 && is_ident(cs[k - 1]) {
                    k -= 1;
                }
                if end > k {
                    key_at = Some(d.buf.pos(l, k));
                }
            }
        }
        let mut tip = HoverTip::new("", (a, b), pos);
        tip.uri = d.uri().unwrap_or_default();
        tip.ver = d.version;
        tip.extra.diags = diags;
        tip.extra.literal = Some((text.clone(), describe_literal(&text)));
        tip.has_lsp = false;
        tip.compose();
        self.hover_show(tip);
        if let Some(ka) = key_at {
            let Some((_, li)) = self.hover_server(i) else { return Some(()) };
            let params = self.hover_pos_params(i, ka);
            let (serial, uri) = self.hover.as_ref().map(|t| (t.serial, t.uri.clone()))?;
            self.lsps[li].request(
                "textDocument/hover",
                params,
                Pending::HoverExtra { uri, at: a, serial, what: HoverWhat::KeyType },
            );
        }
        Some(())
    }

    // ───────────── pointer, keys, timers ─────────────

    /// The pointer moved over the editor text (`None` = elsewhere): keeps the popup alive while the
    /// pointer is on its symbol or inside it, hides it shortly after it leaves, arms a new probe.
    pub fn hover_pointer(&mut self, p: Option<usize>) {
        let (x, y) = self.mouse;
        if let Some(h) = &self.hover {
            let inside = h.rect.contains(x, y);
            let on_symbol =
                p.is_some_and(|p| h.anchor.0 <= p && p <= h.anchor.1.max(h.anchor.0 + 1) && self.over_anchor_line(h));
            if inside || on_symbol {
                self.hv.hide_at = None;
                self.hover_probe = None;
                return;
            }
            if self.hv.hide_at.is_none() {
                self.hv.hide_at = Some(Instant::now() + HIDE_DELAY);
            }
        }
        match p {
            Some(p) => {
                let changed = self.hover_probe.is_none_or(|(_, q, _)| q != p);
                if changed {
                    self.hover_probe = Some((Instant::now(), p, false));
                }
            }
            None => self.hover_probe = None,
        }
    }

    fn over_anchor_line(&self, h: &HoverTip) -> bool {
        let Some(d) = self.docs.get(self.active) else { return false };
        d.screen_pos(h.anchor.0).is_some_and(|(_, y)| {
            let lh = line_h() as f64;
            self.mouse.1 >= y as f64 && self.mouse.1 < y as f64 + lh
        })
    }

    pub fn hover_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if let Some(t) = self.hv.hide_at {
            if t <= now {
                self.hv.hide_at = None;
                self.hover = None;
                self.request_redraw();
            } else {
                want(t);
            }
        }
    }

    /// Mouse moved: true when the popup consumed it (dragging, or the pointer is over it).
    pub fn hover_mouse_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let Some(h) = &mut self.hover else { return false };
        if let Some(grab) = self.hv.drag_bar {
            if let (Some(l), Some(b)) = (h.laid.as_ref().map(|l| &l.1), h.bar) {
                h.scroll = b.pos_for(y, grab, l.h as f64, h.view.h as f64);
            }
            return true;
        }
        if let Some(anchor) = self.hv.drag_sel {
            let top = h.view.y as f64;
            let bot = h.view.bottom() as f64;
            if y < top {
                h.scroll = (h.scroll - 14.0).max(0.0);
            } else if y > bot {
                h.scroll += 14.0;
            }
            clamp_scroll(h);
            if let Some(head) = pos_at(h, &mut self.tx, x, y.clamp(top, bot - 1.0)) {
                h.sel = (head != anchor).then_some((anchor, head));
            }
            return true;
        }
        if !h.rect.contains(x, y) || self.dragging {
            if self.hv.hot.take().is_some() {
                self.request_redraw();
            }
            return false;
        }
        let hot = h.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone());
        if hot != self.hv.hot {
            self.hv.hot = hot;
            self.request_redraw();
        }
        self.hv.hide_at = None;
        self.hover_probe = None;
        true
    }

    pub fn hover_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let h = self.hover.as_ref()?;
        if !h.rect.contains(x, y) {
            return None;
        }
        if h.hits.iter().any(|(r, _)| r.contains(x, y)) || h.bar.is_some_and(|b| b.track.contains(x, y)) {
            return Some(CursorIcon::Pointer);
        }
        Some(if h.view.contains(x, y) { CursorIcon::Text } else { CursorIcon::Default })
    }

    /// Press: handled when inside the popup (links, actions, scrollbar, line selection); outside
    /// it closes the popup and lets the click through.
    pub fn hover_mouse_down(&mut self, button: MouseButton) -> bool {
        let (x, y) = self.mouse;
        let Some(h) = &mut self.hover else { return false };
        if !h.rect.contains(x, y) || button == MouseButton::Right {
            self.hover = None;
            return false;
        }
        if let Some(b) = h.bar.filter(|b| b.track.contains(x, y)) {
            let grab = if b.thumb.contains(x, y) { y - b.thumb.y as f64 } else { b.thumb.h as f64 / 2.0 };
            self.hv.drag_bar = Some(grab);
            if let Some(l) = h.laid.as_ref().map(|l| &l.1) {
                h.scroll = b.pos_for(y, grab, l.h as f64, h.view.h as f64);
            }
            return true;
        }
        if let Some((_, hit)) = h.hits.iter().find(|(r, _)| r.contains(x, y)).cloned() {
            self.hover_activate(hit);
            return true;
        }
        if h.view.contains(x, y) {
            // A press puts the caret between two chars; dragging extends the selection from there.
            h.sel = None;
            if let Some(pt) = pos_at(h, &mut self.tx, x, y) {
                self.hv.drag_sel = Some(pt);
            }
        }
        true
    }

    pub fn hover_mouse_up(&mut self) {
        self.hv.drag_bar = None;
        self.hv.drag_sel = None;
    }

    pub fn hover_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let w = self.wheel_in(delta);
        let Some(h) = &mut self.hover else { return false };
        if !h.rect.contains(x, y) {
            return false;
        }
        let d = if w.dy != 0.0 { w.dy } else { w.dx };
        clamp_scroll(h);
        let max = max_scroll(h);
        h.scr.adopt(h.scroll);
        if w.pixels {
            h.scr.pixels(d, max, scroller::now());
        } else {
            h.scr.wheel(d, 3.0 * PROSE_LH as f64, max, scroller::now());
        }
        h.scroll = h.scr.pos;
        true
    }

    /// Per-frame glide of the wheel scroll. True while the popup is still moving.
    pub fn hover_frame(&mut self, dt: f64) -> bool {
        let Some(h) = &mut self.hover else { return false };
        let max = max_scroll(h);
        h.scr.adopt(h.scroll);
        let moving = h.scr.step(dt, max);
        h.scroll = h.scr.pos;
        moving
    }

    /// Touchpad fingers lifted over the popup: coast.
    pub fn hover_wheel_end(&mut self, now: f64) -> bool {
        let (x, y) = self.mouse;
        let Some(h) = &mut self.hover else { return false };
        if !h.rect.contains(x, y) {
            return false;
        }
        h.scr.adopt(h.scroll);
        h.scr.fling(now);
        true
    }

    /// Esc closes; Ctrl+C copies the selected lines. Returns true when the key was used.
    pub fn hover_key(&mut self, ev: &KeyIn) -> bool {
        let ctrl = self.mods.control_key();
        let Some(h) = &mut self.hover else { return false };
        match &ev.key {
            Key::Named(NamedKey::Escape) => {
                self.hover = None;
                self.request_redraw();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                let Some(text) = sel_text(h, &mut self.tx) else { return false };
                self.clip.set(&text);
                self.say("copied");
                true
            }
            _ => false,
        }
    }

    fn hover_activate(&mut self, hit: Hit) {
        match hit {
            Hit::Act(a) => self.hover_run(a),
            Hit::Link(u) => self.hover_open_link(&u),
        }
    }

    /// Run a footer action on the hovered symbol (the caret moves onto it first: the existing
    /// commands work on the caret).
    pub fn hover_run(&mut self, act: Act) {
        let Some(h) = self.hover.take() else { return };
        let target = if act == Act::QuickFix {
            h.extra
                .diags
                .first()
                .and_then(|d| self.docs.get(self.active).map(|doc| doc.buf.pos(d.line, d.col)))
                .unwrap_or(h.anchor.0)
        } else {
            h.anchor.0
        };
        if act == Act::CopyType {
            let text = h.type_text();
            self.clip.set(&text);
            self.say("type copied");
            return;
        }
        if let Some(d) = self.docs.get_mut(self.active) {
            let p = target.min(d.buf.len_chars());
            d.ed.click(&mut d.buf, p, false);
        }
        match act {
            Act::Definition => self.goto_definition(DefKind::Definition),
            Act::TypeDefinition => self.goto_definition(DefKind::TypeDefinition),
            Act::References => self.find_references(),
            Act::QuickFix => self.code_actions(),
            Act::CopyType => {}
        }
        self.request_redraw();
    }

    fn hover_open_link(&mut self, url: &str) {
        let tip = self.hover.clone();
        if let Some(r) = url.strip_prefix("lumen:goto:") {
            let (p, frag) = r.rsplit_once('#').unwrap_or((r, "0,0"));
            let mut it = frag.split(',').filter_map(|v| v.parse::<usize>().ok());
            let (l, c) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
            self.hover = None;
            self.push_nav();
            self.open_path(std::path::Path::new(p), Some((l, c)));
        } else if url == "lumen:more" {
            self.hover_run(Act::Definition);
        } else if url == "lumen:more-type" {
            self.hover_run(Act::TypeDefinition);
        } else if let Some(rest) = url.strip_prefix("file://") {
            // `file:///x.ts#18,13-18,17`: 1-based line and column
            let (p, frag) = rest.split_once('#').unwrap_or((rest, ""));
            let nums: Vec<usize> = frag.split(|c: char| !c.is_ascii_digit()).filter_map(|v| v.parse().ok()).collect();
            let (l, c) = (
                nums.first().copied().unwrap_or(1).saturating_sub(1),
                nums.get(1).copied().unwrap_or(1).saturating_sub(1),
            );
            if let Some(path) = uri_to_path(&format!("file://{p}")) {
                self.hover = None;
                self.push_nav();
                self.open_path(&path, Some((l, c)));
            }
        } else if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
            open_external(url);
        }
        let _ = tip;
    }

    // ───────────── drawing ─────────────

    pub fn draw_hover(&mut self, cv: &mut Canvas) {
        let Some(mut h) = self.hover.take() else { return };
        if h.is_empty() {
            return;
        }
        self.draw_tip(cv, &mut h);
        self.hover = Some(h);
    }

    fn draw_tip(&mut self, cv: &mut Canvas, h: &mut HoverTip) {
        let chrome = self.theme.chrome;
        let ed = self.layout.editor.intersect(&cv.bounds());
        let maxw = (ed.w - 40).clamp(240, 640);
        let inner_w = maxw - 2 * PAD_X;
        let key: LayKey =
            (inner_w, font_size().to_bits(), self.tx.cell_w.to_bits(), crate::ui::scale().to_bits(), h.blocks_ver);
        if h.laid.as_ref().is_none_or(|(k, _)| *k != key) {
            let lang_blocks = h.blocks.clone();
            let App { tx, theme, tokenizers, assets, .. } = &mut *self;
            let pal = Pal::new(theme);
            let mut hl = |lang: &str, text: &str| -> Option<Vec<Vec<(usize, usize, Rgba, u8)>>> {
                let scope = scope_for(lang)?;
                let tk = match tokenizers.get(scope) {
                    Some(t) => t.clone(),
                    None => {
                        let t = crate::highlight::make_tokenizer(assets, scope)?;
                        tokenizers.insert(scope.to_string(), t.clone());
                        t
                    }
                };
                Some(crate::highlight::highlight_text(&tk, theme, text))
            };
            let mut env = Env::new(tx, pal, &mut hl);
            let laid = layout(&lang_blocks, inner_w, &mut env);
            h.laid = Some((key, laid));
        }
        let pal = Pal::new(&self.theme);
        // footer
        let acts = h.actions();
        let mut widths: Vec<i32> = acts.iter().map(|a| self.tx.ui_width(a.label(), false)).collect();
        let gap = 16;
        let foot_total: i32 = widths.iter().sum::<i32>() + gap * widths.len().saturating_sub(1) as i32;
        let laid = &h.laid.as_ref().unwrap().1;
        let content_w = laid.w.max(foot_total.min(inner_w)).max(110);
        let w = content_w + 2 * PAD_X;
        let rows = footer_rows(&widths, gap, content_w);
        let foot_h = if acts.is_empty() { 0 } else { rows.len() as i32 * FOOT_ROW + 7 };
        let want_h = laid.h + 2 * PAD_Y + foot_h;
        let anchor = self.docs.get(self.active).and_then(|d| d.screen_pos(h.anchor.0)).unwrap_or(h.at);
        let (rect, below) = place(anchor, line_h() as i32, ed, w, want_h, h.side_below);
        h.side_below = Some(below);
        h.rect = rect;
        let view = Rect::new(rect.x + PAD_X, rect.y + PAD_Y, rect.w - 2 * PAD_X, (rect.h - 2 * PAD_Y - foot_h).max(20));
        h.view = view;
        clamp_scroll(h);

        cv.shadow(rect, 10);
        cv.card(rect, 10, chrome.widget_bg, chrome.border);
        let _laid = &h.laid.as_ref().unwrap().1;
        let oy = view.y - h.scroll.round() as i32;
        let clip = Rect::new(rect.x + 1, view.y, rect.w - 2, view.h);
        let (vt, vb) = (h.scroll.round() as i32, h.scroll.round() as i32 + view.h);

        // selection (per character)
        for r in sel_rects(h, &mut self.tx) {
            cv.fill_clip(Rect::new(view.x + r.x, oy + r.y, r.w, r.h), chrome.accent.with_alpha(0x40), clip);
        }
        let laid = &h.laid.as_ref().unwrap().1;
        for d in &laid.decos {
            if d.y + d.h < vt || d.y > vb {
                continue;
            }
            let r = Rect::new(view.x + d.x, oy + d.y, if d.w == 0 { view.w - d.x } else { d.w }, d.h);
            match d.kind {
                DecoKind::Fill(c, rad) => cv.fill_round_clip(r, rad, c, clip),
                DecoKind::Rule(c) => cv.fill_clip(r, c, clip),
            }
        }
        let first = laid.lines.partition_point(|l| l.y + l.h <= vt);
        for l in laid.lines.iter().skip(first) {
            if l.y >= vb {
                break;
            }
            let y = oy + l.y;
            if !l.runs.is_empty() {
                let pieces: Vec<(String, Rgba, u8)> = l.runs.clone();
                self.tx.draw_styled(cv, view.x + l.x0, y, &pieces, clip);
            }
            for s in &l.segs {
                let x = view.x + s.x;
                let hot_link = s.link.is_some_and(|li| self.hv.hot == Some(Hit::Link(laid.urls[li].clone())));
                if s.mono {
                    let ly = y + (l.h - line_h() as i32) / 2;
                    self.tx.draw_line(cv, x, ly, &s.text, s.color, clip);
                } else if let Some(sz) = s.size {
                    self.tx.draw_ui_sized_i(cv, x, y + 2, &s.text, s.color, clip, s.bold, s.italic, sz);
                } else if s.italic {
                    self.tx.draw_ui_italic(cv, x, y, &s.text, s.color, clip, s.bold);
                } else {
                    self.tx.draw_ui(cv, x, y, &s.text, s.color, clip, s.bold);
                }
                if s.link.is_some() {
                    cv.fill_clip(
                        Rect::new(x, y + l.h - 4, s.w, 1),
                        s.color.with_alpha(if hot_link { 0xff } else { 0x70 }),
                        clip,
                    );
                }
                if s.strike {
                    cv.fill_clip(Rect::new(x, y + l.h / 2, s.w, 1), s.color.with_alpha(0xc0), clip);
                }
            }
        }
        // links hit areas
        h.hits.clear();
        for (r, li) in &laid.links {
            let sr = Rect::new(view.x + r.x, oy + r.y, r.w, r.h).intersect(&view);
            if sr.w > 0 && sr.h > 0 {
                h.hits.push((sr, link_target(&laid.urls[*li])));
            }
        }
        // scrollbar
        let track = Rect::new(rect.right() - BAR_W - 2, view.y, BAR_W, view.h);
        h.bar = VBar::new(track, laid.h as f64, view.h as f64, h.scroll);
        if let Some(b) = h.bar {
            let near = (self.mouse.0 >= rect.right() as f64 - 40.0) && rect.contains(self.mouse.0, self.mouse.1);
            b.draw(cv, pal.fg, near, self.hv.drag_bar.is_some());
        }
        // footer
        if foot_h > 0 {
            let fy = rect.bottom() - foot_h;
            cv.fill_clip(Rect::new(rect.x + 1, fy, rect.w - 2, 1), pal.rule, rect);
            let mut k = 0;
            let mut y = fy + 5;
            let fclip = Rect::new(rect.x + 1, fy, rect.w - 2, foot_h - 1);
            for row in &rows {
                let mut x = view.x;
                for _ in row {
                    let a = acts[k];
                    let wd = widths[k];
                    let r = Rect::new(x - 5, y, wd + 10, FOOT_ROW - 2);
                    let hot = self.hv.hot == Some(Hit::Act(a));
                    if hot {
                        cv.fill_round_clip(r, 5, pal.fg.with_alpha(0x1c), fclip);
                    }
                    let col = if hot { pal.link } else { pal.dim };
                    self.tx.draw_ui(cv, x, y + 2, a.label(), col, fclip, false);
                    h.hits.push((r, Hit::Act(a)));
                    x += wd + gap;
                    k += 1;
                }
                y += FOOT_ROW;
            }
        }
        widths.clear();
    }
}

/// Footer action links grouped into rows of at most `w` px.
pub fn footer_rows(widths: &[i32], gap: i32, w: i32) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut cur = 0;
    for (i, &wd) in widths.iter().enumerate() {
        if rows.last().is_none() || cur + gap + wd > w {
            rows.push(vec![]);
            cur = 0;
        }
        cur += if rows.last().unwrap().is_empty() { wd } else { gap + wd };
        rows.last_mut().unwrap().push(i);
    }
    rows
}

fn max_scroll(h: &HoverTip) -> f64 {
    let total = h.laid.as_ref().map_or(0, |l| l.1.h) as f64;
    (total - h.view.h as f64).max(0.0)
}

fn clamp_scroll(h: &mut HoverTip) {
    h.scroll = h.scroll.clamp(0.0, max_scroll(h));
}

/// Index of the laid-out line at screen (x, y).
fn line_at(h: &HoverTip, _x: f64, y: f64) -> Option<usize> {
    let l = &h.laid.as_ref()?.1;
    let cy = (y - h.view.y as f64 + h.scroll) as i32;
    let i = l.lines.partition_point(|ln| ln.y + ln.h <= cy);
    (!l.lines.is_empty()).then(|| i.min(l.lines.len() - 1))
}

/// Every visible char of a line with its x extent: code runs on the monospace grid, text segments
/// measured glyph by glyph. A gap between two segments counts as one space.
pub fn line_boxes(tx: &mut TextCtx, l: &LLine) -> Vec<CharBox> {
    let mut out: Vec<CharBox> = Vec::new();
    let mut i = 0usize;
    for (t, ..) in &l.runs {
        for ch in t.chars().filter(|c| *c != '\n') {
            out.push(CharBox { ch, x0: l.x0 + tx.cells(i), x1: l.x0 + tx.cells(i + 1) });
            i += 1;
        }
    }
    for seg in &l.segs {
        if seg.text.is_empty() {
            continue;
        }
        if let Some(last) = out.last().copied()
            && seg.x > last.x1 + 1
            && !last.ch.is_whitespace()
        {
            out.push(CharBox { ch: ' ', x0: last.x1, x1: seg.x });
        }
        if seg.mono {
            for (k, ch) in seg.text.chars().enumerate() {
                out.push(CharBox { ch, x0: seg.x + tx.cells(k), x1: seg.x + tx.cells(k + 1) });
            }
        } else {
            let xs = tx.ui_char_xs(&seg.text, seg.bold, seg.size);
            for (k, ch) in seg.text.chars().enumerate() {
                out.push(CharBox { ch, x0: seg.x + xs[k].round() as i32, x1: seg.x + xs[k + 1].round() as i32 });
            }
        }
    }
    out
}

/// The cached char boxes of line `line` of the current layout.
fn boxes_of<'a>(h: &'a mut HoverTip, tx: &mut TextCtx, line: usize) -> &'a [CharBox] {
    let key = h.laid.as_ref().map(|l| l.0);
    if h.cells.0 != key || key.is_none() {
        h.cells = (key, Default::default());
    }
    if !h.cells.1.contains_key(&line) {
        let b = h.laid.as_ref().and_then(|l| l.1.lines.get(line)).map(|l| line_boxes(tx, l)).unwrap_or_default();
        h.cells.1.insert(line, b);
    }
    &h.cells.1[&line]
}

/// Char boundary of `boxes` nearest to content-relative x (0 = before the first char).
pub fn col_at(boxes: &[CharBox], x: i32) -> usize {
    boxes.iter().position(|b| x < (b.x0 + b.x1 + 1) / 2).unwrap_or(boxes.len())
}

/// The selection point under screen (x, y): nearest line, nearest char boundary.
fn pos_at(h: &mut HoverTip, tx: &mut TextCtx, x: f64, y: f64) -> Option<SelPt> {
    let line = line_at(h, x, y)?;
    let rel = (x - h.view.x as f64).round() as i32;
    Some((line, col_at(boxes_of(h, tx, line), rel)))
}

fn ordered(a: SelPt, b: SelPt) -> (SelPt, SelPt) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Highlight rectangles of the selection, relative to the content origin (x) and the top of the
/// laid-out content (y).
fn sel_rects(h: &mut HoverTip, tx: &mut TextCtx) -> Vec<Rect> {
    let Some((a, b)) = h.sel else { return vec![] };
    let ((l0, c0), (l1, c1)) = ordered(a, b);
    let n = h.laid.as_ref().map_or(0, |l| l.1.lines.len());
    let mut out = Vec::new();
    for line in l0..=l1.min(n.saturating_sub(1)) {
        let (y, lh) = match h.laid.as_ref().and_then(|l| l.1.lines.get(line)) {
            Some(l) => (l.y, l.h),
            None => break,
        };
        let boxes = boxes_of(h, tx, line);
        let from = if line == l0 { c0 } else { 0 }.min(boxes.len());
        let to = if line == l1 { c1 } else { boxes.len() }.min(boxes.len());
        if from < to {
            // A line that continues into the next one is highlighted a little past its end.
            let tail = if line < l1 { 4 } else { 0 };
            out.push(Rect::new(boxes[from].x0, y, boxes[to - 1].x1 - boxes[from].x0 + tail, lh));
        }
    }
    out
}

/// The selected text, exactly the highlighted chars, lines joined with `\n`.
pub fn sel_text(h: &mut HoverTip, tx: &mut TextCtx) -> Option<String> {
    let (a, b) = h.sel?;
    let ((l0, c0), (l1, c1)) = ordered(a, b);
    let n = h.laid.as_ref()?.1.lines.len();
    if n == 0 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for line in l0..=l1.min(n - 1) {
        let boxes = boxes_of(h, tx, line);
        let from = if line == l0 { c0 } else { 0 }.min(boxes.len());
        let to = if line == l1 { c1 } else { boxes.len() }.min(boxes.len());
        parts.push(boxes[from.min(to)..to].iter().map(|b| b.ch).collect());
    }
    let text = parts.join("\n");
    (!text.is_empty()).then_some(text)
}

fn open_external(url: &str) {
    let url = url.to_string();
    std::thread::spawn(move || {
        let child = std::process::Command::new("xdg-open")
            .arg(&url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            let _ = c.wait();
        }
    });
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Theme {
        let assets = crate::config::assets_dir();
        Theme::load(&assets.join("themes/dark-plus.json")).unwrap()
    }

    fn laid(blocks: &[Blk], w: i32) -> Laid {
        let mut tx = TextCtx::new(None);
        let th = dark();
        let mut hl = |_: &str, _: &str| None;
        let mut env = Env::new(&mut tx, Pal::new(&th), &mut hl);
        layout(blocks, w, &mut env)
    }

    #[test]
    fn prose_wraps_to_width_and_keeps_links() {
        let long = "palavra ".repeat(60);
        let blocks = vec![Blk::Para(vec![
            Span::plain(long),
            Span { text: "link".into(), link: Some("http://x".into()), ..Default::default() },
        ])];
        let l = laid(&blocks, 300);
        assert!(l.lines.len() > 3, "wrapped into {} lines", l.lines.len());
        for ln in &l.lines {
            let right = ln.segs.iter().map(|s| s.x + s.w).max().unwrap_or(0);
            assert!(right <= 300 + 6, "line too wide: {right}");
        }
        assert_eq!(l.links.len(), 1);
        assert_eq!(l.h, l.lines.len() as i32 * PROSE_LH);
    }

    #[test]
    fn code_wraps_at_commas_and_never_loses_text() {
        let line = "function configure(alpha: string, beta: number, gamma: boolean, delta: string[]): Promise<void>";
        let chars = code_chars(line, None, Rgba(1, 2, 3, 255));
        let parts = wrap_code(chars, 40);
        assert!(parts.len() >= 3);
        for p in &parts {
            assert!(p.len() <= 40);
        }
        let joined: String = parts
            .iter()
            .map(|p| p.iter().map(|c| c.0).collect::<String>().trim().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined.split_whitespace().collect::<Vec<_>>().join(" "), line);
        assert!(parts[0].iter().map(|c| c.0).collect::<String>().trim_end().ends_with(','));
    }

    #[test]
    fn code_block_gets_background_and_lines() {
        let l = laid(&[Blk::Code { text: "const a = 1;\nconst b = 2;".into(), lang: "ts".into() }], 400);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.decos.len(), 1);
        assert!(l.h >= 2 * line_h() as i32 + 10);
    }

    #[test]
    fn placement_stays_inside_bounds() {
        let b = Rect::new(0, 30, 800, 500);
        for (ax, ay) in [(0, 40), (790, 40), (400, 520), (400, 300), (790, 540), (-20, 300)] {
            for want in [40, 200, 480, 900] {
                for w in [100, 500, 900] {
                    let (r, _) = place((ax, ay), 20, b, w, want, None);
                    assert!(r.x >= b.x && r.right() <= b.right(), "x {r:?} for {ax},{ay},{want},{w}");
                    assert!(r.y >= b.y && r.bottom() <= b.bottom(), "y {r:?} for {ax},{ay},{want},{w}");
                }
            }
        }
        // prefers above when it fits, below otherwise
        let (r, below) = place((100, 300), 20, b, 300, 100, None);
        assert!(!below && r.bottom() <= 300);
        let (r, below) = place((100, 60), 20, b, 300, 200, None);
        assert!(below && r.y >= 80);
        // keeps its side while still fitting
        let (_, below) = place((100, 300), 20, b, 300, 100, Some(true));
        assert!(below);
    }

    #[test]
    fn footer_rows_wrap() {
        assert_eq!(footer_rows(&[100, 100, 100], 10, 400), vec![vec![0, 1, 2]]);
        assert_eq!(footer_rows(&[100, 100, 100], 10, 250), vec![vec![0, 1], vec![2]]);
        assert_eq!(footer_rows(&[300], 10, 100), vec![vec![0]]);
    }

    #[test]
    fn byte_offsets_pos_from_lspitions() {
        let t = "ab\ncdé😀x\nz";
        assert_eq!(byte_off(t, 0, 1), Some(1));
        assert_eq!(byte_off(t, 1, 0), Some(3));
        // é is 1 utf16 unit, 😀 is 2
        assert_eq!(byte_off(t, 1, 5), Some(3 + 1 + 1 + 2 + 4));
        assert_eq!(byte_off(t, 2, 1), Some(t.len()));
        assert_eq!(byte_off(t, 5, 0), None);
    }

    #[test]
    fn literals_under_the_pointer() {
        let line = r#"  name: "Ana", age: 42, ok: true, p: 3.14,"#;
        let at = |s: &str| line.find(s).unwrap();
        let (a, b) = literal_at(line, at("Ana")).unwrap();
        assert_eq!(&line[a..b], "\"Ana\"");
        let (a, b) = literal_at(line, at("42")).unwrap();
        assert_eq!(&line[a..b], "42");
        assert_eq!(literal_at(line, at("ok")), None);
        let (a, b) = literal_at(line, at("true")).unwrap();
        assert_eq!(&line[a..b], "true");
        let (a, b) = literal_at(line, at("3.14")).unwrap();
        assert_eq!(&line[a..b], "3.14");
        assert_eq!(describe_literal("\"Ana\""), "string · 3 caracteres");
    }

    #[test]
    fn tip_composes_params_returns_and_footer() {
        let md = "```typescript\nfunction sum(a: number, b?: number): number\n```\nSoma.\n\n*@param* `a` — primeiro\n\n*@returns* — total\n\n*@deprecated* — use `add`";
        let t = HoverTip::new(md, (0, 3), (0, 0));
        assert_eq!(t.kind, Kind::Function);
        assert!(matches!(t.blocks[0], Blk::Note { .. }), "deprecated first: {:?}", t.blocks[0]);
        let heads: Vec<String> = t
            .blocks
            .iter()
            .filter_map(|b| if let Blk::Item { head: Some(h), .. } = b { Some(h.clone()) } else { None })
            .collect();
        assert_eq!(heads, ["a: number", "b?: number", "number"]);
        let acts = t.actions();
        assert_eq!(acts, [Act::Definition, Act::References, Act::CopyType]);
        assert_eq!(t.type_text(), "function sum(a: number, b?: number): number");
    }

    #[test]
    fn expanded_declaration_replaces_the_summary() {
        let mut t = HoverTip::new("```typescript\ninterface User\n```\nUm usuário.", (0, 4), (0, 0));
        assert_eq!(t.kind, Kind::Interface);
        t.extra.decl = Some(Decl { text: "interface User {\n  id: number;\n}".into(), more: 3, kind: Kind::Interface });
        t.compose();
        let codes: Vec<&str> = t
            .blocks
            .iter()
            .filter_map(|b| if let Blk::Code { text, .. } = b { Some(text.as_str()) } else { None })
            .collect();
        assert_eq!(codes, ["interface User {\n  id: number;\n}"]);
        assert!(t.blocks.iter().any(|b| matches!(b, Blk::Dim(_))));
        assert_eq!(t.type_text(), "interface User {\n  id: number;\n}");
    }

    #[test]
    fn diagnostics_come_first_with_quick_fix() {
        let mut t = HoverTip::diag_only(
            vec![DiagInfo {
                sev: 1,
                msg: "Wrong type".into(),
                code: "2322".into(),
                url: Some("https://typescript.tv/errors/#TS2322".into()),
                line: 0,
                col: 0,
            }],
            (0, 1),
            (0, 0),
        );
        assert!(matches!(&t.blocks[0], Blk::Diag { code, .. } if code == "2322"));
        assert_eq!(t.actions(), [Act::QuickFix]);
        t.extra.diags.clear();
        t.compose();
        assert!(t.is_empty());
    }

    #[test]
    fn scroll_is_clamped() {
        let mut t = HoverTip::new("```ts\nconst a: 1\n```\n", (0, 1), (0, 0));
        t.laid = Some(((0, 0, 0, 0, 0), Laid { h: 500, ..Default::default() }));
        t.view = Rect::new(0, 0, 100, 200);
        t.scroll = 900.0;
        clamp_scroll(&mut t);
        assert_eq!(t.scroll, 300.0);
        t.scroll = -5.0;
        clamp_scroll(&mut t);
        assert_eq!(t.scroll, 0.0);
    }

    // ───────────── per-character selection ─────────────

    fn tip_with(blocks: &[Blk], tx: &mut TextCtx) -> HoverTip {
        let th = dark();
        let mut hl = |_: &str, _: &str| None;
        let l = {
            let mut env = Env::new(tx, Pal::new(&th), &mut hl);
            layout(blocks, 400, &mut env)
        };
        let mut t = HoverTip::new("", (0, 0), (0, 0));
        t.laid = Some(((7, 7, 7, 7, 7), l));
        t.view = Rect::new(10, 10, 400, 300);
        t
    }

    #[test]
    fn selection_is_per_character_and_copies_exactly() {
        let mut tx = TextCtx::new(None);
        let blocks = vec![
            Blk::Para(vec![Span::plain("hello wonderful world")]),
            Blk::Code { text: "const a = 1;\nconst b = 22;".into(), lang: "ts".into() },
        ];
        let mut t = tip_with(&blocks, &mut tx);
        let boxes = boxes_of(&mut t, &mut tx, 0).to_vec();
        assert_eq!(boxes.iter().map(|b| b.ch).collect::<String>(), "hello wonderful world");
        assert!(
            boxes.windows(2).all(|w| w[0].x0 < w[1].x0 && w[0].x1 <= w[1].x0 + 1),
            "chars advance left to right: {boxes:?}"
        );
        // inside one line
        t.sel = Some(((0, 6), (0, 15)));
        assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("wonderful"));
        t.sel = Some(((0, 15), (0, 6)));
        assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("wonderful"), "order does not matter");
        let rects = sel_rects(&mut t, &mut tx);
        assert_eq!(rects.len(), 1);
        assert_eq!((rects[0].x, rects[0].right()), (boxes[6].x0, boxes[14].x1));
        // across lines: tail of the prose, a whole code line, head of the next
        t.sel = Some(((0, 16), (2, 5)));
        assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("world\nconst a = 1;\nconst"));
        assert_eq!(sel_rects(&mut t, &mut tx).len(), 3);
        // an empty selection copies nothing
        t.sel = Some(((1, 3), (1, 3)));
        assert_eq!(sel_text(&mut t, &mut tx), None);
        t.sel = None;
        assert_eq!(sel_text(&mut t, &mut tx), None);
    }

    #[test]
    fn pointer_maps_to_the_nearest_char_boundary() {
        let mut tx = TextCtx::new(None);
        let mut t = tip_with(&[Blk::Para(vec![Span::plain("abcdef ghij")])], &mut tx);
        let b = boxes_of(&mut t, &mut tx, 0).to_vec();
        let (vx, vy) = (t.view.x as f64, t.view.y as f64 + 3.0);
        // just inside the left half of char 3 -> boundary 3; right half -> boundary 4
        assert_eq!(pos_at(&mut t, &mut tx, vx + b[3].x0 as f64 + 0.5, vy), Some((0, 3)));
        assert_eq!(pos_at(&mut t, &mut tx, vx + b[3].x1 as f64 - 0.5, vy), Some((0, 4)));
        // before the text / past the end / far below
        assert_eq!(pos_at(&mut t, &mut tx, vx - 30.0, vy), Some((0, 0)));
        assert_eq!(pos_at(&mut t, &mut tx, vx + 390.0, vy), Some((0, b.len())));
        assert_eq!(pos_at(&mut t, &mut tx, vx + 5.0, vy + 250.0).map(|p| p.0), Some(0));
    }

    #[test]
    fn code_lines_select_on_the_monospace_grid() {
        let mut tx = TextCtx::new(None);
        let mut t = tip_with(&[Blk::Code { text: "let value = 42;".into(), lang: "ts".into() }], &mut tx);
        t.sel = Some(((0, 4), (0, 9)));
        assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("value"));
        let b = boxes_of(&mut t, &mut tx, 0).to_vec();
        let step = b[1].x0 - b[0].x0;
        assert!(step > 3 && b.windows(2).all(|w| (w[1].x0 - w[0].x0 - step).abs() <= 1), "fixed-width cells");
    }

    #[test]
    fn inline_code_and_styled_runs_keep_their_text() {
        let mut tx = TextCtx::new(None);
        let sp = vec![
            Span::plain("use "),
            Span::code("add"),
            Span::plain(" or "),
            Span { text: "sum".into(), bold: true, ..Default::default() },
            Span::plain(" now"),
        ];
        let mut t = tip_with(&[Blk::Para(sp)], &mut tx);
        let b = boxes_of(&mut t, &mut tx, 0).to_vec();
        assert_eq!(b.iter().map(|c| c.ch).collect::<String>(), "use add or sum now");
        t.sel = Some(((0, 4), (0, 10)));
        assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("add or"));
    }
}
