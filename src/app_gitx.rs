//! Git UI on top of `gitx`: stage/revert hunk, side-by-side diff, file history, merge conflicts.

use crate::{
    app::App,
    app_overlay::{Action, Item},
    git,
    gitx::{self, Pick, Row, RowKind},
    theme::Rgba,
    ui::{Canvas, Rect, line_h},
};
use std::path::{Path, PathBuf};
use winit::event::MouseScrollDelta;

pub struct DiffView {
    pub title: String,
    pub left_name: String,
    pub right_name: String,
    pub rows: Vec<Row>,
    pub scroll: f64, // rows
}

impl App {
    // ───────────── hunks ─────────────

    fn hunk_context(&mut self) -> Option<(String, String, gitx::Hunk, PathBuf, String)> {
        let d = self.docs.get(self.active)?;
        let Some(base) = d.git_base.clone() else {
            self.say("arquivo sem versão no HEAD (não rastreado)");
            return None;
        };
        let cur = d.buf.to_string();
        let (line, _) = d.buf.line_col(d.ed.head());
        let hs = gitx::hunks(&base, &cur);
        let Some(h) = gitx::hunk_at_line(&hs, line).cloned() else {
            self.say("nenhuma alteração sob o cursor");
            return None;
        };
        let path = d.path()?.to_path_buf();
        let top = git::toplevel(path.parent()?).unwrap_or_else(|| self.root.clone());
        let rel = path.strip_prefix(&top).ok()?.to_string_lossy().into_owned();
        Some((base, cur, h, top, rel))
    }

    pub fn git_stage_hunk(&mut self) {
        let Some((base, cur, h, top, rel)) = self.hunk_context() else { return };
        match gitx::stage_hunk(&top, &rel, &base, &cur, &h) {
            Ok(()) => {
                self.refresh_git_status();
                self.say("trecho adicionado ao stage");
            }
            Err(e) => self.say(format!("git: {e}")),
        }
    }

    pub fn git_revert_hunk(&mut self) {
        let Some((base, cur, h, _, _)) = self.hunk_context() else { return };
        let text = gitx::revert_hunk_text(&base, &cur, &h);
        if let Some(d) = self.docs.get_mut(self.active) {
            d.replace_text(&text);
            d.reveal = true;
        }
        self.say("trecho revertido (Ctrl+Z desfaz)");
    }

    // ───────────── diff view ─────────────

    pub fn open_diff_head(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let base = d.git_base.clone().unwrap_or_default();
        let cur = d.buf.to_string();
        let rows = gitx::side_by_side(&base, &cur);
        if rows.iter().all(|r| r.kind == RowKind::Same) {
            return self.say("sem diferenças em relação ao HEAD");
        }
        self.diff = Some(DiffView { title: format!("{} — HEAD vs. arquivo atual", d.name), left_name: "HEAD".into(), right_name: "Atual".into(), rows, scroll: 0.0 });
    }

    pub fn open_diff_commit(&mut self, path: &Path, commit: &str, subject: &str) {
        let old = gitx::parent_file_at(path, commit).unwrap_or_default();
        let new = gitx::file_at(path, commit).unwrap_or_default();
        let rows = gitx::side_by_side(&old, &new);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.diff = Some(DiffView { title: format!("{name} — {commit}  {subject}"), left_name: format!("{commit}^"), right_name: commit.to_string(), rows, scroll: 0.0 });
    }

    pub fn open_file_history(&mut self) {
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf)) else { return };
        let log = gitx::file_log(&path, 200);
        if log.is_empty() {
            return self.say("sem histórico git para este arquivo");
        }
        let items: Vec<Item> = log
            .into_iter()
            .map(|c| Item { label: c.subject.clone(), detail: format!("{} · {} · {}", c.short, c.author, c.when), action: Action::GitShow(path.clone(), c.hash, c.subject) })
            .collect();
        self.open_locations("Histórico do arquivo", items);
    }

    /// Keys while a diff is shown. Returns true when consumed.
    pub fn diff_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(dv) = self.diff.as_mut() else { return false };
        let page = (self.layout.editor.h / line_h() as i32 - 3).max(1) as f64;
        let max = (dv.rows.len() as f64 - ((self.layout.editor.h - 28) / line_h() as i32) as f64).max(0.0);
        match key {
            Key::Named(NamedKey::Escape) => self.diff = None,
            Key::Character(c) if c.as_str() == "q" => self.diff = None,
            Key::Named(NamedKey::ArrowDown) => dv.scroll = (dv.scroll + 1.0).min(max),
            Key::Named(NamedKey::ArrowUp) => dv.scroll = (dv.scroll - 1.0).max(0.0),
            Key::Named(NamedKey::PageDown) => dv.scroll = (dv.scroll + page).min(max),
            Key::Named(NamedKey::PageUp) => dv.scroll = (dv.scroll - page).max(0.0),
            Key::Named(NamedKey::Home) => dv.scroll = 0.0,
            Key::Named(NamedKey::End) => dv.scroll = max,
            Key::Character(c) if c.as_str() == "n" => {
                // Next changed block.
                let start = dv.scroll as usize + 1;
                if let Some(i) = (start..dv.rows.len()).find(|&i| dv.rows[i].kind != RowKind::Same && (i == 0 || dv.rows[i - 1].kind == RowKind::Same)) {
                    dv.scroll = (i as f64).min(max);
                }
            }
            _ => return true, // swallow everything else while the diff is open
        }
        true
    }

    pub fn diff_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let Some(dv) = self.diff.as_mut() else { return false };
        let rows = match delta {
            MouseScrollDelta::LineDelta(_, y) => -(y as f64) * 3.0,
            MouseScrollDelta::PixelDelta(p) => -p.y / line_h() as f64,
        };
        let max = (dv.rows.len() as f64 - ((self.layout.editor.h - 28) / line_h() as i32) as f64).max(0.0);
        dv.scroll = (dv.scroll + rows).clamp(0.0, max);
        true
    }

    pub fn draw_diff(&mut self, cv: &mut Canvas) {
        let editor = self.layout.editor;
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
        self.tx.draw_ui(cv, editor.x + 14, editor.y + 5, &format!("{}   ·   {}", dv.left_name, dv.title), fg, Rect::new(editor.x, editor.y, half, head_h), true);
        self.tx.draw_ui(cv, editor.x + half + 14, editor.y + 5, &format!("{}   ·   Esc fecha · n próximo bloco", dv.right_name), dim, Rect::new(editor.x + half, editor.y, editor.w - half, head_h), false);

        let lh = line_h() as i32;
        let first = dv.scroll as usize;
        let n = (lr.h / lh + 2) as usize;
        let cell = self.tx.cell_w;
        let (removed, added, strong_r, strong_a) = (c.diff_del, c.diff_add, c.diff_del_word, c.diff_add_word);
        let digits = dv.rows.len().to_string().len().max(3);
        let num_w = ((digits + 1) as f32 * cell) as i32;
        let max_cols = (((half - num_w - 16) as f32 / cell) as usize).max(4);
        let mut lp: Vec<(String, Rgba)> = Vec::new();
        let mut rp: Vec<(String, Rgba)> = Vec::new();
        for (k, row) in dv.rows.iter().enumerate().skip(first).take(n) {
            let y = lr.y + (k - first) as i32 * lh;
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
                    for (a, b) in lrg {
                        cv.fill_clip(Rect::new(lr.x + num_w + 8 + (a as f32 * cell) as i32, y, ((b - a) as f32 * cell) as i32, lh), strong_r, lr);
                    }
                    for (a, b) in rrg {
                        cv.fill_clip(Rect::new(rr.x + num_w + 8 + (a as f32 * cell) as i32, y, ((b - a) as f32 * cell) as i32, lh), strong_a, rr);
                    }
                }
            }
            for (side, region, pieces) in [(&row.left, lr, &mut lp), (&row.right, rr, &mut rp)] {
                match side {
                    Some((ln, text)) => {
                        let shown: String = text.chars().take(max_cols).map(|ch| if ch == '\t' { ' ' } else { ch }).collect();
                        self.tx.draw_line(cv, region.x + 6, y, &format!("{:>digits$}", ln + 1), dim, region);
                        pieces.push((shown, fg));
                    }
                    None => pieces.push((String::new(), fg)),
                }
                pieces.push(("\n".into(), fg));
            }
        }
        self.tx.draw(cv, lr.x + num_w + 8, lr.y, &lp, lr);
        self.tx.draw(cv, rr.x + num_w + 8, rr.y, &rp, rr);
        let (total, visible, pos) = (rows_total as f64, (lr.h / lh) as f64, scroll as f64);
        self.region_bar(cv, crate::scrollreg::Reg::Diff, Rect::new(editor.x, lr.y, editor.w, lr.h), total, visible, pos, lh as f64, false);
    }

    // ───────────── conflicts ─────────────

    pub fn resolve_conflict(&mut self, pick: Pick) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let (line, _) = d.buf.line_col(d.ed.head());
        let Some(c) = gitx::conflict_at_line(&d.conflicts, line).cloned() else {
            return self.say("o cursor não está dentro de um conflito de merge");
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
