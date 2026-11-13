//! Startup timeline (`lumen --profile-startup`) and the switch that moves non-essential work behind
//! the first frame. Marks are a handful of `Instant` reads (microseconds), so they are always recorded;
//! the report is only printed when profiling was asked for.

use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

static START: OnceLock<Instant> = OnceLock::new();
static MARKS: Mutex<Vec<(&'static str, f64)>> = Mutex::new(Vec::new());
static PROFILE: AtomicBool = AtomicBool::new(false);
static DEFER: AtomicBool = AtomicBool::new(false);
static PRE_MAIN_MS: Mutex<Option<f64>> = Mutex::new(None);

/// Call first thing in `main`: fixes the clock origin.
pub fn begin() {
    let _ = START.set(Instant::now());
    *PRE_MAIN_MS.lock().unwrap() = process_age_ms();
}

pub fn set_profile(on: bool) {
    PROFILE.store(on, Ordering::Relaxed);
}

pub fn profiling() -> bool {
    PROFILE.load(Ordering::Relaxed)
}

/// Services that are not needed to draw the first frame (file watcher, IDE bridge, recents) start after it.
/// Off by default so in-process tests keep the eager behaviour.
pub fn set_deferred(on: bool) {
    DEFER.store(on, Ordering::Relaxed);
}

pub fn deferred() -> bool {
    DEFER.load(Ordering::Relaxed)
}

/// Record that `name` happened now. Only the first mark of a name is kept.
pub fn mark(name: &'static str) {
    let Some(start) = START.get() else { return };
    let t = start.elapsed().as_secs_f64() * 1e3;
    let mut m = MARKS.lock().unwrap();
    if !m.iter().any(|(n, _)| *n == name) {
        m.push((name, t));
    }
}

/// Wall time between the kernel creating the process and `begin` (dynamic linking, runtime init).
#[cfg(target_os = "linux")]
fn process_age_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the parenthesised command name; starttime is field 22 overall = index 19 after ") ".
    let rest = stat.rsplit_once(") ")?.1;
    let start_ticks: f64 = rest.split(' ').nth(19)?.parse().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime").ok()?.split(' ').next()?.parse().ok()?;
    let age = (uptime - start_ticks / hz) * 1e3;
    // Clock-tick resolution is 10 ms: report only plausible values.
    (0.0..5000.0).contains(&age).then_some(age)
}

#[cfg(not(target_os = "linux"))]
fn process_age_ms() -> Option<f64> {
    None
}

/// The timeline as text: absolute time since `main`, and the step that led to each mark.
pub fn format(marks: &[(&str, f64)], pre_main_ms: Option<f64>) -> String {
    let mut out = String::from("[startup] timeline (ms since main)\n");
    if let Some(p) = pre_main_ms {
        out.push_str(&format!("[startup] process start -> main: ~{p:.0} ms (10 ms clock ticks)\n"));
    }
    let mut prev = 0.0;
    for (name, t) in marks {
        out.push_str(&format!("[startup] {t:>8.1}  +{:>7.1}  {name}\n", t - prev));
        prev = *t;
    }
    out
}

/// The first frame was drawn: record it and, when profiling, print the timeline.
pub fn first_frame() {
    mark("first frame");
    report();
}

/// Print the timeline once, when profiling.
pub fn report() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if !profiling() || DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let marks = MARKS.lock().unwrap().clone();
    let pre = *PRE_MAIN_MS.lock().unwrap();
    eprint!("{}", format(&marks, pre));
}

/// Time of a recorded mark, in ms since `main`.
#[cfg(test)]
pub fn at(name: &str) -> Option<f64> {
    MARKS.lock().unwrap().iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_lists_steps_with_deltas() {
        let s = format(&[("a", 10.0), ("b", 25.5)], Some(12.0));
        assert!(s.contains("process start -> main: ~12 ms"));
        assert!(s.contains("10.0  +   10.0  a"), "{s}");
        assert!(s.contains("25.5  +   15.5  b"), "{s}");
    }

    #[test]
    fn marks_keep_first_occurrence_and_are_monotonic() {
        begin();
        mark("t-one");
        mark("t-two");
        mark("t-one");
        let (a, b) = (at("t-one").unwrap(), at("t-two").unwrap());
        assert!(a <= b);
        assert_eq!(MARKS.lock().unwrap().iter().filter(|(n, _)| *n == "t-one").count(), 1);
    }
}
