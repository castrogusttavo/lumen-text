//! Drawing a terminal grid, shared by the bottom panel's terminal and the AI panel: cell
//! backgrounds, text placed at exact cell positions (a glyph from a fallback font never pushes the
//! rest of the row out of its columns), bold / dim / italic / underline / strikethrough, box-drawing
//! and block characters drawn as shapes (seamless joins whatever the font), the mouse selection,
//! hovered hyperlinks and the cursor in its DECSCUSR shape.

use crate::{
    tasks,
    terminal::{self, Cell, CursorShape, Term},
    theme::Rgba,
    ui::{self, Canvas, Rect, TextCtx},
};

/// Colors and state for one draw.
pub struct Look {
    pub fg: Rgba,
    pub bg: Rgba,
    pub cursor: Rgba,
    pub selection: Rgba,
    /// The terminal has the keyboard focus (solid cursor; otherwise a hollow one).
    pub focused: bool,
    /// OSC 8 link id under the pointer (underlined), 0 = none.
    pub hover_link: u16,
}

/// Rows and columns of `cell_w` x `line_h` cells that fit in `w` x `h` px (at least 2 x 10).
pub fn fit(cell_w: f32, w: i32, h: i32) -> (usize, usize) {
    let lh = ui::line_h() as i32;
    (((h / lh.max(1)) as usize).max(2), ((w as f32 / cell_w.max(1.0)) as usize).max(10))
}

/// The cell under a point, for a grid drawn at (ox, oy); `None` outside the grid.
pub fn cell_at(term: &Term, cell_w: f32, ox: i32, oy: i32, x: f64, y: f64) -> Option<(usize, usize)> {
    let lh = ui::line_h() as f64;
    let (fx, fy) = ((x - ox as f64) / cell_w as f64, (y - oy as f64) / lh);
    if fx < 0.0 || fy < 0.0 {
        return None;
    }
    let (r, c) = (fy as usize, fx as usize);
    (r < term.grid.rows).then_some((r, c.min(term.grid.cols.saturating_sub(1))))
}

/// Can the char join a run of text shaped in one go (its glyph is in the code font and one cell
/// wide)? Everything else is placed on its own at its column.
fn simple(ch: char) -> bool {
    (ch as u32) < 0x0500
}

/// Style flags for `TextCtx::draw_styled`.
fn text_style(c: &Cell) -> u8 {
    let mut s = 0;
    if c.has(terminal::BOLD) {
        s |= ui::BOLD;
    }
    if c.has(terminal::ITALIC) {
        s |= ui::ITALIC;
    }
    s
}

/// What decides a run's look: colors, flags (minus WIDE), selection and link hover.
#[derive(Clone, Copy, PartialEq)]
struct Key {
    fg: Option<Rgba>,
    bg: Option<Rgba>,
    flags: u8,
    selected: bool,
    hot: bool,
}

/// (text color, background fill) of a run.
fn resolve(k: &Key, look: &Look) -> (Rgba, Option<Rgba>) {
    let (mut fg, mut bg) = (k.fg.unwrap_or(look.fg), k.bg);
    if k.flags & terminal::INVERSE != 0 {
        let f = fg;
        fg = bg.unwrap_or(look.bg);
        bg = Some(f);
    }
    if k.selected {
        bg = Some(look.selection.over(bg.unwrap_or(look.bg)));
    }
    let base = bg.unwrap_or(look.bg);
    let mut fg = tasks::legible(fg, base);
    if k.flags & terminal::DIM != 0 {
        fg = fg.mix(base, 0.45);
    }
    (fg, bg)
}

/// Draw the visible screen of `term` with its top-left cell at (ox, oy), clipped to `clip`.
pub fn draw(tx: &mut TextCtx, cv: &mut Canvas, term: &Term, ox: i32, oy: i32, clip: Rect, look: &Look) {
    let g = &term.grid;
    let (cw, lh) = (tx.cell_w, ui::line_h() as i32);
    let off = term.scroll_offset;
    let xs = |c: usize| ox + (c as f32 * cw).round() as i32;
    let mut row: Vec<Cell> = Vec::with_capacity(g.cols);
    for r in 0..g.rows {
        let y = oy + r as i32 * lh;
        if y >= clip.bottom() {
            break;
        }
        if y + lh <= clip.y {
            continue;
        }
        let abs = g.abs_row(r, off);
        row.clear();
        row.extend((0..g.cols).map(|c| g.view_cell(r, c, off)));
        let key = |c: usize, cell: &Cell| Key {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & !terminal::WIDE,
            selected: term.sel.is_some_and(|s| s.contains(abs, c)),
            hot: look.hover_link != 0 && cell.link == look.hover_link,
        };
        // Runs of cells that look the same: one color resolution, one background rect each.
        let mut runs: Vec<(usize, usize, Key)> = Vec::new();
        for (c, cell) in row.iter().enumerate() {
            let k = key(c, cell);
            match runs.last_mut() {
                Some((_, end, rk)) if *rk == k => *end = c + 1,
                _ => runs.push((c, c + 1, k)),
            }
        }
        let resolved: Vec<(Rgba, Option<Rgba>)> = runs.iter().map(|(_, _, k)| resolve(k, look)).collect();
        for ((c0, c1, _), (_, bg)) in runs.iter().zip(&resolved) {
            if let Some(bg) = bg {
                cv.fill_clip(Rect::new(xs(*c0), y, xs(*c1) - xs(*c0), lh), *bg, clip);
            }
        }
        // Block cursor under the text, so the character stays readable on it.
        let cursor_here = off == 0 && g.cursor_visible && g.cy == r && g.cx < g.cols;
        if cursor_here && look.focused && g.cursor_shape == CursorShape::Block {
            let w = if row[g.cx].has(terminal::WIDE) { 2 } else { 1 };
            let (x0, x1) = (xs(g.cx), xs((g.cx + w).min(g.cols)));
            cv.fill_clip(Rect::new(x0, y, x1 - x0, lh), look.cursor.with_alpha(0xb0), clip);
        }
        // Text: runs of simple characters are shaped together; the rest is placed cell by cell.
        let last = row.iter().rposition(|c| c.ch != ' ' && c.ch != terminal::WIDE_TAIL).map_or(0, |i| i + 1);
        let mut pieces: Vec<(String, Rgba, u8)> = Vec::new();
        let mut seg_x: Option<usize> = None;
        let flush =
            |tx: &mut TextCtx, cv: &mut Canvas, pieces: &mut Vec<(String, Rgba, u8)>, seg: &mut Option<usize>| {
                if let Some(c0) = seg.take()
                    && !pieces.is_empty()
                {
                    tx.draw_styled(cv, xs(c0), y, pieces, clip);
                }
                pieces.clear();
            };
        for ((c0, c1, k), (fg, _)) in runs.iter().zip(&resolved) {
            if *c0 >= last {
                break;
            }
            let style = text_style(&row[*c0]);
            for (c, &cell) in row.iter().enumerate().take((*c1).min(last)).skip(*c0) {
                if cell.ch == terminal::WIDE_TAIL {
                    continue;
                }
                let ch = if k.flags & terminal::HIDDEN != 0 { ' ' } else { cell.ch };
                if simple(ch) && !cell.has(terminal::WIDE) {
                    if seg_x.is_none() {
                        seg_x = Some(c);
                    }
                    match pieces.last_mut() {
                        Some((s, pc, ps)) if *pc == *fg && *ps == style => s.push(ch),
                        _ => pieces.push((ch.to_string(), *fg, style)),
                    }
                    continue;
                }
                flush(&mut *tx, &mut *cv, &mut pieces, &mut seg_x);
                let w = if cell.has(terminal::WIDE) { 2 } else { 1 };
                let (x0, x1) = (xs(c), xs(c + w));
                if !block_glyph(cv, ch, Rect::new(x0, y, x1 - x0, lh), *fg, clip) {
                    tx.draw_styled(cv, x0, y, &[(ch.to_string(), *fg, style)], clip);
                }
            }
        }
        flush(&mut *tx, &mut *cv, &mut pieces, &mut seg_x);
        // Decorations: underline (and hovered links), strikethrough.
        for ((c0, c1, k), (fg, _)) in runs.iter().zip(&resolved) {
            let (x0, x1) = (xs(*c0), xs(*c1));
            if k.flags & terminal::UNDERLINE != 0 || k.hot {
                cv.fill_clip(Rect::new(x0, y + lh - 2, x1 - x0, 1), *fg, clip);
            }
            if k.flags & terminal::STRIKE != 0 {
                cv.fill_clip(Rect::new(x0, y + lh / 2, x1 - x0, 1), *fg, clip);
            }
        }
        if cursor_here {
            let w = if row[g.cx].has(terminal::WIDE) { 2 } else { 1 };
            let (x0, x1) = (xs(g.cx), xs((g.cx + w).min(g.cols)));
            let col = look.cursor;
            match (look.focused, g.cursor_shape) {
                (true, CursorShape::Block) => {}
                (true, CursorShape::Underline) => cv.fill_clip(Rect::new(x0, y + lh - 2, x1 - x0, 2), col, clip),
                (true, CursorShape::Bar) => cv.fill_clip(Rect::new(x0, y, 2, lh), col, clip),
                (false, _) => {
                    let c = col.with_alpha(0x90);
                    cv.fill_clip(Rect::new(x0, y, x1 - x0, 1), c, clip);
                    cv.fill_clip(Rect::new(x0, y + lh - 1, x1 - x0, 1), c, clip);
                    cv.fill_clip(Rect::new(x0, y, 1, lh), c, clip);
                    cv.fill_clip(Rect::new(x1 - 1, y, 1, lh), c, clip);
                }
            }
        }
    }
}

/// Arms of a box-drawing character: (up, right, down, left), 0 none, 1 light, 2 heavy, 3 double;
/// `round` for the arc corners.
fn box_arms(ch: char) -> Option<([u8; 4], bool)> {
    let a = |u, r, d, l| Some(([u, r, d, l], false));
    match ch {
        '─' | '┄' | '┈' | '╌' => a(0, 1, 0, 1),
        '━' | '┅' | '┉' | '╍' => a(0, 2, 0, 2),
        '│' | '┆' | '┊' | '╎' => a(1, 0, 1, 0),
        '┃' | '┇' | '┋' | '╏' => a(2, 0, 2, 0),
        '┌' => a(0, 1, 1, 0),
        '┐' => a(0, 0, 1, 1),
        '└' => a(1, 1, 0, 0),
        '┘' => a(1, 0, 0, 1),
        '├' => a(1, 1, 1, 0),
        '┤' => a(1, 0, 1, 1),
        '┬' => a(0, 1, 1, 1),
        '┴' => a(1, 1, 0, 1),
        '┼' => a(1, 1, 1, 1),
        '┏' => a(0, 2, 2, 0),
        '┓' => a(0, 0, 2, 2),
        '┗' => a(2, 2, 0, 0),
        '┛' => a(2, 0, 0, 2),
        '┣' => a(2, 2, 2, 0),
        '┫' => a(2, 0, 2, 2),
        '┳' => a(0, 2, 2, 2),
        '┻' => a(2, 2, 0, 2),
        '╋' => a(2, 2, 2, 2),
        '═' => a(0, 3, 0, 3),
        '║' => a(3, 0, 3, 0),
        '╔' => a(0, 3, 3, 0),
        '╗' => a(0, 0, 3, 3),
        '╚' => a(3, 3, 0, 0),
        '╝' => a(3, 0, 0, 3),
        '╠' => a(3, 3, 3, 0),
        '╣' => a(3, 0, 3, 3),
        '╦' => a(0, 3, 3, 3),
        '╩' => a(3, 3, 0, 3),
        '╬' => a(3, 3, 3, 3),
        '╴' => a(0, 0, 0, 1),
        '╵' => a(1, 0, 0, 0),
        '╶' => a(0, 1, 0, 0),
        '╷' => a(0, 0, 1, 0),
        '╭' => Some(([0, 1, 1, 0], true)),
        '╮' => Some(([0, 0, 1, 1], true)),
        '╯' => Some(([1, 0, 0, 1], true)),
        '╰' => Some(([1, 1, 0, 0], true)),
        _ => None,
    }
}

/// Quadrants of a block character: (upper-left, upper-right, lower-left, lower-right).
fn quadrants(ch: char) -> Option<[bool; 4]> {
    Some(match ch {
        '▘' => [true, false, false, false],
        '▝' => [false, true, false, false],
        '▖' => [false, false, true, false],
        '▗' => [false, false, false, true],
        '▀' => [true, true, false, false],
        '▄' => [false, false, true, true],
        '▌' => [true, false, true, false],
        '▐' => [false, true, false, true],
        '▚' => [true, false, false, true],
        '▞' => [false, true, true, false],
        '▛' => [true, true, true, false],
        '▜' => [true, true, false, true],
        '▙' => [true, false, true, true],
        '▟' => [false, true, true, true],
        '█' => [true, true, true, true],
        _ => return None,
    })
}

/// Draw box-drawing / block characters as shapes filling cell `r`; false when `ch` is not one.
pub fn block_glyph(cv: &mut Canvas, ch: char, r: Rect, col: Rgba, clip: Rect) -> bool {
    let clip = clip.intersect(&r);
    if let Some(q) = quadrants(ch) {
        let (hw, hh) = (r.w / 2, r.h / 2);
        let parts = [
            Rect::new(r.x, r.y, hw, hh),
            Rect::new(r.x + hw, r.y, r.w - hw, hh),
            Rect::new(r.x, r.y + hh, hw, r.h - hh),
            Rect::new(r.x + hw, r.y + hh, r.w - hw, r.h - hh),
        ];
        for (on, p) in q.iter().zip(parts) {
            if *on {
                cv.fill_exact(p, col, clip);
            }
        }
        return true;
    }
    let cp = ch as u32;
    match cp {
        // Lower eighths ▁..▇, upper eighth ▔, left eighths ▉..▏, right eighth ▕.
        0x2581..=0x2587 => {
            let h = r.h * (cp - 0x2580) as i32 / 8;
            cv.fill_exact(Rect::new(r.x, r.bottom() - h, r.w, h), col, clip);
            return true;
        }
        0x2594 => {
            cv.fill_exact(Rect::new(r.x, r.y, r.w, (r.h / 8).max(1)), col, clip);
            return true;
        }
        0x2589..=0x258F => {
            let w = r.w * (8 - (cp - 0x2588) as i32) / 8;
            cv.fill_exact(Rect::new(r.x, r.y, w.max(1), r.h), col, clip);
            return true;
        }
        0x2595 => {
            let w = (r.w / 8).max(1);
            cv.fill_exact(Rect::new(r.right() - w, r.y, w, r.h), col, clip);
            return true;
        }
        // Shades ░ ▒ ▓.
        0x2591..=0x2593 => {
            let a = [0x40u8, 0x80, 0xc0][(cp - 0x2591) as usize];
            cv.fill_exact(r, col.with_alpha(a), clip);
            return true;
        }
        _ => {}
    }
    let Some((arms, round)) = box_arms(ch) else { return false };
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    if round {
        // Arc corner: a quarter circle joining the two arms, then straight to the cell edges.
        let rad = (r.w / 2).min(r.h / 2).max(1);
        let (dx, dy) = (if arms[1] > 0 { 1 } else { -1 }, if arms[2] > 0 { 1 } else { -1 });
        arc(cv, cx, cy, dx, dy, rad, col, clip);
        let hx = cx + dx * rad;
        let (hx0, hx1) = if dx > 0 { (hx, r.right()) } else { (r.x, hx + 1) };
        cv.fill_clip(Rect::new(hx0, cy, hx1 - hx0, 1), col, clip);
        let vy = cy + dy * rad;
        let (vy0, vy1) = if dy > 0 { (vy, r.bottom()) } else { (r.y, vy + 1) };
        cv.fill_clip(Rect::new(cx, vy0, 1, vy1 - vy0), col, clip);
        return true;
    }
    // Straight arms from the center to the edges; heavy = 2 px, double = two 1 px lines.
    for (i, &w) in arms.iter().enumerate() {
        if w == 0 {
            continue;
        }
        let offsets: &[i32] = match w {
            2 => &[0, 1],
            3 => &[-1, 1],
            _ => &[0],
        };
        for &o in offsets {
            let seg = match i {
                0 => Rect::new(cx + o, r.y, 1, cy - r.y + 1),
                1 => Rect::new(cx, cy + o, r.right() - cx, 1),
                2 => Rect::new(cx + o, cy, 1, r.bottom() - cy),
                _ => Rect::new(r.x, cy + o, cx - r.x + 1, 1),
            };
            cv.fill_clip(seg, col, clip);
        }
    }
    true
}

/// Antialiased quarter circle of radius `rad` (logical px) bending from the arm at the cell
/// center (cx, cy) toward (dx, dy): its center sits at (cx + dx·rad, cy + dy·rad).
#[allow(clippy::too_many_arguments)]
fn arc(cv: &mut Canvas, cx: i32, cy: i32, dx: i32, dy: i32, rad: i32, col: Rgba, clip: Rect) {
    let s = ui::scale();
    let (ccx, ccy) = ((cx + dx * rad) as f32 * s, (cy + dy * rad) as f32 * s);
    let (r_out, thick) = (rad as f32 * s + 0.5 * s, s.max(1.0));
    let dclip = cv.to_dev(clip);
    let (x0, x1) = ((cx as f32 * s) as i32, ((cx + dx * rad) as f32 * s) as i32);
    let (y0, y1) = ((cy as f32 * s) as i32, ((cy + dy * rad) as f32 * s) as i32);
    let (xa, xb) = (x0.min(x1) - 1, x0.max(x1) + 1);
    let (ya, yb) = (y0.min(y1) - 1, y0.max(y1) + 1);
    for py in ya..=yb {
        for px in xa..=xb {
            if !dclip.contains(px as f64, py as f64) {
                continue;
            }
            // Only the quadrant facing the cell center.
            let (fx, fy) = (px as f32 + 0.5 - ccx, py as f32 + 0.5 - ccy);
            if fx * dx as f32 > 0.5 || fy * dy as f32 > 0.5 {
                continue;
            }
            let d = (fx * fx + fy * fy).sqrt();
            let cov = ((r_out - d).clamp(0.0, 1.0) - (r_out - thick - d).clamp(0.0, 1.0)).clamp(0.0, 1.0);
            if cov > 0.0 {
                cv.dot_dev(px, py, col.with_alpha((col.3 as f32 * cov) as u8));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Rect = Rect { x: 0, y: 0, w: 40, h: 40 };

    fn canvas(w: i32, h: i32) -> Vec<u32> {
        vec![0u32; (w * h) as usize]
    }

    #[test]
    fn box_and_block_shapes_fill_their_cells() {
        let mut px = canvas(40, 40);
        let mut cv = Canvas { px: &mut px, w: 40, h: 40 };
        let white = Rgba(255, 255, 255, 255);
        let cell = Rect::new(10, 10, 10, 20);
        assert!(block_glyph(&mut cv, '█', cell, white, ALL));
        let lit = |px: &[u32], x: i32, y: i32| px[(y * 40 + x) as usize] != 0;
        assert!(lit(cv.px, 10, 10) && lit(cv.px, 19, 29) && !lit(cv.px, 20, 10) && !lit(cv.px, 9, 10));
        let mut px = canvas(40, 40);
        let mut cv = Canvas { px: &mut px, w: 40, h: 40 };
        assert!(block_glyph(&mut cv, '─', cell, white, ALL));
        assert!(lit(cv.px, 10, 20) && lit(cv.px, 19, 20), "a horizontal line spans the cell");
        assert!(!lit(cv.px, 15, 12));
        let mut px = canvas(40, 40);
        let mut cv = Canvas { px: &mut px, w: 40, h: 40 };
        assert!(block_glyph(&mut cv, '▝', cell, white, ALL));
        assert!(lit(cv.px, 18, 11) && !lit(cv.px, 11, 11) && !lit(cv.px, 18, 25));
        let mut px = canvas(40, 40);
        let mut cv = Canvas { px: &mut px, w: 40, h: 40 };
        assert!(block_glyph(&mut cv, '╭', cell, white, ALL));
        assert!(lit(cv.px, 19, 20) && lit(cv.px, 15, 29), "both arms reach their edges");
        assert!(!block_glyph(&mut cv, 'a', cell, white, ALL));
        assert!(!block_glyph(&mut cv, '⏺', cell, white, ALL));
    }

    #[test]
    fn run_colors_follow_inverse_selection_and_dim() {
        let look = Look {
            fg: Rgba(200, 200, 200, 255),
            bg: Rgba(20, 20, 20, 255),
            cursor: Rgba(255, 255, 255, 255),
            selection: Rgba(40, 80, 160, 255),
            focused: true,
            hover_link: 0,
        };
        let k = Key { fg: None, bg: None, flags: 0, selected: false, hot: false };
        assert_eq!(resolve(&k, &look), (look.fg, None));
        let inv = Key { flags: terminal::INVERSE, ..k };
        assert_eq!(resolve(&inv, &look).1, Some(look.fg), "inverse paints the text color behind");
        let sel = Key { selected: true, ..k };
        assert_eq!(resolve(&sel, &look).1, Some(look.selection));
        let dim = Key { flags: terminal::DIM, ..k };
        let (d, _) = resolve(&dim, &look);
        assert!(d.luminance() < look.fg.luminance(), "dim text is closer to the background");
    }
}
