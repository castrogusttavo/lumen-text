//! File operations from the sidebar / palette: new file, new folder, rename, delete.

use crate::{
    app::{App, Focus},
    app_overlay::{Overlay, Prompt, PromptKind},
    field::TextField,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

impl App {
    /// Directory the next "new file" should land in: the selected folder in the explorer,
    /// else the folder of the active document, else the project root.
    fn target_dir(&self) -> PathBuf {
        if self.focus == Focus::Sidebar
            && let Some(r) = self.tree.rows.get(self.tree.sel)
        {
            return if r.is_dir {
                r.path.clone()
            } else {
                r.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone())
            };
        }
        self.docs
            .get(self.active)
            .and_then(|d| d.path())
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone())
    }

    /// The path rename/delete act on: explorer selection if focused, else the active file.
    fn target_path(&self) -> Option<PathBuf> {
        if self.focus == Focus::Sidebar {
            return self.tree.rows.get(self.tree.sel).map(|r| r.path.clone());
        }
        self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf))
    }

    pub fn prompt_new_file(&mut self, dir: bool) {
        let d = self.target_dir();
        let rel = self.rel(&d);
        let title =
            format!("{} in {}", if dir { "New folder" } else { "New file" }, if rel.is_empty() { "." } else { &rel });
        let kind = if dir { PromptKind::NewDir { dir: d } } else { PromptKind::NewFile { dir: d } };
        self.overlay = Overlay::Prompt(Prompt::new(kind, title, TextField::default()));
    }

    pub fn prompt_rename_path(&mut self) {
        let Some(p) = self.target_path() else { return };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.overlay = Overlay::Prompt(Prompt::new(
            PromptKind::RenamePath { from: p },
            format!("Rename {name}"),
            TextField::with(&name),
        ));
    }

    pub fn prompt_delete_path(&mut self) {
        if self.focus == Focus::Sidebar && self.tree.selection().len() > 1 {
            return self.ctx_delete_selection();
        }
        let Some(p) = self.target_path() else { return };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let what = if p.is_dir() { "the folder (and everything in it)" } else { "the file" };
        self.overlay = Overlay::Prompt(Prompt::new(
            PromptKind::DeletePath { path: p },
            format!("Delete {what} {name}? Type 'yes' to confirm"),
            TextField::default(),
        ));
    }

    pub fn refresh_tree(&mut self) {
        self.tree.rebuild();
        self.file_index = None;
        self.refresh_git_status();
    }

    pub fn create_path(&mut self, dir: &Path, name: &str, is_dir: bool) {
        let name = name.trim().trim_start_matches('/');
        if name.is_empty() {
            return;
        }
        let path = dir.join(name);
        let res = if is_dir {
            std::fs::create_dir_all(&path)
        } else if path.exists() {
            Ok(())
        } else {
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::File::create(&path).map(|_| ()))
        };
        match res {
            Ok(()) => {
                self.refresh_tree();
                if is_dir {
                    self.tree.reveal(&path);
                    self.say(format!("folder created: {name}"));
                } else {
                    self.open_path(&path, None);
                    self.focus = Focus::Editor;
                }
            }
            Err(e) => self.say(format!("error creating: {e}")),
        }
    }

    pub fn rename_path(&mut self, from: &Path, new_name: &str) {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return;
        }
        let to = from.parent().unwrap_or(&self.root).join(new_name);
        if to == from {
            return;
        }
        if to.exists() {
            return self.say(format!("already exists: {new_name}"));
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::rename(from, &to) {
            return self.say(format!("error renaming: {e}"));
        }
        self.repoint_docs(from, &to);
        self.refresh_tree();
        self.tree.reveal(&to);
        self.say(format!("renamed to {new_name}"));
    }

    /// Re-point open documents after `from` moved to `to` (the file itself, or anything inside a
    /// renamed/moved folder).
    pub fn repoint_docs(&mut self, from: &Path, to: &Path) {
        for i in 0..self.docs.len() {
            let Some(old) = self.docs[i].path().map(Path::to_path_buf) else { continue };
            if let Ok(rest) = old.strip_prefix(from) {
                let newp = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
                self.repoint_doc(i, &newp);
            }
        }
        self.request_redraw();
    }

    /// Document `i` now lives at `newp` (renamed or moved; maybe `a.js` -> `a.ts`). The servers
    /// are told the old URI is gone while the document still has it, then the document takes the
    /// new path and language and the sync tick reopens it wherever it fits now.
    fn repoint_doc(&mut self, i: usize, newp: &Path) {
        let old_lang = self.docs[i].lang;
        let new_lang = crate::highlight::language_for(&newp.to_string_lossy());
        if new_lang != old_lang {
            // Servers that don't serve the new language close it (stopping when it was their last
            // document) and its stale diagnostics/semantic state go.
            self.docs[i].lang = new_lang;
            self.lsp_language_changed(i, old_lang, new_lang);
        }
        // Servers that keep the document: close the old URI, keep running.
        self.lsp_notify_close(i);
        if self.docs[i].biome_open {
            self.biome_notify_close(i);
        }
        self.set_doc_path(i, newp);
        let soon = Instant::now() + Duration::from_millis(400);
        let d = &mut self.docs[i];
        d.lsp_open = false;
        d.biome_open = false;
        // The new place may belong to another project (tsconfig / biome.json): look them up again.
        d.lsp_root = None;
        d.biome_root = None;
        d.disk_mtime = d.stat_mtime();
        d.symbols_due = Some(soon);
        d.sem_due = Some(soon);
    }

    pub fn delete_path(&mut self, path: &Path) {
        let res = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        match res {
            Ok(()) => {
                let mut i = 0;
                while i < self.docs.len() {
                    if self.docs[i].path().is_some_and(|p| p.starts_with(path)) {
                        self.close_tab(i, true);
                    } else {
                        i += 1;
                    }
                }
                self.refresh_tree();
                self.say(format!(
                    "deleted: {}",
                    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                ));
            }
            Err(e) => self.say(format!("error deleting: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};

    #[test]
    fn renaming_a_file_gives_the_open_document_the_new_language() {
        let _g = lock();
        let dir = tmp("rename-lang");
        let mut app = test_app(&dir);
        let js = dir.join("a.js");
        std::fs::write(&js, "const a = 1;\n").unwrap();
        let i = app.open_path(&js, None).expect("open");
        assert_eq!(app.docs[i].lang, Some("javascript"));
        app.docs[i].lsp_open = true; // as if a server had it: it must be reopened under the new URI
        app.docs[i].lsp_root = Some(dir.clone());

        app.rename_path(&js, "a.ts");
        let ts = dir.join("a.ts");
        let d = &app.docs[i];
        assert_eq!(d.path(), Some(ts.as_path()));
        assert_eq!(d.name, "a.ts");
        assert_eq!(d.lang, Some("typescript"));
        assert_eq!(d.real_uri(), Some(crate::doc::path_to_uri(&ts)));
        assert!(d.hl.is_some(), "highlighting follows the new language");
        assert!(!d.lsp_open && !d.biome_open, "the sync tick reopens it with the new languageId");
        assert!(d.lsp_root.is_none() && d.symbols_due.is_some());
        assert_eq!(crate::lspreg::language_id(d.lang, d.path()), "typescript");

        // TSX turns JSX editing on; leaving the TS family drops it and the comment prefix follows.
        app.rename_path(&ts, "a.tsx");
        assert_eq!(app.docs[i].lang, Some("tsx"));
        assert!(app.docs[i].ed.jsx);
        app.rename_path(&dir.join("a.tsx"), "a.py");
        let d = &app.docs[i];
        assert_eq!(d.lang, Some("python"));
        assert!(!d.ed.jsx);
        assert_eq!(d.ed.comment, "#");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moving_a_folder_repoints_the_documents_inside_it() {
        let _g = lock();
        let dir = tmp("rename-dir");
        let mut app = test_app(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let f = dir.join("src/m.rs");
        std::fs::write(&f, "fn main() {}\n").unwrap();
        let i = app.open_path(&f, None).expect("open");
        app.rename_path(&dir.join("src"), "lib");
        let d = &app.docs[i];
        assert_eq!(d.path(), Some(dir.join("lib/m.rs").as_path()));
        assert_eq!((d.name.as_str(), d.lang), ("m.rs", Some("rust")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_file_prompt_shows_the_type_icon_after_typing_pauses() {
        use crate::app_overlay::{Overlay, PROMPT_ICON_DEBOUNCE};
        use std::time::{Duration, Instant};
        let _g = lock();
        let dir = tmp("prompt-icon");
        let mut app = test_app(&dir);
        let prompt = |app: &crate::app::App| match &app.overlay {
            Overlay::Prompt(p) => (p.input_icon().map(str::to_string), p.icon_due),
            _ => panic!("no prompt"),
        };
        app.prompt_new_file(true);
        assert_eq!(prompt(&app).0.as_deref(), Some("file/folder"));

        app.prompt_new_file(false);
        assert_eq!(prompt(&app), (Some("file/file".into()), None), "plain file before anything is typed");
        for c in "src/main.rs".chars() {
            let k = c.to_string();
            app.handle_key(&crate::app::KeyIn {
                key: winit::keyboard::Key::Character(k.as_str().into()),
                text: Some(k),
            });
        }
        let (icon, due) = prompt(&app);
        assert_eq!(icon.as_deref(), Some("file/file"), "not resolved while typing");
        let due = due.expect("debounce scheduled");
        assert!(due > Instant::now() && due <= Instant::now() + PROMPT_ICON_DEBOUNCE);
        // Before the deadline the tick only asks to be woken up.
        let mut woke = None;
        app.prompt_icon_tick(due - Duration::from_millis(1), &mut |t| woke = Some(t));
        assert_eq!(woke, Some(due));
        app.prompt_icon_tick(due, &mut |_| {});
        let want = app.tx.icons.file_icon("main.rs");
        assert_ne!(want, "file/file", "rust files have their own icon");
        assert_eq!(prompt(&app), (Some(want), None));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
