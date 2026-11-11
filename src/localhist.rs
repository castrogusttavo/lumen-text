//! Local history: a snapshot of a file every time it is saved (and right before the editor
//! replaces a buffer with what another program wrote), independent of git, like VS Code's
//! Timeline or JetBrains' Local History.
//!
//! Layout under `$XDG_STATE_HOME/lumen-text/history/`: one directory per file, named after a
//! hash of its canonical path, holding `index.json` (the path and one entry per snapshot, oldest
//! first) and one `<unix ms>.snap` file per snapshot with the text exactly as it was.
//!
//! The disk work runs on one background thread ([`record_async`]); the UI thread only clones the
//! rope (O(1)). Limits: files over [`MAX_FILE_BYTES`] are skipped, a snapshot identical to the
//! previous one is not stored, every file keeps at most `max_entries` snapshots, and the whole
//! history stays under a byte budget by dropping the oldest snapshots first (the newest snapshot of
//! every file is always kept).
//!
//! The UI (picker, diff, restore) lives in `app_localhist.rs`.

use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, mpsc},
};

/// Files larger than this are not snapshotted.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Total size of the history before the oldest snapshots are pruned.
pub const TOTAL_BUDGET: u64 = 200 * 1024 * 1024;
pub const DEFAULT_MAX_ENTRIES: usize = 50;
const INDEX: &str = "index.json";

/// Why a snapshot was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The file was saved; the snapshot is what was written.
    Save,
    /// The buffer was about to be replaced by the file's new content on disk (another program,
    /// git checkout / discard, the AI agent): the snapshot is the old buffer.
    Reload,
    /// The buffer was about to be replaced by an older snapshot ("Restore").
    Restore,
}

impl Source {
    fn id(self) -> &'static str {
        match self {
            Source::Save => "save",
            Source::Reload => "before-external-reload",
            Source::Restore => "before-restore",
        }
    }

    fn from_id(s: &str) -> Source {
        match s {
            "before-external-reload" => Source::Reload,
            "before-restore" => Source::Restore,
            _ => Source::Save,
        }
    }

    /// What the picker shows.
    pub fn label(self) -> &'static str {
        match self {
            Source::Save => "Saved",
            Source::Reload => "Before reload from disk",
            Source::Restore => "Before restore",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Unix time in milliseconds (unique per file: the file name is derived from it).
    pub ts: i64,
    pub size: u64,
    pub source: Source,
    /// Snapshot file name inside the file's history directory.
    pub file: String,
    pub hash: u64,
}

impl Entry {
    pub fn secs(&self) -> i64 {
        self.ts.div_euclid(1000)
    }
}

/// `index.json` of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub path: PathBuf,
    /// Oldest first.
    pub entries: Vec<Entry>,
}

impl Index {
    fn to_json(&self) -> Value {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                json!({"ts": e.ts, "size": e.size, "source": e.source.id(), "file": e.file, "hash": format!("{:016x}", e.hash)})
            })
            .collect();
        json!({"version": 1, "path": self.path.to_string_lossy(), "entries": entries})
    }

    fn from_json(v: &Value) -> Option<Index> {
        let path = PathBuf::from(v["path"].as_str()?);
        let entries = v["entries"]
            .as_array()?
            .iter()
            .filter_map(|e| {
                let file = e["file"].as_str()?.to_string();
                if !safe_name(&file) {
                    return None;
                }
                Some(Entry {
                    ts: e["ts"].as_i64()?,
                    size: e["size"].as_u64().unwrap_or(0),
                    source: Source::from_id(e["source"].as_str().unwrap_or("")),
                    file,
                    hash: e["hash"].as_str().and_then(|h| u64::from_str_radix(h, 16).ok()).unwrap_or(0),
                })
            })
            .collect();
        Some(Index { path, entries })
    }
}

/// A snapshot file name we wrote ourselves: never a path that leaves the history directory.
fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s != INDEX
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !s.starts_with('.')
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |a, &b| (a ^ b as u64).wrapping_mul(0x100000001b3))
}

/// The path history is filed under: canonical when it exists; for a deleted file, its
/// canonical parent plus the name.
pub fn canon(path: &Path) -> PathBuf {
    if let Ok(p) = crate::paths::canonical(path) {
        return p;
    }
    match (path.parent().and_then(|d| crate::paths::canonical(d).ok()), path.file_name()) {
        (Some(d), Some(n)) => d.join(n),
        _ => path.to_path_buf(),
    }
}

/// `$XDG_STATE_HOME/lumen-text/history` (platform equivalents elsewhere).
pub fn default_root() -> Option<PathBuf> {
    crate::paths::state_home().map(|s| s.join("lumen-text").join("history"))
}

/// "512 B", "3.4 KB", "1.2 MB".
pub fn size_label(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn read_index(dir: &Path) -> Option<Index> {
    let text = fs::read_to_string(dir.join(INDEX)).ok()?;
    Index::from_json(&serde_json::from_str(&text).ok()?)
}

/// Atomic: a temporary file renamed over the index.
fn write_index(dir: &Path, ix: &Index) -> io::Result<()> {
    let tmp = dir.join(format!("{INDEX}.tmp{}", std::process::id()));
    fs::write(&tmp, ix.to_json().to_string())?;
    fs::rename(&tmp, dir.join(INDEX))
}

#[derive(Clone, Debug)]
pub struct Store {
    pub root: PathBuf,
    pub max_entries: usize,
    pub budget: u64,
}

impl Store {
    pub fn new(root: PathBuf, max_entries: usize) -> Store {
        Store { root, max_entries: max_entries.max(1), budget: TOTAL_BUDGET }
    }

    /// History directory of `canon` (a [`canon`] path). Directory names are a hash of the path;
    /// on the (unlikely) collision with another file the next free suffix is used.
    fn dir_for(&self, canon: &Path) -> PathBuf {
        let base = format!("{:016x}", fnv(canon.to_string_lossy().as_bytes()));
        for n in 0..16 {
            let d = self.root.join(if n == 0 { base.clone() } else { format!("{base}-{n}") });
            match read_index(&d) {
                Some(ix) if ix.path != canon => continue,
                _ => return d,
            }
        }
        self.root.join(base)
    }

    /// Snapshots of `path`, newest first.
    pub fn history(&self, path: &Path) -> Vec<Entry> {
        let c = canon(path);
        let mut v = read_index(&self.dir_for(&c)).filter(|ix| ix.path == c).map(|ix| ix.entries).unwrap_or_default();
        v.reverse();
        v
    }

    /// Text of one snapshot of `path`.
    pub fn read(&self, path: &Path, file: &str) -> io::Result<String> {
        if !safe_name(file) {
            return Err(io::Error::other("invalid snapshot name"));
        }
        fs::read_to_string(self.dir_for(&canon(path)).join(file))
    }

    /// Store `text` as the newest snapshot of `path`, unless it is too big or identical to the
    /// newest one. Applies the per-file cap. Returns how many bytes of snapshots were added
    /// (negative when the cap removed more than was added; 0 when nothing was stored).
    pub fn record(&self, path: &Path, text: &str, source: Source) -> io::Result<i64> {
        let size = text.len() as u64;
        if size > MAX_FILE_BYTES {
            return Ok(0);
        }
        let c = canon(path);
        let dir = self.dir_for(&c);
        let mut ix = read_index(&dir).filter(|ix| ix.path == c).unwrap_or(Index { path: c, entries: Vec::new() });
        let hash = fnv(text.as_bytes());
        if let Some(last) = ix.entries.last()
            && last.hash == hash
            && last.size == size
            && fs::read(dir.join(&last.file)).is_ok_and(|b| b == text.as_bytes())
        {
            return Ok(0);
        }
        fs::create_dir_all(&dir)?;
        // Strictly increasing per file, so names never clash (two saves in the same millisecond,
        // or a clock that went back).
        let mut ts = now_ms().max(ix.entries.last().map_or(i64::MIN, |e| e.ts + 1));
        while dir.join(format!("{ts}.snap")).exists() {
            ts += 1;
        }
        let file = format!("{ts}.snap");
        fs::write(dir.join(&file), text)?;
        ix.entries.push(Entry { ts, size, source, file, hash });
        let mut delta = size as i64;
        let over = ix.entries.len().saturating_sub(self.max_entries);
        for old in ix.entries.drain(..over) {
            let _ = fs::remove_file(dir.join(&old.file));
            delta -= old.size as i64;
        }
        write_index(&dir, &ix)?;
        Ok(delta)
    }

    /// Every file's index (`(directory, index)`), in no particular order.
    fn all(&self) -> Vec<(PathBuf, Index)> {
        let Ok(rd) = fs::read_dir(&self.root) else { return Vec::new() };
        rd.flatten().filter_map(|e| Some((e.path(), read_index(&e.path())?))).collect()
    }

    /// Bytes taken by every snapshot.
    pub fn total_size(&self) -> u64 {
        self.all().iter().flat_map(|(_, ix)| &ix.entries).map(|e| e.size).sum()
    }

    /// Drop the oldest snapshots (across all files) until the history fits the budget; the newest
    /// snapshot of every file stays. Returns the total size afterwards.
    pub fn prune_budget(&self) -> u64 {
        let mut all = self.all();
        let mut total: u64 = all.iter().flat_map(|(_, ix)| &ix.entries).map(|e| e.size).sum();
        if total <= self.budget {
            return total;
        }
        // (ts, file index) of every snapshot but the newest of its file, oldest first.
        let mut cands: Vec<(i64, usize)> = Vec::new();
        for (k, (_, ix)) in all.iter().enumerate() {
            let n = ix.entries.len();
            cands.extend(ix.entries.iter().take(n.saturating_sub(1)).map(|e| (e.ts, k)));
        }
        cands.sort_unstable();
        let mut touched = vec![false; all.len()];
        for (ts, k) in cands {
            if total <= self.budget {
                break;
            }
            let (dir, ix) = &mut all[k];
            if let Some(pos) = ix.entries.iter().position(|e| e.ts == ts) {
                let e = ix.entries.remove(pos);
                let _ = fs::remove_file(dir.join(&e.file));
                total = total.saturating_sub(e.size);
                touched[k] = true;
            }
        }
        for (k, (dir, ix)) in all.iter().enumerate() {
            if touched[k] {
                let _ = write_index(dir, ix);
            }
        }
        total
    }

    /// Files with history whose path no longer exists: `(path, newest snapshot, snapshot count)`,
    /// most recently snapshotted first.
    pub fn deleted(&self) -> Vec<(PathBuf, Entry, usize)> {
        let mut v: Vec<(PathBuf, Entry, usize)> = self
            .all()
            .into_iter()
            .filter(|(_, ix)| !ix.path.exists())
            .filter_map(|(_, ix)| {
                let n = ix.entries.len();
                Some((ix.path, ix.entries.last()?.clone(), n))
            })
            .collect();
        v.sort_by(|a, b| b.1.ts.cmp(&a.1.ts));
        v
    }
}

// ───────────── background writer ─────────────

enum Job {
    Record { store: Store, path: PathBuf, text: ropey::Rope, source: Source },
    Flush(mpsc::Sender<()>),
}

fn sender() -> Option<mpsc::Sender<Job>> {
    static W: OnceLock<Option<Mutex<mpsc::Sender<Job>>>> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new().name("local-history".into()).spawn(move || run(rx)).ok()?;
        Some(Mutex::new(tx))
    })
    .as_ref()
    .and_then(|m| m.lock().ok().map(|s| s.clone()))
}

fn run(rx: mpsc::Receiver<Job>) {
    // Running total per history root, so the budget is checked without rescanning every save.
    let mut totals: HashMap<PathBuf, u64> = HashMap::new();
    for job in rx {
        match job {
            Job::Record { store, path, text, source } => {
                let text = text.to_string();
                let delta = match store.record(&path, &text, source) {
                    Ok(d) if d != 0 => d,
                    _ => continue,
                };
                let total = match totals.get(&store.root) {
                    Some(&t) => (t as i64 + delta).max(0) as u64,
                    None => store.total_size(),
                };
                let total = if total > store.budget { store.prune_budget() } else { total };
                totals.insert(store.root.clone(), total);
            }
            Job::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

/// Snapshot `buf`'s text as `path` in the background (no-op for files over the size limit).
pub fn record_buf(store: &Store, path: &Path, buf: &crate::buffer::Buffer, source: Source) {
    if buf.len_bytes() as u64 > MAX_FILE_BYTES {
        return;
    }
    if let Some(tx) = sender() {
        let _ = tx.send(Job::Record { store: store.clone(), path: path.to_path_buf(), text: buf.snapshot(), source });
    }
}

/// Wait until every snapshot queued so far is on disk.
pub fn flush() {
    let Some(tx) = sender() else { return };
    let (done, wait) = mpsc::channel();
    if tx.send(Job::Flush(done)).is_ok() {
        let _ = wait.recv_timeout(std::time::Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-lh-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        crate::paths::canonical(&d).unwrap()
    }

    #[test]
    fn records_dedupes_and_lists_newest_first() {
        let d = tmp("dedupe");
        let f = d.join("a.txt");
        fs::write(&f, "one").unwrap();
        let s = Store::new(d.join("hist"), 50);
        assert!(s.history(&f).is_empty());
        assert_eq!(s.record(&f, "one", Source::Save).unwrap(), 3);
        assert_eq!(s.record(&f, "one", Source::Save).unwrap(), 0, "identical to the newest: skipped");
        assert_eq!(s.record(&f, "two!", Source::Reload).unwrap(), 4);
        assert_eq!(s.record(&f, "one", Source::Restore).unwrap(), 3, "only the newest counts for dedupe");
        let h = s.history(&f);
        assert_eq!(h.len(), 3);
        assert_eq!(
            h.iter().map(|e| e.source).collect::<Vec<_>>(),
            [Source::Restore, Source::Reload, Source::Save],
            "newest first"
        );
        assert!(h[0].ts > h[1].ts && h[1].ts > h[2].ts);
        assert_eq!(s.read(&f, &h[1].file).unwrap(), "two!");
        assert_eq!(h[1].size, 4);
        assert!(s.read(&f, "../../etc/passwd").is_err(), "no path traversal");
        // A relative or non-canonical spelling of the same file finds the same history.
        let other = d.join(".").join("a.txt");
        assert_eq!(s.history(&other).len(), 3);
        // Too big: skipped.
        let big = "x".repeat(MAX_FILE_BYTES as usize + 1);
        assert_eq!(s.record(&f, &big, Source::Save).unwrap(), 0);
        assert_eq!(s.history(&f).len(), 3);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn index_round_trips() {
        let ix = Index {
            path: "/p/a b.rs".into(),
            entries: vec![
                Entry {
                    ts: 1_700_000_000_123,
                    size: 7,
                    source: Source::Save,
                    file: "1700000000123.snap".into(),
                    hash: 0xdead_beef,
                },
                Entry {
                    ts: 1_700_000_000_200,
                    size: 9,
                    source: Source::Reload,
                    file: "1700000000200.snap".into(),
                    hash: u64::MAX,
                },
                Entry {
                    ts: 1_700_000_000_300,
                    size: 0,
                    source: Source::Restore,
                    file: "1700000000300.snap".into(),
                    hash: 0,
                },
            ],
        };
        let text = ix.to_json().to_string();
        assert_eq!(Index::from_json(&serde_json::from_str(&text).unwrap()), Some(ix));
        // Entries pointing outside the directory are dropped.
        let bad = json!({"path": "/p", "entries": [{"ts": 1, "file": "../x"}, {"ts": 2, "file": "2.snap"}]});
        assert_eq!(Index::from_json(&bad).unwrap().entries.len(), 1);
        assert_eq!(Index::from_json(&json!({"entries": []})), None, "no path");
    }

    #[test]
    fn per_file_cap_drops_the_oldest() {
        let d = tmp("cap");
        let f = d.join("a.txt");
        let s = Store::new(d.join("hist"), 3);
        for i in 0..5 {
            s.record(&f, &format!("v{i}"), Source::Save).unwrap();
        }
        let h = s.history(&f);
        assert_eq!(h.len(), 3);
        assert_eq!(s.read(&f, &h[0].file).unwrap(), "v4");
        assert_eq!(s.read(&f, &h[2].file).unwrap(), "v2");
        let dir = s.dir_for(&canon(&f));
        let snaps = fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name() != INDEX).count();
        assert_eq!(snaps, 3, "dropped snapshot files are deleted");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn budget_prunes_oldest_across_files_and_keeps_each_newest() {
        let d = tmp("budget");
        let (a, b) = (d.join("a.txt"), d.join("b.txt"));
        let mut s = Store::new(d.join("hist"), 50);
        let blob = |c: char| c.to_string().repeat(100);
        s.record(&a, &blob('1'), Source::Save).unwrap(); // oldest
        s.record(&b, &blob('2'), Source::Save).unwrap();
        s.record(&a, &blob('3'), Source::Save).unwrap();
        s.record(&b, &blob('4'), Source::Save).unwrap();
        s.record(&a, &blob('5'), Source::Save).unwrap();
        assert_eq!(s.total_size(), 500);
        s.budget = 300;
        assert_eq!(s.prune_budget(), 300);
        let (ha, hb) = (s.history(&a), s.history(&b));
        assert_eq!(s.read(&a, &ha.last().unwrap().file).unwrap(), blob('3'), "a's oldest went first");
        assert_eq!(hb.len(), 1, "then b's oldest");
        assert_eq!(s.read(&b, &hb[0].file).unwrap(), blob('4'));
        // Below the newest of every file nothing more can go.
        s.budget = 10;
        assert_eq!(s.prune_budget(), 200);
        assert_eq!((s.history(&a).len(), s.history(&b).len()), (1, 1));
        assert_eq!(s.read(&a, &s.history(&a)[0].file).unwrap(), blob('5'));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn deleted_files_are_listed() {
        let d = tmp("deleted");
        let (a, b) = (d.join("a.txt"), d.join("b.txt"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let s = Store::new(d.join("hist"), 50);
        s.record(&a, "a", Source::Save).unwrap();
        s.record(&b, "b", Source::Save).unwrap();
        s.record(&b, "bb", Source::Save).unwrap();
        assert!(s.deleted().is_empty());
        fs::remove_file(&b).unwrap();
        let del = s.deleted();
        assert_eq!(del.len(), 1);
        assert_eq!((del[0].0.clone(), del[0].2), (b.clone(), 2));
        // Still reachable by its old path.
        assert_eq!(s.read(&b, &del[0].1.file).unwrap(), "bb");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn background_writer_and_flush() {
        let d = tmp("async");
        let f = d.join("a.txt");
        let s = Store::new(d.join("hist"), 50);
        let mut buf = crate::buffer::Buffer::from_str("hello\n");
        buf.path = Some(f.clone());
        record_buf(&s, &f, &buf, Source::Save);
        record_buf(&s, &f, &buf, Source::Save);
        flush();
        let h = s.history(&f);
        assert_eq!(h.len(), 1, "the duplicate was skipped");
        assert_eq!(s.read(&f, &h[0].file).unwrap(), "hello\n");
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(2048), "2.0 KB");
        assert_eq!(size_label(3 * 1024 * 1024), "3.0 MB");
        fs::remove_dir_all(&d).ok();
    }
}
