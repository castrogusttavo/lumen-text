//! Hot exit and crash recovery: copies of unsaved buffers on disk.
//!
//! Every dirty buffer (untitled ones too) is written to
//! `$XDG_STATE_HOME/lumen-text/backups/<project key>/` a moment after the last edit (on a worker
//! thread: the UI thread only takes an O(1) rope snapshot) and, with `hot_exit` on, once more when
//! the app quits. The next start of the same project reopens them as unsaved tabs. A backup goes
//! away as soon as its buffer is saved, closed without saving, or no longer differs from disk.
//!
//! One file per buffer: a JSON header line ([`Meta`]) followed by the raw text. Files are written
//! to a temporary name and renamed, so a crash mid-write never leaves half a backup.
//! `clean-exit` marks a backup set written by a normal quit (otherwise the app died).

use ropey::Rope;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Quiet time after the last edit before the buffers are written.
pub const DELAY: Duration = Duration::from_secs(2);
/// Longest a dirty buffer goes without a backup while the user keeps typing.
pub const MAX_WAIT: Duration = Duration::from_secs(10);

const EXT: &str = "bak";
const CLEAN_EXIT: &str = "clean-exit";

/// What a backup holds besides the text.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    /// File the buffer belongs to (`None`: untitled).
    pub path: Option<PathBuf>,
    /// Tab name (untitled buffers keep theirs).
    pub name: String,
    /// Modification time of the file when the buffer was last loaded or saved: if it differs on the
    /// next start, the file changed on disk meanwhile.
    pub disk_mtime: Option<SystemTime>,
    /// The file had already changed on disk under the unsaved edits.
    pub conflict: bool,
    /// Tab position, to restore in the same order.
    pub order: usize,
}

/// `$XDG_STATE_HOME/lumen-text/backups/<key>` for the project at `root` (the key is the hash the
/// session file uses).
pub fn project_dir(root: &Path) -> Option<PathBuf> {
    Some(crate::paths::state_home()?.join("lumen-text").join("backups").join(project_key(root)))
}

pub fn project_key(root: &Path) -> String {
    format!("{:016x}", fnv(root.to_string_lossy().as_bytes()))
}

fn fnv(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf29ce484222325u64, |a, &b| (a ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Backup file for a buffer: named after the path, or a fresh unique name for an untitled one.
pub fn file_name(dir: &Path, path: Option<&Path>) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    match path {
        Some(p) => dir.join(format!("f-{:016x}.{EXT}", fnv(p.to_string_lossy().as_bytes()))),
        None => {
            let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            dir.join(format!("u-{t:x}-{}-{n}.{EXT}", std::process::id()))
        }
    }
}

fn time_json(t: Option<SystemTime>) -> Value {
    match t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => json!([d.as_secs(), d.subsec_nanos()]),
        None => Value::Null,
    }
}

fn time_of(v: &Value) -> Option<SystemTime> {
    let a = v.as_array()?;
    let (s, n) = (a.first()?.as_u64()?, a.get(1)?.as_u64()?);
    Some(UNIX_EPOCH + Duration::new(s, n.min(999_999_999) as u32))
}

/// Write `text` with its header to `file`, atomically.
pub fn write(file: &Path, meta: &Meta, text: &Rope) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(d) = file.parent() {
        fs::create_dir_all(d)?;
    }
    let head = json!({
        "v": 1,
        "path": meta.path.as_ref().map(|p| p.to_string_lossy()),
        "name": meta.name,
        "disk_mtime": time_json(meta.disk_mtime),
        "conflict": meta.conflict,
        "order": meta.order,
    });
    // Unique temporary name: the quit path and a worker may write the same backup.
    let tmp = file.with_extension(format!("tmp{}", SEQ.fetch_add(1, Ordering::Relaxed)));
    let res = write_to(&tmp, &head, text).and_then(|()| fs::rename(&tmp, file));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn write_to(tmp: &Path, head: &Value, text: &Rope) -> io::Result<()> {
    let mut w = BufWriter::new(fs::File::create(tmp)?);
    writeln!(w, "{head}")?;
    text.write_to(&mut w)?;
    w.flush()
}

/// Parse one backup file.
pub fn read(file: &Path) -> Option<(Meta, String)> {
    let data = fs::read_to_string(file).ok()?;
    let (head, text) = data.split_once('\n')?;
    let v: Value = serde_json::from_str(head).ok()?;
    if v["v"].as_u64() != Some(1) {
        return None;
    }
    let meta = Meta {
        path: v["path"].as_str().map(PathBuf::from),
        name: v["name"].as_str().unwrap_or("untitled").to_string(),
        disk_mtime: time_of(&v["disk_mtime"]),
        conflict: v["conflict"].as_bool().unwrap_or(false),
        order: v["order"].as_u64().unwrap_or(u64::MAX) as usize,
    };
    Some((meta, text.to_string()))
}

/// Every backup in `dir`, in tab order. Unreadable files are removed.
pub fn read_all(dir: &Path) -> Vec<(PathBuf, Meta, String)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut v = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        match p.extension().and_then(|x| x.to_str()) {
            Some(EXT) => match read(&p) {
                Some((m, t)) => v.push((p, m, t)),
                None => {
                    let _ = fs::remove_file(&p);
                }
            },
            // Leftover temporary file of an interrupted write.
            Some(x) if x.starts_with("tmp") => {
                let _ = fs::remove_file(&p);
            }
            _ => {}
        }
    }
    v.sort_by(|a, b| a.1.order.cmp(&b.1.order).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Remove every backup in `dir` not in `keep`.
pub fn sweep(dir: &Path, keep: &[&Path]) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == EXT) && !keep.contains(&p.as_path()) {
            let _ = fs::remove_file(&p);
        }
    }
}

pub fn mark_clean_exit(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join(CLEAN_EXIT), b"")
}

/// Was the last backup set written by a normal quit? Clears the mark.
pub fn take_clean_exit(dir: &Path) -> bool {
    fs::remove_file(dir.join(CLEAN_EXIT)).is_ok()
}

/// A backup on disk that holds a buffer's text as of `version`.
#[derive(Clone, Debug)]
pub struct Entry {
    pub file: PathBuf,
    pub version: u64,
}

/// One buffer for the worker to write.
pub struct Job {
    pub file: PathBuf,
    pub meta: Meta,
    pub text: Rope,
}

/// Backup bookkeeping kept by the app.
#[derive(Default)]
pub struct BackupState {
    /// Buffer id ([`crate::buffer::Buffer::gen_id`]) → its backup.
    pub written: HashMap<u64, Entry>,
    /// Fingerprint of the edits not backed up yet, and when they were first seen / are due.
    pub stamp: u64,
    pub since: Option<Instant>,
    pub due: Option<Instant>,
    /// A worker is writing; deletions wait for it so it can't recreate a deleted backup.
    pub busy: Arc<AtomicBool>,
    pub failed: Arc<AtomicBool>,
    pub deferred: Vec<PathBuf>,
}

impl BackupState {
    /// Delete a backup now, or once the running worker is done.
    pub fn delete(&mut self, file: PathBuf) {
        if self.busy.load(Ordering::Acquire) {
            self.deferred.push(file);
        } else {
            let _ = fs::remove_file(&file);
        }
    }

    /// Wait (bounded) for a running worker, e.g. before the quit writes the final backups.
    pub fn wait_idle(&self, max: Duration) {
        let t0 = Instant::now();
        while self.busy.load(Ordering::Acquire) && t0.elapsed() < max {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Run the deletions that waited for the worker.
    pub fn flush_deferred(&mut self) {
        if !self.busy.load(Ordering::Acquire) {
            for f in self.deferred.drain(..) {
                let _ = fs::remove_file(&f);
            }
        }
    }

    /// Write `jobs` on a worker thread.
    pub fn spawn(&mut self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        let (busy, failed) = (self.busy.clone(), self.failed.clone());
        busy.store(true, Ordering::Release);
        let spawned = std::thread::Builder::new().name("backup".into()).spawn(move || {
            for j in jobs {
                if write(&j.file, &j.meta, &j.text).is_err() {
                    failed.store(true, Ordering::Release);
                }
            }
            busy.store(false, Ordering::Release);
        });
        if spawned.is_err() {
            self.busy.store(false, Ordering::Release);
            self.failed.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-bak-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn backups_roundtrip_in_tab_order_and_drop_junk() {
        let dir = tmp("rt");
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 123);
        let a =
            Meta { path: Some("/p/a.rs".into()), name: "a.rs".into(), disk_mtime: Some(t), conflict: true, order: 1 };
        let u = Meta { path: None, name: "untitled".into(), disk_mtime: None, conflict: false, order: 0 };
        let fa = file_name(&dir, a.path.as_deref());
        let fu = file_name(&dir, None);
        assert_ne!(fu, file_name(&dir, None), "untitled names are unique");
        assert_eq!(fa, file_name(&dir, Some(Path::new("/p/a.rs"))), "a path always maps to the same file");
        write(&fa, &a, &Rope::from_str("fn main() {}\r\nline two\n")).unwrap();
        write(&fu, &u, &Rope::from_str("")).unwrap();
        fs::write(dir.join("junk.bak"), "not a backup").unwrap();
        fs::write(dir.join("f-1.tmp3"), "half").unwrap();
        let all = read_all(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!((&all[0].1, all[0].2.as_str()), (&u, ""));
        assert_eq!((&all[1].1, all[1].2.as_str()), (&a, "fn main() {}\r\nline two\n"), "text kept byte for byte");
        assert!(!dir.join("junk.bak").exists() && !dir.join("f-1.tmp3").exists());
        sweep(&dir, &[fa.as_path()]);
        assert!(fa.exists() && !fu.exists());
        assert!(!take_clean_exit(&dir));
        mark_clean_exit(&dir).unwrap();
        assert!(take_clean_exit(&dir));
        assert!(!take_clean_exit(&dir), "the mark is used once");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_dirs_differ_per_root() {
        assert_ne!(project_key(Path::new("/a")), project_key(Path::new("/b")));
        assert_eq!(project_key(Path::new("/a")).len(), 16);
    }

    #[test]
    fn deletions_wait_for_a_running_worker() {
        let dir = tmp("defer");
        let f = dir.join("f-x.bak");
        fs::write(&f, "x").unwrap();
        let mut st = BackupState::default();
        st.busy.store(true, Ordering::Release);
        st.delete(f.clone());
        assert!(f.exists(), "kept while the worker runs");
        st.flush_deferred();
        assert!(f.exists());
        st.busy.store(false, Ordering::Release);
        st.flush_deferred();
        assert!(!f.exists());
        // The worker writes the jobs and clears `busy`.
        let meta = Meta { path: None, name: "untitled".into(), disk_mtime: None, conflict: false, order: 0 };
        st.spawn(vec![Job { file: dir.join("u.bak"), meta, text: Rope::from_str("hi") }]);
        st.wait_idle(Duration::from_secs(5));
        assert_eq!(read(&dir.join("u.bak")).unwrap().1, "hi");
        let _ = fs::remove_dir_all(&dir);
    }
}
