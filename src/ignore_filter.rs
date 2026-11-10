//! Which explorer entries git ignores: every `.gitignore` from the repository root down to the folder
//! (nested ones are read the first time their folder is listed), `.git/info/exclude`, and the user's
//! global ignore file, with git's precedence (deeper file first, then exclude, then global).
//!
//! Cheap on big trees: the explorer never lists an ignored folder (it does not even stat inside
//! `node_modules` or `target`), and one matcher per folder is cached until [`IgnoreFilter::invalidate`]
//! (the watcher calls it when an ignore file changes).

use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
};

pub struct IgnoreFilter {
    /// Where the matching starts: the repository root when the project is inside one (so a monorepo
    /// package honours the parent `.gitignore`), else the project folder itself.
    top: PathBuf,
    exclude: Gitignore,
    global: Gitignore,
    dirs: RefCell<HashMap<PathBuf, Option<Gitignore>>>,
}

/// The enclosing folder with a `.git` entry, if any.
fn git_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

fn load(dir: &Path, file: &Path) -> Option<Gitignore> {
    if !file.is_file() {
        return None;
    }
    let mut b = GitignoreBuilder::new(dir);
    b.add(file);
    b.build().ok().filter(|g| !g.is_empty())
}

impl IgnoreFilter {
    pub fn new(root: &Path) -> Self {
        let git = git_root(root);
        let top = git.clone().unwrap_or_else(|| root.to_path_buf());
        let exclude = git.as_ref().and_then(|g| load(g, &g.join(".git/info/exclude"))).unwrap_or_else(Gitignore::empty);
        // The global file only applies inside a repository, like git itself.
        let global = if git.is_some() { Gitignore::global().0 } else { Gitignore::empty() };
        Self { top, exclude, global, dirs: RefCell::new(HashMap::new()) }
    }

    /// Forget every cached matcher and re-read the exclude and global files.
    pub fn invalidate(&mut self) {
        let root = self.top.clone();
        *self = Self::new(&root);
    }

    fn matcher(&self, dir: &Path) -> Option<Gitignore> {
        let mut dirs = self.dirs.borrow_mut();
        dirs.entry(dir.to_path_buf()).or_insert_with(|| load(dir, &dir.join(".gitignore"))).clone()
    }

    /// Is `path` (a file, or a folder when `is_dir`) ignored? Its parent folders are assumed visible:
    /// the explorer never descends into an ignored folder.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Some(parent) = path.parent() else { return false };
        if !parent.starts_with(&self.top) {
            return false;
        }
        for dir in parent.ancestors() {
            if let Some(g) = self.matcher(dir) {
                match g.matched(path, is_dir) {
                    Match::Ignore(_) => return true,
                    Match::Whitelist(_) => return false,
                    Match::None => {}
                }
            }
            if dir == self.top {
                break;
            }
        }
        for g in [&self.exclude, &self.global] {
            match g.matched(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }

    /// Is this the path of a file that changes what is ignored?
    pub fn is_ignore_file(path: &Path) -> bool {
        path.file_name().is_some_and(|n| n == ".gitignore")
            || path.ends_with(".git/info/exclude")
            || path.file_name().is_some_and(|n| n == "exclude" && path.parent().is_some_and(|p| p.ends_with("info")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-ignore-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn gitignore_nested_negation_exclude_and_dir_only_patterns() {
        let r = tmp("rules");
        std::fs::create_dir_all(r.join(".git/info")).unwrap();
        put(&r.join(".gitignore"), "build/\n*.log\n!keep.log\n/only-root\n");
        put(&r.join("pkg/.gitignore"), "gen\n!*.log\n");
        put(&r.join(".git/info/exclude"), "secret.txt\n");
        let f = IgnoreFilter::new(&r);
        assert!(f.is_ignored(&r.join("build"), true), "directory pattern matches a folder");
        assert!(!f.is_ignored(&r.join("build"), false), "but not a file named like it");
        assert!(f.is_ignored(&r.join("a.log"), false));
        assert!(!f.is_ignored(&r.join("keep.log"), false), "negation");
        assert!(f.is_ignored(&r.join("only-root"), false));
        assert!(!f.is_ignored(&r.join("pkg/only-root"), false), "anchored pattern stays at its level");
        assert!(f.is_ignored(&r.join("pkg/gen"), true), "nested .gitignore");
        assert!(!f.is_ignored(&r.join("pkg/x.log"), false), "a deeper negation wins over the parent rule");
        assert!(f.is_ignored(&r.join("secret.txt"), false), ".git/info/exclude");
        assert!(!f.is_ignored(&r.join("src/main.rs"), false));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_package_inside_a_repo_honours_the_parent_gitignore_and_invalidation_rereads() {
        let r = tmp("mono");
        std::fs::create_dir_all(r.join(".git")).unwrap();
        put(&r.join(".gitignore"), "dist\n");
        std::fs::create_dir_all(r.join("packages/a")).unwrap();
        let mut f = IgnoreFilter::new(&r.join("packages/a"));
        assert!(f.is_ignored(&r.join("packages/a/dist"), true));
        assert!(!f.is_ignored(&r.join("packages/a/out"), true));
        put(&r.join(".gitignore"), "out\n");
        assert!(!f.is_ignored(&r.join("packages/a/out"), true), "cached until invalidated");
        f.invalidate();
        assert!(f.is_ignored(&r.join("packages/a/out"), true));
        assert!(!f.is_ignored(&r.join("packages/a/dist"), true));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn recognises_the_files_that_change_the_rules() {
        assert!(IgnoreFilter::is_ignore_file(Path::new("/p/.gitignore")));
        assert!(IgnoreFilter::is_ignore_file(Path::new("/p/sub/.gitignore")));
        assert!(IgnoreFilter::is_ignore_file(Path::new("/p/.git/info/exclude")));
        assert!(!IgnoreFilter::is_ignore_file(Path::new("/p/src/main.rs")));
    }
}
