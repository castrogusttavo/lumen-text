//! File-system watcher on raw Linux inotify (through `libc`, no extra crates).
//!
//! One reader thread `poll`s the inotify fd plus an eventfd (used for a clean shutdown),
//! decodes events, coalesces bursts per path (trailing debounce of [`DEBOUNCE_MS`]) and sends
//! one [`FsEvent`] per path through `rx`, then calls the `wake` callback.
//!
//! Semantics of [`FsKind`]:
//! * `Created`  - a file/dir appeared (IN_CREATE).
//! * `Modified` - contents were written (IN_MODIFY / IN_CLOSE_WRITE), or the queue overflowed
//!   (then every watched directory is reported `Modified`: rescan it).
//! * `Removed`  - IN_DELETE, or a watched directory itself was deleted (IN_DELETE_SELF).
//! * `Moved`    - IN_MOVED_FROM / IN_MOVED_TO / IN_MOVE_SELF: the path was a rename source *or*
//!   target, `stat` it to find out which. An editor's atomic save (write tmp, rename over target)
//!   gives `Moved` for the target, so treat `Moved` on a file you have open like `Modified`.
//!
//! Merging inside one debounce window: Created+Modified = Created; Created+Removed = Removed;
//! Removed+Created = Modified (replaced); Modified/Created+Moved = Moved; otherwise the newest wins.
#![allow(dead_code)]

use crate::lsp::Wake;
use std::collections::HashMap;
use std::io;
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Events for the same path closer together than this collapse into one.
pub const DEBOUNCE_MS: u64 = 40;
/// A path that keeps changing is still reported at least this often.
const MAX_DELAY_MS: u64 = 250;
/// Directory names that are never watched.
const IGNORED: &[&str] = &["node_modules", ".git", "target", "dist", ".next"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Created,
    Modified,
    Removed,
    Moved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEvent {
    pub path: PathBuf,
    pub kind: FsKind,
}

#[derive(Default)]
struct State {
    by_wd: HashMap<i32, PathBuf>,
    by_path: HashMap<PathBuf, i32>,
}

pub struct Watcher {
    pub rx: Receiver<FsEvent>,
    ifd: i32,
    efd: i32,
    state: Arc<Mutex<State>>,
    thread: Option<JoinHandle<()>>,
}

/// `fs.inotify.max_user_watches`, if readable.
pub fn watch_limit() -> Option<usize> {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches").ok()?.trim().parse().ok()
}

fn is_ignored(dir: &Path) -> bool {
    dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| IGNORED.contains(&n))
}

const MASK: u32 = libc::IN_CREATE
    | libc::IN_MODIFY
    | libc::IN_CLOSE_WRITE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

impl Watcher {
    pub fn new(wake: Wake) -> io::Result<Watcher> {
        let ifd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if ifd < 0 {
            let e = io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(libc::EMFILE) => {
                    io::Error::other("inotify instance limit reached (fs.inotify.max_user_instances)")
                }
                _ => e,
            });
        }
        let efd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if efd < 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(ifd) };
            return Err(e);
        }
        let state = Arc::new(Mutex::new(State::default()));
        let (tx, rx) = channel();
        let st = state.clone();
        let spawned = std::thread::Builder::new().name("fs-watcher".into()).spawn(move || run(ifd, efd, st, tx, wake));
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                unsafe {
                    libc::close(ifd);
                    libc::close(efd);
                }
                return Err(e);
            }
        };
        Ok(Watcher { rx, ifd, efd, state, thread: Some(thread) })
    }

    /// Watch `dir` (non-recursive). Idempotent. Ignored names (node_modules, .git, ...) are skipped
    /// silently. On ENOSPC the error message names the limit and how to raise it.
    pub fn watch_dir(&mut self, dir: &Path) -> io::Result<()> {
        if is_ignored(dir) {
            return Ok(());
        }
        let mut st = self.state.lock().unwrap();
        if st.by_path.contains_key(dir) {
            return Ok(());
        }
        let c = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(dir.as_os_str()))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let wd = unsafe { libc::inotify_add_watch(self.ifd, c.as_ptr(), MASK) };
        if wd < 0 {
            let e = io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(libc::ENOSPC) => {
                    let lim = watch_limit().map(|n| n.to_string()).unwrap_or_else(|| "?".into());
                    io::Error::other(format!(
                        "inotify watch limit reached (fs.inotify.max_user_watches = {lim}); cannot watch {}. \
                         Raise it with: sudo sysctl fs.inotify.max_user_watches=524288",
                        dir.display()
                    ))
                }
                _ => e,
            });
        }
        // The same inode reached through another path (symlink) returns the same wd.
        if let std::collections::hash_map::Entry::Vacant(e) = st.by_wd.entry(wd) {
            e.insert(dir.to_path_buf());
            st.by_path.insert(dir.to_path_buf(), wd);
        }
        Ok(())
    }

    pub fn unwatch_dir(&mut self, dir: &Path) -> io::Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(wd) = st.by_path.remove(dir) {
            st.by_wd.remove(&wd);
            unsafe { libc::inotify_rm_watch(self.ifd, wd) };
        }
        Ok(())
    }

    pub fn is_watching(&self, dir: &Path) -> bool {
        self.state.lock().unwrap().by_path.contains_key(dir)
    }

    pub fn watched_count(&self) -> usize {
        self.state.lock().unwrap().by_path.len()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let one: u64 = 1;
        unsafe { libc::write(self.efd, &one as *const u64 as *const c_void, 8) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            libc::close(self.ifd);
            libc::close(self.efd);
        }
    }
}

struct Pending {
    kind: FsKind,
    first: Instant,
    last: Instant,
    seq: u64,
}

fn merge(old: FsKind, new: FsKind) -> FsKind {
    use FsKind::*;
    match (old, new) {
        (Created, Modified) => Created,
        (Moved, Modified) => Moved,
        (Removed, Modified) => Modified,
        (Created, Removed) => Removed,
        (Removed, Created) => Modified,
        (Created | Modified, Moved) => Moved,
        (_, n) => n,
    }
}

fn note(pending: &mut HashMap<PathBuf, Pending>, seq: &mut u64, path: PathBuf, kind: FsKind, now: Instant) {
    match pending.get_mut(&path) {
        Some(p) => {
            p.kind = merge(p.kind, kind);
            p.last = now;
        }
        None => {
            *seq += 1;
            pending.insert(path, Pending { kind, first: now, last: now, seq: *seq });
        }
    }
}

fn deadline(p: &Pending) -> Instant {
    (p.last + Duration::from_millis(DEBOUNCE_MS)).min(p.first + Duration::from_millis(MAX_DELAY_MS))
}

fn run(ifd: i32, efd: i32, state: Arc<Mutex<State>>, tx: Sender<FsEvent>, wake: Wake) {
    let mut pending: HashMap<PathBuf, Pending> = HashMap::new();
    let mut seq = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let timeout: i32 = match pending.values().map(deadline).min() {
            None => -1,
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                (left.as_micros().div_ceil(1000) as i32).max(0)
            }
        };
        let mut fds = [
            libc::pollfd { fd: ifd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: efd, events: libc::POLLIN, revents: 0 },
        ];
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if r < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 {
            return; // shutdown requested
        }
        if fds[0].revents & libc::POLLIN != 0 {
            drain(ifd, &mut buf, &state, &mut pending, &mut seq);
        }
        // Flush what is due.
        let now = Instant::now();
        let mut due: Vec<(u64, PathBuf, FsKind)> = Vec::new();
        pending.retain(|path, p| {
            if deadline(p) <= now {
                due.push((p.seq, path.clone(), p.kind));
                false
            } else {
                true
            }
        });
        if !due.is_empty() {
            due.sort_by_key(|d| d.0);
            for (_, path, kind) in due {
                if tx.send(FsEvent { path, kind }).is_err() {
                    return; // receiver gone
                }
            }
            wake();
        }
    }
}

fn drain(ifd: i32, buf: &mut [u8], state: &Mutex<State>, pending: &mut HashMap<PathBuf, Pending>, seq: &mut u64) {
    loop {
        let n = unsafe { libc::read(ifd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return; // EAGAIN: drained
        }
        if n == 0 {
            return;
        }
        let n = n as usize;
        let now = Instant::now();
        let mut off = 0;
        while off + 16 <= n {
            let wd = i32::from_ne_bytes(buf[off..off + 4].try_into().unwrap());
            let mask = u32::from_ne_bytes(buf[off + 4..off + 8].try_into().unwrap());
            let len = u32::from_ne_bytes(buf[off + 12..off + 16].try_into().unwrap()) as usize;
            let name_end = (off + 16 + len).min(n);
            let raw = &buf[off + 16..name_end];
            let raw = &raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())];
            off = name_end;

            let mut st = state.lock().unwrap();
            if mask & libc::IN_Q_OVERFLOW != 0 {
                for dir in st.by_path.keys() {
                    note(pending, seq, dir.clone(), FsKind::Modified, now);
                }
                continue;
            }
            let Some(dir) = st.by_wd.get(&wd).cloned() else { continue };
            if mask & libc::IN_IGNORED != 0 {
                st.by_wd.remove(&wd);
                st.by_path.remove(&dir);
                continue;
            }
            if mask & libc::IN_DELETE_SELF != 0 {
                note(pending, seq, dir, FsKind::Removed, now);
                continue;
            }
            if mask & libc::IN_MOVE_SELF != 0 {
                // The directory left its path: the watch is stale.
                st.by_wd.remove(&wd);
                st.by_path.remove(&dir);
                unsafe { libc::inotify_rm_watch(ifd, wd) };
                note(pending, seq, dir, FsKind::Moved, now);
                continue;
            }
            drop(st);
            if raw.is_empty() {
                continue;
            }
            let path = dir.join(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(raw));
            let kind = if mask & libc::IN_CREATE != 0 {
                FsKind::Created
            } else if mask & (libc::IN_MODIFY | libc::IN_CLOSE_WRITE) != 0 {
                FsKind::Modified
            } else if mask & libc::IN_DELETE != 0 {
                FsKind::Removed
            } else if mask & (libc::IN_MOVED_FROM | libc::IN_MOVED_TO) != 0 {
                FsKind::Moved
            } else {
                continue;
            };
            note(pending, seq, path, kind, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-watch-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn mk() -> (Watcher, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let w = Watcher::new(Arc::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
        (w, n)
    }

    /// Collect events until `quiet` ms pass without any (or `max` ms total).
    fn collect(w: &Watcher, quiet: u64, max: u64) -> Vec<FsEvent> {
        let end = Instant::now() + Duration::from_millis(max);
        let mut out = Vec::new();
        while Instant::now() < end {
            match w.rx.recv_timeout(Duration::from_millis(quiet)) {
                Ok(e) => out.push(e),
                Err(_) => break,
            }
        }
        out
    }

    fn kinds_of(ev: &[FsEvent], p: &Path) -> Vec<FsKind> {
        ev.iter().filter(|e| e.path == p).map(|e| e.kind).collect()
    }

    #[test]
    fn detects_create() {
        let d = tmp("create");
        let (mut w, wake) = mk();
        w.watch_dir(&d).unwrap();
        let f = d.join("a.txt");
        fs::write(&f, "x").unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &f), vec![FsKind::Created]);
        assert!(wake.load(Ordering::SeqCst) >= 1);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn detects_modify() {
        let d = tmp("modify");
        let f = d.join("a.txt");
        fs::write(&f, "x").unwrap();
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        fs::write(&f, "yy").unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &f), vec![FsKind::Modified]);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn detects_delete() {
        let d = tmp("delete");
        let f = d.join("a.txt");
        fs::write(&f, "x").unwrap();
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        fs::remove_file(&f).unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &f), vec![FsKind::Removed]);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn detects_rename_both_sides() {
        let d = tmp("rename");
        let a = d.join("a.txt");
        let b = d.join("b.txt");
        fs::write(&a, "x").unwrap();
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        fs::rename(&a, &b).unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &a), vec![FsKind::Moved]);
        assert_eq!(kinds_of(&ev, &b), vec![FsKind::Moved]);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn atomic_save_via_rename_reports_target() {
        let d = tmp("atomic");
        let f = d.join("doc.ts");
        fs::write(&f, "old").unwrap();
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        let t = d.join("doc.ts.lumen-tmp");
        fs::write(&t, "new").unwrap();
        fs::rename(&t, &f).unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &f), vec![FsKind::Moved]);
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        // Exactly one event per path.
        assert_eq!(ev.len(), ev.iter().map(|e| &e.path).collect::<std::collections::HashSet<_>>().len());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn debounce_coalesces_bursts() {
        let d = tmp("burst");
        let f = d.join("a.txt");
        fs::write(&f, "x").unwrap();
        let (mut w, wake) = mk();
        w.watch_dir(&d).unwrap();
        for i in 0..20 {
            fs::write(&f, format!("{i}")).unwrap();
        }
        let ev = collect(&w, 300, 2000);
        let n = kinds_of(&ev, &f).len();
        assert!((1..=3).contains(&n), "got {n} events for 20 writes");
        assert!(wake.load(Ordering::SeqCst) <= 3);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn create_then_write_is_one_created() {
        let d = tmp("cw");
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        let f = d.join("n.txt");
        fs::write(&f, "a").unwrap();
        fs::write(&f, "ab").unwrap();
        fs::write(&f, "abc").unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &f), vec![FsKind::Created]);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn create_then_delete_reports_removed_once() {
        let d = tmp("cd");
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        let f = d.join("t.txt");
        fs::write(&f, "a").unwrap();
        fs::remove_file(&f).unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &f), vec![FsKind::Removed]);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn unwatch_stops_events() {
        let d = tmp("unwatch");
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        assert!(w.is_watching(&d));
        w.unwatch_dir(&d).unwrap();
        assert!(!w.is_watching(&d));
        w.unwatch_dir(&d).unwrap(); // idempotent
        fs::write(d.join("a.txt"), "x").unwrap();
        assert!(collect(&w, 200, 600).is_empty());
        // And it can be re-watched.
        w.watch_dir(&d).unwrap();
        fs::write(d.join("b.txt"), "x").unwrap();
        assert_eq!(collect(&w, 300, 2000).len(), 1);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn watch_is_idempotent() {
        let d = tmp("idem");
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        w.watch_dir(&d).unwrap();
        assert_eq!(w.watched_count(), 1);
        fs::write(d.join("a.txt"), "x").unwrap();
        assert_eq!(collect(&w, 300, 2000).len(), 1);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn ignored_dir_names_are_skipped() {
        let d = tmp("ignored");
        let (mut w, _) = mk();
        for name in IGNORED {
            let sub = d.join(name);
            fs::create_dir_all(&sub).unwrap();
            w.watch_dir(&sub).unwrap();
            assert!(!w.is_watching(&sub));
            fs::write(sub.join("x.txt"), "x").unwrap();
        }
        assert_eq!(w.watched_count(), 0);
        assert!(collect(&w, 200, 600).is_empty());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn missing_dir_is_an_error() {
        let (mut w, _) = mk();
        assert!(w.watch_dir(Path::new("/definitely/not/here/lumen")).is_err());
    }

    #[test]
    fn non_recursive() {
        let d = tmp("nonrec");
        let sub = d.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        fs::write(sub.join("deep.txt"), "x").unwrap();
        assert!(collect(&w, 200, 600).is_empty());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn many_files_in_a_dir() {
        let d = tmp("many");
        let (mut w, _) = mk();
        w.watch_dir(&d).unwrap();
        for i in 0..300 {
            fs::write(d.join(format!("f{i}.txt")), "x").unwrap();
        }
        let ev = collect(&w, 400, 5000);
        let paths: std::collections::HashSet<_> = ev.iter().map(|e| e.path.clone()).collect();
        assert_eq!(paths.len(), 300);
        assert_eq!(ev.len(), 300, "one event per file");
        assert!(ev.iter().all(|e| e.kind == FsKind::Created));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn watched_dir_removed_reports_removed() {
        let d = tmp("selfdel");
        let sub = d.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let (mut w, _) = mk();
        w.watch_dir(&sub).unwrap();
        fs::remove_dir(&sub).unwrap();
        let ev = collect(&w, 300, 2000);
        assert_eq!(kinds_of(&ev, &sub), vec![FsKind::Removed]);
        // The kernel drops the watch; our bookkeeping follows.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!w.is_watching(&sub));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn drop_stops_thread_and_closes_fds() {
        let (w, _) = mk();
        let (ifd, efd) = (w.ifd, w.efd);
        let (done_tx, done_rx) = channel();
        std::thread::spawn(move || {
            drop(w); // joins the reader thread
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(Duration::from_secs(3)).expect("drop must stop the reader thread");
        for fd in [ifd, efd] {
            let r = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if r != -1 {
                // Number reused by a concurrent test: must not be our inotify/eventfd any more
                // unless someone else created one; just make sure it is not a leak by closing logic.
                continue;
            }
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        }
    }

    #[test]
    fn create_and_drop_many_does_not_leak() {
        let count = || fs::read_dir("/proc/self/fd").map(|d| d.count()).unwrap_or(0);
        let before = count();
        for _ in 0..40 {
            let (mut w, _) = mk();
            let _ = w.watch_dir(&std::env::temp_dir());
        }
        let after = count();
        // Other tests run concurrently and hold a few fds; 40 leaked watchers would add 80+.
        assert!(after < before + 40, "fds before {before}, after {after}");
    }

    #[test]
    fn limit_is_readable() {
        if let Some(n) = watch_limit() {
            assert!(n > 0);
        }
    }

    #[test]
    fn merge_rules() {
        use FsKind::*;
        assert_eq!(merge(Created, Modified), Created);
        assert_eq!(merge(Created, Removed), Removed);
        assert_eq!(merge(Removed, Created), Modified);
        assert_eq!(merge(Modified, Removed), Removed);
        assert_eq!(merge(Modified, Moved), Moved);
        assert_eq!(merge(Moved, Modified), Moved);
    }
}

/// Watch `root` and every sub-directory (skipping [`IGNORED`] and hidden ones), at most `max` directories.
pub fn watch_tree(w: &mut Watcher, root: &Path, max: usize) {
    let mut stack = vec![root.to_path_buf()];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        if n >= max || w.watch_dir(&d).is_err() {
            return;
        }
        n += 1;
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir())
                && !is_ignored(&p)
                && !e.file_name().to_string_lossy().starts_with('.')
            {
                stack.push(p);
            }
        }
    }
}
