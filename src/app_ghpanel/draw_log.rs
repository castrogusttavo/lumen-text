//! The log of a job: a filter box, "failed steps only", and the lines (step headers, line numbers,
//! errors and warnings marked) drawn row by row for the visible part only.

use super::*;
use crate::scrollreg::Reg;
use ghub::LogLine;
use items::{Btn, BtnStyle, phase_icon};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogClass {
    Normal,
    Error,
    Warning,
    Group,
    Command,
    /// `##[endgroup]`: not shown.
    EndGroup,
}

/// Escape sequences (colors, cursor moves) out of a runner line.
pub fn strip_ansi(s: &str) -> String {
    if !s.contains('\u{1b}') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if it.peek() == Some(&'[') {
            it.next();
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// What kind of line this is, and the text to show (the `##[..]` marker removed).
pub fn classify(text: &str) -> (LogClass, String) {
    let t = strip_ansi(text);
    let (class, rest) = if let Some(r) = t.strip_prefix("##[error]") {
        (LogClass::Error, r.to_string())
    } else if let Some(r) = t.strip_prefix("##[warning]") {
        (LogClass::Warning, r.to_string())
    } else if let Some(r) = t.strip_prefix("##[group]") {
        (LogClass::Group, r.to_string())
    } else if t.starts_with("##[endgroup]") {
        (LogClass::EndGroup, String::new())
    } else if let Some(r) = t.strip_prefix("##[command]") {
        (LogClass::Command, r.to_string())
    } else if t.starts_with("Error:") || t.starts_with("error:") || t.starts_with("error[") {
        (LogClass::Error, t.clone())
    } else {
        (LogClass::Normal, t.clone())
    };
    (class, rest)
}

/// The rows to show for `lines` under `filter` (lowercase): `(line index, true)` is a step header
/// drawn before the line, `(i, false)` the line itself. End-of-group lines are left out.
pub fn view_rows(lines: &[LogLine], filter: &str) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    let mut step: Option<&str> = None;
    for (i, l) in lines.iter().enumerate() {
        let (class, text) = classify(&l.text);
        if class == LogClass::EndGroup {
            continue;
        }
        if !filter.is_empty() && !text.to_lowercase().contains(filter) {
            continue;
        }
        if filter.is_empty() && step != Some(l.step.as_str()) && !l.step.is_empty() {
            out.push((i, true));
        }
        step = Some(l.step.as_str());
        out.push((i, false));
    }
    out
}

impl App {
    pub(super) fn gh_draw_log(&mut self, cv: &mut Canvas, g: &mut GhPanel, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let Detail::Run(v) = &g.detail else { return };
        let Some(job) = v.job.and_then(|j| v.detail.ready().and_then(|d| d.jobs.get(j))).cloned() else { return };
        let failed_only = v.log_failed_only;
        let focus = v.log_filter_focus && g.focused;
        let filter_text = v.log_filter.text.clone();
        let cursor = v.log_filter.cursor;
        let sel_all = v.log_filter.sel_all;
        // Header: back, job, then the failed-only switch and copy.
        let head = Rect::new(body.x, body.y, body.w, 40);
        let back = Rect::new(body.x + 10, head.y + 6, 28, 28);
        self.gh_icon_btn(cv, g, back, "ui/arrow_left", GhHit::LogBack, false);
        let s = model::run_state(&job.status, &job.conclusion);
        let col = self.gh_color(s.tone);
        self.tx.icon(cv, phase_icon(s.phase), back.right() + 8, head.y + 12, 16, col, head);
        let mut right = head.right() - 10;
        let copy = Rect::new(right - 28, head.y + 6, 28, 28);
        self.gh_icon_btn(cv, g, copy, "ui/copy", GhHit::LogCopy, false);
        right -= 30;
        let toggle = Btn {
            label: "Failed steps".into(),
            icon: if failed_only { "ui/check" } else { "ui/filter" },
            hit: GhHit::LogFailed,
            style: if failed_only { BtnStyle::Primary } else { BtnStyle::Plain },
        };
        let tw = 12 + 16 + 6 + self.tx.ui_width(&toggle.label, false) + 12;
        self.gh_button(cv, g, right - tw, head.y + 6, &toggle, head);
        right -= tw + 6;
        let name_x = back.right() + 32;
        let name = fit(&mut self.tx, &job.name, (right - name_x).max(30), true);
        self.tx.draw_ui(cv, name_x, head.y + Self::gh_ty(40), &name, strong, head, true);
        // Filter.
        let f = Rect::new(body.x + 10, head.bottom(), body.w - 20, 30);
        cv.card(f, 7, c.widget_bg, if focus { c.accent } else { c.border });
        g.hits.push((f, GhHit::LogFilter));
        self.tx.icon(cv, "hugeicons/search", f.x + 8, f.y + 8, 14, dim, f);
        let tx0 = f.x + 29;
        let clip = Rect::new(f.x, f.y, f.w - 30, f.h);
        let has = !filter_text.is_empty();
        if has {
            if sel_all {
                let w = self.tx.ui_width(&filter_text, false);
                cv.fill_round_clip(Rect::new(tx0 - 2, f.y + 4, w + 4, f.h - 8), 4, c.accent.with_alpha(0x55), clip);
            }
            self.tx.draw_ui(cv, tx0, f.y + Self::gh_ty(f.h), &filter_text, self.theme.foreground, clip, false);
        } else {
            self.tx.draw_ui(cv, tx0, f.y + Self::gh_ty(f.h), "Filter log", dim, clip, false);
        }
        if focus {
            let prefix: String = filter_text.chars().take(cursor).collect();
            let cx = tx0 + self.tx.ui_width(&prefix, false);
            cv.fill_clip(Rect::new(cx, f.y + 7, 1, 16), self.theme.foreground, clip);
        }
        if has {
            let b = Rect::new(f.right() - 24, f.y + 5, 20, 20);
            if self.gh_hv(b) {
                cv.fill_round(b, 5, strong.with_alpha(0x18));
            }
            self.tx.icon(cv, "ui/close", b.x + 4, b.y + 4, 12, dim, b);
            g.hits.push((b, GhHit::LogFilterClear));
        }
        let area = Rect::new(body.x, f.bottom() + 8, body.w, (body.bottom() - f.bottom() - 8).max(0));
        cv.fill(Rect::new(area.x, area.y - 1, area.w, 1), c.border);
        let Detail::Run(v) = &g.detail else { return };
        let msg = match &v.log {
            Load::Loading | Load::Idle => Some(("Loading log…".to_string(), dim)),
            Load::Failed(e) => Some((e.clone(), c.error)),
            Load::Ready(l) if l.is_empty() => Some(("This job wrote no output".to_string(), dim)),
            Load::Ready(_) => None,
        };
        if let Some((m, col)) = msg {
            let w = self.tx.ui_width(&m, false);
            self.tx.draw_ui(cv, area.x + (area.w - w) / 2, area.y + 40, &m, col, area, false);
            return;
        }
        let Load::Ready(lines) = v.log.clone() else { return };
        let q = filter_text.trim().to_lowercase();
        if g.log_view.0 != lines.len() || g.log_view.1 != q {
            g.log_view = (lines.len(), q.clone(), view_rows(&lines, &q));
        }
        let rows = g.log_view.2.clone();
        if rows.is_empty() {
            let m = "No matching lines";
            let w = self.tx.ui_width(m, false);
            self.tx.draw_ui(cv, area.x + (area.w - w) / 2, area.y + 40, m, dim, area, false);
            return;
        }
        let lh = (crate::ui::line_h().ceil() as i32).max(16);
        let total = rows.len() as i32 * lh + 16;
        g.scroll[2] = g.scroll[2].clamp(0.0, (total - area.h).max(0) as f64);
        let first = ((g.scroll[2] as i32) / lh).max(0) as usize;
        let last = (first + (area.h / lh) as usize + 2).min(rows.len());
        let digits = lines.len().to_string().len().max(3) as i32;
        let num_w = digits * self.tx.cell_w.ceil() as i32 + 16;
        let text_x = area.x + num_w + 10;
        let cols = (((area.w - num_w - 24) as f32 / self.tx.cell_w.max(1.0)) as usize).max(1);
        let y0 = area.y + 8 - g.scroll[2] as i32;
        for (k, &(i, header)) in rows.iter().enumerate().take(last).skip(first) {
            let y = y0 + k as i32 * lh;
            let l = &lines[i];
            let rr = Rect::new(area.x, y, area.w, lh);
            if header {
                cv.fill_clip(rr, strong.with_alpha(0x0e), area);
                let t = fit(&mut self.tx, &l.step, area.w - num_w - 30, true);
                self.tx.draw_ui(
                    cv,
                    text_x,
                    y + ((lh - crate::ui::ui_line().round() as i32) / 2).max(0),
                    &t,
                    strong,
                    rr.intersect(&area),
                    true,
                );
                continue;
            }
            let (class, text) = classify(&l.text);
            let (fg, bg) = match class {
                LogClass::Error => (c.error, Some(c.error.with_alpha(0x18))),
                LogClass::Warning => (c.warning, Some(c.warning.with_alpha(0x14))),
                LogClass::Group => (strong, None),
                LogClass::Command => (c.accent_text, None),
                _ => (self.theme.foreground, None),
            };
            if let Some(bg) = bg {
                cv.fill_clip(rr, bg, area);
            }
            let num = format!("{:>w$}", i + 1, w = digits as usize);
            self.tx.draw_line(cv, area.x + 8, y, &num, dim.with_alpha(0xa0), rr.intersect(&area));
            let t: String = text.replace('\t', "    ").chars().take(cols).collect();
            self.tx.draw_line(cv, text_x, y, &t, fg, rr.intersect(&area));
        }
        let (pos, visible) = (g.scroll[2], area.h as f64);
        self.region_bar(cv, Reg::GhLog, area, total as f64, visible, pos, 1.0, false);
    }
}
