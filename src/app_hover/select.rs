use super::*;

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

pub(super) fn max_scroll(h: &HoverTip) -> f64 {
    let total = h.laid.as_ref().map_or(0, |l| l.1.h) as f64;
    (total - h.view.h as f64).max(0.0)
}

pub(super) fn clamp_scroll(h: &mut HoverTip) {
    h.scroll = h.scroll.clamp(0.0, max_scroll(h));
}

/// Index of the laid-out line at screen (x, y).
pub(super) fn line_at(h: &HoverTip, _x: f64, y: f64) -> Option<usize> {
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
pub(super) fn boxes_of<'a>(h: &'a mut HoverTip, tx: &mut TextCtx, line: usize) -> &'a [CharBox] {
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
pub(super) fn pos_at(h: &mut HoverTip, tx: &mut TextCtx, x: f64, y: f64) -> Option<SelPt> {
    let line = line_at(h, x, y)?;
    let rel = (x - h.view.x as f64).round() as i32;
    Some((line, col_at(boxes_of(h, tx, line), rel)))
}

pub(super) fn ordered(a: SelPt, b: SelPt) -> (SelPt, SelPt) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Highlight rectangles of the selection, relative to the content origin (x) and the top of the
/// laid-out content (y).
pub(super) fn sel_rects(h: &mut HoverTip, tx: &mut TextCtx) -> Vec<Rect> {
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

pub(super) fn open_external(url: &str) {
    crate::paths::open_url(url);
}
