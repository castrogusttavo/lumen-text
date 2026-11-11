//! Mouse selection in the preview: positions are (segment, op in reading order, line, char) in
//! the laid-out text, so a selection can run through paragraphs, lists, tables and code blocks.
//! The copied text is what the preview shows (code blocks give their lines verbatim).

use super::*;

/// The footnote section's segment id (after every real segment).
pub(super) const FOOT: usize = usize::MAX;

/// A point in the laid-out text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelPos {
    /// Index into `MdView::order` ([`FOOT`] for the footnotes).
    pub(super) seg: usize,
    /// Rank of the op in the segment's reading order (see [`reading`]).
    pub(super) op: usize,
    /// Line inside a code block (0 for text runs).
    pub(super) line: u32,
    /// Char index inside the run / code line.
    pub(super) ch: u32,
}

/// Which selectable piece is being painted.
#[derive(Clone, Copy)]
pub(super) struct SelAt {
    pub(super) seg: usize,
    pub(super) rank: usize,
    pub(super) line: usize,
}

fn selectable(op: &Op) -> bool {
    matches!(op, Op::Text { .. } | Op::Code { .. })
}

/// Selectable ops of a segment in reading order: top to bottom, then left to right (ops are stored
/// by y only, so a list number laid out after its item text still reads first).
pub(super) fn reading(sl: &SegLay) -> Vec<usize> {
    let mut v: Vec<usize> = (0..sl.ops.len()).filter(|&i| selectable(&sl.ops[i].op)).collect();
    let x_of = |o: &Op| match o {
        Op::Text { x, .. } => *x,
        Op::Code { r, .. } => r.x,
        _ => 0,
    };
    v.sort_by_key(|&i| (sl.ops[i].y0, x_of(&sl.ops[i].op)));
    v
}

/// Reading rank of every op (`u32::MAX` for ops that can't be selected).
pub(super) fn ranks_of(sl: &SegLay) -> Vec<u32> {
    let mut inv = vec![u32::MAX; sl.ops.len()];
    for (r, &o) in reading(sl).iter().enumerate() {
        inv[o] = r as u32;
    }
    inv
}

/// (size, bold, left padding, monospace) a text run is drawn with.
fn run_style(flags: Fl, si: u8) -> (f32, bool, f32, bool) {
    if flags & F_CODE != 0 {
        (0.0, false, CHIP_PAD as f32, true)
    } else if flags & F_KBD != 0 {
        (SIZES[SMALL], false, KBD_PAD as f32, false)
    } else {
        (SIZES[run_si(flags, si as usize)], flags & F_B != 0, 0.0, false)
    }
}

/// X offsets (from the run's left edge) of every char boundary of a text run.
pub(super) fn char_xs(tx: &mut TextCtx, text: &str, flags: Fl, si: u8) -> Vec<f32> {
    let (size, bold, pad, mono) = run_style(flags, si);
    if mono {
        let mut v = Vec::with_capacity(text.len() + 1);
        let mut acc = pad;
        v.push(acc);
        for c in text.chars() {
            acc += cell_count(c) as f32 * tx.cell_w;
            v.push(acc);
        }
        v
    } else {
        tx.ui_char_xs(text, bold, Some(size)).into_iter().map(|x| x + pad).collect()
    }
}

/// X offsets of the char boundaries of a code line (monospace cells).
fn code_xs(tx: &TextCtx, line: &str) -> Vec<f32> {
    let mut v = Vec::with_capacity(line.len() + 1);
    let mut acc = 0.0;
    v.push(acc);
    for c in line.chars() {
        acc += cell_count(c) as f32 * tx.cell_w;
        v.push(acc);
    }
    v
}

/// Index of the boundary nearest to `x`.
fn nearest(xs: &[f32], x: f32) -> usize {
    let mut best = (0, f32::MAX);
    for (i, bx) in xs.iter().enumerate() {
        let d = (bx - x).abs();
        if d < best.1 {
            best = (i, d);
        }
    }
    best.0
}

fn char_slice(s: &str, c0: usize, c1: usize) -> &str {
    let b0 = s.char_indices().nth(c0).map_or(s.len(), |(i, _)| i);
    let b1 = s.char_indices().nth(c1).map_or(s.len(), |(i, _)| i);
    &s[b0..b1.max(b0)]
}

/// Selected char range `[c0, c1)` of the piece at (`seg`, `rank`, `line`) that has `n` chars.
fn piece_range(sel: (SelPos, SelPos), seg: usize, rank: usize, line: u32, n: usize) -> Option<(usize, usize)> {
    let (lo, hi) = sel;
    let here = (seg, rank, line);
    if here < (lo.seg, lo.op, lo.line) || here > (hi.seg, hi.op, hi.line) {
        return None;
    }
    let c0 = if here == (lo.seg, lo.op, lo.line) { (lo.ch as usize).min(n) } else { 0 };
    let c1 = if here == (hi.seg, hi.op, hi.line) { (hi.ch as usize).min(n) } else { n };
    (c1 > c0).then_some((c0, c1))
}

/// Paint the selection behind a text run at `r` (x, y, width and line height of the run).
#[allow(clippy::too_many_arguments)]
pub(super) fn sel_text_bg(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    sel: (SelPos, SelPos),
    at: SelAt,
    r: Rect,
    text: &str,
    flags: Fl,
    si: u8,
    col: Rgba,
    clip: Rect,
) {
    let n = text.chars().count();
    let Some((c0, c1)) = piece_range(sel, at.seg, at.rank, 0, n) else { return };
    let (x0, x1) = if c0 == 0 && c1 == n {
        (0.0, r.w as f32)
    } else {
        let xs = char_xs(tx, text, flags, si);
        (xs.get(c0).copied().unwrap_or(0.0), xs.get(c1).copied().unwrap_or(r.w as f32))
    };
    let h = (r.h - 4).max(4);
    let rr = Rect::new(r.x + x0.round() as i32, r.y + (r.h - h) / 2, ((x1 - x0).round() as i32).max(1), h);
    cv.fill_clip(rr, col, clip);
}

/// Paint the selection behind code line `at.line` drawn at `r` (x, y and line height).
#[allow(clippy::too_many_arguments)]
pub(super) fn sel_code_bg(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    sel: (SelPos, SelPos),
    at: SelAt,
    r: Rect,
    line: &str,
    col: Rgba,
    clip: Rect,
) {
    let n = line.chars().count();
    let Some((c0, c1)) = piece_range(sel, at.seg, at.rank, at.line as u32, n) else {
        // An empty line in the middle of a selection still shows as selected.
        let (lo, hi) = sel;
        let here = (at.seg, at.rank, at.line as u32);
        if n == 0 && here > (lo.seg, lo.op, lo.line) && here < (hi.seg, hi.op, hi.line) {
            cv.fill_clip(Rect::new(r.x, r.y, (tx.cell_w * 0.6).ceil() as i32, r.h), col, clip);
        }
        return;
    };
    let xs = code_xs(tx, line);
    let (x0, x1) = (xs.get(c0).copied().unwrap_or(0.0), xs.get(c1).copied().unwrap_or(0.0));
    cv.fill_clip(Rect::new(r.x + x0.round() as i32, r.y, ((x1 - x0).round() as i32).max(1), r.h), col, clip);
}

/// The position in segment `sl` (id `sid`) nearest to (`cx`, `yl`), `yl` relative to the segment.
fn pos_in_seg(tx: &mut TextCtx, sid: usize, sl: &SegLay, cx: f64, yl: f64) -> Option<SelPos> {
    let rd = reading(sl);
    if rd.is_empty() {
        return None;
    }
    let at = |rank: usize, line: u32, ch: usize| SelPos { seg: sid, op: rank, line, ch: ch as u32 };
    // A code block under the point.
    for (rank, &oi) in rd.iter().enumerate() {
        if let Op::Code { r, lines, lh, top, .. } = &sl.ops[oi].op
            && yl >= r.y as f64
            && yl < r.bottom() as f64
        {
            let li = (((yl - (r.y + top) as f64) / (*lh).max(1) as f64).floor().max(0.0) as usize)
                .min(lines.len().saturating_sub(1));
            let line = lines.get(li).map_or("", |s| s.as_str());
            let xs = code_xs(tx, line);
            let ch = nearest(&xs, (cx - (r.x + CODE_PAD_X) as f64) as f32);
            return Some(at(rank, li as u32, ch));
        }
    }
    // Text runs on the line under the point: the one under it, else the nearest.
    let mut best: Option<(usize, f64)> = None;
    for (rank, &oi) in rd.iter().enumerate() {
        let d = &sl.ops[oi];
        if let Op::Text { x, w, .. } = &d.op
            && yl >= d.y0 as f64
            && yl < d.y1 as f64
        {
            let dist = if cx < *x as f64 {
                *x as f64 - cx
            } else if cx > (x + w) as f64 {
                cx - (x + w) as f64
            } else {
                0.0
            };
            if best.is_none_or(|(_, bd)| dist < bd) {
                best = Some((rank, dist));
            }
        }
    }
    if let Some((rank, _)) = best
        && let Op::Text { x, text, flags, si, .. } = &sl.ops[rd[rank]].op
    {
        let xs = char_xs(tx, text, *flags, *si);
        return Some(at(rank, 0, nearest(&xs, (cx - *x as f64) as f32)));
    }
    // Between lines: the start of the next piece below, else the end of the last one.
    if let Some(rank) = rd.iter().position(|&oi| sl.ops[oi].y0 as f64 > yl) {
        return Some(at(rank, 0, 0));
    }
    let rank = rd.len() - 1;
    Some(match &sl.ops[rd[rank]].op {
        Op::Code { lines, .. } => {
            let li = lines.len().saturating_sub(1);
            at(rank, li as u32, lines.get(li).map_or(0, |l| l.chars().count()))
        }
        Op::Text { text, .. } => at(rank, 0, text.chars().count()),
        _ => at(rank, 0, 0),
    })
}

impl MdView {
    /// Segments that take space, in document order: (id, layout, top).
    fn seg_list(&self) -> Vec<(usize, &SegLay, i32)> {
        let Some(lay) = self.lay.as_ref() else { return vec![] };
        let mut v: Vec<(usize, &SegLay, i32)> =
            lay.segs.iter().filter_map(|s| self.seg_lay_of(s).map(|l| (s.ix, l, s.y0))).collect();
        if let Some((l, p)) = self.foot.as_ref() {
            v.push((FOOT, l, p.y0));
        }
        v
    }

    /// Text position under a canvas point (the nearest one when it is between blocks).
    pub(super) fn sel_pos_at(&self, tx: &mut TextCtx, x: f64, y: f64, rect: Rect, scroll_px: f64) -> Option<SelPos> {
        let cx = x - rect.x as f64;
        let cy = y - rect.y as f64 + scroll_px.floor();
        let segs = self.seg_list();
        let i = segs.iter().position(|(_, l, top)| cy < (*top + l.height) as f64);
        let (i, yl_of) = match i {
            Some(i) => (i, None),
            None => (segs.len().checked_sub(1)?, Some(f64::MAX)),
        };
        // From segment i on, the first one that has text (blank / image-only segments don't).
        for &(sid, sl, top) in &segs[i..] {
            let yl = yl_of.unwrap_or(cy - top as f64);
            if let Some(p) = pos_in_seg(tx, sid, sl, cx, yl) {
                return Some(p);
            }
        }
        for &(sid, sl, _) in segs[..i].iter().rev() {
            if let Some(p) = pos_in_seg(tx, sid, sl, cx, f64::MAX) {
                return Some(p);
            }
        }
        None
    }

    /// Start a selection at a canvas point (a click on text).
    pub fn sel_begin(&mut self, tx: &mut TextCtx, x: f64, y: f64, rect: Rect, scroll_px: f64) {
        self.sel = self.sel_pos_at(tx, x, y, rect, scroll_px).map(|p| (p, p));
    }

    /// Extend the selection to a canvas point (dragging).
    pub fn sel_extend(&mut self, tx: &mut TextCtx, x: f64, y: f64, rect: Rect, scroll_px: f64) {
        let Some((a, _)) = self.sel else { return };
        if let Some(p) = self.sel_pos_at(tx, x, y, rect, scroll_px) {
            self.sel = Some((a, p));
        }
    }

    /// Select everything.
    pub fn select_all(&mut self) {
        let lo = SelPos { seg: 0, op: 0, line: 0, ch: 0 };
        let hi = SelPos { seg: FOOT, op: usize::MAX, line: u32::MAX, ch: u32::MAX };
        self.sel = Some((lo, hi));
    }

    pub fn clear_selection(&mut self) {
        self.sel = None;
    }

    /// Something (not just a caret) is selected.
    pub fn has_selection(&self) -> bool {
        self.sel.is_some_and(|(a, b)| a != b)
    }

    /// The selected text: runs on one line joined as shown, wrapped lines joined by a space,
    /// lines of a list / table / code block by newlines, blocks by a blank line.
    pub fn selected_text(&self) -> String {
        let Some((a, b)) = self.sel else { return String::new() };
        let sel = if a <= b { (a, b) } else { (b, a) };
        let (lo, hi) = sel;
        let mut out = String::new();
        // (top, bottom, right edge) of the last piece copied, in document coordinates.
        let mut prev: Option<(i32, i32, i32)> = None;
        let sep = |out: &mut String, prev: Option<(i32, i32, i32)>, y0: i32, x: i32| {
            let Some((py0, py1, px1)) = prev else { return };
            if y0 >= py1 {
                if y0 > py1 + 8 {
                    out.push_str("\n\n");
                } else if y0 > py1 {
                    out.push('\n');
                } else if !out.ends_with(' ') {
                    out.push(' ');
                }
            } else if y0 >= py0 && x > px1 + 2 && !out.ends_with([' ', '\t']) {
                out.push(if x > px1 + 24 { '\t' } else { ' ' });
            }
        };
        for (sid, sl, top) in self.seg_list() {
            if sid < lo.seg || sid > hi.seg {
                continue;
            }
            for (rank, &oi) in reading(sl).iter().enumerate() {
                if (sid, rank) < (lo.seg, lo.op) || (sid, rank) > (hi.seg, hi.op) {
                    continue;
                }
                let d = &sl.ops[oi];
                match &d.op {
                    Op::Text { x, w, text, .. } => {
                        let n = text.chars().count();
                        let Some((c0, c1)) = piece_range(sel, sid, rank, 0, n) else { continue };
                        sep(&mut out, prev, top + d.y0, *x);
                        let piece = char_slice(text, c0, c1);
                        let piece =
                            if out.is_empty() || out.ends_with(['\n', ' ', '\t']) { piece.trim_start() } else { piece };
                        out.push_str(piece);
                        prev = Some((top + d.y0, top + d.y1, x + w));
                    }
                    Op::Code { r, lines, lh, top: ctop, .. } => {
                        let mut first = true;
                        for (li, line) in lines.iter().enumerate() {
                            let n = line.chars().count();
                            let here = (sid, rank, li as u32);
                            if here < (lo.seg, lo.op, lo.line) || here > (hi.seg, hi.op, hi.line) {
                                continue;
                            }
                            let (c0, c1) = piece_range(sel, sid, rank, li as u32, n).unwrap_or((0, 0));
                            let ly = top + r.y + ctop + li as i32 * lh;
                            if first {
                                sep(&mut out, prev, ly, r.x);
                                if prev.is_some() && !out.ends_with('\n') {
                                    out.push('\n');
                                }
                                first = false;
                            } else {
                                out.push('\n');
                            }
                            out.push_str(char_slice(line, c0, c1));
                            prev = Some((ly, ly + lh, r.right()));
                        }
                        if !first {
                            prev = Some((top + r.y, top + r.bottom(), r.right()));
                        }
                    }
                    _ => {}
                }
            }
        }
        out.trim_end().to_string()
    }
}
