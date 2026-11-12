//! Comparing with any branch, tag or commit: the refs to pick from, how a typed spec resolves
//! (merge base included), the files that differ and their two texts. Plain git calls, no UI.

use std::path::Path;
use std::process::{Command, Stdio};

fn git_text(top: &Path, args: &[&str]) -> Result<String, String> {
    let o =
        Command::new("git").arg("-C").arg(top).args(args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        Err(err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("git failed").to_string())
    }
}

/// Kind of a pickable ref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Branch,
    Remote,
    Tag,
    Commit,
}

impl RefKind {
    pub fn word(self) -> &'static str {
        match self {
            RefKind::Branch => "branch",
            RefKind::Remote => "remote branch",
            RefKind::Tag => "tag",
            RefKind::Commit => "commit",
        }
    }
}

/// One entry of the ref picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefItem {
    /// What git is given (`main`, `origin/main`, `v1.2`, a full hash).
    pub name: String,
    pub kind: RefKind,
    /// Short hash and subject of the commit it points to.
    pub short: String,
    pub subject: String,
}

/// Local branches, remote branches and tags (most recent commit first), then the last `commits`
/// commits of HEAD.
pub fn refs(top: &Path, commits: usize) -> Vec<RefItem> {
    let mut out = Vec::new();
    let fmt = "--format=%(refname)%1f%(refname:short)%1f%(objectname:short)%1f%(subject)";
    if let Ok(s) =
        git_text(top, &["for-each-ref", "--sort=-committerdate", fmt, "refs/heads", "refs/remotes", "refs/tags"])
    {
        for l in s.lines() {
            let p: Vec<&str> = l.split('\x1f').collect();
            let [full, short_name, hash, subject] = p[..] else { continue };
            let kind = if full.starts_with("refs/heads/") {
                RefKind::Branch
            } else if full.starts_with("refs/tags/") {
                RefKind::Tag
            } else {
                // `origin/HEAD` only repeats the remote's default branch.
                if full.ends_with("/HEAD") {
                    continue;
                }
                RefKind::Remote
            };
            out.push(RefItem { name: short_name.into(), kind, short: hash.into(), subject: subject.into() });
        }
    }
    // Branches first, then remotes, then tags (each still newest first).
    out.sort_by_key(|r| r.kind as u8);
    let n = format!("--max-count={commits}");
    if let Ok(s) = git_text(top, &["log", &n, "--pretty=format:%H%x1f%h%x1f%s"]) {
        for l in s.lines() {
            let mut p = l.split('\x1f');
            let (Some(h), Some(short)) = (p.next(), p.next()) else { continue };
            out.push(RefItem {
                name: h.into(),
                kind: RefKind::Commit,
                short: short.into(),
                subject: p.next().unwrap_or("").into(),
            });
        }
    }
    out
}

/// The full hash of the commit `rev` names.
pub fn resolve(top: &Path, rev: &str) -> Result<String, String> {
    let rev = rev.trim();
    if rev.is_empty() || rev.starts_with('-') {
        return Err(format!("not a revision: “{rev}”"));
    }
    git_text(top, &["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("unknown revision “{rev}”"))
}

/// Short hash for labels.
pub fn short(hash: &str) -> &str {
    &hash[..hash.len().min(8)]
}

/// What a typed or picked spec means: `base` (old side), an optional `target` (new side; None =
/// the working tree, or HEAD for "since diverged") and whether to start from their merge base.
/// `a...b` / `a...` = since `a` and `b` (or HEAD) diverged, `a..b` = `a` against `b`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub base: String,
    pub target: Option<String>,
    pub merge_base: bool,
}

pub fn parse_spec(s: &str) -> Spec {
    let s = s.trim();
    let side = |x: &str| Some(x.trim()).filter(|x| !x.is_empty()).map(str::to_string);
    if let Some((a, b)) = s.split_once("...") {
        return Spec { base: side(a).unwrap_or_else(|| "HEAD".into()), target: side(b), merge_base: true };
    }
    if let Some((a, b)) = s.split_once("..") {
        return Spec { base: side(a).unwrap_or_else(|| "HEAD".into()), target: side(b), merge_base: false };
    }
    Spec { base: s.to_string(), target: None, merge_base: false }
}

/// A resolved comparison: old side `from`, new side `to` (None = the working tree).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub from: String,
    pub from_label: String,
    pub to: Option<String>,
    pub to_label: String,
}

/// Resolve `spec`. With `merge_base`, the old side is where `base` and the target (HEAD when the
/// target is the working tree) diverged, like a pull request shows its changes.
pub fn resolve_spec(top: &Path, spec: &Spec) -> Result<Resolved, String> {
    let base = resolve(top, &spec.base)?;
    let to = match &spec.target {
        Some(t) => Some(resolve(top, t)?),
        None => None,
    };
    let to_label = spec.target.clone().unwrap_or_else(|| "Working tree".into());
    if !spec.merge_base {
        let from_label =
            if spec.base.len() == 40 && spec.base == base { short(&base).to_string() } else { spec.base.clone() };
        return Ok(Resolved { from: base, from_label, to, to_label });
    }
    let other = match &to {
        Some(t) => t.clone(),
        None => resolve(top, "HEAD")?,
    };
    let mb = git_text(top, &["merge-base", &base, &other])
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!("“{}” and {} have no common ancestor", spec.base, spec.target.as_deref().unwrap_or("HEAD"))
        })?;
    let from_label = format!("merge base of {} ({})", spec.base, short(&mb));
    Ok(Resolved { from: mb, from_label, to, to_label })
}

/// A file that differs in a comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmpFile {
    /// `A`dded, `D`eleted, `M`odified, `R`enamed, `C`opied, `T`ype changed, `?` untracked.
    pub code: char,
    pub path: String,
    /// Path on the old side when renamed / copied.
    pub old: Option<String>,
}

impl CmpFile {
    pub fn what(&self) -> &'static str {
        match self.code {
            'A' => "added",
            'D' => "deleted",
            'R' => "renamed",
            'C' => "copied",
            'T' => "type changed",
            '?' => "untracked",
            _ => "modified",
        }
    }
}

/// Parse `git diff --name-status -z` (`M\0path\0`, `R100\0old\0new\0`).
pub fn parse_name_status_z(out: &str) -> Vec<CmpFile> {
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    let mut v = Vec::new();
    while let Some(st) = it.next() {
        let Some(code) = st.chars().next() else { continue };
        let Some(a) = it.next() else { break };
        if matches!(code, 'R' | 'C') {
            let Some(b) = it.next() else { break };
            v.push(CmpFile { code, path: b.into(), old: Some(a.into()) });
        } else {
            v.push(CmpFile { code, path: a.into(), old: None });
        }
    }
    v
}

/// Files that differ between commit `from` and `to` (None = the working tree, untracked files
/// included), sorted by path.
pub fn changed_files(top: &Path, from: &str, to: Option<&str>) -> Result<Vec<CmpFile>, String> {
    let mut args = vec!["diff", "--name-status", "-z", "-M", "--no-ext-diff", from];
    if let Some(t) = to {
        args.push(t);
    }
    args.push("--");
    let mut v = parse_name_status_z(&git_text(top, &args)?);
    if to.is_none()
        && let Ok(s) = git_text(top, &["ls-files", "-z", "--others", "--exclude-standard"])
    {
        v.extend(s.split('\0').filter(|p| !p.is_empty()).map(|p| CmpFile { code: '?', path: p.into(), old: None }));
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(v)
}

/// The text of `path` at commit `rev` (None when it does not exist there).
pub fn text_at(top: &Path, rev: &str, path: &str) -> Option<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["show", &format!("{rev}:{path}")])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// `(old, new)` texts of `f` in the comparison `from` -> `to` (None = the working tree); a side
/// where the file does not exist is empty.
pub fn texts(top: &Path, from: &str, to: Option<&str>, f: &CmpFile) -> (String, String) {
    let old = match f.code {
        'A' | '?' => String::new(),
        _ => text_at(top, from, f.old.as_deref().unwrap_or(&f.path)).unwrap_or_default(),
    };
    let new = match (f.code, to) {
        ('D', _) => String::new(),
        (_, Some(t)) => text_at(top, t, &f.path).unwrap_or_default(),
        (_, None) => {
            std::fs::read(top.join(&f.path)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
        }
    };
    (old, new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Repo(PathBuf);
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn commit(dir: &Path, files: &[(&str, &str)], msg: &str) {
        for (p, c) in files {
            if let Some(parent) = dir.join(p).parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(dir.join(p), c).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    /// main: base -> m1 (edits a.txt); feature (from base): f1 (edits b.txt, adds c.txt).
    fn forked(tag: &str) -> Repo {
        let d = std::env::temp_dir().join(format!("lumen-gitcmp-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        git(&d, &["init", "-q", "-b", "main"]);
        commit(&d, &[("a.txt", "a\n"), ("b.txt", "b\n"), ("old name.txt", "same\n")], "base");
        git(&d, &["tag", "v1"]);
        git(&d, &["checkout", "-q", "-b", "feature"]);
        commit(&d, &[("b.txt", "B\n"), ("sub/c.txt", "c\n")], "f1");
        git(&d, &["checkout", "-q", "main"]);
        commit(&d, &[("a.txt", "A\n")], "m1");
        git(&d, &["checkout", "-q", "feature"]);
        Repo(d)
    }

    fn paths(v: &[CmpFile]) -> Vec<(char, &str)> {
        v.iter().map(|f| (f.code, f.path.as_str())).collect()
    }

    #[test]
    fn spec_syntax() {
        assert_eq!(parse_spec("main"), Spec { base: "main".into(), target: None, merge_base: false });
        assert_eq!(parse_spec(" main... "), Spec { base: "main".into(), target: None, merge_base: true });
        assert_eq!(
            parse_spec("main...dev"),
            Spec { base: "main".into(), target: Some("dev".into()), merge_base: true }
        );
        assert_eq!(parse_spec("v1..v2"), Spec { base: "v1".into(), target: Some("v2".into()), merge_base: false });
        assert_eq!(parse_spec("..dev"), Spec { base: "HEAD".into(), target: Some("dev".into()), merge_base: false });
        assert_eq!(parse_spec("HEAD~2"), Spec { base: "HEAD~2".into(), target: None, merge_base: false });
    }

    #[test]
    fn name_status_z_parsing() {
        let v = parse_name_status_z("M\0a b.txt\0R087\0old.txt\0new.txt\0A\0x\0");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], CmpFile { code: 'M', path: "a b.txt".into(), old: None });
        assert_eq!(v[1], CmpFile { code: 'R', path: "new.txt".into(), old: Some("old.txt".into()) });
        assert_eq!(v[2].what(), "added");
        assert!(parse_name_status_z("").is_empty());
    }

    #[test]
    fn refs_list_branches_tags_and_commits() {
        let r = forked("refs");
        let items = refs(&r.0, 10);
        let names: Vec<(&str, RefKind)> = items.iter().map(|i| (i.name.as_str(), i.kind)).collect();
        assert!(names.contains(&("main", RefKind::Branch)));
        assert!(names.contains(&("feature", RefKind::Branch)));
        assert!(names.contains(&("v1", RefKind::Tag)));
        let first_tag = items.iter().position(|i| i.kind == RefKind::Tag).unwrap();
        assert!(items[..first_tag].iter().all(|i| i.kind == RefKind::Branch), "branches come first");
        let commits: Vec<&RefItem> = items.iter().filter(|i| i.kind == RefKind::Commit).collect();
        assert_eq!(commits.len(), 2, "HEAD (feature) has two commits");
        assert_eq!(commits[0].subject, "f1");
        assert_eq!(commits[0].name.len(), 40);
    }

    #[test]
    fn working_tree_against_a_branch_directly_and_since_diverged() {
        let r = forked("wt");
        // Uncommitted work: an edit and an untracked file.
        fs::write(r.0.join("b.txt"), "B2\n").unwrap();
        fs::write(r.0.join("new.txt"), "n\n").unwrap();

        // Directly against main: main's own a.txt edit shows up too (as a difference).
        let res = resolve_spec(&r.0, &parse_spec("main")).unwrap();
        assert_eq!((res.from_label.as_str(), res.to.as_deref(), res.to_label.as_str()), ("main", None, "Working tree"));
        let files = changed_files(&r.0, &res.from, None).unwrap();
        assert_eq!(paths(&files), [('M', "a.txt"), ('M', "b.txt"), ('?', "new.txt"), ('A', "sub/c.txt")]);
        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(texts(&r.0, &res.from, None, a), ("A\n".into(), "a\n".into()));

        // Since diverged: only what this branch (and the working tree) changed.
        let res = resolve_spec(&r.0, &parse_spec("main...")).unwrap();
        assert!(res.from_label.starts_with("merge base of main ("), "{}", res.from_label);
        assert_eq!(res.from, resolve(&r.0, "v1").unwrap(), "the fork point is the tagged base");
        let files = changed_files(&r.0, &res.from, None).unwrap();
        assert_eq!(paths(&files), [('M', "b.txt"), ('?', "new.txt"), ('A', "sub/c.txt")]);
        let b = files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!(texts(&r.0, &res.from, None, b), ("b\n".into(), "B2\n".into()));
        let c = files.iter().find(|f| f.path == "sub/c.txt").unwrap();
        assert_eq!(texts(&r.0, &res.from, None, c), (String::new(), "c\n".into()));
    }

    #[test]
    fn two_refs_with_rename_and_errors() {
        let r = forked("two");
        git(&r.0, &["mv", "old name.txt", "new name.txt"]);
        git(&r.0, &["commit", "-q", "-m", "rename"]);
        let res = resolve_spec(&r.0, &parse_spec("v1..feature")).unwrap();
        assert_eq!(res.to_label, "feature");
        let files = changed_files(&r.0, &res.from, res.to.as_deref()).unwrap();
        let ren = files.iter().find(|f| f.code == 'R').expect("rename detected");
        assert_eq!((ren.path.as_str(), ren.old.as_deref()), ("new name.txt", Some("old name.txt")));
        assert_eq!(texts(&r.0, &res.from, res.to.as_deref(), ren), ("same\n".into(), "same\n".into()));
        assert!(!files.iter().any(|f| f.code == '?'), "two commits: no untracked files");
        // main...feature = since they diverged, on feature's side.
        let res = resolve_spec(&r.0, &parse_spec("main...feature")).unwrap();
        let files = changed_files(&r.0, &res.from, res.to.as_deref()).unwrap();
        assert!(!files.iter().any(|f| f.path == "a.txt"), "main's own change is not part of it");
        // Errors are readable.
        assert!(resolve(&r.0, "nope").unwrap_err().contains("nope"));
        assert!(resolve(&r.0, "--all").is_err());
        assert!(resolve_spec(&r.0, &parse_spec("main..nope")).is_err());
        assert_eq!(resolve(&r.0, "HEAD~1").unwrap().len(), 40);
    }
}
