//! The 3-way merge editor ("Git: Open merge editor"): the focused pane is split like the Markdown
//! reader splits it, the read-only Incoming (theirs) | Current (ours) panes on top and the result
//! below. The result is the real document (the conflicted file itself), so every editor feature
//! works in it and Ctrl+Z undoes a choice. Choices replace a conflict's lines in that document;
//! where each conflict sits is found again from the text after every edit (`merge3::locate`), so
//! typing anywhere never confuses the view. The panes scroll with the result.
//!
//! Merge logic, git stages and "mark resolved" live in `merge3.rs`; this file is the App glue.

use crate::{
    app::App,
    merge3::{self, Kind, Merge, Row, State, Take},
    theme::Rgba,
    ui::{Canvas, Rect, line_h},
};
use std::{
    ops::Range,
    path::{Path, PathBuf},
};
use winit::event::MouseScrollDelta;

/// Header (file, counter, buttons), pane titles and the "Result" strip above the document.
const HEAD_H: i32 = 34;
const TITLE_H: i32 = 24;
const FOOT_H: i32 = 24;
/// The pane needs this much height before the merge panes take part of it.
const MIN_AREA_H: i32 = 260;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeBtn {
    Prev,
    Next,
    Resolve,
    Close,
    /// The Incoming / Current checkbox of conflict `i` (index into `MergeView::conflicts`).
    Incoming(usize),
    Current(usize),
    /// Click elsewhere on a conflict's rows: make it the current one.
    Goto(usize),
}

pub struct MergeView {
    pub path: PathBuf,
    pub top: PathBuf,
    pub rel: String,
    pub name: String,
    /// "merge", "rebase", "cherry-pick" (what to finish once every file is resolved).
    pub op: &'static str,
    pub m: Merge,
    /// Region indexes of the conflicts.
    pub conflicts: Vec<usize>,
    /// Marker labels (current, incoming) used when a conflict goes back to markers.
    pub labels: (String, String),
    /// Pane titles (incoming, current).
    pub titles: (String, String),
    pub rows: Vec<Row>,
    pub spans: Vec<Range<usize>>,
    /// Document version `loc` / `states` describe.
    pub ver: Option<i32>,
    pub loc: Vec<Range<usize>>,
    pub states: Vec<State>,
    /// The result still holds conflict markers somewhere ("Mark resolved" is disabled).
    pub markers: bool,
    /// Current conflict (index into `conflicts`).
    pub cur: usize,
    /// First pane row shown (fractional while gliding with the document).
    pub scroll: f64,
    /// Document scroll the panes were last aligned to.
    pub last_top: f64,
    /// Where the panes draw this frame (empty while another tab is active).
    pub area: Rect,
    pub btns: Vec<(Rect, MergeBtn)>,
}

/// What a click on a conflict's checkbox does, given what the conflict holds: checking the second
/// side keeps both in the order they were checked, unchecking the only side brings the markers back.
pub fn toggled(state: State, incoming: bool) -> Take {
    match (incoming, state) {
        (true, State::Incoming) | (false, State::Current) => Take::Markers,
        (true, State::CurrentIncoming | State::IncomingCurrent) => Take::Current,
        (false, State::CurrentIncoming | State::IncomingCurrent) => Take::Incoming,
        (true, State::Current) => Take::CurrentIncoming,
        (false, State::Incoming) => Take::IncomingCurrent,
        (true, _) => Take::Incoming,
        (false, _) => Take::Current,
    }
}

pub fn state_label(s: State) -> &'static str {
    match s {
        State::Unresolved => "unresolved",
        State::Current => "current taken",
        State::Incoming => "incoming taken",
        State::CurrentIncoming => "both taken (current first)",
        State::IncomingCurrent => "both taken (incoming first)",
        State::Base => "base kept (neither side)",
        State::Manual => "edited by hand",
    }
}

impl MergeView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        top: PathBuf,
        rel: String,
        path: PathBuf,
        stages: (String, String, String),
        op: &'static str,
        labels: (String, String),
        titles: (String, String),
    ) -> MergeView {
        let m = Merge::new(&stages.0, &stages.1, &stages.2);
        let conflicts = m.conflicts();
        let (rows, spans) = m.rows();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        MergeView {
            path,
            top,
            rel,
            name,
            op,
            m,
            conflicts,
            labels,
            titles,
            rows,
            spans,
            ver: None,
            loc: Vec::new(),
            states: Vec::new(),
            markers: false,
            cur: 0,
            scroll: 0.0,
            last_top: f64::NAN,
            area: Rect::default(),
            btns: Vec::new(),
        }
    }

    pub fn resolved(&self) -> usize {
        self.states.iter().filter(|s| s.resolved()).count()
    }

    /// Find the regions in the result again (document version `ver`, text `text`).
    pub fn refresh(&mut self, ver: i32, text: &str) {
        self.ver = Some(ver);
        let (loc, states) = self.m.states(text);
        self.loc = loc;
        self.states = states;
        self.markers = merge3::has_markers(text);
    }

    /// The whole result with conflict `ci` replaced by `take` (`refresh` must describe `text`).
    pub fn apply(&self, text: &str, ci: usize, take: Take) -> String {
        let Some(&k) = self.conflicts.get(ci) else { return text.to_string() };
        let r = self.loc.get(k).cloned().unwrap_or(0..0);
        let lines = merge3::split_lines(text);
        let (a, b) = (r.start.min(lines.len()), r.end.min(lines.len()));
        let mut s: String = lines[..a].concat();
        if !s.is_empty() && !s.ends_with('\n') {
            s.push_str(self.m.eol);
        }
        s.push_str(&self.m.take_text(k, take, (&self.labels.0, &self.labels.1)));
        s.push_str(&lines[b..].concat());
        s
    }

    /// Result line -> pane row, and back.
    pub fn row_of_line(&self, line: f64) -> f64 {
        merge3::map_pos(&self.loc, &self.spans, line)
    }

    pub fn line_of_row(&self, row: f64) -> f64 {
        merge3::map_pos(&self.spans, &self.loc, row)
    }

    fn rows_h(&self) -> i32 {
        (self.area.h - HEAD_H - TITLE_H - FOOT_H).max(0)
    }

    fn max_scroll(&self) -> f64 {
        (self.rows.len() as f64 - (self.rows_h() / line_h() as i32) as f64).max(0.0)
    }
}

/// Marker labels, pane titles and the operation in progress in the repository at `top`.
fn labels_for(top: &Path) -> (&'static str, (String, String), (String, String)) {
    use crate::app_gitpanel::{RepoState, current_branch, git_dir, git_run, repo_state};
    let short = |r: &str| git_run(top, &["rev-parse", "--short", r]).ok().map(|s| s.trim().to_string());
    let subject = |r: &str| {
        git_run(top, &["log", "-1", "--format=%s", r])
            .ok()
            .map(|s| s.trim().chars().take(48).collect::<String>())
            .filter(|s| !s.is_empty())
    };
    let head = current_branch(top).or_else(|| short("HEAD")).unwrap_or_else(|| "HEAD".into());
    match repo_state(top) {
        RepoState::Merge => {
            // "Merge branch 'feat'" names what comes in better than a hash.
            let msg = git_dir(top).and_then(|d| std::fs::read_to_string(d.join("MERGE_MSG")).ok()).unwrap_or_default();
            let named = msg.lines().next().and_then(|l| l.split('\'').nth(1)).map(str::to_string);
            let inc = named.or_else(|| short("MERGE_HEAD")).unwrap_or_else(|| "MERGE_HEAD".into());
            ("merge", ("HEAD".into(), inc.clone()), (format!("Incoming · {inc}"), format!("Current · {head}")))
        }
        RepoState::Rebase => {
            let inc = short("REBASE_HEAD").unwrap_or_else(|| "REBASE_HEAD".into());
            let what = subject("REBASE_HEAD").map_or(inc.clone(), |s| format!("{inc} {s}"));
            (
                "rebase",
                ("HEAD".into(), inc),
                (format!("Incoming · replaying {what}"), format!("Current · rebasing onto {head}")),
            )
        }
        RepoState::CherryPick => {
            let inc = short("CHERRY_PICK_HEAD").unwrap_or_else(|| "CHERRY_PICK_HEAD".into());
            let what = subject("CHERRY_PICK_HEAD").map_or(inc.clone(), |s| format!("{inc} {s}"));
            ("cherry-pick", ("HEAD".into(), inc), (format!("Incoming · {what}"), format!("Current · {head}")))
        }
        _ => ("", ("HEAD".into(), "incoming".into()), ("Incoming (theirs)".into(), format!("Current · {head}"))),
    }
}

impl App {
    /// "Git: Open merge editor" on the active file.
    pub fn merge_open_active(&mut self) {
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf)) else {
            return self.say("open a conflicted file first");
        };
        self.merge_open(&path);
    }

    /// Open the merge editor on conflicted file `path` (opening its tab if needed).
    pub fn merge_open(&mut self, path: &Path) {
        let path = crate::paths::canonical(path).unwrap_or_else(|_| path.to_path_buf());
        let Some(top) = path.parent().and_then(crate::git::toplevel) else {
            return self.say("the file is not in a git repository");
        };
        let top = crate::paths::canonical(&top).unwrap_or(top);
        let Ok(rel) = path.strip_prefix(&top).map(|r| r.to_string_lossy().replace('\\', "/")) else {
            return self.say("the file is outside its repository");
        };
        let stages = match merge3::stages(&top, &rel) {
            Ok(s) => s,
            Err(e) => return self.say(e),
        };
        let (op, labels, titles) = labels_for(&top);
        if self.open_path(&path, None).is_none() {
            return;
        }
        self.diff = None; // it would cover the result
        self.merge = Some(Box::new(MergeView::new(top, rel, path, stages, op, labels, titles)));
        self.merge_sync();
        let Some(mv) = self.merge.as_ref() else { return };
        let (n, first) = (mv.conflicts.len(), mv.states.iter().position(|s| !s.resolved()).unwrap_or(0));
        let name = mv.name.clone();
        if n > 0 {
            self.merge_goto(first);
        }
        self.say(match n {
            0 => format!("{name}: git's conflict is fully merged here — check the result and mark it resolved"),
            1 => format!("{name}: 1 conflict"),
            _ => format!("{name}: {n} conflicts"),
        });
    }

    pub fn merge_close(&mut self) {
        if self.merge.take().is_some() {
            self.say("merge editor closed");
        }
    }

    /// Index of the document the merge editor works on.
    fn merge_doc(&self) -> Option<usize> {
        let mv = self.merge.as_ref()?;
        self.docs.iter().position(|d| d.path() == Some(mv.path.as_path()))
    }

    /// Bring the view up to date with the result document; its index.
    fn merge_sync(&mut self) -> Option<usize> {
        let Some(i) = self.merge_doc() else {
            self.merge = None; // its tab was closed
            return None;
        };
        let ver = self.docs[i].version;
        if let Some(mv) = self.merge.as_mut()
            && mv.ver != Some(ver)
        {
            let text = self.docs[i].buf.to_string();
            mv.refresh(ver, &text);
        }
        Some(i)
    }

    /// Put conflict `ci` (index into the conflicts) to `take` in the result.
    pub fn merge_take(&mut self, ci: usize, take: Take) {
        let Some(i) = self.merge_sync() else { return };
        let Some(mv) = self.merge.as_mut() else { return };
        if ci >= mv.conflicts.len() {
            return;
        }
        let text = self.docs[i].buf.to_string();
        let new = mv.apply(&text, ci, take);
        mv.cur = ci;
        self.docs[i].replace_text(&new);
        self.merge_sync();
        self.request_redraw();
    }

    /// Click on conflict `ci`'s Incoming / Current checkbox.
    pub fn merge_toggle(&mut self, ci: usize, incoming: bool) {
        self.merge_sync();
        let Some(st) = self.merge.as_ref().and_then(|mv| mv.states.get(ci).copied()) else { return };
        self.merge_take(ci, toggled(st, incoming));
    }

    /// Make conflict `ci` the current one and show it in both the panes and the result.
    pub fn merge_goto(&mut self, ci: usize) {
        let Some(i) = self.merge_sync() else { return };
        let Some(mv) = self.merge.as_mut() else { return };
        let Some(&k) = mv.conflicts.get(ci) else { return };
        mv.cur = ci;
        let line = mv.loc.get(k).map_or(0, |r| r.start);
        let d = &mut self.docs[i];
        let p = d.buf.pos(line.min(d.buf.line_count().saturating_sub(1)), 0);
        d.ed.click(&mut d.buf, p, false);
        let t = (d.row_of_line(line) as f64 - 3.0).clamp(0.0, d.max_top());
        d.top = t;
        d.target = t;
        d.animating = false;
        mv.scroll = (mv.spans[k].start as f64 - 3.0).clamp(0.0, mv.max_scroll());
        mv.last_top = t;
        self.request_redraw();
    }

    /// Next / previous conflict (wrapping).
    pub fn merge_step(&mut self, next: bool) {
        let Some(mv) = self.merge.as_ref() else { return self.say("the merge editor is not open") };
        let n = mv.conflicts.len();
        if n == 0 {
            return;
        }
        let ci = if next { (mv.cur + 1) % n } else { (mv.cur + n - 1) % n };
        self.merge_goto(ci);
    }

    /// "Mark resolved": save the result and stage it, once no conflict markers are left.
    pub fn merge_mark_resolved(&mut self) {
        let Some(i) = self.merge_sync() else { return self.say("the merge editor is not open") };
        let Some(mv) = self.merge.as_ref() else { return };
        if mv.markers {
            let left = mv.states.iter().filter(|s| !s.resolved()).count().max(1);
            return self.say(format!(
                "conflict markers remain ({left} unresolved) — resolve them before marking the file resolved"
            ));
        }
        let (top, rel, name, op) = (mv.top.clone(), mv.rel.clone(), mv.name.clone(), mv.op);
        if self.docs[i].buf.is_dirty()
            && let Err(e) = self.save_doc(i)
        {
            return self.say(e);
        }
        let text = self.docs[i].buf.to_string();
        if let Err(e) = merge3::mark_resolved(&top, &rel, &text) {
            return self.say(format!("git: {e}"));
        }
        self.merge = None;
        self.docs[i].git_stale = true;
        self.refresh_git_status();
        if let Some(gp) = self.gitpanel.as_mut() {
            gp.refresh();
        }
        let left = merge3::unmerged(&top).len();
        let then = if op.is_empty() { "commit".to_string() } else { format!("continue the {op}") };
        self.say(match left {
            0 => format!("{name} resolved and staged — no conflicts left: {then}"),
            1 => format!("{name} resolved and staged — 1 conflicted file left"),
            n => format!("{name} resolved and staged — {n} conflicted files left"),
        });
    }

    /// Give the top part of `area` (the card or the focused pane) to the merge panes when the merge
    /// editor's file is the active one. Returns what is left for the document.
    pub fn merge_place(&mut self, area: Rect) -> Rect {
        let active = self.merge_doc().is_some_and(|i| i == self.active);
        let diff_here = self.diff.is_some() && self.diff_pane_focused();
        let Some(mv) = self.merge.as_mut() else { return area };
        mv.area = Rect::default();
        if !active || diff_here || area.h < MIN_AREA_H {
            return area;
        }
        let least = HEAD_H + TITLE_H + FOOT_H + 3 * line_h() as i32;
        let top_h = ((area.h as f32 * 0.48) as i32).max(least).min(area.h - 100);
        mv.area = Rect::new(area.x, area.y, area.w, top_h);
        let rest = Rect::new(area.x, area.y + top_h, area.w, area.h - top_h);
        self.layout.editor = rest;
        rest
    }

    /// Mouse press: the merge panes' buttons, checkboxes and conflict rows. True when consumed.
    pub fn merge_mouse(&mut self, x: f64, y: f64) -> bool {
        let Some(mv) = self.merge.as_ref() else { return false };
        if !mv.area.contains(x, y) {
            return false;
        }
        self.focus = crate::app::Focus::Editor;
        let Some(&(_, b)) = mv.btns.iter().find(|(r, _)| r.contains(x, y)) else { return true };
        match b {
            MergeBtn::Prev => self.merge_step(false),
            MergeBtn::Next => self.merge_step(true),
            MergeBtn::Resolve => self.merge_mark_resolved(),
            MergeBtn::Close => self.merge_close(),
            MergeBtn::Incoming(ci) => self.merge_toggle(ci, true),
            MergeBtn::Current(ci) => self.merge_toggle(ci, false),
            MergeBtn::Goto(ci) => self.merge_goto(ci),
        }
        true
    }

    /// A button of the merge panes under the pointer (hand cursor).
    pub fn merge_btn_at(&self, x: f64, y: f64) -> bool {
        self.merge
            .as_ref()
            .is_some_and(|mv| mv.btns.iter().any(|(r, b)| r.contains(x, y) && !matches!(b, MergeBtn::Goto(_))))
    }

    /// Wheel over the panes: they scroll, and the result follows.
    pub fn merge_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        if !self.merge.as_ref().is_some_and(|mv| mv.area.contains(x, y)) {
            return false;
        }
        let Some(i) = self.merge_sync() else { return false };
        let Some(mv) = self.merge.as_mut() else { return false };
        let rows = crate::scroller::wheel_rows(delta, line_h() as f64);
        mv.scroll = (mv.scroll + rows).clamp(0.0, mv.max_scroll());
        let line = mv.line_of_row(mv.scroll);
        let d = &mut self.docs[i];
        let l = (line.floor().max(0.0) as usize).min(d.buf.line_count().saturating_sub(1));
        let t = (d.row_of_line(l) as f64 + line.fract()).clamp(0.0, d.max_top());
        d.top = t;
        d.target = t;
        d.animating = false;
        mv.last_top = t;
        true
    }

    pub fn draw_merge(&mut self, cv: &mut Canvas) {
        let Some(i) = self.merge_sync() else { return };
        let Some(mv) = self.merge.as_mut() else { return };
        let area = mv.area;
        if area.w <= 0 || area.h <= 0 {
            return;
        }
        // Follow the result's scroll (unless the panes were the ones scrolled).
        let d = &self.docs[i];
        if (d.top - mv.last_top).abs() > 0.01 || mv.last_top.is_nan() {
            let (line, _) = d.top_pos();
            mv.scroll = mv.row_of_line(line as f64 + d.top.fract()).clamp(0.0, mv.max_scroll());
            mv.last_top = d.top;
        }
        let c = self.theme.chrome;
        let (fg, dim, bg) = (self.theme.foreground, c.tab_inactive_fg, self.theme.background);
        let (col_in, col_cur) = (c.git_added, c.info);
        let (mx, my) = self.mouse;
        let mut btns: Vec<(Rect, MergeBtn)> = Vec::new();
        cv.fill(area, bg);

        // ── header: file, counter, buttons ──
        let head = Rect::new(area.x, area.y, area.w, HEAD_H);
        cv.fill(head, c.sidebar_bg);
        cv.fill(Rect::new(area.x, head.bottom() - 1, area.w, 1), c.border);
        let mut bx = head.right() - 8;
        // Close.
        let close = Rect::new(bx - 26, head.y + 4, 26, 26);
        if close.contains(mx, my) {
            cv.fill_round(close, 6, fg.with_alpha(0x22));
        }
        self.tx.icon(cv, "ui/close", close.x + 7, close.y + 7, 12, dim, head);
        btns.push((close, MergeBtn::Close));
        bx = close.x - 8;
        // Mark resolved: accent while it can be done, quiet while markers remain.
        let mv = self.merge.as_ref().expect("merge view");
        let can = !mv.markers;
        let label = "Mark resolved";
        let w = self.tx.ui_width(label, true) + 36;
        let rb = Rect::new(bx - w, head.y + 4, w, 26);
        let hov = can && rb.contains(mx, my);
        if can {
            cv.fill_round(rb, 7, if hov { c.accent.with_alpha(0xe0).over(c.sidebar_bg) } else { c.accent });
        } else {
            cv.fill_round(rb, 7, fg.with_alpha(0x0c).over(c.sidebar_bg));
            cv.ring(rb, 7, c.border);
        }
        let tint = if can { c.on_accent } else { dim };
        self.tx.icon(cv, "ui/check", rb.x + 10, rb.y + 7, 12, tint, rb);
        self.tx.draw_ui(cv, rb.x + 28, rb.y + 4, label, tint, rb, true);
        btns.push((rb, MergeBtn::Resolve));
        bx = rb.x - 10;
        // Next / Previous.
        for (label, icon, btn) in
            [("Next", "ui/chevron_down", MergeBtn::Next), ("Previous", "ui/chevron_up", MergeBtn::Prev)]
        {
            let w = self.tx.ui_width(label, false) + 34;
            let r = Rect::new(bx - w, head.y + 4, w, 26);
            if r.contains(mx, my) {
                cv.fill_round(r, 7, fg.with_alpha(0x1c));
            }
            self.tx.icon(cv, icon, r.x + 8, r.y + 7, 12, fg, r);
            self.tx.draw_ui(cv, r.x + 24, r.y + 4, label, fg, r, false);
            btns.push((r, btn));
            bx = r.x - 4;
        }
        // Left: icon, name, counter.
        let mv = self.merge.as_ref().expect("merge view");
        let room = Rect::new(head.x, head.y, (bx - 8 - head.x).max(0), HEAD_H);
        self.tx.icon(cv, "ui/git_merge_conflict", head.x + 12, head.y + 9, 16, c.warning, room);
        let name = crate::ui::fit_ellipsis(&mut self.tx, &mv.name, (room.w - 40).max(0), true);
        let nw = self.tx.ui_width(&name, true);
        self.tx.draw_ui(cv, head.x + 36, head.y + 8, &name, fg, room, true);
        let mv = self.merge.as_ref().expect("merge view");
        let (n, done) = (mv.conflicts.len(), mv.resolved());
        let counter = if n == 0 {
            "no conflicts left".to_string()
        } else {
            format!("{done} of {n} conflict{} resolved", if n == 1 { "" } else { "s" })
        };
        let ccol = if done == n && !mv.markers { c.git_added } else { dim };
        self.tx.draw_ui(cv, head.x + 36 + nw + 12, head.y + 8, &counter, ccol, room, false);

        // ── pane titles ──
        let mv = self.merge.as_ref().expect("merge view");
        let half = area.w / 2;
        let ty = head.bottom();
        let (lt, rt) = (Rect::new(area.x, ty, half, TITLE_H), Rect::new(area.x + half, ty, area.w - half, TITLE_H));
        let titles = mv.titles.clone();
        for (r, title, col) in [(lt, &titles.0, col_in), (rt, &titles.1, col_cur)] {
            cv.fill_round(Rect::new(r.x + 12, r.y + TITLE_H / 2 - 4, 8, 8), 4, col);
            let t = crate::ui::fit_ellipsis(&mut self.tx, title, (r.w - 40).max(0), false);
            self.tx.draw_ui_sized(cv, r.x + 26, r.y + 3, &t, dim, r, false, 12.0);
        }
        cv.fill(Rect::new(area.x, ty + TITLE_H - 1, area.w, 1), c.border);

        // ── aligned panes ──
        let body_y = ty + TITLE_H;
        let body_h = (area.bottom() - FOOT_H - body_y).max(0);
        let (lp_r, rp_r) =
            (Rect::new(area.x, body_y, half, body_h), Rect::new(area.x + half, body_y, area.w - half, body_h));
        cv.fill(Rect::new(area.x + half, body_y, 1, body_h), c.border);
        let lh = line_h() as i32;
        let mv = self.merge.as_ref().expect("merge view");
        let first = mv.scroll.max(0.0) as usize;
        let off = ((mv.scroll.max(0.0) - first as f64) * lh as f64).round() as i32;
        let n_rows = (body_h / lh + 2) as usize;
        let cell = self.tx.cell_w;
        let digits = mv.m.ours.len().max(mv.m.theirs.len()).to_string().len().max(3);
        let num_w = ((digits + 1) as f32 * cell) as i32;
        let max_cols = (((half - num_w - 16) as f32 / cell) as usize).max(4);
        let cur_region = mv.conflicts.get(mv.cur).copied();
        // Conflict index of each region.
        let ci_of = |k: usize| mv.conflicts.iter().position(|&c| c == k);
        let mut lp: Vec<(String, Rgba)> = Vec::new();
        let mut rp: Vec<(String, Rgba)> = Vec::new();
        let mut nums: Vec<(i32, i32, String)> = Vec::new();
        for (r, row) in mv.rows.iter().enumerate().skip(first).take(n_rows) {
            let y = body_y + (r - first) as i32 * lh - off;
            let reg = &mv.m.regions[row.region];
            let st = ci_of(row.region).and_then(|ci| mv.states.get(ci).copied());
            let (lbg, rbg) = match (reg.kind, st) {
                (Kind::Conflict, Some(s)) => {
                    let a = |taken: bool| {
                        if !s.resolved() {
                            0x24
                        } else if taken {
                            0x34
                        } else {
                            0x0a
                        }
                    };
                    (Some(col_in.with_alpha(a(s.has_incoming()))), Some(col_cur.with_alpha(a(s.has_current()))))
                }
                (Kind::Theirs, _) => (Some(c.git_modified.with_alpha(0x18)), None),
                (Kind::Ours, _) => (None, Some(c.git_modified.with_alpha(0x18))),
                (Kind::Both, _) => (Some(c.git_modified.with_alpha(0x10)), Some(c.git_modified.with_alpha(0x10))),
                _ => (None, None),
            };
            if let Some(b) = lbg {
                cv.fill_clip(Rect::new(lp_r.x, y, lp_r.w, lh), b, lp_r);
            }
            if let Some(b) = rbg {
                cv.fill_clip(Rect::new(rp_r.x + 1, y, rp_r.w - 1, lh), b, rp_r);
            }
            if Some(row.region) == cur_region {
                cv.fill_clip(Rect::new(lp_r.x, y, 3, lh), c.accent, lp_r);
                cv.fill_clip(Rect::new(rp_r.x + 1, y, 3, lh), c.accent, rp_r);
            }
            for (side, lines, pieces, x0) in
                [(row.theirs, &mv.m.theirs, &mut lp, lp_r.x), (row.ours, &mv.m.ours, &mut rp, rp_r.x)]
            {
                match side {
                    Some(l) => {
                        let text = merge3::chomp(&lines[l]);
                        pieces.push((crate::gitx::expand_tabs(text, self.config.tab_width, max_cols), fg));
                        nums.push((x0, y, format!("{:>digits$}", l + 1)));
                    }
                    None => pieces.push((String::new(), fg)),
                }
                pieces.push(("\n".into(), fg));
            }
        }
        for (x0, y, num) in &nums {
            let clip = if *x0 == lp_r.x { lp_r } else { rp_r };
            self.tx.draw_line(cv, x0 + 8, *y, num, dim, clip);
        }
        self.tx.draw(cv, lp_r.x + num_w + 12, body_y - off, &lp, lp_r);
        self.tx.draw(cv, rp_r.x + num_w + 12, body_y - off, &rp, rp_r);

        // Checkboxes on the first row of every visible conflict, then the rows themselves.
        let mv = self.merge.as_ref().expect("merge view");
        let wide = half >= 380;
        let boxes: Vec<(usize, usize, State)> = mv
            .conflicts
            .iter()
            .enumerate()
            .filter(|&(_, &k)| mv.spans[k].start < first + n_rows && mv.spans[k].end > first)
            .map(|(ci, &k)| (ci, k, mv.states.get(ci).copied().unwrap_or(State::Unresolved)))
            .collect();
        let spans = mv.spans.clone();
        for (ci, k, st) in boxes {
            let y = body_y + spans[k].start as i32 * lh - first as i32 * lh - off;
            for (incoming, pane) in [(true, lp_r), (false, rp_r)] {
                let checked = if incoming { st.has_incoming() } else { st.has_current() };
                let both = st == State::CurrentIncoming || st == State::IncomingCurrent;
                let nth = match (st, incoming) {
                    (State::CurrentIncoming, true) | (State::IncomingCurrent, false) => " 2nd",
                    _ if both => " 1st",
                    _ => "",
                };
                let base = match (incoming, wide) {
                    (true, true) => "Accept incoming",
                    (false, true) => "Accept current",
                    (true, false) => "Incoming",
                    (false, false) => "Current",
                };
                let label = format!("{base}{nth}");
                let w = self.tx.ui_width_sized(&label, false, 12.0) + 34;
                let r = Rect::new(pane.right() - 18 - w, y + 1, w, lh - 2);
                let col = if incoming { col_in } else { col_cur };
                let hov = r.contains(mx, my);
                cv.fill_round_clip(r, 6, c.sidebar_bg, pane);
                cv.fill_round_clip(r, 6, col.with_alpha(if hov { 0x40 } else { 0x1c }), pane);
                let bx = Rect::new(r.x + 8, r.y + (r.h - 12) / 2, 12, 12);
                if checked {
                    cv.fill_round_clip(bx, 3, col, pane);
                    self.tx.icon(cv, "ui/check", bx.x + 2, bx.y + 2, 8, c.on_accent, pane);
                } else {
                    cv.fill_round_clip(bx, 3, fg.with_alpha(0x30), pane);
                    cv.fill_round_clip(bx.inset(1), 2, c.sidebar_bg, pane);
                }
                self.tx.draw_ui_sized(cv, r.x + 26, r.y + 1, &label, fg, r.intersect(&pane), false, 12.0);
                let hit = r.intersect(&pane);
                if hit.h > 0 {
                    btns.push((hit, if incoming { MergeBtn::Incoming(ci) } else { MergeBtn::Current(ci) }));
                }
            }
            let rows_r = Rect::new(area.x, y, area.w, spans[k].len() as i32 * lh)
                .intersect(&Rect::new(area.x, body_y, area.w, body_h));
            if rows_r.h > 0 {
                btns.push((rows_r, MergeBtn::Goto(ci)));
            }
        }

        // ── "Result" strip above the document ──
        let foot = Rect::new(area.x, area.bottom() - FOOT_H, area.w, FOOT_H);
        cv.fill(foot, c.sidebar_bg);
        cv.fill(Rect::new(area.x, foot.y, area.w, 1), c.border);
        cv.fill(Rect::new(area.x, foot.bottom() - 1, area.w, 1), c.border);
        self.tx.draw_ui_sized(cv, foot.x + 12, foot.y + 3, "Result", fg, foot, true, 12.0);
        let mv = self.merge.as_ref().expect("merge view");
        let hint = match mv.states.get(mv.cur) {
            Some(s) if !mv.conflicts.is_empty() => {
                format!(
                    "conflict {} of {}: {}   ·   edit freely, Ctrl+Z undoes a choice",
                    mv.cur + 1,
                    mv.conflicts.len(),
                    state_label(*s)
                )
            }
            _ => "edit freely, then Mark resolved".to_string(),
        };
        let rw = self.tx.ui_width_sized("Result", true, 12.0);
        let hint = crate::ui::fit_ellipsis(&mut self.tx, &hint, (foot.w - rw - 40).max(0), false);
        self.tx.draw_ui_sized(cv, foot.x + 24 + rw, foot.y + 3, &hint, dim, foot, false, 12.0);
        if let Some(mv) = self.merge.as_mut() {
            mv.btns = btns;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkboxes_combine_in_the_order_they_are_checked() {
        use State::*;
        assert_eq!(toggled(Unresolved, true), Take::Incoming);
        assert_eq!(toggled(Unresolved, false), Take::Current);
        assert_eq!(toggled(Manual, true), Take::Incoming);
        assert_eq!(toggled(Current, true), Take::CurrentIncoming, "current first: it was checked first");
        assert_eq!(toggled(Incoming, false), Take::IncomingCurrent);
        assert_eq!(toggled(CurrentIncoming, true), Take::Current, "unchecking one keeps the other");
        assert_eq!(toggled(IncomingCurrent, false), Take::Incoming);
        assert_eq!(toggled(Incoming, true), Take::Markers, "unchecking the only side brings the markers back");
        assert_eq!(toggled(Current, false), Take::Markers);
    }

    fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).into_owned())
        }
    }

    #[test]
    fn merge_editor_resolves_a_conflicted_file_end_to_end() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let d = tmp("merge-editor");
        git(&d, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(d.join("f.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        git(&d, &["add", "f.txt"]).unwrap();
        git(&d, &["commit", "-q", "-m", "base"]).unwrap();
        git(&d, &["checkout", "-q", "-b", "feat"]).unwrap();
        std::fs::write(d.join("f.txt"), "one\nTWO feat\nthree\nfour\nFIVE feat\n").unwrap();
        git(&d, &["commit", "-q", "-am", "feat"]).unwrap();
        git(&d, &["checkout", "-q", "main"]).unwrap();
        std::fs::write(d.join("f.txt"), "one\nTWO main\nthree\nfour\nFIVE main\n").unwrap();
        git(&d, &["commit", "-q", "-am", "main"]).unwrap();
        assert!(git(&d, &["merge", "feat"]).is_err());

        let mut app = test_app(&d);
        app.merge_open(&d.join("f.txt"));
        let mv = app.merge.as_ref().expect("merge editor open");
        assert_eq!((mv.conflicts.len(), mv.resolved(), mv.op), (2, 0, "merge"));
        assert!(mv.titles.0.contains("feat") && mv.titles.1.contains("main"), "{:?}", mv.titles);
        assert!(mv.markers);
        let di = app.active;
        assert_eq!(app.docs[di].path(), Some(d.join("f.txt").as_path()));

        // Marking resolved is refused while markers remain.
        app.merge_mark_resolved();
        assert!(app.merge.is_some());
        assert_eq!(merge3::unmerged(&d), vec!["f.txt".to_string()]);

        // Check Current then Incoming on the first conflict: both, current first.
        app.merge_toggle(0, false);
        app.merge_toggle(0, true);
        assert_eq!(app.merge.as_ref().unwrap().states[0], State::CurrentIncoming);
        // Undo goes back one choice.
        app.docs[di].buf.close_group();
        app.run_cmd(crate::app_overlay::Cmd::Undo);
        app.docs[di].sync_edits();
        app.merge_sync();
        assert_eq!(app.merge.as_ref().unwrap().states[0], State::Current);
        // Next conflict, take incoming.
        app.merge_step(true);
        assert_eq!(app.merge.as_ref().unwrap().cur, 1);
        app.merge_toggle(1, true);
        let mv = app.merge.as_ref().unwrap();
        assert_eq!((mv.resolved(), mv.markers), (2, false));
        assert_eq!(app.docs[di].buf.to_string(), "one\nTWO main\nthree\nfour\nFIVE feat\n");

        // Draw a frame with the panes laid out (no panic, buttons registered).
        app.size = (1000, 700);
        let mut px = vec![0u32; 1000 * 700];
        let mut cv = Canvas { px: &mut px, w: 1000, h: 700 };
        app.render_into(&mut cv, 0.016);
        let mv = app.merge.as_ref().unwrap();
        assert!(mv.area.h > 0, "the panes take the top of the editor");
        assert!(app.layout.editor.y >= mv.area.bottom(), "the result is below them");
        assert!(mv.btns.iter().any(|(_, b)| *b == MergeBtn::Resolve));

        // Mark resolved: saved, staged, closed.
        app.merge_mark_resolved();
        assert!(app.merge.is_none());
        assert!(merge3::unmerged(&d).is_empty());
        assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), "one\nTWO main\nthree\nfour\nFIVE feat\n");
        assert_eq!(git(&d, &["show", ":f.txt"]).unwrap(), "one\nTWO main\nthree\nfour\nFIVE feat\n");
        drop(app);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn opening_a_file_without_conflicts_says_so() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let d = tmp("merge-none");
        git(&d, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(d.join("a.txt"), "x\n").unwrap();
        git(&d, &["add", "a.txt"]).unwrap();
        git(&d, &["commit", "-q", "-m", "x"]).unwrap();
        let mut app = test_app(&d);
        app.merge_open(&d.join("a.txt"));
        assert!(app.merge.is_none());
        assert!(app.status.as_ref().is_some_and(|(s, _)| s.contains("no merge conflict")));
        drop(app);
        let _ = std::fs::remove_dir_all(&d);
    }
}
