//! App side of hot exit / crash recovery (`backup.rs`) and persistent undo (`undo_store.rs`):
//! when backups are written and dropped, what quitting stores, and how the next start reopens it.

use crate::{
    app::App,
    backup::{self, Entry, Job, Meta},
    buffer::{Buffer, EditKind, SavedAt},
    doc::Doc,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

/// Above this size a restored text replaces the buffer in one splice instead of a minimal diff
/// (`Doc::replace_text` holds both texts as `char` vectors).
const DIFF_MAX: usize = 4 << 20;

fn meta_of(d: &Doc, order: usize) -> Meta {
    Meta {
        path: d.path().map(Path::to_path_buf),
        name: d.name.clone(),
        disk_mtime: d.disk_mtime,
        conflict: d.conflict,
        order,
    }
}

/// Put `text` in the document as one undoable edit (undo goes back to what was there).
fn set_text(d: &mut Doc, text: &str) {
    if d.buf.len_bytes() <= DIFF_MAX && text.len() <= DIFF_MAX {
        return d.replace_text(text);
    }
    let sel = d.ed.sel;
    d.buf.close_group();
    d.buf.begin_compound(sel);
    let len = d.buf.len_chars();
    d.buf.replace(0, len, text, EditKind::Other, sel);
    let len = d.buf.len_chars();
    d.ed.sel = crate::buffer::Sel { anchor: sel.anchor.min(len), head: sel.head.min(len) };
    d.buf.end_compound(d.ed.sel);
    d.sync_edits();
}

/// "a, b, c and 2 more".
fn list_names(names: &[String]) -> String {
    let shown: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
    let more = if names.len() > 3 { format!(" and {} more", names.len() - 3) } else { String::new() };
    format!("{}{more}", shown.join(", "))
}

impl App {
    /// Back up unsaved buffers a moment after the last edit, and drop the backups of buffers that
    /// were saved, closed or undone back to the saved text (from `tick`).
    pub fn backup_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let st = &mut self.backup;
        st.flush_deferred();
        if st.failed.swap(false, Ordering::AcqRel) {
            // A write failed: write everything again on the next pass.
            for e in st.written.values_mut() {
                e.version = u64::MAX;
            }
        }
        // No allocation unless something is stale (this runs on every tick).
        let stale: Vec<u64> = st
            .written
            .keys()
            .filter(|&&g| !self.docs.iter().any(|d| d.buf.gen_id() == g && d.buf.is_dirty()))
            .copied()
            .collect();
        for g in stale {
            if let Some(e) = st.written.remove(&g) {
                st.delete(e.file);
            }
        }
        if !st.deferred.is_empty() {
            want(now + Duration::from_millis(200));
        }
        if !self.config.hot_exit {
            (st.stamp, st.since, st.due) = (0, None, None);
            return;
        }
        // Fingerprint of the edits that have no backup yet: it changes with every edit.
        let mut stamp = 0u64;
        for d in &self.docs {
            let (g, v) = (d.buf.gen_id(), d.buf.version());
            if d.buf.is_dirty() && st.written.get(&g).is_none_or(|e| e.version != v) {
                stamp = (stamp.wrapping_mul(0x100000001b3) ^ g ^ v.rotate_left(32)) | 1;
            }
        }
        if stamp == 0 {
            (st.stamp, st.since, st.due) = (0, None, None);
            return;
        }
        if stamp != st.stamp {
            st.stamp = stamp;
            let since = *st.since.get_or_insert(now);
            st.due = Some((now + backup::DELAY).min(since + backup::MAX_WAIT));
        }
        let due = st.due.unwrap_or(now);
        if due > now || st.busy.load(Ordering::Acquire) {
            want(due.max(now + Duration::from_millis(100)));
            return;
        }
        let Some(dir) = backup::project_dir(&self.root) else { return };
        let mut jobs = Vec::new();
        for (order, d) in self.docs.iter().enumerate() {
            let (g, v) = (d.buf.gen_id(), d.buf.version());
            if !d.buf.is_dirty() || st.written.get(&g).is_some_and(|e| e.version == v) {
                continue;
            }
            let file = st.written.get(&g).map_or_else(|| backup::file_name(&dir, d.path()), |e| e.file.clone());
            // O(1): the rope shares its nodes; the worker does the writing.
            jobs.push(Job { file: file.clone(), meta: meta_of(d, order), text: d.buf.snapshot() });
            st.written.insert(g, Entry { file, version: v });
        }
        (st.stamp, st.since, st.due) = (0, None, None);
        st.spawn(jobs);
    }

    /// Quitting with hot exit: write every unsaved buffer now and mark the set as a normal exit.
    /// False when something could not be written (the caller then asks as before).
    pub fn hot_exit_store(&mut self) -> bool {
        let Some(dir) = backup::project_dir(&self.root) else { return false };
        self.backup.wait_idle(Duration::from_secs(3));
        self.backup.flush_deferred();
        let mut keep: Vec<PathBuf> = Vec::new();
        for (order, d) in self.docs.iter().enumerate() {
            if !d.buf.is_dirty() {
                continue;
            }
            let g = d.buf.gen_id();
            let file =
                self.backup.written.get(&g).map_or_else(|| backup::file_name(&dir, d.path()), |e| e.file.clone());
            if backup::write(&file, &meta_of(d, order), &d.buf.snapshot()).is_err() {
                return false;
            }
            self.backup.written.insert(g, Entry { file: file.clone(), version: d.buf.version() });
            keep.push(file);
        }
        let refs: Vec<&Path> = keep.iter().map(PathBuf::as_path).collect();
        backup::sweep(&dir, &refs);
        keep.is_empty() || backup::mark_clean_exit(&dir).is_ok()
    }

    /// Quitting without hot exit (unsaved changes were discarded): no backup may come back.
    pub fn backup_discard_all(&mut self) {
        self.backup.wait_idle(Duration::from_secs(3));
        self.backup.flush_deferred();
        for (_, e) in self.backup.written.drain() {
            let _ = fs::remove_file(e.file);
        }
        if let Some(dir) = backup::project_dir(&self.root) {
            backup::sweep(&dir, &[]);
            backup::take_clean_exit(&dir);
        }
    }

    /// Does "Restart to update" have to wait for the user to save?
    pub fn restart_needs_save(&self) -> bool {
        !self.config.hot_exit && self.docs.iter().any(|d| d.buf.is_dirty())
    }

    /// Reopen, as unsaved tabs, the buffers backed up by the last run of this project (a hot exit
    /// or a crash), and say so.
    pub fn restore_backups(&mut self) {
        let Some(dir) = backup::project_dir(&self.root) else { return };
        let clean = backup::take_clean_exit(&dir);
        let items = backup::read_all(&dir);
        if items.is_empty() {
            return;
        }
        let active = (!self.docs.is_empty()).then_some(self.active);
        let (mut names, mut changed) = (Vec::new(), Vec::new());
        let mut first = None;
        for (file, meta, text) in items {
            let Some((i, moved)) = self.restore_backup(&meta, &text) else {
                let _ = fs::remove_file(&file);
                continue;
            };
            let d = &self.docs[i];
            names.push(d.name.clone());
            if moved {
                changed.push(d.name.clone());
            }
            self.backup.written.insert(d.buf.gen_id(), Entry { file, version: d.buf.version() });
            first.get_or_insert(i);
        }
        if let Some(i) = active.or(first) {
            self.activate(i);
        }
        if names.is_empty() {
            return;
        }
        if !changed.is_empty() {
            self.say_for(
                format!(
                    "{} changed on disk since the unsaved changes were kept — your version is open (Ctrl+S overwrites)",
                    list_names(&changed)
                ),
                10,
            );
        } else if clean {
            self.say(format!("Restored unsaved changes: {}", list_names(&names)));
        } else {
            self.say_for(format!("Recovered unsaved changes after an unexpected exit: {}", list_names(&names)), 6);
        }
    }

    /// Reopen one backup. Returns the document and whether its file changed on disk meanwhile;
    /// `None` when there is nothing to restore (the backup equals the file).
    fn restore_backup(&mut self, meta: &Meta, text: &str) -> Option<(usize, bool)> {
        let i = match &meta.path {
            Some(p) if p.exists() => {
                if fs::read(p).is_ok_and(|b| b == text.as_bytes()) {
                    return None;
                }
                self.open_path(p, None)?
            }
            // Untitled, or the file is gone: an unsaved tab with that path (saving recreates it).
            _ if text.is_empty() => return None,
            Some(p) => {
                let i = self.push_restored_doc(&meta.name);
                self.set_doc_path(i, p);
                i
            }
            None => self.push_restored_doc(&meta.name),
        };
        let persistent_undo = self.config.persistent_undo;
        let d = &mut self.docs[i];
        if d.buf.is_dirty() || d.buf.to_string() == text {
            return None;
        }
        let path = d.path().map(Path::to_path_buf);
        let unchanged = !meta.conflict && meta.disk_mtime.is_some() && meta.disk_mtime == d.disk_mtime;
        set_text(d, text);
        if persistent_undo && let (Some(p), Some(dir)) = (path.as_deref(), crate::undo_store::dir()) {
            // The history saved at quit belongs to this very text.
            let saved = if unchanged { SavedAt::Recorded } else { SavedAt::Unknown };
            crate::undo_store::load(&dir, p, &mut d.buf, saved);
        }
        let moved = path.is_some() && !unchanged;
        d.conflict = moved;
        Some((i, moved))
    }

    fn push_restored_doc(&mut self, name: &str) -> usize {
        let mut d = Doc::new(Buffer::from_rope(ropey::Rope::new(), None), None, None, "//", self.tx.cell_w);
        d.name = name.to_string();
        d.minimap = self.config.minimap;
        d.indent_guides = self.config.indent_guides;
        d.sticky = self.config.sticky_scroll;
        d.set_word_wrap(self.config.word_wrap);
        self.docs.push(d);
        self.docs.len() - 1
    }

    /// Give a file that was just opened (text straight from disk) its persisted undo history.
    pub fn undo_restore(&self, path: &Path, buf: &mut Buffer) {
        if self.config.persistent_undo
            && let Some(dir) = crate::undo_store::dir()
        {
            crate::undo_store::load(&dir, path, buf, SavedAt::Current);
        }
    }

    /// Persist document `i`'s undo history (it is being closed, or the app quits). An unsaved
    /// buffer only with `dirty_too` (hot exit reopens that very text; discarded text never comes back).
    pub fn undo_persist(&self, i: usize, dirty_too: bool) {
        if !self.config.persistent_undo {
            return;
        }
        let Some(d) = self.docs.get(i) else { return };
        let (Some(p), Some(dir)) = (d.path(), crate::undo_store::dir()) else { return };
        if d.buf.is_dirty() && !dirty_too {
            return;
        }
        let _ = crate::undo_store::save(&dir, p, &d.buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp, untitled};

    /// A second run of the app on `dir` (after [`test_app`] set up the environment): no files on
    /// the command line, so the session and the backups are restored.
    fn reopen(dir: &Path) -> App {
        let assets = crate::config::assets_dir();
        let theme = crate::theme::Theme::load(&assets.join("themes").join("github-dark-default.json")).expect("theme");
        let wake: crate::lsp::Wake = std::sync::Arc::new(|| {});
        let mut app = App::new(Vec::new(), dir.to_path_buf(), theme, wake, crate::config::Config::default());
        app.clip = crate::clipboard::Clipboard::in_process();
        app
    }

    fn status(app: &App) -> String {
        app.status.as_ref().map(|s| s.0.clone()).unwrap_or_default()
    }

    /// Run the periodic backup as if the user stopped typing long ago, and wait for the writes.
    fn backup_now(app: &mut App) {
        let mut want = |_: Instant| {};
        let t = Instant::now();
        app.backup_tick(t, &mut want);
        app.backup_tick(t + Duration::from_secs(60), &mut want);
        app.backup.wait_idle(Duration::from_secs(10));
        app.backup_tick(t + Duration::from_secs(61), &mut want);
    }

    fn seed_idx(app: &App) -> usize {
        app.docs.iter().position(|d| d.name == "seed.txt").expect("seed.txt open")
    }

    #[test]
    fn hot_exit_quits_without_asking_and_restores_unsaved_tabs() {
        let _g = lock();
        let dir = tmp("hotexit");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("edited\n");
        let u = untitled(&mut app, "scratch\n");
        app.docs[u].name = "notes".into();
        assert!(app.try_quit(), "hot exit: no question");
        drop(app);
        assert_eq!(fs::read_to_string(dir.join("seed.txt")).unwrap(), "seed\n", "the file itself is untouched");
        let mut app = reopen(&dir);
        let i = seed_idx(&app);
        assert_eq!(app.docs[i].buf.to_string(), "edited\n");
        assert!(app.docs[i].buf.is_dirty() && !app.docs[i].conflict);
        let n = app.docs.iter().position(|d| d.name == "notes").expect("untitled tab restored");
        assert!(app.docs[n].path().is_none() && app.docs[n].buf.is_dirty());
        assert_eq!(app.docs[n].buf.to_string(), "scratch\n");
        assert!(status(&app).contains("Restored unsaved changes"), "{}", status(&app));
        // The undo history came back with the text: one undo reaches the saved file again.
        app.docs[i].buf.undo();
        assert_eq!(app.docs[i].buf.to_string(), "seed\n");
        assert!(!app.docs[i].buf.is_dirty(), "the saved point survived the restart");
        app.docs[i].buf.redo();
        // Saving drops the backup; closing the untitled tab without saving drops its backup too.
        app.save_doc(i).unwrap();
        let n = app.docs.iter().position(|d| d.name == "notes").unwrap();
        app.close_tab(n, true);
        backup_now(&mut app);
        let bdir = backup::project_dir(&dir).unwrap();
        assert!(backup::read_all(&bdir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_loses_only_the_last_seconds() {
        let _g = lock();
        let dir = tmp("crash");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("typed before the crash\n");
        let mut want = |_: Instant| {};
        app.backup_tick(Instant::now(), &mut want);
        assert!(app.backup.written.is_empty(), "nothing is written while the user types");
        backup_now(&mut app);
        let bdir = backup::project_dir(&dir).unwrap();
        assert_eq!(backup::read_all(&bdir).len(), 1);
        drop(app); // killed: no quit, no session
        let app = reopen(&dir);
        let i = seed_idx(&app);
        assert_eq!(app.docs[i].buf.to_string(), "typed before the crash\n");
        assert!(app.docs[i].buf.is_dirty());
        assert!(status(&app).contains("unexpected exit"), "{}", status(&app));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_changed_on_disk_keeps_the_backup_and_says_so() {
        let _g = lock();
        let dir = tmp("bakdisk");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("mine\n");
        backup_now(&mut app);
        drop(app);
        let seed = dir.join("seed.txt");
        fs::write(&seed, "theirs\n").unwrap();
        let f = fs::File::options().append(true).open(&seed).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
        drop(f);
        let mut app = reopen(&dir);
        let i = seed_idx(&app);
        assert_eq!(app.docs[i].buf.to_string(), "mine\n", "the unsaved version wins");
        assert!(app.docs[i].buf.is_dirty() && app.docs[i].conflict);
        assert!(status(&app).contains("changed on disk"), "{}", status(&app));
        app.docs[i].buf.undo();
        assert_eq!(app.docs[i].buf.to_string(), "theirs\n", "undo shows what is on disk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_backup_equal_to_the_file_is_dropped() {
        let _g = lock();
        let dir = tmp("bakeq");
        let mut app = test_app(&dir);
        app.docs[0].replace_text("same\n");
        backup_now(&mut app);
        drop(app);
        fs::write(dir.join("seed.txt"), "same\n").unwrap();
        let app = reopen(&dir);
        assert!(app.docs.iter().all(|d| !d.buf.is_dirty()));
        assert!(backup::read_all(&backup::project_dir(&dir).unwrap()).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_hot_exit_quitting_still_asks_and_nothing_is_kept() {
        let _g = lock();
        let dir = tmp("nohot");
        let mut app = test_app(&dir);
        app.config.hot_exit = false;
        app.docs[0].replace_text("x\n");
        assert!(app.restart_needs_save());
        backup_now(&mut app);
        assert!(app.backup.written.is_empty(), "no periodic backups");
        assert!(!app.try_quit(), "first Ctrl+Q only warns");
        assert!(app.try_quit(), "second one discards");
        assert!(backup::read_all(&backup::project_dir(&dir).unwrap()).is_empty());
        app.config.hot_exit = true;
        assert!(!app.restart_needs_save(), "with hot exit the update restart needs no save");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_history_survives_closing_the_file_while_the_text_matches() {
        let _g = lock();
        let dir = tmp("pundo");
        let mut app = test_app(&dir);
        let seed = dir.join("seed.txt");
        app.docs[0].replace_text("v2\n");
        app.save_doc(0).unwrap();
        app.close_tab(0, false);
        assert!(app.docs.is_empty());
        let i = app.open_path(&seed, None).unwrap();
        assert!(!app.docs[i].buf.is_dirty());
        app.docs[i].buf.undo();
        assert_eq!(app.docs[i].buf.to_string(), "seed\n", "undo reaches the text before the reopen");
        app.docs[i].buf.redo();
        app.close_tab(i, false);
        // Changed by another program: the old history does not apply any more.
        fs::write(&seed, "other\n").unwrap();
        let i = app.open_path(&seed, None).unwrap();
        assert!(app.docs[i].buf.undo().is_none());
        app.close_tab(i, false);
        // Switched off: nothing is loaded.
        fs::write(&seed, "v2\n").unwrap();
        let i = app.open_path(&seed, None).unwrap();
        app.docs[i].replace_text("v3\n");
        app.save_doc(i).unwrap();
        app.config.persistent_undo = false;
        app.close_tab(i, false);
        let i = app.open_path(&seed, None).unwrap();
        assert!(app.docs[i].buf.undo().is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
