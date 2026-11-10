//! One open document: text, cursor, highlighting, scroll/caret animation and the
//! per-file state that LSP and git attach to it.

use crate::{
    buffer::Buffer,
    docview::{FOLD_W, row_range as row_range_of},
    editor::{Editor, col_from_visual},
    highlight::{Highlighter, Spans},
    rowmap::wrap_breaks,
    theme::{Rgba, Theme},
    ui::{Canvas, Rect, TextCtx, line_h},
};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

const CURSOR_TAU_Y: f64 = 0.055;
const CURSOR_TAU_X: f64 = 0.040;
const CURSOR_W: i32 = 2;
const MIN_GUTTER_DIGITS: usize = 3;

#[derive(Clone, Debug)]
pub struct Diag {
    pub sl: usize,
    pub sc: usize,
    pub el: usize,
    pub ec: usize,
    pub severity: u8, // 1 error, 2 warning, 3 info, 4 hint
    pub message: String,
    pub code: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Copy, Debug)]
pub struct GitMark {
    pub line: usize,
    pub len: usize,
    pub kind: GitKind,
}

/// One entry of the document outline (flattened `documentSymbol` tree).
#[derive(Clone, Debug, PartialEq)]
pub struct Sym {
    pub name: String,
    pub kind: u32,
    pub depth: usize,
    pub sl: usize,
    pub el: usize,
    pub sel_l: usize,
    pub sel_c: usize, // UTF-16 column
}

/// Flatten the two shapes of `textDocument/documentSymbol` results, in document order.
pub fn flatten_symbols(v: &serde_json::Value) -> Vec<Sym> {
    fn walk(v: &serde_json::Value, depth: usize, out: &mut Vec<Sym>) {
        for s in v.as_array().into_iter().flatten() {
            let range = s.get("range").or_else(|| s["location"].get("range"));
            let sel = s.get("selectionRange").or(range);
            let (Some(name), Some(r)) = (s["name"].as_str(), range) else { continue };
            let g = |v: &serde_json::Value, k: &str, k2: &str| v[k][k2].as_u64().unwrap_or(0) as usize;
            out.push(Sym {
                name: name.to_string(),
                kind: s["kind"].as_u64().unwrap_or(0) as u32,
                depth,
                sl: g(r, "start", "line"),
                el: g(r, "end", "line"),
                sel_l: sel.map_or(0, |x| g(x, "start", "line")),
                sel_c: sel.map_or(0, |x| g(x, "start", "character")),
            });
            if let Some(ch) = s.get("children") {
                walk(ch, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(v, 0, &mut out);
    out
}

/// Semantic token classes we recolor (the rest keeps its TextMate color).
pub fn sem_class(name: &str) -> u8 {
    match name {
        "namespace" => 1,
        "class" => 2,
        "enum" => 3,
        "interface" => 4,
        "struct" => 5,
        "typeParameter" => 6,
        "type" => 7,
        "parameter" => 8,
        "variable" => 9,
        "property" => 10,
        "enumMember" => 11,
        "function" => 12,
        "method" => 13,
        "decorator" => 14,
        _ => 0,
    }
}

pub fn sem_scope(class: u8, readonly: bool) -> Option<&'static str> {
    Some(match class {
        1 => "entity.name.namespace",
        2 => "entity.name.type.class",
        3 => "entity.name.type.enum",
        4 => "entity.name.type.interface",
        5 => "entity.name.type.struct",
        6 | 7 => "entity.name.type",
        8 => "variable.parameter",
        9 if readonly => "variable.other.constant",
        10 => "variable.other.property",
        11 => "variable.other.enummember",
        12 | 13 => "entity.name.function",
        14 => "entity.name.function.decorator",
        _ => return None,
    })
}

/// Decode LSP `semanticTokens` (relative encoding) into per-line (utf16 start, utf16 len, class, readonly).
pub fn decode_semantic(
    data: &[u32],
    types: &[String],
    mods: &[String],
) -> std::collections::HashMap<usize, Vec<(usize, usize, u8, bool)>> {
    let ro_bit = mods.iter().position(|m| m == "readonly");
    let classes: Vec<u8> = types.iter().map(|t| sem_class(t)).collect();
    let mut out: std::collections::HashMap<usize, Vec<(usize, usize, u8, bool)>> = std::collections::HashMap::new();
    let (mut line, mut start) = (0usize, 0usize);
    for t in data.as_chunks::<5>().0 {
        let (dl, ds, len, ty, m) = (t[0] as usize, t[1] as usize, t[2] as usize, t[3] as usize, t[4]);
        if dl > 0 {
            line += dl;
            start = ds;
        } else {
            start += ds;
        }
        let class = classes.get(ty).copied().unwrap_or(0);
        if class != 0 {
            let ro = ro_bit.is_some_and(|b| m & (1 << b) != 0);
            out.entry(line).or_default().push((start, len, class, ro));
        }
    }
    out
}

/// Recolor parts of `spans` (byte ranges of `line`) with semantic token colors given as UTF-16 ranges.
pub fn overlay_colors(
    spans: &[(usize, usize, Rgba, u8)],
    line: &str,
    toks: &[(usize, usize, Rgba)],
) -> Vec<(usize, usize, Rgba, u8)> {
    // UTF-16 offset -> byte offset
    let mut map: Vec<usize> = Vec::with_capacity(line.len() + 1);
    for (b, ch) in line.char_indices() {
        for _ in 0..ch.len_utf16() {
            map.push(b);
        }
    }
    map.push(line.len());
    let at = |u: usize| map.get(u).copied().unwrap_or(line.len());
    let mut bt: Vec<(usize, usize, Rgba)> =
        toks.iter().map(|(s, l, c)| (at(*s), at(s + l), *c)).filter(|(a, b, _)| a < b).collect();
    bt.sort_by_key(|t| t.0);
    let mut out = Vec::with_capacity(spans.len() + bt.len() * 2);
    for &(s, e, c, f) in spans {
        let mut cur = s;
        for &(ts, te, tc) in &bt {
            if te <= cur || ts >= e {
                continue;
            }
            let (a, b) = (ts.max(cur), te.min(e));
            if a > cur {
                out.push((cur, a, c, f));
            }
            out.push((a, b, tc, f));
            cur = b;
        }
        if cur < e {
            out.push((cur, e, c, f));
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct Lens {
    pub line: usize,
    pub col: usize, // UTF-16
    pub title: Option<String>,
    pub raw: serde_json::Value,
}

pub struct Doc {
    pub buf: Buffer,
    pub ed: Editor,
    pub hl: Option<Highlighter>,
    pub name: String,
    pub lang: Option<&'static str>,

    pub top: f64,    // current (animated) first visible line, fractional
    pub target: f64, // where the animation is heading
    pub animating: bool,
    pub left: f64, // horizontal scroll in pixels
    pub reveal: bool,
    caret: (f64, f64),
    caret_init: bool,
    caret_animating: bool,
    caret_snap: bool,

    pub rect: Rect,
    pub cell_w: f32,

    // LSP / git attachments
    /// Merged view of both sources, sorted by position; what drawing and navigation use.
    pub diags: Vec<Diag>,
    diags_ts: Vec<Diag>,
    diags_lint: Vec<Diag>,
    diags_eslint: Vec<Diag>,
    pub eslint_due: Option<Instant>,
    /// Test outcomes from the last run: (0-based line, passed). Drawn in the gutter.
    pub test_marks: Vec<(usize, Option<bool>)>,
    /// Selections to return to when shrinking after "expand selection".
    pub sel_stack: Vec<crate::buffer::Sel>,
    pub symbols: Vec<Sym>,
    pub bps: Vec<usize>,
    pub minimap: bool,
    /// Scrollbar of the last drawn frame, and whether the pointer is near it.
    pub sb: Option<crate::ui::VBar>,
    pub sb_hot: bool,
    pub sb_drag: bool,
    /// Horizontal scrollbar of the last drawn frame (only with wide lines and no word wrap).
    pub hb: Option<crate::ui::HBar>,
    pub hb_hot: bool,
    pub hb_drag: bool,
    /// Where the horizontal scroll is heading (equals `left` when still).
    pub left_target: f64,
    /// Wheel dynamics (see doc_scroll.rs).
    pub scr: crate::doc_scroll::DocScroll,
    pub indent_guides: bool,
    pub sticky: bool,
    /// Word wrap + folding state (see docview.rs).
    pub view: crate::docview::ViewMap,
    /// Folded-region "..." markers drawn last frame: (rect, header line).
    pub fold_pills: Vec<(Rect, usize)>,
    pub minimap_layout: Option<crate::minimap::Layout>,
    /// Minimap row infos of rows without token colors (those only depend on the text), valid for the key.
    mm_cache: (u64, std::collections::HashMap<usize, crate::minimap::LineInfo>),
    pub conflicts: Vec<crate::gitx::Conflict>,
    pub conflicts_due: Option<Instant>,
    pub conflict_hits: Vec<(Rect, usize, crate::gitx::Pick)>,
    pub debug_line: Option<usize>,
    pub sem: std::collections::HashMap<usize, Vec<(usize, usize, u8, bool)>>,
    pub sem_valid_until: usize,
    pub sem_due: Option<Instant>,
    /// Windowed semantic tokens and the cheap outline of big files (see `bigfile`).
    pub big: crate::bigfile::Big,
    pub lenses: Vec<Lens>,
    pub lens_due: Option<Instant>,
    pub lens_hits: Vec<(Rect, usize)>,
    pub lens_refs: bool,
    pub lens_impls: bool,
    pub symbols_due: Option<Instant>,
    pub biome_open: bool,
    pub biome_root: Option<Option<PathBuf>>,
    pub lint_changed_at: Option<Instant>,
    pub git: Vec<GitMark>,
    pub git_base: Option<String>,
    pub version: i32,
    pub lsp_open: bool,
    pub lsp_root: Option<PathBuf>,
    pub changed_at: Option<Instant>, // last edit not yet sent to the language server
    pub git_stale: bool,
    pub disk_mtime: Option<std::time::SystemTime>,
    pub conflict: bool, // changed on disk while we hold unsaved edits
    /// Active snippet: remaining/all stops (absolute char ranges), current index, buffer length when the
    /// current stop was selected (to shift later stops by what the user typed).
    pub snippet: Option<(Vec<(usize, usize)>, usize, usize)>,
    pub blame: Option<(usize, String)>,
    /// Side-by-side Markdown reader (None = closed, costs nothing).
    pub md: Option<Box<crate::app_md::MdState>>,
    /// LSP inlay hints for the lines around the viewport (empty = no cost anywhere).
    pub inlay: crate::inlay::InlayState,
    /// Pinned tab (drawn first, kept by "Close all").
    pub pinned: bool,
}

#[derive(Clone, Debug)]
pub struct LspEdit {
    pub sl: usize,
    pub sc: usize,
    pub el: usize,
    pub ec: usize,
    pub text: String,
}

/// An inlay hint under the pointer: the line, its index in `inlay.hints.hints_on(line)` and the label
/// char under the pointer (`None` over the paddings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HintHit {
    pub line: usize,
    pub idx: usize,
    pub off: Option<usize>,
}

pub struct DrawOpts<'a> {
    pub dt: f64,
    pub find: &'a [(usize, usize)],
    pub find_cur: Option<usize>,
    /// False for the unfocused pane of a split editor: no current-line band, dim caret.
    pub focused: bool,
}

impl Doc {
    pub fn new(buf: Buffer, hl: Option<Highlighter>, lang: Option<&'static str>, comment: &str, cell_w: f32) -> Self {
        let name = buf
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "untitled".to_string(), |n| n.to_string_lossy().into_owned());
        let mut ed = Editor::new(&buf, comment);
        ed.jsx = matches!(lang, Some("tsx" | "jsx" | "html" | "vue" | "svelte"));
        let disk_mtime = buf.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok());
        Self {
            buf,
            ed,
            hl,
            name,
            lang,
            top: 0.0,
            target: 0.0,
            animating: false,
            left: 0.0,
            reveal: false,
            caret: (0.0, 0.0),
            caret_init: false,
            caret_animating: false,
            caret_snap: false,
            rect: Rect::new(0, 0, 800, 600),
            cell_w,
            diags: Vec::new(),
            diags_ts: Vec::new(),
            diags_lint: Vec::new(),
            diags_eslint: Vec::new(),
            eslint_due: None,
            test_marks: Vec::new(),
            sel_stack: Vec::new(),
            symbols: Vec::new(),
            bps: Vec::new(),
            minimap: true,
            sb: None,
            sb_hot: false,
            sb_drag: false,
            hb: None,
            hb_hot: false,
            hb_drag: false,
            left_target: 0.0,
            scr: Default::default(),
            indent_guides: true,
            sticky: true,
            view: Default::default(),
            fold_pills: Vec::new(),
            minimap_layout: None,
            mm_cache: (0, Default::default()),
            conflicts: Vec::new(),
            conflicts_due: Some(Instant::now()),
            conflict_hits: Vec::new(),
            debug_line: None,
            sem: Default::default(),
            sem_valid_until: usize::MAX,
            sem_due: Some(Instant::now()),
            big: Default::default(),
            lenses: Vec::new(),
            lens_due: Some(Instant::now()),
            lens_hits: Vec::new(),
            lens_refs: true,
            lens_impls: true,
            symbols_due: Some(Instant::now()),
            biome_open: false,
            biome_root: None,
            lint_changed_at: None,
            git: Vec::new(),
            git_base: None,
            version: 1,
            lsp_open: false,
            lsp_root: None,
            changed_at: None,
            git_stale: true,
            disk_mtime,
            conflict: false,
            snippet: None,
            blame: None,
            md: None,
            inlay: Default::default(),
            pinned: false,
        }
    }

    /// Insert snippet `body` replacing chars [from, to) and start its tab-stop session.
    pub fn insert_snippet(&mut self, from: usize, to: usize, body: &str) {
        use crate::buffer::{EditKind, Sel};
        let (l, _) = self.buf.line_col(from);
        let indent: String = self.buf.line(l).chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let ex = crate::snippets::expand(body, &indent);
        let before = self.ed.sel;
        self.buf.close_group();
        self.buf.replace(from, to, &ex.text, EditKind::Other, before);
        self.buf.close_group();
        let stops: Vec<(usize, usize)> = ex.stops.iter().map(|(a, b)| (from + a, from + b)).collect();
        let first = stops[0];
        self.ed.sel = Sel { anchor: first.0, head: first.1 };
        self.buf.set_after(self.ed.sel);
        self.snippet = if stops.len() > 1 { Some((stops, 0, self.buf.len_chars())) } else { None };
        self.reveal = true;
        self.sync_edits();
    }

    /// Tab inside a snippet: jump to the next stop. Returns false when no snippet is active.
    pub fn snippet_next(&mut self) -> bool {
        use crate::buffer::Sel;
        let Some((stops, idx, last_len)) = self.snippet.take() else { return false };
        let delta = self.buf.len_chars() as isize - last_len as isize;
        let cur_start = stops[idx].0;
        let mut stops = stops;
        // What was typed inside the current stop shifts every later stop.
        for s in stops.iter_mut().skip(idx + 1) {
            if s.0 >= cur_start {
                s.0 = (s.0 as isize + delta).max(0) as usize;
                s.1 = (s.1 as isize + delta).max(0) as usize;
            }
        }
        let next = idx + 1;
        if next >= stops.len() {
            return true;
        }
        let len = self.buf.len_chars();
        let (a, b) = (stops[next].0.min(len), stops[next].1.min(len));
        self.ed.sel = Sel { anchor: a, head: b };
        self.reveal = true;
        self.snippet = if next + 1 < stops.len() { Some((stops, next, self.buf.len_chars())) } else { None };
        true
    }

    /// Symbols that contain `line`, outermost first (the breadcrumb chain / sticky headers).
    pub fn symbol_chain(&self, line: usize) -> Vec<&Sym> {
        let mut chain: Vec<&Sym> = Vec::new();
        for s in &self.symbols {
            if s.sl <= line && line <= s.el && s.depth == chain.len() {
                chain.push(s);
            } else if s.depth < chain.len() && s.sl <= line && line <= s.el {
                chain.truncate(s.depth);
                chain.push(s);
            }
        }
        chain
    }

    /// Click/drag on the minimap: scroll so the pointer's line is centered.
    /// Drag the scrollbar thumb: pointer at `y`, grabbed `grab` px below the thumb's top.
    pub fn scrollbar_drag(&mut self, y: f64, grab: f64) {
        if let Some(b) = self.sb {
            let total = self.total_rows() as f64;
            let pos = b.pos_for(y, grab, total, self.visible_lines()).min(self.max_top());
            self.scroll_direct(pos - self.target);
        }
    }

    pub fn minimap_scroll_to(&mut self, y: f64) {
        if let Some(l) = &self.minimap_layout {
            // The minimap is drawn by visual rows (wrap and folds included), so its unit is a row.
            let top = crate::minimap::drag_to_top_line(l, y, self.total_rows(), self.visible_lines() as usize);
            self.scroll_direct(top as f64 - self.target);
        }
    }

    pub fn stat_mtime(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.path()?).ok()?.modified().ok()
    }

    /// Replace the text with what is on disk, keeping cursor (clamped) and scroll. Clears undo.
    pub fn reload_from_disk(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path().map(Path::to_path_buf) else { return Ok(()) };
        let fresh = Buffer::open(&path)?;
        let sel = self.ed.sel;
        self.buf = fresh;
        let len = self.buf.len_chars();
        self.ed.sel = crate::buffer::Sel { anchor: sel.anchor.min(len), head: sel.head.min(len) };
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(0);
        }
        self.buf.take_dirty_line();
        self.view.folds.unfold_all();
        self.view.avail_ver = -1;
        self.view.reset();
        self.version += 1;
        self.inlay.reset();
        self.changed_at = Some(Instant::now());
        self.git_stale = true;
        self.disk_mtime = self.stat_mtime();
        self.conflict = false;
        self.caret_snap = true;
        self.target = self.target.clamp(0.0, self.max_top());
        self.top = self.top.clamp(0.0, self.max_top());
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.buf.path.as_deref()
    }

    /// Diagnostics from one source replace that source's previous ones; the merged list is rebuilt.
    pub fn set_source_diags(&mut self, lint: bool, v: Vec<Diag>) {
        if lint {
            self.diags_lint = v;
        } else {
            self.diags_ts = v;
        }
        self.rebuild_diags();
    }

    pub fn set_eslint_diags(&mut self, v: Vec<Diag>) {
        self.diags_eslint = v;
        self.rebuild_diags();
    }

    fn rebuild_diags(&mut self) {
        let mut all = self.diags_ts.clone();
        all.extend(self.diags_lint.iter().cloned());
        all.extend(self.diags_eslint.iter().cloned());
        all.sort_by_key(|d| (d.sl, d.sc));
        self.diags = all;
    }

    /// URI as the project sees it (never the language-server mirror).
    pub fn real_uri(&self) -> Option<String> {
        self.path().map(path_to_uri)
    }

    pub fn uri(&self) -> Option<String> {
        self.path().map(|p| path_to_uri(&to_mirror(p)))
    }

    // ── layout ──

    pub fn gutter_digits(&self) -> usize {
        self.buf.line_count().to_string().len().max(MIN_GUTTER_DIGITS)
    }

    pub fn gutter_w(&self) -> i32 {
        ((self.gutter_digits() + 3) as f32 * self.cell_w).round() as i32 + crate::docview::FOLD_W
    }

    pub fn visible_lines(&self) -> f64 {
        (self.rect.h as f32 / line_h()).floor().max(1.0) as f64
    }

    pub fn max_top(&self) -> f64 {
        (self.total_rows() as f64 - self.visible_lines()).max(0.0)
    }

    // ── edits ──

    /// Call after anything that may have changed the text. Returns true if it did.
    pub fn sync_edits(&mut self) -> bool {
        let Some(l) = self.buf.take_dirty_line() else { return false };
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(l);
        }
        self.caret_snap = true;
        self.version += 1;
        self.changed_at = Some(Instant::now());
        self.lint_changed_at = Some(Instant::now());
        self.eslint_due = Some(Instant::now() + std::time::Duration::from_millis(700));
        self.sel_stack.clear();
        let recs = self.view_on_edit(l);
        let buf = &self.buf;
        self.inlay.on_edits(&recs, l, buf.line_count(), &|p| buf.line_col(p));
        self.symbols_due = Some(Instant::now() + std::time::Duration::from_millis(600));
        self.conflicts_due = Some(Instant::now() + std::time::Duration::from_millis(400));
        self.sem_due = Some(Instant::now() + std::time::Duration::from_millis(450));
        self.lens_due = Some(Instant::now() + std::time::Duration::from_millis(1200));
        self.sem_valid_until = self.sem_valid_until.min(l);
        self.git_stale = true;
        self.target = self.target.clamp(0.0, self.max_top());
        self.top = self.top.clamp(0.0, self.max_top());
        true
    }

    // ── scrolling ──

    /// Standard scrolling: no easing, the view moves exactly where asked.
    pub fn scroll_by(&mut self, lines: f64) {
        self.scroll_direct(lines);
    }

    /// Touchpads already smooth their own deltas: follow them 1:1, no extra easing.
    pub fn scroll_direct(&mut self, lines: f64) {
        self.target = (self.target + lines).clamp(0.0, self.max_top());
        self.top = self.target;
        self.animating = false;
    }

    /// Keep the cursor inside the viewport.
    pub fn apply_reveal(&mut self) {
        self.view_reveal_caret();
        let (row, vcol) = self.caret_geom(self.ed.head());
        let vis = self.visible_lines();
        let l = row as f64;
        let before = self.target;
        if l < self.target {
            self.target = l;
        } else if l + 1.0 > self.target + vis {
            self.target = l + 1.0 - vis;
        }
        self.target = self.target.clamp(0.0, self.max_top());
        if (self.target - before).abs() > f64::EPSILON {
            let top_was = self.top;
            self.top = self.target;
            self.animating = false;
            if self.scr.glide_next {
                // Ctrl+Home/End: glide there instead of jumping (the caret glides with the view).
                self.top = top_was.clamp(0.0, self.max_top());
                self.start_reveal_glide();
            }
        }
        self.scr.glide_next = false;
        if self.view.cols > 0 {
            self.left = 0.0; // wrapped text never scrolls sideways
            self.left_target = 0.0;
            return;
        }
        let cw = self.cell_w as f64;
        let cx = vcol as f64 * cw;
        let view_w = self.text_view_w();
        let margin = cw * 4.0;
        if cx < self.left {
            self.left = (cx - margin).max(0.0);
        } else if cx + cw > self.left + view_w {
            self.left = cx + cw - view_w + margin;
        }
        self.left_target = self.left;
    }

    // ── coordinates ──

    /// Char index under window position (x, y).
    pub fn pos_at(&self, x: f64, y: f64) -> usize {
        self.hit_at(x, y).0
    }

    /// Char index under window position (x, y) and the inlay hint the pointer is over, if any
    /// (the char index is then the hint's own column). Works on wrapped rows and with folds.
    pub fn hit_at(&self, x: f64, y: f64) -> (usize, Option<HintHit>) {
        let lh = line_h() as f64;
        let rel_y = y - self.rect.y as f64;
        let row = ((rel_y + self.top * lh) / lh).floor().max(0.0) as usize;
        let tx = (self.rect.x + self.gutter_w()) as f64;
        let cw = self.cell_w as f64;
        let docx = x - tx + self.left;
        let vcol = (docx / cw).round().max(0.0) as usize;
        let cell = (docx / cw).floor().max(0.0) as usize;
        let tab_w = self.ed.tab_w;
        // (char column, hint hit) on `text` with the row's hints (row-relative columns); `idx` maps back to the line's hints.
        let hinted = |line: usize, text: &str, hs: &[crate::inlay::Hint], idx: Option<&[usize]>| {
            if hs.is_empty() {
                return (col_from_visual(text, vcol, tab_w), None);
            }
            let (col, _) = crate::inlay::hit_in(hs, text, vcol, tab_w);
            let hit = crate::inlay::hit_in(hs, text, cell, tab_w).1.map(|(i, start)| HintHit {
                line,
                idx: idx.map_or(i, |m| m[i]),
                off: hs[i].char_at_px((docx - start as f64 * cw) as f32, self.cell_w),
            });
            (col, hit)
        };
        if !self.vm_live() {
            let line = row.min(self.buf.line_count() - 1);
            let text = self.buf.line(line);
            let (col, hit) = hinted(line, &text, self.inlay.hints.hints_on(line), None);
            return (self.buf.pos(line, col), hit);
        }
        let (line, sub) = self.view.map.line_at_row(row);
        let text = self.buf.line(line);
        if self.view.cols == 0 {
            let (col, hit) = hinted(line, &text, self.inlay.hints.hints_on(line), None);
            return (self.buf.pos(line, col), hit);
        }
        let breaks = self.view.breaks(&text);
        let len = text.chars().count();
        let (c0, c1) = crate::docview::row_range(&breaks, sub, len);
        let rowtext: String = text.chars().skip(c0).take(c1 - c0).collect();
        let rh = crate::inlay::row_hints(self.inlay.hints.hints_on(line), len, c0, c1, sub >= breaks.len());
        let (rc, hit) = hinted(line, &rowtext, &rh.hints, Some(&rh.idx));
        let mut c = c0 + rc;
        if sub < breaks.len() {
            c = c.min(c1.saturating_sub(1).max(c0)); // not the last row: stay before the wrap point
        }
        (self.buf.pos(line, c), hit)
    }

    /// Window position (x, y of the cell's top-left) of a char index, if visible.
    pub fn screen_pos(&self, c: usize) -> Option<(i32, i32)> {
        let (row, v) = self.caret_geom(c);
        let x = self.rect.x + self.gutter_w() + (v as f32 * self.cell_w).round() as i32 - self.left.round() as i32;
        let y = self.rect.y + (row as f64 * line_h() as f64 - self.top * line_h() as f64).round() as i32;
        (y >= self.rect.y - line_h() as i32 && y < self.rect.bottom()).then_some((x, y))
    }

    // ── LSP positions (UTF-16 columns) ──

    pub fn lsp_pos(&self, c: usize) -> (usize, usize) {
        let (l, col) = self.buf.line_col(c);
        let u16col: usize = self.buf.line(l).chars().take(col).map(char::len_utf16).sum();
        (l, u16col)
    }

    pub fn pos_from_lsp(&self, line: usize, u16col: usize) -> usize {
        let line = line.min(self.buf.line_count() - 1);
        let mut n = 0;
        let mut col = 0;
        for ch in self.buf.line(line).chars() {
            if n >= u16col {
                break;
            }
            n += ch.len_utf16();
            col += 1;
        }
        self.buf.pos(line, col)
    }

    /// Replace the whole text with `new` as a single undo step, touching only the part that
    /// differs so the cursor and highlighting of the untouched prefix/suffix survive.
    pub fn replace_text(&mut self, new: &str) {
        use crate::buffer::{EditKind, Sel};
        let old = self.buf.to_string();
        if old == new {
            return;
        }
        let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
        let pre = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
        let suf = o[pre..].iter().rev().zip(n[pre..].iter().rev()).take_while(|(a, b)| a == b).count();
        let (oe, ne) = (o.len() - suf, n.len() - suf);
        let mid: String = n[pre..ne].iter().collect();
        let before = self.ed.sel;
        let shift = |p: usize| -> usize {
            if p <= pre {
                p
            } else if p >= oe {
                p + ne - oe
            } else {
                pre + (ne - pre).min(p - pre)
            }
        };
        self.buf.close_group();
        self.buf.begin_compound(before);
        self.buf.replace(pre, oe, &mid, EditKind::Other, before);
        let len = self.buf.len_chars();
        self.ed.sel = Sel { anchor: shift(before.anchor).min(len), head: shift(before.head).min(len) };
        self.buf.end_compound(self.ed.sel);
        self.sync_edits();
    }

    /// Apply LSP text edits (positions in the *current* text) as one undo step.
    /// The selection is shifted by edits that happen before it.
    pub fn apply_lsp_edits(&mut self, edits: &[LspEdit]) {
        let ranges: Vec<(usize, usize, String)> = edits
            .iter()
            .map(|e| (self.pos_from_lsp(e.sl, e.sc), self.pos_from_lsp(e.el, e.ec), e.text.clone()))
            .collect();
        self.apply_char_edits(ranges);
    }

    /// Apply `(start, end, text)` replacements (char positions in the *current* text) as one undo step.
    pub fn apply_char_edits(&mut self, mut ranges: Vec<(usize, usize, String)>) {
        use crate::buffer::{EditKind, Sel};
        if ranges.is_empty() {
            return;
        }
        // Last edit first so earlier positions stay valid; edits at the same spot keep their list
        // order in the result (the later one is applied first), as LSP requires.
        let mut order: Vec<usize> = (0..ranges.len()).collect();
        order.sort_by(|&a, &b| ranges[b].0.cmp(&ranges[a].0).then(ranges[b].1.cmp(&ranges[a].1)).then(b.cmp(&a)));
        ranges = order.into_iter().map(|k| ranges[k].clone()).collect();
        let before = self.ed.sel;
        let shift = |p: usize| {
            let mut np = p as isize;
            for (s, e, t) in &ranges {
                let (s, e) = (*s, *e);
                let n = t.chars().count() as isize;
                if e <= p {
                    np += n - (e - s) as isize;
                } else if s < p {
                    np = s as isize + n;
                }
            }
            np.max(0) as usize
        };
        let sel = Sel { anchor: shift(before.anchor), head: shift(before.head) };
        self.buf.close_group();
        self.buf.begin_compound(before);
        for (s, e, t) in &ranges {
            self.buf.replace(*s, *e, t, EditKind::Other, before);
        }
        let len = self.buf.len_chars();
        self.ed.sel = Sel { anchor: sel.anchor.min(len), head: sel.head.min(len) };
        self.buf.end_compound(self.ed.sel);
        self.sync_edits();
    }

    /// Double click on an inlay hint: apply its text edits (or the type annotation synthesized from
    /// a `: T` hint) as one undo step. False when the hint carries nothing to apply.
    pub fn apply_hint_edits(&mut self, hit: HintHit) -> bool {
        let Some(h) = self.inlay.hints.hints_on(hit.line).get(hit.idx) else { return false };
        let edits = h.annotation_edits();
        if edits.is_empty() {
            return false;
        }
        let (hl, hc) = (h.line as i64, h.col as i64);
        let n = self.buf.line_count() as i64;
        let at = |dl: i64, c: i64| -> Option<usize> {
            let l = hl + dl;
            if l < 0 || l >= n {
                return None;
            }
            let col = if dl == 0 { hc + c } else { c };
            let len = self.buf.line_len(l as usize) as i64;
            Some(self.buf.pos(l as usize, col.clamp(0, len) as usize))
        };
        let ranges: Vec<(usize, usize, String)> =
            edits.iter().filter_map(|e| Some((at(e.dl0, e.c0)?, at(e.dl1, e.c1)?, e.text.clone()))).collect();
        if ranges.is_empty() {
            return false;
        }
        self.apply_char_edits(ranges);
        self.ed.sel = crate::buffer::Sel { anchor: self.ed.sel.head, head: self.ed.sel.head };
        true
    }

    /// The link of the hint label under the pointer (Ctrl+click target), if any.
    pub fn hint_link(&self, hit: HintHit) -> Option<crate::inlay::HintLink> {
        let h = self.inlay.hints.hints_on(hit.line).get(hit.idx)?;
        h.link_at(hit.off?).cloned()
    }

    // ── drawing ──

    /// Draws into `rect`. Returns true while more frames are needed (animation, tokenizing).
    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect, o: &DrawOpts) -> bool {
        // The minimap takes a strip at the right; everything else lays out in what remains.
        let full = rect;
        let mm_w = if self.minimap && rect.w > 560 { crate::minimap::style().width() } else { 0 };
        let rect = Rect::new(rect.x, rect.y, rect.w - mm_w, rect.h);
        self.rect = rect;
        self.cell_w = tx.cell_w;
        self.sync_edits();
        let gutter_w = self.gutter_w();
        let gw0 = gutter_w - FOLD_W; // width of the number/marker part of the gutter
        let cols_avail = (((rect.w - gutter_w - 8) as f32 / self.cell_w).floor() as isize - 1).max(8) as usize;
        self.view_prepare(cols_avail);
        if self.reveal {
            self.apply_reveal();
            self.reveal = false;
        }
        self.step_scroll(o.dt);
        let lh_f = line_h();
        let lh = lh_f as i32;
        let nrows_want = (rect.h as f32 / lh_f).ceil() as usize + 1;
        self.view_sync_visible(nrows_want);

        let cell_w = self.cell_w;
        let body = Rect::new(rect.x + gutter_w, rect.y, (rect.w - gutter_w).max(0), rect.h);
        let first_row = self.top as usize;
        let y_off = -(((self.top - first_row as f64) * lh_f as f64).round() as i32);
        let live = self.vm_live();
        // (line, row within the line) for every visible row.
        let row_lines: Vec<(usize, usize)> = if live {
            self.view.map.visible_lines(first_row, nrows_want)
        } else {
            (first_row..(first_row + nrows_want).min(self.buf.line_count())).map(|l| (l, 0)).collect()
        };
        let mut vlines: Vec<usize> = Vec::with_capacity(row_lines.len());
        for &(l, _) in &row_lines {
            if vlines.last() != Some(&l) {
                vlines.push(l);
            }
        }
        let first = vlines.first().copied().unwrap_or(0);
        let last = vlines.last().map_or(0, |l| l + 1);
        let tab_w = self.ed.tab_w;
        let left = self.left;
        let (cur_line, _) = self.buf.line_col(self.ed.head());
        let sel = self.ed.sel;
        let c = &theme.chrome;
        let inl = &self.inlay.hints;

        // Highlight: tokenize forward under a time budget, then colorize the visible lines.
        let mut pending = false;
        let mut line_spans: Vec<Option<Spans>> = Vec::with_capacity(vlines.len());
        if let Some(hl) = self.hl.as_mut() {
            hl.advance(&self.buf, last, Duration::from_millis(8));
            pending = hl.ready() <= last;
            for &i in &vlines {
                line_spans.push(hl.spans(&self.buf, theme, i));
            }
        }
        let lines: Vec<String> = vlines.iter().map(|&i| self.buf.line(i)).collect();
        // Semantic colors over the TextMate ones (only lines the last response still describes).
        if !self.sem.is_empty() {
            let mut cache: std::collections::HashMap<(u8, bool), Option<Rgba>> = std::collections::HashMap::new();
            for (k, &l) in vlines.iter().enumerate() {
                if l >= self.sem_valid_until {
                    break;
                }
                let (Some(toks), Some(Some(sp))) = (self.sem.get(&l), line_spans.get(k)) else { continue };
                let colored: Vec<(usize, usize, Rgba)> = toks
                    .iter()
                    .filter_map(|&(s, len, class, ro)| {
                        let col = *cache.entry((class, ro)).or_insert_with(|| {
                            let scope = sem_scope(class, ro)?;
                            let c = theme.resolve(&["source.ts", scope]).fg?;
                            (c != theme.foreground).then_some(c)
                        });
                        col.map(|c| (s, len, c))
                    })
                    .collect();
                if !colored.is_empty() {
                    line_spans[k] = Some(Rc::new(overlay_colors(sp, &lines[k], &colored)));
                }
            }
        }
        let geo: Vec<(usize, usize)> = vlines.iter().map(|&i| (self.buf.line_start(i), self.buf.line_len(i))).collect();

        // Visual rows: one per (line, wrap segment). Without wrapping this is one row per line.
        let wrap_cols = if live { self.view.cols } else { 0 };
        let mut rows: Vec<VRow> = Vec::with_capacity(row_lines.len());
        let mut row_range: Vec<(usize, usize)> = vec![(0, 0); vlines.len()];
        {
            let mut lk = 0usize;
            let mut breaks: Vec<usize> = Vec::new();
            let mut offs: Vec<usize> = Vec::new();
            for (ri, &(l, sub)) in row_lines.iter().enumerate() {
                if ri == 0 || row_lines[ri - 1].0 != l {
                    if ri > 0 {
                        lk += 1;
                    }
                    row_range[lk] = (ri, ri);
                    let text = &lines[lk];
                    breaks = if wrap_cols > 0 && (text.len() > wrap_cols || text.contains('\t')) {
                        wrap_breaks(text, wrap_cols, tab_w)
                    } else {
                        Vec::new()
                    };
                    offs.clear();
                    if !breaks.is_empty() {
                        offs.extend(text.char_indices().map(|(b, _)| b));
                        offs.push(text.len());
                    }
                }
                let nchars = geo[lk].1;
                let (c0, c1) = row_range_of(&breaks, sub, nchars);
                let (b0, b1) = if breaks.is_empty() { (0, lines[lk].len()) } else { (offs[c0], offs[c1]) };
                rows.push(VRow { lk, line: l, sub, c0, c1, b0, b1, last: sub >= breaks.len() });
                row_range[lk].1 = ri + 1;
            }
        }

        // Caret glide, in document pixel space (rows, so wrapped lines and folds are accounted for).
        let (crow, cvcol) = self.caret_geom(self.ed.head());
        let target_c = (cvcol as f64 * cell_w as f64, crow as f64 * lh_f as f64);
        let far = (target_c.1 - self.caret.1).abs() > rect.h as f64 * 1.5;
        if true || !self.caret_init || self.caret_snap || far {
            self.caret = target_c;
            self.caret_init = true;
            self.caret_snap = false;
            self.caret_animating = false;
        } else if target_c != self.caret {
            self.caret_animating = true;
            let (kx, ky) = (1.0 - (-o.dt / CURSOR_TAU_X).exp(), 1.0 - (-o.dt / CURSOR_TAU_Y).exp());
            self.caret.0 += (target_c.0 - self.caret.0) * kx;
            self.caret.1 += (target_c.1 - self.caret.1) * ky;
            if (target_c.0 - self.caret.0).abs() < 0.3 && (target_c.1 - self.caret.1).abs() < 0.3 {
                self.caret = target_c;
                self.caret_animating = false;
            }
        }
        let caret_y = rect.y + (self.caret.1 - self.top * lh_f as f64).round() as i32;
        let caret_x = body.x + self.caret.0.round() as i32 - left.round() as i32;

        // Inlay hints per wrapped row (columns relative to the row start); unwrapped rows use the line's own.
        let row_hs: Vec<crate::inlay::RowHints> = if wrap_cols > 0 && !inl.is_empty() {
            rows.iter()
                .map(|row| {
                    let hs = inl.hints_on(row.line);
                    if hs.is_empty() {
                        Default::default()
                    } else {
                        crate::inlay::row_hints(hs, geo[row.lk].1, row.c0, row.c1, row.last)
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let ry = |r: usize| rect.y + (r as f32 * lh_f) as i32 + y_off;
        let vx = |vc: usize| body.x + (vc as f32 * cell_w).round() as i32 - left.round() as i32;
        let rt = |r: usize| row_text(&rows, &lines, r);
        // Visual column of char `col` of row `r` (text + the hint cells before it).
        let vc = |r: usize, col: usize, include_at: bool| -> usize {
            crate::inlay::vcol_in(hints_for(inl, &row_hs, wrap_cols, r, rows[r].line), rt(r), col, tab_w, include_at)
        };
        // y of the first row of `line` when that line is on screen (a line scrolled partly off the top
        // reports where its first row would be).
        let line_y = |line: usize| -> Option<i32> {
            let lk = vlines.binary_search(&line).ok()?;
            let r = row_range[lk].0;
            Some(ry(r) - rows[r].sub as i32 * lh)
        };
        // (row, visual column in that row) of char `col` of a visible `line`.
        let at = |line: usize, col: usize, include_at: bool| -> Option<(usize, usize)> {
            let lk = vlines.binary_search(&line).ok()?;
            let (r0, r1) = row_range[lk];
            (r0..r1).find_map(|r| {
                let row = &rows[r];
                (col >= row.c0 && (col < row.c1 || row.last)).then(|| (r, vc(r, col - row.c0, include_at)))
            })
        };
        let at_end = |line: usize| -> Option<(usize, usize)> {
            let _lk = vlines.binary_search(&line).ok()?;
            at(line, usize::MAX, true)
        };

        cv.fill(rect, theme.background);

        // Merge conflicts: tinted regions plus inline actions above each one.
        self.conflict_hits.clear();
        self.fold_pills.clear();
        for (ci, cf) in self.conflicts.iter().enumerate() {
            if cf.end < first || cf.start >= last {
                continue;
            }
            let ours = c.git_added.with_alpha(0x2a);
            let theirs = c.info.with_alpha(0x2a);
            let marker = c.border.with_alpha(0xa0);
            for (r, row) in rows.iter().enumerate() {
                let l = row.line;
                if l < cf.start || l > cf.end {
                    continue;
                }
                let color = if l == cf.start || l == cf.mid || l == cf.end || cf.base == Some(l) {
                    marker
                } else if l < cf.mid {
                    ours
                } else {
                    theirs
                };
                cv.fill_clip(Rect::new(rect.x, ry(r), rect.w, lh), color, rect);
            }
            if let Some((r, v)) = at_end(cf.start) {
                let mut x = vx(v) + (cell_w * 3.0) as i32;
                for (label, pick) in [
                    ("Aceitar atual", crate::gitx::Pick::Ours),
                    ("Aceitar entrada", crate::gitx::Pick::Theirs),
                    ("Aceitar ambos", crate::gitx::Pick::Both),
                ] {
                    let w = tx.ui_width(label, false);
                    tx.draw_ui(cv, x, ry(r) + (lh - 18) / 2, label, c.accent_text, body, true);
                    self.conflict_hits.push((Rect::new(x, ry(r), w, lh), ci, pick));
                    x += w + 18;
                }
            }
        }

        // Debugger: the line it is stopped on.
        if let Some(dl) = self.debug_line {
            for (r, row) in rows.iter().enumerate() {
                if row.line == dl {
                    cv.fill_clip(Rect::new(rect.x, ry(r), rect.w, lh), c.warning.with_alpha(0x38), rect);
                }
            }
        }

        // Current line highlight (follows the gliding caret), then find matches and selection.
        if sel.is_empty() && o.focused {
            if live {
                for (r, row) in rows.iter().enumerate() {
                    if row.line == cur_line {
                        cv.fill_clip(Rect::new(rect.x, ry(r), rect.w, lh), theme.line_highlight, rect);
                    }
                }
            } else {
                cv.fill_clip(Rect::new(rect.x, caret_y, rect.w, lh), theme.line_highlight, rect);
            }
        }
        let paint_range = |cv: &mut Canvas, a: usize, b: usize, color: Rgba| {
            for (lk, _) in vlines.iter().enumerate() {
                let (ls, ll) = geo[lk];
                let le = ls + ll;
                if b <= ls || a > le {
                    continue;
                }
                let (r0, r1) = row_range[lk];
                for (r, row) in rows.iter().enumerate().take(r1).skip(r0) {
                    let (rs, re) = (ls + row.c0, ls + row.c1 + usize::from(row.last));
                    let (s, e) = (a.max(rs), b.min(re));
                    if s >= e {
                        continue;
                    }
                    let ca = s - rs;
                    let cb = (e - rs).min(row.c1 - row.c0);
                    let extra = usize::from(row.last && b > le);
                    let va = vc(r, ca, true);
                    let vb = vc(r, cb, false) + extra;
                    cv.fill_clip(Rect::new(vx(va), ry(r), vx(vb) - vx(va), lh), color, body);
                }
            }
        };
        for (i, &(a, b)) in o.find.iter().enumerate() {
            let color = if Some(i) == o.find_cur { c.find_match_current } else { c.find_match };
            paint_range(cv, a, b, color);
        }
        if !sel.is_empty() {
            paint_range(cv, sel.start(), sel.end(), theme.selection);
        }
        for x in &self.ed.extra {
            if !x.is_empty() {
                paint_range(cv, x.start(), x.end(), theme.selection);
            }
        }

        // Indent guides.
        if self.indent_guides {
            let unit_w = if self.ed.indent.contains('\t') { tab_w } else { self.ed.indent.chars().count().max(1) };
            let guide = theme.line_number.with_alpha(0x38);
            let mut carry = 0usize;
            let mut widths: Vec<usize> = Vec::with_capacity(lines.len());
            for line in lines.iter() {
                let mut lead = 0usize;
                for ch in line.chars() {
                    match ch {
                        ' ' => lead += 1,
                        '\t' => lead += tab_w - lead % tab_w,
                        _ => break,
                    }
                }
                let blank = line.trim().is_empty();
                widths.push(if blank { carry } else { lead });
                if !blank {
                    carry = lead;
                }
            }
            for (r, row) in rows.iter().enumerate() {
                let width = widths[row.lk];
                let mut g = unit_w;
                while g < width {
                    let x = vx(g);
                    cv.fill_clip(Rect::new(x, ry(r), 1, lh), guide, body);
                    g += unit_w;
                }
            }
        }

        // Matching bracket.
        if sel.is_empty()
            && let Some((a, b)) = crate::editor::match_bracket(&self.buf, self.ed.head())
        {
            for p in [a, b] {
                let (l, col) = self.buf.line_col(p);
                if let Some((r, v)) = at(l, col, true) {
                    cv.fill_clip(Rect::new(vx(v), ry(r), (cell_w).round() as i32, lh), c.accent.with_alpha(0x55), body);
                }
            }
        }

        // Body text: only the visible column window of each row is shaped, so very long
        // lines cost the same as short ones.
        let fg = theme.foreground;
        let vc0 = (left / cell_w as f64).floor().max(0.0) as usize;
        let vc1 = vc0 + ((rect.w - gutter_w) as f32 / cell_w).ceil() as usize + 3;
        let mut pieces: Vec<(String, Rgba, u8)> = Vec::new();
        let hint_fg = theme.line_number;
        let mut labels: Vec<(i32, i32, &str)> = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            let hs = hints_for(inl, &row_hs, wrap_cols, r, row.line);
            if hs.is_empty() {
                window_pieces(
                    &lines[row.lk],
                    line_spans.get(row.lk).and_then(|s| s.as_ref()),
                    row.b0,
                    row.b1,
                    vc0,
                    vc1,
                    tab_w,
                    fg,
                    &mut pieces,
                );
            } else {
                // Inlay hints: a faint pill behind each, its blank cells in the text; the labels
                // (smaller font) are drawn over them after the text.
                hint_decor(cv, rt(r), hs, tab_w, &vx, ry(r), lh, cell_w, hint_fg, body, &mut labels);
                crate::inlay::window_pieces_hinted(
                    &lines[row.lk],
                    line_spans.get(row.lk).and_then(|s| s.as_ref()),
                    row.b0,
                    row.b1,
                    hs,
                    vc0,
                    vc1,
                    tab_w,
                    fg,
                    &mut pieces,
                );
            }
            pieces.push(("\n".to_string(), fg, 0));
        }
        let body_x = body.x + (vc0 as f32 * cell_w).round() as i32 - left.round() as i32;
        tx.draw_styled(cv, body_x, rect.y + y_off, &pieces, body);
        for (x, y, label) in labels.drain(..) {
            tx.draw_hint(cv, x, y, label, hint_fg, body);
        }

        // Folded regions: a "..." marker after the header line (click to unfold).
        if !self.view.folds.is_empty() {
            for (r, row) in rows.iter().enumerate() {
                if !row.last || self.view.folds.is_fold_start(row.line).is_none() {
                    continue;
                }
                let x = vx(vc(r, row.c1 - row.c0, true)) + cell_w as i32;
                if x + 26 < body.right() {
                    let pill = Rect::new(x, ry(r) + 4, 26, lh - 8);
                    cv.fill_round(pill, 4, c.border.with_alpha(0xc0));
                    for k in 0..3 {
                        cv.fill(Rect::new(pill.x + 7 + k * 5, pill.y + pill.h / 2 - 1, 2, 2), theme.line_number);
                    }
                    self.fold_pills.push((pill, row.line));
                }
            }
        }

        // Color swatches next to #rgb / #rrggbb / #rrggbbaa literals.
        for (k, line) in lines.iter().enumerate() {
            if !line.contains('#') || line.len() > 4000 {
                continue;
            }
            let chars: Vec<char> = line.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                if chars[i] == '#' && (i == 0 || !chars[i - 1].is_alphanumeric()) {
                    let n = chars[i + 1..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
                    let boundary = chars.get(i + 1 + n).is_none_or(|c| !c.is_alphanumeric());
                    if matches!(n, 3 | 6 | 8) && boundary {
                        let lit: String = chars[i..i + 1 + n].iter().collect();
                        if let Some(col) = Rgba::parse(&lit)
                            && let Some((r, end)) = at(vlines[k], i + 1 + n, false)
                        {
                            let x = vx(end) + 4;
                            if x > body.x && x + 10 < body.right() {
                                let rr = Rect::new(x, ry(r) + (lh - 10) / 2, 10, 10);
                                cv.fill_round(rr, 3, c.border);
                                cv.fill_round(rr.inset(1), 2, col);
                            }
                        }
                        i += n;
                    }
                }
                i += 1;
            }
        }

        // Code lens: reference/implementation counts after the line's code. Several lenses of one
        // line (e.g. references and implementations) are laid out side by side, separated by a dot.
        self.lens_hits.clear();
        let mut order: Vec<usize> = (0..self.lenses.len()).collect();
        order.sort_by_key(|&i| (self.lenses[i].line, self.lenses[i].col));
        let mut cursor: Option<(usize, i32)> = None; // (line, next x)
        for idx in order {
            let lz = &self.lenses[idx];
            let Some(title) = &lz.title else { continue };
            let is_impl = title.to_lowercase().contains("implementation");
            if (is_impl && !self.lens_impls) || (!is_impl && !self.lens_refs) {
                continue;
            }
            let Some((r, vend)) = at_end(lz.line) else { continue };
            let w = tx.ui_width(title, false);
            let sep = "\u{b7}";
            let sw = tx.ui_width(sep, false);
            let (x, sep_x) = match cursor {
                Some((l, nx)) if l == lz.line => (nx + (cell_w as i32) * 2 + sw, Some(nx + cell_w as i32 / 2 + 2)),
                _ => (vx(vend) + (cell_w * 3.0) as i32, None),
            };
            if x + w >= body.right() {
                continue;
            }
            if let Some(sx) = sep_x {
                tx.draw_ui(cv, sx, ry(r) + (lh - 18) / 2, sep, c.tab_inactive_fg, body, false);
            }
            let rr = Rect::new(x, ry(r), w, lh);
            tx.draw_ui(cv, x, ry(r) + (lh - 18) / 2, title, c.tab_inactive_fg, body, false);
            self.lens_hits.push((rr, idx));
            cursor = Some((lz.line, x + w));
        }

        // Inline git blame for the cursor line, after the code.
        if let Some((bl, text)) = &self.blame
            && sel.is_empty()
            && *bl == cur_line
            && let Some((r, vend)) = at_end(*bl)
        {
            let x = vx(vend) + (cell_w * 4.0) as i32;
            if x < body.right() {
                tx.draw_ui(cv, x, ry(r) + (lh - 18) / 2, text, theme.line_number, body, false);
            }
        }

        // Diagnostics: wavy underline.
        for d in self.diags.iter().filter(|d| d.severity <= 3) {
            if d.el < first || d.sl >= last {
                continue;
            }
            let color = match d.severity {
                1 => c.error,
                2 => c.warning,
                _ => c.info,
            };
            for l in d.sl.max(first)..=d.el.min(last - 1) {
                let Ok(k) = vlines.binary_search(&l) else { continue };
                let ll = geo[k].1;
                let cs = if l == d.sl { d.sc } else { 0 };
                let ce = if l == d.el { d.ec } else { ll };
                let ce = ce.max(cs + 1);
                let (a0, b0) = (cs.min(ll), ce.min(ll.max(cs + 1)).min(ll));
                let (r0, r1) = row_range[k];
                for (r, row) in rows.iter().enumerate().take(r1).skip(r0) {
                    let (lo, hi) = (a0.max(row.c0), b0.min(row.c1));
                    let (va, vb) = if lo < hi {
                        let va = vc(r, lo - row.c0, true);
                        (va, vc(r, hi - row.c0, false).max(va + 1))
                    } else if a0 == b0 && a0 >= row.c0 && (a0 < row.c1 || row.last) {
                        let va = vc(r, a0 - row.c0, true);
                        (va, va + 1)
                    } else {
                        continue;
                    };
                    let (xa, xb) = (vx(va), vx(vb));
                    let base = ry(r) + lh - 3;
                    for x in xa.max(body.x)..xb.min(body.right()) {
                        cv.dot(x, base + ((x / 2) % 2), color);
                    }
                }
            }
        }

        // Gutter: numbers, fold chevrons, git bars, diagnostic markers.
        cv.fill(Rect::new(rect.x, rect.y, gutter_w, rect.h), theme.background);
        let mut gutter: Vec<(String, Rgba)> = Vec::with_capacity(rows.len());
        let digits = self.gutter_digits();
        for row in rows.iter() {
            let color = if row.line == cur_line { theme.line_number_active } else { theme.line_number };
            gutter.push((if row.sub == 0 { format!("{:>digits$}\n", row.line + 1) } else { "\n".to_string() }, color));
        }
        tx.draw(
            cv,
            rect.x + cell_w.round() as i32,
            rect.y + y_off,
            &gutter,
            Rect::new(rect.x, rect.y, gutter_w, rect.h),
        );
        let gclip = Rect::new(rect.x, rect.y, gutter_w, rect.h);
        for (r, row) in rows.iter().enumerate() {
            if row.sub != 0 {
                continue;
            }
            let folded = self.view.folds.is_fold_start(row.line).is_some();
            if folded || self.is_foldable(row.line) {
                let (icon, color) = if folded {
                    ("ui/chevron_right", theme.line_number_active)
                } else {
                    ("ui/chevron_down", theme.line_number.with_alpha(0x80))
                };
                tx.icon(cv, icon, rect.x + gw0 + 1, ry(r) + (lh - 10) / 2, 10, color, gclip);
            }
        }
        for m in &self.git {
            let color = match m.kind {
                GitKind::Added => c.git_added,
                GitKind::Modified => c.git_modified,
                GitKind::Deleted => c.git_deleted,
            };
            if m.kind == GitKind::Deleted {
                if let Some(y) = line_y(m.line).or_else(|| (m.line == last).then(|| ry(rows.len()))) {
                    cv.fill_clip(Rect::new(rect.x + gw0 - 8, y - 1, 6, 2), color, rect);
                }
            } else {
                let i0 = rows.partition_point(|r| r.line < m.line);
                let i1 = rows.partition_point(|r| r.line < m.line + m.len.max(1));
                if i0 < i1 {
                    cv.fill_clip(Rect::new(rect.x + gw0 - 6, ry(i0), 3, ry(i1 - 1) + lh - ry(i0)), color, rect);
                }
            }
        }
        for &(l, ok) in &self.test_marks {
            if let Some(y) = line_y(l) {
                let (icon, color) = match ok {
                    Some(true) => ("ui/check", c.git_added),
                    Some(false) => ("ui/close", c.error),
                    None => ("ui/circle", theme.line_number),
                };
                tx.icon(cv, icon, rect.x + 12, y + (lh - 12) / 2, 12, color, gclip);
            }
        }
        let mut worst: std::collections::HashMap<usize, u8> = std::collections::HashMap::new();
        for d in self.diags.iter().filter(|d| d.severity <= 3) {
            let e = worst.entry(d.sl).or_insert(d.severity);
            *e = (*e).min(d.severity);
        }
        for (&l, &sev) in &worst {
            if let Some(y) = line_y(l) {
                let color = match sev {
                    1 => c.error,
                    2 => c.warning,
                    _ => c.info,
                };
                cv.fill_clip(Rect::new(rect.x + 2, y + lh / 2 - 2, 4, 4), color, rect);
            }
        }

        // Sticky scroll: headers of the scopes that contain the first visible line but have
        // scrolled off above it (from the symbol tree, else from indentation).
        if self.sticky && first > 0 {
            let mut heads: Vec<usize> = Vec::new();
            let chain = self.symbol_chain(first);
            if !chain.is_empty() {
                heads.extend(chain.iter().map(|s| s.sl).filter(|l| *l < first));
            } else {
                let indent_of = |l: usize| self.buf.line(l).chars().take_while(|c| *c == ' ' || *c == '\t').count();
                let mut cur_indent = indent_of(first);
                let mut l = first;
                while l > 0 && heads.len() < STICKY_MAX && cur_indent > 0 {
                    l -= 1;
                    let t = self.buf.line(l);
                    if t.trim().is_empty() {
                        continue;
                    }
                    let ind = indent_of(l);
                    if ind < cur_indent {
                        cur_indent = ind;
                        if t.trim_end().ends_with(['{', '(', ':', '>']) || t.trim_end().ends_with("=>") {
                            heads.push(l);
                        }
                    }
                }
                heads.reverse();
            }
            heads.dedup();
            let heads = sticky_pick(&heads, STICKY_MAX);
            // Whole-line snapping: at rest the band ends on a row boundary of the text underneath,
            // so no line is left half hidden below it (while scrolling it stays put and the text
            // glides under it).
            let at_rest = !self.animating && !self.sb_drag;
            let band_h = sticky_band_h(heads.len(), lh, -y_off, at_rest);
            if band_h > 0 {
                cv.fill(Rect::new(rect.x, rect.y, rect.w, band_h), theme.background);
            }
            for (k, l) in heads.iter().enumerate() {
                let y = rect.y + k as i32 * lh;
                let num = format!("{:>digits$}", l + 1, digits = self.gutter_digits());
                tx.draw_line(
                    cv,
                    rect.x + cell_w.round() as i32,
                    y,
                    &num,
                    theme.line_number,
                    Rect::new(rect.x, y, gutter_w, lh),
                );
                let mut pieces = Vec::new();
                let head_text = self.buf.line(*l);
                let hs = if wrap_cols == 0 { inl.hints_on(*l) } else { &[] };
                let clip = Rect::new(body.x, y, body.w, lh);
                if hs.is_empty() {
                    window_pieces(&head_text, None, 0, head_text.len(), vc0, vc1, tab_w, fg, &mut pieces);
                } else {
                    hint_decor(cv, &head_text, hs, tab_w, &vx, y, lh, cell_w, hint_fg, clip, &mut labels);
                    crate::inlay::window_pieces_hinted(
                        &head_text,
                        None,
                        0,
                        head_text.len(),
                        hs,
                        vc0,
                        vc1,
                        tab_w,
                        fg,
                        &mut pieces,
                    );
                }
                tx.draw_styled(cv, body_x, y, &pieces, clip);
                for (x, ly, label) in labels.drain(..) {
                    tx.draw_hint(cv, x, ly, label, hint_fg, clip);
                }
            }
            if band_h > 0 {
                cv.fill(Rect::new(rect.x, rect.y + band_h - 1, rect.w, 1), c.border);
            }
        }

        // Extra carets (static; only the primary one glides).
        for x in &self.ed.extra {
            let (l, col) = self.buf.line_col(x.head);
            if let Some((r, v)) = at(l, col, false) {
                cv.fill_clip(Rect::new(vx(v), ry(r), CURSOR_W, lh), theme.cursor, body);
            }
        }

        // Breakpoints and the execution arrow, in the gutter.
        for &bl in &self.bps {
            if let Some(y) = line_y(bl) {
                cv.fill_round(Rect::new(rect.x + 2, y + (lh - 10) / 2, 10, 10), 5, c.error);
            }
        }
        if let Some(dl) = self.debug_line
            && let Some(y) = line_y(dl)
        {
            tx.icon(cv, "ui/chevron_right", rect.x + 11, y + (lh - 14) / 2, 14, c.warning, gclip);
        }

        // Minimap.
        if mm_w > 0 {
            // One minimap row per visual row, so word wrap and folds show as the editor shows them.
            let total = self.total_rows();
            let l = crate::minimap::layout_for(full, mm_w, total);
            let (bg, fgc) = (theme.background, theme.foreground);
            let wrap_view = live && self.view.cols > 0;
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (
                    self.buf.version(),
                    self.buf.line_count(),
                    total,
                    live,
                    self.view.cols,
                    tab_w,
                    (fgc.0, fgc.1, fgc.2),
                    l.rect.h,
                )
                    .hash(&mut h);
                self.view.folds.hidden_ranges().hash(&mut h);
                h.finish()
            };
            if self.mm_cache.0 != key {
                self.mm_cache = (key, Default::default());
            }
            let cache = &mut self.mm_cache.1;
            let mut infos = |i: usize| {
                let (line, sub) = if live { self.view.map.line_at_row(i) } else { (i, 0) };
                // Token colors for this row: continuation rows of a wrapped line use the line's spans too.
                let sp = vlines.binary_search(&line).ok().and_then(|k| line_spans.get(k)).and_then(|s| s.as_ref());
                if sp.is_none()
                    && let Some(info) = cache.get(&i)
                {
                    return *info;
                }
                let mut text = self.buf.line(line);
                let mut b0 = 0; // byte offset of the row inside its line
                if sub > 0 && wrap_view {
                    let breaks = self.view.breaks(&text);
                    let (c0, c1) = row_range_of(&breaks, sub, text.chars().count());
                    b0 = text.char_indices().nth(c0).map_or(text.len(), |(b, _)| b);
                    text = text.chars().skip(c0).take(c1 - c0).collect();
                }
                let sp3: Option<Vec<(usize, usize, Rgba)>> =
                    sp.map(|s| crate::minimap::row_spans(s.iter().map(|&(a, b, c, _)| (a, b, c)), b0, b0 + text.len()));
                let info = crate::minimap::line_info(&text, sp3.as_deref(), fgc, tab_w);
                if sp.is_none() {
                    cache.insert(i, info);
                }
                info
            };
            let vis_rows = rect.h as f64 / lh_f as f64;
            crate::minimap::draw_rows(
                cv,
                &l,
                &mut infos,
                total,
                self.top,
                vis_rows,
                bg,
                c.accent,
                crate::minimap::style(),
            );
            cv.fill(Rect::new(l.rect.x, l.rect.y, 1, l.rect.h), c.border);
            self.minimap_layout = Some(l);
        } else {
            self.minimap_layout = None;
        }

        // Thin overlay scrollbar (right edge of the text area, left of the minimap).
        {
            let track = Rect::new(rect.right() - 14, rect.y, 14, rect.h);
            self.sb = crate::ui::VBar::new(track, self.total_rows() as f64, self.visible_lines(), self.top);
            if let Some(b) = &self.sb {
                b.draw(cv, theme.foreground, self.sb_hot, self.sb_drag);
            }
        }

        // Thin horizontal scrollbar along the bottom edge, only with wide lines (and no word wrap).
        {
            let (vw, ml) = (self.text_view_w(), self.max_left());
            let track = Rect::new(body.x, rect.bottom() - 12, (body.w - 14).max(0), 12);
            self.hb = if ml > 0.5 { crate::ui::HBar::new(track, vw + ml, vw, self.left) } else { None };
            if let Some(b) = &self.hb {
                b.draw(cv, theme.foreground, self.hb_hot, self.hb_drag);
            }
        }

        // Caret.
        let caret_color = if o.focused { theme.cursor } else { theme.cursor.with_alpha(0x70) };
        cv.fill_clip(Rect::new(caret_x, caret_y, CURSOR_W, lh), caret_color, body);

        pending || self.animating || self.caret_animating
    }
}

/// Most sticky scroll headers pinned at the top of the editor.
pub const STICKY_MAX: usize = 2;

/// The `max` innermost headers (they arrive outermost first), in the same order.
pub fn sticky_pick(heads: &[usize], max: usize) -> Vec<usize> {
    heads[heads.len().saturating_sub(max)..].to_vec()
}

/// Height in px of the sticky band for `n` headers of `lh` px each, when the text underneath is
/// scrolled `frac` px (0 <= frac < lh) past a row boundary. At rest the band is stretched down to
/// the next row boundary, so the line beneath it is never cut in half; while the text moves
/// (`at_rest == false`) it stays at exactly `n` lines (it would otherwise saw up and down).
pub fn sticky_band_h(n: usize, lh: i32, frac: i32, at_rest: bool) -> i32 {
    let base = n as i32 * lh;
    if n == 0 || !at_rest || frac <= 0 || lh <= 0 {
        return base;
    }
    base + (lh - frac.min(lh))
}

/// One visual row of the viewport: chars `c0..c1` (bytes `b0..b1`) of line `line`; `sub` is the
/// wrap segment index and `last` tells whether it is the final one of its line.
struct VRow {
    lk: usize,
    line: usize,
    sub: usize,
    c0: usize,
    c1: usize,
    b0: usize,
    b1: usize,
    last: bool,
}

/// Hints of visual row `r` (of line `line`): the whole line's when unwrapped, else the row's slice.
fn hints_for<'a>(
    inl: &'a crate::inlay::LineHints,
    row_hs: &'a [crate::inlay::RowHints],
    wrap_cols: usize,
    r: usize,
    line: usize,
) -> &'a [crate::inlay::Hint] {
    if wrap_cols == 0 { inl.hints_on(line) } else { row_hs.get(r).map_or(&[], |x| x.hints.as_slice()) }
}

/// Paint the faint pills behind the labels of one row (`text` = the row's text, `hs` its hints)
/// and queue the labels as `(x, y, text)`: each centred in its whole-cell slot, at the row's `y`.
#[allow(clippy::too_many_arguments)]
fn hint_decor<'a>(
    cv: &mut Canvas,
    text: &str,
    hs: &'a [crate::inlay::Hint],
    tab_w: usize,
    vx: &dyn Fn(usize) -> i32,
    y: i32,
    lh: i32,
    cell_w: f32,
    hint_fg: Rgba,
    clip: Rect,
    labels: &mut Vec<(i32, i32, &'a str)>,
) {
    for (h, (v, w)) in hs.iter().zip(crate::inlay::hint_cells(text, hs, tab_w)) {
        let pill = Rect::new(vx(v), y + 1, vx(v + w) - vx(v), lh - 2);
        if pill.x >= clip.x && pill.right() <= clip.right() && pill.y >= clip.y && pill.bottom() <= clip.bottom() {
            cv.fill_round(pill, 3, hint_fg.with_alpha(0x26));
        } else {
            cv.fill_clip(pill, hint_fg.with_alpha(0x26), clip);
        }
        let text_w = h.label.chars().count() as f32 * cell_w * crate::inlay::HINT_SCALE;
        labels.push((pill.x + ((pill.w as f32 - text_w) / 2.0).round() as i32, y, h.label.as_str()));
    }
}

fn row_text<'a>(rows: &'a [VRow], lines: &'a [String], r: usize) -> &'a str {
    let w = &rows[r];
    &lines[w.lk][w.b0..w.b1]
}

/// (mirror dir, real project root) pairs: documents of a project served from a narrow mirror
/// are addressed to the language server under the mirror path and mapped back on the way in.
static MIRRORS: std::sync::Mutex<Vec<(PathBuf, PathBuf)>> = std::sync::Mutex::new(Vec::new());

pub fn register_mirror(mirror: PathBuf, root: PathBuf) {
    let mut m = MIRRORS.lock().unwrap();
    m.retain(|(_, r)| *r != root);
    m.push((mirror, root));
}

pub fn unregister_mirror(root: &Path) {
    MIRRORS.lock().unwrap().retain(|(_, r)| r != root);
}

pub fn to_mirror(p: &Path) -> PathBuf {
    for (m, r) in MIRRORS.lock().unwrap().iter() {
        if let Ok(rel) = p.strip_prefix(r) {
            return m.join(rel);
        }
    }
    p.to_path_buf()
}

pub fn from_mirror(p: PathBuf) -> PathBuf {
    for (m, r) in MIRRORS.lock().unwrap().iter() {
        if let Ok(rel) = p.strip_prefix(m) {
            return r.join(rel);
        }
    }
    p
}

pub fn path_to_uri(p: &Path) -> String {
    let mut s = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(from_mirror(PathBuf::from(String::from_utf8(out).ok()?)))
}

/// Colored text pieces of bytes [b0, b1) of one line (a visual row), restricted to visual
/// columns [vc0, vc1) counted from b0, with tabs expanded to spaces.
fn window_pieces(
    line: &str,
    spans: Option<&Spans>,
    b0: usize,
    b1: usize,
    vc0: usize,
    vc1: usize,
    tab_w: usize,
    fg: Rgba,
    out: &mut Vec<(String, Rgba, u8)>,
) {
    let mut vc = 0usize;
    let mut seg = |text: &str, color: Rgba, flags: u8, vc: &mut usize| {
        let mut s = String::new();
        for ch in text.chars() {
            if *vc >= vc1 {
                break;
            }
            if ch == '\t' {
                let n = tab_w - *vc % tab_w;
                for _ in 0..n {
                    if *vc >= vc0 && *vc < vc1 {
                        s.push(' ');
                    }
                    *vc += 1;
                }
            } else {
                if *vc >= vc0 {
                    s.push(ch);
                }
                *vc += 1;
            }
        }
        if !s.is_empty() {
            out.push((s, color, flags));
        }
    };
    // Emit line[x..y] clipped to the row.
    let mut put = |x: usize, y: usize, color: Rgba, flags: u8, vc: &mut usize| {
        let (x, y) = (x.max(b0), y.min(b1));
        if x < y {
            seg(&line[x..y], color, flags, vc);
        }
    };
    match spans {
        Some(sp) => {
            let mut at = 0;
            for &(s, e, c, f) in sp.iter() {
                if s > at {
                    put(at, s, fg, 0, &mut vc);
                }
                put(s, e, c, f, &mut vc);
                at = e;
            }
            if at < line.len() {
                put(at, line.len(), fg, 0, &mut vc);
            }
        }
        None => put(0, line.len(), fg, 0, &mut vc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip() {
        let p = Path::new("/home/a b/ç/x.ts");
        let u = path_to_uri(p);
        assert_eq!(u, "file:///home/a%20b/%C3%A7/x.ts");
        assert_eq!(uri_to_path(&u).unwrap(), p);
    }

    #[test]
    fn replace_text_is_one_undo_step_and_keeps_cursor() {
        let mut d = Doc::new(Buffer::from_str("const  a=1\nlet b = 2\n"), None, None, "//", 8.0);
        d.ed.click(&mut d.buf, 17, false); // inside "let b = 2"
        d.replace_text("const a = 1\nlet b = 2\n");
        assert_eq!(d.buf.to_string(), "const a = 1\nlet b = 2\n");
        // text before the cursor changed by +0 chars net (two edits in the first line): cursor stays on the same token
        assert_eq!(d.buf.line_col(d.ed.head()).0, 1);
        d.ed.undo(&mut d.buf);
        assert_eq!(d.buf.to_string(), "const  a=1\nlet b = 2\n", "single undo restores everything");
        assert!(!d.buf.is_dirty(), "undo returns to the saved version");
    }

    #[test]
    fn lsp_format_edits_apply_in_order_as_one_undo_step() {
        let e = |sl, sc, el, ec, t: &str| LspEdit { sl, sc, el, ec, text: t.into() };
        let mut d = Doc::new(Buffer::from_str("a  b{c\nd\n"), None, None, "//", 8.0);
        d.ed.click(&mut d.buf, 9, false); // end of the text, after the edits
        // Two inserts at the same spot must land in list order ("\n" then "  "), plus a replace.
        d.apply_lsp_edits(&[e(0, 1, 0, 3, " "), e(0, 5, 0, 5, "\n"), e(0, 5, 0, 5, "  "), e(1, 0, 1, 0, "  ")]);
        assert_eq!(d.buf.to_string(), "a b{\n  c\n  d\n");
        assert_eq!(d.buf.line_col(d.ed.head()), (3, 0), "cursor stays at the end of the text");
        d.ed.undo(&mut d.buf);
        assert_eq!(d.buf.to_string(), "a  b{c\nd\n", "one undo step");
    }

    #[test]
    fn lsp_positions_use_utf16() {
        let b = Buffer::from_str("a😀b\nx");
        let d = Doc::new(b, None, None, "//", 8.0);
        // 'b' is char 2 -> utf16 col 3 (the emoji takes two units)
        assert_eq!(d.lsp_pos(2), (0, 3));
        assert_eq!(d.pos_from_lsp(0, 3), 2);
        assert_eq!(d.pos_from_lsp(1, 1), 5);
    }

    #[test]
    fn sticky_keeps_the_two_innermost_headers() {
        assert_eq!(sticky_pick(&[1, 5, 9], STICKY_MAX), vec![5, 9]);
        assert_eq!(sticky_pick(&[4], STICKY_MAX), vec![4]);
        assert!(sticky_pick(&[], STICKY_MAX).is_empty());
    }

    #[test]
    fn sticky_band_snaps_to_the_row_grid_at_rest() {
        let lh = 22;
        // Aligned text, or nothing pinned: exactly n lines.
        assert_eq!(sticky_band_h(2, lh, 0, true), 44);
        assert_eq!(sticky_band_h(0, lh, 7, true), 0);
        // Rows start at k*lh - frac; the band ends on one of those boundaries.
        for frac in 1..lh {
            let h = sticky_band_h(2, lh, frac, true);
            assert!(h >= 44 && h < 44 + lh);
            assert_eq!((h + frac) % lh, 0, "frac {frac}: band end {h} is a row boundary");
        }
        // Fully scrolled to the next row: already aligned.
        assert_eq!(sticky_band_h(2, lh, lh, true), 44);
        // While the text glides the band keeps its size.
        assert_eq!(sticky_band_h(2, lh, 9, false), 44);
    }
}
