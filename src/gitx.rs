//! Git helpers independent of the UI: line hunks (stage / revert), merge
//! conflict parsing and resolution, file history, and side-by-side diffs.
#![allow(dead_code)]

use similar::{DiffTag, TextDiff};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Split keeping the line terminators (`"a\nb"` -> `["a\n", "b"]`).
fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// Strip a trailing `\n` / `\r\n`.
fn chomp(s: &str) -> &str {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.strip_suffix('\r').unwrap_or(s)
}

// ───────────────────────────── 1. HUNKS ─────────────────────────────

/// A contiguous changed region. Indexes are 0-based line numbers; `base` is
/// the HEAD text and `new` is the buffer text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    pub base_start: usize,
    pub base_len: usize,
    pub new_start: usize,
    pub new_len: usize,
}

/// Compute the hunks between `base` and `current` (adjacent ops are merged).
pub fn hunks(base: &str, current: &str) -> Vec<Hunk> {
    let a = lines(base);
    let b = lines(current);
    let diff = TextDiff::from_slices(&a, &b);
    let mut out: Vec<Hunk> = Vec::new();
    let mut cur: Option<Hunk> = None;
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            if let Some(h) = cur.take() {
                out.push(h);
            }
            continue;
        }
        match cur.as_mut() {
            Some(h) => {
                h.base_len = o.end - h.base_start;
                h.new_len = n.end - h.new_start;
            }
            None => {
                cur = Some(Hunk {
                    base_start: o.start,
                    base_len: o.len(),
                    new_start: n.start,
                    new_len: n.len(),
                })
            }
        }
    }
    if let Some(h) = cur {
        out.push(h);
    }
    out
}

/// The hunk covering buffer line `line`. A pure deletion (`new_len == 0`)
/// matches the line where the deletion sits (`new_start`).
pub fn hunk_at_line(hunks: &[Hunk], line: usize) -> Option<&Hunk> {
    hunks.iter().find(|h| {
        if h.new_len == 0 {
            h.new_start == line
        } else {
            line >= h.new_start && line < h.new_start + h.new_len
        }
    })
}

fn push_body(out: &mut String, prefix: char, ls: &[&str]) {
    for l in ls {
        out.push(prefix);
        out.push_str(l);
        if !l.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    }
}

fn range_header(start: usize, len: usize) -> String {
    // git: empty ranges report the line *before* the gap (0 at file start).
    let s = if len == 0 { start } else { start + 1 };
    if len == 1 { format!("{s}") } else { format!("{s},{len}") }
}

/// Unified diff with zero context for exactly hunk `h`, suitable for
/// `git apply --cached --unidiff-zero -`.
///
/// Both ranges start at `base_start`: the patch is applied on its own, without the hunks before
/// it, so the lines land where they are in the *base*. (git places a zero-context insertion by
/// the new-side position, which would be off by the net size of every earlier hunk.)
pub fn hunk_patch(rel_path: &str, base: &str, current: &str, h: &Hunk) -> String {
    hunk_patch_at(rel_path, base, current, h, h.base_start)
}

/// Same as [`hunk_patch`] with an explicit line where the hunk sits in the file it is applied to.
fn hunk_patch_at(rel_path: &str, base: &str, current: &str, h: &Hunk, at: usize) -> String {
    let a = lines(base);
    let b = lines(current);
    let mut out = format!("--- a/{rel_path}\n+++ b/{rel_path}\n");
    out.push_str(&format!("@@ -{} +{} @@\n", range_header(at, h.base_len), range_header(at, h.new_len)));
    push_body(&mut out, '-', &a[h.base_start..h.base_start + h.base_len]);
    push_body(&mut out, '+', &b[h.new_start..h.new_start + h.new_len]);
    out
}

/// Stage only hunk `h` in the index of the repo at `repo_dir`. `base` / `current` are the texts
/// the hunk was computed from (the index and the working file, for a working-tree hunk).
pub fn stage_hunk(
    repo_dir: &Path,
    rel_path: &str,
    base: &str,
    current: &str,
    h: &Hunk,
) -> Result<(), String> {
    apply_hunk(repo_dir, rel_path, base, current, h, false)
}

/// Remove hunk `h` from the index: `base` is HEAD's text and `current` the index text, so the
/// patch describes what is staged and is applied in reverse.
pub fn unstage_hunk(
    repo_dir: &Path,
    rel_path: &str,
    base: &str,
    current: &str,
    h: &Hunk,
) -> Result<(), String> {
    apply_hunk(repo_dir, rel_path, base, current, h, true)
}

fn apply_hunk(
    repo_dir: &Path,
    rel_path: &str,
    base: &str,
    current: &str,
    h: &Hunk,
    reverse: bool,
) -> Result<(), String> {
    // Unstaging applies HEAD -> index in reverse onto the index, where the hunk sits at `new_start`.
    let patch = hunk_patch_at(rel_path, base, current, h, if reverse { h.new_start } else { h.base_start });
    let mut args = vec!["apply", "--cached", "--unidiff-zero"];
    if reverse {
        args.push("--reverse");
    }
    args.push("-");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(patch.as_bytes())
        .map_err(|e| e.to_string())?;
    let o = child.wait_with_output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// Which two texts a diff compares, for hunk staging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Index against the working file: its hunks can be staged.
    Unstaged,
    /// HEAD against the index: its hunks can be unstaged.
    Staged,
}

fn blob(top: &Path, spec: &str) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(top).args(["show", spec]).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `(old, new)` of a file for `side`; None = that version does not exist (untracked, deleted,
/// added, unmerged), which also means its hunks cannot be staged one by one.
pub fn side_texts(top: &Path, rel: &str, old_rel: Option<&str>, side: Side) -> (Option<String>, Option<String>) {
    match side {
        Side::Unstaged => (blob(top, &format!(":{rel}")), std::fs::read(top.join(rel)).ok().map(|b| String::from_utf8_lossy(&b).into_owned())),
        Side::Staged => (blob(top, &format!("HEAD:{}", old_rel.unwrap_or(rel))), blob(top, &format!(":{rel}"))),
    }
}

/// Stage (`Unstaged` side) or unstage (`Staged` side) one hunk.
pub fn toggle_hunk(top: &Path, rel: &str, side: Side, old: &str, new: &str, h: &Hunk) -> Result<(), String> {
    match side {
        Side::Unstaged => stage_hunk(top, rel, old, new, h),
        Side::Staged => unstage_hunk(top, rel, old, new, h),
    }
}

/// The full current text with hunk `h` replaced by the base lines.
pub fn revert_hunk_text(base: &str, current: &str, h: &Hunk) -> String {
    let a = lines(base);
    let b = lines(current);
    let mut out = String::new();
    b[..h.new_start].iter().for_each(|l| out.push_str(l));
    a[h.base_start..h.base_start + h.base_len].iter().for_each(|l| out.push_str(l));
    b[h.new_start + h.new_len..].iter().for_each(|l| out.push_str(l));
    out
}

// ─────────────────────────── 2. CONFLICTS ───────────────────────────

/// A complete conflict block; all fields are 0-based line indexes of marker lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// `<<<<<<<` line.
    pub start: usize,
    /// `|||||||` line (diff3 style), if present.
    pub base: Option<usize>,
    /// `=======` line.
    pub mid: usize,
    /// `>>>>>>>` line.
    pub end: usize,
    pub ours_label: String,
    pub theirs_label: String,
}

/// Which side(s) to keep when resolving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Ours,
    Theirs,
    Both,
    BothReversed,
}

fn marker_label(line: &str, ch: char) -> Option<String> {
    let rest = line.strip_prefix(&ch.to_string().repeat(7))?;
    if rest.is_empty() {
        Some(String::new())
    } else {
        rest.strip_prefix(' ').map(|r| r.trim().to_string())
    }
}

/// Parse conflict blocks. Incomplete blocks are ignored; a new `<<<<<<<`
/// before the previous block closed discards the earlier one.
pub fn parse_conflicts(text: &str) -> Vec<Conflict> {
    #[derive(PartialEq)]
    enum St {
        Out,
        Ours,
        Base,
        Theirs,
    }
    let mut out = Vec::new();
    let mut st = St::Out;
    let (mut start, mut base, mut mid, mut label) = (0, None, 0, String::new());
    for (i, raw) in lines(text).into_iter().enumerate() {
        let l = chomp(raw);
        if let Some(lb) = marker_label(l, '<') {
            st = St::Ours;
            start = i;
            base = None;
            label = lb;
        } else if st == St::Ours && marker_label(l, '|').is_some() {
            base = Some(i);
            st = St::Base;
        } else if (st == St::Ours || st == St::Base) && l == "=======" {
            mid = i;
            st = St::Theirs;
        } else if st == St::Theirs {
            if let Some(tl) = marker_label(l, '>') {
                out.push(Conflict {
                    start,
                    base,
                    mid,
                    end: i,
                    ours_label: std::mem::take(&mut label),
                    theirs_label: tl,
                });
                st = St::Out;
            }
        }
    }
    out
}

/// Replace conflict `c` by the chosen side(s), keeping line endings intact.
pub fn resolve(text: &str, c: &Conflict, pick: Pick) -> String {
    let ls = lines(text);
    let ours = &ls[c.start + 1..c.base.unwrap_or(c.mid)];
    let theirs = &ls[c.mid + 1..c.end];
    let mut out = String::new();
    ls[..c.start].iter().for_each(|l| out.push_str(l));
    let parts: [&[&str]; 2] = match pick {
        Pick::Ours => [ours, &[]],
        Pick::Theirs => [theirs, &[]],
        Pick::Both => [ours, theirs],
        Pick::BothReversed => [theirs, ours],
    };
    parts.iter().for_each(|p| p.iter().for_each(|l| out.push_str(l)));
    ls[(c.end + 1).min(ls.len())..].iter().for_each(|l| out.push_str(l));
    out
}

/// The conflict whose block (markers included) contains `line`.
pub fn conflict_at_line(cs: &[Conflict], line: usize) -> Option<&Conflict> {
    cs.iter().find(|c| line >= c.start && line <= c.end)
}

// ──────────────────────────── 3. HISTORY ────────────────────────────

/// One commit of a file's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Relative date, e.g. "3 days ago".
    pub when: String,
    pub subject: String,
}

fn split_path(path: &Path) -> Option<(&Path, &str)> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Some((dir, path.file_name()?.to_str()?))
}

fn git_out(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    o.status.success().then_some(o.stdout)
}

fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    git_out(dir, args).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// History of a file (follows renames), newest first, at most `limit` entries.
pub fn file_log(path: &Path, limit: usize) -> Vec<Commit> {
    let Some((dir, name)) = split_path(path) else { return vec![] };
    let n = limit.to_string();
    let Some(s) = git_text(
        dir,
        &["log", "--follow", "--pretty=format:%H%x1f%h%x1f%an%x1f%ar%x1f%s", "-n", &n, "--", name],
    ) else {
        return vec![];
    };
    s.lines()
        .filter_map(|l| {
            let mut p = l.split('\x1f');
            Some(Commit {
                hash: p.next()?.to_string(),
                short: p.next()?.to_string(),
                author: p.next()?.to_string(),
                when: p.next()?.to_string(),
                subject: p.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Repo-root-relative name this file had at `commit` (follows renames):
/// the name recorded by the newest history entry that is an ancestor of
/// (or equal to) `commit`.
fn name_at_commit(dir: &Path, name: &str, commit: &str) -> Option<String> {
    let full = git_text(dir, &["rev-parse", "--verify", "-q", &format!("{commit}^{{commit}}")])?;
    let full = full.trim().to_string();
    let s = git_text(dir, &["log", "--follow", "--name-only", "--pretty=format:%H", "--", name])?;
    let mut entry: Option<&str> = None;
    for l in s.lines().filter(|l| !l.is_empty()) {
        if l.len() == 40 && l.bytes().all(|b| b.is_ascii_hexdigit()) {
            entry = Some(l);
        } else if let Some(h) = entry.take() {
            let ok = h == full
                || Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(["merge-base", "--is-ancestor", h, &full])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
            if ok {
                return Some(l.to_string());
            }
        }
    }
    None
}

fn show_at(dir: &Path, name: &str, rev: &str) -> Option<String> {
    if let Some(old) = name_at_commit(dir, name, rev) {
        if let Some(s) = git_text(dir, &["show", &format!("{rev}:{old}")]) {
            return Some(s);
        }
    }
    git_text(dir, &["show", &format!("{rev}:./{name}")])
}

/// Content of the file at `commit` (None if absent there; renames followed).
pub fn file_at(path: &Path, commit: &str) -> Option<String> {
    let (dir, name) = split_path(path)?;
    show_at(dir, name, commit)
}

/// Content of the file in the first parent of `commit`. None for root
/// commits and for files the commit created (renames are followed).
pub fn parent_file_at(path: &Path, commit: &str) -> Option<String> {
    let (dir, name) = split_path(path)?;
    let parent = git_text(dir, &["rev-parse", "--verify", "-q", &format!("{commit}^^{{commit}}")])?;
    show_at(dir, name, parent.trim())
}

// ───────────────────────── 4. SIDE-BY-SIDE ──────────────────────────

/// Kind of an aligned row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Same,
    Added,
    Removed,
    Changed,
}

/// One aligned row; sides hold `(0-based line number, text without newline)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub kind: RowKind,
}

/// Row ranges `(first, end)` of the changed blocks (runs of non-`Same` rows). They line up one to
/// one with `hunks(old, new)`, since both come from the same line diff.
pub fn blocks(rows: &[Row]) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (i, r) in rows.iter().enumerate() {
        match (r.kind == RowKind::Same, start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                v.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        v.push((s, rows.len()));
    }
    v
}

/// Align `old` and `new` into side-by-side rows. Replace blocks pair lines
/// up; the surplus becomes Removed / Added rows.
pub fn side_by_side(old: &str, new: &str) -> Vec<Row> {
    let a = lines(old);
    let b = lines(new);
    let diff = TextDiff::from_slices(&a, &b);
    let l = |i: usize| Some((i, chomp(a[i]).to_string()));
    let r = |i: usize| Some((i, chomp(b[i]).to_string()));
    let mut rows = Vec::new();
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {
                for (i, j) in o.zip(n) {
                    rows.push(Row { left: l(i), right: r(j), kind: RowKind::Same });
                }
            }
            DiffTag::Delete => {
                for i in o {
                    rows.push(Row { left: l(i), right: None, kind: RowKind::Removed });
                }
            }
            DiffTag::Insert => {
                for j in n {
                    rows.push(Row { left: None, right: r(j), kind: RowKind::Added });
                }
            }
            DiffTag::Replace => {
                let k = o.len().max(n.len());
                for t in 0..k {
                    let (i, j) = (o.start + t, n.start + t);
                    let (li, rj) = (i < o.end, j < n.end);
                    rows.push(match (li, rj) {
                        (true, true) => Row { left: l(i), right: r(j), kind: RowKind::Changed },
                        (true, false) => Row { left: l(i), right: None, kind: RowKind::Removed },
                        _ => Row { left: None, right: r(j), kind: RowKind::Added },
                    });
                }
            }
        }
    }
    rows
}

fn push_range(v: &mut Vec<(usize, usize)>, s: usize, e: usize) {
    if s >= e {
        return;
    }
    match v.last_mut() {
        Some(last) if last.1 == s => last.1 = e,
        _ => v.push((s, e)),
    }
}

/// Char ranges (`start..end`, in chars) of intra-line differences in `a` and `b`.
pub fn changed_ranges(a: &str, b: &str) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let diff = TextDiff::from_chars(a, b);
    let (mut ra, mut rb) = (Vec::new(), Vec::new());
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {}
            DiffTag::Delete => push_range(&mut ra, o.start, o.end),
            DiffTag::Insert => push_range(&mut rb, n.start, n.end),
            DiffTag::Replace => {
                push_range(&mut ra, o.start, o.end);
                push_range(&mut rb, n.start, n.end);
            }
        }
    }
    (ra, rb)
}

// ───────────────────────────── TESTS ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Tmp {
            let p = std::env::temp_dir().join(format!(
                "lumen-gitx-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn init() -> Tmp {
        let t = Tmp::new();
        git(&t.0, &["init", "-q"]);
        t
    }

    fn commit_file(dir: &Path, name: &str, content: &str, msg: &str) {
        fs::write(dir.join(name), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    /// base with only hunk `h` taken from `current`.
    fn expected_staged(base: &str, current: &str, h: &Hunk) -> String {
        let a = lines(base);
        let b = lines(current);
        let mut s = String::new();
        a[..h.base_start].iter().for_each(|l| s.push_str(l));
        b[h.new_start..h.new_start + h.new_len].iter().for_each(|l| s.push_str(l));
        a[h.base_start + h.base_len..].iter().for_each(|l| s.push_str(l));
        s
    }

    /// Stage every hunk individually in a real repo and verify the index.
    fn check_apply(base: &str, current: &str) -> usize {
        let t = init();
        commit_file(&t.0, "f.txt", base, "base");
        fs::write(t.0.join("f.txt"), current).unwrap();
        let hs = hunks(base, current);
        for h in &hs {
            git(&t.0, &["reset", "-q"]);
            stage_hunk(&t.0, "f.txt", base, current, h).unwrap();
            let idx = git(&t.0, &["show", ":f.txt"]);
            assert_eq!(idx, expected_staged(base, current, h), "hunk {h:?}");
        }
        // all hunks, last first, must reproduce the current text
        git(&t.0, &["reset", "-q"]);
        for h in hs.iter().rev() {
            stage_hunk(&t.0, "f.txt", base, current, h).unwrap();
        }
        assert_eq!(git(&t.0, &["show", ":f.txt"]), current);
        if base == current {
            assert!(git(&t.0, &["diff", "--cached"]).is_empty());
        }
        hs.len()
    }

    // ---- hunks ----

    #[test]
    fn hunks_identical() {
        assert!(hunks("a\nb\n", "a\nb\n").is_empty());
        assert!(hunks("", "").is_empty());
    }

    #[test]
    fn hunks_add() {
        let h = hunks("a\nb\n", "a\nx\ny\nb\n");
        assert_eq!(h, vec![Hunk { base_start: 1, base_len: 0, new_start: 1, new_len: 2 }]);
    }

    #[test]
    fn hunks_modify_delete() {
        let h = hunks("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(h, vec![Hunk { base_start: 1, base_len: 1, new_start: 1, new_len: 1 }]);
        let h = hunks("a\nb\nc\n", "a\nc\n");
        assert_eq!(h, vec![Hunk { base_start: 1, base_len: 1, new_start: 1, new_len: 0 }]);
    }

    #[test]
    fn hunks_multi() {
        let h = hunks("1\n2\n3\n4\n5\n6\n", "1\nX\n3\n4\n5\n");
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].base_start, 1);
        assert_eq!(h[1], Hunk { base_start: 5, base_len: 1, new_start: 5, new_len: 0 });
    }

    #[test]
    fn hunks_no_trailing_newline() {
        let h = hunks("a\nb", "a\nb\n");
        assert_eq!(h, vec![Hunk { base_start: 1, base_len: 1, new_start: 1, new_len: 1 }]);
    }

    #[test]
    fn hunk_at_line_cases() {
        let h = hunks("a\nb\nc\n", "a\nX\nY\nc\n");
        assert!(hunk_at_line(&h, 0).is_none());
        assert!(hunk_at_line(&h, 1).is_some());
        assert!(hunk_at_line(&h, 2).is_some());
        assert!(hunk_at_line(&h, 3).is_none());
        let d = hunks("a\nb\nc\n", "a\nc\n");
        assert_eq!(hunk_at_line(&d, 1), Some(&d[0]));
        assert!(hunk_at_line(&d, 0).is_none());
    }

    #[test]
    fn patch_text_shape() {
        let (b, c) = ("a\nb\nc\n", "a\nB\nc\n");
        let h = hunks(b, c);
        assert_eq!(
            hunk_patch("src/x.rs", b, c, &h[0]),
            "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -2 +2 @@\n-b\n+B\n"
        );
        let c = "a\nb\nq\nr\nc\n";
        let h = hunks(b, c);
        assert!(hunk_patch("f", b, c, &h[0]).contains("@@ -2,0 +3,2 @@\n"));
        let c = "b\nc\n";
        let h = hunks(b, c);
        assert!(hunk_patch("f", b, c, &h[0]).contains("@@ -1 +0,0 @@\n"));
    }

    #[test]
    fn patch_no_newline_marker() {
        let (b, c) = ("a\nb", "a\nb\nc");
        let h = hunks(b, c);
        let p = hunk_patch("f", b, c, &h[0]);
        assert!(p.contains("-b\n\\ No newline at end of file\n+b\n+c\n\\ No newline"));
    }

    // ---- real git apply ----

    #[test]
    fn apply_add() {
        assert_eq!(check_apply("a\nb\n", "a\nx\ny\nb\n"), 1);
    }
    #[test]
    fn apply_add_at_start_and_end() {
        check_apply("a\nb\n", "z\na\nb\n");
        check_apply("a\nb\n", "a\nb\nz\n");
    }
    #[test]
    fn apply_modify() {
        assert_eq!(check_apply("a\nb\nc\n", "a\nB\nc\n"), 1);
    }
    #[test]
    fn apply_delete() {
        check_apply("a\nb\nc\n", "a\nc\n");
        check_apply("a\nb\nc\n", "b\nc\n");
        check_apply("a\nb\nc\n", "a\nb\n");
    }
    #[test]
    fn apply_multi() {
        let b = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let c = "0\n1\n2x\n3\n4\n6\n7\n8\n9\n10\n";
        assert!(check_apply(b, c) >= 3);
    }
    #[test]
    fn apply_no_trailing_newline() {
        check_apply("a\nb", "a\nb\nc");
        check_apply("a\nb\n", "a\nb");
        check_apply("a\nb", "a\nb\n");
        check_apply("a\nb", "a\nB");
        check_apply("a\nb", "a");
    }
    #[test]
    fn apply_to_and_from_empty() {
        check_apply("", "a\nb\n");
        check_apply("a\nb\n", "");
    }
    #[test]
    fn apply_repeated_lines() {
        check_apply("x\nx\nx\nx\n", "x\ny\nx\nx\nz\n");
    }

    #[test]
    fn stage_hunk_error() {
        let t = init();
        commit_file(&t.0, "f.txt", "a\n", "c");
        let h = hunks("zzz\n", "yyy\n");
        assert!(stage_hunk(&t.0, "f.txt", "zzz\n", "yyy\n", &h[0]).is_err());
    }

    // ---- stage / unstage with partial staging ----

    /// A file with four separate hunks: stage 1 and 3, look at both sides, unstage 1 again.
    #[test]
    fn stage_and_unstage_hunks_with_partial_staging() {
        let base = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        // hunk 1: modify b; hunk 2: delete e; hunk 3: add after h; hunk 4: modify l
        let cur = "a\nB\nc\nd\nf\ng\nh\nNEW\ni\nj\nk\nL\n";
        let t = init();
        commit_file(&t.0, "f.txt", base, "base");
        fs::write(t.0.join("f.txt"), cur).unwrap();
        let (top, rel) = (t.0.as_path(), "f.txt");

        // Unstaged side: index == HEAD, so the same four hunks as base -> cur, and blocks agree.
        let (o, n) = side_texts(top, rel, None, Side::Unstaged);
        let (o, n) = (o.unwrap(), n.unwrap());
        assert_eq!(o, base);
        let hs = hunks(&o, &n);
        assert_eq!(hs.len(), 4);
        assert_eq!(blocks(&side_by_side(&o, &n)).len(), 4, "diff view blocks line up with hunks");
        assert!(side_texts(top, rel, None, Side::Staged).1.as_deref() == Some(base), "nothing staged yet");

        // Stage hunk 3, then (index changed) re-diff and stage what is now hunk 1.
        toggle_hunk(top, rel, Side::Unstaged, &o, &n, &hs[2]).unwrap();
        let staged = git(top, &["show", ":f.txt"]);
        assert_eq!(staged, "a\nb\nc\nd\ne\nf\ng\nh\nNEW\ni\nj\nk\nl\n");
        let (o, n) = side_texts(top, rel, None, Side::Unstaged);
        let (o, n) = (o.unwrap(), n.unwrap());
        let hs = hunks(&o, &n);
        assert_eq!(hs.len(), 3, "the staged hunk left the unstaged side");
        toggle_hunk(top, rel, Side::Unstaged, &o, &n, &hs[0]).unwrap();
        assert_eq!(git(top, &["show", ":f.txt"]), "a\nB\nc\nd\ne\nf\ng\nh\nNEW\ni\nj\nk\nl\n");

        // Staged side: HEAD -> index has two hunks; unstage the first (b) and keep the other.
        let (ho, hi) = side_texts(top, rel, None, Side::Staged);
        let (ho, hi) = (ho.unwrap(), hi.unwrap());
        let hs = hunks(&ho, &hi);
        assert_eq!(hs.len(), 2);
        toggle_hunk(top, rel, Side::Staged, &ho, &hi, &hs[0]).unwrap();
        assert_eq!(git(top, &["show", ":f.txt"]), "a\nb\nc\nd\ne\nf\ng\nh\nNEW\ni\nj\nk\nl\n");
        // And the working file is untouched.
        assert_eq!(fs::read_to_string(t.0.join("f.txt")).unwrap(), cur);

        // Unstage the rest: the index is HEAD again.
        let (ho, hi) = side_texts(top, rel, None, Side::Staged);
        let (ho, hi) = (ho.unwrap(), hi.unwrap());
        let hs = hunks(&ho, &hi);
        toggle_hunk(top, rel, Side::Staged, &ho, &hi, &hs[0]).unwrap();
        assert!(git(top, &["diff", "--cached"]).is_empty());
    }

    /// base with the hunks of `which` (indexes into `hs`) taken from `cur`.
    fn with_hunks(base: &str, cur: &str, hs: &[Hunk], which: &[usize]) -> String {
        let (a, b) = (lines(base), lines(cur));
        let mut out = String::new();
        let mut at = 0;
        for (i, h) in hs.iter().enumerate() {
            a[at..h.base_start].iter().for_each(|l| out.push_str(l));
            if which.contains(&i) {
                b[h.new_start..h.new_start + h.new_len].iter().for_each(|l| out.push_str(l));
            } else {
                a[h.base_start..h.base_start + h.base_len].iter().for_each(|l| out.push_str(l));
            }
            at = h.base_start + h.base_len;
        }
        a[at..].iter().for_each(|l| out.push_str(l));
        out
    }

    /// Every subset of hunks staged one at a time, then each one unstaged again: the index must be
    /// exactly "base plus the remaining subset" (hunks of different sizes shift the line numbers).
    #[test]
    fn any_order_of_stage_and_unstage_keeps_the_index_exact() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n";
        // delete 2,3 | insert x,y,z after 5 | modify 8 | insert w after 11 | delete 14
        let cur = "1\n4\n5\nx\ny\nz\n6\n7\nEIGHT\n9\n10\n11\nw\n12\n13\n";
        let hs = hunks(base, cur);
        assert_eq!(hs.len(), 5, "{hs:?}");
        let t = init();
        commit_file(&t.0, "f.txt", base, "base");
        fs::write(t.0.join("f.txt"), cur).unwrap();
        let (top, rel) = (t.0.as_path(), "f.txt");
        let n = hs.len();
        for mask in 1u32..(1 << n) {
            let set: Vec<usize> = (0..n).filter(|i| mask & (1 << i) != 0).collect();
            git(top, &["reset", "-q"]);
            // Stage in a mask-dependent order: the remaining unstaged hunks are always re-diffed.
            let mut order = set.clone();
            if mask % 2 == 0 {
                order.reverse();
            }
            let mut left: Vec<usize> = (0..n).collect();
            for k in &order {
                let (o, w) = side_texts(top, rel, None, Side::Unstaged);
                let (o, w) = (o.unwrap(), w.unwrap());
                let cur_hs = hunks(&o, &w);
                let pos = left.iter().position(|x| x == k).unwrap();
                toggle_hunk(top, rel, Side::Unstaged, &o, &w, &cur_hs[pos]).unwrap();
                left.remove(pos);
            }
            assert_eq!(git(top, &["show", ":f.txt"]), with_hunks(base, cur, &hs, &set), "stage mask {mask:b} order {order:?}");
            // Unstage them again, a different order.
            let mut staged = set.clone();
            let mut undo = set.clone();
            if mask % 3 == 0 {
                undo.reverse();
            }
            for k in &undo {
                let (o, i) = side_texts(top, rel, None, Side::Staged);
                let (o, i) = (o.unwrap(), i.unwrap());
                let cur_hs = hunks(&o, &i);
                let pos = staged.iter().position(|x| x == k).unwrap();
                toggle_hunk(top, rel, Side::Staged, &o, &i, &cur_hs[pos]).unwrap();
                staged.remove(pos);
                assert_eq!(git(top, &["show", ":f.txt"]), with_hunks(base, cur, &hs, &staged), "unstage {k} mask {mask:b}");
            }
        }
    }

    #[test]
    fn side_texts_are_none_for_files_without_both_versions() {
        let t = init();
        commit_file(&t.0, "kept.txt", "x\n", "base");
        fs::write(t.0.join("new.txt"), "n\n").unwrap();
        let (o, n) = side_texts(&t.0, "new.txt", None, Side::Unstaged);
        assert!(o.is_none() && n.is_some(), "untracked: no index version");
        git(&t.0, &["add", "new.txt"]);
        let (o, n) = side_texts(&t.0, "new.txt", None, Side::Staged);
        assert!(o.is_none() && n.is_some(), "added: no HEAD version");
        fs::remove_file(t.0.join("kept.txt")).unwrap();
        let (o, n) = side_texts(&t.0, "kept.txt", None, Side::Unstaged);
        assert!(o.is_some() && n.is_none(), "deleted in the working tree");
    }

    // ---- revert ----

    #[test]
    fn revert_roundtrip() {
        let cases = [
            ("a\nb\nc\n", "a\nX\nc\n"),
            ("a\nb\nc\n", "a\nc\n"),
            ("a\nc\n", "a\nb\nc\n"),
            ("a\nb", "a\nb\nc\n"),
            ("1\n2\n3\n4\n5\n", "1\nA\n3\n4\nB\nC\n"),
            ("", "x\n"),
            ("x\n", ""),
        ];
        for (b, c) in cases {
            let hs = hunks(b, c);
            // reverting the only/last hunk repeatedly returns base
            let mut cur = c.to_string();
            while let Some(h) = hunks(b, &cur).last().copied() {
                cur = revert_hunk_text(b, &cur, &h);
            }
            assert_eq!(cur, b, "case {b:?} -> {c:?}");
            if hs.len() == 1 {
                assert_eq!(revert_hunk_text(b, c, &hs[0]), b);
            }
        }
    }

    #[test]
    fn revert_one_of_two() {
        let b = "1\n2\n3\n4\n5\n";
        let c = "1\nA\n3\n4\nB\n";
        let hs = hunks(b, c);
        assert_eq!(hs.len(), 2);
        assert_eq!(revert_hunk_text(b, c, &hs[0]), "1\n2\n3\n4\nB\n");
        assert_eq!(revert_hunk_text(b, c, &hs[1]), "1\nA\n3\n4\n5\n");
    }

    // ---- conflicts ----

    const SIMPLE: &str = "pre\n<<<<<<< HEAD\nours1\nours2\n=======\ntheirs1\n>>>>>>> feature\npost\n";

    #[test]
    fn conflict_simple() {
        let cs = parse_conflicts(SIMPLE);
        assert_eq!(cs.len(), 1);
        let c = &cs[0];
        assert_eq!((c.start, c.base, c.mid, c.end), (1, None, 4, 6));
        assert_eq!(c.ours_label, "HEAD");
        assert_eq!(c.theirs_label, "feature");
    }

    #[test]
    fn conflict_diff3() {
        let t = "<<<<<<< HEAD\no\n||||||| base\nb\n=======\nt\n>>>>>>> br\n";
        let cs = parse_conflicts(t);
        assert_eq!(cs.len(), 1);
        assert_eq!((cs[0].start, cs[0].base, cs[0].mid, cs[0].end), (0, Some(2), 4, 6));
        assert_eq!(resolve(t, &cs[0], Pick::Ours), "o\n");
        assert_eq!(resolve(t, &cs[0], Pick::Theirs), "t\n");
        assert_eq!(resolve(t, &cs[0], Pick::Both), "o\nt\n");
    }

    #[test]
    fn conflict_multiple_and_lookup() {
        let t = format!("{SIMPLE}{SIMPLE}");
        let cs = parse_conflicts(&t);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[1].start, 9);
        assert!(conflict_at_line(&cs, 0).is_none());
        assert_eq!(conflict_at_line(&cs, 1), Some(&cs[0]));
        assert_eq!(conflict_at_line(&cs, 6), Some(&cs[0]));
        assert!(conflict_at_line(&cs, 7).is_none());
        assert_eq!(conflict_at_line(&cs, 12), Some(&cs[1]));
    }

    #[test]
    fn conflict_incomplete_ignored() {
        assert!(parse_conflicts("<<<<<<< a\nx\n").is_empty());
        assert!(parse_conflicts("<<<<<<< a\nx\n=======\ny\n").is_empty());
        assert!(parse_conflicts("=======\n>>>>>>> b\n").is_empty());
        assert!(parse_conflicts("x\n>>>>>>> b\n").is_empty());
    }

    #[test]
    fn conflict_nested_restart() {
        let t = "<<<<<<< a\nx\n<<<<<<< b\ny\n=======\nz\n>>>>>>> c\n";
        let cs = parse_conflicts(t);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].start, 2);
        assert_eq!(cs[0].ours_label, "b");
    }

    #[test]
    fn conflict_not_markers() {
        // 8 chars, or text glued to the marker, are not markers
        assert!(parse_conflicts("<<<<<<<< a\n=======\n>>>>>>>> b\n").is_empty());
        assert!(parse_conflicts("<<<<<<<x\n=======\n>>>>>>> b\n").is_empty());
        let cs = parse_conflicts("<<<<<<<\na\n=======\nb\n>>>>>>>\n");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].ours_label, "");
    }

    #[test]
    fn resolve_each_pick() {
        let c = &parse_conflicts(SIMPLE)[0];
        assert_eq!(resolve(SIMPLE, c, Pick::Ours), "pre\nours1\nours2\npost\n");
        assert_eq!(resolve(SIMPLE, c, Pick::Theirs), "pre\ntheirs1\npost\n");
        assert_eq!(resolve(SIMPLE, c, Pick::Both), "pre\nours1\nours2\ntheirs1\npost\n");
        assert_eq!(resolve(SIMPLE, c, Pick::BothReversed), "pre\ntheirs1\nours1\nours2\npost\n");
    }

    #[test]
    fn resolve_crlf() {
        let t = "a\r\n<<<<<<< HEAD\r\no\r\n=======\r\nt\r\n>>>>>>> br\r\nz\r\n";
        let cs = parse_conflicts(t);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].theirs_label, "br");
        assert_eq!(resolve(t, &cs[0], Pick::Both), "a\r\no\r\nt\r\nz\r\n");
        assert_eq!(resolve(t, &cs[0], Pick::Theirs), "a\r\nt\r\nz\r\n");
    }

    #[test]
    fn resolve_end_at_eof_without_newline() {
        let t = "<<<<<<< a\nx\n=======\ny\n>>>>>>> b";
        let c = &parse_conflicts(t)[0];
        assert_eq!(resolve(t, c, Pick::Theirs), "y\n");
        assert_eq!(resolve(t, c, Pick::Ours), "x\n");
    }

    #[test]
    fn resolve_one_of_two_then_other() {
        let t = format!("{SIMPLE}{SIMPLE}");
        let cs = parse_conflicts(&t);
        let t2 = resolve(&t, &cs[0], Pick::Ours);
        let cs2 = parse_conflicts(&t2);
        assert_eq!(cs2.len(), 1);
        let t3 = resolve(&t2, &cs2[0], Pick::Theirs);
        assert!(parse_conflicts(&t3).is_empty());
        assert_eq!(t3, "pre\nours1\nours2\npost\npre\ntheirs1\npost\n");
    }

    // ---- history ----

    fn three_commits() -> Tmp {
        let t = init();
        commit_file(&t.0, "a.txt", "one\n", "first");
        commit_file(&t.0, "a.txt", "one\ntwo\n", "second");
        git(&t.0, &["mv", "a.txt", "b.txt"]);
        git(&t.0, &["commit", "-q", "-m", "rename"]);
        commit_file(&t.0, "b.txt", "one\ntwo\nthree\n", "third");
        t
    }

    #[test]
    fn log_follows_rename() {
        let t = three_commits();
        let log = file_log(&t.0.join("b.txt"), 10);
        let subj: Vec<_> = log.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subj, ["third", "rename", "second", "first"]);
        assert_eq!(log[0].hash.len(), 40);
        assert!(log[0].hash.starts_with(&log[0].short));
        assert_eq!(log[0].author, "T");
        assert!(log[0].when.contains("ago") || log[0].when.contains("now"));
        assert_eq!(file_log(&t.0.join("b.txt"), 2).len(), 2);
    }

    #[test]
    fn log_unknown_file() {
        let t = three_commits();
        assert!(file_log(&t.0.join("nope.txt"), 5).is_empty());
        assert!(file_log(Path::new("/nonexistent-dir-xyz/f"), 5).is_empty());
    }

    #[test]
    fn file_at_commits() {
        let t = three_commits();
        let p = t.0.join("b.txt");
        let log = file_log(&p, 10);
        assert_eq!(file_at(&p, &log[0].hash).unwrap(), "one\ntwo\nthree\n");
        assert_eq!(file_at(&p, &log[1].hash).unwrap(), "one\ntwo\n");
        // before the rename the file had another name
        assert_eq!(file_at(&p, &log[2].hash).unwrap(), "one\ntwo\n");
        assert_eq!(file_at(&p, &log[3].hash).unwrap(), "one\n");
        assert!(file_at(&p, "deadbeef").is_none());
    }

    #[test]
    fn parent_file_at_commits() {
        let t = three_commits();
        let p = t.0.join("b.txt");
        let log = file_log(&p, 10);
        assert_eq!(parent_file_at(&p, &log[0].hash).unwrap(), "one\ntwo\n");
        // the rename commit: parent has it under the old name
        assert_eq!(parent_file_at(&p, &log[1].hash).unwrap(), "one\ntwo\n");
        assert_eq!(parent_file_at(&p, &log[2].hash).unwrap(), "one\n");
        assert!(parent_file_at(&p, &log[3].hash).is_none()); // root commit
    }

    #[test]
    fn parent_none_for_new_file() {
        let t = init();
        commit_file(&t.0, "a.txt", "a\n", "one");
        commit_file(&t.0, "n.txt", "n\n", "two");
        let p = t.0.join("n.txt");
        let log = file_log(&p, 5);
        assert!(parent_file_at(&p, &log[0].hash).is_none());
    }

    #[test]
    fn history_in_subdir() {
        let t = init();
        fs::create_dir_all(t.0.join("sub")).unwrap();
        commit_file(&t.0, "sub/x.txt", "1\n", "one");
        commit_file(&t.0, "sub/x.txt", "1\n2\n", "two");
        let p = t.0.join("sub/x.txt");
        let log = file_log(&p, 5);
        assert_eq!(log.len(), 2);
        assert_eq!(file_at(&p, &log[0].hash).unwrap(), "1\n2\n");
        assert_eq!(parent_file_at(&p, &log[0].hash).unwrap(), "1\n");
    }

    // ---- side by side ----

    #[test]
    fn sbs_same_and_empty() {
        assert!(side_by_side("", "").is_empty());
        let r = side_by_side("a\nb\n", "a\nb\n");
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|r| r.kind == RowKind::Same));
        assert_eq!(r[1].left, Some((1, "b".into())));
        assert_eq!(r[1].right, Some((1, "b".into())));
    }

    #[test]
    fn sbs_added_removed() {
        let r = side_by_side("a\nc\n", "a\nb\nc\n");
        let k: Vec<_> = r.iter().map(|r| r.kind).collect();
        assert_eq!(k, [RowKind::Same, RowKind::Added, RowKind::Same]);
        assert_eq!(r[1].left, None);
        assert_eq!(r[1].right, Some((1, "b".into())));
        assert_eq!(r[2].left, Some((1, "c".into())));
        assert_eq!(r[2].right, Some((2, "c".into())));
        let r = side_by_side("a\nb\nc\n", "a\nc\n");
        assert_eq!(r[1].kind, RowKind::Removed);
        assert_eq!(r[1].right, None);
    }

    #[test]
    fn sbs_changed_pairs_and_surplus() {
        let r = side_by_side("a\nb1\nb2\nb3\nz\n", "a\nB1\nB2\nz\n");
        let k: Vec<_> = r.iter().map(|r| r.kind).collect();
        assert_eq!(
            k,
            [RowKind::Same, RowKind::Changed, RowKind::Changed, RowKind::Removed, RowKind::Same]
        );
        assert_eq!(r[3].left, Some((3, "b3".into())));
        let r = side_by_side("a\nb\nz\n", "a\nX\nY\nZ\nz\n");
        let k: Vec<_> = r.iter().map(|r| r.kind).collect();
        assert_eq!(
            k,
            [RowKind::Same, RowKind::Changed, RowKind::Added, RowKind::Added, RowKind::Same]
        );
        assert_eq!(r[4].left, Some((2, "z".into())));
        assert_eq!(r[4].right, Some((4, "z".into())));
    }

    #[test]
    fn sbs_row_counts() {
        let (o, n) = ("1\n2\n3\n4\n", "1\nx\n3\n4\n5\n6\n");
        let r = side_by_side(o, n);
        let left = r.iter().filter(|r| r.left.is_some()).count();
        let right = r.iter().filter(|r| r.right.is_some()).count();
        assert_eq!((left, right), (4, 6));
    }

    #[test]
    fn sbs_crlf_and_no_newline() {
        let r = side_by_side("a\r\nb", "a\r\nb");
        assert_eq!(r[0].left, Some((0, "a".into())));
        assert_eq!(r[1].left, Some((1, "b".into())));
        assert!(r.iter().all(|r| r.kind == RowKind::Same));
    }

    // ---- changed_ranges ----

    #[test]
    fn ranges_basic() {
        let (a, b) = changed_ranges("hello world", "hello brave world");
        assert!(a.is_empty());
        assert_eq!(b, vec![(6, 12)]);
        let (a, b) = changed_ranges("foo(bar)", "foo(baz)");
        assert_eq!(a, vec![(6, 7)]);
        assert_eq!(b, vec![(6, 7)]);
    }

    #[test]
    fn ranges_identical_and_empty() {
        assert_eq!(changed_ranges("abc", "abc"), (vec![], vec![]));
        assert_eq!(changed_ranges("", "ab"), (vec![], vec![(0, 2)]));
        assert_eq!(changed_ranges("ab", ""), (vec![(0, 2)], vec![]));
    }

    #[test]
    fn ranges_use_char_indexes() {
        let (a, b) = changed_ranges("héllo", "hállo");
        assert_eq!(a, vec![(1, 2)]);
        assert_eq!(b, vec![(1, 2)]);
    }
}
