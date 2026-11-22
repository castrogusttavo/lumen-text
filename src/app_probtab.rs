//! The Problems tab of the bottom panel: diagnostics grouped by file in collapsible groups, one
//! severity icon and one line per problem, counts in chips, a scope switch (open files / whole
//! project) and a friendly "No problems found" state. A click on a problem jumps to its location,
//! a click on a file header folds or unfolds it.
//!
//! The rows come from `App::problem_groups` (the same data the floating picker lists), rebuilt by
//! `problems_rebuild` whenever the diagnostics change. Drawing is virtualized: only the rows in
//! view are laid out.

use crate::{
    anim::lerp_rgba,
    app::{App, Focus},
    app_gitpanel::fit,
    app_panel::PanelTab,
    app_problems::SCOPE_LABELS,
    ds,
    problems::FileGroup,
    theme::Rgba,
    tool_ui,
    ui::{Canvas, Rect, TextCtx},
    ui_empty::{Empty, draw_empty},
};
use std::{collections::HashSet, path::PathBuf};
use winit::event::MouseScrollDelta;

/// Height of the toolbar (counts and scope switch) above the list.
const TOOL_H: i32 = 38;
/// File header row and problem row heights.
const HEAD_H: i32 = ds::ROW_LIST;
const ROW_H: i32 = 26;
/// Chip metrics (the design system asks for 10-11 px text in pills).
const CHIP_H: i32 = 18;
const CHIP_FONT: f32 = 11.0;
const CHIP_LINE: i32 = 15;
/// The panel never shorter than this to be useful.
pub const MIN_TAB_H: i32 = 180;

/// What a click on the tab does.
#[derive(Clone, Debug, PartialEq)]
pub enum PHit {
    /// Fold / unfold a file.
    Group(PathBuf),
    /// Jump to line, column of a file.
    Open(PathBuf, usize, usize),
    /// Switch the scope (true = whole project).
    Scope(bool),
}

/// One visible row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flat {
    /// File header of group `g`.
    Group(usize),
    /// Problem `r` of group `g`.
    Row(usize, usize),
}

impl Flat {
    pub fn height(self) -> i32 {
        match self {
            Flat::Group(_) => HEAD_H,
            Flat::Row(..) => ROW_H,
        }
    }
}

/// The rows to show: every header, and the problems of the groups that are not folded.
pub fn flat_rows(groups: &[FileGroup], collapsed: &HashSet<PathBuf>) -> Vec<Flat> {
    let mut out = Vec::new();
    for (g, grp) in groups.iter().enumerate() {
        out.push(Flat::Group(g));
        if !collapsed.contains(&grp.path) {
            out.extend((0..grp.rows.len()).map(|r| Flat::Row(g, r)));
        }
    }
    out
}

/// Total height of the rows.
pub fn content_h(rows: &[Flat]) -> i32 {
    rows.iter().map(|f| f.height()).sum()
}

/// State of the tab (kept in `WsState`).
#[derive(Default)]
pub struct ProbTab {
    pub groups: Vec<FileGroup>,
    /// Problems the store dropped to stay bounded.
    pub dropped: usize,
    /// A workspace pull is in flight.
    pub checking: bool,
    pub collapsed: HashSet<PathBuf>,
    pub scroll: f64,
    max_scroll: f64,
    hits: Vec<(Rect, PHit)>,
}

impl ProbTab {
    /// (errors, warnings, infos) over every file.
    pub fn counts(&self) -> (usize, usize, usize) {
        self.groups.iter().fold((0, 0, 0), |a, g| (a.0 + g.errors, a.1 + g.warnings, a.2 + g.infos))
    }
}

/// Width of a count chip showing `n`.
fn count_chip_w(tx: &mut TextCtx, n: usize) -> i32 {
    ds::S2 + 12 + 4 + tx.ui_width_sized(&n.to_string(), false, CHIP_FONT) + ds::S2
}

/// A pill with a severity icon and a count; muted when the count is zero.
#[allow(clippy::too_many_arguments)]
fn draw_count_chip(
    cv: &mut Canvas,
    tx: &mut TextCtx,
    (x, y): (i32, i32),
    icon: &str,
    n: usize,
    col: Rgba,
    dim: Rgba,
    clip: Rect,
) -> Rect {
    let r = Rect::new(x, y, count_chip_w(tx, n), CHIP_H);
    let fg = if n > 0 { col } else { dim };
    cv.fill_round_clip(r, ds::R_CTRL, fg.with_alpha(if n > 0 { 0x24 } else { 0x12 }), clip);
    tx.icon(cv, icon, r.x + ds::S2, r.y + (CHIP_H - 12) / 2, 12, fg, clip);
    let text = n.to_string();
    tx.draw_ui_sized_i(
        cv,
        r.x + ds::S2 + 12 + 4,
        r.y + (CHIP_H - CHIP_LINE) / 2,
        &text,
        fg,
        clip,
        false,
        false,
        CHIP_FONT,
    );
    r
}

impl App {
    /// Is the Problems tab the one on screen?
    pub fn problems_tab_shown(&self) -> bool {
        self.panel.open && self.panel.tab == PanelTab::Problems
    }

    /// Show the tab (opening the panel when needed) in the configured scope.
    pub fn open_problems_tab(&mut self) {
        let project = self.problems_project();
        self.lspx.ws.asked.clear();
        if project {
            self.ws_pull_new();
        }
        if !self.panel.open {
            self.panel.motion.arm = true;
        }
        self.panel.open = true;
        self.panel.tab = PanelTab::Problems;
        self.panel.height = self.panel.height.max(MIN_TAB_H);
        self.focus = Focus::Editor;
        self.lspx.ws.tab.scroll = 0.0;
        self.problems_tab_refresh();
        self.lspx.ws.shown = Some(self.problems_signature());
        self.lspx.ws.built_at = Some(std::time::Instant::now());
        self.request_redraw();
    }

    /// Rebuild the tab's groups from the current diagnostics.
    pub(crate) fn problems_tab_refresh(&mut self) {
        let project = self.problems_project();
        let groups = self.problem_groups(project);
        let dropped = if project { self.lspx.problems.dropped } else { 0 };
        let checking = project && !self.lspx.ws.inflight.is_empty();
        let t = &mut self.lspx.ws.tab;
        t.groups = groups;
        t.dropped = dropped;
        t.checking = checking;
        // Forget folds of files that no longer have problems.
        let live: HashSet<&PathBuf> = t.groups.iter().map(|g| &g.path).collect();
        t.collapsed.retain(|p| live.contains(p));
    }

    /// Per frame: true while the loading shimmer needs more frames.
    pub fn problems_frame(&mut self) -> bool {
        let t = &self.lspx.ws.tab;
        self.problems_tab_shown() && t.checking && t.groups.is_empty() && !crate::motion::reduced()
    }

    pub fn problems_tab_wheel(&mut self, delta: MouseScrollDelta) {
        let w = self.wheel_in(delta);
        let px = if w.pixels { w.dy } else { w.dy * ROW_H as f64 * 2.0 };
        let t = &mut self.lspx.ws.tab;
        t.scroll = (t.scroll + px).clamp(0.0, t.max_scroll);
    }

    /// Is there something clickable under (x, y)? (pointer cursor; for `cursor_for`)
    #[allow(dead_code)]
    pub fn problems_clickable_at(&self, x: f64, y: f64) -> bool {
        self.problems_tab_shown() && self.lspx.ws.tab.hits.iter().any(|(r, _)| r.contains(x, y))
    }

    pub fn problems_tab_click(&mut self, x: f64, y: f64) {
        let hit = self.lspx.ws.tab.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
        match hit {
            Some(PHit::Group(p)) => {
                let t = &mut self.lspx.ws.tab;
                if !t.collapsed.remove(&p) {
                    t.collapsed.insert(p);
                }
            }
            Some(PHit::Open(p, line, col)) => {
                self.focus = Focus::Editor;
                self.open_path(&p, Some((line, col)));
            }
            Some(PHit::Scope(project)) => self.set_problems_scope(project),
            None => {}
        }
    }

    // ───────────── drawing ─────────────

    pub fn draw_problems_tab(&mut self, cv: &mut Canvas, body: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (mx, my) = self.mouse;
        let free = self.overlay.is_none();
        // Last frame's regions give the hover of the scope switch before it is laid out again.
        let old = std::mem::take(&mut self.lspx.ws.tab.hits);
        let hover = old.iter().find_map(|(r, h)| match h {
            PHit::Scope(p) if free && r.contains(mx, my) => Some(*p as usize),
            _ => None,
        });
        let project = self.problems_project();
        let bar = Rect::new(body.x, body.y, body.w, TOOL_H);
        let area = Rect::new(body.x, body.y + TOOL_H, body.w, (body.h - TOOL_H).max(0));
        let mut hits: Vec<(Rect, PHit)> = Vec::new();

        // Toolbar: counts on the left, scope switch on the right.
        let (e, w, i) = self.lspx.ws.tab.counts();
        let cy = body.y + (TOOL_H - CHIP_H) / 2;
        let mut x = body.x + ds::S3;
        for (n, icon, col) in [(e, "ui/x_circle", c.error), (w, "ui/warning", c.warning), (i, "ui/info", c.info)] {
            let r = draw_count_chip(cv, &mut self.tx, (x, cy), icon, n, col, dim, bar);
            x = r.right() + ds::S2;
        }
        let segs = cv.seg_control(
            &mut self.tx,
            &c,
            (body.right() - ds::S3, body.y + (TOOL_H - ds::HIT_MIN) / 2),
            &SCOPE_LABELS,
            project as usize,
            hover,
        );
        for (k, r) in segs.iter().enumerate() {
            hits.push((*r, PHit::Scope(k == 1)));
        }
        let status = {
            let t = &self.lspx.ws.tab;
            if t.checking {
                "checking the project…".to_string()
            } else if t.dropped > 0 {
                format!("{} not shown", t.dropped)
            } else {
                String::new()
            }
        };
        let room = segs.first().map_or(body.right(), |r| r.x) - x - ds::S4;
        if !status.is_empty() && room > 40 {
            let s = fit(&mut self.tx, &status, room, false);
            self.tx.draw_ui(cv, x + ds::S1, body.y + (TOOL_H - 18) / 2, &s, dim, bar, false);
        }
        cv.fill(Rect::new(body.x, body.y + TOOL_H - 1, body.w, 1), c.border);

        if self.lspx.ws.tab.groups.is_empty() {
            if self.lspx.ws.tab.checking {
                let pad = Rect::new(area.x + ds::S4, area.y + ds::S4, (area.w - 2 * ds::S4).max(0), area.h);
                cv.skeleton_lines(&self.theme, pad, 3, area, tool_ui::clock());
            } else {
                let hint = if project { "Nothing to fix in the project" } else { "Nothing to fix in the open files" };
                let e = Empty {
                    icon: "ui/check",
                    title: "No problems found",
                    hint,
                    action: (!project).then_some("Check the whole project"),
                };
                if let Some(btn) = draw_empty(&mut self.tx, cv, &self.theme, area, &e, (mx, my), 1.0) {
                    hits.push((btn, PHit::Scope(true)));
                }
            }
            self.lspx.ws.tab.max_scroll = 0.0;
            self.lspx.ws.tab.scroll = 0.0;
            self.lspx.ws.tab.hits = hits;
            return;
        }

        let groups = std::mem::take(&mut self.lspx.ws.tab.groups);
        let collapsed = std::mem::take(&mut self.lspx.ws.tab.collapsed);
        let flat = flat_rows(&groups, &collapsed);
        let total = (content_h(&flat) + 2 * ds::S1) as f64;
        let max_scroll = (total - area.h as f64).max(0.0);
        let scroll = self.lspx.ws.tab.scroll.clamp(0.0, max_scroll);
        let row_w = (area.w - 2 * ds::S2 - if max_scroll > 0.0 { 6 } else { 0 }).max(0);
        let mut y = area.y + ds::S1 - scroll as i32;
        for f in &flat {
            let h = f.height();
            if y + h > area.y && y < area.bottom() {
                let row = Rect::new(area.x + ds::S2, y, row_w, h);
                let hot = free && row.contains(mx, my) && area.contains(mx, my);
                if hot {
                    cv.fill_round_clip(row, ds::R_CTRL, c.hover_overlay, area);
                }
                match *f {
                    Flat::Group(g) => {
                        let grp = &groups[g];
                        let fold = collapsed.contains(&grp.path);
                        self.draw_problem_head(cv, area, row, grp, fold, hot);
                        hits.push((row.intersect(&area), PHit::Group(grp.path.clone())));
                    }
                    Flat::Row(g, r) => {
                        let grp = &groups[g];
                        let pr = &grp.rows[r];
                        let (icon, col) = match pr.severity {
                            1 => ("ui/x_circle", c.error),
                            2 => ("ui/warning", c.warning),
                            _ => ("ui/info", c.info),
                        };
                        let ix = row.x + ds::S2 + 18;
                        self.tx.icon(cv, icon, ix, row.y + (h - ds::ICON_S) / 2, ds::ICON_S, col, area);
                        let mut loc = format!("{}:{}", pr.line + 1, pr.col + 1);
                        if !pr.source.is_empty() {
                            loc = format!("{loc} · {}", pr.source);
                        }
                        let lw = self.tx.ui_width(&loc, false).min(row.w / 3);
                        let loc = fit(&mut self.tx, &loc, lw, false);
                        let lw = self.tx.ui_width(&loc, false);
                        let ty = row.y + (h - 18) / 2;
                        self.tx.draw_ui(cv, row.right() - ds::S3 - lw, ty, &loc, dim, area, false);
                        let mx0 = ix + ds::ICON_S + ds::S2;
                        let avail = (row.right() - ds::S3 - lw - ds::S3 - mx0).max(0);
                        let msg = fit(&mut self.tx, &pr.message, avail, false);
                        let fg = if hot { strong } else { lerp_rgba(dim, strong, 0.7) };
                        self.tx.draw_ui(cv, mx0, ty, &msg, fg, area, false);
                        hits.push((row.intersect(&area), PHit::Open(grp.path.clone(), pr.line, pr.col)));
                    }
                }
            }
            y += h;
        }
        self.lspx.ws.tab.groups = groups;
        self.lspx.ws.tab.collapsed = collapsed;
        self.lspx.ws.tab.max_scroll = max_scroll;
        self.lspx.ws.tab.scroll = scroll;
        self.lspx.ws.tab.hits = hits;
        if max_scroll > 0.0 {
            let th = ((area.h as f64 * area.h as f64 / total) as i32).max(24);
            let ty = area.y + ((area.h - th) as f64 * scroll / max_scroll) as i32;
            cv.fill_round_clip(Rect::new(area.right() - 6, ty, 3, th), 1, strong.with_alpha(0x30), area);
        }
    }

    /// File header: chevron, name, folder, and the counts at the right.
    fn draw_problem_head(&mut self, cv: &mut Canvas, area: Rect, row: Rect, g: &FileGroup, fold: bool, hot: bool) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let chev = if fold { "ui/chevron_right" } else { "ui/chevron_down" };
        self.tx.icon(
            cv,
            chev,
            row.x + ds::S2,
            row.y + (row.h - ds::ICON_S) / 2,
            ds::ICON_S,
            if hot { strong } else { dim },
            area,
        );
        // Counts, right to left.
        let cy = row.y + (row.h - CHIP_H) / 2;
        let mut right = row.right() - ds::S2;
        for (n, icon, col) in
            [(g.infos, "ui/info", c.info), (g.warnings, "ui/warning", c.warning), (g.errors, "ui/x_circle", c.error)]
        {
            if n == 0 {
                continue;
            }
            let w = count_chip_w(&mut self.tx, n);
            draw_count_chip(cv, &mut self.tx, (right - w, cy), icon, n, col, dim, area);
            right -= w + ds::S1;
        }
        let rel = self.rel(&g.path).replace('\\', "/");
        let (dir, name) = match rel.rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), rel.clone()),
        };
        let nx = row.x + ds::S2 + ds::ICON_S + ds::S2;
        let avail = (right - ds::S3 - nx).max(0);
        let name = fit(&mut self.tx, &name, avail, true);
        let ty = row.y + (row.h - 18) / 2;
        self.tx.draw_ui(cv, nx, ty, &name, strong, area, true);
        let nw = self.tx.ui_width(&name, true);
        let rest = avail - nw - ds::S2;
        if !dir.is_empty() && rest > 40 {
            let dir = fit(&mut self.tx, &dir, rest, false);
            self.tx.draw_ui(cv, nx + nw + ds::S2, ty, &dir, dim, area, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::problems::{Row, group};

    fn row(line: usize, sev: u8) -> Row {
        Row { line, col: 0, severity: sev, message: format!("m{line}"), source: String::new() }
    }

    fn two_files() -> Vec<FileGroup> {
        group(vec![
            (PathBuf::from("a.rs"), row(1, 1)),
            (PathBuf::from("a.rs"), row(2, 2)),
            (PathBuf::from("b.rs"), row(3, 2)),
        ])
    }

    #[test]
    fn folded_groups_keep_only_their_header() {
        let g = two_files();
        assert_eq!(g[0].path, PathBuf::from("a.rs"), "the file with errors comes first");
        let open = flat_rows(&g, &HashSet::new());
        assert_eq!(open.len(), 5, "2 headers + 3 problems");
        let mut folded = HashSet::new();
        folded.insert(PathBuf::from("a.rs"));
        let rows = flat_rows(&g, &folded);
        assert_eq!(rows, vec![Flat::Group(0), Flat::Group(1), Flat::Row(1, 0)]);
        assert_eq!(content_h(&rows), 2 * HEAD_H + ROW_H);
    }

    #[test]
    fn counts_add_up_over_files() {
        let t = ProbTab { groups: two_files(), ..Default::default() };
        assert_eq!(t.counts(), (1, 2, 0));
        assert_eq!(ProbTab::default().counts(), (0, 0, 0));
    }

    #[test]
    fn heights_follow_the_kind_of_row() {
        assert_eq!(Flat::Group(0).height(), HEAD_H);
        assert_eq!(Flat::Row(0, 0).height(), ROW_H);
        assert_eq!(content_h(&[]), 0);
    }
}
