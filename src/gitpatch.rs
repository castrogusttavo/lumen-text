//! Line-level staging: a patch that carries only some changed lines of a side-by-side diff.
//!
//! The selection is made of rows of [`gitx::side_by_side_ws`]. The builder first produces the
//! text the target (index or working file) should end up with — every row kept as it is in the
//! target except the selected changed rows, which are taken from the other side — and then the
//! unified diff from the target to that text, with three lines of real context. Because the patch
//! is an ordinary diff between two complete texts it applies with a plain `git apply --cached`:
//! context, CRLF line endings and "\ No newline at end of file" are all exact.
//!
//! Interleaving: within a changed block the rows keep the order they are drawn in, so selecting
//! the first row of `a b -> A B` stages `A b`, and the second `a B`.

use crate::gitx::{self, Row, RowKind};
use similar::{DiffTag, TextDiff, group_diff_ops};
use std::path::Path;

/// Which way the selected lines go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Bring the selected changes into the *old* text (stage: old = index, new = working file).
    Apply,
    /// Take the selected changes back out of the *new* text (unstage: old = HEAD, new = index;
    /// revert: old = index, new = working file).
    Undo,
}

/// Split keeping the line terminators.
fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// The line ending used when a line that had none must be followed by another one: the
/// target's own style (its first terminated line), else the other text's, else `\n`.
fn eol_of(target: &str, other: &str) -> &'static str {
    let first = |s: &str| s.split_inclusive('\n').find(|l| l.ends_with('\n')).map(|l| l.ends_with("\r\n"));
    match first(target).or_else(|| first(other)) {
        Some(true) => "\r\n",
        _ => "\n",
    }
}

/// The text the patch for `dir` applies to.
pub fn target<'a>(old: &'a str, new: &'a str, dir: Dir) -> &'a str {
    match dir {
        Dir::Apply => old,
        Dir::Undo => new,
    }
}

/// The target text with only the changes of the selected rows applied ([`Dir::Apply`]: the old
/// text plus them) or undone ([`Dir::Undo`]: the new text minus them). `rows` must come from
/// `side_by_side_ws(old, new, _)`; `selected(k)` tells whether row `k` is selected (only changed
/// rows count). A line that had no line ending and is now followed by another one gets one.
pub fn select_text(old: &str, new: &str, rows: &[Row], selected: impl Fn(usize) -> bool, dir: Dir) -> String {
    let (a, b) = (lines(old), lines(new));
    let eol = eol_of(target(old, new, dir), target(new, old, dir));
    let mut out = String::with_capacity(old.len().max(new.len()));
    let mut push = |l: &str| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(l);
    };
    for (k, r) in rows.iter().enumerate() {
        let left = r.left.as_ref().and_then(|(i, _)| a.get(*i).copied());
        let right = r.right.as_ref().and_then(|(j, _)| b.get(*j).copied());
        // Same rows always come from the target (under `-w` the two sides may differ in spacing).
        let from_old = match r.kind {
            RowKind::Same => dir == Dir::Apply,
            _ => (dir == Dir::Apply) != selected(k),
        };
        if let Some(l) = if from_old { left } else { right } {
            push(l);
        }
    }
    out
}

/// Unified diff from `from` to `to` for `rel_path`, `ctx` lines of context, git's conventions for
/// empty ranges and missing final newlines. Empty when the texts are equal.
pub fn unified(rel_path: &str, from: &str, to: &str, ctx: usize) -> String {
    let (a, b) = (lines(from), lines(to));
    let ops = TextDiff::from_slices(&a, &b).ops().to_vec();
    let groups = group_diff_ops(ops, ctx);
    if groups.is_empty() {
        return String::new();
    }
    let mut out = format!("--- a/{rel_path}\n+++ b/{rel_path}\n");
    for g in groups {
        let (Some(first), Some(last)) = (g.first(), g.last()) else { continue };
        let (os, oe) = (first.old_range().start, last.old_range().end);
        let (ns, ne) = (first.new_range().start, last.new_range().end);
        out.push_str(&format!("@@ -{} +{} @@\n", gitx::range_header(os, oe - os), gitx::range_header(ns, ne - ns)));
        for op in g {
            let (tag, o, n) = op.as_tag_tuple();
            match tag {
                DiffTag::Equal => gitx::push_body(&mut out, ' ', &a[o]),
                DiffTag::Delete => gitx::push_body(&mut out, '-', &a[o]),
                DiffTag::Insert => gitx::push_body(&mut out, '+', &b[n]),
                DiffTag::Replace => {
                    gitx::push_body(&mut out, '-', &a[o]);
                    gitx::push_body(&mut out, '+', &b[n]);
                }
            }
        }
    }
    out
}

/// The patch that applies ([`Dir::Apply`]) or undoes ([`Dir::Undo`]) only the selected rows,
/// meant for the target text (old for Apply, new for Undo). None when the selection holds no
/// change (only unchanged rows, or nothing).
pub fn partial_patch(
    rel_path: &str,
    old: &str,
    new: &str,
    rows: &[Row],
    selected: impl Fn(usize) -> bool,
    dir: Dir,
) -> Option<String> {
    let goal = select_text(old, new, rows, selected, dir);
    let from = target(old, new, dir);
    (goal != from).then(|| unified(rel_path, from, &goal, 3))
}

/// Apply a patch made by [`partial_patch`] to the index (`cached`) or to the working tree.
pub fn apply(top: &Path, patch: &str, cached: bool) -> Result<(), String> {
    gitx::git_apply(top, patch, if cached { &["--cached"] } else { &[] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitx::side_by_side_ws;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// `select_text` for the rows whose index is in `sel`.
    fn sel_text(old: &str, new: &str, sel: &[usize], dir: Dir) -> String {
        let rows = side_by_side_ws(old, new, false);
        select_text(old, new, &rows, |k| sel.contains(&k), dir)
    }

    fn kinds(old: &str, new: &str) -> Vec<RowKind> {
        side_by_side_ws(old, new, false).iter().map(|r| r.kind).collect()
    }

    #[test]
    fn nothing_selected_is_the_target_and_everything_is_the_other_side() {
        let cases = [
            ("a\nb\nc\n", "a\nB\nc\nd\n"),
            ("a\nb", "a\nb\nc"),
            ("x\r\ny\r\n", "x\r\nY\r\nz\r\n"),
            ("", "new\n"),
            ("gone\n", ""),
            ("1\n2\n3\n4\n5\n", "0\n1\n3\n4\nfive\n6"),
        ];
        for (old, new) in cases {
            let n = side_by_side_ws(old, new, false).len();
            let all: Vec<usize> = (0..n).collect();
            assert_eq!(sel_text(old, new, &[], Dir::Apply), old, "{old:?} -> {new:?}");
            assert_eq!(sel_text(old, new, &[], Dir::Undo), new);
            assert_eq!(sel_text(old, new, &all, Dir::Apply), new);
            assert_eq!(sel_text(old, new, &all, Dir::Undo), old);
        }
    }

    #[test]
    fn rows_of_a_replaced_block_keep_their_order() {
        let (old, new) = ("a\nb\nz\n", "A\nB\nz\n");
        assert_eq!(kinds(old, new), [RowKind::Changed, RowKind::Changed, RowKind::Same]);
        assert_eq!(sel_text(old, new, &[0], Dir::Apply), "A\nb\nz\n");
        assert_eq!(sel_text(old, new, &[1], Dir::Apply), "a\nB\nz\n");
        // Undo from the new side: the restored line goes back where it was.
        assert_eq!(sel_text(old, new, &[0], Dir::Undo), "a\nB\nz\n");
        assert_eq!(sel_text(old, new, &[1], Dir::Undo), "A\nb\nz\n");
    }

    #[test]
    fn uneven_replace_and_pure_additions_or_removals() {
        // a b c -> X  (one changed row, two removed rows)
        let (old, new) = ("k\na\nb\nc\nk\n", "k\nX\nk\n");
        assert_eq!(
            kinds(old, new),
            [RowKind::Same, RowKind::Changed, RowKind::Removed, RowKind::Removed, RowKind::Same]
        );
        assert_eq!(sel_text(old, new, &[2], Dir::Apply), "k\na\nc\nk\n", "drop only b");
        assert_eq!(sel_text(old, new, &[1], Dir::Apply), "k\nX\nb\nc\nk\n", "only a -> X");
        assert_eq!(sel_text(old, new, &[3], Dir::Undo), "k\nX\nc\nk\n", "put back c only");
        // Additions: stage the second of three new lines.
        let (old, new) = ("a\nz\n", "a\n1\n2\n3\nz\n");
        assert_eq!(sel_text(old, new, &[2], Dir::Apply), "a\n2\nz\n");
        assert_eq!(sel_text(old, new, &[1, 3], Dir::Undo), "a\n2\nz\n");
        // Selecting unchanged rows changes nothing.
        assert_eq!(sel_text(old, new, &[0, 4], Dir::Apply), old);
    }

    #[test]
    fn missing_final_newline_gets_one_only_when_a_line_follows() {
        // "b" had no newline; adding "c" after it must give "b" one.
        let (old, new) = ("a\nb", "a\nb\nc");
        assert_eq!(kinds(old, new), [RowKind::Same, RowKind::Changed, RowKind::Added]);
        assert_eq!(sel_text(old, new, &[2], Dir::Apply), "a\nb\nc");
        // Staging only the "b" -> "b\n" row adds just the newline.
        assert_eq!(sel_text(old, new, &[1], Dir::Apply), "a\nb\n");
        // The new side lost its final newline: undoing only that keeps the rest.
        let (old, new) = ("a\nb\n", "a\nB");
        assert_eq!(sel_text(old, new, &[1], Dir::Undo), "a\nb\n");
        // Undo of a removed last line after a line without newline.
        let (old, new) = ("a\nb\nc", "a\nB");
        let rows = side_by_side_ws(old, new, false);
        let removed = rows.iter().position(|r| r.kind == RowKind::Removed).unwrap();
        assert_eq!(sel_text(old, new, &[removed], Dir::Undo), "a\nB\nc");
    }

    #[test]
    fn crlf_lines_stay_crlf() {
        let (old, new) = ("a\r\nb\r\n", "a\r\nb\r\nc\r\nd\r\n");
        assert_eq!(sel_text(old, new, &[3], Dir::Apply), "a\r\nb\r\nd\r\n");
        // An inserted newline after a last line without one follows the file's style.
        let (old, new) = ("a\r\nb", "a\r\nb\r\nc");
        assert_eq!(sel_text(old, new, &[2], Dir::Apply), "a\r\nb\r\nc");
        let p = partial_patch("f", old, new, &side_by_side_ws(old, new, false), |k| k == 2, Dir::Apply).unwrap();
        assert!(p.contains("-b\n\\ No newline at end of file\n+b\r\n+c\n\\ No newline at end of file\n"), "{p}");
    }

    #[test]
    fn whitespace_only_rows_are_never_carried_under_dash_w() {
        let old = "fn a() {\n  x();\n  y();\n}\n";
        let new = "fn a() {\n    x();\n    Y();\n}\n";
        let rows = side_by_side_ws(old, new, true);
        let k: Vec<_> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(k, [RowKind::Same, RowKind::Same, RowKind::Changed, RowKind::Same]);
        // The changed row is taken whole (its own spacing too); the re-indented x() stays out.
        let got = select_text(old, new, &rows, |k| k == 2, Dir::Apply);
        assert_eq!(got, "fn a() {\n  x();\n    Y();\n}\n");
        let got = select_text(old, new, &rows, |k| k == 2, Dir::Undo);
        assert_eq!(got, "fn a() {\n    x();\n  y();\n}\n");
    }

    #[test]
    fn patch_shape_has_context_and_git_ranges() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = "1\n2\n3\n4\nFIVE\n6\n7\n8\n9\nten\n";
        let rows = side_by_side_ws(old, new, false);
        let five = rows.iter().position(|r| r.kind == RowKind::Changed).unwrap();
        let p = partial_patch("d/f.txt", old, new, &rows, |k| k == five, Dir::Apply).unwrap();
        assert_eq!(p, "--- a/d/f.txt\n+++ b/d/f.txt\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+FIVE\n 6\n 7\n 8\n");
        assert!(partial_patch("f", old, new, &rows, |_| false, Dir::Apply).is_none());
        assert!(partial_patch("f", old, new, &rows, |k| k == 0, Dir::Apply).is_none(), "unchanged row only");
        assert_eq!(unified("f", "a\n", "a\n", 3), "");
        // Insert at the top of an empty file: git's empty range is "-0,0".
        assert!(unified("f", "", "x\n", 3).contains("@@ -0,0 +1 @@\n+x\n"));
    }

    // ───── end to end against a real repository ─────

    static N: AtomicUsize = AtomicUsize::new(0);

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
                "core.autocrlf=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn repo(base: &str, work: &str) -> Repo {
        let d = std::env::temp_dir().join(format!(
            "lumen-gitpatch-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        git(&d, &["init", "-q"]);
        git(&d, &["config", "core.autocrlf", "false"]);
        fs::write(d.join("f.txt"), base).unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", "base"]);
        fs::write(d.join("f.txt"), work).unwrap();
        Repo(d)
    }

    fn index(r: &Repo) -> String {
        git(&r.0, &["show", ":f.txt"])
    }

    /// Stage the selected rows of index -> working file and check the index is exactly what the
    /// builder said; then unstage the same lines from HEAD -> index and get HEAD back.
    fn stage_unstage(base: &str, work: &str, pick: impl Fn(&[Row]) -> Vec<usize>) {
        let r = repo(base, work);
        let rows = side_by_side_ws(base, work, false);
        let sel = pick(&rows);
        let want = select_text(base, work, &rows, |k| sel.contains(&k), Dir::Apply);
        let p = partial_patch("f.txt", base, work, &rows, |k| sel.contains(&k), Dir::Apply).expect("a change");
        apply(&r.0, &p, true).unwrap_or_else(|e| panic!("stage: {e}\n{p}"));
        assert_eq!(index(&r), want, "staged {sel:?} of {base:?} -> {work:?}\n{p}");
        assert_eq!(fs::read_to_string(r.0.join("f.txt")).unwrap(), work, "working file untouched");
        // Unstage everything that is staged now, line by line from the staged side's rows.
        let staged = index(&r);
        let rows = side_by_side_ws(base, &staged, false);
        let all: Vec<usize> = (0..rows.len()).collect();
        if let Some(p) = partial_patch("f.txt", base, &staged, &rows, |k| all.contains(&k), Dir::Undo) {
            apply(&r.0, &p, true).unwrap_or_else(|e| panic!("unstage: {e}\n{p}"));
        }
        assert_eq!(index(&r), base);
    }

    fn changed(rows: &[Row]) -> Vec<usize> {
        rows.iter().enumerate().filter(|(_, r)| r.kind != RowKind::Same).map(|(k, _)| k).collect()
    }

    #[test]
    fn e2e_stage_each_changed_line_alone() {
        let cases = [
            ("1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n", "1\nTWO\n3\n4\n5\nsix\nseven\n8\n9\n10\neleven\n"),
            ("a\nb\nc\n", "x\na\nc\n"),
            ("a\nb", "a\nb\nc"),
            ("a\nb\n", "a\nB"),
            ("a\r\nb\r\nc\r\n", "a\r\nB\r\nc\r\nd\r\n"),
            ("a\r\nb", "a\r\nb\r\nc\r\n"),
            ("", "one\ntwo\n"),
            ("x\nx\nx\nx\nx\nx\nx\nx\n", "x\nx\ny\nx\nx\nx\nx\ny\nx\nx\n"),
        ];
        for (base, work) in cases {
            let rows = side_by_side_ws(base, work, false);
            for k in changed(&rows) {
                stage_unstage(base, work, |_| vec![k]);
            }
            // And every other changed line together.
            stage_unstage(base, work, |rows| changed(rows).into_iter().step_by(2).collect());
        }
    }

    #[test]
    fn e2e_revert_lines_in_the_working_file() {
        let base = "keep\nold one\nold two\nkeep\n";
        let work = "keep\nnew one\nnew two\nkeep\nadded\n";
        let r = repo(base, work);
        let rows = side_by_side_ws(base, work, false);
        // Revert the second changed row and the added line, keep the first change.
        let sel = [2usize, 4];
        let p = partial_patch("f.txt", base, work, &rows, |k| sel.contains(&k), Dir::Undo).unwrap();
        apply(&r.0, &p, false).unwrap();
        assert_eq!(fs::read_to_string(r.0.join("f.txt")).unwrap(), "keep\nnew one\nold two\nkeep\n");
        assert_eq!(index(&r), base, "the index is untouched by a revert");
    }

    #[test]
    fn e2e_stage_lines_under_ignore_whitespace() {
        let base = "a\n  b\nc\nd\n";
        let work = "a\n    b\nC\nd\nE\n";
        let r = repo(base, work);
        let rows = side_by_side_ws(base, work, true);
        assert_eq!(rows[1].kind, RowKind::Same, "re-indent hidden");
        let c = rows.iter().position(|r| r.kind == RowKind::Changed).unwrap();
        let p = partial_patch("f.txt", base, work, &rows, |k| k == c, Dir::Apply).unwrap();
        apply(&r.0, &p, true).unwrap();
        assert_eq!(index(&r), "a\n  b\nC\nd\n", "only C staged, the indentation change stays unstaged");
        // The real (exact) diff still shows the indentation and E as unstaged.
        let left = crate::gitx::hunks(&index(&r), work);
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn e2e_stale_patch_fails_cleanly() {
        let (base, work) = ("a\nb\nc\n", "a\nB\nc\n");
        let r = repo(base, work);
        let rows = side_by_side_ws(base, work, false);
        let p = partial_patch("f.txt", base, work, &rows, |k| k == 1, Dir::Apply).unwrap();
        apply(&r.0, &p, true).unwrap();
        assert!(apply(&r.0, &p, true).is_err(), "the same patch again no longer matches the index");
        assert_eq!(index(&r), work);
    }
}
