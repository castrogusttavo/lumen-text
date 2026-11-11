//! Persistent undo: the undo tree of a file outlives closing it (and quitting).
//!
//! One file per document under `$XDG_STATE_HOME/lumen-text/undo/`, named after a hash of the
//! canonical path, holding the path itself (checked on load) and the history encoded by
//! [`Buffer::history_bytes`]. A history is adopted only when the text matches the one it was
//! recorded against, so an edit made elsewhere simply makes it unused. Each file is capped
//! ([`MAX_FILE`]) and the folder is kept under [`BUDGET`] by dropping the least recently used ones
//! (a load touches the file); entries unused for [`MAX_AGE_DAYS`] go too.

use crate::buffer::{Buffer, SavedAt};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
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

/// Keep `buf`'s history for `path` (canonical). Nothing to keep removes an old entry.
pub fn save(dir: &Path, path: &Path, buf: &Buffer) -> io::Result<()> {
    let file = file_for(dir, path);
    let Some(hist) = buf.history_bytes(MAX_FILE) else {
        return match fs::remove_file(&file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
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
    prune(dir, BUDGET, Duration::from_secs(MAX_AGE_DAYS * 86_400));
    Ok(())
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
/// most `budget` bytes.
pub fn prune(dir: &Path, budget: u64, max_age: Duration) {
    let Ok(rd) = fs::read_dir(dir) else { return };
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
        prune(&dir, 2000, Duration::from_secs(86_400));
        assert!(!dir.join("a.undo").exists(), "the oldest goes first");
        assert!(dir.join("b.undo").exists() && dir.join("c.undo").exists());
        assert!(dir.join("keep.txt").exists(), "only undo files are touched");
        prune(&dir, 1 << 20, Duration::from_secs(60));
        assert!(!dir.join("b.undo").exists() && !dir.join("c.undo").exists(), "too old");
        let _ = fs::remove_dir_all(&dir);
    }
}
