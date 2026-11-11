//! Git worktrees, independent of the UI: the list (`git worktree list --porcelain`), the default
//! folder of a new one (`<repo>/../<repo>-worktrees/<branch>`, or the `git_worktree_dir` setting),
//! the arguments of `worktree add / remove / prune` (run on a worker by `gitsync`), the dirty check
//! and "is this folder a linked worktree?". The App glue is `app_worktree.rs`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// One entry of `git worktree list --porcelain`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Checked-out commit ("" for a bare repository).
    pub head: String,
    /// Short branch name (`feature/x`); None when HEAD is detached (or the entry is bare).
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
    /// Its folder is gone: `git worktree prune` would drop it.
    pub prunable: bool,
    /// The first entry: the main worktree (or the bare repository) the others hang from.
    pub main: bool,
}

impl Worktree {
    /// What the pickers call it: the branch, else `(detached 1a2b3c4)` / `(bare)`.
    pub fn name(&self) -> String {
        match (&self.branch, self.bare) {
            (Some(b), _) => b.clone(),
            (None, true) => "(bare)".into(),
            (None, false) => format!("(detached {})", &self.head[..self.head.len().min(7)]),
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `git worktree list --porcelain`: records separated by blank lines, the first one is the
/// main worktree.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut list: Vec<Worktree> = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in out.lines().map(|l| l.trim_end_matches('\r')) {
        if line.is_empty() {
            list.extend(cur.take());
            continue;
        }
        let (key, val) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            list.extend(cur.take());
            cur = Some(Worktree { path: PathBuf::from(val), ..Worktree::default() });
            continue;
        }
        let Some(w) = cur.as_mut() else { continue };
        match key {
            "HEAD" => w.head = val.to_string(),
            "branch" => w.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string()),
            "bare" => w.bare = true,
            "locked" => w.locked = true,
            "prunable" => w.prunable = true,
            _ => {}
        }
    }
    list.extend(cur);
    if let Some(first) = list.first_mut() {
        first.main = true;
    }
    list
}

/// Worktrees of the repository `dir` belongs to (empty when it is not a repository).
pub fn list(dir: &Path) -> Vec<Worktree> {
    git(dir, &["worktree", "list", "--porcelain"]).map(|o| parse_porcelain(&o)).unwrap_or_default()
}

/// The worktree has changes (modified, staged or untracked files) that `worktree remove` refuses to drop.
pub fn is_dirty(path: &Path) -> bool {
    path.is_dir() && git(path, &["status", "--porcelain"]).is_some_and(|o| !o.trim().is_empty())
}

/// [`is_dirty`] for every worktree, checked in parallel (the slowest `git status` sets the time).
pub fn dirty_flags(list: &[Worktree]) -> Vec<bool> {
    std::thread::scope(|s| {
        let hs: Vec<_> = list.iter().map(|w| s.spawn(move || !w.bare && !w.prunable && is_dirty(&w.path))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or(false)).collect()
    })
}

/// When `top` (the top folder of a checkout) is a linked worktree, the main worktree it belongs to.
/// A linked worktree has a `.git` *file* pointing into `<main>/.git/worktrees/<name>` (a submodule
/// has one too, pointing into `.git/modules/`, and is not a worktree). Reads one small file.
pub fn linked_main(top: &Path) -> Option<PathBuf> {
    let dot = top.join(".git");
    if !dot.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let gitdir = top.join(gitdir); // absolute stays absolute; relative (worktree.useRelativePaths) is resolved
    let parent = gitdir.parent()?;
    if parent.file_name()? != "worktrees" {
        return None;
    }
    let common = parent.parent()?;
    // `<main>/.git` -> `<main>`; a bare repository is its own "main".
    let main = if common.file_name().is_some_and(|n| n == ".git") { common.parent()? } else { common };
    Some(normalize(main))
}

/// Remove `.` and `..` components without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// A branch name as one folder name: `feature/login` -> `feature-login`.
pub fn folder_name(branch: &str) -> String {
    let s: String = branch
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    let s = s.trim_matches(|c| c == '.' || c == '-');
    if s.is_empty() { "worktree".into() } else { s.to_string() }
}

/// Folder that holds the worktrees of the repository whose main worktree is `main`:
/// `<main>/../<repo>-worktrees`, or the `git_worktree_dir` setting (`~` and `{repo}` expanded;
/// a relative path is relative to the main worktree).
pub fn base_dir(main: &Path, setting: &str) -> PathBuf {
    let repo = main.file_name().map_or_else(|| "repo".to_string(), |n| n.to_string_lossy().into_owned());
    let setting = setting.trim();
    if setting.is_empty() {
        let parent = main.parent().unwrap_or(main);
        return parent.join(format!("{repo}-worktrees"));
    }
    let s = setting.replace("{repo}", &repo);
    let p = match s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        Some(rest) => crate::paths::home().map_or_else(|| PathBuf::from(&s), |h| h.join(rest)),
        None if s == "~" => crate::paths::home().unwrap_or_else(|| PathBuf::from(&s)),
        None => PathBuf::from(&s),
    };
    if p.is_absolute() { p } else { normalize(&main.join(p)) }
}

/// Default folder of a new worktree for `branch`; a suffix (`-2`, `-3`…) avoids a folder that
/// already exists.
pub fn default_path(main: &Path, branch: &str, setting: &str) -> PathBuf {
    let base = base_dir(main, setting);
    let name = folder_name(branch);
    let first = base.join(&name);
    if !first.exists() {
        return first;
    }
    (2..1000).map(|n| base.join(format!("{name}-{n}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// A path typed in the prompt: `~` expanded, relative to `main`'s parent folder.
pub fn resolve_typed(main: &Path, typed: &str) -> PathBuf {
    let t = typed.trim();
    let p = match t.strip_prefix("~/").or_else(|| t.strip_prefix("~\\")) {
        Some(rest) => crate::paths::home().map_or_else(|| PathBuf::from(t), |h| h.join(rest)),
        None => PathBuf::from(t),
    };
    if p.is_absolute() { p } else { normalize(&main.parent().unwrap_or(main).join(p)) }
}

/// Arguments of `git worktree add`. With `create`, a new branch `branch` is made at `base` (HEAD
/// when None; a remote branch such as `origin/x` makes it track that branch); otherwise the
/// existing local `branch` is checked out there.
pub fn add_args(path: &Path, branch: &str, create: bool, base: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = vec!["worktree".into(), "add".into()];
    let p = path.to_string_lossy().into_owned();
    if create {
        v.extend(["-b".into(), branch.to_string(), p]);
        v.extend(base.map(str::to_string));
    } else {
        v.extend([p, branch.to_string()]);
    }
    v
}

/// Arguments of `git worktree remove`. A locked worktree needs the force twice.
pub fn remove_args(path: &Path, force: bool, locked: bool) -> Vec<String> {
    let mut v: Vec<String> = vec!["worktree".into(), "remove".into()];
    if force {
        v.push("--force".into());
        if locked {
            v.push("--force".into());
        }
    }
    v.push(path.to_string_lossy().into_owned());
    v
}

pub fn prune_args() -> Vec<String> {
    vec!["worktree".into(), "prune".into()]
}

/// The worktree among `list` that holds `branch`, if any.
pub fn holding<'a>(list: &'a [Worktree], branch: &str) -> Option<&'a Worktree> {
    list.iter().find(|w| w.branch.as_deref() == Some(branch))
}

/// Index of the worktree `dir` is in (the deepest one whose folder contains it).
pub fn containing(list: &[Worktree], dir: &Path) -> Option<usize> {
    let dir = crate::paths::canonical(dir).unwrap_or_else(|_| dir.to_path_buf());
    list.iter()
        .enumerate()
        .filter(|(_, w)| !w.bare)
        .filter(|(_, w)| {
            let p = crate::paths::canonical(&w.path).unwrap_or_else(|_| w.path.clone());
            dir.starts_with(&p)
        })
        .max_by_key(|(_, w)| w.path.components().count())
        .map(|(i, _)| i)
}

/// Same folder, compared after resolving symlinks when both exist.
pub fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || match (crate::paths::canonical(a), crate::paths::canonical(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn run(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git").arg("-C").arg(dir).args(args).output().unwrap()
    }

    /// A repository with one commit (`a.txt`) at `<tmp>/lumen-wt-<tag>-<pid>/repo`; remove the
    /// parent folder when done.
    pub fn repo(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-wt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let r = d.join("repo");
        std::fs::create_dir_all(&r).unwrap();
        let r = crate::paths::canonical(&r).unwrap();
        run(&r, &["init", "-q", "-b", "main"]);
        run(&r, &["config", "user.email", "t@t"]);
        run(&r, &["config", "user.name", "t"]);
        std::fs::write(r.join("a.txt"), "one\n").unwrap();
        run(&r, &["add", "."]);
        run(&r, &["commit", "-q", "-m", "init"]);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "worktree /src/repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /src/repo-worktrees/feature-x
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feature/x
locked reason here

worktree /src/repo-worktrees/old
HEAD 3333333333333333333333333333333333333333
detached
prunable gitdir file points to non-existent location

";

    #[test]
    fn porcelain_is_parsed_with_flags_and_the_main_entry_first() {
        let l = parse_porcelain(SAMPLE);
        assert_eq!(l.len(), 3);
        assert!(l[0].main && !l[1].main && !l[2].main);
        assert_eq!(l[0].path, PathBuf::from("/src/repo"));
        assert_eq!(l[0].branch.as_deref(), Some("main"));
        assert_eq!(l[1].branch.as_deref(), Some("feature/x"));
        assert!(l[1].locked && !l[1].prunable);
        assert!(l[2].prunable && l[2].branch.is_none());
        assert_eq!(l[2].name(), "(detached 3333333)");
        assert_eq!(
            holding(&l, "feature/x").map(|w| w.path.clone()),
            Some(PathBuf::from("/src/repo-worktrees/feature-x"))
        );
        assert!(holding(&l, "nope").is_none());
    }

    #[test]
    fn porcelain_handles_bare_crlf_and_no_trailing_blank_line() {
        let l = parse_porcelain("worktree /r.git\r\nbare\r\n\r\nworktree /w\r\nHEAD abc\r\nbranch refs/heads/dev");
        assert_eq!(l.len(), 2);
        assert!(l[0].bare && l[0].main && l[0].name() == "(bare)");
        assert_eq!((l[1].path.as_path(), l[1].branch.as_deref()), (Path::new("/w"), Some("dev")));
        assert!(parse_porcelain("").is_empty());
    }

    #[test]
    fn folder_names_and_default_locations() {
        assert_eq!(folder_name("feature/login"), "feature-login");
        assert_eq!(folder_name("fix: a b"), "fix--a-b");
        assert_eq!(folder_name("../.."), "worktree");
        let main = Path::new("/w/lumen");
        assert_eq!(base_dir(main, ""), PathBuf::from("/w/lumen-worktrees"));
        assert_eq!(base_dir(main, "  "), PathBuf::from("/w/lumen-worktrees"));
        assert_eq!(base_dir(main, "/trees/{repo}"), PathBuf::from("/trees/lumen"));
        assert_eq!(base_dir(main, ".worktrees"), PathBuf::from("/w/lumen/.worktrees"));
        assert_eq!(base_dir(main, "../wt"), PathBuf::from("/w/wt"));
        if let Some(h) = crate::paths::home() {
            assert_eq!(base_dir(main, "~/wt/{repo}"), h.join("wt/lumen"));
        }
        assert_eq!(default_path(main, "feat/a", ""), PathBuf::from("/w/lumen-worktrees/feat-a"));
        assert_eq!(resolve_typed(main, "lumen-worktrees/x"), PathBuf::from("/w/lumen-worktrees/x"));
        assert_eq!(resolve_typed(main, "/abs/x"), PathBuf::from("/abs/x"));
    }

    #[test]
    fn default_path_skips_folders_that_exist() {
        let main = std::env::temp_dir().join(format!("lumen-wtdef-{}", std::process::id())).join("repo");
        let base = base_dir(&main, "");
        let _ = std::fs::remove_dir_all(main.parent().unwrap());
        std::fs::create_dir_all(base.join("dev")).unwrap();
        assert_eq!(default_path(&main, "dev", ""), base.join("dev-2"));
        std::fs::create_dir_all(base.join("dev-2")).unwrap();
        assert_eq!(default_path(&main, "dev", ""), base.join("dev-3"));
        let _ = std::fs::remove_dir_all(main.parent().unwrap());
    }

    #[test]
    fn command_arguments() {
        let p = Path::new("/t/x");
        assert_eq!(add_args(p, "x", true, None), ["worktree", "add", "-b", "x", "/t/x"]);
        assert_eq!(add_args(p, "x", true, Some("origin/x")), ["worktree", "add", "-b", "x", "/t/x", "origin/x"]);
        assert_eq!(add_args(p, "dev", false, None), ["worktree", "add", "/t/x", "dev"]);
        assert_eq!(remove_args(p, false, true), ["worktree", "remove", "/t/x"]);
        assert_eq!(remove_args(p, true, false), ["worktree", "remove", "--force", "/t/x"]);
        assert_eq!(remove_args(p, true, true), ["worktree", "remove", "--force", "--force", "/t/x"]);
        assert_eq!(prune_args(), ["worktree", "prune"]);
    }

    use super::test_support::{repo, run};

    #[test]
    fn create_list_detect_and_remove_against_a_real_repo() {
        let r = repo("real");
        let path = default_path(&r, "feat/one", "");
        let args = add_args(&path, "feat/one", true, None);
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(&r, &a);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(path.join("a.txt").is_file());

        let l = list(&r);
        assert_eq!(l.len(), 2, "{l:?}");
        assert!(l[0].main && same_path(&l[0].path, &r));
        assert_eq!(l[1].branch.as_deref(), Some("feat/one"));
        assert!(same_path(&l[1].path, &path));
        // Seen from inside the linked worktree, the list is the same.
        assert_eq!(list(&path).len(), 2);

        assert!(linked_main(&r).is_none(), "the main worktree is not linked");
        let main = linked_main(&path).expect("linked");
        assert!(same_path(&main, &r), "{main:?}");

        assert_eq!(containing(&l, &r), Some(0));
        assert_eq!(containing(&l, &path.join("sub")), Some(1), "the deepest folder wins");
        assert_eq!(containing(&l, Path::new("/elsewhere")), None);

        assert_eq!(dirty_flags(&l), vec![false, false]);
        std::fs::write(path.join("new.txt"), "x").unwrap();
        assert!(is_dirty(&path));

        // Dirty: a plain remove refuses, the forced one goes through.
        let rm = remove_args(&path, false, false);
        let a: Vec<&str> = rm.iter().map(String::as_str).collect();
        let out = run(&r, &a);
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(err.contains("modified or untracked"), "{err}");
        assert_eq!(crate::gitsync::classify(&[err]).kind, crate::gitsync::FailKind::Dirty);
        let rm = remove_args(&path, true, false);
        let a: Vec<&str> = rm.iter().map(String::as_str).collect();
        assert!(run(&r, &a).status.success());
        assert!(!path.exists());
        assert_eq!(list(&r).len(), 1);
        let _ = std::fs::remove_dir_all(r.parent().unwrap());
    }

    #[test]
    fn a_vanished_worktree_is_prunable_and_prune_drops_it() {
        let r = repo("prune");
        let path = default_path(&r, "gone", "");
        let args = add_args(&path, "gone", true, None);
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        assert!(run(&r, &a).status.success());
        std::fs::remove_dir_all(&path).unwrap();
        let l = list(&r);
        assert!(l.iter().any(|w| w.prunable), "{l:?}");
        let p = prune_args();
        let a: Vec<&str> = p.iter().map(String::as_str).collect();
        assert!(run(&r, &a).status.success());
        assert_eq!(list(&r).len(), 1);
        let _ = std::fs::remove_dir_all(r.parent().unwrap());
    }
}
