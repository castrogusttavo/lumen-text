//! Test coverage without any UI: LCOV (`lcov.info`) and Cobertura XML parsers, path resolution,
//! the per-file report, and the gutter marks of an open document, which follow its edits (lines
//! inserted or removed above move them; a line whose text changed loses its mark, since its
//! coverage is no longer known). The App glue lives in `app_coverage.rs`.

use crate::buffer::{EditRec, LineOp};
use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    path::{Component, Path, PathBuf},
};

/// Coverage of one source file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileCov {
    /// 1-based line -> execution count.
    pub lines: BTreeMap<u32, u64>,
    /// 1-based line -> (branches taken, branches).
    pub branches: BTreeMap<u32, (u32, u32)>,
    pub fn_found: u32,
    pub fn_hit: u32,
    /// `LF` / `LH` as reported (used when there are no `DA` lines).
    lf: u32,
    lh: u32,
}

impl FileCov {
    /// (covered, instrumented) lines.
    pub fn line_counts(&self) -> (u32, u32) {
        if self.lines.is_empty() {
            return (self.lh, self.lf);
        }
        (self.lines.values().filter(|h| **h > 0).count() as u32, self.lines.len() as u32)
    }

    /// Share of instrumented lines that ran, 0..=100; None when nothing is instrumented.
    pub fn pct(&self) -> Option<f32> {
        let (hit, found) = self.line_counts();
        (found > 0).then(|| hit as f32 * 100.0 / found as f32)
    }

    pub fn branch_counts(&self) -> (u32, u32) {
        self.branches.values().fold((0, 0), |(a, b), (t, n)| (a + t, b + n))
    }

    /// "87% of lines (52/60) · branches 3/4 · functions 2/2", the parts that are known.
    pub fn describe(&self) -> String {
        let (hit, found) = self.line_counts();
        let mut s = match self.pct() {
            Some(p) => format!("{p:.0}% of lines ({hit}/{found})"),
            None => "no instrumented lines".to_string(),
        };
        let (bt, bn) = self.branch_counts();
        if bn > 0 {
            s.push_str(&format!(" · branches {bt}/{bn}"));
        }
        if self.fn_found > 0 {
            s.push_str(&format!(" · functions {}/{}", self.fn_hit, self.fn_found));
        }
        s
    }

    /// Fold another record of the same file in (several test runs / `TN:` blocks).
    fn merge(&mut self, o: FileCov) {
        for (l, h) in o.lines {
            *self.lines.entry(l).or_insert(0) += h;
        }
        for (l, (t, n)) in o.branches {
            let e = self.branches.entry(l).or_insert((0, 0));
            *e = (e.0.max(t), e.1.max(n));
        }
        self.fn_found = self.fn_found.max(o.fn_found);
        self.fn_hit = self.fn_hit.max(o.fn_hit);
        self.lf = self.lf.max(o.lf);
        self.lh = self.lh.max(o.lh);
    }
}

/// Parse an LCOV tracefile: `(SF path as written, coverage)` per source file, in file order,
/// records of the same file merged.
pub fn parse_lcov(text: &str) -> Vec<(String, FileCov)> {
    let mut out: Vec<(String, FileCov)> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut cur: Option<(String, FileCov)> = None;
    // Per record: branch outcomes by (line, block, branch) and functions by name.
    let mut brda: HashMap<(u32, String, String), bool> = HashMap::new();
    let mut fns: HashMap<String, u64> = HashMap::new();
    let (mut fnf, mut fnh) = (None::<u32>, None::<u32>);
    let mut finish = |cur: &mut Option<(String, FileCov)>,
                      brda: &mut HashMap<(u32, String, String), bool>,
                      fns: &mut HashMap<String, u64>,
                      fnf: &mut Option<u32>,
                      fnh: &mut Option<u32>| {
        let Some((path, mut fc)) = cur.take() else { return };
        for ((line, _, _), taken) in brda.drain() {
            let e = fc.branches.entry(line).or_insert((0, 0));
            e.1 += 1;
            if taken {
                e.0 += 1;
            }
        }
        if fns.is_empty() {
            fc.fn_found = fnf.unwrap_or(0);
            fc.fn_hit = fnh.unwrap_or(0);
        } else {
            fc.fn_found = fns.len() as u32;
            fc.fn_hit = fns.values().filter(|h| **h > 0).count() as u32;
        }
        fns.clear();
        (*fnf, *fnh) = (None, None);
        match idx.get(&path) {
            Some(&i) => out[i].1.merge(fc),
            None => {
                idx.insert(path.clone(), out.len());
                out.push((path, fc));
            }
        }
    };
    for raw in text.lines() {
        let l = raw.trim();
        let (tag, val) = l.split_once(':').unwrap_or((l, ""));
        match tag {
            "SF" => {
                finish(&mut cur, &mut brda, &mut fns, &mut fnf, &mut fnh);
                cur = Some((val.trim().to_string(), FileCov::default()));
            }
            "end_of_record" => finish(&mut cur, &mut brda, &mut fns, &mut fnf, &mut fnh),
            _ => {
                let Some((_, fc)) = cur.as_mut() else { continue };
                let parts: Vec<&str> = val.split(',').collect();
                match tag {
                    "DA" => {
                        if let (Some(Ok(line)), Some(hits)) =
                            (parts.first().map(|s| s.trim().parse::<u32>()), parts.get(1))
                            && line > 0
                        {
                            // Counts can be written as floats by some tools ("1.0e3").
                            let h = hits
                                .trim()
                                .parse::<u64>()
                                .ok()
                                .or_else(|| hits.trim().parse::<f64>().ok().map(|f| f.max(0.0) as u64));
                            if let Some(h) = h {
                                *fc.lines.entry(line).or_insert(0) += h;
                            }
                        }
                    }
                    "BRDA" if parts.len() >= 4 => {
                        if let Ok(line) = parts[0].trim().parse::<u32>() {
                            let taken = parts[3].trim();
                            let hit = taken != "-" && taken.parse::<f64>().is_ok_and(|t| t > 0.0);
                            let e = brda
                                .entry((line, parts[1].trim().to_string(), parts[2].trim().to_string()))
                                .or_insert(false);
                            *e |= hit;
                        }
                    }
                    "FN" => {
                        // FN:<line>[,<end line>],<name>
                        if let Some(name) = parts.last().filter(|_| parts.len() >= 2) {
                            fns.entry(name.trim().to_string()).or_insert(0);
                        }
                    }
                    "FNDA" if parts.len() >= 2 => {
                        let h = parts[0].trim().parse::<u64>().unwrap_or(0);
                        *fns.entry(parts[1..].join(",").trim().to_string()).or_insert(0) += h;
                    }
                    "FNF" => fnf = val.trim().parse().ok(),
                    "FNH" => fnh = val.trim().parse().ok(),
                    "LF" => fc.lf = val.trim().parse().unwrap_or(0),
                    "LH" => fc.lh = val.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
    }
    finish(&mut cur, &mut brda, &mut fns, &mut fnf, &mut fnh);
    out
}

/// Attribute `name="value"` pairs of an XML start tag's inside.
fn attrs(tag: &str) -> HashMap<&str, &str> {
    let mut m = HashMap::new();
    let mut rest = tag;
    while let Some(eq) = rest.find("=\"") {
        let name = rest[..eq].rsplit(|c: char| c.is_whitespace()).next().unwrap_or("").trim();
        let after = &rest[eq + 2..];
        let Some(end) = after.find('"') else { break };
        m.insert(name, &after[..end]);
        rest = &after[end + 1..];
    }
    m
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Parse a Cobertura XML report: `(filename, coverage)` per class file, plus the `<source>`
/// directories its file names are relative to.
pub fn parse_cobertura(text: &str) -> (Vec<(String, FileCov)>, Vec<String>) {
    let mut sources = Vec::new();
    let mut rest = text;
    while let Some(a) = rest.find("<source>") {
        let after = &rest[a + 8..];
        let Some(b) = after.find("</source>") else { break };
        sources.push(xml_unescape(after[..b].trim()));
        rest = &after[b..];
    }
    let mut out: Vec<(String, FileCov)> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut cur: Option<usize> = None;
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else { break };
        let tag = &after[..gt];
        rest = &after[gt + 1..];
        let name = tag.split(|c: char| c.is_whitespace()).next().unwrap_or("").trim_end_matches('/');
        match name {
            "class" => {
                let a = attrs(tag);
                let Some(f) = a.get("filename") else { continue };
                let f = xml_unescape(f);
                let i = *idx.entry(f.clone()).or_insert_with(|| {
                    out.push((f, FileCov::default()));
                    out.len() - 1
                });
                cur = Some(i);
            }
            "/class" => cur = None,
            "line" => {
                let Some(i) = cur else { continue };
                let a = attrs(tag);
                let (Some(Ok(n)), Some(h)) = (a.get("number").map(|s| s.parse::<u32>()), a.get("hits")) else {
                    continue;
                };
                let h = h.parse::<u64>().ok().or_else(|| h.parse::<f64>().ok().map(|f| f.max(0.0) as u64)).unwrap_or(0);
                let fc = &mut out[i].1;
                // Method lines repeat the class lines: keep the larger count, do not add.
                let e = fc.lines.entry(n).or_insert(0);
                *e = (*e).max(h);
                // condition-coverage="50% (1/2)"
                if a.get("branch").is_some_and(|b| *b == "true")
                    && let Some(cc) = a.get("condition-coverage")
                    && let Some((t, total)) = cc.split('(').nth(1).and_then(|s| s.trim_end_matches(')').split_once('/'))
                    && let (Ok(t), Ok(total)) = (t.trim().parse::<u32>(), total.trim().parse::<u32>())
                {
                    fc.branches.insert(n, (t, total));
                }
            }
            _ => {}
        }
    }
    (out, sources)
}

/// A path without `.` and `..` components (no file system access).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Where a report's file name points: absolute names as they are, relative ones against the
/// first base directory where the file exists (else the first base).
pub fn resolve(name: &str, bases: &[PathBuf]) -> PathBuf {
    let p = Path::new(name);
    if p.is_absolute() {
        return normalize(p);
    }
    for b in bases {
        let full = normalize(&b.join(p));
        if full.exists() {
            return full;
        }
    }
    normalize(&bases.first().map_or_else(|| p.to_path_buf(), |b| b.join(p)))
}

/// A coverage report, by absolute file path.
#[derive(Debug, Default)]
pub struct Report {
    pub files: Vec<(PathBuf, FileCov)>,
    idx: HashMap<PathBuf, usize>,
    /// Where it was read from (shown to the user).
    pub source: PathBuf,
}

impl Report {
    /// Build from a report's text (LCOV, or Cobertura when it looks like XML). Relative file
    /// names resolve against `bases` (the folder the tests ran in, then the report's folders).
    pub fn parse(text: &str, bases: &[PathBuf], source: PathBuf) -> Report {
        let (files, sources) =
            if text.trim_start().starts_with('<') { parse_cobertura(text) } else { (parse_lcov(text), Vec::new()) };
        let mut all: Vec<PathBuf> = sources.iter().map(PathBuf::from).collect();
        all.extend(bases.iter().cloned());
        let mut r = Report { source, ..Default::default() };
        for (name, fc) in files {
            let path = resolve(&name, &all);
            match r.idx.get(&path) {
                Some(&i) => r.files[i].1.merge(fc),
                None => {
                    let i = r.files.len();
                    r.idx.insert(path.clone(), i);
                    // Documents are opened by their canonical path: index that one too.
                    if let Ok(c) = crate::paths::canonical(&path)
                        && c != path
                    {
                        r.idx.insert(c, i);
                    }
                    r.files.push((path, fc));
                }
            }
        }
        r
    }

    pub fn get(&self, p: &Path) -> Option<&FileCov> {
        self.idx.get(p).or_else(|| self.idx.get(&normalize(p))).map(|&i| &self.files[i].1)
    }

    /// Covered / instrumented lines over every file.
    pub fn total_pct(&self) -> Option<f32> {
        let (h, f) = self.files.iter().fold((0u64, 0u64), |(h, f), (_, c)| {
            let (a, b) = c.line_counts();
            (h + a as u64, f + b as u64)
        });
        (f > 0).then(|| h as f32 * 100.0 / f as f32)
    }
}

/// How a line fared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cov {
    Covered,
    /// Ran, but not every branch on it did.
    Partial,
    Uncovered,
}

/// A coverage mark in a document's gutter: a 0-based line, how it fared, and a hash of its text
/// when the report was applied (a changed line drops its mark).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub line: usize,
    pub cov: Cov,
    pub hash: u64,
}

pub fn line_hash(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.trim_end().hash(&mut h);
    h.finish()
}

/// Gutter marks of a document with `line_count` lines (`text(l)` = line `l` without its break).
pub fn marks_for(fc: &FileCov, line_count: usize, text: &dyn Fn(usize) -> String) -> Vec<Mark> {
    fc.lines
        .iter()
        .filter(|(l, _)| (**l as usize) >= 1 && (**l as usize) <= line_count)
        .map(|(&l, &hits)| {
            let line = l as usize - 1;
            let cov = match (hits > 0, fc.branches.get(&l)) {
                (false, _) => Cov::Uncovered,
                (true, Some(&(t, n))) if t < n => Cov::Partial,
                _ => Cov::Covered,
            };
            Mark { line, cov, hash: line_hash(&text(line)) }
        })
        .collect()
}

/// Keep `marks` (sorted by line) in step with edits `recs` (applied in order): marks move with
/// their lines, marks of removed lines go, and marks of lines whose text changed are dropped.
/// `text(l)` reads line `l` of the edited document (None past its end).
pub fn on_edits(marks: &mut Vec<Mark>, recs: &[EditRec], text: &dyn Fn(usize) -> Option<String>) {
    if marks.is_empty() || recs.is_empty() {
        return;
    }
    let (mut lo, mut hi) = (usize::MAX, 0usize);
    for r in recs {
        for op in r.line_ops() {
            match op {
                LineOp::Remove { after, n } => {
                    let first = (after + 1).max(0) as usize;
                    marks.retain(|m| m.line < first || m.line >= first + n);
                    for m in marks.iter_mut().filter(|m| m.line >= first + n) {
                        m.line -= n;
                    }
                }
                LineOp::Insert { after, n } => {
                    let first = (after + 1).max(0) as usize;
                    for m in marks.iter_mut().filter(|m| m.line >= first) {
                        m.line += n;
                    }
                }
                LineOp::Swap { first, a_lines, b_lines } => {
                    for m in marks.iter_mut() {
                        if m.line >= first && m.line < first + a_lines {
                            m.line += b_lines;
                        } else if m.line >= first + a_lines && m.line < first + a_lines + b_lines {
                            m.line -= a_lines;
                        }
                    }
                }
            }
        }
        lo = lo.min(r.line);
        hi = hi.max(r.line + r.ins_lines);
    }
    // Lines the edits may have touched (later edits can push earlier ones down: be generous).
    let hi = hi + recs.iter().map(|r| r.ins_lines).sum::<usize>() + recs.len();
    marks.retain(|m| m.line < lo || m.line > hi || text(m.line).is_some_and(|t| line_hash(&t) == m.hash));
    marks.sort_by_key(|m| m.line);
    marks.dedup_by_key(|m| m.line);
}

#[cfg(test)]
mod tests {
    use super::*;

    const LCOV: &str = "TN:\nSF:src/a.ts\nFN:1,add\nFN:5,unused\nFNDA:3,add\nFNDA:0,unused\nFNF:2\nFNH:1\nDA:1,3\nDA:2,3\nDA:5,0\nDA:6,0\nBRDA:2,0,0,3\nBRDA:2,0,1,-\nBRF:2\nBRH:1\nLF:4\nLH:2\nend_of_record\nTN:\nSF:/abs/b.ts\nDA:10,1,abcdef\nLF:1\nLH:1\nend_of_record\n";

    #[test]
    fn lcov_lines_branches_and_functions() {
        let files = parse_lcov(LCOV);
        assert_eq!(files.len(), 2);
        let (name, a) = &files[0];
        assert_eq!(name, "src/a.ts");
        assert_eq!(a.lines.get(&1), Some(&3));
        assert_eq!(a.lines.get(&5), Some(&0));
        assert_eq!(a.line_counts(), (2, 4));
        assert_eq!(a.pct(), Some(50.0));
        assert_eq!(a.branches.get(&2), Some(&(1, 2)), "one of two branches on line 2 ran");
        assert_eq!(a.branch_counts(), (1, 2));
        assert_eq!((a.fn_found, a.fn_hit), (2, 1));
        assert_eq!(a.describe(), "50% of lines (2/4) · branches 1/2 · functions 1/2");
        let (name, b) = &files[1];
        assert_eq!(name, "/abs/b.ts");
        assert_eq!(b.lines.get(&10), Some(&1), "the optional checksum is ignored");
        assert_eq!(b.pct(), Some(100.0));
    }

    #[test]
    fn lcov_merges_repeated_files_tolerates_crlf_and_uses_lf_lh_without_da() {
        let t = "SF:x.rs\r\nDA:1,0\r\nDA:2,1\r\nend_of_record\r\nSF:x.rs\r\nDA:1,2\r\nend_of_record\r\nSF:y.rs\nFNF:3\nFNH:2\nLF:10\nLH:7\nend_of_record\n";
        let files = parse_lcov(t);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].1.lines.get(&1), Some(&2), "counts of the same file add up");
        assert_eq!(files[0].1.pct(), Some(100.0));
        assert_eq!(files[1].1.line_counts(), (7, 10));
        assert_eq!((files[1].1.fn_found, files[1].1.fn_hit), (3, 2));
        // A record without end_of_record at the end of the file still counts; junk is skipped.
        let files = parse_lcov("garbage\nSF:z.js\nDA:x,1\nDA:3,1\n");
        assert_eq!(files[0].1.lines.len(), 1);
    }

    #[test]
    fn cobertura_classes_lines_and_conditions() {
        let xml = r#"<?xml version="1.0" ?>
<coverage line-rate="0.5"><sources><source>/proj/src</source></sources>
<packages><package name="p"><classes>
<class name="a" filename="pkg/a.py" line-rate="0.5"><methods><method name="f"><lines><line number="1" hits="2"/></lines></method></methods>
<lines><line number="1" hits="2"/><line number="2" hits="0"/><line number="3" hits="1" branch="true" condition-coverage="50% (1/2)"/></lines></class>
<class name="b" filename="pkg/b&amp;c.py"><lines><line number="7" hits="1"/></lines></class>
</classes></package></packages></coverage>"#;
        let (files, sources) = parse_cobertura(xml);
        assert_eq!(sources, vec!["/proj/src".to_string()]);
        assert_eq!(files.len(), 2);
        let a = &files[0].1;
        assert_eq!(a.lines.get(&1), Some(&2), "method lines do not double the count");
        assert_eq!(a.line_counts(), (2, 3));
        assert_eq!(a.branches.get(&3), Some(&(1, 2)));
        assert_eq!(files[1].0, "pkg/b&c.py");
        // Through `Report`: names resolve against the <source> directory.
        let r = Report::parse(xml, &[PathBuf::from("/elsewhere")], PathBuf::from("cov.xml"));
        assert!(r.get(Path::new("/proj/src/pkg/a.py")).is_some());
    }

    #[test]
    fn relative_and_absolute_paths_resolve() {
        let d = std::env::temp_dir().join(format!("lumen-cov-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("pkg/src")).unwrap();
        std::fs::write(d.join("pkg/src/a.ts"), "x\n").unwrap();
        // The run happened in `pkg`, the report sits in pkg/coverage: the first base where the
        // file exists wins.
        let bases = vec![d.clone(), d.join("pkg")];
        assert_eq!(resolve("src/a.ts", &bases), d.join("pkg/src/a.ts"));
        assert_eq!(resolve("./pkg/src/../src/a.ts", &bases), d.join("pkg/src/a.ts"));
        assert_eq!(resolve("/abs/x/../y.ts", &bases), PathBuf::from("/abs/y.ts"));
        assert_eq!(resolve("missing.ts", &bases), d.join("missing.ts"), "unknown files land on the first base");
        let text = format!(
            "SF:src/a.ts\nDA:1,1\nend_of_record\nSF:{}\nDA:1,0\nend_of_record\n",
            d.join("pkg/src/a.ts").display()
        );
        let r = Report::parse(&text, &bases, d.join("lcov.info"));
        assert_eq!(r.files.len(), 1, "a relative and an absolute name of the same file are one entry");
        let fc = r.get(&d.join("pkg/src/a.ts")).unwrap();
        assert_eq!(fc.lines.get(&1), Some(&1));
        assert_eq!(r.total_pct(), Some(100.0));
        std::fs::remove_dir_all(&d).ok();
    }

    fn doc_marks(text: &str, fc: &FileCov) -> (crate::buffer::Buffer, Vec<Mark>) {
        let b = crate::buffer::Buffer::from_str(text);
        let m = marks_for(fc, b.line_count(), &|l| b.line(l));
        (b, m)
    }

    fn lines_of(m: &[Mark]) -> Vec<(usize, Cov)> {
        m.iter().map(|m| (m.line, m.cov)).collect()
    }

    #[test]
    fn marks_classify_lines_and_skip_lines_past_the_end() {
        let files = parse_lcov("SF:a\nDA:1,1\nDA:2,1\nDA:3,0\nDA:99,1\nBRDA:2,0,0,1\nBRDA:2,0,1,0\nend_of_record\n");
        let (_, m) = doc_marks("a\nb\nc\n", &files[0].1);
        assert_eq!(lines_of(&m), vec![(0, Cov::Covered), (1, Cov::Partial), (2, Cov::Uncovered)]);
    }

    /// Apply `edit` (char range, replacement) to the buffer and update the marks.
    fn edit(b: &mut crate::buffer::Buffer, m: &mut Vec<Mark>, at: usize, del: usize, ins: &str) {
        let _ = b.take_edits();
        b.replace(at, at + del, ins, crate::buffer::EditKind::Other, crate::buffer::Sel::caret(at));
        let recs = b.take_edits();
        on_edits(m, &recs, &|l| (l < b.line_count()).then(|| b.line(l)));
    }

    #[test]
    fn marks_follow_inserted_and_removed_lines_and_drop_changed_ones() {
        let files = parse_lcov("SF:a\nDA:1,1\nDA:2,0\nDA:3,1\nDA:4,1\nend_of_record\n");
        let (mut b, mut m) = doc_marks("one\ntwo\nthree\nfour\n", &files[0].1);
        assert_eq!(m.len(), 4);
        // A new line at the top pushes every mark down.
        edit(&mut b, &mut m, 0, 0, "zero\n");
        assert_eq!(m.iter().map(|m| m.line).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        // Typing inside "three" (line 3 now): its coverage is unknown, the mark goes.
        let p = b.pos(3, 2);
        edit(&mut b, &mut m, p, 0, "X");
        assert_eq!(m.iter().map(|m| m.line).collect::<Vec<_>>(), vec![1, 2, 4]);
        // Deleting the line "two" takes its mark and moves "four" up.
        let (s, e) = (b.pos(2, 0), b.pos(3, 0));
        edit(&mut b, &mut m, s, e - s, "");
        assert_eq!(lines_of(&m), vec![(1, Cov::Covered), (3, Cov::Covered)]);
        assert_eq!(b.line(3), "four");
        // Splitting a marked line in two with Enter mid-line changes it: dropped.
        let p = b.pos(1, 1);
        edit(&mut b, &mut m, p, 0, "\n");
        assert_eq!(m.iter().map(|m| m.line).collect::<Vec<_>>(), vec![4]);
        assert_eq!(b.line(4), "four");
    }
}
