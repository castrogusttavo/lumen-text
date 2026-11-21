//! The native chat of the AI panel: the transcript of a [`Chat`] drawn with the editor's own
//! canvas (no terminal, no browser). User messages are right-aligned cards, the assistant's text is
//! rendered Markdown (the editor's preview renderer, one per text block), tool calls are
//! collapsible cards with their input, a diff for edits and their output, thinking is a collapsible
//! line, and a permission the agent asks for is a card with Allow / Deny. The column is centered
//! and at most [`MAX_COL`] px wide, so a full-page panel stays readable. Pointer rules are plain
//! functions ([`cursor_kind`]) with unit tests; every clickable area is registered in `hits`.

use super::{
    backend::Backend,
    chat::{self, Block, Chat, DiffKind, Message, Phase, Role, ToolCall, ToolStatus},
};
use crate::{
    mdview::{Act, MdView},
    theme::Theme,
    ui::{Canvas, Rect, TextCtx, VBar, fit_ellipsis, line_h, ui_line},
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Instant,
};

/// Widest the conversation column gets.
pub const MAX_COL: i32 = 760;
const PADX: i32 = 16;
const SMALL: f32 = 12.0;
const HEAD_H: i32 = 30;
/// Lines of a tool's output / diff shown before "… more".
const BODY_MAX: usize = 14;
const DIFF_MAX: usize = 40;

/// A structured conversation with one agent CLI.
pub struct ChatSession {
    pub chat: Chat,
    pub be: Backend,
    pub view: ChatView,
    pub started: Instant,
}

impl ChatSession {
    pub fn new(be: Backend) -> ChatSession {
        ChatSession { chat: Chat::new(), be, view: ChatView::new(), started: Instant::now() }
    }

    pub fn pump(&mut self) -> bool {
        self.be.pump(&mut self.chat)
    }

    /// Send a user message (the transcript follows it to the end).
    pub fn send(&mut self, text: &str) {
        self.view.follow = true;
        self.be.send(&mut self.chat, text);
    }

    /// The last thing the user asked.
    pub fn last_prompt(&self) -> String {
        self.chat
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.blocks.iter().find_map(|b| if let Block::Text(t) = b { Some(t.clone()) } else { None }))
            .unwrap_or_default()
    }
}

/// What the pointer is over, as far as the pointer shape goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cur {
    Default,
    Pointer,
    Text,
}

/// A clickable area of the transcript.
#[derive(Clone, PartialEq, Debug)]
pub enum ChatHit {
    /// A collapsible card's header.
    Toggle(String),
    /// A permission answer: request id, allow.
    Perm(String, bool),
    /// A file named by a tool card (path, line).
    File(String, Option<usize>),
    /// "Jump to the end".
    ToEnd,
    /// A Markdown block (text selection, links, copy buttons).
    Md((usize, usize)),
    /// The copy button of a message (its whole text).
    CopyMsg(usize),
}

/// Pointer shape for what is under it: hand on anything clickable, I-beam over text (a hand on a
/// link inside it), the default arrow elsewhere.
pub fn cursor_kind(hit: Option<&ChatHit>, md_clickable: bool) -> Cur {
    match hit {
        None => Cur::Default,
        Some(ChatHit::Md(_)) if md_clickable => Cur::Pointer,
        Some(ChatHit::Md(_)) => Cur::Text,
        Some(_) => Cur::Pointer,
    }
}

/// What a press on a Markdown block did.
#[derive(Debug, PartialEq)]
pub enum MdPress {
    Nothing,
    Link(String),
    Copy(String),
    Toggled,
    Selecting,
}

struct MdSlot {
    view: MdView,
    src: String,
    rect: Rect,
    scroll: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ln {
    Add,
    Del,
    Plain,
    Dim,
    Err,
    Label,
}

enum Item {
    Head,
    User(Vec<String>, i32, usize),
    Md((usize, usize)),
    Think { key: String, open: bool, lines: Vec<String>, live: bool },
    Tool { key: String, mi: usize, bi: usize, open: bool, body: Vec<(Ln, String)> },
    Error(Vec<String>),
    Perm(usize, Vec<String>),
    Working,
    Empty,
}

struct Row {
    item: Item,
    h: i32,
    /// Space left above it.
    gap: i32,
}

#[derive(Default)]
pub struct ChatView {
    /// Scroll offset from the top of the transcript.
    pub scroll: f64,
    /// Stuck to the end: new output scrolls into view.
    pub follow: bool,
    pub content_h: i32,
    pub view_h: i32,
    pub body: Rect,
    toggled: HashSet<String>,
    mds: HashMap<(usize, usize), MdSlot>,
    pub hits: Vec<(Rect, ChatHit)>,
    exists: HashMap<String, bool>,
    was_working: bool,
    sel: Option<(usize, usize)>,
    pub dragging: bool,
    last_sig: u64,
    /// Last frame drawn for a spinner (throttles the animation).
    pub anim: Option<Instant>,
}

fn mono_h() -> i32 {
    (line_h() - 2.0).round().max(14.0) as i32
}

fn small_h() -> i32 {
    (SMALL * 1.4).round() as i32
}

fn clean(l: &str) -> String {
    let t = l.replace('\t', "    ");
    if t.chars().count() > 400 { t.chars().take(400).collect() } else { t }
}

impl ChatView {
    pub fn new() -> ChatView {
        ChatView { follow: true, ..Default::default() }
    }

    fn open_of(&self, key: &str, default: bool) -> bool {
        default ^ self.toggled.contains(key)
    }

    pub fn toggle(&mut self, key: &str) {
        if !self.toggled.remove(key) {
            self.toggled.insert(key.to_string());
        }
    }

    pub fn hit_at(&self, x: f64, y: f64) -> Option<(Rect, ChatHit)> {
        if !self.body.contains(x, y) {
            return None;
        }
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).cloned()
    }

    /// The pointer shape at (x, y).
    pub fn cursor_at(&self, x: f64, y: f64) -> Cur {
        let hit = self.hit_at(x, y);
        let clickable = match &hit {
            Some((_, ChatHit::Md(k))) => self.mds.get(k).is_some_and(|s| s.view.clickable_at(x, y, s.rect, s.scroll)),
            _ => false,
        };
        cursor_kind(hit.as_ref().map(|(_, h)| h), clickable)
    }

    /// The pointer moved: true when what is under it changed (a hover to redraw).
    pub fn moved(&mut self, x: f64, y: f64) -> bool {
        let mut sig = 0u64;
        if let Some((r, h)) = self.hit_at(x, y) {
            sig = (r.x as u64) << 40 ^ (r.y as u64) << 20 ^ r.w as u64 ^ 1;
            if let ChatHit::Md(k) = h
                && let Some(s) = self.mds.get(&k)
            {
                sig ^= s.view.hover_sig(x, y, s.rect, s.scroll).rotate_left(7);
            }
        }
        let changed = sig != self.last_sig;
        self.last_sig = sig;
        changed
    }

    pub fn scroll_by(&mut self, px: f64) {
        let max = (self.content_h - self.view_h).max(0) as f64;
        self.scroll = (self.scroll + px).clamp(0.0, max);
        self.follow = max - self.scroll < 1.0;
    }

    pub fn to_end(&mut self) {
        self.follow = true;
    }

    /// A press on Markdown block `key`.
    pub fn md_press(&mut self, tx: &mut TextCtx, key: (usize, usize), x: f64, y: f64) -> MdPress {
        for (k, s) in self.mds.iter_mut() {
            if *k != key {
                s.view.clear_selection();
            }
        }
        let Some(s) = self.mds.get_mut(&key) else { return MdPress::Nothing };
        if let Some(act) = s.view.hit_at(x, y, s.rect, s.scroll) {
            return match act {
                Act::Copy(k) => match s.view.code_text(k) {
                    Some(t) => {
                        s.view.mark_copied(k);
                        MdPress::Copy(t)
                    }
                    None => MdPress::Nothing,
                },
                Act::Toggle(id) => {
                    s.view.toggle_details(id);
                    MdPress::Toggled
                }
                Act::Code(k) => {
                    s.view.toggle_code(k);
                    MdPress::Toggled
                }
            };
        }
        if let Some(u) = s.view.link_at(x, y, s.rect, s.scroll) {
            return MdPress::Link(u);
        }
        s.view.sel_begin(tx, x, y, s.rect, s.scroll);
        self.sel = Some(key);
        self.dragging = true;
        MdPress::Selecting
    }

    pub fn md_drag(&mut self, tx: &mut TextCtx, x: f64, y: f64) -> bool {
        let Some(key) = self.sel.filter(|_| self.dragging) else { return false };
        let Some(s) = self.mds.get_mut(&key) else { return false };
        let yy = y.clamp(self.body.y as f64, (self.body.bottom() - 1) as f64);
        s.view.sel_extend(tx, x, yy, s.rect, s.scroll);
        true
    }

    /// The text selected in the transcript, if any.
    pub fn selected_text(&self) -> Option<String> {
        let s = self.mds.get(&self.sel?)?;
        s.view.has_selection().then(|| s.view.selected_text()).filter(|t| !t.is_empty())
    }

    // ───────────────────────────── layout ─────────────────────────────

    fn md_height(&mut self, tx: &mut TextCtx, key: (usize, usize), text: &str, cwd: &Path, w: i32) -> i32 {
        let slot = self.mds.entry(key).or_insert_with(|| MdSlot {
            view: MdView::new(),
            src: String::new(),
            rect: Rect::default(),
            scroll: 0.0,
        });
        if slot.src != text {
            slot.src = text.to_string();
            let exists = &mut self.exists;
            let linked = chat::linkify_paths(text, &mut |p: &str| {
                *exists.entry(p.to_string()).or_insert_with(|| {
                    let path = Path::new(p);
                    if path.is_absolute() { path.is_file() } else { cwd.join(path).is_file() }
                })
            });
            slot.view.set_text(&linked);
        }
        slot.view.ensure_layout(tx, w);
        slot.view.content_height().max(ui_line() as i32)
    }

    fn tool_body(t: &ToolCall) -> Vec<(Ln, String)> {
        let mut body: Vec<(Ln, String)> = Vec::new();
        let diff = chat::diff_lines(&t.name, &t.input);
        if diff.is_empty() {
            match t.name.as_str() {
                "Bash" => {
                    let cmd = t.input.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    body.extend(cmd.lines().take(8).map(|l| (Ln::Plain, format!("$ {}", clean(l)))));
                }
                "Read" | "Edit" | "Write" | "MultiEdit" | "Grep" | "Glob" | "WebFetch" | "WebSearch" | "TodoWrite" => {}
                _ => {
                    if let Some(o) = t.input.as_object() {
                        for (k, v) in o.iter().take(6) {
                            let s = match v {
                                serde_json::Value::String(s) => s.lines().next().unwrap_or("").to_string(),
                                other => other.to_string(),
                            };
                            body.push((Ln::Dim, clean(&format!("{k}: {s}"))));
                        }
                    }
                }
            }
        } else {
            let n = diff.len();
            for (k, l) in diff.into_iter().take(DIFF_MAX) {
                body.push((if k == DiffKind::Add { Ln::Add } else { Ln::Del }, clean(&l)));
            }
            if n > DIFF_MAX {
                body.push((Ln::Dim, format!("… {} more lines", n - DIFF_MAX)));
            }
        }
        if let Some(r) = t.result.as_deref().filter(|r| !r.trim().is_empty()) {
            let err = t.status == ToolStatus::Error;
            body.push((Ln::Label, if err { "Error".into() } else { "Output".into() }));
            let (lines, skipped) = chat::tail_lines(r, BODY_MAX);
            if skipped > 0 {
                body.push((Ln::Dim, format!("… {skipped} earlier lines")));
            }
            body.extend(lines.into_iter().map(|l| (if err { Ln::Err } else { Ln::Plain }, clean(l))));
        }
        body
    }

    fn build(&mut self, tx: &mut TextCtx, chat: &Chat, cwd: &Path, cw: i32, view_h: i32) -> Vec<Row> {
        let lh = ui_line().ceil() as i32;
        let mut rows: Vec<Row> = Vec::new();
        let push = |rows: &mut Vec<Row>, item: Item, h: i32, gap: i32| rows.push(Row { item, h, gap });
        if chat.messages.is_empty() && chat.perms.is_empty() {
            push(&mut rows, Item::Empty, view_h.max(200), 0);
            return rows;
        }
        for (mi, m) in chat.messages.iter().enumerate() {
            match m.role {
                Role::User => {
                    let text = user_text(m);
                    let bw_max = (cw * 85 / 100).max(80);
                    let inner = (bw_max - 24) as f32;
                    let lines = chat::wrap_text(&text, inner, &mut |s: &str| tx.ui_char_xs(s, false, None));
                    let widest = lines.iter().map(|l| tx.ui_width(l, false)).max().unwrap_or(0);
                    let bw = (widest + 24).clamp(48, bw_max);
                    let h = lines.len() as i32 * lh + 16;
                    push(&mut rows, Item::User(lines, bw, mi), h, if mi == 0 { 0 } else { 18 });
                }
                Role::Assistant => {
                    push(&mut rows, Item::Head, 22, if mi == 0 { 0 } else { 14 });
                    for (bi, b) in m.blocks.iter().enumerate() {
                        match b {
                            Block::Text(t) if t.trim().is_empty() => {}
                            Block::Text(t) => {
                                let h = self.md_height(tx, (mi, bi), t, cwd, cw);
                                push(&mut rows, Item::Md((mi, bi)), h, 6);
                            }
                            Block::Thinking(t) if t.trim().is_empty() => {}
                            Block::Thinking(t) => {
                                let key = format!("k:{mi}:{bi}");
                                let open = self.open_of(&key, false);
                                let lines = if open {
                                    let w = (cw - 28) as f32;
                                    chat::wrap_text(t.trim(), w, &mut |s: &str| tx.ui_char_xs(s, false, Some(SMALL)))
                                } else {
                                    Vec::new()
                                };
                                let live = chat.working() && mi + 1 == chat.messages.len() && bi + 1 == m.blocks.len();
                                let h = 26 + if open { lines.len() as i32 * small_h() + 8 } else { 0 };
                                push(&mut rows, Item::Think { key, open, lines, live }, h, 4);
                            }
                            Block::Tool(t) => {
                                let key = format!("t:{}", t.id);
                                let default = chat::opens_by_default(&t.name) || t.status == ToolStatus::Asking;
                                let open = self.open_of(&key, default);
                                let body = if open { Self::tool_body(t) } else { Vec::new() };
                                let h = HEAD_H + if body.is_empty() { 0 } else { body.len() as i32 * mono_h() + 14 };
                                push(&mut rows, Item::Tool { key, mi, bi, open, body }, h, 6);
                            }
                            Block::Error(e) => {
                                let w = (cw - 44) as f32;
                                let lines = chat::wrap_text(e, w, &mut |s: &str| tx.ui_char_xs(s, false, None));
                                let h = lines.len() as i32 * lh + 20;
                                push(&mut rows, Item::Error(lines), h, 6);
                            }
                        }
                    }
                }
            }
        }
        for (pi, p) in chat.perms.iter().enumerate() {
            let sum = chat::tool_summary(&p.name, &p.input);
            let text = if p.name == "Bash" {
                p.input.get("command").and_then(|c| c.as_str()).unwrap_or(&sum).to_string()
            } else {
                sum
            };
            let w = (cw - 32) as f32;
            let mut lines = chat::wrap_text(&text, w, &mut |s: &str| tx.ui_char_xs(s, false, None));
            lines.truncate(5);
            let h = 14 + lh + 6 + lines.len() as i32 * lh + 12 + 30 + 14;
            push(&mut rows, Item::Perm(pi, lines), h, 10);
        }
        if chat.phase == Phase::Working {
            push(&mut rows, Item::Working, 30, 8);
        }
        rows
    }

    // ───────────────────────────── drawing ─────────────────────────────

    /// Draw the transcript into `body`. Returns true while something animates (a spinner).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        cv: &mut Canvas,
        tx: &mut TextCtx,
        theme: &Theme,
        body: Rect,
        chat: &Chat,
        agent: (&str, &str),
        cwd: &Path,
        mouse: (f64, f64),
        clock: f32,
    ) -> bool {
        self.hits.clear();
        self.body = body;
        if self.mds.is_empty() && self.content_h == 0 {
            self.follow = true;
        }
        let working = chat.working();
        if self.was_working && !working {
            self.exists.clear();
        }
        self.was_working = working;
        let cw = (body.w - 2 * PADX).clamp(80, MAX_COL);
        let cx = body.x + (body.w - cw) / 2;
        let rows = self.build(tx, chat, cwd, cw, body.h);
        let total = 14 + rows.iter().map(|r| r.h + r.gap).sum::<i32>() + 20;
        self.content_h = total;
        self.view_h = body.h;
        let max = (total - body.h).max(0) as f64;
        if self.follow {
            self.scroll = max;
        } else {
            self.scroll = self.scroll.clamp(0.0, max);
            if max - self.scroll < 1.0 {
                self.follow = true;
            }
        }
        let mut y = body.y + 14 - self.scroll.round() as i32;
        let mut anim = false;
        for row in &rows {
            y += row.gap;
            let r = Rect::new(cx, y, cw, row.h);
            if r.bottom() >= body.y && r.y <= body.bottom() {
                anim |= self.paint_row(cv, tx, theme, body, r, row, chat, agent, cwd, mouse, clock);
            }
            y += row.h;
        }
        let c = theme.chrome;
        if let Some(bar) =
            VBar::new(Rect::new(body.right() - 8, body.y, 8, body.h), total as f64, body.h as f64, self.scroll)
        {
            bar.draw(cv, c.tab_inactive_fg, false, false);
        }
        if !self.follow {
            let d = 32;
            let b = Rect::new(body.x + (body.w - d) / 2, body.bottom() - d - 10, d, d);
            let hot = b.contains(mouse.0, mouse.1);
            cv.shadow(b, d / 2);
            cv.card(b, d / 2, if hot { c.list_hover_bg.over(c.widget_bg) } else { c.widget_bg }, c.border);
            tx.icon(cv, "ui/arrow_down", b.x + 8, b.y + 8, 16, c.tab_active_fg, b);
            self.hits.push((b, ChatHit::ToEnd));
        }
        anim
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_row(
        &mut self,
        cv: &mut Canvas,
        tx: &mut TextCtx,
        theme: &Theme,
        body: Rect,
        r: Rect,
        row: &Row,
        chat: &Chat,
        agent: (&str, &str),
        cwd: &Path,
        mouse: (f64, f64),
        clock: f32,
    ) -> bool {
        let c = theme.chrome;
        let (strong, dim, fg) = (c.tab_active_fg, c.tab_inactive_fg, theme.foreground);
        let lh = ui_line().ceil() as i32;
        let clip = r.intersect(&body);
        let hot = |rr: Rect| rr.contains(mouse.0, mouse.1) && body.contains(mouse.0, mouse.1);
        match &row.item {
            Item::Empty => {
                let disc = Rect::new(r.x + (r.w - 48) / 2, r.y + (r.h - 120).max(0) / 2, 48, 48);
                cv.fill_round_clip(disc, 24, c.accent.with_alpha(0x1c), clip);
                tx.icon(cv, agent.1, disc.x + 12, disc.y + 12, 24, c.accent_text, clip);
                let title = format!("Ask {} anything", agent.0);
                let tw = tx.ui_width(&title, true);
                tx.draw_ui(cv, r.x + (r.w - tw) / 2, disc.bottom() + 14, &title, strong, clip, true);
                let sub = "It works in this project: reads, edits and runs commands.";
                let sub = fit_ellipsis(tx, sub, r.w, false);
                let sw = tx.ui_width_sized(&sub, false, SMALL);
                tx.draw_ui_sized(cv, r.x + (r.w - sw) / 2, disc.bottom() + 40, &sub, dim, clip, false, SMALL);
                false
            }
            Item::Head => {
                tx.icon(cv, agent.1, r.x, r.y + 3, 16, c.accent_text, clip);
                let mut label = agent.0.to_string();
                if let Some(m) = &chat.model {
                    label = format!("{label} · {m}");
                }
                tx.draw_ui_sized(cv, r.x + 24, r.y + 2, &label, dim, clip, false, SMALL);
                false
            }
            Item::User(lines, bw, mi) => {
                let b = Rect::new(r.right() - bw, r.y, *bw, r.h);
                cv.card(b, 12, c.widget_bg, c.border);
                for (k, l) in lines.iter().enumerate() {
                    tx.draw_ui(cv, b.x + 12, b.y + 8 + k as i32 * lh, l, fg, clip.intersect(&b), false);
                }
                // A copy button on hover, left of the card.
                let cb = Rect::new(b.x - 26, b.y + 4, 22, 22);
                if hot(b) || hot(cb) {
                    let h = hot(cb);
                    if h {
                        cv.fill_round_clip(cb, 6, strong.with_alpha(0x18), clip);
                    }
                    tx.icon(cv, "ui/copy", cb.x + 4, cb.y + 4, 14, if h { strong } else { dim }, clip);
                    self.hits.push((cb.intersect(&body), ChatHit::CopyMsg(*mi)));
                }
                false
            }
            Item::Md(key) => {
                let vis = r.intersect(&body);
                if vis.h > 0
                    && let Some(s) = self.mds.get_mut(key)
                {
                    let off = (vis.y - r.y) as f64;
                    s.view.set_hover(vis.contains(mouse.0, mouse.1).then_some(mouse));
                    s.view.draw(cv, tx, theme, vis, off);
                    s.rect = vis;
                    s.scroll = off;
                    self.hits.push((vis, ChatHit::Md(*key)));
                }
                false
            }
            Item::Think { key, open, lines, live } => {
                let head = Rect::new(r.x, r.y, r.w, 24);
                if hot(head) {
                    cv.fill_round_clip(head, 6, strong.with_alpha(0x0e), clip);
                }
                let ch = if *open { "ui/chevron_down" } else { "ui/chevron_right" };
                tx.icon(cv, ch, r.x + 6, r.y + 6, 12, dim, clip);
                tx.icon(cv, "ui/thinking_mode", r.x + 24, r.y + 5, 14, dim, clip);
                tx.draw_ui_sized(cv, r.x + 44, r.y + 4, "Thinking", dim, clip, false, SMALL + 1.0);
                if *live {
                    crate::app_tests::spinner_t(cv, r.x + 112, r.y + 12, 4, dim, theme.background, clock, clip);
                }
                self.hits.push((head.intersect(&body), ChatHit::Toggle(key.clone())));
                for (k, l) in lines.iter().enumerate() {
                    let y = r.y + 26 + k as i32 * small_h();
                    tx.draw_ui_sized(cv, r.x + 24, y, l, dim, clip, false, SMALL);
                }
                *live
            }
            Item::Tool { key, mi, bi, open, body: lines } => {
                let Some(Block::Tool(t)) = chat.messages.get(*mi).and_then(|m| m.blocks.get(*bi)) else {
                    return false;
                };
                self.paint_tool(cv, tx, theme, body, r, t, key, *open, lines, cwd, mouse, clock)
            }
            Item::Error(lines) => {
                cv.fill_round_clip(r, 8, c.error.with_alpha(0x16), clip);
                cv.ring(r, 8, c.error.with_alpha(0x60));
                tx.icon(cv, "ui/x_circle", r.x + 10, r.y + 10, 16, c.error, clip);
                for (k, l) in lines.iter().enumerate() {
                    tx.draw_ui(cv, r.x + 34, r.y + 10 + k as i32 * lh, l, fg, clip, false);
                }
                false
            }
            Item::Perm(pi, lines) => {
                let Some(p) = chat.perms.get(*pi) else { return false };
                cv.fill_round_clip(r, 10, c.warning.with_alpha(0x14), clip);
                cv.ring(r, 10, c.warning.with_alpha(0x80));
                let mut y = r.y + 14;
                tx.icon(cv, chat::tool_icon(&p.name), r.x + 14, y + 1, 16, c.warning, clip);
                let title = format!("Allow {}?", p.name);
                tx.draw_ui(cv, r.x + 38, y, &title, strong, clip, true);
                y += lh + 6;
                for l in lines {
                    tx.draw_ui(cv, r.x + 16, y, l, fg, clip, false);
                    y += lh;
                }
                y += 12;
                let mut x = r.x + 16;
                for (label, allow) in [("Allow", true), ("Deny", false)] {
                    let w = tx.ui_width(label, true) + 32;
                    let b = Rect::new(x, y, w, 30);
                    let h = hot(b);
                    if allow {
                        cv.fill_round_clip(b, 8, if h { c.accent.with_alpha(0xe0) } else { c.accent }, clip);
                        tx.draw_ui(cv, b.x + 16, b.y + 6, label, c.on_accent, clip, true);
                    } else {
                        if h {
                            cv.fill_round_clip(b, 8, strong.with_alpha(0x14), clip);
                        }
                        cv.ring(b, 8, c.border);
                        tx.draw_ui(cv, b.x + 16, b.y + 6, label, strong, clip, true);
                    }
                    self.hits.push((b.intersect(&body), ChatHit::Perm(p.request_id.clone(), allow)));
                    x += w + 8;
                }
                false
            }
            Item::Working => {
                crate::app_tests::spinner_t(cv, r.x + 8, r.y + 14, 5, c.accent_text, theme.background, clock, clip);
                let secs = chat.turn_started.map_or(0, |t| t.elapsed().as_secs());
                let label = if secs > 0 { format!("Working… {secs}s") } else { "Working…".to_string() };
                tx.draw_ui_sized(cv, r.x + 26, r.y + 8, &label, dim, clip, false, SMALL + 1.0);
                true
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_tool(
        &mut self,
        cv: &mut Canvas,
        tx: &mut TextCtx,
        theme: &Theme,
        body: Rect,
        r: Rect,
        t: &ToolCall,
        key: &str,
        open: bool,
        lines: &[(Ln, String)],
        cwd: &Path,
        mouse: (f64, f64),
        clock: f32,
    ) -> bool {
        let c = theme.chrome;
        let (strong, dim, fg) = (c.tab_active_fg, c.tab_inactive_fg, theme.foreground);
        let clip = r.intersect(&body);
        let hot = |rr: Rect| rr.contains(mouse.0, mouse.1) && body.contains(mouse.0, mouse.1);
        let asking = t.status == ToolStatus::Asking;
        cv.card(r, 8, c.widget_bg, if asking { c.warning.with_alpha(0xa0) } else { c.border });
        let head = Rect::new(r.x, r.y, r.w, HEAD_H);
        if hot(head) {
            cv.fill_round_clip(head, 8, strong.with_alpha(0x0e), clip);
        }
        let ch = if open { "ui/chevron_down" } else { "ui/chevron_right" };
        tx.icon(cv, ch, r.x + 8, r.y + 9, 12, dim, clip);
        tx.icon(cv, chat::tool_icon(&t.name), r.x + 26, r.y + 8, 14, dim, clip);
        let name = if t.name.chars().count() > 28 {
            format!("{}…", t.name.chars().take(27).collect::<String>())
        } else {
            t.name.clone()
        };
        tx.draw_ui(cv, r.x + 46, r.y + 5, &name, strong, clip, true);
        let nw = tx.ui_width(&name, true);
        // Status on the right.
        let mut right = r.right() - 10;
        let mut anim = false;
        match t.status {
            ToolStatus::Running => {
                crate::app_tests::spinner_t(cv, right - 6, r.y + 15, 5, c.accent_text, c.widget_bg, clock, clip);
                right -= 18;
                anim = true;
            }
            ToolStatus::Ok => {
                tx.icon(cv, "ui/check", right - 14, r.y + 8, 14, c.git_added, clip);
                right -= 22;
            }
            ToolStatus::Error => {
                tx.icon(cv, "ui/x_circle", right - 14, r.y + 8, 14, c.error, clip);
                right -= 22;
            }
            ToolStatus::Asking => {
                let w = tx.ui_width_sized("Needs approval", false, SMALL);
                tx.draw_ui_sized(cv, right - w, r.y + 6, "Needs approval", c.warning, clip, false, SMALL);
                right -= w + 8;
            }
            ToolStatus::Denied => {
                let w = tx.ui_width_sized("Denied", false, SMALL);
                tx.draw_ui_sized(cv, right - w, r.y + 6, "Denied", dim, clip, false, SMALL);
                right -= w + 8;
            }
        }
        self.hits.push((head.intersect(&body), ChatHit::Toggle(key.to_string())));
        // One-line summary: a file path is a link to the file.
        let file = chat::tool_file(&t.name, &t.input);
        let sum = match &file {
            Some(p) => chat::rel_path(p, cwd),
            None => chat::tool_summary(&t.name, &t.input),
        };
        let sx = r.x + 46 + nw + 10;
        let room = right - sx - 6;
        if room > 30 && !sum.is_empty() {
            let text = fit_ellipsis(tx, &sum, room, false);
            let tw = tx.ui_width(&text, false);
            let sr = Rect::new(sx, r.y + 4, tw, 22);
            let over = file.is_some() && hot(sr);
            let col = if over { c.accent_text } else { dim };
            tx.draw_ui(cv, sx, r.y + 5, &text, col, clip, false);
            if over {
                cv.fill_clip(Rect::new(sx, r.y + 5 + ui_line() as i32, tw, 1), col, clip);
            }
            if let Some(p) = file {
                self.hits.push((sr.intersect(&body), ChatHit::File(p, None)));
            }
        }
        if !lines.is_empty() {
            let mh = mono_h();
            let top = r.y + HEAD_H;
            cv.fill_clip(Rect::new(r.x + 1, top, r.w - 2, 1), c.border, clip);
            let inner = Rect::new(r.x + 1, top + 1, r.w - 2, r.h - HEAD_H - 2).intersect(&clip);
            let mut y = top + 8;
            for (k, text) in lines {
                let row = Rect::new(r.x + 1, y, r.w - 2, mh);
                if row.bottom() >= body.y && row.y <= body.bottom() {
                    let (prefix, col) = match k {
                        Ln::Add => {
                            cv.fill_clip(row, c.diff_add, inner);
                            ("+ ", fg)
                        }
                        Ln::Del => {
                            cv.fill_clip(row, c.diff_del, inner);
                            ("- ", fg)
                        }
                        Ln::Plain => ("", fg),
                        Ln::Dim => ("", dim),
                        Ln::Err => ("", c.error),
                        Ln::Label => ("", dim),
                    };
                    if *k == Ln::Label {
                        tx.draw_ui_sized(cv, r.x + 12, y, text, dim, inner, true, SMALL);
                    } else {
                        tx.draw_line(cv, r.x + 12, y - 1, &format!("{prefix}{text}"), col, inner);
                    }
                }
                y += mh;
            }
        }
        anim
    }
}

fn user_text(m: &Message) -> String {
    m.blocks
        .iter()
        .filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of message `mi` (copy button).
pub fn message_text(chat: &Chat, mi: usize) -> String {
    let Some(m) = chat.messages.get(mi) else { return String::new() };
    m.blocks
        .iter()
        .filter_map(|b| if let Block::Text(t) = b { Some(t.as_str()) } else { None })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_follows_what_is_under_it() {
        assert_eq!(cursor_kind(None, false), Cur::Default);
        assert_eq!(cursor_kind(Some(&ChatHit::Toggle("t:1".into())), false), Cur::Pointer, "cards");
        assert_eq!(cursor_kind(Some(&ChatHit::File("a.rs".into(), None)), false), Cur::Pointer, "paths");
        assert_eq!(cursor_kind(Some(&ChatHit::Perm("r".into(), true)), false), Cur::Pointer, "Allow / Deny");
        assert_eq!(cursor_kind(Some(&ChatHit::ToEnd), false), Cur::Pointer);
        assert_eq!(cursor_kind(Some(&ChatHit::CopyMsg(0)), false), Cur::Pointer);
        assert_eq!(cursor_kind(Some(&ChatHit::Md((0, 0))), false), Cur::Text, "selectable text");
        assert_eq!(cursor_kind(Some(&ChatHit::Md((0, 0))), true), Cur::Pointer, "a link or copy button in it");
    }

    #[test]
    fn toggling_flips_the_default() {
        let mut v = ChatView::new();
        assert!(!v.open_of("t:1", false) && v.open_of("t:1", true));
        v.toggle("t:1");
        assert!(v.open_of("t:1", false) && !v.open_of("t:1", true));
        v.toggle("t:1");
        assert!(!v.open_of("t:1", false));
    }

    #[test]
    fn scrolling_up_unsticks_and_reaching_the_end_sticks_again() {
        let mut v = ChatView::new();
        v.content_h = 1000;
        v.view_h = 400;
        v.scroll = 600.0;
        v.scroll_by(-100.0);
        assert!(!v.follow && v.scroll == 500.0);
        v.scroll_by(500.0);
        assert!(v.follow && v.scroll == 600.0);
        v.scroll_by(-10_000.0);
        assert_eq!(v.scroll, 0.0);
    }

    #[test]
    fn tool_bodies_show_diffs_commands_and_output() {
        use serde_json::json;
        let mut t = ToolCall::new("1", "Edit", json!({"file_path": "a", "old_string": "x", "new_string": "y\nz"}));
        t.result = Some("ok".into());
        t.status = ToolStatus::Ok;
        let b = ChatView::tool_body(&t);
        let kinds: Vec<Ln> = b.iter().map(|(k, _)| *k).collect();
        assert!(kinds == vec![Ln::Del, Ln::Add, Ln::Add, Ln::Label, Ln::Plain]);
        let t = ToolCall::new("2", "Bash", json!({"command": "ls\npwd"}));
        let b = ChatView::tool_body(&t);
        assert_eq!(b[0].1, "$ ls");
        assert_eq!(b.len(), 2);
        let mut big = ToolCall::new("3", "Read", json!({}));
        big.result = Some((0..40).map(|i| i.to_string()).collect::<Vec<_>>().join("\n"));
        let b = ChatView::tool_body(&big);
        assert_eq!(b.len(), 1 + 1 + BODY_MAX, "label, the cut note, the tail");
    }
}
