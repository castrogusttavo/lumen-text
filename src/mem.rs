//! Memory measurement. PSS (proportional set size) is the honest number:
//! RSS double-counts shared libraries (GPU drivers, libc, ...).

use std::fs;

#[derive(Debug, Clone, Copy)]
pub struct MemSnapshot {
    pub pss_kb: u64,
    pub rss_kb: u64,
}

pub fn snapshot() -> Option<MemSnapshot> {
    let text = fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    let field = |name: &str| -> Option<u64> {
        text.lines()
            .find(|l| l.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    Some(MemSnapshot { pss_kb: field("Pss:")?, rss_kb: field("Rss:")? })
}

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
pub fn pss_of(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    text.lines().find(|l| l.starts_with("Pss:"))?.split_whitespace().nth(1)?.parse().ok()
}
