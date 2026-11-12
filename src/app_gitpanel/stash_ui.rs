//! App glue for stashes: the "Stash…" flow (message prompt, then the include-untracked question
//! when untracked files are involved), the file selection it works on (Ctrl / Shift + click in
//! the Changes list), the Stashes rows of the Changes tab (expand to list the files and show their
//! diff, Apply / Pop / Drop), the palette commands and the conflict hand-off: a stash that applies
//! with conflicts opens the first conflicting file, where the editor's conflict blocks (accept
//! current / incoming / both) take over. The git work itself is in `stash.rs`.

use super::*;
use crate::app_overlay::{Action, Item, Overlay, choice_picker};

/// Card of a stash chosen in the "Stashes…" picker.
pub fn stash_card(e: &StashEntry) -> Ask {
    let what = if e.message.is_empty() { e.name() } else { format!("{} “{}”", e.name(), e.message) };
    let on = if e.branch.is_empty() { String::new() } else { format!(", made on {}", e.branch) };
    Ask {
        text: format!("{what}{on}. Apply keeps the stash; Pop applies it and then drops it."),
        warn: false,
        buttons: vec![
            btn("Apply", GitAct::StashApply { sha: e.sha.clone(), pop: false }, Tone::Accent),
            btn("Pop", GitAct::StashApply { sha: e.sha.clone(), pop: true }, Tone::Accent),
            btn("Drop…", GitAct::StashDropAsk { sha: e.sha.clone() }, Tone::Danger),
            cancel(),
        ],
    }
}

/// Card asking before a stash is dropped.
pub fn stash_drop_card(e: &StashEntry) -> Ask {
    let what = if e.message.is_empty() { e.name() } else { format!("{} “{}”", e.name(), e.message) };
    Ask {
        text: format!("Drop {what}? Its changes are deleted (only git fsck could still find them)."),
        warn: true,
        buttons: vec![btn("Drop stash", GitAct::StashDrop { sha: e.sha.clone() }, Tone::Danger), cancel()],
    }
}

/// Second line of a stash row / detail of a picker row: "stash@{0} · main · 2 days ago".
pub fn stash_detail(e: &StashEntry, now: i64) -> String {
    let mut v = vec![e.name()];
    if !e.branch.is_empty() {
        v.push(e.branch.clone());
    }
    v.push(git::ago((now - e.ts).max(0)));
    v.join("  ·  ")
}

/// `git status` of `top`, parsed.
fn status_of(top: &Path) -> Vec<Change> {
    git_run(top, &["status", "--porcelain=v1", "-z", "-uall"]).map(|o| parse_status_z(&o)).unwrap_or_default()
}

impl GitPanel {
    /// Click on a file row with Ctrl (toggle it) or Shift (select from the anchor to it, in
    /// list order). Selected files are what "Stash" takes.
    pub fn select_change(&mut self, i: usize, ctrl: bool, shift: bool) {
        let Some(path) = self.changes.get(i).map(|c| c.path.clone()) else { return };
        match self.sel_anchor.filter(|_| shift) {
            Some(a) => {
                let order: Vec<usize> =
                    self.rows().into_iter().filter_map(|r| if let Row::File(f) = r { Some(f) } else { None }).collect();
                let (pa, pi) = (order.iter().position(|&f| f == a), order.iter().position(|&f| f == i));
                if let (Some(pa), Some(pi)) = (pa, pi) {
                    if !ctrl {
                        self.selected.clear();
                    }
                    for &f in &order[pa.min(pi)..=pa.max(pi)] {
                        self.selected.insert(self.changes[f].path.clone());
                    }
                }
            }
            None => {
                if !self.selected.remove(&path) {
                    self.selected.insert(path);
                }
                self.sel_anchor = Some(i);
            }
        }
    }
}

impl App {
    /// What the stash commands take: the files selected in the panel, else everything (empty).
    pub(super) fn stash_targets(&self) -> Vec<String> {
        self.gitpanel.as_ref().map(|gp| gp.selected.iter().cloned().collect()).unwrap_or_default()
    }

    /// "Stash File(s)…" in the menu of the changed files (absolute paths).
    pub(crate) fn git_stash_files(&mut self, paths: &[PathBuf]) {
        let Some(top) = self.git_top() else { return };
        let rels: Vec<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(&top).ok())
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .collect();
        if rels.is_empty() {
            return;
        }
        self.git_stash_start(rels, None);
    }

    /// Palette "Git: Stash…" (`untracked`: "…including untracked").
    pub fn git_stash_cmd(&mut self, untracked: bool) {
        let paths = self.stash_targets();
        self.git_stash_start(paths, untracked.then_some(true));
    }

    /// Ask for the stash message (`paths` empty = all changes).
    pub(super) fn git_stash_start(&mut self, paths: Vec<String>, untracked: Option<bool>) {
        let Some(top) = self.git_top() else { return };
        if status_of(&top).is_empty() {
            return self.git_report("No local changes to stash", false);
        }
        let what = match paths.len() {
            0 => "all changes".to_string(),
            1 => paths[0].clone(),
            n => format!("{n} selected files"),
        };
        let extra = if untracked == Some(true) { " (untracked files included)" } else { "" };
        let title = format!("Stash {what}{extra}: message (optional, Enter to stash)");
        self.git_prompt(GitAsk::StashMessage { paths, untracked }, title, "");
    }

    /// Enter in the stash message prompt: stash, or first ask about untracked files.
    pub(super) fn git_stash_message(&mut self, top: &Path, paths: Vec<String>, untracked: Option<bool>, text: String) {
        let message = text.trim().to_string();
        let untracked = match untracked {
            Some(u) => u,
            None => {
                let changes = status_of(top);
                let loose: Vec<&str> =
                    changes.iter().filter(|c| c.group == Group::Untracked).map(|c| c.path.as_str()).collect();
                let involved = if paths.is_empty() {
                    loose.len()
                } else {
                    paths.iter().filter(|p| loose.contains(&p.as_str())).count()
                };
                if involved == 0 {
                    false
                } else if !paths.is_empty() && involved == paths.len() {
                    // Only untracked files were selected: stashing them is the point.
                    true
                } else {
                    let files = if involved == 1 { "1 untracked file is" } else { "untracked files are" };
                    let n = if involved == 1 { String::new() } else { format!("{involved} ") };
                    return self.git_ask(Ask {
                        text: format!(
                            "{n}{files} involved. Include them in the stash? They leave the folder until the stash is applied."
                        ),
                        warn: false,
                        buttons: vec![
                            btn(
                                "Include untracked",
                                GitAct::Stash { message: message.clone(), paths: paths.clone(), untracked: true },
                                Tone::Accent,
                            ),
                            btn("Tracked only", GitAct::Stash { message, paths, untracked: false }, Tone::Accent),
                            cancel(),
                        ],
                    });
                }
            }
        };
        self.git_act(GitAct::Stash { message, paths, untracked });
    }

    /// `git stash push`, then refresh everything that shows the files.
    pub(super) fn git_stash_run(&mut self, top: &Path, message: &str, mut paths: Vec<String>, untracked: bool) {
        if !untracked && !paths.is_empty() {
            // Without --include-untracked git refuses pathspecs it does not track.
            let changes = status_of(top);
            paths.retain(|p| !changes.iter().any(|c| c.group == Group::Untracked && &c.path == p));
            if paths.is_empty() {
                return self
                    .git_report("Only untracked files are selected: include untracked files to stash them", false);
            }
        }
        match stash_push(top, message, &paths, untracked) {
            Ok(m) => {
                if let Some(gp) = self.gitpanel.as_mut() {
                    gp.selected.clear();
                    gp.sel_anchor = None;
                }
                self.diff = None;
                self.git_refs_changed(true);
                self.git_report(m, true);
            }
            Err(e) => self.git_report(format!("git stash: {e}"), false),
        }
    }

    /// Apply (or pop) a stash; conflicts open the first conflicting file.
    pub(super) fn git_stash_apply(&mut self, top: &Path, sha: &str, pop: bool) {
        match stash_apply(top, sha, pop) {
            Ok(m) => {
                self.diff = None;
                self.git_refs_changed(true);
                self.git_report(m, true);
            }
            Err(StashFail::Conflict(files)) => {
                self.diff = None;
                self.git_refs_changed(true);
                self.git_stash_conflicts(top, &files, pop);
            }
            Err(StashFail::Other(e)) => {
                self.git_report(format!("git stash {}: {e}", if pop { "pop" } else { "apply" }), false)
            }
        }
    }

    /// A stash applied with conflicts: show the conflicting files in the panel, reload the open
    /// ones and open the first, where the conflict blocks can be resolved (accept current /
    /// incoming / both); staging a file marks it resolved.
    fn git_stash_conflicts(&mut self, top: &Path, files: &[String], pop: bool) {
        if self.gitpanel.is_none() {
            self.open_git_panel();
        }
        let now = Instant::now();
        let abs: Vec<PathBuf> =
            files.iter().map(|f| crate::paths::canonical(&top.join(f)).unwrap_or_else(|_| top.join(f))).collect();
        for d in &mut self.docs {
            if d.path().is_some_and(|p| abs.iter().any(|a| a == p)) {
                if !d.buf.is_dirty() {
                    let _ = d.reload_from_disk();
                }
                d.conflicts_due = Some(now);
            }
        }
        if let Some(first) = abs.first()
            && let Some(k) = self.open_path(first, None)
        {
            self.docs[k].conflicts_due = Some(now);
        }
        if let Some(gp) = self.gitpanel.as_mut() {
            gp.tab = Tab::Changes;
            // Keys go to the editor, where the conflict is.
            gp.focused = false;
            gp.msg_focus = false;
        }
        let names: Vec<&str> = files.iter().take(3).map(|f| split_name(f).0).collect();
        let more = if files.len() > 3 { format!(" and {} more", files.len() - 3) } else { String::new() };
        let kept = if pop { "The stash was kept: drop it once you are done." } else { "The stash was kept." };
        self.git_report(
            format!(
                "Stash applied with conflicts in {}{more}: resolve the marked blocks (Git: Conflict — accept current / incoming), then stage the files. {kept}",
                names.join(", ")
            ),
            false,
        );
    }

    /// Palette "Git: Stash Pop": the newest stash.
    pub fn git_stash_pop_latest(&mut self) {
        let Some(top) = self.git_top() else { return };
        match list_stashes(&top).into_iter().next() {
            Some(e) => self.git_act(GitAct::StashApply { sha: e.sha, pop: true }),
            None => self.say("No stashes"),
        }
    }

    /// Palette "Git: Stashes…": choose one, then Apply / Pop / Drop from its card.
    pub fn git_stash_list(&mut self) {
        let Some(top) = self.git_top() else { return };
        let now = now_secs();
        let items: Vec<Item> = list_stashes(&top)
            .into_iter()
            .map(|e| Item {
                label: if e.message.is_empty() { e.name() } else { e.message.clone() },
                detail: stash_detail(&e, now),
                action: Action::Git(GitPick::Stash(e.sha)),
            })
            .collect();
        if items.is_empty() {
            return self.say("No stashes");
        }
        self.overlay = Overlay::Picker(choice_picker("Stashes…", items, 0));
    }

    /// A stash chosen in the picker: expanded in the panel (files and diff), with its card.
    pub(super) fn git_stash_show(&mut self, sha: &str) {
        if self.gitpanel.is_none() {
            self.open_git_panel();
        }
        let Some(mut gp) = self.gitpanel.take() else { return };
        gp.tab = Tab::Changes;
        gp.focused = true;
        gp.msg_focus = false;
        let found = gp.stashes.iter().position(|e| e.sha == sha);
        if let Some(i) = found
            && gp.open_stash != Some(i)
        {
            self.gp_toggle_stash(&mut gp, i);
        }
        let entry = found.and_then(|i| gp.stashes.get(i).cloned());
        self.gitpanel = Some(gp);
        match entry {
            Some(e) => self.git_ask(stash_card(&e)),
            None => self.git_report("That stash no longer exists", false),
        }
    }

    // ───────────── panel rows ─────────────

    /// Expand (or collapse) stash `i`: list its files and show the first one's diff.
    pub(super) fn gp_toggle_stash(&mut self, gp: &mut GitPanel, i: usize) {
        if gp.open_stash == Some(i) {
            gp.open_stash = None;
            gp.stash_files.clear();
            gp.open_sfile = None;
            return;
        }
        let (Some(top), Some(e)) = (gp.top.clone(), gp.stashes.get(i).cloned()) else { return };
        gp.stash_files = stash_files(&top, &e.sha);
        gp.open_stash = Some(i);
        gp.open_sfile = None;
        if let Some(k) = gp.index_of(Row::Stash(i)) {
            gp.cursor = k;
            let view = self.gitpanel_rect().h - gp.chrome_h() - gp.commit_area_h();
            gp.reveal_span(k, k + gp.stash_files.len(), view);
        }
        if gp.stash_files.is_empty() {
            self.diff = None;
        } else {
            self.gp_open_diff_stash(gp, 0);
        }
    }

    pub(super) fn gp_open_diff_stash(&mut self, gp: &mut GitPanel, j: usize) {
        let (Some(top), Some(i)) = (gp.top.clone(), gp.open_stash) else { return };
        let (Some(f), Some(e)) = (gp.stash_files.get(j).cloned(), gp.stashes.get(i).cloned()) else { return };
        let (old, new) = stash_pair(&top, &e.sha, &f);
        let rows = gitx::side_by_side(&old, &new);
        let name = split_name(&f.path).0.to_string();
        let label = if e.message.is_empty() { e.name() } else { format!("{}  {}", e.name(), e.message) };
        let left = if f.code == '?' { "(untracked)".to_string() } else { format!("{}^", e.name()) };
        self.diff = Some(DiffView::plain(format!("{name} — {label}"), left, e.name(), rows));
        gp.open_sfile = Some(j);
    }

    /// Draw a row of the Stashes section (`k` = row index, for the keyboard cursor).
    pub(super) fn gp_draw_stash_row(
        &mut self,
        cv: &mut Canvas,
        gp: &mut GitPanel,
        row: Row,
        k: usize,
        y: i32,
        body: Rect,
    ) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let h = GitPanel::row_h(row);
        let clip = body;
        let vis = Rect::new(body.x, y.max(body.y), body.w, (y + h).min(body.bottom()) - y.max(body.y));
        let r = Rect::new(body.x + 6, y, body.w - 12, h);
        let hov = self.hv(r) && self.hv(vis);
        let cur = gp.focused && !gp.msg_focus && gp.cursor == k;
        match row {
            Row::NoChanges => {
                self.tx.draw_ui(cv, body.x + 16, y + 6, "No changes", dim, clip, false);
            }
            Row::StashHead => {
                let n = gp.stashes.len();
                let title = if n > 0 { format!("Stashes  {n}") } else { "Stashes".to_string() };
                self.tx.draw_ui(cv, body.x + 16, y + 9, &title, dim, clip, true);
                if !gp.changes.is_empty() {
                    let label = match gp.selected.len() {
                        0 => "Stash…".to_string(),
                        n => format!("Stash {n} selected…"),
                    };
                    let before = gp.hits.len();
                    self.gp_text_btn(cv, gp, body.right() - 10, y + 5, &label, Hit::StashNew, c.accent_text);
                    if let Some(last) = gp.hits.get_mut(before) {
                        last.0 = last.0.intersect(&vis);
                    }
                }
            }
            Row::Stash(i) => {
                let Some(e) = gp.stashes.get(i).cloned() else { return };
                let open = gp.open_stash == Some(i);
                if cur {
                    cv.fill_round_clip(r, 8, strong.with_alpha(0x1e), clip);
                } else if open {
                    cv.fill_round_clip(r, 8, strong.with_alpha(0x1a), clip);
                } else if hov {
                    cv.fill_round_clip(r, 8, strong.with_alpha(0x12), clip);
                }
                let chev = if open { "ui/chevron_down" } else { "ui/chevron_right" };
                self.tx.icon(cv, chev, r.x + 8, y + 13, 16, dim, vis);
                gp.hits.push((r.intersect(&vis), Hit::Stash(i)));
                // Apply / Pop / Drop on the right while the row is hovered, selected or open.
                let mut right = r.right() - 6;
                if hov || cur || open {
                    let tr = Rect::new(right - 26, y + 9, 24, 24);
                    let th = self.hv(tr);
                    if th {
                        cv.fill_round_clip(tr, 8, c.error.with_alpha(0x22), clip);
                    }
                    self.tx.icon(cv, "ui/trash", tr.x + 4, tr.y + 4, 16, if th { c.error } else { dim }, vis);
                    gp.hits.push((tr.intersect(&vis), Hit::StashDrop(i)));
                    right = tr.x - 2;
                    for (label, hit) in [("Pop", Hit::StashPop(i)), ("Apply", Hit::StashApply(i))] {
                        let before = gp.hits.len();
                        right -= self.gp_text_btn(cv, gp, right, y + 9, label, hit, c.accent_text) + 2;
                        if let Some(last) = gp.hits.get_mut(before) {
                            last.0 = last.0.intersect(&vis);
                        }
                    }
                }
                let text_x = r.x + 30;
                let msg = if e.message.is_empty() { e.name() } else { e.message.clone() };
                let msg = fit(&mut self.tx, &msg, right - text_x - 6, open);
                self.tx.draw_ui(cv, text_x, y + 4, &msg, strong, vis, open);
                let detail = fit(&mut self.tx, &stash_detail(&e, now_secs()), right - text_x - 6, false);
                self.tx.draw_ui_sized(cv, text_x, y + 23, &detail, dim, vis, false, 12.0);
            }
            Row::SFile(j) => {
                let Some(f) = gp.stash_files.get(j).cloned() else { return };
                let sel = gp.open_sfile == Some(j);
                let rr = Rect::new(r.x, y + 1, r.w, h - 2);
                if sel || cur {
                    cv.fill_round_clip(rr, 8, strong.with_alpha(0x1e), clip);
                } else if hov {
                    cv.fill_round_clip(rr, 8, strong.with_alpha(0x12), clip);
                }
                let letter = if f.code == '?' { "U".to_string() } else { f.code.to_string() };
                self.tx.draw_ui(cv, r.x + 34, y + 5, &letter, code_color(&c, f.code), vis, true);
                let (name, dir) = split_name(&f.path);
                let avail = r.w - 76;
                let nm = fit(&mut self.tx, name, avail, false);
                let nw = self.tx.ui_width(&nm, false);
                self.tx.draw_ui(cv, r.x + 56, y + 5, &nm, strong, vis, false);
                if !dir.is_empty() && avail - nw > 40 {
                    let d = fit(&mut self.tx, dir, avail - nw - 10, false);
                    self.tx.draw_ui_sized(cv, r.x + 56 + nw + 8, y + 7, &d, dim, vis, false, 12.0);
                }
                gp.hits.push((rr.intersect(&vis), Hit::SFile(j)));
            }
            _ => {}
        }
    }
}
