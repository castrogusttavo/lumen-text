//! Git UI on top of `gitx`: stage/revert hunk, side-by-side diff, file history, merge conflicts.

use crate::{
    app::App,
    app_overlay::{Action, Item},
    git,
    gitx::{self, Pick, Row, RowKind, Side},
    theme::Rgba,
    ui::{Canvas, Rect, line_h},
};
use std::path::{Path, PathBuf};
use winit::event::MouseScrollDelta;

/// Where the rows of a diff came from, when its hunks can be staged / unstaged one by one.
pub struct DiffSrc {
    pub top: PathBuf,
    /// Path relative to the repository root.
    pub rel: String,
    /// Previous path of a staged rename.
    pub old_rel: Option<String>,
    pub side: Side,
    /// One per changed block of `rows`, in the same order.
    pub hunks: Vec<gitx::Hunk>,
    pub old: String,
    pub new: String,
    /// Both versions exist (not untracked, added, deleted or unmerged): hunks can be applied.
    pub editable: bool,
    /// The other side (staged / not staged) has changes too: a header button flips to it.
    pub can_flip: bool,
    /// Display name for the title.
    pub name: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffBtn {
    /// Stage / unstage block `i`.
    Hunk(usize),
    Flip,
    /// Accept / reject the agent's proposed changes.
    Accept,
    Reject,
}

pub struct DiffView {
    pub title: String,
    pub left_name: String,
    pub right_name: String,
    pub rows: Vec<Row>,
    pub scroll: f64, // rows
    /// Hunk source; None for read-only diffs (history, buffer against HEAD).
    pub src: Option<DiffSrc>,
    /// `(first row, end row)` of every changed block.
    pub blocks: Vec<(usize, usize)>,
    /// Block that `s` acts on and `n` / `p` move.
    pub cur: usize,
    /// Buttons drawn last frame.
    pub btns: Vec<(Rect, DiffBtn)>,
    /// Changes proposed by the agent over the IDE bridge (key of its pending `openDiff`): the
    /// header offers Accept / Reject instead of the hint.
    pub proposal: Option<u64>,
}

impl DiffView {
    /// A read-only diff.
    pub fn plain(title: String, left_name: String, right_name: String, rows: Vec<Row>) -> Self {
        let blocks = gitx::blocks(&rows);
        DiffView {
            title,
            left_name,
            right_name,
            rows,
            scroll: 0.0,
            src: None,
            blocks,
            cur: 0,
            btns: Vec::new(),
            proposal: None,
        }
    }

    /// Height of the header (a proposal's is taller: two lines and the Accept / Reject pills).
    pub fn head_h(&self) -> i32 {
        if self.proposal.is_some() { 40 } else { 28 }
    }

    /// The diff of one file on `side` of the index, with hunk staging. None = no differences there.
    pub fn staging(top: &Path, rel: &str, old_rel: Option<&str>, side: Side, name: &str) -> Option<DiffView> {
        let (old, new) = gitx::side_texts(top, rel, old_rel, side);
        let editable = old.is_some() && new.is_some();
        // Unmerged files have no index version: show HEAD against the file instead.
        let shown_old = match (&old, side) {
            (None, Side::Unstaged) => gitx::side_texts(top, rel, old_rel, Side::Staged).0.unwrap_or_default(),
            _ => old.clone().unwrap_or_default(),
        };
        let (old, new) = (shown_old, new.unwrap_or_default());
        let rows = gitx::side_by_side(&old, &new);
        if rows.iter().all(|r| r.kind == RowKind::Same) {
            return None;
        }
        let hunks = gitx::hunks(&old, &new);
        let blocks = gitx::blocks(&rows);
        let editable = editable && hunks.len() == blocks.len();
        let (left, right, what) = match side {
            Side::Unstaged => ("Index", "Current", "changes"),
            Side::Staged => ("HEAD", "Index", "staged"),
        };
        let src = DiffSrc {
            top: top.to_path_buf(),
            rel: rel.to_string(),
            old_rel: old_rel.map(str::to_string),
            side,
            hunks,
            old,
            new,
            editable,
            can_flip: false,
            name: name.to_string(),
        };
        let mut dv = DiffView::plain(format!("{name} — {what}"), left.into(), right.into(), rows);
        dv.src = Some(src);
        Some(dv)
    }

    /// `staging` on `side`, falling back to the other side when this one has nothing; marks
    /// `can_flip` when both have changes.
    pub fn staging_auto(top: &Path, rel: &str, old_rel: Option<&str>, side: Side, name: &str) -> Option<DiffView> {
        let other = if side == Side::Unstaged { Side::Staged } else { Side::Unstaged };
        let first = Self::staging(top, rel, old_rel, side, name);
        let second = Self::staging(top, rel, old_rel, other, name);
        match (first, second) {
            (Some(mut a), Some(_)) => {
                if let Some(s) = a.src.as_mut() {
                    s.can_flip = true;
                }
                Some(a)
            }
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }
}

impl App {
    // ───────────── hunks ─────────────

    /// The hunk under the cursor. `staging` compares the buffer with the *index* (so hunks that
    /// are already staged do not shift the patch); reverting compares with HEAD.
    fn hunk_context(&mut self, staging: bool) -> Option<(String, String, gitx::Hunk, PathBuf, String)> {
        let d = self.docs.get(self.active)?;
        let Some(head) = d.git_base.clone() else {
            self.say("file has no version in HEAD (untracked)");
            return None;
        };
        let cur = d.buf.to_string();
        let (line, _) = d.buf.line_col(d.ed.head());
        let path = d.path()?.to_path_buf();
        let top = git::toplevel(path.parent()?).unwrap_or_else(|| self.root.clone());
        let rel = path.strip_prefix(&top).ok()?.to_string_lossy().into_owned();
        let base = if staging { gitx::side_texts(&top, &rel, None, Side::Unstaged).0.unwrap_or(head) } else { head };
        let hs = gitx::hunks(&base, &cur);
        let Some(h) = gitx::hunk_at_line(&hs, line).cloned() else {
            self.say("no change under the cursor");
            return None;
        };
        Some((base, cur, h, top, rel))
    }

    pub fn git_stage_hunk(&mut self) {
        let Some((base, cur, h, top, rel)) = self.hunk_context(true) else { return };
        match gitx::stage_hunk(&top, &rel, &base, &cur, &h) {
            Ok(()) => {
                self.refresh_git_status();
                self.say("hunk added to stage");
            }
            Err(e) => self.say(format!("git: {e}")),
        }
    }

    pub fn git_revert_hunk(&mut self) {
        let Some((base, cur, h, _, _)) = self.hunk_context(false) else { return };
        let text = gitx::revert_hunk_text(&base, &cur, &h);
        if let Some(d) = self.docs.get_mut(self.active) {
            d.replace_text(&text);
            d.reveal = true;
        }
        self.say("hunk reverted (Ctrl+Z undoes)");
    }

    // ───────────── diff view ─────────────

    pub fn open_diff_head(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let base = d.git_base.clone().unwrap_or_default();
        let cur = d.buf.to_string();
        let rows = gitx::side_by_side(&base, &cur);
        if rows.iter().all(|r| r.kind == RowKind::Same) {
            return self.say("no differences from HEAD");
        }
        self.diff =
            Some(DiffView::plain(format!("{} — HEAD vs. current file", d.name), "HEAD".into(), "Current".into(), rows));
    }

    pub fn open_diff_commit(&mut self, path: &Path, commit: &str, subject: &str) {
        let old = gitx::parent_file_at(path, commit).unwrap_or_default();
        let new = gitx::file_at(path, commit).unwrap_or_default();
        let rows = gitx::side_by_side(&old, &new);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.diff = Some(DiffView::plain(
            format!("{name} — {commit}  {subject}"),
            format!("{commit}^"),
            commit.to_string(),
            rows,
        ));
    }

    pub fn open_file_history(&mut self) {
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf)) else { return };
        let log = gitx::file_log(&path, 200);
        if log.is_empty() {
            return self.say("no git history for this file");
        }
        let items: Vec<Item> = log
            .into_iter()
            .map(|c| Item {
                label: c.subject.clone(),
                detail: format!("{} · {} · {}", c.short, c.author, c.when),
                action: Action::GitShow(path.clone(), c.hash, c.subject),
            })
            .collect();
        self.open_locations("File history", items);
    }

    /// Keys while a diff is shown. Returns true when consumed.
    pub fn diff_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if self.diff.as_ref().is_some_and(|d| d.proposal.is_some()) {
            // The agent's proposal: Ctrl+Enter accepts, Esc rejects, Q does not close it unanswered.
            match key {
                Key::Named(NamedKey::Enter) if ctrl && !alt => {
                    self.ide_decide(true);
                    return true;
                }
                Key::Named(NamedKey::Escape) if !ctrl && !alt => {
                    self.ide_decide(false);
                    return true;
                }
                Key::Character(c) if c.as_str() == "q" && !ctrl && !alt => return true,
                _ => {}
            }
        }
        if ctrl || alt {
            // Bound shortcuts and chords (Ctrl+J, Ctrl+S, Ctrl+P, Ctrl+K ...) stay global while a
            // diff is open; other Ctrl/Alt letters mean nothing here (and must not stage or close).
            if self.keymap.lookup(ctrl, shift, alt, key).is_some()
                || self.keymap.chord_start(ctrl, shift, alt, key).is_some()
            {
                return false;
            }
            if matches!(key, Key::Character(_)) {
                return true;
            }
        }
        if matches!(key, Key::Character(c) if c.as_str() == "s") && self.diff.as_ref().is_some_and(|d| d.src.is_some())
        {
            let cur = self.diff.as_ref().map_or(0, |d| d.cur);
            self.diff_toggle_block(cur);
            return true;
        }
        let area_h = self.diff_area().h;
        let Some(dv) = self.diff.as_mut() else { return false };
        let page = (area_h / line_h() as i32 - 3).max(1) as f64;
        let max = (dv.rows.len() as f64 - ((area_h - dv.head_h()) / line_h() as i32) as f64).max(0.0);
        match key {
            Key::Named(NamedKey::Escape) => self.diff = None,
            Key::Character(c) if c.as_str() == "q" => self.diff = None,
            Key::Named(NamedKey::ArrowDown) => dv.scroll = (dv.scroll + 1.0).min(max),
            Key::Named(NamedKey::ArrowUp) => dv.scroll = (dv.scroll - 1.0).max(0.0),
            Key::Named(NamedKey::PageDown) => dv.scroll = (dv.scroll + page).min(max),
            Key::Named(NamedKey::PageUp) => dv.scroll = (dv.scroll - page).max(0.0),
            Key::Named(NamedKey::Home) => dv.scroll = 0.0,
            Key::Named(NamedKey::End) => dv.scroll = max,
            Key::Character(c) if matches!(c.as_str(), "n" | "p") => {
                // Next / previous changed block: it becomes the current one (what `s` stages).
                let next = c.as_str() == "n";
                let n = dv.blocks.len();
                if n > 0 {
                    // Starting point: the current block, unless it is already off-screen.
                    let top = dv.scroll as usize;
                    let at = dv.blocks.get(dv.cur).map_or(0, |b| b.0);
                    let visible = at >= top && at < top + page as usize;
                    dv.cur = match (next, visible) {
                        (true, true) => (dv.cur + 1).min(n - 1),
                        (false, true) => dv.cur.saturating_sub(1),
                        (true, false) => dv.blocks.iter().position(|b| b.0 > top).unwrap_or(n - 1),
                        (false, false) => dv.blocks.iter().rposition(|b| b.0 < top).unwrap_or(0),
                    };
                    let first = dv.blocks[dv.cur].0 as f64;
                    if first < dv.scroll || first >= dv.scroll + page {
                        dv.scroll = first.min(max);
                    }
                }
            }
            _ => return true, // swallow everything else while the diff is open
        }
        true
    }

    // ───────────── hunk staging ─────────────

    /// Stage or unstage block `i` of the open diff, then re-read it from git.
    pub fn diff_toggle_block(&mut self, i: usize) {
        let Some(src) = self.diff.as_ref().and_then(|d| d.src.as_ref()) else { return };
        if !src.editable {
            return self
                .say("hunk-by-hunk only works for files that already exist in HEAD and the index; use the checkbox");
        }
        let Some(h) = src.hunks.get(i).copied() else { return };
        let staging = src.side == Side::Unstaged;
        if let Err(e) = gitx::toggle_hunk(&src.top, &src.rel, src.side, &src.old, &src.new, &h) {
            return self.say(format!("git: {e}"));
        }
        self.diff_reload(i);
        self.refresh_git_status();
        self.say(if staging { "hunk added to stage" } else { "hunk removed from stage" });
    }

    /// Re-read the open staging diff after the index changed; `keep` is the block to stay near.
    fn diff_reload(&mut self, keep: usize) {
        let Some(dv) = self.diff.take() else { return };
        let Some(src) = dv.src.as_ref() else {
            self.diff = Some(dv);
            return;
        };
        let (scroll, old_rel) = (dv.scroll, src.old_rel.clone());
        self.diff = DiffView::staging_auto(&src.top, &src.rel, old_rel.as_deref(), src.side, &src.name);
        let area_h = self.diff_area().h;
        match self.diff.as_mut() {
            Some(nd) => {
                nd.scroll = scroll;
                nd.cur = keep.min(nd.blocks.len().saturating_sub(1));
                let max = (nd.rows.len() as f64 - ((area_h - nd.head_h()) / line_h() as i32) as f64).max(0.0);
                nd.scroll = nd.scroll.min(max);
            }
            None => self.say("no hunks left in this file"),
        }
    }

    /// Show the other side (staged / not staged) of the file in the open diff.
    pub fn diff_flip(&mut self) {
        let Some(src) = self.diff.as_ref().and_then(|d| d.src.as_ref()) else { return };
        let side = if src.side == Side::Unstaged { Side::Staged } else { Side::Unstaged };
        if let Some(nd) = DiffView::staging_auto(&src.top, &src.rel, src.old_rel.as_deref(), side, &src.name) {
            self.diff = Some(nd);
        }
    }

    /// The open diff's button under the pointer (for the hand cursor; the buttons draw their own hover).
    pub fn diff_btn_at(&self, x: f64, y: f64) -> Option<Rect> {
        self.diff.as_ref()?.btns.iter().find(|(r, _)| r.contains(x, y)).map(|(r, _)| *r)
    }

    /// Left click on a hunk button / the flip button. Returns true when consumed.
    pub fn diff_click(&mut self, x: f64, y: f64) -> bool {
        let Some(&(_, b)) = self.diff.as_ref().and_then(|d| d.btns.iter().find(|(r, _)| r.contains(x, y))) else {
            return false;
        };
        match b {
            DiffBtn::Hunk(i) => {
                if let Some(d) = self.diff.as_mut() {
                    d.cur = i;
                }
                self.diff_toggle_block(i);
            }
            DiffBtn::Flip => self.diff_flip(),
            DiffBtn::Accept => self.ide_decide(true),
            DiffBtn::Reject => self.ide_decide(false),
        }
        true
    }

    pub fn diff_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        // In a split the wheel only scrolls the diff when the pointer is over the pane it covers.
        if self.split.is_some() && !self.diff_area().contains(self.mouse.0, self.mouse.1) {
            return false;
        }
        let area_h = self.diff_area().h;
        let Some(dv) = self.diff.as_mut() else { return false };
        let rows = crate::scroller::wheel_rows(delta, line_h() as f64);
        let max = (dv.rows.len() as f64 - ((area_h - dv.head_h()) / line_h() as i32) as f64).max(0.0);
        dv.scroll = (dv.scroll + rows).clamp(0.0, max);
        true
    }

    pub fn draw_diff(&mut self, cv: &mut Canvas) {
        let editor = self.diff_area();
        let Some(dv) = &self.diff else { return };
        let (rows_total, scroll) = (dv.rows.len(), dv.scroll);
        let c = self.theme.chrome;
        let (fg, dim, bg) = (self.theme.foreground, c.tab_inactive_fg, self.theme.background);
        cv.fill(editor, bg);
        let head_h = dv.head_h();
        let proposal = dv.proposal.is_some();
        let half = editor.w / 2;
        let (lr, rr) = (
            Rect::new(editor.x, editor.y + head_h, half, editor.h - head_h),
            Rect::new(editor.x + half, editor.y + head_h, editor.w - half, editor.h - head_h),
        );
        cv.fill(Rect::new(editor.x, editor.y, editor.w, head_h), c.sidebar_bg);
        cv.fill(Rect::new(editor.x, editor.y + head_h - 1, editor.w, 1), c.border);
        cv.fill(Rect::new(editor.x + half, editor.y + if proposal { head_h } else { 0 }, 1, editor.h), c.border);
        let mut btns: Vec<(Rect, DiffBtn)> = Vec::new();
        let staging = dv.src.as_ref().map(|s| (s.side, s.editable, s.can_flip));
        // Header, right half: the hint, and (when the file has both) the button to flip sides.
        let mut hint_right = editor.right() - 10;
        if let Some((side, _, true)) = staging {
            let label = if side == Side::Unstaged { "View what is staged" } else { "View what is not staged" };
            let w = self.tx.ui_width(label, false) + 20;
            let r = Rect::new(editor.right() - 10 - w, editor.y + 3, w, head_h - 6);
            let hov = r.contains(self.mouse.0, self.mouse.1);
            cv.fill_round(r, 7, c.accent.with_alpha(if hov { 0x40 } else { 0x24 }));
            self.tx.draw_ui(cv, r.x + 10, r.y + 3, label, c.accent_text, r, false);
            btns.push((r, DiffBtn::Flip));
            hint_right = r.x - 10;
        }
        if proposal {
            let title = dv.title.clone();
            self.draw_proposal_head(cv, Rect::new(editor.x, editor.y, editor.w, head_h), &title, &mut btns);
        } else {
            self.draw_diff_hint(cv, editor, head_h, hint_right, staging.is_some());
        }

        let Some(dv) = &self.diff else { return };

        let lh = line_h() as i32;
        let first = dv.scroll as usize;
        // `scroll` is fractional while the wheel glides: rows sit that much higher.
        let off = ((dv.scroll - first as f64) * lh as f64).round() as i32;
        let n = (lr.h / lh + 2) as usize;
        let cell = self.tx.cell_w;
        let (removed, added, strong_r, strong_a) = (c.diff_del, c.diff_add, c.diff_del_word, c.diff_add_word);
        let digits = dv.rows.len().to_string().len().max(3);
        let num_w = ((digits + 1) as f32 * cell) as i32;
        let max_cols = (((half - num_w - 16) as f32 / cell) as usize).max(4);
        let mut lp: Vec<(String, Rgba)> = Vec::new();
        let mut rp: Vec<(String, Rgba)> = Vec::new();
        let cur_block = dv.blocks.get(dv.cur).copied().filter(|_| staging.is_some());
        for (k, row) in dv.rows.iter().enumerate().skip(first).take(n) {
            let y = lr.y + (k - first) as i32 * lh - off;
            if cur_block.is_some_and(|(a, b)| k >= a && k < b) {
                cv.fill_clip(Rect::new(lr.x, y, 3, lh), c.accent, lr);
            }
            let (lbg, rbg) = match row.kind {
                RowKind::Same => (None, None),
                RowKind::Removed => (Some(removed), None),
                RowKind::Added => (None, Some(added)),
                RowKind::Changed => (Some(removed), Some(added)),
            };
            if let Some(b) = lbg {
                cv.fill_clip(Rect::new(lr.x, y, lr.w, lh), b, lr);
            }
            if let Some(b) = rbg {
                cv.fill_clip(Rect::new(rr.x, y, rr.w, lh), b, rr);
            }
            if row.kind == RowKind::Changed
                && let (Some((_, l)), Some((_, r))) = (&row.left, &row.right)
            {
                let (lrg, rrg) = gitx::changed_ranges(l, r);
                // Ranges are in characters; tabs expand to the configured width on screen.
                let tab = self.config.tab_width;
                for (a, b) in lrg {
                    let (a, b) = (crate::editor::visual_col(l, a, tab), crate::editor::visual_col(l, b, tab));
                    cv.fill_clip(
                        Rect::new(lr.x + num_w + 8 + (a as f32 * cell) as i32, y, ((b - a) as f32 * cell) as i32, lh),
                        strong_r,
                        lr,
                    );
                }
                for (a, b) in rrg {
                    let (a, b) = (crate::editor::visual_col(r, a, tab), crate::editor::visual_col(r, b, tab));
                    cv.fill_clip(
                        Rect::new(rr.x + num_w + 8 + (a as f32 * cell) as i32, y, ((b - a) as f32 * cell) as i32, lh),
                        strong_a,
                        rr,
                    );
                }
            }
            for (side, region, pieces) in [(&row.left, lr, &mut lp), (&row.right, rr, &mut rp)] {
                match side {
                    Some((ln, text)) => {
                        let shown = crate::gitx::expand_tabs(text, self.config.tab_width, max_cols);
                        self.tx.draw_line(cv, region.x + 6, y, &format!("{:>digits$}", ln + 1), dim, region);
                        pieces.push((shown, fg));
                    }
                    None => pieces.push((String::new(), fg)),
                }
                pieces.push(("\n".into(), fg));
            }
        }
        self.tx.draw(cv, lr.x + num_w + 8, lr.y - off, &lp, lr);
        self.tx.draw(cv, rr.x + num_w + 8, rr.y - off, &rp, rr);
        // Per-hunk button on the first row of every visible block (right pane, right edge).
        if let Some((side, editable, _)) = staging {
            // Short labels when the pane is narrow (the Git panel takes a good part of the window).
            let wide = rr.w >= 420;
            let label = match (side == Side::Unstaged, wide) {
                (true, true) => "Add to stage",
                (false, true) => "Unstage",
                (true, false) => "Stage",
                (false, false) => "Unstage",
            };
            let bw = self.tx.ui_width(label, false) + 20;
            for (i, &(a, _)) in dv.blocks.iter().enumerate() {
                if a < first || a >= first + n {
                    continue;
                }
                let y = lr.y + (a - first) as i32 * lh - off;
                let r = Rect::new(rr.right() - 22 - bw, y + 1, bw, lh - 2);
                let hov = editable && r.contains(self.mouse.0, self.mouse.1);
                cv.fill_round_clip(r, 6, c.sidebar_bg, rr);
                cv.fill_round_clip(
                    r,
                    6,
                    c.accent.with_alpha(if !editable {
                        0x10
                    } else if hov {
                        0x48
                    } else {
                        0x26
                    }),
                    rr,
                );
                self.tx.draw_ui_sized(
                    cv,
                    r.x + 10,
                    r.y + 1,
                    label,
                    if editable { c.accent_text } else { dim },
                    r,
                    false,
                    12.0,
                );
                btns.push((r, DiffBtn::Hunk(i)));
            }
        }
        let (total, visible, pos) = (rows_total as f64, (lr.h / lh) as f64, scroll);
        self.region_bar(
            cv,
            crate::scrollreg::Reg::Diff,
            Rect::new(editor.x, lr.y, editor.w, lr.h),
            total,
            visible,
            pos,
            lh as f64,
            false,
        );
        if let Some(d) = self.diff.as_mut() {
            d.btns = btns;
        }
    }

    /// Header of a plain / staging diff: "left · title" over the left pane, the right side's
    /// name and the keys over the right one.
    fn draw_diff_hint(&mut self, cv: &mut Canvas, editor: Rect, head_h: i32, hint_right: i32, staging: bool) {
        let Some(dv) = &self.diff else { return };
        let c = self.theme.chrome;
        let half = editor.w / 2;
        let left = format!("{}   ·   {}", dv.left_name, dv.title);
        let narrow = hint_right - editor.x - half < 330;
        let hint = match (staging, narrow) {
            (true, false) => format!("{}   ·   Esc closes · n/p block · s stage", dv.right_name),
            (true, true) => format!("{}  ·  n/p · s", dv.right_name),
            (false, _) => format!("{}   ·   Esc closes · n next block", dv.right_name),
        };
        let fg = self.theme.foreground;
        self.tx.draw_ui(cv, editor.x + 14, editor.y + 5, &left, fg, Rect::new(editor.x, editor.y, half, head_h), true);
        self.tx.draw_ui(
            cv,
            editor.x + half + 14,
            editor.y + 5,
            &hint,
            c.tab_inactive_fg,
            Rect::new(editor.x + half, editor.y, (hint_right - editor.x - half).max(0), head_h),
            false,
        );
    }

    /// Header of the agent's proposed changes: who proposes what (and how many more wait), then
    /// Reject and Accept (accent) pills with their keys.
    fn draw_proposal_head(&mut self, cv: &mut Canvas, head: Rect, file: &str, btns: &mut Vec<(Rect, DiffBtn)>) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let (mx, my) = self.mouse;
        // Pills, right to left: Accept (accent), Reject (outlined).
        let small = 11.0;
        let pill = |tx: &mut crate::ui::TextCtx, label: &str, keys: &str| {
            10 + 14 + 6 + tx.ui_width(label, true) + 8 + tx.ui_width_sized(keys, false, small) + 10
        };
        let aw = pill(&mut self.tx, "Accept", "Ctrl+Enter");
        let rw = pill(&mut self.tx, "Reject", "Esc");
        let acc = Rect::new(head.right() - 12 - aw, head.y + 6, aw, 28);
        let rej = Rect::new(acc.x - 8 - rw, head.y + 6, rw, 28);
        for (r, label, keys, icon, accent, btn) in [
            (rej, "Reject", "Esc", "ui/close", false, DiffBtn::Reject),
            (acc, "Accept", "Ctrl+Enter", "ui/check", true, DiffBtn::Accept),
        ] {
            let hov = r.contains(mx, my);
            let (fg, sub) = if accent {
                cv.fill_round(r, 8, if hov { c.accent.with_alpha(0xe0).over(c.sidebar_bg) } else { c.accent });
                (c.on_accent, c.on_accent.with_alpha(0xb0))
            } else {
                cv.fill_round(r, 8, strong.with_alpha(if hov { 0x1c } else { 0x0c }).over(c.sidebar_bg));
                cv.ring(r, 8, c.border);
                (strong, dim)
            };
            self.tx.icon(cv, icon, r.x + 10, r.y + 7, 14, fg, r);
            self.tx.draw_ui(cv, r.x + 30, r.y + 5, label, fg, r, true);
            let kx = r.x + 30 + self.tx.ui_width(label, true) + 8;
            self.tx.draw_ui_sized(cv, kx, r.y + 7, keys, sub, r, false, small);
            btns.push((r, btn));
        }
        // Left: sparkle, who proposes, the file and how many more wait.
        let room = Rect::new(head.x, head.y, (rej.x - 12 - head.x).max(0), head.h);
        let disc = Rect::new(head.x + 12, head.y + 8, 24, 24);
        cv.fill_round(disc, 12, c.accent.with_alpha(0x22).over(c.sidebar_bg));
        self.tx.icon(cv, "ui/sparkle", disc.x + 5, disc.y + 5, 14, c.accent_text, room);
        let who = self.ai.agent.map_or("the agent", |a| a.name);
        let more = self.ai.bridge.proposals.len().saturating_sub(1);
        let title =
            if more > 0 { format!("Proposed by {who}  ·  {more} more waiting") } else { format!("Proposed by {who}") };
        let tw = (room.w - 46).max(0);
        let title = crate::ui::fit_ellipsis(&mut self.tx, &title, tw, true);
        self.tx.draw_ui(cv, head.x + 46, head.y + 2, &title, strong, room, true);
        let sub = crate::ui::fit_ellipsis(&mut self.tx, &format!("{file}  ·  current ← → proposed"), tw, false);
        self.tx.draw_ui_sized(cv, head.x + 46, head.y + 21, &sub, dim, room, false, 12.0);
    }

    // ───────────── conflicts ─────────────

    pub fn resolve_conflict(&mut self, pick: Pick) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let (line, _) = d.buf.line_col(d.ed.head());
        let Some(c) = gitx::conflict_at_line(&d.conflicts, line).cloned() else {
            return self.say("the cursor is not inside a merge conflict");
        };
        let text = gitx::resolve(&d.buf.to_string(), &c, pick);
        d.replace_text(&text);
        d.conflicts_due = Some(std::time::Instant::now());
        d.reveal = true;
    }

    /// Click on one of the inline "Aceitar …" actions drawn above a conflict.
    pub fn conflict_click(&mut self, x: f64, y: f64) -> bool {
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        let Some((_, ci, pick)) = d.conflict_hits.iter().find(|(r, _, _)| r.contains(x, y)).copied() else {
            return false;
        };
        let Some(c) = d.conflicts.get(ci).cloned() else { return false };
        let text = gitx::resolve(&d.buf.to_string(), &c, pick);
        d.replace_text(&text);
        d.conflicts_due = Some(std::time::Instant::now());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_gitpanel::git_run;

    fn repo(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-dv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "T"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git_run(&d, a).unwrap();
        }
        d
    }

    #[test]
    fn staging_view_blocks_match_hunks_and_flip_sides() {
        let d = repo("flip");
        std::fs::write(d.join("f.txt"), "a\nb\nc\nd\ne\nf\n").unwrap();
        git_run(&d, &["add", "."]).unwrap();
        git_run(&d, &["commit", "-q", "-m", "x"]).unwrap();
        std::fs::write(d.join("f.txt"), "A\nb\nc\nd\ne\nF\n").unwrap();

        let dv = DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt").unwrap();
        let src = dv.src.as_ref().unwrap();
        assert!(src.editable && !src.can_flip, "nothing staged: one side only");
        assert_eq!((dv.blocks.len(), src.hunks.len()), (2, 2));
        assert_eq!((dv.left_name.as_str(), dv.right_name.as_str()), ("Index", "Current"));

        // Stage the first hunk: the file now has changes on both sides.
        gitx::toggle_hunk(&d, "f.txt", Side::Unstaged, &src.old, &src.new, &src.hunks[0]).unwrap();
        let dv = DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt").unwrap();
        let src = dv.src.as_ref().unwrap();
        assert!(src.can_flip, "staged + modified offers the other side");
        assert_eq!(dv.blocks.len(), 1, "only the unstaged hunk is left on this side");
        let staged = DiffView::staging_auto(&d, "f.txt", None, Side::Staged, "f.txt").unwrap();
        assert_eq!((staged.left_name.as_str(), staged.right_name.as_str()), ("HEAD", "Index"));
        assert_eq!(staged.blocks.len(), 1);

        // Stage the last one too: the unstaged side is empty, so the view falls back to the staged side.
        gitx::toggle_hunk(&d, "f.txt", Side::Unstaged, &src.old, &src.new, &src.hunks[0]).unwrap();
        let dv = DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt").unwrap();
        assert_eq!(dv.src.as_ref().unwrap().side, Side::Staged);
        assert_eq!(dv.blocks.len(), 2);
        assert!(!dv.src.as_ref().unwrap().can_flip);

        // Unstage everything and revert the file: no differences anywhere.
        git_run(&d, &["reset", "-q"]).unwrap();
        git_run(&d, &["checkout", "--", "f.txt"]).unwrap();
        assert!(DiffView::staging_auto(&d, "f.txt", None, Side::Unstaged, "f.txt").is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn untracked_and_deleted_files_show_but_cannot_stage_hunks() {
        let d = repo("ro");
        std::fs::write(d.join("keep.txt"), "x\ny\n").unwrap();
        git_run(&d, &["add", "."]).unwrap();
        git_run(&d, &["commit", "-q", "-m", "x"]).unwrap();
        std::fs::write(d.join("new.txt"), "n\n").unwrap();
        let dv = DiffView::staging_auto(&d, "new.txt", None, Side::Unstaged, "new.txt").unwrap();
        assert!(!dv.src.as_ref().unwrap().editable, "untracked has no index version");
        assert_eq!(dv.blocks.len(), 1, "but it still shows");
        std::fs::remove_file(d.join("keep.txt")).unwrap();
        let dv = DiffView::staging_auto(&d, "keep.txt", None, Side::Unstaged, "keep.txt").unwrap();
        assert!(!dv.src.as_ref().unwrap().editable, "deleted in the working tree");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn global_shortcuts_pass_through_an_open_diff() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        use winit::keyboard::{Key, ModifiersState};
        let _g = lock();
        let dir = tmp("diffkeys");
        let mut app = test_app(&dir);
        let press = |app: &mut App, mods: ModifiersState, c: &str| {
            app.mods = mods;
            app.handle_key(&crate::app::KeyIn { key: Key::Character(c.into()), text: None });
            app.mods = ModifiersState::empty();
        };
        app.diff = Some(DiffView::plain("d".into(), "a".into(), "b".into(), Vec::new()));
        let open = app.panel.open;
        press(&mut app, ModifiersState::CONTROL, "j");
        assert_eq!(app.panel.open, !open, "Ctrl+J toggles the panel even with a diff open");
        assert!(app.diff.is_some());
        // An unbound Ctrl+letter is swallowed, but it does not act like the bare letter.
        press(&mut app, ModifiersState::CONTROL | ModifiersState::ALT, "q");
        assert!(app.diff.is_some(), "Ctrl+Alt+Q must not close the diff like Q");
        press(&mut app, ModifiersState::empty(), "q");
        assert!(app.diff.is_none(), "plain Q still closes it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
