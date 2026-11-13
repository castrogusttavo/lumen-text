//! Git actions: status badges in the explorer, changed-files list, stage/unstage,
//! discard and commit. Everything shells out to `git`.

use crate::{
    app::App,
    app_overlay::{Action, Item, Overlay, Prompt, PromptKind},
    field::TextField,
    git,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

/// Single-letter status for the explorer from a porcelain `XY` pair.
pub fn badge(xy: &str) -> char {
    let b = xy.as_bytes();
    let (x, y) = (b.first().copied().unwrap_or(b' ') as char, b.get(1).copied().unwrap_or(b' ') as char);
    if xy == "??" {
        '?'
    } else if x == 'D' || y == 'D' {
        'D'
    } else if x == 'A' {
        'A'
    } else if x == 'R' {
        'R'
    } else {
        'M'
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&out.stderr);
        Err(e.lines().find(|l| !l.trim().is_empty()).unwrap_or("failed").to_string())
    }
}

impl App {
    pub fn refresh_git_status(&mut self) {
        let gr = self.git_root();
        self.branch = git::branch(&gr);
        let top = git::toplevel(&gr).unwrap_or_else(|| gr.clone());
        self.gitbg.worktree_main = crate::worktree::linked_main(&top);
        let mut map: HashMap<PathBuf, char> = HashMap::new();
        let primary_top =
            if gr == self.root { top } else { git::toplevel(&self.root).unwrap_or_else(|| self.root.clone()) };
        collect_status(&self.root, &primary_top, &mut map);
        // Multi-root workspace: every other folder has its own repository, status and branch.
        if !self.ws.extra.is_empty() {
            for e in &self.ws.extra {
                let t = git::toplevel(e).unwrap_or_else(|| e.clone());
                collect_status(e, &t, &mut map);
            }
            self.ws.branches = self.roots().iter().map(|r| git::branch(r)).collect();
        }
        self.git_status = map;
    }

    fn current_file(&self) -> Option<PathBuf> {
        self.docs.get(self.active).and_then(|d| d.path().map(Path::to_path_buf))
    }

    pub fn open_git_changes(&mut self) {
        self.refresh_git_status();
        let mut items: Vec<Item> = self
            .git_status
            .iter()
            .map(|(p, b)| Item {
                label: self.rel(p),
                detail: match b {
                    'M' => "modified",
                    'A' => "added",
                    'D' => "removed",
                    'R' => "renamed",
                    _ => "new",
                }
                .to_string(),
                action: Action::Open(p.clone(), None),
            })
            .collect();
        items.sort_by(|a, b| a.label.cmp(&b.label));
        if items.is_empty() {
            return self.say("no changes in the repository");
        }
        let title = format!("{} changed file(s)", items.len());
        self.open_locations(&title, items);
    }

    pub fn git_stage(&mut self, stage: bool) {
        let Some(f) = self.current_file() else { return };
        let arg = f.to_string_lossy().into_owned();
        let args: Vec<&str> = if stage { vec!["add", "--", &arg] } else { vec!["restore", "--staged", "--", &arg] };
        match run_git(&self.git_root(), &args) {
            Ok(_) => {
                self.refresh_git_status();
                self.say(if stage { "added to stage" } else { "removed from stage" });
            }
            Err(e) => self.say(format!("git: {e}")),
        }
    }

    pub fn prompt_git_discard(&mut self) {
        let Some(f) = self.current_file() else { return };
        let name = self.rel(&f);
        self.overlay = Overlay::Prompt(Prompt::new(
            PromptKind::GitDiscard { path: f },
            format!("Discard changes to {name}? Type 'yes' to confirm"),
            TextField::default(),
        ));
    }

    pub fn git_discard(&mut self, path: &Path) {
        let arg = path.to_string_lossy().into_owned();
        match run_git(&self.git_root(), &["checkout", "--", &arg]) {
            Ok(_) => {
                if let Some(i) = self.docs.iter().position(|d| d.path() == Some(path)) {
                    self.hist_snapshot(i, crate::localhist::Source::Reload);
                    let _ = self.docs[i].reload_from_disk();
                }
                self.refresh_git_status();
                self.say("changes discarded");
            }
            Err(e) => self.say(format!("git: {e}")),
        }
    }

    pub fn prompt_git_commit(&mut self) {
        self.overlay = Overlay::Prompt(Prompt::new(
            PromptKind::GitCommit,
            "Commit message (for what is staged)".into(),
            TextField::default(),
        ));
    }

    pub fn git_commit(&mut self, msg: &str) {
        if msg.trim().is_empty() {
            return;
        }
        match run_git(&self.git_root(), &["commit", "-m", msg.trim()]) {
            Ok(out) => {
                self.refresh_git_status();
                for d in &mut self.docs {
                    if let Some(p) = d.path().map(Path::to_path_buf) {
                        d.git_base = git::head_text(&p);
                        d.git_stale = true;
                    }
                }
                self.say(format!("commit: {}", out.lines().next().unwrap_or("ok")));
            }
            Err(e) => self.say(format!("git commit: {e}")),
        }
    }

    pub fn git_stage_all(&mut self) {
        match run_git(&self.git_root(), &["add", "-A"]) {
            Ok(_) => {
                self.refresh_git_status();
                self.say("everything added to stage");
            }
            Err(e) => self.say(format!("git: {e}")),
        }
    }
}

/// Add the `git status` of the project at `root` (repository top `top`) to `map`.
fn collect_status(root: &Path, top: &Path, map: &mut HashMap<PathBuf, char>) {
    for (xy, rel) in git::status(root) {
        let rel = rel.rsplit(" -> ").next().unwrap_or(&rel).trim_matches('"').to_string();
        map.insert(top.join(rel), badge(&xy));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges() {
        assert_eq!(badge("??"), '?');
        assert_eq!(badge(" M"), 'M');
        assert_eq!(badge("M "), 'M');
        assert_eq!(badge("A "), 'A');
        assert_eq!(badge(" D"), 'D');
        assert_eq!(badge("R "), 'R');
    }

    #[test]
    fn stage_commit_discard_in_a_real_repo() {
        let dir = std::env::temp_dir().join(format!("lumen-gitact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| run_git(&dir, a);
        g(&["init", "-q", "-b", "main"]).unwrap();
        g(&["config", "user.email", "t@t"]).unwrap();
        g(&["config", "user.name", "t"]).unwrap();
        std::fs::write(dir.join("a.ts"), "one\n").unwrap();
        g(&["add", "-A"]).unwrap();
        g(&["commit", "-q", "-m", "init"]).unwrap();
        std::fs::write(dir.join("a.ts"), "two\n").unwrap();
        std::fs::write(dir.join("new.ts"), "x\n").unwrap();
        let st: HashMap<String, char> = git::status(&dir).into_iter().map(|(xy, p)| (p, badge(&xy))).collect();
        assert_eq!(st.get("a.ts"), Some(&'M'));
        assert_eq!(st.get("new.ts"), Some(&'?'));
        g(&["checkout", "--", "a.ts"]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), "one\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
