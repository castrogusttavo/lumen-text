//! Persistent undo: the undo tree of a file outlives closing it (and quitting).
//!
//! One file per document under `$XDG_STATE_HOME/lumen-text/undo/`, named after a hash of the
//! canonical path, holding the path itself (checked on load) and the history encoded by
//! [`Buffer::history_bytes`]. A history is adopted only when the text matches the one it was
//! recorded against, so an edit made elsewhere simply makes it unused. Each file is capped
//! ([`MAX_FILE`]) and the folder is kept under [`BUDGET`] by dropping the least recently used ones
//! (a load touches the file); entries unused for [`MAX_AGE_DAYS`] go too.
//!
//! The folder is read once per session (the first [`save`] measures it) and once more by the
//! pruning at quit ([`finish`]): in between a running total says whether the budget is exceeded,
//! so closing a tab costs one small write and no directory scan.

use crate::buffer::{Buffer, SavedAt};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

/// Largest history kept for one file (older steps are dropped to fit).
pub const MAX_FILE: usize = 2 << 20;
/// Total size of the undo folder.
pub const BUDGET: u64 = 48 << 20;
pub const MAX_AGE_DAYS: u64 = 60;

const MAGIC: &[u8; 8] = b"LTUNDOF1";

/// `$XDG_STATE_HOME/lumen-text/undo`.
pub fn dir() -> Option<PathBuf> {
    Some(crate::paths::state_home()?.join("lumen-text").join("undo"))
}

fn file_for(dir: &Path, path: &Path) -> PathBuf {
    let h =
        path.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    dir.join(format!("{h:016x}.undo"))
}

/// Bytes held by each undo folder written to in this session.
static TOTALS: Mutex<Option<HashMap<PathBuf, u64>>> = Mutex::new(None);

/// Size of the `.undo` files in `dir` (the one scan a session makes before quitting).
fn folder_size(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "undo"))
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// An entry of `dir` went from `old` to `new` bytes (already on disk): the folder's new total.
fn account(dir: &Path, old: u64, new: u64) -> u64 {
    let mut g = TOTALS.lock().unwrap_or_else(|e| e.into_inner());
    let totals = g.get_or_insert_with(HashMap::new);
    let total = match totals.get(dir) {
        Some(t) => (t + new).saturating_sub(old),
        // First write of the session: measure (the change is on disk, so it is counted).
        None => folder_size(dir),
    };
    totals.insert(dir.to_path_buf(), total);
    total
}

fn set_total(dir: &Path, total: u64) {
    let mut g = TOTALS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new).insert(dir.to_path_buf(), total);
}

/// Keep `buf`'s history for `path` (canonical). Nothing to keep removes an old entry. The folder
/// is pruned by [`finish`] at quit; here only when the running total passes the budget.
pub fn save(dir: &Path, path: &Path, buf: &Buffer) -> io::Result<()> {
    save_within(dir, path, buf, BUDGET)
}

fn save_within(dir: &Path, path: &Path, buf: &Buffer, budget: u64) -> io::Result<()> {
    let file = file_for(dir, path);
    let old = fs::metadata(&file).map_or(0, |m| m.len());
    let Some(hist) = buf.history_bytes(MAX_FILE) else {
        return match fs::remove_file(&file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            Err(_) => Ok(()),
            Ok(()) => {
                account(dir, old, 0);
                Ok(())
            }
        };
    };
    fs::create_dir_all(dir)?;
    let p = path.to_string_lossy();
    let tmp = file.with_extension("undo-tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(MAGIC)?;
        f.write_all(&(p.len() as u32).to_le_bytes())?;
        f.write_all(p.as_bytes())?;
        f.write_all(&hist)?;
    }
    fs::rename(&tmp, &file)?;
    let new = (MAGIC.len() + 4 + p.len() + hist.len()) as u64;
    if account(dir, old, new) > budget {
        set_total(dir, prune(dir, budget, Duration::from_secs(MAX_AGE_DAYS * 86_400)));
    }
    Ok(())
}

/// End of the session, after every history was saved: drop what is too old or over the budget,
/// in one pass over the folder.
pub fn finish(dir: &Path) {
    set_total(dir, prune(dir, BUDGET, Duration::from_secs(MAX_AGE_DAYS * 86_400)));
}

/// The encoded history stored for `path`, if any.
fn read(dir: &Path, path: &Path) -> Option<Vec<u8>> {
    let data = fs::read(file_for(dir, path)).ok()?;
    let rest = data.strip_prefix(MAGIC.as_slice())?;
    let n = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let stored = rest.get(4..4 + n)?;
    (stored == path.to_string_lossy().as_bytes()).then(|| rest[4 + n..].to_vec())
}

/// Give `buf` (holding `path`'s text) the history kept for it, when the text still matches.
/// The file is touched so the pruning sees it as recently used.
pub fn load(dir: &Path, path: &Path, buf: &mut Buffer, saved: SavedAt) -> bool {
    let Some(hist) = read(dir, path) else { return false };
    // Hashing the text costs a pass over it: only when there is a candidate history.
    if !buf.adopt_history(&hist, saved) {
        return false;
    }
    if let Ok(f) = fs::File::options().append(true).open(file_for(dir, path)) {
        let _ = f.set_modified(SystemTime::now());
    }
    true
}

/// Drop entries older than `max_age`, then the least recently used ones until the folder holds at
/// most `budget` bytes. Returns the bytes left.
pub fn prune(dir: &Path, budget: u64, max_age: Duration) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "undo") {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        let t = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if now.duration_since(t).is_ok_and(|age| age > max_age) {
            let _ = fs::remove_file(&p);
            continue;
        }
        files.push((t, m.len(), p));
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, len, p) in files {
        if total <= budget {
            break;
        }
        if fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{EditKind, Sel};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-undo-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn history_comes_back_only_for_the_same_text_and_path() {
        let dir = tmp("store");
        let path = dir.join("a.txt");
        let mut b = Buffer::from_str("one");
        b.replace(3, 3, " two", EditKind::Other, Sel::caret(3));
        save(&dir, &path, &b).unwrap();
        let mut c = Buffer::from_str("one two");
        assert!(load(&dir, &path, &mut c, SavedAt::Current));
        c.undo();
        assert_eq!(c.to_string(), "one");
        // The file was edited elsewhere: the history is not used.
        let mut d = Buffer::from_str("one two!");
        assert!(!load(&dir, &path, &mut d, SavedAt::Current));
        assert!(!d.has_history());
        // Another path never gets it.
        let mut e = Buffer::from_str("one two");
        assert!(!load(&dir, &dir.join("b.txt"), &mut e, SavedAt::Current));
        // Saving a buffer without history drops the entry.
        save(&dir, &path, &Buffer::from_str("x")).unwrap();
        assert!(read(&dir, &path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    fn edited(text: &str) -> Buffer {
        let mut b = Buffer::from_str("");
        b.replace(0, 0, text, EditKind::Other, Sel::caret(0));
        b
    }

    fn known_total(dir: &Path) -> Option<u64> {
        TOTALS.lock().unwrap().as_ref().and_then(|m| m.get(dir).copied())
    }

    fn set_age(p: &Path, age: Duration) {
        let f = fs::File::options().append(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn saving_keeps_a_running_total_and_leaves_pruning_to_the_end_of_the_session() {
        let dir = tmp("total");
        // Left by an earlier session and long unused: saving does not go looking for it.
        let stale = dir.join("stale.undo");
        fs::write(&stale, vec![0u8; 700]).unwrap();
        set_age(&stale, Duration::from_secs((MAX_AGE_DAYS + 1) * 86_400));
        assert_eq!(known_total(&dir), None);
        save(&dir, &dir.join("a.txt"), &edited("one")).unwrap();
        assert_eq!(known_total(&dir), Some(folder_size(&dir)), "measured by the first save");
        assert!(stale.exists(), "no pruning on save");
        // Later saves adjust the total without reading the folder: a file that appears behind
        // its back is not counted.
        fs::write(dir.join("unseen.undo"), vec![0u8; 300]).unwrap();
        save(&dir, &dir.join("b.txt"), &edited("two two")).unwrap();
        save(&dir, &dir.join("a.txt"), &edited("one, but longer this time")).unwrap();
        assert_eq!(known_total(&dir), Some(folder_size(&dir) - 300), "new and replaced entries are accounted");
        save(&dir, &dir.join("b.txt"), &Buffer::from_str("x")).unwrap();
        assert_eq!(known_total(&dir), Some(folder_size(&dir) - 300), "a dropped entry too");
        // The end of the session prunes once and takes stock.
        finish(&dir);
        assert!(!stale.exists(), "too old");
        assert!(file_for(&dir, &dir.join("a.txt")).exists());
        assert_eq!(known_total(&dir), Some(folder_size(&dir)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_that_passes_the_budget_prunes_right_away() {
        let dir = tmp("overbudget");
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        save_within(&dir, &a, &edited(&"a".repeat(600)), 1000).unwrap();
        set_age(&file_for(&dir, &a), Duration::from_secs(3600));
        assert!(file_for(&dir, &a).exists());
        save_within(&dir, &b, &edited(&"b".repeat(600)), 1000).unwrap();
        assert!(!file_for(&dir, &a).exists(), "the least recently used entry made room");
        assert!(file_for(&dir, &b).exists());
        assert_eq!(known_total(&dir), Some(folder_size(&dir)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_keeps_the_budget_dropping_least_recently_used_first() {
        let dir = tmp("prune");
        let old = SystemTime::now() - Duration::from_secs(3600);
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            let p = dir.join(format!("{name}.undo"));
            fs::write(&p, vec![0u8; 1000]).unwrap();
            let f = fs::File::options().append(true).open(&p).unwrap();
            f.set_modified(old + Duration::from_secs(i as u64 * 60)).unwrap();
        }
        fs::write(dir.join("keep.txt"), vec![0u8; 5000]).unwrap();
        assert_eq!(prune(&dir, 2000, Duration::from_secs(86_400)), 2000);
        assert!(!dir.join("a.undo").exists(), "the oldest goes first");
        assert!(dir.join("b.undo").exists() && dir.join("c.undo").exists());
        assert!(dir.join("keep.txt").exists(), "only undo files are touched");
        prune(&dir, 1 << 20, Duration::from_secs(60));
        assert!(!dir.join("b.undo").exists() && !dir.join("c.undo").exists(), "too old");
        let _ = fs::remove_dir_all(&dir);
    }
}
