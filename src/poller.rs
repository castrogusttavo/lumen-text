//! Cheap polling for the folders inotify cannot be trusted on (see [`crate::fstype`]).
//!
//! Only what the user can see is polled: the open files and the folders expanded in the explorer
//! (never the whole tree). Every [`Poller::every`] a worker thread lists each folder (name, size,
//! mtime, inode of every entry) and stats each file, compares with the previous pass and sends the
//! differences as ordinary [`FsEvent`]s, so the consumers of the inotify watcher handle them the
//! same way. A pass that finds nothing changed sends nothing and does not wake the UI.
//!
//! The file system is read without holding the lock, so updating the targets from the UI thread
//! never waits for a slow network mount.

use crate::lsp::Wake;
use crate::watcher::{FsEvent, FsKind};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

/// Default interval between two passes (`watcher_poll_ms`).
pub const DEFAULT_MS: u64 = 2000;
/// Shortest interval accepted from the config (tests use their own, shorter, ones).
pub const MIN_MS: u64 = 250;
/// Entries read per folder per pass: a huge folder is only partly compared.
const MAX_ENTRIES: usize = 5000;

/// What a pass remembers of one file: size, modification time, inode (an atomic save that keeps
/// the size and lands in the same second on a coarse file system still changes the inode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    dir: bool,
    len: u64,
    mtime: Option<SystemTime>,
    ino: u64,
}

fn stamp_of(m: &std::fs::Metadata) -> Stamp {
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(m);
    #[cfg(not(unix))]
    let ino = 0;
    Stamp { dir: m.is_dir(), len: m.len(), mtime: m.modified().ok(), ino }
}

type Snap = HashMap<OsString, Stamp>;

#[derive(Clone, Debug, Default)]
enum DirState {
    /// Added since the last pass: the next pass only records it.
    #[default]
    New,
    Missing,
    Seen(Snap),
}

#[derive(Clone, Debug, Default)]
enum FileState {
    #[default]
    New,
    Seen(Option<Stamp>),
}

/// The folders and files being polled and what the last pass saw of them.
#[derive(Default)]
pub struct PollSet {
    dirs: HashMap<PathBuf, DirState>,
    files: HashMap<PathBuf, FileState>,
}

impl PollSet {
    /// Replace the targets. Targets kept from before keep their state (no spurious events); new ones
    /// are recorded by the next pass. A file inside a polled folder is covered by the folder.
    pub fn set(&mut self, dirs: &[PathBuf], files: &[PathBuf]) {
        let mut nd: HashMap<PathBuf, DirState> = HashMap::new();
        for d in dirs {
            let st = self.dirs.remove(d).unwrap_or_default();
            nd.insert(d.clone(), st);
        }
        let mut nf: HashMap<PathBuf, FileState> = HashMap::new();
        for f in files {
            if f.parent().is_some_and(|p| nd.contains_key(p)) {
                continue;
            }
            let st = self.files.remove(f).unwrap_or_default();
            nf.insert(f.clone(), st);
        }
        self.dirs = nd;
        self.files = nf;
    }

    #[allow(dead_code)] // tests, diagnostics
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.files.is_empty()
    }

    #[allow(dead_code)] // tests, diagnostics
    pub fn len(&self) -> usize {
        self.dirs.len() + self.files.len()
    }

    #[allow(dead_code)] // tests, diagnostics
    pub fn has_dir(&self, d: &Path) -> bool {
        self.dirs.contains_key(d)
    }

    #[allow(dead_code)] // tests, diagnostics
    pub fn has_file(&self, f: &Path) -> bool {
        self.files.contains_key(f)
    }
}

/// List a folder (None when it cannot be read: gone, or no permission).
fn scan(dir: &Path) -> Option<Snap> {
    let mut out = Snap::new();
    for e in std::fs::read_dir(dir).ok()?.flatten().take(MAX_ENTRIES) {
        if let Ok(m) = e.metadata() {
            out.insert(e.file_name(), stamp_of(&m));
        }
    }
    Some(out)
}

fn diff(dir: &Path, old: &Snap, new: &Snap, out: &mut Vec<FsEvent>) {
    for (name, v) in new {
        match old.get(name) {
            None => out.push(FsEvent { path: dir.join(name), kind: FsKind::Created }),
            // A folder's own mtime moves when its entries change: that is reported by polling the
            // folder itself (when it is expanded), not as a change of the parent.
            Some(o) if o != v && !(o.dir && v.dir) => {
                let kind = if o.ino != v.ino { FsKind::Moved } else { FsKind::Modified };
                out.push(FsEvent { path: dir.join(name), kind });
            }
            _ => {}
        }
    }
    for name in old.keys().filter(|n| !new.contains_key(*n)) {
        out.push(FsEvent { path: dir.join(name), kind: FsKind::Removed });
    }
}

/// One pass over the targets of `set`: the file system is read without the lock held.
pub fn poll(set: &Mutex<PollSet>) -> Vec<FsEvent> {
    let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) = {
        let s = set.lock().unwrap();
        (s.dirs.keys().cloned().collect(), s.files.keys().cloned().collect())
    };
    let dir_scans: Vec<(PathBuf, Option<Snap>)> = dirs.into_iter().map(|d| (d.clone(), scan(&d))).collect();
    let file_stamps: Vec<(PathBuf, Option<Stamp>)> =
        files.into_iter().map(|f| (f.clone(), std::fs::metadata(&f).ok().map(|m| stamp_of(&m)))).collect();
    let mut out = Vec::new();
    let mut s = set.lock().unwrap();
    for (d, new) in dir_scans {
        // Unwatched while the pass ran: forget it.
        let Some(st) = s.dirs.get_mut(&d) else { continue };
        *st = match (std::mem::take(st), new) {
            (DirState::New, Some(n)) => DirState::Seen(n),
            (DirState::New | DirState::Missing, None) => DirState::Missing,
            (DirState::Missing, Some(n)) => {
                out.push(FsEvent { path: d.clone(), kind: FsKind::Created });
                DirState::Seen(n)
            }
            (DirState::Seen(_), None) => {
                out.push(FsEvent { path: d.clone(), kind: FsKind::Removed });
                DirState::Missing
            }
            (DirState::Seen(old), Some(n)) => {
                diff(&d, &old, &n, &mut out);
                DirState::Seen(n)
            }
        };
    }
    for (f, new) in file_stamps {
        let Some(st) = s.files.get_mut(&f) else { continue };
        if let FileState::Seen(old) = st {
            match (*old, new) {
                (Some(_), None) => out.push(FsEvent { path: f.clone(), kind: FsKind::Removed }),
                (None, Some(_)) => out.push(FsEvent { path: f.clone(), kind: FsKind::Created }),
                (Some(o), Some(n)) if o != n => {
                    let kind = if o.ino != n.ino { FsKind::Moved } else { FsKind::Modified };
                    out.push(FsEvent { path: f.clone(), kind });
                }
                _ => {}
            }
        }
        *st = FileState::Seen(new);
    }
    out
}

enum Ctl {
    /// Run a pass now (new targets get recorded right away).
    Now,
    Stop,
}

/// The polling thread and the channel its events arrive on.
pub struct Poller {
    pub rx: Receiver<FsEvent>,
    pub every: Duration,
    set: Arc<Mutex<PollSet>>,
    ctl: Sender<Ctl>,
    thread: Option<JoinHandle<()>>,
    /// Passes run so far (tests, diagnostics).
    #[allow(dead_code)]
    passes: Arc<std::sync::atomic::AtomicUsize>,
}

impl Poller {
    pub fn new(every: Duration, wake: Wake) -> std::io::Result<Poller> {
        let set = Arc::new(Mutex::new(PollSet::default()));
        let (tx, rx) = channel();
        let (ctl, ctl_rx) = channel();
        let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s, p) = (set.clone(), passes.clone());
        let thread =
            std::thread::Builder::new().name("fs-poller".into()).spawn(move || run(s, every, ctl_rx, tx, wake, p))?;
        Ok(Poller { rx, every, set, ctl, thread: Some(thread), passes })
    }

    /// Poll exactly these folders and files from now on.
    pub fn set_targets(&self, dirs: &[PathBuf], files: &[PathBuf]) {
        let fresh = {
            let mut s = self.set.lock().unwrap();
            let before: std::collections::HashSet<PathBuf> = s.dirs.keys().chain(s.files.keys()).cloned().collect();
            s.set(dirs, files);
            s.dirs.keys().chain(s.files.keys()).any(|p| !before.contains(p))
        };
        // Record new targets right away, so a change right after this call is not missed.
        if fresh {
            let _ = self.ctl.send(Ctl::Now);
        }
    }

    /// Run a pass now instead of waiting for the interval.
    pub fn poll_now(&self) {
        let _ = self.ctl.send(Ctl::Now);
    }

    #[allow(dead_code)] // tests, diagnostics
    pub fn passes(&self) -> usize {
        self.passes.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[allow(dead_code)] // tests, diagnostics
    pub fn is_polling_dir(&self, d: &Path) -> bool {
        self.set.lock().unwrap().has_dir(d)
    }

    #[allow(dead_code)] // tests, diagnostics
    pub fn is_polling_file(&self, f: &Path) -> bool {
        self.set.lock().unwrap().has_file(f)
    }
}

fn run(
    set: Arc<Mutex<PollSet>>,
    every: Duration,
    ctl: Receiver<Ctl>,
    tx: Sender<FsEvent>,
    wake: Wake,
    passes: Arc<std::sync::atomic::AtomicUsize>,
) {
    loop {
        match ctl.recv_timeout(every) {
            Ok(Ctl::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Ctl::Now) | Err(RecvTimeoutError::Timeout) => {}
        }
        let evs = poll(&set);
        passes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if evs.is_empty() {
            continue; // nothing changed: the UI sleeps on
        }
        for e in evs {
            if tx.send(e).is_err() {
                return;
            }
        }
        wake();
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        let _ = self.ctl.send(Ctl::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-poll-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn kinds(ev: &[FsEvent], p: &Path) -> Vec<FsKind> {
        ev.iter().filter(|e| e.path == p).map(|e| e.kind).collect()
    }

    /// Make sure the next write lands on a different mtime even on a coarse file system.
    fn bump(p: &Path, text: &str) {
        let before = fs::metadata(p).ok().and_then(|m| m.modified().ok());
        fs::write(p, text).unwrap();
        if before.is_some() && fs::metadata(p).ok().and_then(|m| m.modified().ok()) == before {
            let f = fs::File::options().write(true).open(p).unwrap();
            let t = before.unwrap() + Duration::from_secs(2);
            f.set_modified(t).unwrap();
        }
    }

    #[test]
    fn first_pass_only_records_then_diffs() {
        let d = tmp("diff");
        let a = d.join("a.txt");
        fs::write(&a, "1").unwrap();
        let set = Mutex::new(PollSet::default());
        set.lock().unwrap().set(std::slice::from_ref(&d), &[]);
        assert!(poll(&set).is_empty(), "the first pass is a baseline");
        assert!(poll(&set).is_empty(), "nothing changed, nothing reported");
        bump(&a, "22");
        let b = d.join("b.txt");
        fs::write(&b, "x").unwrap();
        let ev = poll(&set);
        assert_eq!(kinds(&ev, &a), vec![FsKind::Modified]);
        assert_eq!(kinds(&ev, &b), vec![FsKind::Created]);
        fs::remove_file(&a).unwrap();
        let ev = poll(&set);
        assert_eq!(kinds(&ev, &a), vec![FsKind::Removed]);
        assert!(poll(&set).is_empty());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn atomic_rename_over_is_seen_even_with_same_size_and_mtime() {
        let d = tmp("atomic");
        let f = d.join("doc.ts");
        fs::write(&f, "old").unwrap();
        let set = Mutex::new(PollSet::default());
        set.lock().unwrap().set(&[], std::slice::from_ref(&f));
        assert!(poll(&set).is_empty());
        let t = d.join("doc.ts.tmp");
        fs::write(&t, "new").unwrap();
        // Same size and mtime as the original: only the inode tells.
        let m = fs::metadata(&f).unwrap().modified().unwrap();
        fs::File::options().write(true).open(&t).unwrap().set_modified(m).unwrap();
        fs::rename(&t, &f).unwrap();
        let ev = poll(&set);
        assert_eq!(kinds(&ev, &f).len(), 1, "{ev:?}");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn delete_and_recreate_of_a_file_and_of_a_folder() {
        let d = tmp("recreate");
        let sub = d.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let f = d.join("f.txt");
        fs::write(&f, "1").unwrap();
        let set = Mutex::new(PollSet::default());
        set.lock().unwrap().set(std::slice::from_ref(&sub), std::slice::from_ref(&f));
        assert!(poll(&set).is_empty());
        fs::remove_file(&f).unwrap();
        fs::remove_dir(&sub).unwrap();
        let ev = poll(&set);
        assert_eq!(kinds(&ev, &f), vec![FsKind::Removed]);
        assert_eq!(kinds(&ev, &sub), vec![FsKind::Removed]);
        assert!(poll(&set).is_empty(), "missing stays quiet");
        fs::write(&f, "2").unwrap();
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("n.txt"), "x").unwrap();
        let ev = poll(&set);
        assert_eq!(kinds(&ev, &f), vec![FsKind::Created]);
        assert_eq!(kinds(&ev, &sub), vec![FsKind::Created]);
        // The recreated folder keeps being polled.
        fs::write(sub.join("m.txt"), "x").unwrap();
        let ev = poll(&set);
        assert_eq!(kinds(&ev, &sub.join("m.txt")), vec![FsKind::Created]);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn targets_keep_their_state_and_files_in_polled_folders_are_covered() {
        let d = tmp("targets");
        let f = d.join("f.txt");
        fs::write(&f, "1").unwrap();
        let mut s = PollSet::default();
        s.set(std::slice::from_ref(&d), std::slice::from_ref(&f));
        assert!(s.has_dir(&d) && !s.has_file(&f), "the folder pass covers the file");
        assert_eq!(s.len(), 1);
        let set = Mutex::new(s);
        poll(&set);
        bump(&f, "changed");
        // Setting the same targets again does not forget what was seen.
        set.lock().unwrap().set(std::slice::from_ref(&d), &[]);
        assert_eq!(kinds(&poll(&set), &f), vec![FsKind::Modified]);
        set.lock().unwrap().set(&[], &[]);
        assert!(set.lock().unwrap().is_empty());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn subfolder_mtime_changes_are_not_reported_on_the_parent() {
        let d = tmp("subdir");
        let sub = d.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let set = Mutex::new(PollSet::default());
        set.lock().unwrap().set(std::slice::from_ref(&d), &[]);
        poll(&set);
        fs::write(sub.join("deep.txt"), "x").unwrap();
        assert!(poll(&set).is_empty(), "only expanded (polled) folders report their contents");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn thread_reports_changes_and_stays_quiet_when_idle() {
        let d = tmp("thread");
        let f = d.join("a.txt");
        fs::write(&f, "1").unwrap();
        let wakes = Arc::new(AtomicUsize::new(0));
        let w2 = wakes.clone();
        let p = Poller::new(
            Duration::from_millis(30),
            Arc::new(move || {
                w2.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
        p.set_targets(std::slice::from_ref(&d), &[]);
        assert!(p.is_polling_dir(&d));
        // Several idle passes: no event, no wake.
        let t0 = Instant::now();
        while p.passes() < 4 && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(p.passes() >= 4);
        assert_eq!(wakes.load(Ordering::SeqCst), 0, "idle passes never wake the UI");
        assert!(p.rx.try_recv().is_err());
        bump(&f, "changed!");
        let e = p.rx.recv_timeout(Duration::from_secs(3)).expect("the change is reported");
        assert_eq!((e.path.as_path(), e.kind), (f.as_path(), FsKind::Modified));
        assert!(wakes.load(Ordering::SeqCst) >= 1);
        // Drop joins the thread promptly.
        let t = Instant::now();
        drop(p);
        assert!(t.elapsed() < Duration::from_secs(2));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_target_added_later_is_recorded_at_once() {
        let d = tmp("late");
        let f = d.join("x.txt");
        fs::write(&f, "1").unwrap();
        // A long interval: only the immediate pass of `set_targets` can record the file.
        let p = Poller::new(Duration::from_secs(60), Arc::new(|| {})).unwrap();
        p.set_targets(&[], std::slice::from_ref(&f));
        let t0 = Instant::now();
        while p.passes() < 1 && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
        }
        bump(&f, "two");
        p.poll_now();
        let e = p.rx.recv_timeout(Duration::from_secs(3)).expect("reported on poll_now");
        assert_eq!(e.path, f);
        fs::remove_dir_all(&d).ok();
    }
}
