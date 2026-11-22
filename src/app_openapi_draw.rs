//! Drawing of the OpenAPI explorer (see `app_openapi` for the state and the input).
//!
//! Immediate mode: every frame walks the visible part of each column, drawing and registering
//! the clickable areas (`Hit`) it passes. The endpoint list is virtualized (only the rows in
//! view are touched), the center and right columns skip every block outside the viewport.

use crate::{
    app_openapi::{ApiView, BODY_ROWS, Hit, NARROW, NavKind, Sel, flash_key, line_col},
    openapi::{
        copy::{self, Flash},
        example,
        lint::{self, Severity},
        request::Verdict,
        schema, snippets,
        spec::{SecKind, Spec},
        tree::{Node, to_json},
    },
    tasks::Segs,
    theme::{Chrome, Rgba, Theme},
    ui::{Canvas, Rect, TextCtx, VBar, line_h, ui_size},
};
use std::sync::Arc;

const PAD: i32 = 28;
const MAX_COL: i32 = 820;
const MAX_EXAMPLE_LINES: usize = 60;
const MAX_BODY_SHOWN: usize = 400;

fn sz(k: f32) -> f32 {
    ui_size() * k
}

fn lh(size: f32) -> i32 {
    (size * 1.4).round() as i32
}

pub fn method_color(c: &Chrome, m: &str) -> Rgba {
    match m {
        "get" => c.git_added,
        "post" => c.info,
        "put" => c.warning,
        "patch" => c.git_modified,
        "delete" => c.error,
        _ => c.tab_inactive_fg,
    }
}

fn status_color(c: &Chrome, code: u16) -> Rgba {
    match code / 100 {
        2 => c.git_added,
        3 => c.info,
        4 => c.warning,
        _ => c.error,
    }
}

fn sev_color(c: &Chrome, s: Severity) -> Rgba {
    match s {
        Severity::Error => c.error,
        Severity::Warning => c.warning,
        Severity::Info => c.info,
    }
}

/// Markup of the descriptions without the markers (`**`, backticks, links).
fn inline(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < ch.len() {
        match ch[i] {
            '`' => i += 1,
            '*' | '_' if ch.get(i + 1) == Some(&ch[i]) => i += 2,
            '[' => {
                let rest: String = ch[i..].iter().collect();
                if let Some(a) = rest.find("](")
                    && let Some(b) = rest[a..].find(')')
                {
                    out.push_str(&rest[1..a]);
                    i += rest[..a + b + 1].chars().count();
                } else {
                    out.push('[');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The painter of one column: draws clipped to it and collects the clickable areas.
struct P<'a, 'b> {
    cv: &'a mut Canvas<'b>,
    tx: &'a mut TextCtx,
    th: &'a Theme,
    clip: Rect,
    mouse: (f64, f64),
    hits: Vec<(Rect, Hit)>,
    /// The last copy (a button of it says "Copied").
    flash: Option<Flash>,
    /// Copyable blocks (Ctrl+C over one copies it).
    blocks: Vec<(Rect, Hit)>,
}

impl<'a, 'b> P<'a, 'b> {
    fn new(cv: &'a mut Canvas<'b>, tx: &'a mut TextCtx, th: &'a Theme, clip: Rect, mouse: (f64, f64)) -> P<'a, 'b> {
        P { cv, tx, th, clip, mouse, hits: Vec::new(), flash: None, blocks: Vec::new() }
    }
    fn c(&self) -> Chrome {
        self.th.chrome
    }
    fn fg(&self) -> Rgba {
        self.th.foreground
    }
    fn dim(&self) -> Rgba {
        self.th.chrome.tab_inactive_fg
    }
    fn vis(&self, y: i32, h: i32) -> bool {
        y + h > self.clip.y && y < self.clip.bottom()
    }
    fn hover(&self, r: Rect) -> bool {
        r.intersect(&self.clip).contains(self.mouse.0, self.mouse.1)
    }
    fn hit(&mut self, r: Rect, h: Hit) {
        let c = r.intersect(&self.clip);
        if c.w > 0 && c.h > 0 {
            self.hits.push((c, h));
        }
    }
    fn tw(&mut self, s: &str, bold: bool, size: f32) -> i32 {
        self.tx.ui_width_sized(s, bold, size)
    }
    fn text_c(&mut self, x: i32, y: i32, s: &str, col: Rgba, bold: bool, size: f32, clip: Rect) -> i32 {
        let w = self.tx.ui_width_sized(s, bold, size);
        if !s.is_empty() && y + lh(size) > clip.y && y < clip.bottom() {
            self.tx.draw_ui_sized(self.cv, x, y, s, col, clip, bold, size);
        }
        w
    }
    fn text(&mut self, x: i32, y: i32, s: &str, col: Rgba, bold: bool, size: f32) -> i32 {
        let clip = self.clip;
        self.text_c(x, y, s, col, bold, size, clip)
    }
    fn fit(&mut self, s: &str, max: i32, bold: bool, size: f32) -> String {
        if max <= 0 {
            return String::new();
        }
        if self.tx.ui_width_sized(s, bold, size) <= max {
            return s.to_string();
        }
        let ch: Vec<char> = s.chars().collect();
        let (mut lo, mut hi) = (0usize, ch.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let cand = ch[..mid].iter().collect::<String>() + "…";
            if self.tx.ui_width_sized(&cand, bold, size) <= max {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        ch[..lo].iter().collect::<String>() + "…"
    }
    fn wrap(&mut self, s: &str, w: i32, bold: bool, size: f32) -> Vec<String> {
        let mut out = Vec::new();
        for para in s.split('\n') {
            if para.trim().is_empty() {
                out.push(String::new());
                continue;
            }
            let mut cur = String::new();
            for word in para.split_whitespace() {
                let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
                if cur.is_empty() || self.tx.ui_width_sized(&cand, bold, size) <= w {
                    cur = cand;
                } else {
                    out.push(std::mem::take(&mut cur));
                    cur = word.to_string();
                }
            }
            out.push(cur);
        }
        out
    }
    /// Wrapped paragraph; returns its height.
    fn para(&mut self, x: i32, y: i32, w: i32, s: &str, col: Rgba, bold: bool, size: f32) -> i32 {
        let lines = self.wrap(s, w, bold, size);
        let h = lh(size);
        for (i, l) in lines.iter().enumerate() {
            self.text(x, y + i as i32 * h, l, col, bold, size);
        }
        lines.len() as i32 * h
    }
    fn round(&mut self, r: Rect, rad: i32, col: Rgba) {
        if r.intersect(&self.clip).h > 0 {
            let clip = self.clip;
            self.cv.fill_round_clip(r, rad, col, clip);
        }
    }
    fn fill(&mut self, r: Rect, col: Rgba) {
        let clip = self.clip;
        self.cv.fill_clip(r, col, clip);
    }
    fn panel(&mut self, r: Rect, fill: Rgba, border: Rgba) {
        self.round(r, 8, border);
        self.round(r.inset(1), 7, fill);
    }
    fn card_bg(&self) -> Rgba {
        self.th.background.mix(self.c().widget_bg, 0.45)
    }
    fn rule(&mut self, x: i32, y: i32, w: i32) {
        let col = self.c().border.with_alpha(0x60);
        self.fill(Rect::new(x, y, w, 1), col);
    }
    /// Pill with `label`; returns its width.
    fn chip(&mut self, x: i32, y: i32, label: &str, fg: Rgba, bg: Rgba, size: f32) -> i32 {
        let w = self.tw(label, true, size) + 16;
        let h = lh(size) + 4;
        self.round(Rect::new(x, y, w, h), h / 2, bg);
        self.text(x + 8, y + 2, label, fg, true, size);
        w
    }
    /// Method badge of fixed width.
    fn badge(&mut self, x: i32, y: i32, method: &str, w: i32, size: f32) {
        let col = method_color(&self.c(), method);
        let label = method.to_uppercase();
        let h = lh(size) + 6;
        self.round(Rect::new(x, y, w, h), 5, col.with_alpha(0x2c));
        let fg = col.readable_on(&[self.th.background], 4.5);
        let tw = self.tw(&label, true, size);
        self.text(x + (w - tw) / 2, y + 3, &label, fg, true, size);
    }
    fn button(&mut self, r: Rect, label: &str, hit: Hit, primary: bool) {
        let c = self.c();
        let hov = self.hover(r);
        let (bg, fg) = if primary {
            (if hov { c.accent.shift(18) } else { c.accent }, c.on_accent)
        } else {
            (if hov { c.list_hover_bg } else { c.widget_bg }, c.tab_active_fg)
        };
        self.round(r, 6, bg);
        let size = sz(0.95);
        let w = self.tw(label, true, size);
        self.text(r.x + (r.w - w) / 2, r.y + (r.h - lh(size)) / 2, label, fg, true, size);
        self.hit(r, hit);
    }
    /// A tab / toggle chip (selected = accent tint).
    fn tab(&mut self, x: i32, y: i32, label: &str, on: bool, hit: Hit) -> i32 {
        let c = self.c();
        let size = sz(0.9);
        let w = self.tw(label, on, size) + 20;
        let r = Rect::new(x, y, w, lh(size) + 8);
        let bg = if on {
            c.accent.with_alpha(0x30)
        } else if self.hover(r) {
            c.list_hover_bg
        } else {
            c.widget_bg.with_alpha(0x90)
        };
        self.round(r, 6, bg);
        let fg = if on { c.accent_text } else { c.tab_active_fg };
        self.text(r.x + 10, r.y + 4, label, fg, on, size);
        self.hit(r, hit);
        w + 6
    }
    #[allow(clippy::too_many_arguments)]
    fn input(&mut self, r: Rect, id: &str, text: &str, cursor: usize, ph: &str, focused: bool, secret: bool) {
        let c = self.c();
        let size = sz(0.95);
        self.round(r, 6, if focused { c.accent } else { c.border });
        self.round(r.inset(1), 5, c.input_bg);
        let inner = r.inset(1).intersect(&self.clip);
        let shown: String = if secret { text.chars().map(|_| '•').collect() } else { text.to_string() };
        let ty = r.y + (r.h - lh(size)) / 2;
        if shown.is_empty() {
            self.text_c(r.x + 10, ty, ph, self.dim().with_alpha(0xB0), false, size, inner);
        } else {
            let before: String = shown.chars().take(cursor).collect();
            let cx = self.tw(&before, false, size);
            let shift = (cx - (r.w - 28)).max(0);
            let col = c.tab_active_fg;
            self.text_c(r.x + 10 - shift, ty, &shown, col, false, size, inner);
            if focused {
                self.cv.fill_clip(Rect::new(r.x + 10 - shift + cx, ty + 1, 1, lh(size) - 2), col, inner);
            }
        }
        if shown.is_empty() && focused {
            self.cv.fill_clip(Rect::new(r.x + 10, ty + 1, 1, lh(size) - 2), c.tab_active_fg, inner);
        }
        self.hit(r, Hit::Field(id.to_string()));
    }
    /// Small "Copy" button; says "Copied" for a moment after a click.
    fn copy_btn(&mut self, r: Rect, hit: Hit) {
        if !self.vis(r.y, r.h) {
            return;
        }
        let c = self.c();
        let on = self.flash.is_some_and(|f| f.on(flash_key(&hit)));
        let hov = self.hover(r);
        let bg = if on {
            c.git_added.with_alpha(0x38)
        } else if hov {
            c.list_hover_bg
        } else {
            c.widget_bg.with_alpha(0xD0)
        };
        self.round(r, 5, bg);
        let size = sz(0.78);
        let label = if on { "Copied" } else { "Copy" };
        let fg = if on { c.git_added.readable_on(&[self.th.background], 4.5) } else { c.tab_active_fg };
        let w = self.tw(label, true, size);
        self.text(r.x + (r.w - w) / 2, r.y + (r.h - lh(size)) / 2, label, fg, true, size);
        self.hit(r, hit);
    }

    /// A copy button for `text` (built only when the button is on screen).
    fn copy_text(&mut self, r: Rect, what: &'static str, text: impl FnOnce() -> String) {
        if self.vis(r.y, r.h) {
            self.copy_btn(r, Hit::Copy(Arc::from(text()), what));
        }
    }

    /// Pill-shaped action button of `label` (any copy hit); returns its width with the gap.
    fn pill(&mut self, x: i32, y: i32, label: &str, hit: Hit) -> i32 {
        let c = self.c();
        let size = sz(0.82);
        let on = self.flash.is_some_and(|f| f.on(flash_key(&hit)));
        let shown = if on { "Copied" } else { label };
        let w = self.tw(label, true, size).max(self.tw("Copied", true, size)) + 18;
        let r = Rect::new(x, y, w, lh(size) + 8);
        let bg = if on {
            c.git_added.with_alpha(0x38)
        } else if self.hover(r) {
            c.list_hover_bg
        } else {
            c.widget_bg.with_alpha(0xB0)
        };
        self.round(r, 6, bg);
        let fg = if on { c.git_added.readable_on(&[self.th.background], 4.5) } else { c.tab_active_fg };
        let tw = self.tw(shown, true, size);
        self.text(r.x + (w - tw) / 2, r.y + 4, shown, fg, true, size);
        self.hit(r, hit);
        w + 6
    }

    /// Code block with its own Copy button in the corner; Ctrl+C over it copies `text`.
    fn code_block_copy(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        lines: &[Segs],
        what: &'static str,
        text: impl FnOnce() -> String,
    ) -> i32 {
        let h = self.code_block(x, y, w, lines);
        if self.vis(y, h) {
            self.block_hit(Rect::new(x, y, w, h), Hit::Copy(Arc::from(text()), what));
        }
        h
    }

    /// Make the block `r` copyable: a corner button and Ctrl+C.
    fn block_hit(&mut self, r: Rect, hit: Hit) {
        self.blocks.push((r.intersect(&self.clip), hit.clone()));
        self.copy_btn(Rect::new(r.right() - 62, r.y + 6, 54, 22), hit);
    }

    /// Code font block; returns its height. Only the visible lines are drawn.
    fn code_block(&mut self, x: i32, y: i32, w: i32, lines: &[Segs]) -> i32 {
        let lhm = (line_h().round() as i32).max(14);
        let h = lines.len() as i32 * lhm + 24;
        let r = Rect::new(x, y, w, h);
        if r.intersect(&self.clip).h > 0 {
            let c = self.c();
            self.panel(r, self.th.background.mix(c.sidebar_bg, 0.8), c.border.with_alpha(0x90));
            let inner = r.inset(1).intersect(&self.clip);
            for (i, segs) in lines.iter().enumerate() {
                let ly = y + 12 + i as i32 * lhm;
                if ly + lhm > self.clip.y && ly < self.clip.bottom() {
                    self.tx.draw(self.cv, x + 14, ly, segs, inner);
                }
            }
        }
        h
    }
}

/// `Segs` of plain / JSON text lines.
fn json_lines(text_lines: &[String], json: bool, fg: Rgba, dim: Rgba) -> Vec<Segs> {
    text_lines
        .iter()
        .map(|l| if json { crate::app_http::json_segs(l, fg, dim) } else { vec![(l.clone(), fg)] })
        .collect()
}

/// The plain text of code lines.
fn segs_text(lines: &[Segs]) -> String {
    lines.iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect::<String>()).collect::<Vec<_>>().join("\n")
}

/// Tabs laid out in rows that wrap at `x + w`; returns the y below them.
fn tabs_wrap(p: &mut P, x: i32, w: i32, mut y: i32, items: Vec<(String, bool, Hit)>) -> i32 {
    let mut cx = x;
    let row = lh(sz(0.9)) + 8 + 6;
    for (label, on, hit) in items {
        let tw = p.tw(&label, on, sz(0.9)) + 20;
        if cx + tw > x + w && cx > x {
            cx = x;
            y += row;
        }
        cx += p.tab(cx, y, &label, on, hit);
    }
    y + row + 4
}

/// Simple markdown: headings, bullets, fenced code, paragraphs. Returns the height.
fn markdown(p: &mut P, x: i32, y0: i32, w: i32, src: &str, size: f32) -> i32 {
    let (fg, dim) = (p.fg(), p.dim());
    let mut y = y0;
    let mut para = String::new();
    let mut code: Option<Vec<Segs>> = None;
    fn flush(p: &mut P, para: &mut String, x: i32, y: &mut i32, w: i32, size: f32, col: Rgba) {
        if !para.trim().is_empty() {
            let t = inline(para.trim());
            *y += p.para(x, *y, w, &t, col, false, size) + 6;
        }
        para.clear();
    }
    for line in src.lines() {
        let t = line.trim_end();
        if t.trim_start().starts_with("```") {
            match code.take() {
                Some(block) => y += p.code_block_copy(x, y, w, &block, "code", || segs_text(&block)) + 8,
                None => {
                    flush(p, &mut para, x, &mut y, w, size, fg);
                    code = Some(Vec::new());
                }
            }
            continue;
        }
        if let Some(block) = code.as_mut() {
            block.push(vec![(t.to_string(), fg)]);
            continue;
        }
        let tt = t.trim_start();
        if let Some(h) = tt.strip_prefix('#') {
            flush(p, &mut para, x, &mut y, w, size, fg);
            let text = inline(h.trim_start_matches('#').trim());
            y += 4;
            y += p.para(x, y, w, &text, fg, true, size * 1.15) + 4;
        } else if let Some(b) = tt.strip_prefix("- ").or_else(|| tt.strip_prefix("* ")) {
            flush(p, &mut para, x, &mut y, w, size, fg);
            p.text(x + 4, y, "•", dim, true, size);
            let text = inline(b);
            y += p.para(x + 20, y, w - 20, &text, fg, false, size) + 2;
        } else if tt.is_empty() {
            flush(p, &mut para, x, &mut y, w, size, fg);
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(tt);
        }
    }
    flush(p, &mut para, x, &mut y, w, size, fg);
    if let Some(block) = code {
        y += p.code_block_copy(x, y, w, &block, "code", || segs_text(&block)) + 8;
    }
    (y - y0 - 6).max(0)
}

fn heading(p: &mut P, x: i32, y: i32, text: &str) -> i32 {
    let col = p.c().tab_active_fg;
    p.text(x, y, text, col, true, sz(1.25));
    lh(sz(1.25)) + 10
}

fn label(p: &mut P, x: i32, y: i32, text: &str) -> i32 {
    let col = p.dim();
    p.text(x, y, &text.to_uppercase(), col, true, sz(0.76));
    lh(sz(0.76)) + 6
}

impl ApiView {
    /// Draw the explorer into `rect`. True while a frame loop is needed (reading, sending).
    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, rect: Rect) -> bool {
        self.poll();
        self.rect = rect;
        self.hits.clear();
        self.blocks.clear();
        cv.fill(rect, th.background);
        if self.spec.is_none() {
            let mut p = P::new(cv, tx, th, rect, self.mouse);
            let (title, body) = match &self.load_err {
                Some(e) => ("This is not a valid OpenAPI document".to_string(), e.clone()),
                None => ("Reading the spec…".to_string(), String::new()),
            };
            let tw = p.tw(&title, true, sz(1.3));
            let col = p.c().tab_active_fg;
            let y = rect.y + (rect.h / 3).max(40);
            p.text(rect.x + (rect.w - tw) / 2, y, &title, col, true, sz(1.3));
            let w = (rect.w - 80).clamp(100, 600);
            let dim = p.dim();
            p.para(rect.x + (rect.w - w) / 2, y + 36, w, &body, dim, false, sz(1.0));
            if self.load_err.is_some() {
                let r = Rect::new(rect.x + rect.w / 2 - 70, y + 90, 140, 34);
                p.button(r, "Show source", Hit::ShowSource, true);
            }
            self.hits = p.hits;
            return self.loading;
        }
        let narrow = rect.w < NARROW;
        let nav_w = if rect.w < 520 { 0 } else { self.nav_w.clamp(200, (rect.w / 2).max(200)).min(rect.w) };
        let nav = Rect::new(rect.x, rect.y, nav_w, rect.h);
        let rest = Rect::new(nav.right(), rect.y, rect.w - nav_w, rect.h);
        let (center, right) = if narrow {
            (rest, Rect::default())
        } else {
            let rw = (rest.w * 38 / 100).clamp(340, 480).min(rest.w / 2);
            (Rect::new(rest.x, rest.y, rest.w - rw, rest.h), Rect::new(rest.right() - rw, rest.y, rw, rest.h))
        };
        self.nav_r = nav;
        self.center_r = center;
        self.right_r = right;
        if nav_w > 0 {
            self.hits.push((Rect::new(nav.right() - 3, rect.y, 6, rect.h), Hit::Divider));
            self.draw_nav(cv, tx, th, nav);
        }
        let hot = self.drag || Rect::new(nav.right() - 3, rect.y, 6, rect.h).contains(self.mouse.0, self.mouse.1);
        if nav_w > 0 && hot {
            cv.fill(Rect::new(nav.right() - 2, rect.y, 3, rect.h), th.chrome.accent.with_alpha(0xB0));
        }
        if narrow {
            self.draw_narrow(cv, tx, th, center);
        } else {
            self.draw_center(cv, tx, th, center);
            self.draw_right(cv, tx, th, right);
            cv.fill(Rect::new(right.x, right.y, 1, right.h), th.chrome.border);
        }
        self.loading || self.inflight.is_some() || self.flash.is_some_and(|f| f.alive())
    }

    // ── left column ──

    fn draw_nav(&mut self, cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect) {
        let c = th.chrome;
        let mut p = P::new(cv, tx, th, r, self.mouse);
        p.fill(r, c.sidebar_bg);
        p.fill(Rect::new(r.right() - 1, r.y, 1, r.h), c.border);
        let (title, ver) = self.spec.as_ref().map(|s| (s.title.clone(), s.api_version.clone())).unwrap_or_default();
        let t = p.fit(&title, r.w - 100, true, sz(1.15));
        p.text(r.x + 16, r.y + 14, &t, c.tab_active_fg, true, sz(1.15));
        if !ver.is_empty() {
            p.chip(r.x + 16, r.y + 14 + lh(sz(1.15)) + 4, &format!("v{ver}"), c.tab_inactive_fg, c.widget_bg, sz(0.78));
        }
        p.button(Rect::new(r.right() - 76, r.y + 14, 60, 28), "Source", Hit::ShowSource, false);
        let sy = r.y + 14 + lh(sz(1.15)) + 4 + lh(sz(0.78)) + 16;
        let search = Rect::new(r.x + 12, sy, r.w - 25, 32);
        let focused = self.focus.as_deref() == Some("search");
        p.input(search, "search", &self.query.text, self.query.cursor, "Search endpoints", focused, false);
        let list = Rect::new(r.x, search.bottom() + 10, r.w - 1, (r.bottom() - search.bottom() - 10).max(0));
        p.clip = list;
        self.nav_scroll = self.nav_scroll.clamp(0.0, (self.nav_h - list.h).max(0) as f64);
        let scroll = self.nav_scroll as i32;
        let first = self.nav.partition_point(|row| row.y + row.h <= scroll);
        let Some(spec) = self.spec.as_ref() else { return };
        for row in self.nav.iter().skip(first) {
            let y = list.y + row.y - scroll;
            if y >= list.bottom() {
                break;
            }
            let rr = Rect::new(list.x, y, list.w, row.h);
            let hov = p.hover(rr);
            match &row.kind {
                NavKind::Head { key, label, count, open } => {
                    if hov {
                        p.fill(rr, c.list_hover_bg.with_alpha(0x70));
                    }
                    let sign = if *open { "−" } else { "+" };
                    p.text(rr.x + 16, y + 10, sign, c.tab_inactive_fg, true, sz(0.9));
                    let l = p.fit(label, rr.w - 90, true, sz(0.85));
                    p.text(rr.x + 34, y + 10, &l, c.tab_active_fg, true, sz(0.85));
                    let n = count.to_string();
                    let nw = p.tw(&n, false, sz(0.8));
                    p.text(rr.right() - nw - 16, y + 11, &n, c.tab_inactive_fg, false, sz(0.8));
                    p.hit(rr, Hit::Group(key.clone()));
                }
                NavKind::Item { label, sel } => {
                    let on = self.sel == *sel;
                    if on {
                        p.fill(rr, c.list_active_bg);
                        p.fill(Rect::new(rr.x, y + 4, 3, rr.h - 8), c.accent);
                    } else if hov {
                        p.fill(rr, c.list_hover_bg);
                    }
                    let fg = if on { c.tab_active_fg } else { c.sidebar_fg };
                    match sel {
                        Sel::Op(i) => {
                            let op = &spec.ops[*i];
                            p.badge(rr.x + 14, y + (rr.h - lh(sz(0.72)) - 6) / 2, &op.method, 52, sz(0.72));
                            let text = if op.summary.is_empty() { op.path.clone() } else { op.summary.clone() };
                            let fg = if op.deprecated { c.tab_inactive_fg } else { fg };
                            let l = p.fit(&text, rr.w - 86, on, sz(0.95));
                            p.text(rr.x + 74, y + (rr.h - lh(sz(0.95))) / 2, &l, fg, on, sz(0.95));
                        }
                        Sel::Lint => {
                            let (e, _, _) = lint::counts(&self.findings);
                            let col = if e > 0 { c.error } else { fg };
                            let l = p.fit(label, rr.w - 40, true, sz(0.95));
                            p.text(rr.x + 16, y + (rr.h - lh(sz(0.95))) / 2, &l, col, true, sz(0.95));
                        }
                        _ => {
                            let l = p.fit(label, rr.w - 40, matches!(sel, Sel::Overview), sz(0.95));
                            p.text(
                                rr.x + 16,
                                y + (rr.h - lh(sz(0.95))) / 2,
                                &l,
                                fg,
                                matches!(sel, Sel::Overview),
                                sz(0.95),
                            );
                        }
                    }
                    p.hit(rr, Hit::Nav(*sel));
                }
            }
        }
        if let Some(b) = VBar::new(
            Rect::new(list.right() - 10, list.y, 10, list.h),
            self.nav_h as f64,
            list.h as f64,
            self.nav_scroll,
        ) {
            b.draw(p.cv, th.foreground, false, false);
        }
        let hits = p.hits;
        self.hits.extend(hits);
    }

    // ── center column ──

    /// Narrow layout: one column with a "Reference | Try it" switch.
    fn draw_narrow(&mut self, cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect) {
        if !matches!(self.sel, Sel::Op(_)) {
            return self.draw_center(cv, tx, th, r);
        }
        let strip = Rect::new(r.x, r.y, r.w, 44);
        let mut p = P::new(cv, tx, th, strip, self.mouse);
        let mut x = r.x + 16;
        x += p.tab(x, r.y + 8, "Reference", !self.try_pane, Hit::Pane(false));
        p.tab(x, r.y + 8, "Try it", self.try_pane, Hit::Pane(true));
        p.rule(r.x, strip.bottom() - 1, r.w);
        let hits = p.hits;
        self.hits.extend(hits);
        let body = Rect::new(r.x, strip.bottom(), r.w, (r.h - strip.h).max(0));
        self.right_r = Rect::default();
        if self.try_pane {
            self.right_r = body;
            self.draw_right(cv, tx, th, body);
        } else {
            self.center_r = body;
            self.draw_center(cv, tx, th, body);
        }
    }

    fn draw_center(&mut self, cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect) {
        let mut p = P::new(cv, tx, th, r, self.mouse);
        p.flash = self.flash;
        let top = r.y - self.scroll_c as i32;
        let end = self.center_body(&mut p, r, top);
        let content = end - top + 32;
        self.scroll_c = self.scroll_c.clamp(0.0, (content - r.h).max(0) as f64);
        if let Some(b) = VBar::new(Rect::new(r.right() - 10, r.y, 10, r.h), content as f64, r.h as f64, self.scroll_c) {
            b.draw(p.cv, th.foreground, false, false);
        }
        let hits = p.hits;
        self.hits.extend(hits);
        self.blocks.extend(p.blocks);
    }

    fn center_body(&self, p: &mut P, r: Rect, top: i32) -> i32 {
        let Some(spec) = self.spec.as_ref() else { return top };
        let w = (r.w - 2 * PAD).clamp(160, MAX_COL.max(160));
        let x = r.x + ((r.w - w) / 2).max(PAD.min(r.w / 8));
        let y = top + PAD;
        match self.sel {
            Sel::Overview => self.overview(p, spec, x, w, y),
            Sel::Op(i) => match spec.ops.get(i) {
                Some(_) => self.operation(p, spec, i, x, w, y),
                None => y,
            },
            Sel::Schema(i) => self.schema_page(p, spec, i, x, w, y),
            Sel::Lint => self.lint_page(p, x, w, y),
        }
    }

    fn overview(&self, p: &mut P, spec: &Spec, x: i32, w: i32, mut y: i32) -> i32 {
        let c = p.c();
        let oas = if spec.swagger2 { format!("Swagger {}", spec.version) } else { format!("OAS {}", spec.version) };
        let mut cx = x;
        if !spec.api_version.is_empty() {
            cx += p.chip(cx, y, &spec.api_version, c.tab_inactive_fg, c.widget_bg, sz(0.8)) + 6;
        }
        p.chip(cx, y, &oas, c.tab_inactive_fg, c.widget_bg, sz(0.8));
        y += lh(sz(0.8)) + 14;
        y += p.para(x, y, w, &spec.title, c.tab_active_fg, true, sz(2.0)) + 10;
        if !spec.description.trim().is_empty() {
            y += markdown(p, x, y, w, &spec.description, sz(1.02)) + 16;
        }
        if !spec.servers.is_empty() {
            y += label(p, x, y, "Servers");
            for s in &spec.servers {
                let r = Rect::new(x, y, w, 40);
                p.panel(r, p.card_bg(), c.border.with_alpha(0x90));
                p.copy_text(Rect::new(x + w - 62, y + 9, 54, 22), "server URL", || s.url.clone());
                let u = p.fit(&s.url, w - 80, true, sz(0.95));
                p.text(x + 12, y + 3, &u, c.tab_active_fg, true, sz(0.95));
                if !s.description.is_empty() {
                    let d = p.fit(&s.description, w - 24, false, sz(0.8));
                    p.text(x + 12, y + 3 + lh(sz(0.95)), &d, c.tab_inactive_fg, false, sz(0.8));
                }
                y += 48;
            }
            y += 8;
        }
        if !spec.schemes.is_empty() {
            y += label(p, x, y, "Authentication");
            for s in &spec.schemes {
                let kind = match &s.kind {
                    SecKind::Bearer => "HTTP Bearer".to_string(),
                    SecKind::Basic => "HTTP Basic".to_string(),
                    SecKind::ApiKey { name, loc } => format!("API key · {name} in {loc}"),
                    SecKind::OAuth2 => "OAuth 2".to_string(),
                    SecKind::Other => "other".to_string(),
                };
                let cw = p.chip(x, y, &s.name, c.accent_text, c.accent.with_alpha(0x28), sz(0.85));
                p.text(x + cw + 10, y + 2, &kind, c.tab_inactive_fg, false, sz(0.9));
                y += lh(sz(0.85)) + 8;
                if !s.description.is_empty() {
                    y += p.para(x, y, w, &s.description, c.tab_inactive_fg, false, sz(0.88)) + 6;
                }
            }
            y += 8;
        }
        // Lint summary.
        let (e, wn, i) = lint::counts(&self.findings);
        let r = Rect::new(x, y, w, 46);
        let hov = p.hover(r);
        p.panel(r, if hov { c.list_hover_bg } else { p.card_bg() }, c.border.with_alpha(0x90));
        let mut cx = x + 14;
        p.text(cx, y + 12, "Spec lint", c.tab_active_fg, true, sz(0.95));
        cx += 90;
        for (n, name, col) in [(e, "errors", c.error), (wn, "warnings", c.warning), (i, "notes", c.info)] {
            cx += p.chip(
                cx,
                y + 11,
                &format!("{n} {name}"),
                col.readable_on(&[p.th.background], 4.5),
                col.with_alpha(0x28),
                sz(0.8),
            ) + 6;
        }
        p.hit(r, Hit::Nav(Sel::Lint));
        y += 62;
        // Endpoints by tag.
        y += heading(p, x, y, "Endpoints");
        for g in spec.groups() {
            if p.vis(y, 30) {
                p.text(x, y, &g.name, c.tab_active_fg, true, sz(1.05));
            }
            y += lh(sz(1.05)) + 4;
            if !g.description.is_empty() {
                y += p.para(x, y, w, &g.description, c.tab_inactive_fg, false, sz(0.9)) + 4;
            }
            for &oi in &g.ops {
                let op = &spec.ops[oi];
                let rr = Rect::new(x, y, w, 30);
                if p.vis(y, 30) {
                    if p.hover(rr) {
                        p.round(rr, 6, c.list_hover_bg);
                    }
                    p.badge(x + 8, y + 3, &op.method, 56, sz(0.74));
                    let t = p.fit(&op.path, w / 2, true, sz(0.95));
                    let tw = p.text(x + 76, y + 5, &t, c.tab_active_fg, true, sz(0.95));
                    let s = p.fit(&op.summary, w - 100 - tw, false, sz(0.9));
                    p.text(x + 90 + tw, y + 6, &s, c.tab_inactive_fg, false, sz(0.9));
                    p.hit(rr, Hit::Nav(Sel::Op(oi)));
                }
                y += 30;
            }
            y += 14;
        }
        y
    }

    fn operation(&self, p: &mut P, spec: &Spec, i: usize, x: i32, w: i32, mut y: i32) -> i32 {
        let c = p.c();
        let op = &spec.ops[i];
        if let Some(t) = op.tags.first() {
            p.text(x, y, t, c.tab_inactive_fg, false, sz(0.85));
            y += lh(sz(0.85)) + 4;
        }
        let title = if op.summary.is_empty() { op.path.clone() } else { op.summary.clone() };
        y += p.para(x, y, w, &title, c.tab_active_fg, true, sz(1.8)) + 8;
        // Method + path.
        p.badge(x, y, &op.method, 64, sz(0.9));
        let pt = p.fit(&op.path, w - 190, true, sz(1.05));
        let pw = p.text(x + 76, y + 4, &pt, c.tab_active_fg, true, sz(1.05));
        let mut cx = x + 76 + pw + 12;
        if op.deprecated {
            cx += p.chip(cx, y + 3, "deprecated", c.warning, c.warning.with_alpha(0x28), sz(0.78)) + 6;
        }
        if !op.op_id.is_empty() && cx + 120 < x + w {
            p.chip(cx, y + 3, &op.op_id, c.tab_inactive_fg, c.widget_bg, sz(0.78));
        }
        p.copy_text(Rect::new(x + w - 56 - 62, y + 2, 54, 22), "path", || op.path.clone());
        let src = Rect::new(x + w - 56, y + 2, 56, 24);
        let hov = p.hover(src);
        let col = if hov { c.accent_text } else { c.tab_inactive_fg };
        p.text(src.x + 4, src.y + 3, "Source ↗", col, hov, sz(0.82));
        p.hit(src, Hit::Source(op.line));
        y += 40;
        if !op.description.trim().is_empty() {
            y += markdown(p, x, y, w, &op.description, sz(1.0)) + 14;
        }
        if !op.security.is_empty() {
            let mut cx = x;
            cx += p.text(cx, y + 2, "Requires", c.tab_inactive_fg, false, sz(0.88)) + 10;
            for s in &op.security {
                cx += p.chip(cx, y, s, c.accent_text, c.accent.with_alpha(0x28), sz(0.82)) + 6;
            }
            y += 34;
        }
        // Parameters, by location.
        for (loc, title) in
            [("path", "Path parameters"), ("query", "Query parameters"), ("header", "Headers"), ("cookie", "Cookies")]
        {
            let ps: Vec<_> = op.params.iter().filter(|q| q.loc == loc).collect();
            if ps.is_empty() {
                continue;
            }
            y += 6;
            y += heading(p, x, y, title);
            for q in ps {
                let ty = schema::type_label(&spec.root, &q.schema, 0);
                let nw = p.text(x, y, &q.name, c.tab_active_fg, true, sz(1.0));
                let mut cx = x + nw + 10;
                cx += p.text(cx, y + 1, &ty, c.info, false, sz(0.88)) + 10;
                if q.required {
                    cx += p.chip(
                        cx,
                        y,
                        "required",
                        c.error.readable_on(&[p.th.background], 4.5),
                        c.error.with_alpha(0x24),
                        sz(0.74),
                    ) + 6;
                }
                if q.deprecated {
                    p.chip(cx, y, "deprecated", c.warning, c.warning.with_alpha(0x24), sz(0.74));
                }
                p.copy_text(Rect::new(x + w - 54, y - 2, 54, 22), "parameter name", || q.name.clone());
                y += lh(sz(1.0)) + 4;
                let d = schema::deref(&spec.root, &q.schema);
                if !q.description.is_empty() {
                    y += p.para(x, y, w, &inline(&q.description), c.tab_inactive_fg, false, sz(0.9)) + 2;
                }
                let en: Vec<String> = d
                    .get("enum")
                    .map(|e| e.arr().iter().map(crate::openapi::tree::scalar_text).collect())
                    .unwrap_or_default();
                if !en.is_empty() {
                    let mut cx = x;
                    for v in en.iter().take(8) {
                        cx += p.chip(cx, y, v, c.tab_active_fg, c.widget_bg, sz(0.76)) + 4;
                    }
                    y += lh(sz(0.76)) + 8;
                }
                y += 6;
                p.rule(x, y, w);
                y += 10;
            }
        }
        // Request body.
        if let Some(b) = &op.body {
            y += 6;
            y += heading(p, x, y, if b.required { "Request body (required)" } else { "Request body" });
            if !b.description.is_empty() {
                y += p.para(x, y, w, &inline(&b.description), c.tab_inactive_fg, false, sz(0.92)) + 6;
            }
            if b.media.len() > 1 {
                let mut cx = x;
                for (mi, m) in b.media.iter().enumerate() {
                    cx += p.tab(cx, y, &m.content_type, mi == self.body_media, Hit::Media(mi));
                }
                y += 36;
            }
            if let Some(m) = b.media.get(self.body_media) {
                y = self.schema_and_example(p, spec, m.schema.as_ref(), m.example.as_ref(), x, w, y);
            }
        }
        // Responses.
        if !op.responses.is_empty() {
            y += 10;
            y += heading(p, x, y, "Responses");
            let mut cx = x;
            for (ri, r) in op.responses.iter().enumerate() {
                let code: u16 = r.code.parse().unwrap_or(0);
                let on = ri == self.resp_sel;
                let size = sz(0.9);
                let tw = p.tw(&r.code, true, size) + 20;
                if cx + tw > x + w && cx > x {
                    cx = x;
                    y += 36;
                }
                let rr = Rect::new(cx, y, tw, lh(size) + 8);
                let col = if code == 0 { c.tab_inactive_fg } else { status_color(&c, code) };
                let bg = if on {
                    col.with_alpha(0x38)
                } else if p.hover(rr) {
                    c.list_hover_bg
                } else {
                    c.widget_bg.with_alpha(0x90)
                };
                p.round(rr, 6, bg);
                let fg = if on { col.readable_on(&[p.th.background], 4.5) } else { c.tab_active_fg };
                p.text(cx + 10, y + 4, &r.code, fg, true, size);
                p.hit(rr, Hit::Resp(ri));
                cx += tw + 6;
            }
            y += 40;
            if let Some(r) = op.responses.get(self.resp_sel) {
                if !r.description.is_empty() {
                    y += p.para(x, y, w, &inline(&r.description), c.tab_active_fg, false, sz(1.0)) + 8;
                }
                if !r.headers.is_empty() {
                    let mut cx = x;
                    cx += p.text(cx, y + 2, "Headers", c.tab_inactive_fg, false, sz(0.85)) + 8;
                    for h in &r.headers {
                        cx += p.chip(cx, y, h, c.tab_active_fg, c.widget_bg, sz(0.76)) + 4;
                    }
                    y += 30;
                }
                if let Some(m) = r.media.iter().find(|m| m.content_type.contains("json")).or(r.media.first()) {
                    y = self.schema_and_example(p, spec, m.schema.as_ref(), m.example.as_ref(), x, w, y);
                }
            }
        }
        y
    }

    /// The schema tree, then an example (given, else generated).
    #[allow(clippy::too_many_arguments)]
    fn schema_and_example(
        &self,
        p: &mut P,
        spec: &Spec,
        sch: Option<&Node>,
        ex: Option<&Node>,
        x: i32,
        w: i32,
        mut y: i32,
    ) -> i32 {
        let Some(sch) = sch else {
            if let Some(e) = ex {
                let text = crate::openapi::tree::to_json(e, true);
                y = self.example_block(p, &text, x, w, y);
            }
            return y;
        };
        p.copy_text(Rect::new(x + w - 54, y, 54, 22), "schema", || to_json(sch, true));
        y += 28;
        y = self.schema_tree(p, spec, sch, x, w, y);
        let text = match ex {
            Some(e) => crate::openapi::tree::to_json(e, true),
            None => example::example_json(&spec.root, sch),
        };
        self.example_block(p, &text, x, w, y + 8)
    }

    fn example_block(&self, p: &mut P, text: &str, x: i32, w: i32, mut y: i32) -> i32 {
        let (fg, dim) = (p.fg(), p.dim());
        y += label(p, x, y, "Example");
        let all: Vec<String> = text.lines().map(String::from).collect();
        let mut shown: Vec<String> = all.iter().take(MAX_EXAMPLE_LINES).cloned().collect();
        if all.len() > MAX_EXAMPLE_LINES {
            shown.push(format!("… {} more lines", all.len() - MAX_EXAMPLE_LINES));
        }
        let json = text.starts_with(['{', '[']);
        let lines = json_lines(&shown, json, fg, dim);
        y + p.code_block_copy(x, y, w, &lines, "example", || text.to_string()) + 12
    }

    fn schema_tree(&self, p: &mut P, spec: &Spec, sch: &Node, x: i32, w: i32, mut y: i32) -> i32 {
        let c = p.c();
        let rows = schema::rows(&spec.root, sch, &self.toggled);
        let top = y;
        for r in &rows {
            let extra = !r.enums.is_empty() || !r.format.is_empty() || !r.notes.is_empty();
            let h = 32 + if extra { 24 } else { 0 };
            let rr = Rect::new(x, y, w, h);
            if p.vis(y, h) {
                let ix = x + 6 + r.depth as i32 * 22;
                if r.expandable && p.hover(rr) {
                    p.round(rr, 6, c.list_hover_bg.with_alpha(0x80));
                }
                for d in 0..r.depth {
                    p.fill(Rect::new(x + 6 + d as i32 * 22 + 8, y, 1, h), c.border.with_alpha(0x60));
                }
                let mut cx = ix + 22;
                if r.expandable {
                    let box_r = Rect::new(ix, y + 8, 16, 16);
                    p.round(box_r, 4, c.widget_bg);
                    p.text(ix + 4, y + 8, if r.open { "−" } else { "+" }, c.tab_active_fg, true, sz(0.9));
                }
                cx += p.text(cx, y + 6, &r.name, c.tab_active_fg, true, sz(0.98)) + 10;
                let tcol = if r.circular { c.warning } else { c.info };
                cx += p.text(cx, y + 7, &r.ty, tcol, false, sz(0.88)) + 10;
                if r.required {
                    cx += p.text(cx, y + 7, "required", c.error.readable_on(&[p.th.background], 4.5), false, sz(0.8))
                        + 10;
                }
                if r.nullable {
                    cx += p.text(cx, y + 7, "nullable", c.tab_inactive_fg, false, sz(0.8)) + 10;
                }
                if !r.desc.is_empty() && cx + 60 < x + w {
                    let d = p.fit(&inline(&r.desc), x + w - cx - 8, false, sz(0.88));
                    p.text(cx, y + 7, &d, c.tab_inactive_fg, false, sz(0.88));
                }
                if extra {
                    let mut ex = ix + 22;
                    if !r.format.is_empty() {
                        ex += p.chip(ex, y + 31, &r.format, c.accent_text, c.accent.with_alpha(0x24), sz(0.72)) + 4;
                    }
                    for e in r.enums.iter().take(8) {
                        if ex + 60 > x + w {
                            break;
                        }
                        ex += p.chip(ex, y + 31, e, c.tab_active_fg, c.widget_bg, sz(0.72)) + 4;
                    }
                    for n in r.notes.iter().take(3) {
                        let t = p.fit(n, x + w - ex - 8, false, sz(0.78));
                        ex += p.text(ex, y + 33, &t, c.tab_inactive_fg, false, sz(0.78)) + 10;
                    }
                }
                p.rule(x, y + h - 1, w);
                if r.expandable {
                    p.hit(rr, Hit::Toggle(r.path.clone()));
                }
            }
            y += h;
        }
        if rows.is_empty() {
            p.text(x, top, "No schema", c.tab_inactive_fg, false, sz(0.9));
            y += 26;
        }
        y
    }

    fn schema_page(&self, p: &mut P, spec: &Spec, i: usize, x: i32, w: i32, mut y: i32) -> i32 {
        let c = p.c();
        let Some((name, line)) = spec.schemas.get(i) else { return y };
        p.text(x, y, "Model", c.tab_inactive_fg, false, sz(0.85));
        y += lh(sz(0.85)) + 4;
        y += p.para(x, y, w, name, c.tab_active_fg, true, sz(1.8)) + 6;
        let src = Rect::new(x, y, 80, 24);
        let hov = p.hover(src);
        p.text(x, y + 2, "Source ↗", if hov { c.accent_text } else { c.tab_inactive_fg }, hov, sz(0.85));
        p.hit(src, Hit::Source(*line));
        y += 36;
        let ptr = if spec.swagger2 { format!("/definitions/{name}") } else { format!("/components/schemas/{name}") };
        let node = schema::pointer(&spec.root, &ptr);
        if let Some(n) = node {
            if let Some(d) = n.gets("description") {
                y += markdown(p, x, y, w, d, sz(1.0)) + 14;
            }
            y = self.schema_tree(p, spec, n, x, w, y);
            let text = example::example_json(&spec.root, n);
            y = self.example_block(p, &text, x, w, y + 8);
        }
        y
    }

    fn lint_page(&self, p: &mut P, x: i32, w: i32, mut y: i32) -> i32 {
        let c = p.c();
        y += p.para(x, y, w, "Spec lint", c.tab_active_fg, true, sz(1.8)) + 8;
        let (e, wn, i) = lint::counts(&self.findings);
        let mut cx = x;
        for (n, name, col) in [(e, "errors", c.error), (wn, "warnings", c.warning), (i, "notes", c.info)] {
            cx += p.chip(
                cx,
                y,
                &format!("{n} {name}"),
                col.readable_on(&[p.th.background], 4.5),
                col.with_alpha(0x28),
                sz(0.85),
            ) + 8;
        }
        p.copy_text(Rect::new(x + w - 54, y, 54, 22), "all findings", || copy::findings(&self.findings));
        y += 40;
        if self.findings.is_empty() {
            p.panel(Rect::new(x, y, w, 52), p.card_bg(), c.border.with_alpha(0x90));
            p.text(x + 16, y + 14, "No issues found.", c.git_added, true, sz(1.0));
            return y + 60;
        }
        for f in &self.findings {
            let lines = p.wrap(&f.msg, w - 330, false, sz(0.95));
            let h = (lines.len() as i32 * lh(sz(0.95)) + 18).max(40);
            let rr = Rect::new(x, y, w, h);
            if p.vis(y, h) {
                let hov = p.hover(rr);
                p.panel(rr, if hov { c.list_hover_bg } else { p.card_bg() }, c.border.with_alpha(0x70));
                let col = sev_color(&c, f.severity);
                let name = match f.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Info => "note",
                };
                p.chip(x + 12, y + 9, name, col.readable_on(&[p.th.background], 4.5), col.with_alpha(0x28), sz(0.74));
                for (li, l) in lines.iter().enumerate() {
                    p.text(x + 100, y + 9 + li as i32 * lh(sz(0.95)), l, c.tab_active_fg, false, sz(0.95));
                }
                let loc = format!("{} · line {}", f.code, f.line + 1);
                let lw = p.tw(&loc, false, sz(0.85));
                p.text(
                    x + w - lw - 14,
                    y + 10,
                    &loc,
                    if hov { c.accent_text } else { c.tab_inactive_fg },
                    hov,
                    sz(0.85),
                );
                p.copy_text(Rect::new(x + w - lw - 14 - 62, y + 9, 54, 22), "finding", || copy::finding(f));
                p.hit(rr, Hit::Source(f.line));
            }
            y += h + 8;
        }
        y
    }

    // ── right column: Try it ──

    fn draw_right(&mut self, cv: &mut Canvas, tx: &mut TextCtx, th: &Theme, r: Rect) {
        let mut p = P::new(cv, tx, th, r, self.mouse);
        p.flash = self.flash;
        p.fill(r, th.chrome.sidebar_bg.mix(th.background, 0.5));
        let top = r.y - self.scroll_r as i32;
        let end = self.try_body(&mut p, r, top);
        let content = end - top + 32;
        self.scroll_r = self.scroll_r.clamp(0.0, (content - r.h).max(0) as f64);
        if let Some(b) = VBar::new(Rect::new(r.right() - 10, r.y, 10, r.h), content as f64, r.h as f64, self.scroll_r) {
            b.draw(p.cv, th.foreground, false, false);
        }
        let hits = p.hits;
        self.hits.extend(hits);
        self.blocks.extend(p.blocks);
    }

    fn try_body(&self, p: &mut P, r: Rect, top: i32) -> i32 {
        let c = p.c();
        let x = r.x + 20;
        let w = (r.w - 40).max(120);
        let mut y = top + 22;
        y += p.para(x, y, w, "Try it", c.tab_active_fg, true, sz(1.35)) + 6;
        let (Some(spec), Some(op)) = (self.spec.as_ref(), self.op()) else {
            let hint = "Pick an endpoint on the left to send a real request: server, parameters, auth and body, with the answer checked against the spec.";
            return y + p.para(x, y, w, hint, c.tab_inactive_fg, false, sz(0.95));
        };
        p.badge(x, y, &op.method, 56, sz(0.8));
        let pt = p.fit(&op.path, w - 70, true, sz(0.95));
        p.text(x + 66, y + 3, &pt, c.tab_active_fg, true, sz(0.95));
        y += 38;

        // Server.
        y += label(p, x, y, "Server");
        let base = self.fields.get("base");
        let base_text = base.map(|f| f.text.as_str()).unwrap_or("");
        for (si, s) in spec.servers.iter().enumerate() {
            let on = s.url == base_text;
            let t = p.fit(&s.url, w - 24, on, sz(0.9));
            let rr = Rect::new(x, y, w, 28);
            let hov = p.hover(rr);
            let bg = if on {
                c.accent.with_alpha(0x28)
            } else if hov {
                c.list_hover_bg
            } else {
                c.widget_bg.with_alpha(0x70)
            };
            p.round(rr, 6, bg);
            p.text(x + 10, y + 5, &t, if on { c.accent_text } else { c.tab_active_fg }, on, sz(0.9));
            p.hit(rr, Hit::Server(si));
            y += 32;
        }
        let focus = self.focus.as_deref();
        let tf = |id: &str| self.fields.get(id).map(|f| (f.text.as_str(), f.cursor)).unwrap_or(("", 0));
        let (t, cur) = tf("base");
        p.input(Rect::new(x, y, w, 32), "base", t, cur, "https://api.example.com", focus == Some("base"), false);
        y += 46;

        // Auth.
        if !spec.schemes.is_empty() {
            y += label(p, x, y, "Authentication");
            let mut cx = x;
            let row = |p: &mut P, label: &str, on: bool, hit: Hit, cx: &mut i32, y: &mut i32| {
                let size = sz(0.88);
                let tw = p.tw(label, on, size) + 20;
                if *cx + tw > x + w && *cx > x {
                    *cx = x;
                    *y += 34;
                }
                let used = p.tab(*cx, *y, label, on, hit);
                *cx += used;
            };
            row(p, "None", self.auth.is_none(), Hit::Scheme(None), &mut cx, &mut y);
            for (si, s) in spec.schemes.iter().enumerate() {
                row(p, &s.name, self.auth == Some(si), Hit::Scheme(Some(si)), &mut cx, &mut y);
            }
            y += 38;
            if let Some(s) = self.auth.and_then(|i| spec.schemes.get(i)) {
                match &s.kind {
                    SecKind::Basic => {
                        let (t, cur) = tf("auth:user");
                        p.input(
                            Rect::new(x, y, w, 32),
                            "auth:user",
                            t,
                            cur,
                            "Username",
                            focus == Some("auth:user"),
                            false,
                        );
                        y += 40;
                        let (t, cur) = tf("auth:value");
                        let rr = Rect::new(x, y, w - 64, 32);
                        p.input(rr, "auth:value", t, cur, "Password", focus == Some("auth:value"), !self.show_secret);
                        p.button(
                            Rect::new(x + w - 58, y, 58, 32),
                            if self.show_secret { "Hide" } else { "Show" },
                            Hit::Eye,
                            false,
                        );
                        y += 40;
                    }
                    kind => {
                        let ph = match kind {
                            SecKind::ApiKey { name, loc } if loc.as_str() == "cookie" => {
                                format!("{name} value (or a pasted name=value cookie)")
                            }
                            SecKind::ApiKey { name, loc } => format!("{name} ({loc})"),
                            _ => "Token".to_string(),
                        };
                        let (t, cur) = tf("auth:value");
                        let rr = Rect::new(x, y, w - 64, 32);
                        p.input(rr, "auth:value", t, cur, &ph, focus == Some("auth:value"), !self.show_secret);
                        p.button(
                            Rect::new(x + w - 58, y, 58, 32),
                            if self.show_secret { "Hide" } else { "Show" },
                            Hit::Eye,
                            false,
                        );
                        y += 40;
                        if let SecKind::ApiKey { loc, .. } = kind
                            && loc.as_str() == "cookie"
                        {
                            y += p.para(x, y, w, "Sent in the Cookie header.", c.tab_inactive_fg, false, sz(0.8)) + 4;
                        }
                    }
                }
            }
            y += 10;
        }

        // Parameters.
        let named: Vec<_> =
            op.params.iter().filter(|q| matches!(q.loc.as_str(), "path" | "query" | "header" | "cookie")).collect();
        if !named.is_empty() {
            p.pill(x + w - 82, y - 4, "Copy all", Hit::CopyParams);
            y += label(p, x, y, "Parameters");
            for q in named {
                let nw = p.text(x, y, &q.name, c.tab_active_fg, true, sz(0.92));
                let lc = p.chip(x + nw + 8, y - 1, &q.loc, c.tab_inactive_fg, c.widget_bg, sz(0.7));
                if q.required {
                    p.text(
                        x + nw + lc + 16,
                        y,
                        "required",
                        c.error.readable_on(&[p.th.background], 4.5),
                        false,
                        sz(0.78),
                    );
                }
                let id = ApiView::field_id(&q.loc, &q.name);
                p.copy_text(Rect::new(x + w - 54, y - 3, 54, 22), "parameter", || {
                    let v = self.fields.get(&id).map(|f| f.text.trim().to_string()).unwrap_or_default();
                    if v.is_empty() { q.name.clone() } else { copy::pair(&q.name, &v) }
                });
                y += lh(sz(0.92)) + 4;
                let (t, cur) = tf(&id);
                let ph = schema::type_label(&spec.root, &q.schema, 0);
                p.input(Rect::new(x, y, w, 32), &id, t, cur, &ph, focus == Some(id.as_str()), false);
                y += 42;
            }
            y += 4;
        }

        // Body.
        if let Some(b) = &op.body {
            p.copy_text(Rect::new(x + w - 54, y - 4, 54, 22), "request body", || copy::pretty(&self.body));
            y += label(p, x, y, "Body");
            if b.media.len() > 1 {
                let mut cx = x;
                for (mi, m) in b.media.iter().enumerate() {
                    cx += p.tab(cx, y, &m.content_type, mi == self.body_media, Hit::Media(mi));
                }
                y += 36;
            }
            let lhm = (line_h().round() as i32).max(14);
            let h = BODY_ROWS as i32 * lhm + 16;
            let rr = Rect::new(x, y, w, h);
            let on = focus == Some("body");
            p.round(rr, 6, if on { c.accent } else { c.border });
            p.round(rr.inset(1), 5, c.input_bg);
            self.body_geom.set((rr, p.tx.cell_w, lhm));
            let inner = rr.inset(1).intersect(&p.clip);
            let json = self.body.trim_start().starts_with(['{', '[']);
            let (fg, dim) = (p.fg(), p.dim());
            for (li, line) in self.body.split('\n').enumerate().skip(self.body_top).take(BODY_ROWS) {
                let ly = y + 8 + (li - self.body_top) as i32 * lhm;
                if ly + lhm > p.clip.y && ly < p.clip.bottom() {
                    let segs =
                        if json { crate::app_http::json_segs(line, fg, dim) } else { vec![(line.to_string(), fg)] };
                    p.tx.draw(p.cv, x + 10, ly, &segs, inner);
                }
            }
            if on {
                let (l, col) = line_col(&self.body, self.body_cur);
                if l >= self.body_top && l < self.body_top + BODY_ROWS {
                    let cxp = x + 10 + (col as f32 * p.tx.cell_w) as i32;
                    let cyp = y + 8 + (l - self.body_top) as i32 * lhm;
                    p.cv.fill_clip(Rect::new(cxp, cyp, 1, lhm), fg, inner);
                }
            }
            p.hit(rr, Hit::Body);
            y += h + 8;
            let rb = Rect::new(x + w - 130, y, 130, 26);
            p.button(rb, "Reset to example", Hit::Example, false);
            y += 40;
        }

        // Send.
        if let Some(f) = &self.inflight {
            let secs = f.started.elapsed().as_secs_f32();
            let what = if f.op == self.sel_op() { "Sending" } else { "Sending (another endpoint)" };
            p.button(Rect::new(x, y, w - 90, 38), &format!("{what}… {secs:.1}s"), Hit::Cancel, false);
            p.button(Rect::new(x + w - 82, y, 82, 38), "Cancel", Hit::Cancel, false);
        } else {
            p.button(Rect::new(x, y, w, 38), "Send request", Hit::Send, true);
        }
        y += 56;

        // Response.
        if let Some(l) = self.last.as_ref().filter(|l| l.op == self.sel_op()) {
            y = self.response_card(p, l, x, w, y);
        }

        // Request URL and code.
        let req = self.build_req(false);
        y += label(p, x, y, "Request URL");
        let url = req.as_ref().map(|r| r.url.clone()).unwrap_or_default();
        let ur = Rect::new(x, y, w, 34);
        p.panel(ur, p.card_bg(), c.border.with_alpha(0x90));
        let shown = p.fit(&url, w - 90, false, sz(0.88));
        p.text(x + 12, y + (34 - lh(sz(0.88))) / 2, &shown, c.tab_active_fg, false, sz(0.88));
        if !url.is_empty() {
            p.blocks.push((ur.intersect(&p.clip), Hit::CopyUrl));
            p.copy_btn(Rect::new(x + w - 62, y + 6, 54, 22), Hit::CopyUrl);
        }
        y += 50;
        y += label(p, x, y, "Code");
        let groups: Vec<(String, bool, Hit)> = snippets::groups()
            .into_iter()
            .map(|g| (g.to_string(), g == snippets::group_of(self.lang), Hit::LangGroup(g)))
            .collect();
        y = tabs_wrap(p, x, w, y, groups);
        let clients: Vec<(String, bool, Hit)> = snippets::of_group(snippets::group_of(self.lang))
            .into_iter()
            .map(|i| (snippets::TARGETS[i].name.to_string(), i == self.lang, Hit::Lang(i)))
            .collect();
        if clients.len() > 1 {
            y = tabs_wrap(p, x, w, y - 4, clients);
        }
        // Quick copies.
        let mut cx = x;
        let quick: [(&str, Hit); 6] = [
            ("Copy snippet", Hit::CopySnippet),
            ("Copy as cURL", Hit::CopyAs(snippets::CURL)),
            ("Copy as HTTPie", Hit::CopyAs(snippets::HTTPIE)),
            ("Copy URL", Hit::CopyUrl),
            ("Copy params", Hit::CopyParams),
            ("Copy headers", Hit::CopyReqHeaders),
        ];
        let size = sz(0.82);
        for (name, hit) in quick {
            let pw = p.tw(name, true, size).max(p.tw("Copied", true, size)) + 24;
            if cx + pw > x + w && cx > x {
                cx = x;
                y += lh(size) + 8 + 6;
            }
            cx += p.pill(cx, y, name, hit);
        }
        y += lh(size) + 8 + 14;
        let text = match &req {
            Ok(req) => snippets::render(self.lang, req),
            Err(e) => e.clone(),
        };
        let fg = p.fg();
        let lines: Vec<Segs> = text.lines().map(|l| vec![(l.to_string(), fg)]).collect();
        let h = p.code_block(x, y, w, &lines);
        if req.is_ok() && p.vis(y, h) {
            p.block_hit(Rect::new(x, y, w, h), Hit::CopySnippet);
        }
        y + h + 8
    }

    fn sel_op(&self) -> usize {
        match self.sel {
            Sel::Op(i) => i,
            _ => usize::MAX,
        }
    }

    fn response_card(&self, p: &mut P, l: &crate::app_openapi::Last, x: i32, w: i32, mut y: i32) -> i32 {
        let c = p.c();
        y += label(p, x, y, "Response");
        let r = match &l.result {
            Err(e) => {
                let h = p.wrap(e, w - 28, false, sz(0.92)).len() as i32 * lh(sz(0.92)) + 28;
                p.panel(Rect::new(x, y, w, h), c.error.with_alpha(0x1c), c.error.with_alpha(0x80));
                p.para(x + 14, y + 14, w - 28, e, c.error.readable_on(&[p.th.background], 4.5), false, sz(0.92));
                p.copy_text(Rect::new(x + w - 62, y + 6, 54, 22), "error", || e.clone());
                return y + h + 20;
            }
            Ok(r) => r,
        };
        let col = status_color(&c, r.status);
        let status = format!("{} {}", r.status, r.reason);
        let mut cx = x;
        cx += p.chip(cx, y, status.trim(), col.readable_on(&[p.th.background], 4.5), col.with_alpha(0x2c), sz(0.9)) + 8;
        let meta = format!("{} ms · {}", r.ms, crate::httpfile::human_size(r.body.len()));
        cx += p.text(cx, y + 3, &meta, c.tab_inactive_fg, false, sz(0.85)) + 10;
        match &l.verdict {
            Some(Verdict::Pass) => {
                p.chip(
                    cx,
                    y,
                    "PASS",
                    c.git_added.readable_on(&[p.th.background], 4.5),
                    c.git_added.with_alpha(0x2c),
                    sz(0.8),
                );
            }
            Some(Verdict::Fail(v)) => {
                p.chip(
                    cx,
                    y,
                    &format!("FAIL · {}", v.len()),
                    c.error.readable_on(&[p.th.background], 4.5),
                    c.error.with_alpha(0x2c),
                    sz(0.8),
                );
            }
            Some(Verdict::Skip(_)) | None => {
                p.chip(cx, y, "NOT CHECKED", c.tab_inactive_fg, c.widget_bg, sz(0.8));
            }
        }
        y += 34;
        if !l.req.url.is_empty() {
            p.pill(x + w - 82, y - 6, "Copy all", Hit::CopyResp);
            let line = p.fit(&format!("{} {}", l.req.method, l.req.url), w - 90, false, sz(0.8));
            p.text(x, y - 4, &line, c.tab_inactive_fg, false, sz(0.8));
            y += lh(sz(0.8));
        }
        let nissues = match &l.verdict {
            Some(Verdict::Fail(v)) => v.len(),
            _ => 0,
        };
        let vlabel = if nissues > 0 { format!("Validation ({nissues})") } else { "Validation".to_string() };
        let mut cx = x;
        cx += p.tab(cx, y, "Body", self.resp_tab == 0, Hit::RespTab(0));
        cx += p.tab(cx, y, "Headers", self.resp_tab == 1, Hit::RespTab(1));
        p.tab(cx, y, &vlabel, self.resp_tab == 2, Hit::RespTab(2));
        let tab_copy = match self.resp_tab {
            0 => Hit::CopyBody,
            1 => Hit::Copy(Arc::from(copy::headers(&r.headers)), "response headers"),
            _ => match &l.verdict {
                Some(Verdict::Fail(v)) => Hit::Copy(Arc::from(copy::issues(v)), "validation issues"),
                Some(Verdict::Pass) => Hit::Copy(Arc::from("PASS"), "validation"),
                _ => Hit::CopyBody,
            },
        };
        p.button(Rect::new(x + w - 64, y, 64, 28), "Copy", tab_copy, false);
        y += 40;
        let (fg, dim) = (p.fg(), p.dim());
        match self.resp_tab {
            0 => {
                if l.lines.is_empty() {
                    p.text(x, y, "(empty body)", dim, false, sz(0.9));
                    y += 28;
                } else {
                    let mut shown: Vec<String> = l.lines.iter().take(MAX_BODY_SHOWN).cloned().collect();
                    if l.lines.len() > MAX_BODY_SHOWN {
                        shown.push(format!(
                            "… {} more lines (Copy gives the whole body)",
                            l.lines.len() - MAX_BODY_SHOWN
                        ));
                    }
                    let lines = json_lines(&shown, l.json, fg, dim);
                    let h = p.code_block(x, y, w, &lines);
                    p.block_hit(Rect::new(x, y, w, h), Hit::CopyBody);
                    y += h + 8;
                }
            }
            1 => {
                let lines: Vec<Segs> =
                    r.headers.iter().map(|(k, v)| vec![(format!("{k}: "), c.info), (v.clone(), fg)]).collect();
                y += p.code_block_copy(x, y, w, &lines, "response headers", || copy::headers(&r.headers)) + 8;
            }
            _ => match &l.verdict {
                Some(Verdict::Pass) => {
                    p.panel(Rect::new(x, y, w, 52), c.git_added.with_alpha(0x1c), c.git_added.with_alpha(0x70));
                    p.text(x + 14, y + 7, "PASS", c.git_added.readable_on(&[p.th.background], 4.5), true, sz(1.0));
                    p.text(
                        x + 14,
                        y + 7 + lh(sz(1.0)),
                        "The answer matches the documented response.",
                        c.tab_active_fg,
                        false,
                        sz(0.85),
                    );
                    y += 64;
                }
                Some(Verdict::Skip(why)) => {
                    y += p.para(x, y, w, &format!("Not checked: {why}."), c.tab_inactive_fg, false, sz(0.92)) + 12;
                }
                Some(Verdict::Fail(issues)) => {
                    for iss in issues {
                        let lines = p.wrap(&iss.msg, w - 28, false, sz(0.88));
                        let h = lh(sz(0.85)) + lines.len() as i32 * lh(sz(0.88)) + 18;
                        p.panel(Rect::new(x, y, w, h), c.error.with_alpha(0x14), c.error.with_alpha(0x60));
                        p.text(x + 14, y + 7, &iss.path, c.warning, true, sz(0.85));
                        p.copy_text(Rect::new(x + w - 62, y + 5, 54, 22), "issue", || copy::pair(&iss.path, &iss.msg));
                        for (li, line) in lines.iter().enumerate() {
                            p.text(
                                x + 14,
                                y + 9 + lh(sz(0.85)) + li as i32 * lh(sz(0.88)),
                                line,
                                c.tab_active_fg,
                                false,
                                sz(0.88),
                            );
                        }
                        y += h + 6;
                    }
                    y += 6;
                }
                None => {}
            },
        }
        y + 12
    }
}
