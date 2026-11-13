//! App glue for multi-root workspaces (the file formats and path helpers are in `workspace.rs`).
//!
//! A project window has one primary root (`App::root`: session, terminals, `.lumen/` of the
//! project) and any number of extra folders (`Ws::extra`). With extra folders:
//!
//! * the explorer shows every folder as a top-level node (`Tree::set_extra_roots`);
//! * quick open, find in files and replace walk every folder; lists show `folder/relative/path`;
//! * git status badges come from every folder's repository; the Git panel works on one folder at a
//!   time, the "git folder" (`Ws::git_idx`, "Workspace: Git folder…"), whose branch the status bar
//!   shows; every folder's branch is kept in `Ws::branches`;
//! * language servers are told about every folder (`workspaceFolders`, `didChangeWorkspaceFolders`);
//! * "Run task" lists the tasks of every folder; the file watcher watches every folder;
//! * each folder's `.lumen/config.json` is merged under the primary root's settings;
//! * the set of folders is saved with the session and can be saved as a `.lumen-workspace` file.
//!
//! Commands: "Workspace: Add folder…", "Workspace: Remove folder…", "Workspace: Git folder…",
//! "Workspace: Open workspace file…", "Workspace: Save as…"; "Add as workspace folder" in the
//! worktree menu.

use crate::{
    app::App,
    app_overlay::{Action, Item, Overlay, Prompt, PromptKind, choice_picker},
    app_topbar::tilde,
    field::TextField,
    workspace,
};
use std::path::{Path, PathBuf};

/// Multi-root state of a window; `Default` is the ordinary single-root project.
#[derive(Default)]
pub struct Ws {
    /// Folders after `App::root`.
    pub extra: Vec<PathBuf>,
    /// The workspace file the folders came from, or were saved to.
    pub file: Option<PathBuf>,
    /// Which folder the Git panel works on: 0 = `App::root`, n = `extra[n - 1]`.
    pub git_idx: usize,
    /// Branch of every folder (primary first); filled by `refresh_git_status` with extra folders.
    pub branches: Vec<Option<String>>,
}

/// Picker rows of the workspace commands (`Action::Workspace`).
#[derive(Clone, Debug, PartialEq)]
pub enum WsPick {
    Remove(PathBuf),
    GitFolder(usize),
}

/// Where a path typed in a prompt points: `~` expanded, relative paths against `base`.
pub fn resolve_typed(text: &str, base: &Path) -> PathBuf {
    let t = text.trim();
    let p = if t == "~" || t.starts_with("~/") {
        match crate::paths::home() {
            Some(h) => h.join(t.trim_start_matches('~').trim_start_matches('/')),
            None => PathBuf::from(t),
        }
    } else {
        PathBuf::from(t)
    };
    workspace::normalize(&if p.is_absolute() { p } else { base.join(p) })
}

/// `cmd` to run inside `dir`, as one shell command line.
pub fn in_dir(dir: &Path, cmd: &str) -> String {
    #[cfg(windows)]
    {
        format!("cd /d \"{}\" && {cmd}", dir.display())
    }
    #[cfg(not(windows))]
    {
        format!("cd {} && {cmd}", crate::tasks::shell_quote(&dir.to_string_lossy()))
    }
}

impl App {
    /// Every folder of the workspace, the primary root first.
    pub fn roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.root.clone()).chain(self.ws.extra.iter().cloned()).collect()
    }

    /// The folder the Git panel and the status bar's branch refer to.
    pub fn git_root(&self) -> PathBuf {
        match self.ws.git_idx {
            0 => self.root.clone(),
            n => self.ws.extra.get(n - 1).cloned().unwrap_or_else(|| self.root.clone()),
        }
    }

    /// Forget the extra folders (the project changes: its session brings its own back).
    pub fn ws_reset(&mut self) {
        self.ws = Ws::default();
        workspace::set_lsp_roots(&[]);
    }

    /// Change the workspace folders with `f`, then bring everything that depends on them along:
    /// language servers, the explorer, the file index, git status, the watcher, the settings.
    fn ws_change(&mut self, f: impl FnOnce(&mut Ws)) {
        let before: Vec<Vec<PathBuf>> = self.lsps.iter().map(|l| workspace::lsp_folders(&l.root)).collect();
        f(&mut self.ws);
        if self.ws.git_idx > self.ws.extra.len() {
            self.ws.git_idx = 0;
        }
        workspace::set_lsp_roots(&self.roots());
        for (l, old) in self.lsps.iter_mut().zip(before) {
            if let Some(p) = workspace::folders_change(&old, &workspace::lsp_folders(&l.root)) {
                l.notify("workspace/didChangeWorkspaceFolders", p);
            }
        }
        self.tree.set_extra_roots(&self.ws.extra);
        self.file_index = None;
        self.refresh_git_status();
        self.watch_dirs();
        self.projcfg_reload(false);
        self.request_redraw();
    }

    /// Make `extras` (and `file`, if the folders came from one) the workspace folders; folders
    /// that are not directories, or are the primary root, are skipped.
    pub fn ws_attach(&mut self, extras: Vec<PathBuf>, file: Option<PathBuf>) {
        let mut keep: Vec<PathBuf> = Vec::new();
        for e in extras {
            let e = crate::paths::canonical(&e).unwrap_or(e);
            if e.is_dir() && e != self.root && !keep.contains(&e) {
                keep.push(e);
            }
        }
        if keep == self.ws.extra && file == self.ws.file {
            return;
        }
        self.ws_change(|w| {
            w.extra = keep;
            w.file = file;
        });
    }

    /// Add `dir` to the workspace.
    pub fn ws_add_folder(&mut self, dir: PathBuf) {
        let dir = crate::paths::canonical(&dir).unwrap_or(dir);
        if !dir.is_dir() {
            return self.say(format!("{} is not a folder", dir.display()));
        }
        if dir == self.root || self.ws.extra.contains(&dir) {
            return self.say(format!("{} is already in the workspace", tilde(&dir)));
        }
        self.ws_change(|w| w.extra.push(dir.clone()));
        self.tree.reveal(&dir);
        self.say(format!("added {} to the workspace", tilde(&dir)));
    }

    /// Take `dir` out of the workspace (the primary root stays: switch project to change it).
    pub fn ws_remove_folder(&mut self, dir: &Path) {
        if dir == self.root {
            return self.say("the first folder is the project itself: open another project to change it");
        }
        if !self.ws.extra.iter().any(|e| e == dir) {
            return self.say("that folder is not in the workspace");
        }
        // The git folder is found again by path: the indexes shift when an earlier folder goes.
        let git = self.git_root();
        self.ws_change(|w| {
            w.extra.retain(|e| e != dir);
            w.git_idx = w.extra.iter().position(|e| *e == git).map_or(0, |i| i + 1);
        });
        self.say(format!("removed {} from the workspace", tilde(dir)));
    }

    fn ws_prompt(&mut self, kind: PromptKind, title: &str, text: &str) {
        self.overlay = Overlay::Prompt(Prompt::new(kind, title.to_string(), TextField::with(text)));
        self.request_redraw();
    }

    /// "Workspace: Add folder…": ask for the folder.
    pub fn ws_add_prompt(&mut self) {
        let start = self.root.parent().map(|p| format!("{}/", p.display())).unwrap_or_default();
        self.ws_prompt(PromptKind::WorkspaceAdd, "Add folder to workspace (path, ~ and relative paths work)", &start);
    }

    pub fn ws_add_answer(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.ws_add_folder(resolve_typed(text, &self.root.clone()));
    }

    /// "Workspace: Remove folder…": pick one of the extra folders.
    pub fn ws_remove_pick(&mut self) {
        if self.ws.extra.is_empty() {
            return self.say("the workspace has a single folder");
        }
        let items: Vec<Item> = self
            .ws
            .extra
            .iter()
            .map(|e| Item {
                label: e.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                detail: tilde(e),
                action: Action::Workspace(WsPick::Remove(e.clone())),
            })
            .collect();
        self.overlay = Overlay::Picker(choice_picker("Remove folder from workspace", items, 0));
    }

    /// "Workspace: Git folder…": which folder the Git panel and the branch chip follow.
    pub fn ws_git_pick(&mut self) {
        if self.ws.extra.is_empty() {
            return self.say("the workspace has a single folder");
        }
        let roots = self.roots();
        let names = workspace::names(&roots);
        let items: Vec<Item> = roots
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let branch = self.ws.branches.get(i).cloned().flatten();
                let mark = if i == self.ws.git_idx { "current · " } else { "" };
                Item {
                    label: names[i].clone(),
                    detail: format!("{mark}{} · {}", branch.unwrap_or_else(|| "no repository".into()), tilde(r)),
                    action: Action::Workspace(WsPick::GitFolder(i)),
                }
            })
            .collect();
        self.overlay = Overlay::Picker(choice_picker("Git folder of the workspace", items, self.ws.git_idx));
    }

    pub fn ws_pick(&mut self, p: WsPick) {
        match p {
            WsPick::Remove(dir) => self.ws_remove_folder(&dir),
            WsPick::GitFolder(i) => {
                if i == self.ws.git_idx || i > self.ws.extra.len() {
                    return;
                }
                self.ws.git_idx = i;
                self.refresh_git_status();
                self.sync_docs_with_head();
                let name = workspace::names(&self.roots())[i].clone();
                self.say(format!("git folder: {name}"));
            }
        }
    }

    /// "Workspace: Save as…": write the folders to a `.lumen-workspace` file.
    pub fn ws_save_prompt(&mut self) {
        let def = self.ws.file.clone().unwrap_or_else(|| self.root.join(workspace::FILE_NAME));
        self.ws_prompt(PromptKind::WorkspaceSave, "Save workspace file", &def.to_string_lossy());
    }

    pub fn ws_save_answer(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let mut file = resolve_typed(text, &self.root.clone());
        if file.is_dir() {
            file = file.join(workspace::FILE_NAME);
        }
        let Some(dir) = file.parent() else { return };
        let body = workspace::serialize(dir, &self.roots());
        match std::fs::write(&file, body) {
            Ok(()) => {
                self.ws.file = Some(file.clone());
                self.say(format!("workspace saved to {}", tilde(&file)));
            }
            Err(e) => self.say(format!("could not save {}: {e}", tilde(&file))),
        }
    }

    /// "Workspace: Open workspace file…": ask for the file.
    pub fn ws_open_prompt(&mut self) {
        let def = self.ws.file.clone().unwrap_or_else(|| self.root.join(workspace::FILE_NAME));
        self.ws_prompt(
            PromptKind::WorkspaceOpen,
            "Open workspace file (.lumen-workspace or .code-workspace)",
            &def.to_string_lossy(),
        );
    }

    pub fn ws_open_answer(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let file = resolve_typed(text, &self.root.clone());
        match workspace::load(&file) {
            Err(e) => self.say(format!("{}: {e}", tilde(&file))),
            Ok(folders) if folders.first() == Some(&self.root) => {
                self.ws_attach(folders.into_iter().skip(1).collect(), Some(file.clone()));
                self.say(format!("workspace: {}", tilde(&file)));
            }
            Ok(_) => {
                // Another primary root: that is another project, in a window of its own.
                let args = vec!["--new-window".to_string(), file.to_string_lossy().into_owned()];
                match crate::app_window::spawn_window(&args) {
                    Ok(()) => self.say(format!("opening {} in a new window", tilde(&file))),
                    Err(e) => self.say(format!("could not open a new window: {e}")),
                }
            }
        }
    }

    // ── session ──

    /// The folders and the git folder, for the session file.
    pub fn ws_session_save(&self, v: &mut serde_json::Value) {
        if self.ws.extra.is_empty() {
            return;
        }
        v["roots"] = serde_json::json!(self.ws.extra);
        v["git_root"] = serde_json::json!(self.ws.git_idx);
        if let Some(f) = &self.ws.file {
            v["workspace_file"] = serde_json::json!(f);
        }
    }

    pub fn ws_session_restore(&mut self, v: &serde_json::Value) {
        let Some(list) = v["roots"].as_array() else { return };
        let extras: Vec<PathBuf> = list.iter().filter_map(|x| x.as_str()).map(PathBuf::from).collect();
        let file = v["workspace_file"].as_str().map(PathBuf::from).filter(|f| f.is_file());
        self.ws_attach(extras, file);
        let idx = v["git_root"].as_u64().unwrap_or(0) as usize;
        if idx <= self.ws.extra.len() && idx != self.ws.git_idx {
            self.ws.git_idx = idx;
            self.refresh_git_status();
        }
    }

    // ── tasks ──

    /// "Run task" entries of the folders after the primary root, each running inside its folder.
    pub fn ws_task_items(&self) -> Vec<Item> {
        let roots = self.roots();
        let names = workspace::names(&roots);
        let mut out = Vec::new();
        for (i, r) in self.ws.extra.iter().enumerate() {
            for (name, cmd) in crate::tasks::project_tasks(r) {
                out.push(Item {
                    label: format!("{name} ({})", names[i + 1]),
                    detail: cmd.clone(),
                    action: Action::Run(in_dir(r, &cmd)),
                });
            }
        }
        out
    }

    /// Text of a path in lists: relative to its folder, prefixed by the folder with several.
    pub fn ws_label(&self, p: &Path) -> String {
        workspace::label(&self.roots(), p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app};

    /// The language-server folder list is process-wide: put it back when a test ends.
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            workspace::set_lsp_roots(&[]);
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-ws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        crate::paths::canonical(&d).unwrap_or(d)
    }

    #[test]
    fn typed_paths_resolve_against_the_project() {
        assert_eq!(resolve_typed("../web", Path::new("/w/api")), PathBuf::from("/w/web"));
        assert_eq!(resolve_typed(" /abs/dir ", Path::new("/w")), PathBuf::from("/abs/dir"));
        assert_eq!(resolve_typed("sub", Path::new("/w")), PathBuf::from("/w/sub"));
    }

    #[cfg(unix)]
    #[test]
    fn commands_run_in_their_folder() {
        assert_eq!(in_dir(Path::new("/w/it's"), "cargo test"), "cd '/w/it'\\''s' && cargo test");
    }

    #[test]
    fn add_and_remove_folders_update_tree_roots_and_labels() {
        let (a, b) = (tmp("a"), tmp("b"));
        std::fs::write(a.join("one.txt"), "x").unwrap();
        std::fs::write(b.join("two.txt"), "x").unwrap();
        let _g = lock();
        let _r = Reset;
        let mut app = test_app(&a);
        assert_eq!(app.roots(), vec![a.clone()]);
        // Single root: rows start under the root, no header.
        assert!(app.tree.rows.iter().all(|r| r.depth == 0) && app.tree.rows.iter().any(|r| r.name == "one.txt"));
        app.ws_add_folder(b.clone());
        assert_eq!(app.roots(), vec![a.clone(), b.clone()]);
        let tops: Vec<&Path> = app.tree.rows.iter().filter(|r| r.depth == 0).map(|r| r.path.as_path()).collect();
        assert_eq!(tops, vec![a.as_path(), b.as_path()]);
        assert!(app.tree.rows.iter().any(|r| r.path == b.join("two.txt") && r.depth == 1));
        assert!(app.tree.is_root(&b) && !app.tree.is_root(&b.join("two.txt")));
        // Nothing to move or delete when the folder nodes are selected.
        let i = app.tree.rows.iter().position(|r| r.path == b).unwrap();
        app.tree.select_only(i);
        assert!(app.tree.selection().is_empty());
        // Search and lists see both folders.
        assert_eq!(app.rel(&b.join("two.txt")), format!("{}/two.txt", b.file_name().unwrap().to_string_lossy()));
        // Duplicates and the primary root are refused.
        app.ws_add_folder(b.clone());
        app.ws_add_folder(a.clone());
        assert_eq!(app.ws.extra.len(), 1);
        app.ws_remove_folder(&a);
        assert_eq!(app.ws.extra.len(), 1, "the primary root stays");
        app.ws_remove_folder(&b);
        assert!(app.ws.extra.is_empty());
        assert!(app.tree.rows.iter().all(|r| r.depth == 0), "back to the single-root tree");
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn file_index_and_tasks_cover_every_folder() {
        let (a, b) = (tmp("ia"), tmp("ib"));
        std::fs::write(a.join("one.txt"), "x").unwrap();
        std::fs::write(b.join("two.txt"), "x").unwrap();
        std::fs::write(b.join("Cargo.toml"), "[package]\n").unwrap();
        let _g = lock();
        let _r = Reset;
        let mut app = test_app(&a);
        app.ws_add_folder(b.clone());
        app.open_quick_open();
        let Overlay::Picker(p) = &app.overlay else { panic!("no picker") };
        let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.ends_with("/one.txt")) && labels.iter().any(|l| l.ends_with("/two.txt")),
            "{labels:?}"
        );
        let tasks = app.ws_task_items();
        assert!(tasks.iter().any(|t| t.label.starts_with("cargo test (")
            && matches!(&t.action, Action::Run(c) if c.contains("cargo test") && c.starts_with("cd "))));
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn the_folder_set_survives_a_session() {
        let (a, b) = (tmp("sa"), tmp("sb"));
        let _g = lock();
        let _r = Reset;
        let mut app = test_app(&a);
        app.ws_add_folder(b.clone());
        app.ws.git_idx = 1;
        let mut v = serde_json::json!({});
        app.ws_session_save(&mut v);
        let mut other = test_app(&a);
        other.ws_session_restore(&v);
        assert_eq!(other.ws.extra, vec![b.clone()]);
        assert_eq!(other.ws.git_idx, 1);
        assert_eq!(other.git_root(), b);
        // A folder that vanished is dropped.
        std::fs::remove_dir_all(&b).ok();
        let mut third = test_app(&a);
        third.ws_session_restore(&v);
        assert!(third.ws.extra.is_empty() && third.ws.git_idx == 0);
        std::fs::remove_dir_all(&a).ok();
    }

    #[test]
    fn saving_and_opening_a_workspace_file_round_trips() {
        let (a, b) = (tmp("fa"), tmp("fb"));
        let _g = lock();
        let _r = Reset;
        let mut app = test_app(&a);
        app.ws_add_folder(b.clone());
        let file = a.join("team.lumen-workspace");
        app.ws_save_answer(&file.to_string_lossy());
        assert_eq!(app.ws.file.as_deref(), Some(file.as_path()));
        assert_eq!(workspace::load(&file).unwrap(), vec![a.clone(), b.clone()]);
        let mut other = test_app(&a);
        other.ws_open_answer(&file.to_string_lossy());
        assert_eq!(other.ws.extra, vec![b.clone()]);
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }
}
