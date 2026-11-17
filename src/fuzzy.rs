//! Fuzzy subsequence matching with fzf-like scoring (word starts, consecutive runs,
//! basename preference). Returns matched char positions for highlighting.

use std::cell::RefCell;

fn is_boundary(prev: char, cur: char) -> bool {
    matches!(prev, '/' | '.' | '_' | '-' | ' ' | '\\') || (prev.is_lowercase() && cur.is_uppercase())
}

/// A query prepared once for many texts: its lowercase form is built here, not per text.
pub struct Query {
    /// The query as typed (the exact-case bonus compares against it).
    orig: Vec<char>,
    lower: Vec<char>,
    /// `lower` as bytes when all of it is ASCII (texts that are ASCII are matched on their bytes).
    ascii: Option<Vec<u8>>,
    /// Chars of a non-ASCII text and their lowercase, reused between texts.
    scratch: RefCell<(Vec<char>, Vec<char>)>,
}

impl Query {
    pub fn new(query: &str) -> Query {
        let lower: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        let ascii = lower.iter().all(char::is_ascii).then(|| lower.iter().map(|c| *c as u8).collect());
        Query { orig: query.chars().collect(), lower, ascii, scratch: RefCell::default() }
    }

    /// The score of `text`, `None` when the query is not a subsequence of it. Allocates nothing
    /// for ASCII text.
    pub fn score(&self, text: &str) -> Option<i32> {
        self.run(text, None)
    }

    /// Score and matched char positions of `text` (for highlighting).
    pub fn matched(&self, text: &str) -> Option<(i32, Vec<usize>)> {
        let mut pos = Vec::new();
        let score = self.run(text, Some(&mut pos))?;
        Some((score, pos))
    }

    fn run(&self, text: &str, pos: Option<&mut Vec<usize>>) -> Option<i32> {
        if self.lower.is_empty() {
            return Some(0);
        }
        if text.is_ascii() {
            // A lowercase ASCII char never equals a non-ASCII one: no match without an ASCII query.
            let q = self.ascii.as_ref()?;
            let b = text.as_bytes();
            if b.len() < q.len() {
                return None;
            }
            let base = || b.iter().rposition(|&c| c == b'/').map_or(0, |p| p + 1);
            return run(q, &self.orig, b.len(), base, |i| b[i].to_ascii_lowercase(), |i| b[i] as char, pos);
        }
        let mut scratch = self.scratch.borrow_mut();
        let (t, tl) = &mut *scratch;
        t.clear();
        t.extend(text.chars());
        tl.clear();
        tl.extend(t.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)));
        let base = || t.iter().rposition(|c| *c == '/').map_or(0, |p| p + 1);
        run(&self.lower, &self.orig, t.len(), base, |i| tl[i], |i| t[i], pos)
    }
}

/// The matcher over a text of `n` chars: `low(i)` is char `i` lowercased (in the unit of `q`),
/// `ch(i)` the char itself, `base()` where the basename starts.
fn run<T: Copy + PartialEq>(
    q: &[T],
    orig: &[char],
    n: usize,
    base: impl FnOnce() -> usize,
    low: impl Fn(usize) -> T,
    ch: impl Fn(usize) -> char,
    mut pos: Option<&mut Vec<usize>>,
) -> Option<i32> {
    // Forward pass: find the end of the earliest match (this is also the subsequence check).
    let (mut qi, mut end) = (0, 0);
    for i in 0..n {
        if low(i) == q[qi] {
            qi += 1;
            if qi == q.len() {
                end = i;
                break;
            }
        }
    }
    if qi < q.len() {
        return None;
    }
    if let Some(v) = pos.as_deref_mut() {
        v.clear();
        v.resize(q.len(), 0);
    }
    let base = base();
    // Backward pass: tighten the start so the match is as compact as possible, scoring each
    // matched char as it is placed (`next` is the position of the query char after it).
    let mut score = 0i32;
    let (mut next, mut first) = (usize::MAX, 0);
    let mut i = end + 1;
    while qi > 0 && i > 0 {
        i -= 1;
        if low(i) != q[qi - 1] {
            continue;
        }
        qi -= 1;
        score += 10;
        if i == 0 || is_boundary(ch(i - 1), ch(i)) {
            score += 12;
        }
        if i >= base {
            score += 6;
        }
        if next != usize::MAX {
            let gap = next - i - 1;
            if gap == 0 {
                score += 8;
            } else {
                score -= gap.min(8) as i32;
            }
        }
        if ch(i) == orig.get(qi).copied().unwrap_or('\0') {
            score += 1; // exact-case bonus
        }
        if let Some(v) = pos.as_deref_mut() {
            v[qi] = i;
        }
        next = i;
        first = i;
    }
    score -= (n as i32) / 8;
    if first == base {
        score += 15; // starts the basename
    }
    Some(score)
}

pub fn fuzzy(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    Query::new(query).matched(text)
}

/// The best `keep` of `n` labels for the whitespace-separated terms of `query` (every term must
/// match somewhere in the label), best first, as (label index, matched char positions). A row
/// whose `label` is `None` is not listed.
pub fn rank<'a>(
    query: &str,
    n: usize,
    label: impl Fn(usize) -> Option<&'a str>,
    keep: usize,
) -> Vec<(usize, Vec<usize>)> {
    let terms: Vec<Query> = query.split_whitespace().map(Query::new).collect();
    let mut scored: Vec<(i32, usize)> = Vec::new();
    for i in 0..n {
        let Some(text) = label(i) else { continue };
        if let Some(total) = terms.iter().map(|t| t.score(text)).sum::<Option<i32>>() {
            scored.push((total, i));
        }
    }
    // Highest score first, then list order: a total order, so the unstable sorts are deterministic.
    let best_first = |a: &(i32, usize), b: &(i32, usize)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
    if keep == 0 {
        return Vec::new();
    }
    if scored.len() > keep {
        scored.select_nth_unstable_by(keep - 1, best_first);
        scored.truncate(keep);
    }
    scored.sort_unstable_by(best_first);
    scored
        .into_iter()
        .map(|(_, i)| {
            // Positions only for the rows that are shown.
            let mut pos: Vec<usize> = terms.iter().filter_map(|t| t.matched(label(i)?)).flat_map(|(_, p)| p).collect();
            pos.sort_unstable();
            pos.dedup();
            (i, pos)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(fuzzy("abc", "a-b-c").is_some());
        assert!(fuzzy("abc", "acb").is_none());
        assert_eq!(fuzzy("", "x").unwrap().1.len(), 0);
    }

    #[test]
    fn prefers_basename_and_word_starts() {
        let a = fuzzy("isv", "src/services/issue.service.ts").unwrap().0;
        let b = fuzzy("isv", "src/other/xxisxxvxx.ts").unwrap().0;
        assert!(a > b, "{a} vs {b}");
        let exact = fuzzy("app", "src/app.rs").unwrap().0;
        let loose = fuzzy("app", "src/a/p/p.rs").unwrap().0;
        assert!(exact > loose);
    }

    #[test]
    fn positions_are_tight_and_case_insensitive() {
        let (_, p) = fuzzy("IS", "xxIssue").unwrap();
        assert_eq!(p, vec![2, 3]);
        let (_, p) = fuzzy("ab", "a__ab").unwrap();
        assert_eq!(p, vec![3, 4], "tightened to the compact occurrence");
    }

    /// The matcher as it was before `Query` (three `Vec<char>` per call), kept as the reference.
    fn reference(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
        if query.is_empty() {
            return Some((0, Vec::new()));
        }
        let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        let t: Vec<char> = text.chars().collect();
        let tl: Vec<char> = t.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();

        // Forward pass: find the end of the earliest match.
        let (mut qi, mut end) = (0, 0);
        for (i, c) in tl.iter().enumerate() {
            if *c == q[qi] {
                qi += 1;
                if qi == q.len() {
                    end = i;
                    break;
                }
            }
        }
        if qi < q.len() {
            return None;
        }
        // Backward pass: tighten the start so the match is as compact as possible.
        let mut pos = vec![0; q.len()];
        let mut qi = q.len();
        let mut i = end as isize;
        while qi > 0 && i >= 0 {
            if tl[i as usize] == q[qi - 1] {
                qi -= 1;
                pos[qi] = i as usize;
            }
            i -= 1;
        }

        let base = t.iter().rposition(|c| *c == '/').map_or(0, |p| p + 1);
        let mut score = 0i32;
        for (k, &p) in pos.iter().enumerate() {
            score += 10;
            if p == 0 || is_boundary(t[p - 1], t[p]) {
                score += 12;
            }
            if p >= base {
                score += 6;
            }
            if k > 0 {
                let gap = p - pos[k - 1] - 1;
                if gap == 0 {
                    score += 8;
                } else {
                    score -= gap.min(8) as i32;
                }
            }
            if t[p] == query.chars().nth(k).unwrap_or('\0') {
                score += 1; // exact-case bonus
            }
        }
        score -= (t.len() as i32) / 8;
        if pos[0] >= base && pos[0] == base {
            score += 15; // starts the basename
        }
        Some((score, pos))
    }

    fn corpus() -> Vec<String> {
        let mut out: Vec<String> = [
            "src/app.rs",
            "src/app/mod.rs",
            "src/services/issue.service.ts",
            "src/other/xxisxxvxx.ts",
            "README.md",
            "a/b/c/d/e/f/g/LongFileNameWithCamelCase.tsx",
            "docs/Über uns/straße.md",
            "docs/ÉCOLE/İstanbul.txt",
            "src\\windows\\path.rs",
            "node_modules/.bin/tsc",
            "",
            "/",
            "a",
            "K kelvin \u{212a}",
            "x_y-z.w v/Q",
            "日本語/ファイル.rs",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // A generated tree: many shapes of path with repeated letters.
        let (dirs, names) =
            (["src", "app", "lib", "Tests", "ui_kit"], ["index", "AppShell", "use-thing", "a.b.c", "Zeta"]);
        for (i, d) in dirs.iter().enumerate() {
            for (j, n) in names.iter().enumerate() {
                out.push(format!("{d}/{}/{n}{}.ts", dirs[(i + j) % dirs.len()], i * j));
            }
        }
        out
    }

    #[test]
    fn query_agrees_with_the_reference_matcher() {
        let queries = [
            "a", "A", "app", "APP", "isv", "src", "s/a", "tsx", "ts", "lfn", "LFN", "é", "É", "ü", "über", "ß", "İ",
            "i\u{307}", "\u{212a}", "k", "zz", "x y", "/", ".", "_", "-", "\\", "日本", "ファ", "apps", "indexts",
            "at0", "aaaa", "mod.rs", "Rs", "e", "ee", "c.d",
        ];
        let corpus = corpus();
        for q in queries {
            let prepared = Query::new(q);
            for t in &corpus {
                let want = reference(q, t);
                assert_eq!(prepared.matched(t), want, "query {q:?} text {t:?}");
                assert_eq!(fuzzy(q, t), want, "query {q:?} text {t:?}");
                assert_eq!(prepared.score(t), want.map(|w| w.0), "score of {q:?} in {t:?}");
            }
        }
    }

    #[test]
    fn rank_keeps_the_best_in_the_order_of_a_full_sort() {
        let corpus = corpus();
        for q in ["a", "s", "ts", "app ts", "src index", "zz", "e"] {
            let terms: Vec<&str> = q.split_whitespace().collect();
            let mut all: Vec<(i32, usize, Vec<usize>)> = corpus
                .iter()
                .enumerate()
                .filter_map(|(i, label)| {
                    let (mut total, mut pos) = (0, Vec::new());
                    for t in &terms {
                        let (s, p) = reference(t, label)?;
                        total += s;
                        pos.extend(p);
                    }
                    pos.sort_unstable();
                    pos.dedup();
                    Some((total, i, pos))
                })
                .collect();
            all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            for keep in [0, 1, 3, 7, 1000] {
                let want: Vec<(usize, Vec<usize>)> = all.iter().take(keep).map(|(_, i, p)| (*i, p.clone())).collect();
                assert_eq!(rank(q, corpus.len(), |i| Some(corpus[i].as_str()), keep), want, "query {q:?} keep {keep}");
            }
        }
    }
}
