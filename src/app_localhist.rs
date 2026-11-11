//! Local history UI on top of `localhist`: snapshot on save / before a reload from disk, the
//! "File: Local history" picker, the side-by-side diff (snapshot vs. current buffer, the git diff
//! view) with Restore, and the list of deleted files that still have history.

use crate::{
    app::App,
    app_gitx::DiffView,
    app_overlay::{Action, Item},
    gitx::{self, RowKind},
    localhist::{self, Source, Store},
};
use std::path::{Path, PathBuf};

/// What the diff's Restore puts back.
#[derive(Clone, Debug)]
pub struct Restore {
    /// Canonical path of the file (see `localhist::canon`).
    pub path: PathBuf,
    pub text: String,
    /// "3 minutes ago", for the status message.
    pub when: String,
}

/// "3 minutes ago" for a snapshot taken at `secs`.
fn ago(secs: i64) -> String {
    crate::git::ago((crate::paths::now_unix() - secs).max(0))
}

impl App {
    /// The history store to write to, when local history is on.
    pub(crate) fn hist_store(&self) -> Option<Store> {
        self.config.local_history.then_some(())?;
        self.hist_reader()
    }

    /// The history store to read from (also when recording is off: what is there stays reachable).
    fn hist_reader(&self) -> Option<Store> {
        Some(Store::new(self.hist_root.clone()?, self.config.local_history_max_entries))
    }

    /// Snapshot document `i`'s text in the background (after a save, before a reload or a restore).
    pub fn hist_snapshot(&self, i: usize, source: Source) {
        let (Some(store), Some(d)) = (self.hist_store(), self.docs.get(i)) else { return };
        if let Some(p) = d.path() {
            localhist::record_buf(&store, p, &d.buf, source);
        }
    }

    /// Index of the open document for `canon` (a canonical path), if any.
    fn hist_doc(&self, canon: &Path) -> Option<usize> {
        self.docs.iter().position(|d| d.path().is_some_and(|p| p == canon || localhist::canon(p) == canon))
    }

    /// "File: Local history": the snapshots of the active file.
    pub fn open_local_history(&mut self) {
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf)) else {
            return self.say("local history: this document has no file yet");
        };
        self.hist_list(&path);
    }

    /// Picker over the snapshots of `path`, newest first.
    pub fn hist_list(&mut self, path: &Path) {
        let Some(store) = self.hist_reader() else { return self.say("local history: no state folder") };
        let canon = localhist::canon(path);
        let entries = store.history(&canon);
        let name = canon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if entries.is_empty() {
            return self.say(if self.config.local_history {
                format!("no local history for {name} yet (a snapshot is kept on every save)")
            } else {
                format!("no local history for {name} (setting local_history is off)")
            });
        }
        let items: Vec<Item> = entries
            .into_iter()
            .map(|e| Item {
                label: format!("{}  ·  {}", ago(e.secs()), e.source.label()),
                detail: format!("{} · {}", crate::app_tooltip::local_datetime(e.secs()), localhist::size_label(e.size)),
                action: Action::LocalHist(canon.clone(), Some(e.file)),
            })
            .collect();
        let title = format!("Local history — {}", self.rel(&canon));
        self.open_locations(&title, items);
    }

    /// "File: Local history (deleted files)": files whose path is gone but whose history is kept.
    pub fn open_local_history_deleted(&mut self) {
        let Some(store) = self.hist_reader() else { return self.say("local history: no state folder") };
        let gone = store.deleted();
        if gone.is_empty() {
            return self.say("no deleted files in the local history");
        }
        let items: Vec<Item> = gone
            .into_iter()
            .map(|(path, newest, n)| Item {
                label: self.rel(&path),
                detail: format!(
                    "deleted · last snapshot {} · {n} version{}",
                    ago(newest.secs()),
                    if n == 1 { "" } else { "s" }
                ),
                action: Action::LocalHist(path, None),
            })
            .collect();
        self.open_locations("Local history — deleted files", items);
    }

    /// A picked snapshot: the diff of the snapshot (left) against the current buffer (right; the
    /// file on disk when it is not open, nothing when it was deleted), with Restore.
    pub fn hist_show(&mut self, path: &Path, file: &str) {
        let Some(store) = self.hist_reader() else { return };
        let Some(entry) = store.history(path).into_iter().find(|e| e.file == file) else {
            return self.say("this snapshot no longer exists");
        };
        let old = match store.read(path, file) {
            Ok(t) => t,
            Err(e) => return self.say(format!("local history: {e}")),
        };
        let (cur, right) = match self.hist_doc(path) {
            Some(i) => (self.docs[i].buf.to_string(), "Current"),
            None => match std::fs::read_to_string(path) {
                Ok(t) => (t, "On disk"),
                Err(_) => (String::new(), "Deleted"),
            },
        };
        let when = ago(entry.secs());
        let rows = gitx::side_by_side(&old, &cur);
        if rows.iter().all(|r| r.kind == RowKind::Same) && right != "Deleted" {
            return self.say(format!("the version from {when} is the same as the current file"));
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let left = format!("{} ({})", crate::app_tooltip::local_datetime(entry.secs()), entry.source.label());
        let mut dv = DiffView::plain(format!("{name} — local history, {when}"), left, right.into(), rows);
        dv.restore = Some(Restore { path: path.to_path_buf(), text: old, when });
        self.diff = Some(dv);
    }

    /// Restore of the open local-history diff: the snapshot replaces the buffer as one undoable
    /// edit (the current text is snapshotted first). A deleted file is written back to disk.
    pub fn hist_restore(&mut self) {
        let Some(r) = self.diff.as_ref().and_then(|d| d.restore.clone()) else { return };
        let i = match self.hist_doc(&r.path) {
            Some(i) => i,
            None => {
                if !r.path.exists() {
                    let made = r.path.parent().map_or(Ok(()), std::fs::create_dir_all);
                    if let Err(e) = made.and_then(|()| std::fs::write(&r.path, &r.text)) {
                        return self.say(format!("could not recreate {}: {e}", r.path.display()));
                    }
                    self.diff = None;
                    self.refresh_tree();
                    self.open_path(&r.path, None);
                    return self.say(format!("Recreated {} from the version of {}", self.rel(&r.path), r.when));
                }
                match self.open_path(&r.path, None) {
                    Some(i) => i,
                    None => return,
                }
            }
        };
        self.hist_snapshot(i, Source::Restore);
        self.diff = None;
        self.activate(i);
        let d = &mut self.docs[i];
        d.replace_text(&r.text);
        d.reveal = true;
        self.say(format!("Restored the version from {} (Ctrl+Z undoes; save to keep it)", r.when));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_overlay::Overlay;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    fn history_app(tag: &str) -> (App, PathBuf) {
        let dir = tmp(tag);
        let mut app = test_app(&dir);
        app.hist_root = Some(dir.join(".hist"));
        (app, dir)
    }

    #[test]
    fn save_snapshots_and_restore_is_one_undoable_edit() {
        let _g = lock();
        let (mut app, dir) = history_app("lh-restore");
        let path = dir.join("seed.txt");
        let i = app.docs.iter().position(|d| d.path() == Some(path.as_path())).unwrap();
        app.activate(i);
        app.docs[i].replace_text("v1\n");
        app.save_active();
        app.docs[i].replace_text("v2\n");
        app.save_active();
        localhist::flush();
        let store = app.hist_reader().unwrap();
        let h = store.history(&path);
        assert_eq!(h.len(), 2, "one snapshot per save");
        assert_eq!(store.read(&path, &h[1].file).unwrap(), "v1\n");

        // The command lists them, newest first.
        app.run_cmd(crate::app_overlay::Cmd::LocalHistory);
        let Overlay::Picker(p) = &app.overlay else { panic!("picker expected") };
        assert_eq!(p.items.len(), 2);
        assert!(p.items[0].label.contains("Saved"), "{}", p.items[0].label);
        app.overlay = Overlay::None;

        // Pick the oldest: a diff with Restore.
        app.docs[i].replace_text("v3 unsaved\n");
        app.run_action(Action::LocalHist(path.clone(), Some(h[1].file.clone())));
        let dv = app.diff.as_ref().expect("diff open");
        assert!(dv.restore.is_some());
        assert_eq!(dv.right_name, "Current");
        app.hist_restore();
        assert!(app.diff.is_none());
        assert_eq!(app.docs[i].buf.to_string(), "v1\n");
        // The text it replaced was snapshotted first.
        localhist::flush();
        let h = store.history(&path);
        assert_eq!(h[0].source, Source::Restore);
        assert_eq!(store.read(&path, &h[0].file).unwrap(), "v3 unsaved\n");
        // One undo step brings it back.
        app.run_cmd(crate::app_overlay::Cmd::Undo);
        assert_eq!(app.docs[i].buf.to_string(), "v3 unsaved\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_reload_keeps_the_old_buffer() {
        let _g = lock();
        let (mut app, dir) = history_app("lh-reload");
        let path = dir.join("seed.txt");
        let i = app.docs.iter().position(|d| d.path() == Some(path.as_path())).unwrap();
        // Another program overwrites the file the editor never saved.
        std::fs::write(&path, "rewritten by a tool\n").unwrap();
        app.docs[i].disk_mtime = Some(std::time::UNIX_EPOCH);
        app.last_disk_check = std::time::Instant::now() - std::time::Duration::from_secs(60);
        app.tick();
        assert_eq!(app.docs[i].buf.to_string(), "rewritten by a tool\n", "reloaded");
        localhist::flush();
        let store = app.hist_reader().unwrap();
        let h = store.history(&path);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].source, Source::Reload);
        assert_eq!(store.read(&path, &h[0].file).unwrap(), "seed\n", "the only copy of the old text survives");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn off_records_nothing_and_deleted_files_can_be_recreated() {
        let _g = lock();
        let (mut app, dir) = history_app("lh-deleted");
        let path = dir.join("seed.txt");
        let i = app.docs.iter().position(|d| d.path() == Some(path.as_path())).unwrap();
        app.activate(i);
        app.config.local_history = false;
        app.docs[i].replace_text("off\n");
        app.save_active();
        localhist::flush();
        let store = app.hist_reader().unwrap();
        assert!(store.history(&path).is_empty(), "local_history off: nothing recorded");

        app.config.local_history = true;
        app.docs[i].replace_text("keep me\n");
        app.save_active();
        localhist::flush();
        // The file is deleted (tab closed, file removed by another program).
        app.close_tab(i, true);
        std::fs::remove_file(&path).unwrap();
        app.run_cmd(crate::app_overlay::Cmd::LocalHistoryDeleted);
        let Overlay::Picker(p) = &app.overlay else { panic!("picker expected") };
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].label, "seed.txt");
        let pick = p.items[0].action.clone();
        app.overlay = Overlay::None;
        app.run_action(pick);
        let Overlay::Picker(p) = &app.overlay else { panic!("snapshot picker expected") };
        let Action::LocalHist(_, Some(file)) = p.items[0].action.clone() else { panic!("snapshot item") };
        app.overlay = Overlay::None;
        app.hist_show(&path, &file);
        assert_eq!(app.diff.as_ref().map(|d| d.right_name.as_str()), Some("Deleted"));
        app.hist_restore();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me\n", "written back");
        assert!(app.docs.iter().any(|d| d.path() == Some(path.as_path())), "and opened");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
