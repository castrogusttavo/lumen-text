use super::*;

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
pub(super) struct VRow {
    pub(super) lk: usize,
    pub(super) line: usize,
    pub(super) sub: usize,
    pub(super) c0: usize,
    pub(super) c1: usize,
    pub(super) b0: usize,
    pub(super) b1: usize,
    pub(super) last: bool,
}

/// Hints of visual row `r` (of line `line`): the whole line's when unwrapped, else the row's slice.
pub(super) fn hints_for<'a>(
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
pub(super) fn hint_decor<'a>(
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
        // Inline-completion ghost text is part of the row's text pieces: no pill, no label.
        if h.kind == crate::inlay::HintKind::Ghost {
            continue;
        }
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

pub(super) fn row_text<'a>(rows: &'a [VRow], lines: &'a [String], r: usize) -> &'a str {
    let w = &rows[r];
    &lines[w.lk][w.b0..w.b1]
}

/// Colored text pieces of bytes [b0, b1) of one line (a visual row), restricted to visual
/// columns [vc0, vc1) counted from b0, with tabs expanded to spaces.
pub(super) fn window_pieces(
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
