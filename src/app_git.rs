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
        Err(e.lines().find(|l| !l.trim().is_empty()).unwrap_or("falhou").to_string())
    }
}

impl App {
    pub fn refresh_git_status(&mut self) {
        self.branch = git::branch(&self.root);
        let top = git::toplevel(&self.root).unwrap_or_else(|| self.root.clone());
        let mut map: HashMap<PathBuf, char> = HashMap::new();
        for (xy, rel) in git::status(&self.root) {
            let rel = rel.rsplit(" -> ").next().unwrap_or(&rel).trim_matches('"').to_string();
            map.insert(top.join(rel), badge(&xy));
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
                    'M' => "modificado",
                    'A' => "adicionado",
                    'D' => "removido",
                    'R' => "renomeado",
                    _ => "novo",
                }
                .to_string(),
                action: Action::Open(p.clone(), None),
            })
            .collect();
        items.sort_by(|a, b| a.label.cmp(&b.label));
        if items.is_empty() {
            return self.say("sem alterações no repositório");
        }
        let title = format!("{} arquivo(s) alterado(s)", items.len());
        self.open_locations(&title, items);
    }

    pub fn git_stage(&mut self, stage: bool) {
        let Some(f) = self.current_file() else { return };
        let arg = f.to_string_lossy().into_owned();
        let args: Vec<&str> = if stage { vec!["add", "--", &arg] } else { vec!["restore", "--staged", "--", &arg] };
        match run_git(&self.root, &args) {
            Ok(_) => {
                self.refresh_git_status();
                self.say(if stage { "adicionado ao stage" } else { "removido do stage" });
            }
            Err(e) => self.say(format!("git: {e}")),
        }
    }

    pub fn prompt_git_discard(&mut self) {
        let Some(f) = self.current_file() else { return };
        let name = self.rel(&f);
        self.overlay = Overlay::Prompt(Prompt {
            title: format!("Descartar alterações de {name}? Digite 'sim' para confirmar"),
            kind: PromptKind::GitDiscard { path: f },
            field: TextField::default(),
        });
    }

    pub fn git_discard(&mut self, path: &Path) {
        let arg = path.to_string_lossy().into_owned();
        match run_git(&self.root, &["checkout", "--", &arg]) {
            Ok(_) => {
                if let Some(i) = self.docs.iter().position(|d| d.path() == Some(path)) {
                    let _ = self.docs[i].reload_from_disk();
                }
                self.refresh_git_status();
                self.say("alterações descartadas");
            }
            Err(e) => self.say(format!("git: {e}")),
        }
    }

    pub fn prompt_git_commit(&mut self) {
        self.overlay = Overlay::Prompt(Prompt { title: "Mensagem do commit (do que está no stage)".into(), kind: PromptKind::GitCommit, field: TextField::default() });
    }

    pub fn git_commit(&mut self, msg: &str) {
        if msg.trim().is_empty() {
            return;
        }
        match run_git(&self.root, &["commit", "-m", msg.trim()]) {
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
        match run_git(&self.root, &["add", "-A"]) {
            Ok(_) => {
                self.refresh_git_status();
                self.say("tudo adicionado ao stage");
            }
            Err(e) => self.say(format!("git: {e}")),
        }
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
