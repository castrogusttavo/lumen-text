//! Markdown reader beside the editor: `Ctrl+Shift+V` (or the palette) splits the editor area in
//! two columns, the `.md` source on the left and the rendered preview ([`MdView`]) on the right.
//! The state lives in `Doc::md` (an `Option<Box<_>>`), so a document without a reader pays nothing.
//!
//! Scroll sync is two-way and works on source lines: the editor's fractional top line maps to a
//! preview pixel offset through the line anchors of the layout, and back (by bisection, so the two
//! directions are exact inverses). Near the end of the document the mapping blends into "both at
//! the bottom", because the editor can't scroll its last line to the top of the view.

use crate::{
    app::{App, Focus},
    doc::Doc,
    mdview::MdView,
    ui::{Canvas, Rect, line_h},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use winit::event::{MouseButton, MouseScrollDelta};

/// Per-document reader state.
pub struct MdState {
    pub view: MdView,
    /// Preview scroll offset in px from the top of the rendered document.
    pub scroll: f64,
    /// Where the preview was last drawn (hit-testing links needs it).
    pub rect: Rect,
    ver: i32,
    pub top_seen: f64,
    fresh: bool,
    /// `#heading` to scroll to once the layout exists (links to other files carry one).
    pub anchor: Option<String>,
    /// The divider between the editor and the preview is being dragged.
    pub dragging: bool,
    /// Last time the image files were checked for edits on disk.
    img_check: std::time::Instant,
}

impl MdState {
    pub fn new() -> Self {
        MdState {
            view: MdView::new(),
            scroll: 0.0,
            rect: Rect::default(),
            ver: 0,
            top_seen: -1.0,
            fresh: true,
            anchor: None,
            dragging: false,
            img_check: std::time::Instant::now(),
        }
    }
}

impl Default for MdState {
    fn default() -> Self {
        Self::new()
    }
}

/// The reader needs at least this much editor area; narrower, the editor keeps all of it.
pub const MIN_AREA: i32 = 420;
/// Narrowest side (editor or preview) the divider can squeeze.
const MIN_PANE: i32 = 160;
/// Pixels on each side of the 1 px divider that grab the mouse.
const GRAB: i32 = 3;

/// (editor, divider, preview) inside `card` for a source-side share `ratio`; `None` when the card
/// is narrower than [`MIN_AREA`].
pub fn md_rects(card: Rect, ratio: f32) -> Option<(Rect, Rect, Rect)> {
    if card.w < MIN_AREA {
        return None;
    }
    let avail = card.w - 1;
    let lw = ((avail as f32 * ratio).round() as i32).clamp(MIN_PANE, avail - MIN_PANE);
    Some((
        Rect::new(card.x, card.y, lw, card.h),
        Rect::new(card.x + lw, card.y, 1, card.h),
        Rect::new(card.x + lw + 1, card.y, avail - lw, card.h),
    ))
}

/// Source-side share that puts the divider under window x (kept inside the configured range).
pub fn md_ratio_at(card: Rect, x: f64) -> f32 {
    (((x - card.x as f64) / (card.w - 1).max(1) as f64) as f32)
        .clamp(crate::config::MD_SPLIT_RANGE.0, crate::config::MD_SPLIT_RANGE.1)
}

pub fn is_markdown_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.ends_with(".md") || n.ends_with(".markdown") || n.ends_with(".mdx")
}

// ── scroll mapping ──

/// Preview offset (px) that shows the same source line as the editor at fractional line `top`.
pub fn top_to_scroll(v: &MdView, top: f64, max_top: f64, max_scroll: f64) -> f64 {
    if max_scroll <= 0.0 {
        return 0.0;
    }
    let l = top.floor().max(0.0) as usize;
    let frac = top - l as f64;
    let (Some(a), Some(b)) = (v.y_for_source_line(l), v.y_for_source_line(l + 1)) else { return 0.0 };
    // The first lines sit under the document's top padding: fade that offset out so top 0 = scroll 0.
    let pad = v.y_for_source_line(0).unwrap_or(0) as f64 * (1.0 - top / 8.0).max(0.0);
    let base = (a as f64 + frac * (b - a) as f64 - pad).clamp(0.0, max_scroll);
    // Last stretch: blend towards the bottom so that editor-at-end means preview-at-end.
    let k = max_top.min(40.0);
    if max_top > 0.0 && k > 0.0 && top > max_top - k {
        let t = ((top - (max_top - k)) / k).clamp(0.0, 1.0);
        return base * (1.0 - t) + max_scroll * t;
    }
    base
}

/// Inverse of [`top_to_scroll`]: the editor top line for preview offset `scroll`.
pub fn scroll_to_top(v: &MdView, scroll: f64, max_top: f64, max_scroll: f64) -> f64 {
    if max_scroll <= 0.0 || max_top <= 0.0 {
        return 0.0;
    }
    if scroll <= 0.0 {
        return 0.0;
    }
    if scroll >= max_scroll - 0.5 {
        return max_top;
    }
    // top_to_scroll is monotonic in `top`: find the smallest top that reaches `scroll`.
    let (mut lo, mut hi) = (0.0f64, max_top);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if top_to_scroll(v, mid, max_top, max_scroll) < scroll {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi.clamp(0.0, max_top)
}

// ── links ──

#[derive(Debug, PartialEq)]
pub enum LinkAction {
    /// Handed to `xdg-open` (http, https, mailto, ...).
    Web(String),
    /// `#section` inside the same document (lowercased, percent-decoded).
    Anchor(String),
    /// A path relative to the document (or absolute), plus an optional `#section`.
    File(PathBuf, Option<String>),
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn classify_link(url: &str, base_dir: Option<&Path>) -> LinkAction {
    let url = url.trim();
    if let Some(frag) = url.strip_prefix('#') {
        return LinkAction::Anchor(percent_decode(frag).to_lowercase());
    }
    let scheme_len =
        url.find(':').filter(|&i| i > 1 && url[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)));
    if scheme_len.is_some() {
        return LinkAction::Web(url.to_string());
    }
    let (path, frag) = match url.split_once('#') {
        Some((p, f)) => (p, Some(percent_decode(f).to_lowercase())),
        None => (url, None),
    };
    let path = path.split('?').next().unwrap_or("");
    let path = percent_decode(path);
    let p = PathBuf::from(&path);
    let full = if p.is_absolute() { p } else { base_dir.map_or(p.clone(), |b| b.join(&p)) };
    LinkAction::File(full, frag)
}

fn find_anchor(v: &MdView, name: &str) -> Option<i32> {
    let heads = v.heading_anchors();
    let slug = crate::mdview::slugify(name);
    heads.iter().find(|(s, _)| s == name).or_else(|| heads.iter().find(|(s, _)| *s == slug)).map(|(_, y)| *y)
}

fn open_external(url: &str) {
    crate::paths::open_url(url);
}

// ── code highlighting ──

/// TextMate scope for a fence language: a manifest id (`typescript`), one of its aliases (`ts`,
/// `sh`) or a top-level alias (`bash`), case-insensitive.
pub fn scope_of(assets: &Path, lang: &str) -> Option<String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m = HashMap::new();
        let Ok(text) = std::fs::read_to_string(assets.join("language-manifest.json")) else { return m };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return m };
        let mut by_id: HashMap<String, String> = HashMap::new();
        for l in v["languages"].as_array().into_iter().flatten() {
            let (Some(id), Some(scope)) = (l["id"].as_str(), l["scopeName"].as_str()) else { continue };
            by_id.insert(id.to_string(), scope.to_string());
            m.insert(id.to_lowercase(), scope.to_string());
            for a in l["aliases"].as_array().into_iter().flatten().filter_map(|a| a.as_str()) {
                m.entry(a.to_lowercase()).or_insert_with(|| scope.to_string());
            }
        }
        for (alias, id) in v["aliases"].as_object().into_iter().flatten() {
            if let Some(scope) = id.as_str().and_then(|i| by_id.get(i)) {
                m.entry(alias.to_lowercase()).or_insert_with(|| scope.clone());
            }
        }
        m
    });
    map.get(&lang.to_lowercase()).cloned()
}

/// The character that flips when a task checkbox is clicked: `' '` <-> `'x'` between the brackets
/// at char index `c` (`[ ]` / `[x]`), if that is what the text holds there.
pub fn task_flip(buf: &crate::buffer::Buffer, c: usize) -> Option<(usize, &'static str)> {
    if buf.char_at(c) != Some('[') || buf.char_at(c + 2) != Some(']') {
        return None;
    }
    match buf.char_at(c + 1)? {
        ' ' => Some((c + 1, "x")),
        'x' | 'X' => Some((c + 1, " ")),
        _ => None,
    }
}

// ── App integration ──

impl Doc {
    fn md_is_markdown(&self) -> bool {
        is_markdown_name(&self.name)
    }

    /// Reopen the reader when the session said it was open (no-op for files that aren't Markdown).
    pub fn md_restore(&mut self, open: bool) {
        if open && self.md_is_markdown() && self.md.is_none() {
            self.md = Some(Box::new(MdState::new()));
        }
    }
}

impl App {
    /// Timers of the open preview: image files edited on disk are noticed about once a second
    /// (cached size and pixels dropped, layout redone), and an animated image that is on screen
    /// asks for a redraw when its next frame is due. Nothing runs for a closed preview or for
    /// animations scrolled out of view, so an idle editor stays idle.
    pub fn md_tick(&mut self, now: std::time::Instant, want: &mut impl FnMut(std::time::Instant)) {
        if self.layout.md.w <= 0 {
            return;
        }
        let Some(st) = self.docs.get_mut(self.active).and_then(|d| d.md.as_mut()) else { return };
        let mut redraw = false;
        if st.img_check + std::time::Duration::from_secs(1) <= now {
            st.img_check = now;
            redraw |= st.view.refresh_images();
        }
        want(st.img_check + std::time::Duration::from_secs(1));
        if let Some(due) = st.view.anim_due() {
            if due <= now {
                redraw = true;
            } else {
                want(due);
            }
        }
        if redraw {
            self.request_redraw();
        }
    }

    pub fn active_is_markdown(&self) -> bool {
        self.docs.get(self.active).is_some_and(|d| d.md_is_markdown())
    }

    /// Open / close the reader of the active document.
    pub fn toggle_md_reader(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if d.md.is_some() {
            d.md = None;
            self.layout.md = Rect::default();
            self.say("Markdown reader closed");
        } else if d.md_is_markdown() {
            d.md = Some(Box::new(MdState::new()));
        } else {
            self.say("The Markdown reader only opens .md files");
        }
        self.request_redraw();
    }

    /// Called at the end of `compute_layout`: gives the right part of the card to the preview.
    pub fn md_split(&mut self) {
        self.md_place(self.layout.card);
    }

    /// Give the right part of `area` (the card, or the focused pane of a split) to the preview of
    /// the active document, if its reader is open and the area is wide enough.
    pub fn md_place(&mut self, area: Rect) {
        self.layout.md = Rect::default();
        self.layout.divider = Rect::default();
        if (self.diff.is_some() && self.diff_area() == self.layout.editor)
            || !self.docs.get(self.active).is_some_and(|d| d.md.is_some())
        {
            return;
        }
        if let Some((ed, dv, md)) = md_rects(area, self.config.md_split) {
            self.layout.editor = ed;
            self.layout.divider = dv; // the split-pane dividers have their own list (`Split::divs`)
            self.layout.md = md;
        }
    }

    /// Mouse move while dragging the reader's divider.
    pub fn md_drag(&mut self, x: f64) -> bool {
        // The area the reader splits: the editor part, the divider and the preview together.
        let (ed, dv, md) = (self.layout.editor, self.layout.divider, self.layout.md);
        let area = Rect::new(ed.x, ed.y, ed.w + dv.w + md.w, ed.h);
        if !self.docs.get(self.active).and_then(|d| d.md.as_ref()).is_some_and(|s| s.dragging) {
            return false;
        }
        self.config.md_split = md_ratio_at(area, x);
        self.split_layout();
        for d in &mut self.docs {
            d.reveal = true;
        }
        true
    }

    /// Mouse release: ends a divider drag and keeps the ratio in config.json.
    pub fn md_release(&mut self) {
        let mut was = false;
        for st in self.docs.iter_mut().filter_map(|d| d.md.as_mut()) {
            was |= std::mem::replace(&mut st.dragging, false);
        }
        if was {
            self.config.save();
        }
    }

    /// Draw the preview and keep both scroll positions in step. Returns true while a redraw is wanted.
    pub fn draw_md(&mut self, cv: &mut Canvas) -> bool {
        let r = self.layout.md;
        if r.w <= 0 || r.h <= 0 {
            return false;
        }
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        let max_top = d.max_top();
        let base = d.path().and_then(Path::parent).map(Path::to_path_buf);
        let Some(st) = d.md.as_mut() else { return false };
        let border = self.theme.chrome.border;
        cv.fill(Rect::new(r.x - 1, r.y, 1, r.h), border);

        let text_changed = st.ver != d.version || st.fresh;
        if text_changed {
            st.view.set_text(&d.buf.to_string());
            st.ver = d.version;
        }
        st.view.set_base_dir(base.as_deref());
        st.view.ensure_layout(&mut self.tx, r.w);
        let max_scroll = (st.view.content_height() - r.h).max(0) as f64;

        let mut busy = false;
        let mut moved = false;
        if let Some(a) = st.anchor.take()
            && let Some(y) = find_anchor(&st.view, &a)
        {
            st.scroll = (y as f64).clamp(0.0, max_scroll);
            let top = scroll_to_top(&st.view, st.scroll, max_top, max_scroll);
            d.top = top;
            d.target = top;
            d.animating = false;
            st.top_seen = top;
            moved = true;
            busy = true;
        }
        if !moved && (text_changed || r.w != st.rect.w || (d.top - st.top_seen).abs() > 1e-6) {
            st.scroll = top_to_scroll(&st.view, d.top, max_top, max_scroll);
            st.top_seen = d.top;
        }
        st.fresh = false;
        st.scroll = st.scroll.clamp(0.0, max_scroll);
        st.rect = r;
        {
            let (theme, assets, tokenizers) = (&self.theme, &self.assets, &mut self.tokenizers);
            let mut hl = |lang: &str, text: &str| {
                let scope = scope_of(assets, lang)?;
                let tk = match tokenizers.get(&scope) {
                    Some(t) => t.clone(),
                    None => {
                        let t = crate::highlight::make_tokenizer(assets, &scope)?;
                        tokenizers.insert(scope.clone(), t.clone());
                        t
                    }
                };
                Some(crate::highlight::highlight_text(&tk, theme, text))
            };
            st.view.draw_with(cv, &mut self.tx, &self.theme, r, st.scroll, Some(&mut hl));
        }
        busy |= st.view.pending();
        let (total, visible, pos) = (st.view.content_height() as f64, r.h as f64, st.scroll);
        self.region_bar(cv, crate::scrollreg::Reg::Md, r, total, visible, pos, 1.0, false);
        busy
    }

    /// Mouse wheel over the preview scrolls it (and the editor follows). False when not over it.
    pub fn md_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        let r = self.layout.md;
        if r.w <= 0 || !r.contains(x, y) {
            return false;
        }
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        let max_top = d.max_top();
        let Some(st) = d.md.as_mut() else { return false };
        let px = crate::scroller::wheel_px(delta, line_h() as f64);
        let max_scroll = (st.view.content_height() - r.h).max(0) as f64;
        st.scroll = (st.scroll + px).clamp(0.0, max_scroll);
        let top = scroll_to_top(&st.view, st.scroll, max_top, max_scroll);
        d.top = top;
        d.target = top;
        d.animating = false;
        st.top_seen = top;
        true
    }

    /// Click inside the preview: follows links. False when the click is not on the preview.
    pub fn md_mouse_down(&mut self, x: f64, y: f64, button: MouseButton) -> bool {
        let dv = self.layout.divider;
        if button == MouseButton::Left
            && self.layout.md.w > 0
            && dv.w > 0
            && Rect::new(dv.x - GRAB, dv.y, dv.w + 2 * GRAB, dv.h).contains(x, y)
            && let Some(st) = self.docs.get_mut(self.active).and_then(|d| d.md.as_mut())
        {
            st.dragging = true;
            return true;
        }
        let r = self.layout.md;
        if r.w <= 0 || !r.contains(x, y) {
            return false;
        }
        self.focus = Focus::Editor;
        if button != MouseButton::Left {
            return true;
        }
        let Some(d) = self.docs.get(self.active) else { return true };
        let Some(st) = d.md.as_ref() else { return true };
        if let Some(off) = st.view.check_at(x, y, st.rect, st.scroll) {
            self.md_toggle_task(off);
            return true;
        }
        let Some(url) = st.view.link_at(x, y, st.rect, st.scroll) else { return true };
        let base = d.path().and_then(Path::parent).map(Path::to_path_buf);
        self.md_follow(classify_link(&url, base.as_deref()));
        true
    }

    /// Right click in the preview on a rendered formula: copy its TeX source. True when there was
    /// one under the pointer.
    pub fn md_copy_tex(&mut self, x: f64, y: f64) -> bool {
        let r = self.layout.md;
        if r.w <= 0 || !r.contains(x, y) {
            return false;
        }
        let st = self.docs.get(self.active).and_then(|d| d.md.as_ref());
        let Some(tex) = st.and_then(|st| st.view.math_at(x, y, st.rect, st.scroll)) else { return false };
        self.clip.set(&tex);
        self.toast("TeX source copied", true);
        true
    }

    /// A task checkbox was clicked: flip `[ ]` / `[x]` in the source (one undo step). `off` is the
    /// byte offset of the `[` in the text the preview was built from.
    pub fn md_toggle_task(&mut self, off: usize) {
        use crate::buffer::EditKind;
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if d.md.as_ref().is_none_or(|st| st.ver != d.version) {
            return; // the text changed since the last draw: the offset may be stale
        }
        let c = d.buf.snapshot().byte_to_char(off.min(d.buf.len_bytes()));
        let Some((at, new)) = task_flip(&d.buf, c) else { return };
        let (top, target, before) = (d.top, d.target, d.ed.sel);
        d.buf.close_group();
        d.buf.replace(at, at + 1, new, EditKind::Other, before);
        d.buf.close_group();
        d.sync_edits();
        d.top = top;
        d.target = target;
        self.request_redraw();
    }

    pub fn md_follow(&mut self, act: LinkAction) {
        match act {
            LinkAction::Web(u) => {
                open_external(&u);
                self.say(format!("Abrindo {u}"));
            }
            LinkAction::Anchor(a) => {
                if let Some(st) = self.docs.get_mut(self.active).and_then(|d| d.md.as_mut()) {
                    if find_anchor(&st.view, &a).is_some() {
                        st.anchor = Some(a);
                    } else {
                        self.say(format!("Section #{a} not found"));
                    }
                }
            }
            LinkAction::File(p, frag) => {
                if !p.is_file() {
                    return self.say(format!("File not found: {}", p.display()));
                }
                self.push_nav();
                let Some(i) = self.open_path(&p, None) else { return };
                let d = &mut self.docs[i];
                if d.md_is_markdown() {
                    let st = d.md.get_or_insert_with(|| Box::new(MdState::new()));
                    st.anchor = frag;
                }
            }
        }
        self.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::TextCtx;

    fn doc_text() -> String {
        let mut s = String::new();
        for i in 0..40 {
            s.push_str(&format!(
                "## Seção {i}\n\nParágrafo {i} com algum texto para ocupar espaço.\n\n- item a\n- item b\n\n"
            ));
        }
        s
    }

    fn laid_out(src: &str) -> (MdView, usize) {
        let mut v = MdView::new();
        v.set_text(src);
        let mut tx = TextCtx::new(None);
        v.ensure_layout(&mut tx, 600);
        (v, src.lines().count())
    }

    #[test]
    fn md_rects_keep_the_420_minimum_and_clamp_the_panes() {
        let card = |w| Rect::new(10, 20, w, 300);
        assert!(md_rects(card(419), 0.5).is_none(), "below 420 px nothing is split");
        let (ed, dv, md) = md_rects(card(420), 0.5).unwrap();
        assert_eq!((ed.x, ed.w, dv.w, md.w), (10, 210, 1, 209));
        assert_eq!(ed.w + dv.w + md.w, 420, "the three parts tile the card");
        assert_eq!(dv.x, ed.right());
        assert_eq!(md.x, dv.right());
        // Extreme ratios never squeeze a side under the minimum pane.
        let (ed, _, md) = md_rects(card(600), 0.0).unwrap();
        assert_eq!(ed.w, MIN_PANE);
        assert!(md.w >= MIN_PANE);
        let (ed, _, md) = md_rects(card(600), 1.0).unwrap();
        assert_eq!(md.w, MIN_PANE);
        assert!(ed.w >= MIN_PANE);
        let (ed, _, _) = md_rects(card(1000), 0.7).unwrap();
        assert_eq!(ed.w, 699);
    }

    #[test]
    fn dragging_maps_x_to_a_bounded_ratio() {
        let card = Rect::new(100, 0, 801, 300);
        assert!((md_ratio_at(card, 100.0 + 400.0) - 0.5).abs() < 1e-3);
        assert_eq!(md_ratio_at(card, 0.0), crate::config::MD_SPLIT_RANGE.0);
        assert_eq!(md_ratio_at(card, 5000.0), crate::config::MD_SPLIT_RANGE.1);
        // Round trip: dragging to the divider's own x keeps the ratio.
        let (_, dv, _) = md_rects(card, 0.35).unwrap();
        let r = md_ratio_at(card, dv.x as f64);
        assert_eq!(md_rects(card, r).unwrap().1.x, dv.x);
    }

    #[test]
    fn md_restore_only_reopens_markdown_files() {
        use crate::buffer::Buffer;
        let mk = |name: &str| {
            let mut d = Doc::new(Buffer::from_str("# a\n"), None, None, "//", 8.0);
            d.name = name.to_string();
            d
        };
        let mut a = mk("notes.md");
        a.md_restore(false);
        assert!(a.md.is_none(), "closed in the session -> stays closed");
        a.md_restore(true);
        assert!(a.md.is_some());
        let mut b = mk("main.rs");
        b.md_restore(true);
        assert!(b.md.is_none(), "not a Markdown file");
    }

    #[test]
    fn markdown_names() {
        assert!(is_markdown_name("README.md"));
        assert!(is_markdown_name("a.MARKDOWN"));
        assert!(!is_markdown_name("main.rs"));
    }

    #[test]
    fn classifies_links() {
        let base = Path::new("/p/docs");
        assert_eq!(classify_link("https://a.dev/x", Some(base)), LinkAction::Web("https://a.dev/x".into()));
        assert_eq!(classify_link("mailto:a@b.c", Some(base)), LinkAction::Web("mailto:a@b.c".into()));
        assert_eq!(classify_link("#Instala%C3%A7%C3%A3o", Some(base)), LinkAction::Anchor("instalação".into()));
        assert_eq!(classify_link("guia.md", Some(base)), LinkAction::File("/p/docs/guia.md".into(), None));
        assert_eq!(
            classify_link("../README.md#Uso", Some(base)),
            LinkAction::File("/p/docs/../README.md".into(), Some("uso".into()))
        );
        assert_eq!(classify_link("a%20b.md", Some(base)), LinkAction::File("/p/docs/a b.md".into(), None));
        assert_eq!(classify_link("/abs/x.md", Some(base)), LinkAction::File("/abs/x.md".into(), None));
    }

    #[test]
    fn percent_decoding_edges() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%41"), "A");
    }

    #[test]
    fn scroll_mapping_is_monotonic_and_invertible() {
        let src = doc_text();
        let (v, lines) = laid_out(&src);
        let max_top = (lines as f64 - 30.0).max(1.0);
        let max_scroll = (v.content_height() - 500).max(1) as f64;
        assert!(max_scroll > 100.0);
        assert_eq!(top_to_scroll(&v, 0.0, max_top, max_scroll), 0.0);
        assert_eq!(top_to_scroll(&v, max_top, max_top, max_scroll), max_scroll);
        let mut prev = -1.0;
        let mut t = 0.0;
        while t <= max_top {
            let s = top_to_scroll(&v, t, max_top, max_scroll);
            assert!(s >= prev - 1e-9, "monotonic at {t}: {s} < {prev}");
            prev = s;
            // round trip lands on a top that maps to the same offset
            let back = scroll_to_top(&v, s, max_top, max_scroll);
            let s2 = top_to_scroll(&v, back, max_top, max_scroll);
            assert!((s - s2).abs() < 1.0, "round trip at {t}: {s} vs {s2}");
            t += 3.7;
        }
        assert_eq!(scroll_to_top(&v, max_scroll, max_top, max_scroll), max_top);
        assert_eq!(scroll_to_top(&v, 0.0, max_top, max_scroll), 0.0);
    }

    #[test]
    fn short_documents_do_not_scroll() {
        let (v, _) = laid_out("# oi\n\ntexto\n");
        assert_eq!(top_to_scroll(&v, 0.0, 0.0, 0.0), 0.0);
        assert_eq!(scroll_to_top(&v, 0.0, 0.0, 0.0), 0.0);
    }

    #[test]
    fn finds_anchors() {
        let (v, _) = laid_out("# Título Grande\n\ntexto\n\n## Instalação\n\nmais\n");
        assert_eq!(find_anchor(&v, "título-grande").is_some(), true);
        assert!(find_anchor(&v, "instalação").is_some());
        assert!(find_anchor(&v, "Instalação").is_some(), "falls back to slugifying");
        assert!(find_anchor(&v, "nao-existe").is_none());
        assert!(find_anchor(&v, "instalação").unwrap() > find_anchor(&v, "título-grande").unwrap());
    }

    #[test]
    fn anchor_link_in_laid_out_doc() {
        let mut src = String::from("# Topo\n\n[ir para o fim](#fim) e [outro](b.md#segunda)\n\n");
        for i in 0..30 {
            src.push_str(&format!("## Seção {i}\n\ntexto {i}\n\n"));
        }
        src.push_str("## Fim\n\nchegou\n");
        let (v, _) = laid_out(&src);
        let r = Rect::new(0, 0, 600, 400);
        let mut hit = None;
        for y in 0..400 {
            for x in (0..600).step_by(4) {
                if let Some(u) = v.link_at(x as f64, y as f64, r, 0.0) {
                    hit = Some((x, y, u));
                    break;
                }
            }
            if hit.is_some() {
                break;
            }
        }
        let (_, _, u) = hit.expect("a link is found");
        assert_eq!(u, "#fim");
        assert_eq!(classify_link(&u, None), LinkAction::Anchor("fim".into()));
        assert!(find_anchor(&v, "fim").is_some(), "heads: {:?}", v.heading_anchors().last());
    }
}
