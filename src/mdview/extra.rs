//! Layout of the interactive and HTML-derived blocks (details, Mermaid, code-block buttons, image
//! and video cards, HTML paragraphs / tables, definition lists), and the preview's click targets.

use super::*;
use crate::mdimg::ImgState;

/// How long the Copy button says "Copied".
pub(super) const COPIED_MS: u128 = 1600;
const BTN_H: i32 = 26;
const COPY_W: i32 = 76;
const CODE_W: i32 = 70;
const DIAGRAM_W: i32 = 86;

/// What a click on a preview button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Act {
    /// Open / close the `<details>` with this id.
    Toggle(u64),
    /// Copy the code block with this key.
    Copy(u64),
    /// Switch the Mermaid block with this key between diagram and source.
    Code(u64),
}

/// A button's hit box (x absolute, y relative to its segment).
#[derive(Clone, Copy, Debug)]
pub(super) struct Hit {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) w: i32,
    pub(super) h: i32,
    pub(super) act: Act,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Btn {
    Copy(u64),
    ShowCode,
    ShowDiagram,
}

/// State of the interactive blocks, kept while the preview is open.
#[derive(Default)]
pub(super) struct Ext {
    /// `<details>` clicked into the state opposite to their default.
    pub(super) toggled: HashSet<u64>,
    /// Mermaid blocks shown as source.
    pub(super) code: HashSet<u64>,
    /// Diagram layouts per Mermaid source (or why it could not be drawn).
    pub(super) dias: KeyMap<Result<Rc<mermaid::Diagram>, String>>,
}

/// Size index a run is measured / drawn at.
pub(super) fn run_si(flags: Fl, si: usize) -> usize {
    if flags & (F_SUP | F_SUB) != 0 {
        SUP
    } else if flags & F_KBD != 0 {
        SMALL
    } else {
        si
    }
}

/// Size index of a diagram font size (diagrams use 16, 13 and 11 px).
fn size_index(size: f32) -> usize {
    if size >= 15.0 {
        BODY
    } else if size >= 12.0 {
        SMALL
    } else {
        TINY
    }
}

/// Key of a Mermaid block (its source).
pub(super) fn mermaid_key(text: &str) -> u64 {
    fxhash(text.as_bytes(), 0x6d65_726d)
}

/// Key of a code block (language + text), as used by the highlight cache and the Copy button.
pub(super) fn code_key(lang: &str, text: &str) -> u64 {
    fxhash(text.as_bytes(), fxhash(lang.as_bytes(), 3))
}

/// Visit `bl` and every block nested in it; stops (returning true) when `f` does.
pub(super) fn visit(bl: &[Block], f: &mut dyn FnMut(&Block) -> bool) -> bool {
    for b in bl {
        if f(b) {
            return true;
        }
        let stop = match &b.kind {
            Kind::Quote(v) | Kind::Alert(_, v) => visit(v, &mut *f),
            Kind::List { items, .. } => items.iter().any(|i| visit(&i.blocks, &mut *f)),
            Kind::FootDef { blocks, .. } | Kind::Details { blocks, .. } => visit(blocks, &mut *f),
            Kind::DefList(items) => items.iter().any(|it| it.defs.iter().any(|d| visit(d, &mut *f))),
            _ => false,
        };
        if stop {
            return true;
        }
    }
    false
}

impl Lay<'_> {
    /// Source line range -> y range anchor without a block at hand.
    pub(super) fn anchor_lines(&mut self, l0: usize, l1: usize, y0: i32, y1: i32) {
        let (mut l0, mut l1) = (l0 as i64, l1 as i64);
        if let Some(p) = self.anchors.last() {
            l0 = l0.max(p.l0);
            l1 = l1.max(p.l1);
        }
        self.anchors.push(Anchor { l0, l1: l1.max(l0), y0, y1 });
    }

    fn button(&mut self, area: Rect, r: Rect, kind: Btn, act: Act, key: u64) {
        self.op(r.y, r.bottom(), Op::Button { r, area, kind });
        self.hits.push(Hit { x: r.x, y: r.y, w: r.w, h: r.h, act });
        if !self.areas.iter().any(|(a, _)| *a == area) {
            self.areas.push((area, key));
        }
    }

    /// The Copy button in the top-right corner of a code block (shown on hover).
    pub(super) fn copy_button(&mut self, block: Rect, key: u64) {
        if block.w < COPY_W + 40 || block.h < BTN_H + 8 {
            return;
        }
        let r = Rect::new(block.right() - COPY_W - 6, block.y + 6, COPY_W, BTN_H);
        self.button(block, r, Btn::Copy(key), Act::Copy(key), key);
    }

    /// A plain code block (no Copy / highlight differences from `Kind::Code`): its rect.
    fn code_box(&mut self, lang: &str, text: &str, x: i32, w: i32, y: i32) -> Rect {
        let lh = code_lh();
        let lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').replace('\t', "    ")).collect();
        let top = CODE_PAD_Y;
        let h = top + lines.len() as i32 * lh + CODE_PAD_Y;
        let r = Rect::new(x, y, w, h);
        let key = code_key(lang, text);
        self.op(y, y + h, Op::Code { r, lang: lang.to_string(), lines, lh, top, key, math: false, err: None });
        self.copy_button(r, key);
        r
    }

    /// A Mermaid block: the diagram in a card (with a Code button on hover), its source when asked
    /// for, or the source with a note when the diagram type is not supported.
    pub(super) fn mermaid(&mut self, b: &Block, text: &str, x: i32, w: i32, y: i32) -> i32 {
        let key = mermaid_key(text);
        let res = match self.ext.dias.get(&key) {
            Some(r) => r.clone(),
            None => {
                let (m, tx) = (&mut *self.m, &mut *self.tx);
                let mut meas = |s: &str, bold: bool, size: f32| m.w(tx, s, bold, size_index(size));
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mermaid::build(text, &mut meas)))
                    .unwrap_or_else(|_| Err("this diagram could not be drawn".into()))
                    .map(Rc::new);
                if self.ext.dias.len() > 64 {
                    self.ext.dias.clear();
                }
                self.ext.dias.insert(key, r.clone());
                r
            }
        };
        let show_code = self.ext.code.contains(&key);
        match res {
            Ok(d) if !show_code => {
                let pad = 16;
                let avail = (w - 2 * pad).max(40) as f32;
                let k = (avail / d.w.max(1.0)).min(1.0);
                let (dw, dh) = (((d.w * k).round() as i32).max(1), ((d.h * k).round() as i32).max(1));
                let h = dh + 2 * pad + if d.w * k < avail - DIAGRAM_W as f32 { 0 } else { BTN_H };
                let card = Rect::new(x, y, w, h);
                self.op(y, y + h, Op::Card { r: card, rad: 8, fill: Col::Bg, border: Col::Border });
                let r = Rect::new(x + (w - dw) / 2, y + h - pad - dh, dw, dh);
                self.op(r.y, r.bottom(), Op::Diagram { r, key, dia: d });
                let br = Rect::new(card.right() - CODE_W - 6, y + 6, CODE_W, BTN_H);
                self.button(card, br, Btn::ShowCode, Act::Code(key), key);
                self.anchor(b, y, y + h);
                y + h
            }
            Ok(_) => {
                let r = self.code_box("mermaid", text, x, w, y);
                if r.w >= COPY_W + DIAGRAM_W + 60 {
                    let br = Rect::new(r.right() - COPY_W - 6 - DIAGRAM_W - 6, y + 6, DIAGRAM_W, BTN_H);
                    self.button(r, br, Btn::ShowDiagram, Act::Code(key), key);
                }
                self.anchor(b, y, r.bottom());
                r.bottom()
            }
            Err(msg) => {
                let r = self.code_box("mermaid", text, x, w, y);
                let note = format!("Mermaid: {msg}");
                let (note, nw) = self.fit(&note, TINY, false, w);
                let ny = r.bottom() + 4;
                self.op(
                    ny,
                    ny + LH[TINY],
                    Op::Label { x, w: nw, text: note, col: Col::Dim, si: TINY as u8, lh: LH[TINY], bold: false },
                );
                self.anchor(b, y, ny + LH[TINY]);
                ny + LH[TINY]
            }
        }
    }

    /// `<details>`: a bordered card whose summary row toggles the content below it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn details(
        &mut self,
        b: &Block,
        (id, open0): (u64, bool),
        summary: &[Inl],
        blocks: &[Block],
        x: i32,
        w: i32,
        y: i32,
        st: St,
    ) -> i32 {
        let open = open0 != self.ext.toggled.contains(&id);
        let card = self.ops.len();
        self.op(y, y, Op::Card { r: Rect::new(x, y, w, 0), rad: 8, fill: Col::Bg, border: Col::Border });
        let pad = 10;
        let ty = y + pad;
        let fallback = [Inl::Text { text: "Details".into(), flags: 0, link: 0 }];
        let summary = if blank(summary) { &fallback[..] } else { summary };
        let lines = self.flow(summary, w - 36 - 12, BODY, false, true);
        let th = self.emit(lines, x + 36, ty, BODY, 0).max(LH[BODY]);
        let head_h = th + 2 * pad;
        self.op(y, y + head_h, Op::Hover { r: Rect::new(x + 1, y + 1, w - 2, head_h - 1), rad: 7 });
        let name = if open { "ui/chevron_down" } else { "ui/chevron_right" };
        self.op(ty, ty + LH[BODY], Op::Icon { x: x + 13, y: ty + (LH[BODY] - 14) / 2, size: 14, name, col: Col::Dim });
        self.hits.push(Hit { x, y, w, h: head_h, act: Act::Toggle(id) });
        let mut y1 = y + head_h;
        if open {
            self.anchor_lines(b.line, b.line, y, y1);
            self.fill(Rect::new(x + 1, y1, w - 2, 1), Col::Border, 0);
            if !blocks.is_empty() {
                y1 = self.blocks(blocks, x + 18, w - 36, y1 + 14, st) + 14;
            }
        } else {
            self.anchor(b, y, y1);
        }
        if let Some(o) = self.ops.get_mut(card) {
            o.y1 = y1;
            if let Op::Card { r, .. } = &mut o.op {
                r.h = y1 - y;
            }
        }
        y1
    }

    /// Text of an aligned HTML block (`<p align="center">`, `<h1 align="center">`).
    pub(super) fn html_para(&mut self, b: &Block, (align, head): (u8, u8), inl: &[Inl], x: i32, w: i32, y: i32) -> i32 {
        let heading = (1..=6).contains(&head);
        let si = if heading { (head - 1) as usize } else { BODY };
        let lines = self.flow(inl, w, si, heading, true);
        if heading {
            self.heads.push((slugify(&plain(inl)), y, false));
        }
        let mut ly = y;
        for line in lines {
            let lw = line.last().map_or(0.0, |r| r.x + r.w).round() as i32;
            let ox = match align {
                1 => ((w - lw) / 2).max(0),
                2 => (w - lw).max(0),
                _ => 0,
            };
            ly += self.emit(vec![line], x + ox, ly, si, if heading { F_B } else { 0 });
        }
        let y1 = ly.max(y + LH[si]);
        self.anchor(b, y, y1);
        y1
    }

    /// Raw HTML `<table>`: like a GFM table, with cells spanning columns and per-cell alignment.
    pub(super) fn html_table(&mut self, b: &Block, rows: &[Vec<HCell>], x: i32, w: i32, y: i32) -> i32 {
        let ncols = rows.iter().map(|r| r.iter().map(|c| c.span as usize).sum::<usize>()).max().unwrap_or(0);
        if ncols == 0 {
            return y;
        }
        let cap = (self.m.w(self.tx, "0", false, CELL) * CELL_MAX_CH).ceil() as i32;
        let mut nat = vec![36; ncols];
        let mut wide: Vec<(usize, usize, i32)> = vec![];
        for row in rows {
            let mut c = 0;
            for cell in row {
                let one = self.flow(&cell.inl, i32::MAX / 4, CELL, cell.head, false);
                let wd = one.iter().map(|l| l.last().map_or(0.0, |r| r.x + r.w)).fold(0.0f32, f32::max);
                let n = (wd.ceil() as i32).min(cap) + 2 * CELL_PAD_X;
                let sp = cell.span as usize;
                if sp == 1 {
                    if let Some(v) = nat.get_mut(c) {
                        *v = (*v).max(n);
                    }
                } else {
                    wide.push((c, sp, n));
                }
                c += sp;
            }
        }
        for (c, sp, n) in wide {
            let end = (c + sp).min(ncols);
            if c >= end {
                continue;
            }
            let have: i32 = nat[c..end].iter().sum();
            if n > have {
                let extra = (n - have) / (end - c) as i32 + 1;
                for v in &mut nat[c..end] {
                    *v += extra;
                }
            }
        }
        let colw = fit_columns(&nat, w);
        let tw: i32 = colw.iter().sum();
        let lh = LH[CELL];
        type CellLay = (i32, i32, Vec<Vec<Run>>, u8, bool);
        let mut grid: Vec<Vec<CellLay>> = Vec::with_capacity(rows.len());
        let mut row_h: Vec<i32> = Vec::with_capacity(rows.len());
        for row in rows {
            let (mut c, mut cx, mut n) = (0usize, x, 1usize);
            let mut cells = vec![];
            for cell in row {
                let start = c.min(ncols);
                let end = (c + cell.span as usize).min(ncols);
                let cw: i32 = colw[start..end].iter().sum();
                let lines = self.flow(&cell.inl, (cw - 2 * CELL_PAD_X).max(10), CELL, cell.head, true);
                n = n.max(lines.len());
                cells.push((cx, cw, lines, cell.align, cell.head));
                cx += cw;
                c = end;
            }
            row_h.push(n as i32 * lh + 2 * CELL_PAD_Y);
            grid.push(cells);
        }
        let h: i32 = row_h.iter().sum();
        self.op(y, y + h, Op::Card { r: Rect::new(x, y, tw, h), rad: TABLE_RAD, fill: Col::Bg, border: Col::Border });
        let mut ry = y;
        for (ri, cells) in grid.iter().enumerate() {
            let rh = row_h[ri];
            if !cells.is_empty() && cells.iter().all(|c| c.4) {
                if ri == 0 {
                    let top = Rect::new(x + 1, y + 1, tw - 2, rh - 1);
                    self.op(y, top.bottom(), Op::Fill { r: top, col: Col::Head, rad: TABLE_RAD - 1 });
                    let low = Rect::new(x + 1, y + rh - TABLE_RAD, tw - 2, TABLE_RAD);
                    self.op(y, low.bottom(), Op::Fill { r: low, col: Col::Head, rad: 0 });
                } else {
                    self.fill(Rect::new(x + 1, ry, tw - 2, rh), Col::Head, 0);
                }
            }
            ry += rh;
        }
        let mut ry = y;
        for (ri, cells) in grid.into_iter().enumerate() {
            let rh = row_h[ri];
            if ri > 0 {
                self.fill(Rect::new(x, ry - 1, tw, 1), Col::Border, 0);
            }
            for (cx, cw, lines, align, head) in cells {
                if cx > x {
                    self.fill(Rect::new(cx, ry, 1, rh), Col::Border, 0);
                }
                let avail = cw - 2 * CELL_PAD_X;
                for (li, line) in lines.into_iter().enumerate() {
                    let lw = line.last().map_or(0.0, |r| r.x + r.w).round() as i32;
                    let ox = match align {
                        1 => ((avail - lw) / 2).max(0),
                        2 => (avail - lw).max(0),
                        _ => 0,
                    };
                    let ty = ry + CELL_PAD_Y + li as i32 * lh;
                    self.emit(vec![line], cx + CELL_PAD_X + ox, ty, CELL, if head { F_B } else { 0 });
                }
            }
            ry += rh;
        }
        self.anchor(b, y, y + h);
        y + h
    }

    /// `Term` / `: definition`: the term in semibold, definitions indented under it.
    pub(super) fn def_list(&mut self, _b: &Block, items: &[DefItem], x: i32, w: i32, y: i32, st: St) -> i32 {
        let mut y = y;
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                y += 10;
            }
            let lines = self.flow(&it.title, w, BODY, true, true);
            y += self.emit(lines, x, y, BODY, F_B).max(LH[BODY]);
            for d in &it.defs {
                y = self.blocks(d, x + 24, w - 24, y + 2, St { tight: Some(4), ..st });
            }
        }
        y
    }

    /// Images in rows; a file that can't be shown (missing, still downloading, remote images off)
    /// keeps a card with its alt text and why, and a video is a card that opens it externally.
    pub(super) fn images(&mut self, b: &Block, imgs: &[Img], x: i32, w: i32, y: i32) -> i32 {
        const H: i32 = 54;
        const GAP: i32 = 8;
        let (mut cx, mut cy, mut row_h) = (0, y, 0);
        for im in imgs {
            if !im.video {
                let real = self.imgs.resolve(&im.url).and_then(|id| self.imgs.size(id).map(|s| (id, s)));
                if let Some((id, (nw, nh))) = real {
                    let (nw, nh) = (nw as i32, nh as i32);
                    let (mut dw, mut dh) = match im.size {
                        (Some(a), Some(b)) => (a, b),
                        (Some(a), None) => (a, (nh as f32 * a as f32 / nw as f32).round() as i32),
                        (None, Some(b)) => ((nw as f32 * b as f32 / nh as f32).round() as i32, b),
                        _ => (nw, nh),
                    };
                    if dw > w {
                        dh = (dh as f32 * w as f32 / dw as f32).round() as i32;
                        dw = w;
                    }
                    if dh > 720 {
                        dw = (dw as f32 * 720.0 / dh as f32).round() as i32;
                        dh = 720;
                    }
                    let (dw, dh) = (dw.max(1), dh.max(1));
                    if cx > 0 && cx + dw > w {
                        cx = 0;
                        cy += row_h + GAP;
                        row_h = 0;
                    }
                    let r = Rect::new(x + cx, cy, dw, dh);
                    self.op(cy, cy + dh, Op::Image { r, id });
                    if im.link != 0 {
                        self.links.push(LinkHit { x: r.x, y: cy, w: dw, h: dh, link: im.link });
                    }
                    cx += dw + GAP;
                    row_h = row_h.max(dh);
                    continue;
                }
            }
            let name = im.url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
            let title = if !im.alt.trim().is_empty() {
                im.alt.trim().to_string()
            } else if !name.is_empty() {
                crate::mdimg::percent_decode(&name)
            } else if im.video {
                "video".to_string()
            } else {
                "image".to_string()
            };
            let (icon, status, link) = if im.video {
                let target = if crate::mdimg::is_remote(&im.url) {
                    Some(im.url.trim().to_string())
                } else {
                    self.imgs.local_path(&im.url).map(|p| format!("file://{}", p.display()))
                };
                let link = if im.link != 0 { im.link } else { target.map_or(0, |t| self.urls.id(&t)) };
                ("ui/play_filled", "Video \u{b7} open externally".to_string(), link)
            } else {
                let why = match self.imgs.state_of(&im.url) {
                    ImgState::Missing => "Image not found",
                    ImgState::Loading => "Loading\u{2026}",
                    ImgState::Failed => "Couldn't load the image",
                    ImgState::Off => "Remote images are off",
                    ImgState::Bad => "Unsupported image",
                };
                ("ui/image", format!("{why} \u{b7} {}", im.url.trim()), im.link)
            };
            let tw0 = self.m.w(self.tx, &title, false, SMALL).ceil() as i32;
            let sw0 = self.m.w(self.tx, &status, false, TINY).ceil() as i32;
            let bw = (tw0.max(sw0) + 66).clamp(200, w.max(60));
            if cx > 0 && cx + bw > w {
                cx = 0;
                cy += row_h + GAP;
                row_h = 0;
            }
            let r = Rect::new(x + cx, cy, bw, H);
            self.op(cy, cy + H, Op::Card { r, rad: 8, fill: Col::Code, border: Col::Border });
            let tile = Rect::new(r.x + 11, cy + 11, 32, 32);
            self.fill(tile, Col::AccentSoft, 8);
            self.op(
                tile.y,
                tile.bottom(),
                Op::Icon { x: tile.x + 8, y: tile.y + 8, size: 16, name: icon, col: Col::Accent },
            );
            let avail = bw - 54 - 12;
            let (t, tw) = self.fit(&title, SMALL, false, avail);
            let (s, sw) = self.fit(&status, TINY, false, avail);
            self.op(
                cy + 8,
                cy + 8 + LH[SMALL],
                Op::Label { x: r.x + 54, w: tw, text: t, col: Col::Fg, si: SMALL as u8, lh: LH[SMALL], bold: false },
            );
            let scol = if im.video { Col::Accent } else { Col::Dim };
            self.op(
                cy + 28,
                cy + 28 + LH[TINY],
                Op::Label { x: r.x + 54, w: sw, text: s, col: scol, si: TINY as u8, lh: LH[TINY], bold: false },
            );
            if link != 0 {
                self.links.push(LinkHit { x: r.x, y: cy, w: bw, h: H, link });
                self.op(cy, cy + H, Op::Hover { r: r.inset(1), rad: 7 });
            }
            cx += bw + GAP;
            row_h = row_h.max(H);
        }
        let y1 = cy + row_h;
        self.anchor(b, y, y1);
        y1
    }
}

impl MdView {
    /// Whether `http(s)` images are fetched (the `markdown_remote_images` setting).
    pub fn set_remote_images(&mut self, on: bool) {
        if self.imgs.set_remote(on) {
            self.base_gen += 1;
            self.lay = None;
        }
    }

    /// Where the pointer is (canvas coordinates), `None` when it left the preview. Drives the hover
    /// buttons and the link highlight on the next draw.
    pub fn set_hover(&mut self, p: Option<(f64, f64)>) {
        self.hover = p;
    }

    /// The segment under a canvas point and the point in its coordinates.
    fn seg_point(&self, x: f64, y: f64, rect: Rect, scroll_px: f64) -> Option<(&SegLay, f64, f64)> {
        if !rect.contains(x, y) {
            return None;
        }
        let lay = self.lay.as_ref()?;
        let cx = x - rect.x as f64;
        let cy = y - rect.y as f64 + scroll_px.floor();
        let (sl, base) = self.seg_at_y(lay, cy as i32)?;
        Some((sl, cx, cy - base as f64))
    }

    /// The button / toggle under the pointer.
    pub fn hit_at(&self, x: f64, y: f64, rect: Rect, scroll_px: f64) -> Option<Act> {
        let (sl, cx, cy) = self.seg_point(x, y, rect, scroll_px)?;
        sl.hits.iter().find(|h| Rect::new(h.x, h.y, h.w, h.h).contains(cx, cy)).map(|h| h.act)
    }

    /// Anything that reacts to a click is under the pointer (link, checkbox, button, toggle).
    pub fn clickable_at(&self, x: f64, y: f64, rect: Rect, scroll_px: f64) -> bool {
        self.hit_at(x, y, rect, scroll_px).is_some()
            || self.link_at(x, y, rect, scroll_px).is_some()
            || self.check_at(x, y, rect, scroll_px).is_some()
    }

    /// A value that changes when what the pointer is over changes (link, button, block with hover
    /// buttons): the caller redraws only then.
    pub fn hover_sig(&self, x: f64, y: f64, rect: Rect, scroll_px: f64) -> u64 {
        let Some((sl, cx, cy)) = self.seg_point(x, y, rect, scroll_px) else { return 0 };
        let mut h = 1u64;
        if let Some(u) = self.link_at(x, y, rect, scroll_px) {
            h = fxhash(u.as_bytes(), h);
        }
        if let Some(a) = self.hit_at(x, y, rect, scroll_px) {
            h = fxhash(format!("{a:?}").as_bytes(), h);
        }
        for (a, k) in &sl.areas {
            if a.contains(cx, cy) {
                h ^= k.rotate_left(17);
            }
        }
        h
    }

    /// Lay out again the segments whose blocks match `f` (a toggle changed how they look).
    fn relayout_where(&mut self, f: &mut dyn FnMut(&Block) -> bool) {
        for sd in self.cache.values_mut() {
            if visit(&sd.blocks, &mut *f) {
                sd.lay = None;
            }
        }
        self.lay = None;
    }

    /// Open / close a `<details>` block.
    pub fn toggle_details(&mut self, id: u64) {
        if !self.ext.toggled.remove(&id) {
            self.ext.toggled.insert(id);
        }
        self.relayout_where(&mut |b| matches!(&b.kind, Kind::Details { id: i, .. } if *i == id));
    }

    /// Show a Mermaid block as its source, or back as a diagram.
    pub fn toggle_code(&mut self, key: u64) {
        if !self.ext.code.remove(&key) {
            self.ext.code.insert(key);
        }
        self.relayout_where(
            &mut |b| matches!(&b.kind, Kind::Code { lang, text } if lang.eq_ignore_ascii_case("mermaid") && mermaid_key(text) == key),
        );
    }

    /// The source of the code block with `key` (for its Copy button).
    pub fn code_text(&self, key: u64) -> Option<String> {
        let mut found = None;
        for p in &self.order {
            let Some(sd) = self.cache.get(&p.key) else { continue };
            visit(&sd.blocks, &mut |b| {
                if let Kind::Code { lang, text } = &b.kind
                    && code_key(lang, text) == key
                {
                    found = Some(text.clone());
                    return true;
                }
                false
            });
            if found.is_some() {
                break;
            }
        }
        found
    }

    /// The Copy button of code block `key` was used: it says so for a moment.
    pub fn mark_copied(&mut self, key: u64) {
        self.copied = Some((key, std::time::Instant::now()));
    }

    /// When a transient state (the "Copied" label) needs a redraw to go away.
    pub fn ui_due(&self) -> Option<std::time::Instant> {
        let (_, t) = self.copied?;
        let due = t + std::time::Duration::from_millis(COPIED_MS as u64);
        (due > std::time::Instant::now()).then_some(due)
    }
}
