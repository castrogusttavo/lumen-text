//! Resource panel model: which processes Lumen runs (editor, language servers, terminals, AI
//! agents, debug adapter, task, Build & Run), what they cost and the soft memory budget. Pure
//! logic and formatting; the app glue is `app_resmon`. Nothing here samples by itself: the
//! panel measures only while it is open and the budget check only runs when a budget is set.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Title of the panel (the tick recognises the open panel by it).
pub const TITLE: &str = "Resources";
/// Panel refresh period while open.
pub const REFRESH: Duration = Duration::from_secs(2);
/// Budget check period (only when a budget is set).
pub const BUDGET_EVERY: Duration = Duration::from_secs(10);
/// The warning re-arms once the total falls below this share of the budget.
const REARM: f64 = 0.9;
const MAX_BUDGET_MB: u32 = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Editor,
    Lsp,
    Terminal,
    Agent,
    Debug,
    Task,
    Build,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Editor => "Editor",
            Kind::Lsp => "Language server",
            Kind::Terminal => "Terminal",
            Kind::Agent => "AI agent",
            Kind::Debug => "Debug adapter",
            Kind::Task => "Task",
            Kind::Build => "Build & Run",
        }
    }
}

/// What a row acts on. Servers are keyed by pid (the list may shift between refreshes).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    Editor,
    Lsp(u32),
    Term(u32),
    Agent(u64),
    Debug,
    Task,
    Build,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub kind: Kind,
    pub name: String,
    pub target: Target,
    pub pss_kb: u64,
    pub rss_kb: u64,
    pub up: Option<Duration>,
    /// A server with no document open (safe to stop).
    pub idle: bool,
}

/// Actions of the panel's rows and menus.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ResPick {
    /// Nothing to do (header rows).
    Noop,
    /// Open the Stop / Restart menu of a row.
    Menu(Target),
    Stop(Target),
    Restart(Target),
    /// Stop every idle language server.
    StopIdle,
    /// Back to the list.
    Back,
}

/// Panel and budget state kept in the app.
pub struct State {
    pub started: Instant,
    /// Next panel refresh (set while the panel is open).
    pub next_refresh: Option<Instant>,
    pub next_budget: Instant,
    /// The total is over the budget (drives the badge).
    pub over: bool,
    /// The toast for the current excess was shown.
    pub warned: bool,
}

impl State {
    pub fn new() -> Self {
        let now = Instant::now();
        State { started: now, next_refresh: None, next_budget: now + BUDGET_EVERY, over: false, warned: false }
    }

    /// Feed one measurement; true when the warning must be shown now (first time over, again only
    /// after the total dropped well below the budget). `budget_mb == 0` switches everything off.
    pub fn observe(&mut self, total_kb: u64, budget_mb: u32) -> bool {
        if budget_mb == 0 {
            self.over = false;
            self.warned = false;
            return false;
        }
        let total_mb = total_kb as f64 / 1024.0;
        let budget = f64::from(budget_mb);
        if total_mb > budget {
            self.over = true;
            let fresh = !self.warned;
            self.warned = true;
            return fresh;
        }
        if total_mb < budget * REARM {
            self.over = false;
            self.warned = false;
        }
        false
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// The `mem_budget_mb` setting: 0 (off) up to a sane cap; anything unusable is off.
pub fn clamp_budget(v: Option<f64>) -> u32 {
    match v {
        Some(f) if f.is_finite() && f > 0.0 => (f.round() as u64).min(u64::from(MAX_BUDGET_MB)) as u32,
        _ => 0,
    }
}

/// "812 MB", "1.4 GB".
pub fn fmt_kb(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

/// "42s", "7m", "2h 05m".
pub fn fmt_uptime(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Detail text of a row: "PSS 120 MB · RSS 150 MB · up 7m · idle".
pub fn row_detail(r: &Row) -> String {
    let mut s = format!("PSS {} · RSS {}", fmt_kb(r.pss_kb), fmt_kb(r.rss_kb));
    if let Some(u) = r.up {
        s.push_str(&format!(" · up {}", fmt_uptime(u)));
    }
    if r.idle {
        s.push_str(" · idle");
    }
    s
}

/// Parent -> children map from `ps -A -o pid= -o ppid=` output.
pub fn parse_ps(text: &str) -> HashMap<u32, Vec<u32>> {
    let mut m: HashMap<u32, Vec<u32>> = HashMap::new();
    for l in text.lines() {
        let mut it = l.split_whitespace();
        if let (Some(Ok(pid)), Some(Ok(ppid))) = (it.next().map(str::parse::<u32>), it.next().map(str::parse::<u32>)) {
            m.entry(ppid).or_default().push(pid);
        }
    }
    m
}

/// `root` and everything below it (cycle-safe, capped).
pub fn subtree(children: &HashMap<u32, Vec<u32>>, root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() && out.len() < 512 {
        if let Some(cs) = children.get(&out[i]) {
            for c in cs {
                if !out.contains(c) {
                    out.push(*c);
                }
            }
        }
        i += 1;
    }
    out
}

/// The process tree of the machine (one `ps`); empty where unavailable.
#[cfg(unix)]
pub fn process_tree() -> HashMap<u32, Vec<u32>> {
    match std::process::Command::new("ps").args(["-A", "-o", "pid=", "-o", "ppid="]).output() {
        Ok(o) => parse_ps(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => HashMap::new(),
    }
}

#[cfg(not(unix))]
pub fn process_tree() -> HashMap<u32, Vec<u32>> {
    HashMap::new()
}

/// Summed (PSS, RSS) in kB of `pid` and its descendants.
pub fn tree_mem(children: &HashMap<u32, Vec<u32>>, pid: u32) -> (u64, u64) {
    subtree(children, pid)
        .into_iter()
        .filter_map(crate::mem::mem_of)
        .fold((0, 0), |a, m| (a.0 + m.pss_kb, a.1 + m.rss_kb))
}

/// Sum of the PSS of all rows.
pub fn total_kb(rows: &[Row]) -> u64 {
    rows.iter().map(|r| r.pss_kb).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: Kind, pss: u64, idle: bool) -> Row {
        Row { kind, name: "x".into(), target: Target::Editor, pss_kb: pss, rss_kb: pss, up: None, idle }
    }

    #[test]
    fn formats_sizes_and_uptime() {
        assert_eq!(fmt_kb(120 * 1024), "120 MB");
        assert_eq!(fmt_kb(1536 * 1024), "1.5 GB");
        assert_eq!(fmt_uptime(Duration::from_secs(42)), "42s");
        assert_eq!(fmt_uptime(Duration::from_secs(7 * 60 + 5)), "7m");
        assert_eq!(fmt_uptime(Duration::from_secs(2 * 3600 + 5 * 60)), "2h 05m");
    }

    #[test]
    fn detail_lists_memory_uptime_and_idle() {
        let mut r = row(Kind::Lsp, 100 * 1024, true);
        r.up = Some(Duration::from_secs(120));
        assert_eq!(row_detail(&r), "PSS 100 MB · RSS 100 MB · up 2m · idle");
        assert_eq!(row_detail(&row(Kind::Editor, 1024, false)), "PSS 1 MB · RSS 1 MB");
    }

    #[test]
    fn process_tree_is_walked_with_descendants() {
        let m = parse_ps("  1 0\n 10 1\n 11 10\n 12 11\n 20 1\n junk\n");
        let mut t = subtree(&m, 10);
        t.sort_unstable();
        assert_eq!(t, vec![10, 11, 12]);
        assert_eq!(subtree(&m, 99), vec![99]);
    }

    #[test]
    fn subtree_survives_a_cycle() {
        let m = parse_ps("1 2\n2 1\n");
        assert_eq!(subtree(&m, 1).len(), 2);
    }

    #[test]
    fn budget_setting_is_sanitised() {
        assert_eq!(clamp_budget(None), 0);
        assert_eq!(clamp_budget(Some(-5.0)), 0);
        assert_eq!(clamp_budget(Some(f64::NAN)), 0);
        assert_eq!(clamp_budget(Some(1024.4)), 1024);
        assert_eq!(clamp_budget(Some(1e12)), MAX_BUDGET_MB);
    }

    #[test]
    fn warning_fires_once_and_rearms_below_the_budget() {
        let mut s = State::new();
        assert!(!s.observe(5000 * 1024, 0), "off");
        assert!(!s.over);
        assert!(!s.observe(500 * 1024, 1000));
        assert!(s.observe(1200 * 1024, 1000), "crossing warns");
        assert!(s.over);
        assert!(!s.observe(1300 * 1024, 1000), "no repeat");
        assert!(!s.observe(950 * 1024, 1000), "between 90 % and 100 % stays armed off");
        assert!(s.warned);
        assert!(!s.observe(800 * 1024, 1000));
        assert!(!s.over);
        assert!(s.observe(1100 * 1024, 1000), "warns again after dropping");
    }

    #[test]
    fn total_sums_pss() {
        assert_eq!(total_kb(&[row(Kind::Editor, 10, false), row(Kind::Lsp, 5, true)]), 15);
    }

    #[test]
    fn labels_are_distinct() {
        let kinds = [Kind::Editor, Kind::Lsp, Kind::Terminal, Kind::Agent, Kind::Debug, Kind::Task, Kind::Build];
        let mut l: Vec<_> = kinds.iter().map(|k| k.label()).collect();
        l.sort_unstable();
        l.dedup();
        assert_eq!(l.len(), kinds.len());
    }
}
