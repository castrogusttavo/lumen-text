//! Keeps tsgo's memory in check. Type-checking a whole big project is intrinsically expensive
//! (a 1,800-file Next.js app with ~160 dependencies reaches ~2 GB), so big projects start in lean
//! mode (open files + their imports) and a running full-project server that grows past the budget
//! is restarted in lean mode.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// A full-project tsgo above this many MB (PSS) is restarted in lean mode, unless the machine has
/// plenty of RAM (see `budget_mb`).
pub const BUDGET_MB: u64 = 1000;
/// Machines with at least this much RAM never start lean on their own: a big project fits comfortably.
const ROOMY_MB: u64 = 12 * 1024;
/// Projects with more TS/JS source files than this start lean.
pub const HEAVY_FILES: usize = 400;
/// Projects declaring at least this many dependencies start lean (their type packages are huge).
pub const HEAVY_DEPS: usize = 150;
const CHECK_EVERY: Duration = Duration::from_secs(5);
const STRIKES: u8 = 2;

/// Total physical RAM in MB (Linux; elsewhere unknown, treated as roomy).
pub fn total_ram_mb() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| l.strip_prefix("MemTotal:")?.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(u64::MAX, |kb| kb / 1024)
}

/// Memory a full-project server may use: 1 GB on small machines, 20 % of the RAM on bigger ones.
pub fn budget_mb(total_mb: u64) -> u64 {
    BUDGET_MB.max(total_mb / 5)
}

pub struct Guard {
    /// Roots that must run lean for the rest of the session.
    pub lean: HashSet<PathBuf>,
    last: Instant,
    strikes: u8,
}

impl Guard {
    pub fn new() -> Self {
        Guard { lean: HashSet::new(), last: Instant::now(), strikes: 0 }
    }

    /// True when the periodic check is due; resets the timer.
    pub fn due(&mut self) -> bool {
        if self.last.elapsed() < CHECK_EVERY {
            return false;
        }
        self.last = Instant::now();
        true
    }

    /// Record one sample of a full-project server; true after `STRIKES` consecutive over-budget samples.
    pub fn over_budget(&mut self, pss_mb: u64, budget: u64) -> bool {
        if pss_mb > budget {
            self.strikes += 1;
        } else {
            self.strikes = 0;
        }
        if self.strikes >= STRIKES {
            self.strikes = 0;
            return true;
        }
        false
    }
}

impl Default for Guard {
    fn default() -> Self {
        Self::new()
    }
}

/// Number of TS/JS source files under `root` (honouring .gitignore), stopping at `cap`.
pub fn count_sources(root: &Path, cap: usize) -> usize {
    let mut n = 0;
    for e in ignore::WalkBuilder::new(root).build().flatten() {
        let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or("");
        if matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs")
            && !e.path().to_string_lossy().ends_with(".d.ts")
        {
            n += 1;
            if n > cap {
                break;
            }
        }
    }
    n
}

/// Dependencies + devDependencies declared by the project's package.json.
pub fn count_deps(root: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(root.join("package.json")) else { return 0 };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return 0 };
    ["dependencies", "devDependencies"].iter().filter_map(|k| v[*k].as_object()).map(|o| o.len()).sum()
}

/// Why `root` should start lean, or None when full-project mode is affordable.
pub fn heavy_reason(root: &Path) -> Option<String> {
    heavy_reason_on(root, total_ram_mb())
}

fn heavy_reason_on(root: &Path, total_mb: u64) -> Option<String> {
    if total_mb >= ROOMY_MB {
        return None;
    }
    let files = count_sources(root, HEAVY_FILES);
    if files > HEAVY_FILES {
        return Some(format!("over {HEAVY_FILES} source files"));
    }
    let deps = count_deps(root);
    (deps >= HEAVY_DEPS).then(|| format!("{deps} dependencies"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_project_stays_full() {
        let d = std::env::temp_dir().join(format!("lumen-guard-{}", std::process::id()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.ts"), "").unwrap();
        std::fs::write(d.join("package.json"), r#"{"dependencies":{"a":"1"}}"#).unwrap();
        assert_eq!(heavy_reason_on(&d, 7_600), None);
        assert_eq!(count_deps(&d), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn many_files_or_deps_start_lean() {
        let d = std::env::temp_dir().join(format!("lumen-guard-big-{}", std::process::id()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        for i in 0..=HEAVY_FILES {
            std::fs::write(d.join(format!("src/f{i}.ts")), "").unwrap();
        }
        assert!(heavy_reason_on(&d, 7_600).unwrap().contains("source files"));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        let deps: String = (0..HEAVY_DEPS).map(|i| format!("\"p{i}\":\"1\"")).collect::<Vec<_>>().join(",");
        std::fs::write(d.join("package.json"), format!("{{\"dependencies\":{{{deps}}}}}")).unwrap();
        assert!(heavy_reason_on(&d, 7_600).unwrap().contains("dependencies"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn over_budget_needs_consecutive_samples() {
        let mut g = Guard::new();
        assert!(!g.over_budget(BUDGET_MB + 1, BUDGET_MB));
        assert!(!g.over_budget(100, BUDGET_MB));
        assert!(!g.over_budget(BUDGET_MB + 1, BUDGET_MB));
        assert!(g.over_budget(BUDGET_MB + 1, BUDGET_MB));
    }

    #[test]
    fn roomy_machines_never_start_lean_and_get_a_bigger_budget() {
        let d = std::env::temp_dir().join(format!("lumen-guard-roomy-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for i in 0..=HEAVY_FILES {
            std::fs::write(d.join(format!("f{i}.ts")), "").unwrap();
        }
        assert!(heavy_reason_on(&d, 7_600).is_some());
        assert_eq!(heavy_reason_on(&d, 16_000), None);
        assert_eq!(budget_mb(4_000), BUDGET_MB);
        assert_eq!(budget_mb(7_600), 1_520);
        assert_eq!(budget_mb(16_000), 3_200);
        std::fs::remove_dir_all(&d).ok();
    }
}
