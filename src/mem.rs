//! Memory measurement. PSS (proportional set size) is the honest number:
//! RSS double-counts shared libraries (GPU drivers, libc, ...).

#[cfg(target_os = "linux")]
use std::fs;

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

/// PSS in kB of another process (e.g. a language server we spawned).
#[cfg(not(target_os = "linux"))]
pub fn pss_of(pid: u32) -> Option<u64> {
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

#[cfg(target_os = "linux")]
pub fn pss_of(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    text.lines().find(|l| l.starts_with("Pss:"))?.split_whitespace().nth(1)?.parse().ok()
}
