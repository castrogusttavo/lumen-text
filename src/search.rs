//! Project file index and text search, built on `ignore` (gitignore-aware walker).

use regex::RegexBuilder;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
    },
};

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", ".next", ".turbo", ".cache", ".stryker"];

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize, // char column of the match
    pub text: String,
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
pub fn search(
    root: &Path,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    generation: Arc<AtomicUsize>,
    my_gen: usize,
    tx: Sender<(usize, Vec<Hit>)>,
    wake: impl Fn(),
) {
    if query.is_empty() {
        return;
    }
    let pat = if is_regex { query.to_string() } else { regex::escape(query) };
    let Ok(re) = RegexBuilder::new(&pat).case_insensitive(!case_sensitive).build() else { return };
    let mut batch = Vec::new();
    let mut total = 0;
    for entry in walker(root).flatten() {
        if generation.load(Ordering::Relaxed) != my_gen || total >= 2000 {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > 1_000_000 {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        if bytes[..bytes.len().min(8000)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (ln, line) in text.lines().enumerate() {
            if let Some(m) = re.find(line) {
                let col = line[..m.start()].chars().count();
                batch.push(Hit {
                    path: entry.path().to_path_buf(),
                    line: ln,
                    col,
                    text: line.trim().chars().take(200).collect(),
                });
                total += 1;
                if total >= 2000 {
                    break;
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
        search(&root, "needle", false, false, generation, 1, tx, || {});
        let hits: Vec<Hit> = rx.try_iter().flat_map(|(_, h)| h).collect();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!((hits[0].line, hits[0].col), (0, 6));
        assert_eq!((hits[1].line, hits[1].col), (2, 2));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stale_generation_aborts() {
        let root = project();
        let (tx, rx) = channel();
        let generation = Arc::new(AtomicUsize::new(2));
        search(&root, "needle", false, false, generation, 1, tx, || {});
        assert_eq!(rx.try_iter().count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }
}
