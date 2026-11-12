//! Project file index and text search, built on `ignore` (gitignore-aware walker).

use crate::replace::Matcher;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
    },
};

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", ".next", ".turbo", ".cache", ".stryker"];

/// What project search looks for (see `replace::Matcher` for the options).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub case: bool,
    pub regex: bool,
    /// Whole words only.
    pub word: bool,
    /// Replace mode: one hit per match (not per line), each with its replacement preview.
    pub replace: Option<String>,
}

/// Hits a search reports at most (one per line).
pub const MAX_HITS: usize = 2000;
/// Hits a search in replace mode reports at most (one per match).
pub const MAX_REPLACE_HITS: usize = 5000;
/// Files larger than this are skipped (search and replace alike).
pub const MAX_FILE: u64 = 1_000_000;

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize, // char column of the match
    /// The line around the match, trimmed (a "…" marks a cut start in long lines).
    pub text: String,
    /// Byte range of the match in the whole line.
    pub start: usize,
    pub end: usize,
    /// Char range of the match inside `text`.
    pub mark: (usize, usize),
    /// Replace mode: what replaces the match.
    pub rep: Option<String>,
}

/// The shown part of `line` for a match at bytes `start..end`: trimmed, at most 200 chars, starting
/// up to 60 chars before the match; also the match's char range in it.
pub fn snippet(line: &str, start: usize, end: usize) -> (String, (usize, usize)) {
    let lead = line.len() - line.trim_start().len();
    let body = line.trim();
    let body_end = lead + body.len();
    let s = start.clamp(lead, body_end);
    let e = end.clamp(s, body_end);
    let pre = line[lead..s].chars().count();
    let len = line[s..e].chars().count();
    let skip = pre.saturating_sub(60);
    let mut text = String::new();
    if skip > 0 {
        text.push('…');
    }
    text.extend(body.chars().skip(skip).take(200));
    let n = text.chars().count();
    let a = (pre - skip + (skip > 0) as usize).min(n);
    (text, (a, (a + len).min(n)))
}

fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .require_git(false)
        .filter_entry(|e| {
            !(e.file_type().is_some_and(|t| t.is_dir())
                && SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()))
        })
        .build()
}

pub fn list_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    walker(root)
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .take(limit)
        .collect()
}

/// Streams hits through `tx` in batches; stops early when `generation` no longer equals `my_gen`.
/// `overrides` holds the text of files open with unsaved changes (searched instead of the disk).
pub fn search(
    root: &Path,
    q: &Query,
    overrides: &HashMap<PathBuf, String>,
    generation: Arc<AtomicUsize>,
    my_gen: usize,
    tx: Sender<(usize, Vec<Hit>)>,
    wake: impl Fn(),
) {
    let Ok(m) = Matcher::new(&q.text, q.case, q.regex, q.word) else { return };
    let cap = if q.replace.is_some() { MAX_REPLACE_HITS } else { MAX_HITS };
    let mut batch = Vec::new();
    let mut total = 0;
    for entry in walker(root).flatten() {
        if generation.load(Ordering::Relaxed) != my_gen || total >= cap {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let owned;
        let text: &str = match overrides.get(entry.path()) {
            Some(t) if t.len() as u64 <= MAX_FILE => t,
            Some(_) => continue,
            None => {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.len() > MAX_FILE {
                    continue;
                }
                let Ok(bytes) = std::fs::read(entry.path()) else { continue };
                if bytes[..bytes.len().min(8000)].contains(&0) {
                    continue;
                }
                owned = String::from_utf8_lossy(&bytes).into_owned();
                &owned
            }
        };
        // Same lines as `replace::replace_text` (a BOM is not part of the first line).
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        'lines: for (ln, line) in text.lines().enumerate() {
            let found: Vec<(usize, usize, Option<String>)> = match &q.replace {
                Some(rep) => m.matches(line, rep).into_iter().map(|x| (x.start, x.end, Some(x.rep))).collect(),
                None => m.first(line).map(|(s, e)| (s, e, None)).into_iter().collect(),
            };
            for (start, end, rep) in found {
                let (text, mark) = snippet(line, start, end);
                batch.push(Hit {
                    path: entry.path().to_path_buf(),
                    line: ln,
                    col: line[..start].chars().count(),
                    text,
                    start,
                    end,
                    mark,
                    rep,
                });
                total += 1;
                if total >= cap {
                    break 'lines;
                }
            }
        }
        if batch.len() >= 40 {
            if tx.send((my_gen, std::mem::take(&mut batch))).is_err() {
                return;
            }
            wake();
        }
    }
    if !batch.is_empty() {
        let _ = tx.send((my_gen, batch));
    }
    wake();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn project() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("lumen-search-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(root.join("src/a.ts"), "const needle = 1\nfoo\n  Needle()\n").unwrap();
        std::fs::write(root.join("node_modules/x/n.ts"), "needle").unwrap();
        std::fs::write(root.join("ignored/i.ts"), "needle").unwrap();
        std::fs::write(root.join("bin.dat"), [0u8, 1, 2, b'n']).unwrap();
        root
    }

    #[test]
    fn file_index_respects_ignores() {
        let root = project();
        let files = list_files(&root, 100);
        let names: Vec<String> =
            files.iter().map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert!(names.contains(&"src/a.ts".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("node_modules") || n.starts_with("ignored")), "{names:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_finds_hits_case_insensitively_and_skips_junk() {
        let root = project();
        let (tx, rx) = channel();
        let generation = Arc::new(AtomicUsize::new(1));
        search(&root, &q("needle"), &HashMap::new(), generation, 1, tx, || {});
        let hits: Vec<Hit> = rx.try_iter().flat_map(|(_, h)| h).collect();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!((hits[0].line, hits[0].col), (0, 6));
        assert_eq!((hits[1].line, hits[1].col), (2, 2));
        assert_eq!((hits[1].text.as_str(), hits[1].mark), ("Needle()", (0, 6)));
        std::fs::remove_dir_all(&root).ok();
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Default::default() }
    }

    fn run(root: &Path, q: &Query, ov: &HashMap<PathBuf, String>) -> Vec<Hit> {
        let (tx, rx) = channel();
        search(root, q, ov, Arc::new(AtomicUsize::new(1)), 1, tx, || {});
        rx.try_iter().flat_map(|(_, h)| h).collect()
    }

    #[test]
    fn whole_words_replace_mode_and_unsaved_text() {
        let root = project();
        std::fs::write(root.join("src/b.ts"), "needles needle needle\n").unwrap();
        let b = root.join("src/b.ts");
        let only_b = |hits: Vec<Hit>| hits.into_iter().filter(|h| h.path == b).collect::<Vec<_>>();
        // Plain search: one hit per line; whole word skips "needles".
        let hits = only_b(run(&root, &q("needle"), &HashMap::new()));
        assert_eq!(hits.iter().map(|h| h.col).collect::<Vec<_>>(), vec![0]);
        let hits = only_b(run(&root, &Query { word: true, ..q("needle") }, &HashMap::new()));
        assert_eq!(hits.iter().map(|h| h.col).collect::<Vec<_>>(), vec![8]);
        // Replace mode: one hit per match, with the expanded replacement.
        let rq = Query { regex: true, replace: Some("<$1>".into()), ..q("need(le)") };
        let hits = only_b(run(&root, &rq, &HashMap::new()));
        assert_eq!(hits.iter().map(|h| (h.start, h.end)).collect::<Vec<_>>(), vec![(0, 6), (8, 14), (15, 21)]);
        assert!(hits.iter().all(|h| h.rep.as_deref() == Some("<le>")), "{hits:?}");
        // Unsaved text of an open file is searched instead of the disk.
        let ov = HashMap::from([(b.clone(), "x\nneedle\n".to_string())]);
        let hits = only_b(run(&root, &q("needle"), &ov));
        assert_eq!(hits.iter().map(|h| h.line).collect::<Vec<_>>(), vec![1]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn snippets_keep_the_match_in_view() {
        assert_eq!(snippet("    let a = b;  ", 8, 9), ("let a = b;".to_string(), (4, 5)));
        let long = format!("{}MATCH tail", "x".repeat(100));
        let (t, (a, b)) = snippet(&long, 100, 105);
        assert!(t.starts_with('…'), "{t}");
        assert_eq!(t.chars().skip(a).take(b - a).collect::<String>(), "MATCH");
    }

    #[test]
    fn stale_generation_aborts() {
        let root = project();
        let (tx, rx) = channel();
        let generation = Arc::new(AtomicUsize::new(2));
        search(&root, &q("needle"), &HashMap::new(), generation, 1, tx, || {});
        assert_eq!(rx.try_iter().count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }
}
