//! Drawing of the History tab: the commit graph (coloured lanes, merge / fork curves, nodes),
//! branch / tag / HEAD labels, subject, author and date. Only the rows inside the viewport are
//! visited, and the next page of history is requested when the viewport nears the end of what
//! is loaded.

use super::*;

/// Horizontal distance between two lanes.
pub(super) const LANE_W: i32 = 14;
/// Line width of the lanes (logical px).
const STROKE: f32 = 2.0;

/// Antialiased disc centred at (cx, cy) (logical coordinates).
fn disc(cv: &mut Canvas, cx: f32, cy: f32, r: f32, col: Rgba, clip: Rect) {
    let s = crate::ui::scale();
    let (cx, cy, r) = (cx * s, cy * s, r * s);
    let dc = cv.to_dev(clip);
    let (x0, x1) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
    let (y0, y1) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
    for y in y0.max(dc.y)..=y1.min(dc.bottom() - 1) {
        for x in x0.max(dc.x)..=x1.min(dc.right() - 1) {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let cov = (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            if cov > 0.0 {
                cv.dot_dev(x, y, col.with_alpha((col.3 as f32 * cov) as u8));
            }
        }
    }
}

/// Distance from p to the segment a-b.
fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (qx, qy) = (a.0 + t * vx - p.0, a.1 + t * vy - p.1);
    (qx * qx + qy * qy).sqrt()
}

/// A lane line from (xa, ya) to (xb, yb): a straight bar when vertical, else an antialiased
/// S-curve leaving and arriving vertically (a cubic with vertical tangents).
fn lane_line(cv: &mut Canvas, (xa, ya): (i32, i32), (xb, yb): (i32, i32), col: Rgba, clip: Rect) {
    if yb <= ya {
        return;
    }
    if xa == xb {
        cv.fill_clip(Rect::new(xa - 1, ya, 2, yb - ya), col, clip);
        return;
    }
    let s = crate::ui::scale();
    let (x0, y0, x1, y1) = (xa as f32 * s, ya as f32 * s, xb as f32 * s, yb as f32 * s);
    let ym = (y0 + y1) / 2.0;
    const N: usize = 12;
    let mut pts = [(0.0f32, 0.0f32); N + 1];
    for (i, p) in pts.iter_mut().enumerate() {
        let t = i as f32 / N as f32;
        let u = 1.0 - t;
        // P0 = (x0, y0), P1 = (x0, ym), P2 = (x1, ym), P3 = (x1, y1).
        let x = u * u * u * x0 + 3.0 * u * u * t * x0 + 3.0 * u * t * t * x1 + t * t * t * x1;
        let y = u * u * u * y0 + 3.0 * u * u * t * ym + 3.0 * u * t * t * ym + t * t * t * y1;
        *p = (x, y);
    }
    let half = STROKE * s / 2.0;
    let dc = cv.to_dev(clip);
    let (lx, hx) = (x0.min(x1) - half - 1.0, x0.max(x1) + half + 1.0);
    for y in (y0.floor() as i32).max(dc.y)..(y1.ceil() as i32).min(dc.bottom()) {
        for x in (lx.floor() as i32).max(dc.x)..(hx.ceil() as i32).min(dc.right()) {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d = pts.windows(2).map(|w| seg_dist(p, w[0], w[1])).fold(f32::MAX, f32::min);
            let cov = (half + 0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                cv.dot_dev(x, y, col.with_alpha((col.3 as f32 * cov) as u8));
            }
        }
    }
}

impl App {
    pub(super) fn gp_draw_history(&mut self, cv: &mut Canvas, gp: &mut GitPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        gp.poll_history();
        if gp.commits.is_empty() {
            if !gp.log_done && gp.loader.is_none() {
                gp.load_more();
            }
            let msg = if gp.log_done { "No commits" } else { "Loading…" };
            let w = self.tx.ui_width(msg, false);
            self.tx.draw_ui(cv, body.x + (body.w - w) / 2, body.y + 40, msg, dim, body, false);
            return;
        }
        let total = gp.content_h() + 28;
        gp.max_scroll[1] = (total - body.h).max(0) as f64;
        gp.scroll[1] = gp.scroll[1].clamp(0.0, gp.max_scroll[1]);
        let scroll = gp.scroll[1] as i32;
        let n = gp.hist_len();
        let first = gp.hist_at_y(scroll);
        let mut last = first;
        while last + 1 < n && gp.hist_top(last + 1) < scroll + body.h {
            last += 1;
        }
        // Graph width: the widest visible row, at most 40% of the panel.
        let lanes = (first..=last)
            .filter_map(|k| match gp.hist_row(k) {
                Some(Row::Commit(i)) => gp.graph.rows.get(i).map(|g| g.width as i32),
                _ => None,
            })
            .max()
            .unwrap_or(1)
            .max(1);
        let cap = ((body.w - 12) * 2 / 5 / LANE_W).max(2);
        let gw = lanes.min(cap) * LANE_W;
        let gx = body.x + 10;
        let lane_x = |col: u16| gx + col as i32 * LANE_W + LANE_W / 2;
        let text_x = gx + gw + 6;
        let pal = gp.palette(c.sidebar_bg, &[strong.with_alpha(0x1e), strong.with_alpha(0x12)]);
        let now = now_secs();
        let mut last_commit = 0;
        for k in first..=last {
            let Some(row) = gp.hist_row(k) else { break };
            let y = body.y + gp.hist_top(k) - scroll;
            let h = GitPanel::row_h(row);
            let vis = Rect::new(body.x, y.max(body.y), body.w, (y + h).min(body.bottom()) - y.max(body.y));
            if vis.h <= 0 {
                continue;
            }
            let r = Rect::new(body.x + 6, y + 1, body.w - 12, h - 2);
            let hov = self.hv(r) && self.hv(vis);
            let cur = gp.focused && gp.cursor == k;
            let gclip = Rect::new(gx - 2, vis.y, gw + 4, vis.h);
            match row {
                Row::Commit(i) => {
                    last_commit = i;
                    let open = gp.open_commit == Some(i);
                    if open {
                        cv.fill_round_clip(r, 8, strong.with_alpha(0x1a), body);
                    } else if hov || cur {
                        cv.fill_round_clip(r, 8, strong.with_alpha(if cur { 0x1e } else { 0x12 }), body);
                    }
                    // Lanes, then the node on top.
                    let mid = y + h / 2;
                    for s in gp.graph.segs_of(i) {
                        let (ya, yb) = if s.lower { (mid, y + h) } else { (y, mid) };
                        let col = pal[s.color as usize % LANE_COLORS];
                        lane_line(cv, (lane_x(s.from), ya), (lane_x(s.to), yb), col, gclip);
                    }
                    if let Some(g) = gp.graph.rows.get(i).copied() {
                        let (nx, ny) = (lane_x(g.col) as f32, mid as f32);
                        let col = pal[g.color as usize % LANE_COLORS];
                        if gp.is_head(i) {
                            // The checked-out commit: a ring around the node.
                            disc(cv, nx, ny, 7.0, strong, gclip);
                            disc(cv, nx, ny, 5.5, c.sidebar_bg, gclip);
                            disc(cv, nx, ny, 4.0, col, gclip);
                        } else if g.merge {
                            // Merges are hollow.
                            disc(cv, nx, ny, 4.5, col, gclip);
                            disc(cv, nx, ny, 2.0, c.sidebar_bg, gclip);
                        } else {
                            disc(cv, nx, ny, 4.0, col, gclip);
                        }
                    }
                    let e = gp.commits[i].clone();
                    let avail = r.right() - 8 - text_x;
                    // Labels before the subject (at most half of the line), then the subject.
                    let mut x = text_x;
                    let chips = gp.chips_of(i).to_vec();
                    let chip_max = text_x + avail / 2;
                    for (ci, ch) in chips.iter().enumerate() {
                        let label = fit(&mut self.tx, &ch.name, 110, false);
                        let bold = matches!(ch.kind, ChipKind::Current | ChipKind::Head);
                        let pw = self.tx.ui_width_sized(&label, bold, 12.0) + 12;
                        if x + pw > chip_max {
                            let more = format!("+{}", chips.len() - ci);
                            let mw = self.tx.ui_width_sized(&more, false, 12.0) + 10;
                            let pr = Rect::new(x, y + 6, mw, 18);
                            cv.fill_round_clip(pr, 8, strong.with_alpha(0x1e), vis);
                            self.tx.draw_ui_sized(
                                cv,
                                pr.x + 5,
                                pr.y + 1,
                                &more,
                                strong,
                                pr.intersect(&vis),
                                false,
                                12.0,
                            );
                            x += mw + 4;
                            break;
                        }
                        let pr = Rect::new(x, y + 6, pw, 18);
                        let fg = match ch.kind {
                            ChipKind::Head | ChipKind::Current => {
                                cv.fill_round_clip(pr, 8, c.accent, vis);
                                c.on_accent
                            }
                            ChipKind::Branch => {
                                cv.fill_round_clip(pr, 8, c.accent.with_alpha(0x2e), vis);
                                c.accent_text
                            }
                            ChipKind::Tag => {
                                cv.ring(pr.intersect(&vis), 8, c.accent_text.with_alpha(0xa0));
                                c.accent_text
                            }
                            ChipKind::Remote => {
                                cv.fill_round_clip(pr, 8, strong.with_alpha(0x18), vis);
                                strong
                            }
                        };
                        self.tx.draw_ui_sized(cv, pr.x + 6, pr.y + 1, &label, fg, pr.intersect(&vis), bold, 12.0);
                        x += pw + 4;
                    }
                    let subj = if e.subject.is_empty() { "(no subject)".to_string() } else { e.subject.clone() };
                    let subj = fit(&mut self.tx, &subj, r.right() - 8 - x, open);
                    self.tx.draw_ui(cv, x, y + 5, &subj, strong, vis, open);
                    let meta = format!("{}  ·  {}  ·  {}", e.short, e.author, git::ago((now - e.ts).max(0)));
                    let meta = fit(&mut self.tx, &meta, avail, false);
                    self.tx.draw_ui_sized(cv, text_x, y + 25, &meta, dim, vis, false, 12.0);
                    gp.hits.push((r.intersect(&vis), Hit::Commit(i)));
                }
                Row::CFile(j) => {
                    // The lanes keep running past the files of the expanded commit.
                    if let Some(o) = gp.open_commit {
                        for (col, ci) in gp.graph.below(o) {
                            lane_line(
                                cv,
                                (lane_x(col), y),
                                (lane_x(col), y + h),
                                pal[ci as usize % LANE_COLORS],
                                gclip,
                            );
                        }
                    }
                    let f = gp.files[j].clone();
                    let sel = gp.open_file == Some(j);
                    let fr = Rect::new(text_x - 4, r.y, r.right() - text_x + 4, r.h);
                    let fh = self.hv(fr) && self.hv(vis);
                    if sel || cur {
                        cv.fill_round_clip(fr, 8, strong.with_alpha(0x1e), body);
                    } else if fh {
                        cv.fill_round_clip(fr, 8, strong.with_alpha(0x12), body);
                    }
                    let letter = f.code.to_string();
                    self.tx.draw_ui(cv, text_x + 4, y + 5, &letter, code_color(&c, f.code), vis, true);
                    let (name, dir) = split_name(&f.path);
                    let nx = text_x + 24;
                    let avail = r.right() - 8 - nx;
                    let nm = fit(&mut self.tx, name, avail, false);
                    let nw = self.tx.ui_width(&nm, false);
                    self.tx.draw_ui(cv, nx, y + 5, &nm, strong, vis, false);
                    if !dir.is_empty() && avail - nw > 40 {
                        let d = fit(&mut self.tx, dir, avail - nw - 10, false);
                        self.tx.draw_ui_sized(cv, nx + nw + 8, y + 7, &d, dim, vis, false, 12.0);
                    }
                    gp.hits.push((fr.intersect(&vis), Hit::CFile(j)));
                }
                _ => {}
            }
        }
        // Keep a page ahead of the viewport.
        if !gp.log_done && last_commit + LOAD_AHEAD >= gp.commits.len() {
            gp.load_more();
        }
        let end_y = body.y + gp.hist_top(n) - scroll;
        if end_y < body.bottom() {
            let msg = if gp.log_done { format!("{} commits", gp.commits.len()) } else { "Loading…".to_string() };
            let w = self.tx.ui_width(&msg, false);
            self.tx.draw_ui(cv, body.x + (body.w - w) / 2, end_y + 4, &msg, dim, body, false);
        }
        let (pos, visible) = (gp.scroll[1], body.h as f64);
        self.region_bar(cv, crate::scrollreg::Reg::GitLog, body, total as f64, visible, pos, 1.0, false);
    }
}
