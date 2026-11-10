//! File operations from the sidebar / palette: new file, new folder, rename, delete.

use crate::{
    app::{App, Focus},
    app_overlay::{Overlay, Prompt, PromptKind},
    field::TextField,
};
use std::path::{Path, PathBuf};

impl App {
    /// Directory the next "new file" should land in: the selected folder in the explorer,
    /// else the folder of the active document, else the project root.
    fn target_dir(&self) -> PathBuf {
        if self.focus == Focus::Sidebar {
            if let Some(r) = self.tree.rows.get(self.tree.sel) {
                return if r.is_dir { r.path.clone() } else { r.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()) };
            }
        }
        self.docs.get(self.active).and_then(|d| d.path()).and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| self.root.clone())
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
        let title = format!("{} em {}", if dir { "Nova pasta" } else { "Novo arquivo" }, if rel.is_empty() { "." } else { &rel });
        let kind = if dir { PromptKind::NewDir { dir: d } } else { PromptKind::NewFile { dir: d } };
        self.overlay = Overlay::Prompt(Prompt { kind, title, field: TextField::default() });
    }

    pub fn prompt_rename_path(&mut self) {
        let Some(p) = self.target_path() else { return };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.overlay = Overlay::Prompt(Prompt { title: format!("Renomear {name}"), kind: PromptKind::RenamePath { from: p }, field: TextField::with(&name) });
    }

    pub fn prompt_delete_path(&mut self) {
        let Some(p) = self.target_path() else { return };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let what = if p.is_dir() { "a pasta (e tudo dentro)" } else { "o arquivo" };
        self.overlay = Overlay::Prompt(Prompt { title: format!("Excluir {what} {name}? Digite 'sim' para confirmar"), kind: PromptKind::DeletePath { path: p }, field: TextField::default() });
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
                    self.say(format!("pasta criada: {name}"));
                } else {
                    self.open_path(&path, None);
                    self.focus = Focus::Editor;
                }
            }
            Err(e) => self.say(format!("erro ao criar: {e}")),
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
            return self.say(format!("já existe: {new_name}"));
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::rename(from, &to) {
            return self.say(format!("erro ao renomear: {e}"));
        }
        self.repoint_docs(from, &to);
        self.refresh_tree();
        self.tree.reveal(&to);
        self.say(format!("renomeado para {new_name}"));
    }

    /// Re-point open documents after `from` moved to `to` (the file itself, or anything inside a
    /// renamed/moved folder).
    pub fn repoint_docs(&mut self, from: &Path, to: &Path) {
        for i in 0..self.docs.len() {
            let Some(old) = self.docs[i].path().map(Path::to_path_buf) else { continue };
            if let Ok(rest) = old.strip_prefix(from) {
                let newp = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
                self.lsp_notify_close(i);
                let d = &mut self.docs[i];
                d.lsp_open = false;
                d.buf.path = Some(newp.clone());
                d.name = newp.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                d.disk_mtime = d.stat_mtime();
                d.git_base = crate::git::head_text(&newp);
                d.git_stale = true;
            }
        }
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
                self.say(format!("excluído: {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            }
            Err(e) => self.say(format!("erro ao excluir: {e}")),
        }
    }
}
