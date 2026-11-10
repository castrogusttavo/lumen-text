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
}

impl DiffView {
    /// A read-only diff.
    pub fn plain(title: String, left_name: String, right_name: String, rows: Vec<Row>) -> Self {
        let blocks = gitx::blocks(&rows);
        DiffView { title, left_name, right_name, rows, scroll: 0.0, src: None, blocks, cur: 0, btns: Vec::new() }
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
        let src = DiffSrc { top: top.to_path_buf(), rel: rel.to_string(), old_rel: old_rel.map(str::to_string), side, hunks, old, new, editable, can_flip: false, name: name.to_string() };
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
        self.diff = Some(DiffView::plain(format!("{} — HEAD vs. current file", d.name), "HEAD".into(), "Current".into(), rows));
    }

    pub fn open_diff_commit(&mut self, path: &Path, commit: &str, subject: &str) {
        let old = gitx::parent_file_at(path, commit).unwrap_or_default();
        let new = gitx::file_at(path, commit).unwrap_or_default();
        let rows = gitx::side_by_side(&old, &new);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.diff = Some(DiffView::plain(format!("{name} — {commit}  {subject}"), format!("{commit}^"), commit.to_string(), rows));
    }

    pub fn open_file_history(&mut self) {
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf)) else { return };
        let log = gitx::file_log(&path, 200);
        if log.is_empty() {
            return self.say("no git history for this file");
        }
        let items: Vec<Item> = log
            .into_iter()
            .map(|c| Item { label: c.subject.clone(), detail: format!("{} · {} · {}", c.short, c.author, c.when), action: Action::GitShow(path.clone(), c.hash, c.subject) })
            .collect();
        self.open_locations("File history", items);
    }

    /// Keys while a diff is shown. Returns true when consumed.
    pub fn diff_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if matches!(key, Key::Character(c) if c.as_str() == "s") && self.diff.as_ref().is_some_and(|d| d.src.is_some()) {
            let cur = self.diff.as_ref().map_or(0, |d| d.cur);
            self.diff_toggle_block(cur);
            return true;
        }
        let area_h = self.diff_area().h;
        let Some(dv) = self.diff.as_mut() else { return false };
        let page = (area_h / line_h() as i32 - 3).max(1) as f64;
        let max = (dv.rows.len() as f64 - ((area_h - 28) / line_h() as i32) as f64).max(0.0);
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
            return self.say("hunk-by-hunk only works for files that already exist in HEAD and the index; use the checkbox");
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
                let max = (nd.rows.len() as f64 - ((area_h - 28) / line_h() as i32) as f64).max(0.0);
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

    /// Left click on a hunk button / the flip button. Returns true when consumed.
    pub fn diff_click(&mut self, x: f64, y: f64) -> bool {
        let Some(&(_, b)) = self.diff.as_ref().and_then(|d| d.btns.iter().find(|(r, _)| r.contains(x, y))) else { return false };
        match b {
            DiffBtn::Hunk(i) => {
                if let Some(d) = self.diff.as_mut() {
                    d.cur = i;
                }
                self.diff_toggle_block(i);
            }
            DiffBtn::Flip => self.diff_flip(),
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
        let rows = match delta {
            MouseScrollDelta::LineDelta(_, y) => -(y as f64) * 3.0,
            MouseScrollDelta::PixelDelta(p) => -p.y / line_h() as f64,
        };
        let max = (dv.rows.len() as f64 - ((area_h - 28) / line_h() as i32) as f64).max(0.0);
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
        let head_h = 28;
        let half = editor.w / 2;
        let (lr, rr) = (Rect::new(editor.x, editor.y + head_h, half, editor.h - head_h), Rect::new(editor.x + half, editor.y + head_h, editor.w - half, editor.h - head_h));
        cv.fill(Rect::new(editor.x, editor.y, editor.w, head_h), c.sidebar_bg);
        cv.fill(Rect::new(editor.x, editor.y + head_h - 1, editor.w, 1), c.border);
        cv.fill(Rect::new(editor.x + half, editor.y, 1, editor.h), c.border);
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
        self.tx.draw_ui(cv, editor.x + 14, editor.y + 5, &format!("{}   ·   {}", dv.left_name, dv.title), fg, Rect::new(editor.x, editor.y, half, head_h), true);
        let narrow = hint_right - editor.x - half < 330;
        let hint = match (staging.is_some(), narrow) {
            (true, false) => format!("{}   ·   Esc closes · n/p block · s stage", dv.right_name),
            (true, true) => format!("{}  ·  n/p · s", dv.right_name),
            (false, _) => format!("{}   ·   Esc closes · n next block", dv.right_name),
        };
        self.tx.draw_ui(cv, editor.x + half + 14, editor.y + 5, &hint, dim, Rect::new(editor.x + half, editor.y, (hint_right - editor.x - half).max(0), head_h), false);

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
            if row.kind == RowKind::Changed {
                if let (Some((_, l)), Some((_, r))) = (&row.left, &row.right) {
                    let (lrg, rrg) = gitx::changed_ranges(l, r);
                    // Ranges are in characters; tabs expand to the configured width on screen.
                    let tab = self.config.tab_width;
                    for (a, b) in lrg {
                        let (a, b) = (crate::editor::visual_col(l, a, tab), crate::editor::visual_col(l, b, tab));
                        cv.fill_clip(Rect::new(lr.x + num_w + 8 + (a as f32 * cell) as i32, y, ((b - a) as f32 * cell) as i32, lh), strong_r, lr);
                    }
                    for (a, b) in rrg {
                        let (a, b) = (crate::editor::visual_col(r, a, tab), crate::editor::visual_col(r, b, tab));
                        cv.fill_clip(Rect::new(rr.x + num_w + 8 + (a as f32 * cell) as i32, y, ((b - a) as f32 * cell) as i32, lh), strong_a, rr);
                    }
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
                cv.fill_round_clip(r, 6, c.accent.with_alpha(if !editable { 0x10 } else if hov { 0x48 } else { 0x26 }), rr);
                self.tx.draw_ui_sized(cv, r.x + 10, r.y + 1, label, if editable { c.accent_text } else { dim }, r, false, 12.0);
                btns.push((r, DiffBtn::Hunk(i)));
            }
        }
        let (total, visible, pos) = (rows_total as f64, (lr.h / lh) as f64, scroll as f64);
        self.region_bar(cv, crate::scrollreg::Reg::Diff, Rect::new(editor.x, lr.y, editor.w, lr.h), total, visible, pos, lh as f64, false);
        if let Some(d) = self.diff.as_mut() {
            d.btns = btns;
        }
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
        let Some((_, ci, pick)) = d.conflict_hits.iter().find(|(r, _, _)| r.contains(x, y)).copied() else { return false };
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
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "T"], &["config", "commit.gpgsign", "false"]] {
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
}
