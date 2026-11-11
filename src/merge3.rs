//! Three-way merge for the merge editor, without any UI: the diff3 regions of base / ours /
//! theirs (the index stages `:1:`, `:2:`, `:3:` of a conflicted file), the text each choice puts
//! in the result, where every region sits in a result the user edits freely, and what each
//! conflict has been resolved to. The App glue lives in `app_merge.rs`.
//!
//! Naming follows git: "ours" is the *current* side (HEAD; during a rebase the branch being
//! rebased onto), "theirs" the *incoming* one (the merged branch, the replayed commit).

use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::{
    ops::Range,
    path::Path,
    process::{Command, Stdio},
};

/// What happened to a region relative to the base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Untouched on both sides.
    Same,
    /// Changed only on our side (taken automatically).
    Ours,
    /// Changed only on their side (taken automatically).
    Theirs,
    /// Changed identically on both sides.
    Both,
    /// Changed differently: the user decides.
    Conflict,
}

/// One region of the merge; ranges are 0-based line indexes into each version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub kind: Kind,
    pub base: Range<usize>,
    pub ours: Range<usize>,
    pub theirs: Range<usize>,
}

/// What to put in the result for a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Current,
    Incoming,
    /// Both, current first.
    CurrentIncoming,
    /// Both, incoming first.
    IncomingCurrent,
    /// Back to conflict markers.
    Markers,
}

/// What a conflict holds in the result right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Unresolved,
    Current,
    Incoming,
    CurrentIncoming,
    IncomingCurrent,
    /// Neither side: the base text is back.
    Base,
    /// Anything else the user typed.
    Manual,
}

impl State {
    pub fn resolved(self) -> bool {
        self != State::Unresolved
    }
    /// Does the result hold the current (ours) side?
    pub fn has_current(self) -> bool {
        matches!(self, State::Current | State::CurrentIncoming | State::IncomingCurrent)
    }
    pub fn has_incoming(self) -> bool {
        matches!(self, State::Incoming | State::CurrentIncoming | State::IncomingCurrent)
    }
}

/// One row of the aligned Incoming | Current panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub region: usize,
    pub theirs: Option<usize>,
    pub ours: Option<usize>,
}

/// Split keeping the line terminators (`"a\nb"` -> `["a\n", "b"]`).
pub fn split_lines(s: &str) -> Vec<String> {
    s.split_inclusive('\n').map(str::to_string).collect()
}

/// A line without its `\n` / `\r\n`.
pub fn chomp(s: &str) -> &str {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.strip_suffix('\r').unwrap_or(s)
}

fn chomped(ls: &[String]) -> Vec<&str> {
    ls.iter().map(|l| chomp(l)).collect()
}

/// A conflict marker line: `<<<<<<<` or `>>>>>>>` (optionally followed by a label) or `|||||||`.
pub fn is_marker(line: &str) -> bool {
    let l = chomp(line);
    ["<<<<<<<", ">>>>>>>", "|||||||"]
        .iter()
        .any(|m| l.strip_prefix(m).is_some_and(|rest| rest.is_empty() || rest.starts_with(' ')))
}

/// Does `text` still hold conflict markers?
pub fn has_markers(text: &str) -> bool {
    text.lines().any(is_marker)
}

/// For each line of `a`, the line of `b` it matches in a line diff.
fn line_map(a: &[&str], b: &[&str]) -> Vec<Option<usize>> {
    let mut m = vec![None; a.len()];
    for op in capture_diff_slices(Algorithm::Myers, a, b) {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            for (i, j) in o.zip(n) {
                m[i] = Some(j);
            }
        }
    }
    m
}

/// The diff3 regions of three versions (lines compared without their terminators). Conflicts are
/// refined like git's "zealous" merge: lines both sides share at the start, end or middle of a
/// conflict are taken out of it, so the conflicts line up with the markers git writes.
pub fn diff3(base: &[&str], ours: &[&str], theirs: &[&str]) -> Vec<Region> {
    let mo = line_map(base, ours);
    let mt = line_map(base, theirs);
    let (mut i, mut j, mut k) = (0, 0, 0);
    let mut out = Vec::new();
    loop {
        let (si, sj, sk) = (i, j, k);
        while i < base.len() && mo[i] == Some(j) && mt[i] == Some(k) {
            i += 1;
            j += 1;
            k += 1;
        }
        if i > si {
            out.push(Region { kind: Kind::Same, base: si..i, ours: sj..j, theirs: sk..k });
        }
        if i >= base.len() && j >= ours.len() && k >= theirs.len() {
            break;
        }
        // The next base line both sides kept is where the changed chunk ends.
        let next = (i..base.len()).find(|&x| matches!((mo[x], mt[x]), (Some(a), Some(b)) if a >= j && b >= k));
        let (ni, nj, nk) = match next {
            Some(x) => (x, mo[x].unwrap_or(j), mt[x].unwrap_or(k)),
            None => (base.len(), ours.len(), theirs.len()),
        };
        push_chunk(&mut out, base, ours, theirs, i..ni, j..nj, k..nk);
        (i, j, k) = (ni, nj, nk);
        if next.is_none() {
            break;
        }
    }
    out
}

fn push_chunk(
    out: &mut Vec<Region>,
    base: &[&str],
    ours: &[&str],
    theirs: &[&str],
    b: Range<usize>,
    o: Range<usize>,
    t: Range<usize>,
) {
    let (bs, os, ts) = (&base[b.clone()], &ours[o.clone()], &theirs[t.clone()]);
    let kind = if os == bs {
        Kind::Theirs
    } else if ts == bs {
        Kind::Ours
    } else if os == ts {
        Kind::Both
    } else {
        Kind::Conflict
    };
    if kind != Kind::Conflict {
        out.push(Region { kind, base: b, ours: o, theirs: t });
        return;
    }
    // Refine: what both sides share inside the conflict is not in conflict.
    let mut base_left = Some(b.clone());
    let mut conflict: Option<(Range<usize>, Range<usize>)> = None;
    let flush = |out: &mut Vec<Region>, c: &mut Option<(Range<usize>, Range<usize>)>, bl: &mut Option<Range<usize>>| {
        if let Some((co, ct)) = c.take() {
            // The whole base goes with the first conflict piece (it cannot be split reliably).
            let base = bl.take().unwrap_or(b.end..b.end);
            out.push(Region { kind: Kind::Conflict, base, ours: co, theirs: ct });
        }
    };
    for op in capture_diff_slices(Algorithm::Myers, os, ts) {
        let (tag, x, y) = op.as_tag_tuple();
        let (x, y) = (o.start + x.start..o.start + x.end, t.start + y.start..t.start + y.end);
        if tag == DiffTag::Equal {
            flush(out, &mut conflict, &mut base_left);
            let at = base_left.as_ref().map_or(b.end, |r| r.start);
            out.push(Region { kind: Kind::Both, base: at..at, ours: x, theirs: y });
        } else {
            conflict = Some(match conflict.take() {
                Some((co, ct)) => (co.start..x.end.max(co.end), ct.start..y.end.max(ct.end)),
                None => (x, y),
            });
        }
    }
    flush(out, &mut conflict, &mut base_left);
    // A conflict whose base was never attached (cannot happen, but keep the base lines covered).
    if let Some(bl) = base_left
        && !bl.is_empty()
        && let Some(last) = out.iter_mut().rev().find(|r| r.kind == Kind::Conflict)
    {
        last.base = last.base.start.min(bl.start)..last.base.end.max(bl.end);
    }
}

/// A merge of three versions, with everything the editor needs to show and resolve it.
#[derive(Clone, Debug)]
pub struct Merge {
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
    pub regions: Vec<Region>,
    /// Line ending used for markers and added breaks (the one of the current side).
    pub eol: &'static str,
}

impl Merge {
    pub fn new(base: &str, ours: &str, theirs: &str) -> Merge {
        let (b, o, t) = (split_lines(base), split_lines(ours), split_lines(theirs));
        let regions = diff3(&chomped(&b), &chomped(&o), &chomped(&t));
        let eol = if ours.contains("\r\n") { "\r\n" } else { "\n" };
        Merge { base: b, ours: o, theirs: t, regions, eol }
    }

    /// Indexes (into `regions`) of the conflicts, in file order.
    pub fn conflicts(&self) -> Vec<usize> {
        (0..self.regions.len()).filter(|&k| self.regions[k].kind == Kind::Conflict).collect()
    }

    pub fn ours_of(&self, k: usize) -> &[String] {
        &self.ours[self.regions[k].ours.clone()]
    }

    pub fn theirs_of(&self, k: usize) -> &[String] {
        &self.theirs[self.regions[k].theirs.clone()]
    }

    /// What a non-conflict region puts in the result; None for a conflict.
    pub fn fixed(&self, k: usize) -> Option<&[String]> {
        match self.regions[k].kind {
            Kind::Same | Kind::Ours | Kind::Both => Some(self.ours_of(k)),
            Kind::Theirs => Some(self.theirs_of(k)),
            Kind::Conflict => None,
        }
    }

    /// Lines joined into text; every line but possibly the very last one of the file ends with a
    /// line break (`at_end`: nothing follows this text in the result).
    fn join(&self, parts: &[&[String]], at_end: bool) -> String {
        let all: Vec<&String> = parts.iter().flat_map(|p| p.iter()).collect();
        let mut s = String::new();
        for (i, l) in all.iter().enumerate() {
            s.push_str(l);
            if !l.ends_with('\n') && (i + 1 < all.len() || !at_end) {
                s.push_str(self.eol);
            }
        }
        s
    }

    /// The text conflict region `k` gets for `take`. `labels` = (current, incoming) marker labels.
    pub fn take_text(&self, k: usize, take: Take, labels: (&str, &str)) -> String {
        let at_end = k + 1 == self.regions.len();
        let (o, t) = (self.ours_of(k), self.theirs_of(k));
        match take {
            Take::Current => self.join(&[o], at_end),
            Take::Incoming => self.join(&[t], at_end),
            Take::CurrentIncoming => self.join(&[o, t], at_end),
            Take::IncomingCurrent => self.join(&[t, o], at_end),
            Take::Markers => {
                let eol = self.eol;
                let label = |m: &str, l: &str| if l.is_empty() { format!("{m}{eol}") } else { format!("{m} {l}{eol}") };
                let mut s = label("<<<<<<<", labels.0);
                s.push_str(&self.join(&[o], false));
                s.push_str(&format!("======={eol}"));
                s.push_str(&self.join(&[t], false));
                s.push_str(&label(">>>>>>>", labels.1));
                s
            }
        }
    }

    /// The whole result with `takes[i]` applied to the i-th conflict (missing ones get markers).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn assemble(&self, takes: &[Take], labels: (&str, &str)) -> String {
        let mut s = String::new();
        let mut ci = 0;
        for k in 0..self.regions.len() {
            match self.fixed(k) {
                Some(ls) => s.push_str(&self.join(&[ls], k + 1 == self.regions.len())),
                None => {
                    let take = takes.get(ci).copied().unwrap_or(Take::Markers);
                    s.push_str(&self.take_text(k, take, labels));
                    ci += 1;
                }
            }
        }
        s
    }

    /// Where every region sits in `result` (a line range per region), however the user edited it.
    /// The text of the non-conflict regions anchors the search (a line diff against the result);
    /// a conflict owns whatever lies between its neighbours.
    pub fn locate(&self, result: &str) -> Vec<Range<usize>> {
        let res_lines = split_lines(result);
        let res = chomped(&res_lines);
        let n = self.regions.len();
        let mut anchor: Vec<&str> = Vec::new();
        let mut a_start = Vec::with_capacity(n + 1);
        for k in 0..n {
            a_start.push(anchor.len());
            if let Some(ls) = self.fixed(k) {
                anchor.extend(ls.iter().map(|l| chomp(l)));
            }
        }
        a_start.push(anchor.len());
        let m = line_map(&anchor, &res);
        // Position in the result right after the last matched anchor line before `a`, plus the
        // unmatched anchor lines in between.
        let estimate = |a: usize| -> usize {
            match (0..a).rev().find(|&i| m[i].is_some()) {
                Some(i) => m[i].unwrap_or(0) + 1 + (a - 1 - i),
                None => a,
            }
        };
        let mut b = vec![0usize; n + 1];
        b[n] = res.len();
        for k in 1..n {
            let a = a_start[k];
            let prev_anchor = self.regions[k - 1].kind != Kind::Conflict && a > a_start[k - 1];
            let next_anchor = self.regions[k].kind != Kind::Conflict && a < a_start[k + 1];
            let pos = if prev_anchor && let Some(j) = m[a - 1] {
                j + 1
            } else if next_anchor && let Some(j) = m[a] {
                j
            } else {
                estimate(a)
            };
            b[k] = pos.clamp(b[k - 1], res.len());
        }
        for k in (1..n).rev() {
            b[k] = b[k].min(b[k + 1]);
        }
        (0..n).map(|k| b[k]..b[k + 1]).collect()
    }

    /// What conflict region `k` holds, given its lines in the result.
    pub fn state(&self, k: usize, lines: &[&str]) -> State {
        if lines.iter().any(|l| is_marker(l)) {
            return State::Unresolved;
        }
        let (o, t) = (chomped(self.ours_of(k)), chomped(self.theirs_of(k)));
        let cat = |a: &[&str], b: &[&str]| -> Vec<String> { a.iter().chain(b).map(|s| s.to_string()).collect() };
        let got: Vec<String> = lines.iter().map(|l| chomp(l).to_string()).collect();
        let same = |v: &[&str]| v.len() == got.len() && v.iter().zip(&got).all(|(a, b)| *a == b.as_str());
        if same(&o) {
            State::Current
        } else if same(&t) {
            State::Incoming
        } else if got == cat(&o, &t) {
            State::CurrentIncoming
        } else if got == cat(&t, &o) {
            State::IncomingCurrent
        } else if same(&chomped(&self.base[self.regions[k].base.clone()])) {
            State::Base
        } else {
            State::Manual
        }
    }

    /// Located ranges and the state of every conflict of `result` (in `conflicts()` order).
    pub fn states(&self, result: &str) -> (Vec<Range<usize>>, Vec<State>) {
        let loc = self.locate(result);
        let lines = split_lines(result);
        let states = self
            .conflicts()
            .into_iter()
            .map(|k| {
                let r = loc[k].clone();
                let ls: Vec<&str> =
                    lines[r.start.min(lines.len())..r.end.min(lines.len())].iter().map(|s| s.as_str()).collect();
                self.state(k, &ls)
            })
            .collect();
        (loc, states)
    }

    /// The Incoming | Current panes aligned row by row, and the row span of every region.
    pub fn rows(&self) -> (Vec<Row>, Vec<Range<usize>>) {
        let mut rows = Vec::new();
        let mut spans = Vec::with_capacity(self.regions.len());
        for (k, r) in self.regions.iter().enumerate() {
            let start = rows.len();
            for t in 0..r.ours.len().max(r.theirs.len()) {
                rows.push(Row {
                    region: k,
                    theirs: (r.theirs.start + t < r.theirs.end).then_some(r.theirs.start + t),
                    ours: (r.ours.start + t < r.ours.end).then_some(r.ours.start + t),
                });
            }
            spans.push(start..rows.len());
        }
        (rows, spans)
    }
}

/// Map a (fractional) position through piecewise ranges: `x` in `from[k]` lands proportionally
/// in `to[k]`. Used both ways between result lines and pane rows (synchronised scrolling).
pub fn map_pos(from: &[Range<usize>], to: &[Range<usize>], x: f64) -> f64 {
    for (f, t) in from.iter().zip(to) {
        if f.is_empty() {
            continue;
        }
        if x < f.end as f64 {
            let off = (x - f.start as f64).max(0.0);
            return t.start as f64 + off * t.len() as f64 / f.len() as f64;
        }
    }
    let (fe, te) = (from.last().map_or(0, |r| r.end), to.last().map_or(0, |r| r.end));
    te as f64 + (x - fe as f64).max(0.0)
}

// ───────────── git ─────────────

fn git_show(top: &Path, spec: &str) -> Option<String> {
    let o = Command::new("git").arg("-C").arg(top).args(["show", spec]).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// The three index stages of conflicted file `rel` (repository-relative, `/` separated):
/// (base, ours, theirs). A missing base (both sides added the file) is empty.
pub fn stages(top: &Path, rel: &str) -> Result<(String, String, String), String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["ls-files", "-u", "-z", "--", rel])
        .output()
        .map_err(|e| e.to_string())?;
    let listing = String::from_utf8_lossy(&o.stdout).into_owned();
    if listing.trim().is_empty() {
        return Err(format!("{rel} has no merge conflict"));
    }
    let has = |n: char| listing.split('\0').any(|e| e.split('\t').next().is_some_and(|m| m.ends_with(n)));
    if !has('2') || !has('3') {
        return Err(format!("{rel} was deleted on one side: keep or remove it from the Git panel or a terminal"));
    }
    let base = if has('1') { git_show(top, &format!(":1:{rel}")).unwrap_or_default() } else { String::new() };
    let ours = git_show(top, &format!(":2:{rel}")).ok_or("could not read the current version (:2)")?;
    let theirs = git_show(top, &format!(":3:{rel}")).ok_or("could not read the incoming version (:3)")?;
    Ok((base, ours, theirs))
}

/// "Mark resolved": refuse while conflict markers remain in `text` (the saved result), else stage
/// the file (`git add`), which clears its conflict in the index.
pub fn mark_resolved(top: &Path, rel: &str, text: &str) -> Result<(), String> {
    if has_markers(text) {
        return Err("conflict markers remain in the result".into());
    }
    let o = Command::new("git").arg("-C").arg(top).args(["add", "--", rel]).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
}

/// Files still in conflict (repository-relative).
pub fn unmerged(top: &Path) -> Vec<String> {
    Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["diff", "--name-only", "--diff-filter=U", "-z"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(m: &Merge) -> Vec<Kind> {
        m.regions.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn regions_classify_one_sided_identical_and_conflicting_changes() {
        let base = "a\nb\nc\nd\ne\nf\ng\n";
        let ours = "a\nB\nc\nd\ne\nF1\ng\n";
        let theirs = "a\nb\nc\nD\ne\nF2\ng\n";
        let m = Merge::new(base, ours, theirs);
        use Kind::*;
        assert_eq!(kinds(&m), vec![Same, Ours, Same, Theirs, Same, Conflict, Same]);
        assert_eq!(m.regions[1].ours, 1..2);
        assert_eq!(m.regions[3].theirs, 3..4);
        let c = &m.regions[5];
        assert_eq!((c.base.clone(), c.ours.clone(), c.theirs.clone()), (5..6, 5..6, 5..6));
        assert_eq!(m.conflicts(), vec![5]);
        assert_eq!(m.assemble(&[Take::Current], ("", "")), "a\nB\nc\nD\ne\nF1\ng\n");
        // Changes on neighbouring lines conflict (as in git).
        let m = Merge::new("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nC\nd\n");
        assert_eq!(kinds(&m), vec![Same, Conflict, Same]);
        // Same change on both sides is not a conflict.
        let m = Merge::new("x\ny\n", "x\nY\n", "x\nY\n");
        assert_eq!(kinds(&m), vec![Kind::Same, Kind::Both]);
        // Nothing changed.
        assert_eq!(kinds(&Merge::new("x\n", "x\n", "x\n")), vec![Kind::Same]);
    }

    #[test]
    fn insertions_at_the_same_place_conflict_and_deletions_are_regions_too() {
        let m = Merge::new("a\nz\n", "a\nours\nz\n", "a\ntheirs\nz\n");
        assert_eq!(kinds(&m), vec![Kind::Same, Kind::Conflict, Kind::Same]);
        assert_eq!(m.regions[1].base, 1..1, "an insertion has an empty base");
        // Ours deletes a line theirs left alone: taken automatically.
        let m = Merge::new("a\nb\nc\n", "a\nc\n", "a\nb\nc\n");
        assert_eq!(kinds(&m), vec![Kind::Same, Kind::Ours, Kind::Same]);
        assert_eq!(m.assemble(&[], ("", "")), "a\nc\n");
        // Ours deletes a line theirs changed: conflict with an empty current side.
        let m = Merge::new("a\nb\nc\n", "a\nc\n", "a\nB\nc\n");
        assert_eq!(kinds(&m), vec![Kind::Same, Kind::Conflict, Kind::Same]);
        assert!(m.regions[1].ours.is_empty());
    }

    #[test]
    fn conflicts_are_refined_like_git_zealous_merges() {
        // Both sides rewrote the block but kept its first and last lines and one in the middle.
        let base = "fn f() {\n    old();\n}\n";
        let ours = "fn f() {\n    one();\n    keep();\n    two();\n}\n";
        let theirs = "fn f() {\n    uno();\n    keep();\n    dos();\n}\n";
        let m = Merge::new(base, ours, theirs);
        assert_eq!(
            kinds(&m),
            vec![Kind::Same, Kind::Conflict, Kind::Both, Kind::Conflict, Kind::Same],
            "{:?}",
            m.regions
        );
        assert_eq!(m.conflicts().len(), 2);
        // The base goes with the first piece; ranges stay ordered.
        assert_eq!(m.regions[1].base, 1..2);
        assert_eq!(m.regions[3].base, 2..2);
        // Taking a side per piece rebuilds a whole version.
        assert_eq!(m.assemble(&[Take::Current, Take::Current], ("", "")), ours);
        assert_eq!(m.assemble(&[Take::Incoming, Take::Incoming], ("", "")), theirs);
    }

    #[test]
    fn assembly_applies_every_take_and_writes_markers_for_the_rest() {
        let base = "a\nb\nc\n";
        let m = Merge::new(base, "a\nO\nc\n", "a\nT\nc\n");
        let l = ("HEAD", "feature");
        assert_eq!(m.assemble(&[Take::Current], l), "a\nO\nc\n");
        assert_eq!(m.assemble(&[Take::Incoming], l), "a\nT\nc\n");
        assert_eq!(m.assemble(&[Take::CurrentIncoming], l), "a\nO\nT\nc\n");
        assert_eq!(m.assemble(&[Take::IncomingCurrent], l), "a\nT\nO\nc\n");
        assert_eq!(m.assemble(&[], l), "a\n<<<<<<< HEAD\nO\n=======\nT\n>>>>>>> feature\nc\n");
        assert_eq!(m.assemble(&[Take::Markers], ("", "")), "a\n<<<<<<<\nO\n=======\nT\n>>>>>>>\nc\n");
    }

    #[test]
    fn assembly_keeps_a_missing_final_newline_and_crlf() {
        let m = Merge::new("a\nb", "a\nours", "a\ntheirs");
        assert_eq!(m.assemble(&[Take::Current], ("", "")), "a\nours");
        assert_eq!(m.assemble(&[Take::CurrentIncoming], ("", "")), "a\nours\ntheirs");
        assert!(m.assemble(&[Take::Markers], ("x", "y")).ends_with("theirs\n>>>>>>> y\n"));
        let m = Merge::new("a\r\nb\r\n", "a\r\nO\r\n", "a\r\nT\r\n");
        assert_eq!(m.eol, "\r\n");
        assert_eq!(m.assemble(&[Take::Markers], ("x", "y")), "a\r\n<<<<<<< x\r\nO\r\n=======\r\nT\r\n>>>>>>> y\r\n");
    }

    #[test]
    fn locate_follows_choices_and_free_edits() {
        let base = "top\na\nmid\nb\nend\n";
        let m = Merge::new(base, "top\nA1\nmid\nB1\nend\n", "top\nA2\nmid\nB2\nend\n");
        assert_eq!(m.conflicts(), vec![1, 3]);
        let labels = ("HEAD", "x");
        // Fresh result: markers everywhere.
        let r = m.assemble(&[], labels);
        let (loc, st) = m.states(&r);
        assert_eq!(st, vec![State::Unresolved, State::Unresolved]);
        assert_eq!(loc[1], 1..6);
        assert_eq!(loc[2], 6..7);
        // One taken, one still open.
        let r = m.assemble(&[Take::IncomingCurrent, Take::Markers], labels);
        assert_eq!(m.states(&r).1, vec![State::IncomingCurrent, State::Unresolved]);
        // Both taken in some way.
        let r = m.assemble(&[Take::Current, Take::Incoming], labels);
        let (loc, st) = m.states(&r);
        assert_eq!(st, vec![State::Current, State::Incoming]);
        assert_eq!(loc, vec![0..1, 1..2, 2..3, 3..4, 4..5]);
        // The user types inside a conflict and adds lines above everything: still found.
        let r =
            format!("// new header\n{}", m.assemble(&[Take::Current, Take::Current], labels).replace("B1", "B1 + B2"));
        let (loc, st) = m.states(&r);
        assert_eq!(st, vec![State::Current, State::Manual]);
        assert_eq!(loc[3], 4..5);
        // A conflict resolved to nothing, one back to the base text.
        let r = "top\nmid\nb\nend\n";
        let (loc, st) = m.states(r);
        assert_eq!(st, vec![State::Manual, State::Base]);
        assert!(loc[1].is_empty());
    }

    #[test]
    fn locate_finds_git_written_markers_with_base_sections() {
        // git's diff3 style puts the base between ||||||| and =======.
        let m = Merge::new("a\nb\nc\n", "a\nO\nc\n", "a\nT\nc\n");
        let git = "a\n<<<<<<< HEAD\nO\n||||||| base\nb\n=======\nT\n>>>>>>> feat\nc\n";
        let (loc, st) = m.states(git);
        assert_eq!(st, vec![State::Unresolved]);
        assert_eq!(loc[1], 1..8);
        assert_eq!(loc[2], 8..9);
    }

    #[test]
    fn markers_are_recognised_but_lookalikes_are_not() {
        assert!(is_marker("<<<<<<< HEAD"));
        assert!(is_marker(">>>>>>>"));
        assert!(is_marker("||||||| merged common ancestors\r\n"));
        assert!(!is_marker("<<<<<<<< too long"));
        assert!(!is_marker("<<<<<<<x"));
        assert!(!is_marker("======="), "a lone separator is a Markdown heading underline");
        assert!(has_markers("a\n<<<<<<< x\nb\n"));
        assert!(!has_markers("a\n=======\nb\n"));
    }

    #[test]
    fn rows_align_both_panes_and_map_to_result_lines() {
        let m = Merge::new("a\nb\nc\n", "a\nO1\nO2\nO3\nc\n", "a\nT\nc\n");
        let (rows, spans) = m.rows();
        assert_eq!(spans, vec![0..1, 1..4, 4..5]);
        assert_eq!(rows[1], Row { region: 1, theirs: Some(1), ours: Some(1) });
        assert_eq!(rows[3], Row { region: 1, theirs: None, ours: Some(3) });
        // Result took the incoming side (1 line where the panes have 3 rows).
        let loc = m.locate("a\nT\nc\n");
        assert_eq!(loc, vec![0..1, 1..2, 2..3]);
        assert_eq!(map_pos(&loc, &spans, 0.0), 0.0);
        assert_eq!(map_pos(&loc, &spans, 1.0), 1.0);
        assert_eq!(map_pos(&loc, &spans, 1.5), 2.5, "inside a region: proportional");
        assert_eq!(map_pos(&loc, &spans, 2.0), 4.0);
        assert_eq!(map_pos(&spans, &loc, 3.0), 1.0 + 2.0 / 3.0);
        assert_eq!(map_pos(&spans, &loc, 4.0), 2.0);
        assert_eq!(map_pos(&loc, &spans, 9.0), 11.0, "past the end: one to one");
    }

    // ── against real repositories ──

    fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).into_owned())
        }
    }

    /// A repository where `main` and `feat` changed `f.txt` differently, on `main`.
    fn conflicted_repo(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-merge3-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        git(&d, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(d.join("f.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        git(&d, &["add", "."]).unwrap();
        git(&d, &["commit", "-q", "-m", "base"]).unwrap();
        git(&d, &["checkout", "-q", "-b", "feat"]).unwrap();
        std::fs::write(d.join("f.txt"), "one\nTWO feat\nthree\nfour\nFIVE feat\n").unwrap();
        git(&d, &["commit", "-q", "-am", "feat"]).unwrap();
        git(&d, &["checkout", "-q", "main"]).unwrap();
        std::fs::write(d.join("f.txt"), "one\nTWO main\nthree\nfour\nFIVE main\n").unwrap();
        git(&d, &["commit", "-q", "-am", "main"]).unwrap();
        d
    }

    #[test]
    fn merge_conflict_resolves_and_marks_resolved_in_a_real_repo() {
        let d = conflicted_repo("merge");
        assert!(git(&d, &["merge", "feat"]).is_err(), "the merge must stop on the conflict");
        let (base, ours, theirs) = stages(&d, "f.txt").unwrap();
        assert!(ours.contains("TWO main") && theirs.contains("TWO feat") && base.contains("two"));
        let m = Merge::new(&base, &ours, &theirs);
        assert_eq!(m.conflicts().len(), 2);
        // git's own working file is understood as two open conflicts.
        let on_disk = std::fs::read_to_string(d.join("f.txt")).unwrap();
        assert_eq!(m.states(&on_disk).1, vec![State::Unresolved, State::Unresolved]);
        // Refused while markers remain.
        assert!(mark_resolved(&d, "f.txt", &on_disk).is_err());
        assert_eq!(unmerged(&d), vec!["f.txt".to_string()]);
        // Resolve: incoming then current for the first, current for the second.
        let result = m.assemble(&[Take::IncomingCurrent, Take::Current], ("HEAD", "feat"));
        assert_eq!(result, "one\nTWO feat\nTWO main\nthree\nfour\nFIVE main\n");
        std::fs::write(d.join("f.txt"), &result).unwrap();
        mark_resolved(&d, "f.txt", &result).unwrap();
        assert!(unmerged(&d).is_empty());
        assert_eq!(git(&d, &["show", ":f.txt"]).unwrap(), result, "the result is what got staged");
        git(&d, &["commit", "-q", "--no-edit"]).unwrap();
        assert!(stages(&d, "f.txt").is_err(), "no conflict any more");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rebase_and_cherry_pick_conflicts_load_the_same_way() {
        // Rebase feat onto main: "ours" is main's version, "theirs" the replayed feat commit.
        let d = conflicted_repo("rebase");
        git(&d, &["checkout", "-q", "feat"]).unwrap();
        assert!(git(&d, &["rebase", "main"]).is_err());
        let (_, ours, theirs) = stages(&d, "f.txt").unwrap();
        assert!(ours.contains("TWO main") && theirs.contains("TWO feat"));
        let m = Merge::new(&stages(&d, "f.txt").unwrap().0, &ours, &theirs);
        let result = m.assemble(&[Take::Incoming, Take::Incoming], ("", ""));
        std::fs::write(d.join("f.txt"), &result).unwrap();
        mark_resolved(&d, "f.txt", &result).unwrap();
        assert!(unmerged(&d).is_empty());
        git(&d, &["-c", "core.editor=true", "rebase", "--continue"]).unwrap();
        std::fs::remove_dir_all(&d).ok();

        let d = conflicted_repo("pick");
        let feat = git(&d, &["rev-parse", "feat"]).unwrap();
        assert!(git(&d, &["cherry-pick", feat.trim()]).is_err());
        let (base, ours, theirs) = stages(&d, "f.txt").unwrap();
        let m = Merge::new(&base, &ours, &theirs);
        assert_eq!(m.conflicts().len(), 2);
        let result = m.assemble(&[Take::Current, Take::CurrentIncoming], ("", ""));
        assert!(result.ends_with("FIVE main\nFIVE feat\n"));
        std::fs::write(d.join("f.txt"), &result).unwrap();
        mark_resolved(&d, "f.txt", &result).unwrap();
        assert!(unmerged(&d).is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn files_deleted_on_one_side_or_not_in_conflict_are_refused() {
        let d = conflicted_repo("del");
        git(&d, &["checkout", "-q", "feat"]).unwrap();
        git(&d, &["rm", "-q", "f.txt"]).unwrap();
        git(&d, &["commit", "-q", "-m", "rm"]).unwrap();
        git(&d, &["checkout", "-q", "main"]).unwrap();
        assert!(git(&d, &["merge", "feat"]).is_err());
        assert!(stages(&d, "f.txt").unwrap_err().contains("deleted"));
        assert!(stages(&d, "nope.txt").unwrap_err().contains("no merge conflict"));
        std::fs::remove_dir_all(&d).ok();
    }
}
