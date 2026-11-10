//! Fuzzy subsequence matching with fzf-like scoring (word starts, consecutive runs,
//! basename preference). Returns matched char positions for highlighting.

fn is_boundary(prev: char, cur: char) -> bool {
    matches!(prev, '/' | '.' | '_' | '-' | ' ' | '\\') || (prev.is_lowercase() && cur.is_uppercase())
}

pub fn fuzzy(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
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
}
