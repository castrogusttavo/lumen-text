use super::*;

impl Doc {
    // ── drawing ──

    /// Draws the text into `rect` (`Doc::draw` in `imgview` picks this or the picture of an image
    /// tab). Returns true while more frames are needed (animation, tokenizing).
    pub fn draw_text(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect, o: &DrawOpts) -> bool {
        // The minimap takes a strip at the right; everything else lays out in what remains.
        let full = rect;
        let mm_w = if self.minimap && rect.w > 560 { crate::minimap::style().width() } else { 0 };
        let rect = Rect::new(rect.x, rect.y, rect.w - mm_w, rect.h);
        self.rect = rect;
        self.cell_w = tx.cell_w;
        crate::perf::lap!("doc.pre");
        self.sync_edits();
        crate::perf::lap!("doc.sync_edits");
        let gutter_w = self.gutter_w();
        let gw0 = gutter_w - FOLD_W; // width of the number/marker part of the gutter
        let cols_avail = (((rect.w - gutter_w - 8) as f32 / self.cell_w).floor() as isize - 1).max(8) as usize;
        self.view_prepare(cols_avail);
        if self.reveal {
            self.apply_reveal();
            self.reveal = false;
        }
        crate::perf::lap!("doc.view_prepare");
        self.step_scroll(o.dt);
        crate::perf::lap!("doc.step_scroll");
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

        crate::perf::lap!("doc.layout");
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
        crate::perf::lap!("doc.highlight");
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

        crate::perf::lap!("doc.semantic");
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

        crate::perf::lap!("doc.rows");
        // Inline completion: the lines after the first of a multi-line suggestion are virtual rows
        // right below the last row of the cursor line; the rows after them move down that much.
        let ghost_n = self.ghost.as_ref().map_or(0, |g| g.rest.len());
        let ghost_after: Option<usize> = if ghost_n == 0 {
            None
        } else {
            self.ghost.as_ref().and_then(|g| vlines.binary_search(&g.line).ok()).map(|lk| row_range[lk].1.max(1) - 1)
        };
        let ghost_rest: Vec<String> = match (ghost_after, &self.ghost) {
            (Some(_), Some(g)) => g.rest.clone(),
            _ => Vec::new(),
        };
        self.ghost_rows = ghost_after.map(|r| (first_row + r, ghost_n));
        let vshift = |r: usize| if ghost_after.is_some_and(|g| r > g) { ghost_n } else { 0 };
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
        let ry = |r: usize| rect.y + ((r + vshift(r)) as f32 * lh_f) as i32 + y_off;
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

        crate::perf::lap!("doc.caret_hints");
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

        crate::perf::lap!("doc.conflict_debug");
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
        // Calls `f` with the on-screen rect of each visible row piece of the char range `a..b`.
        let range_rects = |a: usize, b: usize, f: &mut dyn FnMut(Rect)| {
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
                    f(Rect::new(vx(va), ry(r), vx(vb) - vx(va), lh));
                }
            }
        };
        let paint_range = |cv: &mut Canvas, a: usize, b: usize, color: Rgba| {
            range_rects(a, b, &mut |r| cv.fill_clip(r, color, body));
        };
        for (i, &(a, b)) in o.find.iter().enumerate() {
            let color = if Some(i) == o.find_cur { c.find_match_current } else { c.find_match };
            paint_range(cv, a, b, color);
        }
        // Document highlights: the occurrences of the symbol under the caret (read / write from the
        // language server, or the same word when no server offers them). Only what is on screen.
        let (doc_hl, hl_word) = self.assist.live_hl(self.buf.version());
        if o.find.is_empty() && sel.is_empty() && self.ed.extra.is_empty() && !geo.is_empty() {
            let (vis_a, vis_b) = (geo[0].0, geo[geo.len() - 1].0 + geo[geo.len() - 1].1);
            for h in doc_hl.iter().filter(|h| h.b >= vis_a && h.a <= vis_b) {
                paint_range(cv, h.a, h.b, if h.write { c.word_hl_strong } else { c.word_hl });
            }
            if let Some(w) = hl_word {
                let q = crate::editor::Occurrence { needle: w.to_string(), whole_word: true };
                let mut hits: Vec<(usize, usize)> = Vec::new();
                for (lk, text) in lines.iter().enumerate() {
                    let n0 = hits.len();
                    crate::editor::line_occurrences(text, &q, OCCURRENCE_CAP, &mut hits);
                    for h in &mut hits[n0..] {
                        *h = (geo[lk].0 + h.0, geo[lk].0 + h.1);
                    }
                }
                for (a, b) in hits {
                    paint_range(cv, a, b, c.word_hl);
                }
            }
        }
        // Other occurrences of the selected text (Sublime/Zed): a soft tint of the selection color with a
        // faint outline. Only the visible lines are scanned, so it costs the same in any file; it steps
        // aside while the find bar shows its own matches.
        if o.find.is_empty()
            && let Some(q) = crate::editor::occurrence_query(&self.buf, &self.ed)
        {
            let base = theme.selection.over(theme.background);
            let (fill, edge) = (base.with_alpha(0x70), base.mix(theme.foreground, 0.25).with_alpha(0x90));
            let mut hits: Vec<(usize, usize)> = Vec::new();
            for (lk, text) in lines.iter().enumerate() {
                let n0 = hits.len();
                crate::editor::line_occurrences(text, &q, OCCURRENCE_CAP, &mut hits);
                for h in &mut hits[n0..] {
                    *h = (geo[lk].0 + h.0, geo[lk].0 + h.1);
                }
            }
            for &(a, b) in hits.iter().filter(|h| h.0 != sel.start()) {
                range_rects(a, b, &mut |r| {
                    cv.fill_round_clip(r, 3, fill, body);
                    if r.x >= body.x && r.right() <= body.right() && r.y >= body.y && r.bottom() <= body.bottom() {
                        cv.ring(r, 3, edge);
                    }
                });
            }
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

        // Whitespace (`render_whitespace`): a faint dot per space, an arrow per tab. Only marks in
        // the visible columns are placed, so a long line costs no more than a short one.
        let ws_mode = crate::display::whitespace();
        if ws_mode != crate::whitespace::Render::Off {
            let ink = theme.whitespace;
            let wv0 = (left / cell_w as f64).floor().max(0.0) as usize;
            let wv1 = wv0 + (body.w as f32 / cell_w).ceil() as usize + 2;
            let cw = cell_w.round() as i32;
            let dot = ((cell_w / 7.0).round() as i32).clamp(2, 4);
            let mut marks: Vec<(usize, bool)> = Vec::new();
            let mut marks_lk = usize::MAX;
            let mut placed: Vec<(usize, usize, bool)> = Vec::new(); // (visual column, cells, is tab)
            for (r, row) in rows.iter().enumerate() {
                if marks_lk != row.lk {
                    crate::whitespace::marks(&lines[row.lk], ws_mode, &mut marks);
                    marks_lk = row.lk;
                }
                let m0 = marks.partition_point(|m| m.0 < row.c0);
                if marks.get(m0).is_none_or(|m| m.0 >= row.c1) {
                    continue;
                }
                placed.clear();
                if hints_for(inl, &row_hs, wrap_cols, r, row.line).is_empty() {
                    // One pass over the row: tabs run to the next tab stop, everything else is one cell.
                    let (mut v, mut mi) = (0usize, m0);
                    for (k, ch) in rt(r).chars().enumerate() {
                        if v > wv1 || marks.get(mi).is_none_or(|m| m.0 >= row.c1) {
                            break;
                        }
                        let w = if ch == '\t' { tab_w - v % tab_w } else { 1 };
                        if marks[mi].0 == row.c0 + k {
                            placed.push((v, w, marks[mi].1));
                            mi += 1;
                        }
                        v += w;
                    }
                } else {
                    for &(col, tab) in marks[m0..].iter().take_while(|m| m.0 < row.c1 && m.0 - row.c0 <= wv1) {
                        let v = vc(r, col - row.c0, true);
                        let w = if tab { vc(r, col - row.c0 + 1, false).saturating_sub(v).max(1) } else { 1 };
                        placed.push((v, w, tab));
                    }
                }
                let y = ry(r);
                for &(v, w, tab) in &placed {
                    if v + w < wv0 || v > wv1 {
                        continue;
                    }
                    let x = vx(v);
                    if tab {
                        let pad = (cell_w * 0.25).round() as i32;
                        let (x0, x1) = (x + pad, vx(v + w) - pad);
                        let ym = y + lh / 2;
                        cv.fill_clip(Rect::new(x0, ym, (x1 - x0).max(1), 1), ink, body);
                        for k in 1..=2 {
                            cv.fill_clip(Rect::new(x1 - k, ym - k, 1, 2 * k + 1), ink, body);
                        }
                    } else {
                        cv.fill_clip(Rect::new(x + (cw - dot) / 2, y + (lh - dot) / 2, dot, dot), ink, body);
                    }
                }
            }
        }

        // Rulers: thin vertical lines at fixed columns (the cell grid, scrolled with the text).
        for x in crate::assist::ruler_xs(&self.assist.rulers, body.x, body.right(), cell_w, left) {
            cv.fill_clip(Rect::new(x, rect.y, 1, rect.h), c.ruler, body);
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

        crate::perf::lap!("doc.decor");
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
            if Some(r) == ghost_after {
                let gc = fg.with_alpha(crate::inlay::GHOST_ALPHA);
                for gl in &ghost_rest {
                    let s: String = gl.chars().skip(vc0).take(vc1.saturating_sub(vc0)).collect();
                    if !s.is_empty() {
                        pieces.push((s, gc, crate::ui::ITALIC));
                    }
                    pieces.push(("\n".to_string(), fg, 0));
                }
            }
        }
        let body_x = body.x + (vc0 as f32 * cell_w).round() as i32 - left.round() as i32;
        tx.draw_styled(cv, body_x, rect.y + y_off, &pieces, body);
        for (x, y, label) in labels.drain(..) {
            tx.draw_hint(cv, x, y, label, hint_fg, body);
        }

        crate::perf::lap!("doc.text");
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
        let mut blame_end: Option<(usize, i32)> = None;
        if let Some((bl, text)) = &self.blame
            && sel.is_empty()
            && *bl == cur_line
            && let Some((r, vend)) = at_end(*bl)
        {
            let x = vx(vend) + (cell_w * 4.0) as i32;
            if x < body.right() {
                tx.draw_ui(cv, x, ry(r) + (lh - 18) / 2, text, theme.line_number, body, false);
                blame_end = Some((*bl, x + tx.ui_width(text, false)));
            }
        }

        // Inline diagnostics (Error Lens style): the most severe message of a line after its code and
        // after whatever is already drawn there (hints, fold marker, lenses, blame), faded and cut to
        // the room left on the line's last row; with too little room (a long or wrapped line) none.
        if self.assist.inline != crate::assist::InlineDiags::Off && !self.diags.is_empty() {
            let only = (self.assist.inline == crate::assist::InlineDiags::CurrentLine).then_some(cur_line);
            for (line, di) in crate::assist::worst_per_line(&self.diags, first, last, only) {
                // Ghost text owns the end of its line: no diagnostic message under or over it.
                if self.ghost.as_ref().is_some_and(|g| g.line == line) {
                    continue;
                }
                let Some((r, vend)) = at_end(line) else { continue };
                let mut end = vx(vend);
                for (pill, l) in &self.fold_pills {
                    if *l == line {
                        end = end.max(pill.right());
                    }
                }
                for (hit, idx) in &self.lens_hits {
                    if self.lenses.get(*idx).is_some_and(|lz| lz.line == line) {
                        end = end.max(hit.right());
                    }
                }
                if let Some((bl, be)) = blame_end
                    && bl == line
                {
                    end = end.max(be);
                }
                let gap = (cell_w * 3.0).round() as i32;
                let Some((x, room)) = crate::assist::inline_slot(end, body.right() - 6, gap, (cell_w * 6.0) as i32)
                else {
                    continue;
                };
                let d = &self.diags[di];
                let Some(msg) = crate::assist::fit_message(&d.message, room, &mut |s| tx.ui_width(s, false)) else {
                    continue;
                };
                let color = match d.severity {
                    1 => c.error,
                    2 => c.warning,
                    _ => c.info,
                };
                tx.draw_ui(cv, x, ry(r) + (lh - 18) / 2, &msg, color.with_alpha(0xc0), body, false);
            }
        }

        crate::perf::lap!("doc.folds_lens");
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
                        if d.severity <= 2 {
                            // Errors and warnings: wavy.
                            cv.dot(x, base + ((x / 2) % 2), color);
                        } else if info_dot(x - xa) {
                            // Info (spelling, style): flat dotted, so it never reads as an error.
                            cv.dot(x, base + 1, color);
                        }
                    }
                }
            }
        }

        crate::perf::lap!("doc.diagnostics");
        // Gutter: numbers, fold chevrons, git bars, diagnostic markers.
        cv.fill(Rect::new(rect.x, rect.y, gutter_w, rect.h), theme.background);
        let mut gutter: Vec<(String, Rgba)> = Vec::with_capacity(rows.len());
        let digits = self.gutter_digits();
        for (r, row) in rows.iter().enumerate() {
            let color = if row.line == cur_line { theme.line_number_active } else { theme.line_number };
            gutter.push((if row.sub == 0 { format!("{:>digits$}\n", row.line + 1) } else { "\n".to_string() }, color));
            if Some(r) == ghost_after {
                gutter.extend((0..ghost_n).map(|_| ("\n".to_string(), color)));
            }
        }
        if crate::display::show_line_numbers() {
            tx.draw(
                cv,
                rect.x + cell_w.round() as i32,
                rect.y + y_off,
                &gutter,
                Rect::new(rect.x, rect.y, gutter_w, rect.h),
            );
        }
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
        // Coverage: a thin bar at the inner edge of the gutter, on every row of a marked line.
        if !self.cov.is_empty() {
            for (r, row) in rows.iter().enumerate() {
                let Ok(i) = self.cov.binary_search_by_key(&row.line, |m| m.line) else { continue };
                let color = match self.cov[i].cov {
                    crate::coverage::Cov::Covered => c.git_added,
                    crate::coverage::Cov::Partial => c.warning,
                    crate::coverage::Cov::Uncovered => c.error,
                };
                cv.fill_clip(Rect::new(rect.x + gutter_w - 3, ry(r), 2, lh), color.with_alpha(0xd0), gclip);
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

        crate::perf::lap!("doc.gutter");
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
                if crate::display::show_line_numbers() {
                    tx.draw_line(
                        cv,
                        rect.x + cell_w.round() as i32,
                        y,
                        &num,
                        theme.line_number,
                        Rect::new(rect.x, y, gutter_w, lh),
                    );
                }
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

        crate::perf::lap!("doc.sticky");
        // Extra carets (static; only the primary one glides).
        for x in &self.ed.extra {
            let (l, col) = self.buf.line_col(x.head);
            if let Some((r, v)) = at(l, col, false) {
                cv.fill_clip(Rect::new(vx(v), ry(r), CURSOR_W, lh), theme.cursor, body);
            }
        }

        // Breakpoints and the execution arrow, in the gutter.
        if crate::display::show_breakpoints() {
            for &bl in &self.bps {
                if let Some(y) = line_y(bl) {
                    cv.fill_round(Rect::new(rect.x + 2, y + (lh - 10) / 2, 10, 10), 5, c.error);
                }
            }
        }
        if crate::display::show_bookmarks() {
            for &ml in &self.marks {
                if let Some(y) = line_y(ml) {
                    tx.icon(cv, "ui/bookmark", rect.x + gw0 - 14, y + (lh - 9) / 2, 9, c.accent, gclip);
                }
            }
        }
        if let Some(dl) = self.debug_line
            && let Some(y) = line_y(dl)
        {
            tx.icon(cv, "ui/chevron_right", rect.x + 11, y + (lh - 14) / 2, 14, c.warning, gclip);
        }

        crate::perf::lap!("doc.carets_bp");
        // Minimap.
        if mm_w > 0 {
            // One minimap row per visual row, so word wrap and folds show as the editor shows them.
            let total = self.total_rows();
            let l = crate::minimap::layout_for(full, mm_w, total);
            let (bg, fgc) = (theme.background, theme.foreground);
            let wrap_view = live && self.view.cols > 0;
            // Row infos are a pure function of the row's text. The cache is stamped with the buffer version
            // it describes: `sync_edits` re-stamps it across single-line edits (dropping just that row) and
            // any other change leaves a stale stamp, which empties it.
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (
                    self.buf.line_count(),
                    total,
                    live,
                    self.view.cols,
                    tab_w,
                    (fgc.0, fgc.1, fgc.2),
                    l.rect.h,
                    crate::fontcfg::generation(),
                )
                    .hash(&mut h);
                self.view.folds.hidden_ranges().hash(&mut h);
                h.finish()
            };
            let stamp = self.buf.version();
            if self.mm_cache.0 != key || self.mm_cache.1 != stamp {
                self.mm_cache = (key, stamp, Default::default());
            }
            let cache = &mut self.mm_cache.2;
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

        crate::perf::lap!("doc.minimap");
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

        crate::perf::lap!("doc.scrollbars");
        // Caret.
        let caret_color = if o.focused { theme.cursor } else { theme.cursor.with_alpha(0x70) };
        cv.fill_clip(Rect::new(caret_x, caret_y, CURSOR_W, lh), caret_color, body);

        pending || self.animating || self.caret_animating
    }
}

/// Info-level underline pattern: 2 px dots with 2 px gaps from the start of the range (errors and
/// warnings are wavy; see the diagnostics loop of [`Doc::draw`]).
pub(crate) fn info_dot(dx: i32) -> bool {
    dx.rem_euclid(4) < 2
}

#[cfg(test)]
mod underline_tests {
    #[test]
    fn info_underline_is_dotted_not_wavy() {
        let on: Vec<bool> = (0..8).map(super::info_dot).collect();
        assert_eq!(on, [true, true, false, false, true, true, false, false]);
        assert!(super::info_dot(-4) && !super::info_dot(-1), "stable left of the range start too");
    }
}
