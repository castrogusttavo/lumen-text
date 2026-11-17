//! Memory measurement. PSS (proportional set size) is the honest number:
//! RSS double-counts shared libraries (GPU drivers, libc, ...).

#[cfg(target_os = "linux")]
use std::fs;
use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    time::Duration,
};

#[derive(Debug, Clone, Copy)]
pub struct MemSnapshot {
    pub pss_kb: u64,
    pub rss_kb: u64,
}

#[cfg(not(target_os = "linux"))]
pub fn snapshot() -> Option<MemSnapshot> {
    // No PSS outside Linux: the resident size is the closest number the OS offers.
    let kb = resident_kb(std::process::id())?;
    Some(MemSnapshot { pss_kb: kb, rss_kb: kb })
}

#[cfg(target_os = "linux")]
pub fn snapshot() -> Option<MemSnapshot> {
    let text = fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    let field = |name: &str| -> Option<u64> {
        text.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse().ok()
    };
    Some(MemSnapshot { pss_kb: field("Pss:")?, rss_kb: field("Rss:")? })
}

#[cfg(not(target_os = "linux"))]
pub fn report(label: &str) {
    if let Some(m) = snapshot() {
        eprintln!("[mem] {label:<28} RSS {:>7.1} MB", m.rss_kb as f64 / 1024.0);
    }
}

#[cfg(target_os = "linux")]
pub fn report(label: &str) {
    match snapshot() {
        Some(m) => {
            let mi = unsafe { libc::mallinfo2() };
            eprintln!(
                "[mem] {label:<28} PSS {:>7.1} MB   RSS {:>7.1} MB   heap live {:>7.1} MB free {:>7.1} MB mmap {:>6.1} MB",
                m.pss_kb as f64 / 1024.0,
                m.rss_kb as f64 / 1024.0,
                mi.uordblks as f64 / 1048576.0,
                mi.fordblks as f64 / 1048576.0,
                mi.hblkhd as f64 / 1048576.0
            )
        }
        None => eprintln!("[mem] {label}: /proc/self/smaps_rollup unavailable"),
    }
}

/// PSS in kB of another process (e.g. a language server we spawned), read now.
#[cfg(not(target_os = "linux"))]
fn pss_direct(pid: u32) -> Option<u64> {
    resident_kb(pid)
}

/// Resident set size in kB of any process of ours (Windows working set).
#[cfg(windows)]
fn resident_kb(pid: u32) -> Option<u64> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };
    // SAFETY: the handle is closed again and the counters struct is plain data of the declared size.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = GetProcessMemoryInfo(h, &mut c, c.cb);
        CloseHandle(h);
        (ok != 0).then_some(c.WorkingSetSize as u64 / 1024)
    }
}

/// Resident set size in kB of any process of ours (macOS `proc_pidinfo`).
#[cfg(target_os = "macos")]
fn resident_kb(pid: u32) -> Option<u64> {
    // SAFETY: plain data out-parameter with its exact size.
    unsafe {
        let mut ti: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let n = libc::proc_pidinfo(pid as i32, libc::PROC_PIDTASKINFO, 0, &mut ti as *mut _ as *mut libc::c_void, size);
        (n == size).then_some(ti.pti_resident_size / 1024)
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn resident_kb(_pid: u32) -> Option<u64> {
    None
}

/// PSS and RSS of any process of ours; outside Linux both are the resident size.
#[cfg(target_os = "linux")]
pub fn mem_of(pid: u32) -> Option<MemSnapshot> {
    let text = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    let field = |name: &str| -> Option<u64> {
        text.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse().ok()
    };
    Some(MemSnapshot { pss_kb: field("Pss:")?, rss_kb: field("Rss:")? })
}

#[cfg(not(target_os = "linux"))]
pub fn mem_of(pid: u32) -> Option<MemSnapshot> {
    let kb = resident_kb(pid)?;
    Some(MemSnapshot { pss_kb: kb, rss_kb: kb })
}

#[cfg(target_os = "linux")]
fn pss_direct(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    text.lines().find(|l| l.starts_with("Pss:"))?.split_whitespace().nth(1)?.parse().ok()
}

// ───────────── resident memory (cheap) ─────────────
//
// What the periodic threshold checks read (the tsgo memory guard and the memory budget): the
// anonymous and shmem resident pages, `RssAnon` + `RssShmem` of `/proc/<pid>/status`. The kernel
// answers that from counters it keeps per process, with no page-table walk and without the
// target's mmap lock, so the cost does not grow with the process (unlike `smaps_rollup`).
//
// It is not PSS. It over-counts whatever is shared: a shmem page, or an anonymous page still
// shared copy-on-write after a fork, counts in full for every process mapping it, where PSS
// divides it among them. And it leaves file-backed pages out altogether (the executable and
// mapped files, a few tens of MB that can be dropped and re-read at will). For a language
// server nearly everything is its own heap, which is anonymous and private, so the two numbers
// differ by tens of MB at most: nothing against thresholds of one to five GB that already need
// two samples in a row. The numbers the user reads (status bar, Resources panel) stay PSS.

/// `RssAnon` + `RssShmem` in kB out of the text of `/proc/<pid>/status`.
#[cfg(any(target_os = "linux", test))]
fn status_resident_kb(text: &str) -> Option<u64> {
    let field = |name: &str| -> Option<u64> {
        text.lines().find_map(|l| l.strip_prefix(name)?.split_whitespace().next()?.parse().ok())
    };
    Some(field("RssAnon:")? + field("RssShmem:").unwrap_or(0))
}

/// Anonymous + shmem resident memory in kB of a process, read now (see above).
#[cfg(target_os = "linux")]
fn resident_direct(pid: u32) -> Option<u64> {
    status_resident_kb(&fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
}

/// Outside Linux the resident size is the only number there is.
#[cfg(not(target_os = "linux"))]
fn resident_direct(pid: u32) -> Option<u64> {
    resident_kb(pid)
}

// ───────────── background sampler ─────────────
//
// `smaps_rollup` makes the kernel walk the page tables of the process it describes while holding
// that process's mmap lock, so reading it costs time in proportion to the process's size (a
// language server is hundreds of MB to several GB) and briefly stalls it. One thread therefore
// does all the periodic reading and publishes the numbers; the status bar, the memory guard and
// the budget check read what was published and never touch /proc on the UI thread.

/// Period of the background sampler.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// A displayed number must move by more than this for the sampler to wake the UI.
const SHOW_STEP_KB: u64 = 1024;

/// What the sampler knows about one process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcMem {
    /// PSS (what the user reads); None when it was not sampled or the process is gone.
    pub pss_kb: Option<u64>,
    /// Anonymous + shmem resident memory (what the threshold checks read); None likewise.
    pub resident_kb: Option<u64>,
}

/// One published measurement: the editor itself and the watched processes, by pid.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub own: ProcMem,
    pub procs: Vec<(u32, ProcMem)>,
}

/// What the UI thread asked the sampler to measure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Want {
    pids: Vec<u32>,
    /// PSS of everything (the status bar shows it): one `smaps_rollup` per process.
    pss: bool,
    /// Resident memory of everything (the guard and the budget check): one `status` per process.
    resident: bool,
}

impl Want {
    /// Nothing to measure: the thread sleeps until the request changes.
    fn idle(&self) -> bool {
        !self.pss && !self.resident
    }
}

struct SamplerState {
    want: Want,
    /// Measure now instead of waiting for the period to end.
    kick: bool,
    sample: Sample,
    /// Number of samples published so far.
    generation: u64,
}

struct Sampler {
    state: Mutex<SamplerState>,
    cv: Condvar,
}

impl Sampler {
    fn lock(&self) -> MutexGuard<'_, SamplerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

static SAMPLER: OnceLock<Arc<Sampler>> = OnceLock::new();

/// Tell the sampler what to measure (called from the UI tick; cheap when nothing changed). The
/// thread starts on the first call and measures at once whenever the request changes. Tests have
/// no sampler: every reader below then measures synchronously, as before.
pub fn watch(
    pids: impl Iterator<Item = u32> + Clone,
    pss: bool,
    resident: bool,
    wake: impl FnOnce() -> Arc<dyn Fn() + Send + Sync>,
) {
    if cfg!(test) {
        return;
    }
    let s = SAMPLER.get_or_init(|| {
        let s = Arc::new(Sampler {
            state: Mutex::new(SamplerState {
                want: Want::default(),
                kick: false,
                sample: Sample::default(),
                generation: 0,
            }),
            cv: Condvar::new(),
        });
        let (t, w) = (s.clone(), wake());
        // Without the thread nothing is ever published and every reader falls back to reading now.
        let _ = std::thread::Builder::new().name("mem-sampler".into()).spawn(move || run(&t, &*w));
        s
    });
    let mut g = s.lock();
    if g.want.pss == pss && g.want.resident == resident && g.want.pids.iter().copied().eq(pids.clone()) {
        return;
    }
    g.want = Want { pids: pids.collect(), pss, resident };
    g.kick = true;
    drop(g);
    s.cv.notify_one();
}

fn run(s: &Sampler, wake: &(dyn Fn() + Send + Sync)) {
    // The numbers the UI last drew (it is woken only when one of them moves visibly).
    let mut shown: Vec<(u32, u64)> = Vec::new();
    loop {
        let want = {
            let mut g = s.lock();
            while !g.kick {
                if g.want.idle() {
                    g = s.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                } else {
                    let (ng, t) = s.cv.wait_timeout(g, SAMPLE_EVERY).unwrap_or_else(|e| e.into_inner());
                    g = ng;
                    if t.timed_out() {
                        break;
                    }
                }
            }
            g.kick = false;
            g.want.clone()
        };
        if want.idle() {
            continue;
        }
        let sample = measure(&want);
        let cur = displayed(&sample);
        {
            let mut g = s.lock();
            g.sample = sample;
            g.generation += 1;
        }
        if display_changed(&shown, &cur) {
            shown = cur;
            wake();
        }
    }
}

/// Read everything `want` asks for, now (the sampler thread; tests call it directly).
fn measure(want: &Want) -> Sample {
    let one = |pss: Option<u64>, pid: u32| ProcMem {
        pss_kb: pss,
        resident_kb: if want.resident { resident_direct(pid) } else { None },
    };
    let own = one(if want.pss { snapshot().map(|m| m.pss_kb) } else { None }, std::process::id());
    let procs = want.pids.iter().map(|&pid| (pid, one(if want.pss { pss_direct(pid) } else { None }, pid))).collect();
    Sample { own, procs }
}

/// The numbers of a sample the status bar shows: the editor as pid 0, then each process.
fn displayed(s: &Sample) -> Vec<(u32, u64)> {
    let own = s.own.pss_kb.map(|kb| (0, kb));
    own.into_iter().chain(s.procs.iter().filter_map(|(pid, m)| m.pss_kb.map(|kb| (*pid, kb)))).collect()
}

/// True when the status bar would read differently: another set of processes, or one number
/// moved by more than a MB since the UI was last woken.
fn display_changed(prev: &[(u32, u64)], cur: &[(u32, u64)]) -> bool {
    prev.len() != cur.len() || prev.iter().zip(cur).any(|(a, b)| a.0 != b.0 || a.1.abs_diff(b.1) > SHOW_STEP_KB)
}

/// Published numbers of `pid` (None = the editor itself); None when nothing was published for it.
fn published(pid: Option<u32>) -> Option<ProcMem> {
    let g = SAMPLER.get()?.lock();
    if g.generation == 0 {
        return None;
    }
    match pid {
        None => Some(g.sample.own),
        Some(pid) => g.sample.procs.iter().find(|(p, _)| *p == pid).map(|(_, m)| *m),
    }
}

/// How many samples were published; None while no sampler runs (tests, before the first tick).
/// A reader that counts consecutive samples uses it to tell a new sample from the one it saw.
pub fn generation() -> Option<u64> {
    Some(SAMPLER.get()?.lock().generation)
}

/// PSS in kB of the editor: the published number, read now when there is none yet.
pub fn own_pss_kb() -> Option<u64> {
    published(None).and_then(|m| m.pss_kb).or_else(|| snapshot().map(|m| m.pss_kb))
}

/// PSS in kB of another process (e.g. a language server we spawned): the published number, read
/// now when there is none yet (a server that just started, or no sampler).
pub fn pss_of(pid: u32) -> Option<u64> {
    published(Some(pid)).and_then(|m| m.pss_kb).or_else(|| pss_direct(pid))
}

/// Anonymous + shmem resident memory in kB of the editor (published, else read now): for
/// threshold checks, not for display.
pub fn own_resident_kb() -> Option<u64> {
    published(None).and_then(|m| m.resident_kb).or_else(|| resident_direct(std::process::id()))
}

/// Anonymous + shmem resident memory in kB of another process (published, else read now): for
/// threshold checks, not for display.
pub fn resident_of(pid: u32) -> Option<u64> {
    published(Some(pid)).and_then(|m| m.resident_kb).or_else(|| resident_direct(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_is_woken_only_by_a_visible_change() {
        let a = [(0, 100 * 1024), (7, 400 * 1024)];
        assert!(display_changed(&[], &a), "first sample");
        assert!(!display_changed(&a, &a));
        assert!(!display_changed(&a, &[(0, 100 * 1024 + 1024), (7, 400 * 1024 - 900)]), "within a MB");
        assert!(display_changed(&a, &[(0, 100 * 1024), (7, 402 * 1024)]), "a server grew 2 MB");
        assert!(display_changed(&a, &[(0, 100 * 1024), (8, 400 * 1024)]), "another server");
        assert!(display_changed(&a, &[(0, 100 * 1024)]), "a server went away");
    }

    #[test]
    fn sample_lists_the_editor_first_and_skips_dead_processes() {
        let s = Sample {
            own: ProcMem { pss_kb: Some(10), resident_kb: Some(9) },
            procs: vec![
                (5, ProcMem { pss_kb: None, resident_kb: Some(70) }),
                (6, ProcMem { pss_kb: Some(30), resident_kb: None }),
            ],
        };
        assert_eq!(displayed(&s), vec![(0, 10), (6, 30)]);
        assert_eq!(displayed(&Sample::default()), Vec::new());
    }

    #[test]
    fn measuring_reads_only_what_was_asked() {
        let me = std::process::id();
        let off = Want { pids: vec![me], pss: false, resident: false };
        assert!(off.idle());
        let s = measure(&off);
        assert_eq!((s.own, s.procs), (ProcMem::default(), vec![(me, ProcMem::default())]));
        let on = Want { pids: vec![me], pss: true, resident: false };
        assert!(!on.idle());
        let s = measure(&on);
        // Where the OS offers no number (unsupported platform) both stay empty.
        assert_eq!(s.own.pss_kb.is_some(), s.procs[0].1.pss_kb.is_some());
        assert_eq!((s.own.resident_kb, s.procs[0].1.resident_kb), (None, None));
        if let Some(kb) = s.procs[0].1.pss_kb {
            assert!(kb > 0);
        }
        // The guard alone needs no PSS: no `smaps_rollup` is read for it.
        let cheap = Want { pids: vec![me], pss: false, resident: true };
        assert!(!cheap.idle());
        let s = measure(&cheap);
        assert_eq!((s.own.pss_kb, s.procs[0].1.pss_kb), (None, None));
        assert_eq!(s.own.resident_kb.is_some(), s.procs[0].1.resident_kb.is_some());
        assert!(displayed(&s).is_empty(), "nothing displayed changes: the UI is not woken");
    }

    #[test]
    fn resident_memory_is_anonymous_plus_shmem() {
        let status = "Name:\ttsgo\nVmRSS:\t 2100000 kB\nRssAnon:\t 2000000 kB\nRssFile:\t   90000 kB\n\
                      RssShmem:\t   10000 kB\nVmData:\t 9999999 kB\n";
        assert_eq!(status_resident_kb(status), Some(2_010_000), "file-backed pages are left out");
        assert_eq!(status_resident_kb("RssAnon:\t 5 kB\n"), Some(5));
        assert_eq!(status_resident_kb("Name:\tkthreadd\n"), None, "no memory fields");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resident_memory_of_this_process_is_within_its_rss() {
        let me = std::process::id();
        let (res, snap) = (resident_direct(me).unwrap(), snapshot().unwrap());
        // Two readings a moment apart while other tests allocate: allow some slack.
        assert!(res > 0 && res <= snap.rss_kb + 64 * 1024, "anon + shmem {res} kB, RSS {} kB", snap.rss_kb);
        assert_eq!(resident_of(me).is_some(), own_resident_kb().is_some());
        assert_eq!(resident_direct(u32::MAX), None, "no such process");
    }

    #[test]
    fn without_a_sampler_readers_measure_directly() {
        // Tests never start the thread: nothing is published and the readers fall back.
        watch([1u32].into_iter(), true, true, || Arc::new(|| {}));
        assert_eq!(generation(), None);
        assert_eq!(published(None), None);
        assert_eq!(own_pss_kb().is_some(), snapshot().is_some());
        assert_eq!(pss_of(std::process::id()).is_some(), snapshot().is_some());
    }
}
