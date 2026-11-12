//! The Tests panel: run the project's tests, watch the results arrive as a tree, jump to the
//! failures, re-run what broke and debug a single test. Parsing lives in `testrun.rs`.

use crate::{
    app::{App, Focus, KeyIn},
    app_build::{load_target, save_target},
    app_gitpanel::fit,
    app_overlay::{Action, Item},
    app_panel::PanelTab,
    scrollreg::Reg,
    tasks::{self, Ansi, Event, Running, Segs},
    testrun::{self, Case, Collector, Counts, Plan, Runner, Scope, ScriptTarget, Status},
    theme::Rgba,
    ui::{Canvas, Rect, TextCtx, line_h},
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use winit::keyboard::{Key, NamedKey};

const TOOLBAR_H: i32 = 40;
const PROG_H: i32 = 3;
const FILTER_H: i32 = 32;
/// Height of a test row; a group row adds `GROUP_GAP` of air above it (no separator lines).
const ROW_H: i32 = 30;
const GROUP_GAP: i32 = 4;
const GROUP_H: i32 = ROW_H + GROUP_GAP;
/// Padding above the first and below the last row of the list.
const LIST_PAD: i32 = 2;
const DETAIL_HEAD: i32 = 100;
/// Buttons, chips and the detail badge: same height and corner radius as the git panel's pills.
const BTN_H: i32 = 26;
const CHIP_H: i32 = 24;
const RAD: i32 = 8;
/// Secondary text (folders, describe prefixes, durations, meta).
const SMALL: f32 = 12.0;
const MIN_PANEL_H: i32 = 340;
const MAX_RAW: usize = 6000;
const BAR_W: i32 = 11;
/// Id of the placeholder shown when a run produced no tests at all (never a real test).
const RUN_FAILED: &str = "run::failed";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Failed,
    Passed,
    Skipped,
}

impl Filter {
    fn accepts(self, s: Status) -> bool {
        match self {
            Filter::All => true,
            Filter::Failed => s == Status::Failed,
            Filter::Passed => s == Status::Passed,
            Filter::Skipped => s == Status::Skipped,
        }
    }
}

/// What the last run covered, so "run again" and watch mode can repeat it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Last {
    All,
    File(String),
    Test { file: Option<String>, name: String, id: Option<String> },
    Failed,
}

/// Things of the toolbar that explain themselves on hover (disabled ones included).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tip {
    Target,
    Failed,
}

#[derive(Clone, Debug)]
pub enum Hit {
    RunAll,
    RunFailed,
    Stop,
    Watch,
    Runner,
    Filter(Filter),
    Fold(bool),
    Group(String),
    Case(String),
    RunOne(String),
    DebugOne(String),
    OpenOne(String),
    ShowOutput(bool),
    Link(String, usize, usize),
}

enum Mode {
    Run,
    /// Building the test binaries so gdb can be started on the one that has this test id.
    DebugBuild(String),
}

pub struct TestsState {
    pub detected: Vec<Runner>,
    detected_root: Option<PathBuf>,
    pub runner: Option<Runner>,
    /// The project's package.json test scripts (see `testrun::script_targets`).
    pub scripts: Vec<ScriptTarget>,
    /// Id of the chosen script target; `None`: Auto (the detected runner, `runner`).
    pub target: Option<String>,
    /// The current / last run is a log-only script: no tree, only its output and exit code.
    pub log_only: bool,
    /// Folder the current / last run ran in (a script's package).
    run_dir: PathBuf,
    pub tips: Vec<(Rect, Tip)>,
    pub cases: Vec<Case>,
    idx: HashMap<String, usize>,
    proc: Option<Running>,
    plan: Option<Plan>,
    collector: Option<Collector>,
    mode: Mode,
    pub started: Option<Instant>,
    started_sys: Option<SystemTime>,
    pub last_ms: Option<f64>,
    pub exit: Option<i32>,
    pub last: Option<Last>,
    pub raw: Vec<Segs>,
    pub raw_plain: Vec<String>,
    raw_ansi: Ansi,
    pub filter: Filter,
    pub collapsed: HashSet<String>,
    pub sel: Option<String>,
    pub scroll: f64,
    pub detail_scroll: f64,
    pub show_output: bool,
    pub hits: Vec<(Rect, Hit)>,
    seq: u32,
    pub watch_due: Option<Instant>,
    dbg_json: Vec<String>,
    last_click: Option<(String, Instant)>,
    /// The list has the keyboard (set by a click in the panel, dropped by any other click or
    /// by a key the list does not use).
    pub focused: bool,
    /// Keyboard cursor parked on a group row; `None` means it is on the selected test.
    pub cur_group: Option<String>,
    /// Height of the list viewport at the last draw (for keeping the cursor in view).
    list_view: i32,
    /// Coverage: the loaded report and whether runs collect it (see `app_coverage`).
    pub cov: crate::app_coverage::CovState,
}

impl Default for TestsState {
    fn default() -> Self {
        Self {
            detected: Vec::new(),
            detected_root: None,
            runner: None,
            scripts: Vec::new(),
            target: None,
            log_only: false,
            run_dir: PathBuf::new(),
            tips: Vec::new(),
            cases: Vec::new(),
            idx: HashMap::new(),
            proc: None,
            plan: None,
            collector: None,
            mode: Mode::Run,
            started: None,
            started_sys: None,
            last_ms: None,
            exit: None,
            last: None,
            raw: Vec::new(),
            raw_plain: Vec::new(),
            raw_ansi: Ansi::new(),
            filter: Filter::All,
            collapsed: HashSet::new(),
            sel: None,
            scroll: 0.0,
            detail_scroll: 0.0,
            show_output: false,
            hits: Vec::new(),
            seq: 0,
            watch_due: None,
            dbg_json: Vec::new(),
            last_click: None,
            focused: false,
            cur_group: None,
            list_view: 0,
            cov: Default::default(),
        }
    }
}

impl TestsState {
    /// Forget everything about the previous project.
    pub fn reset(&mut self) {
        if let Some(mut r) = self.proc.take() {
            tasks::kill_group(r.pid);
            let _ = r.child.wait();
        }
        if let Some(p) = self.plan.take() {
            p.remove_temp();
        }
        *self = TestsState::default();
    }

    /// Drop the "the run failed" placeholder; a new run replaces it.
    fn drop_placeholder(&mut self) {
        if self.idx.contains_key(RUN_FAILED) {
            self.cases.retain(|c| c.id != RUN_FAILED);
            self.idx = self.cases.iter().enumerate().map(|(i, c)| (c.id.clone(), i)).collect();
        }
    }

    fn real_failures(&self) -> Vec<Case> {
        self.cases.iter().filter(|c| c.status == Status::Failed && c.id != RUN_FAILED).cloned().collect()
    }

    fn clear_cases(&mut self) {
        self.cases.clear();
        self.idx.clear();
    }

    fn upsert(&mut self, c: Case) {
        match self.idx.get(&c.id).copied() {
            Some(i) => {
                let old = &self.cases[i];
                let mut c = c;
                if c.file.is_none() {
                    c.file = old.file.clone();
                }
                if c.line.is_none() {
                    c.line = old.line;
                }
                self.cases[i] = c;
            }
            None => {
                self.idx.insert(c.id.clone(), self.cases.len());
                self.cases.push(c);
            }
        }
    }

    fn case(&self, id: &str) -> Option<&Case> {
        self.idx.get(id).and_then(|&i| self.cases.get(i))
    }

    /// The chosen script target (`None`: Auto).
    pub fn target(&self) -> Option<&ScriptTarget> {
        let id = self.target.as_deref()?;
        self.scripts.iter().find(|t| t.id == id)
    }

    /// Is the chosen target a script whose results cannot be parsed?
    pub fn log_only_target(&self) -> bool {
        self.target().is_some_and(|t| t.run.is_none())
    }
}

/// The remembered target, if the project still has it (`""` or a vanished script: Auto).
pub fn restore_target(saved: Option<&str>, scripts: &[ScriptTarget]) -> Option<String> {
    saved.filter(|id| scripts.iter().any(|t| t.id == *id)).map(str::to_string)
}

/// Where the chosen test target of a project is remembered (`None` in tests: nothing is written).
fn state_path(root: &Path) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let base = crate::paths::state_home()?;
    let h =
        root.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    Some(base.join("lumen-text").join(format!("tests-{h:016x}.json")))
}

/// A file name whose icon stands for the runner (`None`: a generic one).
fn runner_marker(r: Runner) -> Option<&'static str> {
    Some(match r {
        Runner::Custom => return None,
        Runner::Nextest | Runner::Cargo => "Cargo.toml",
        Runner::Vitest | Runner::Jest | Runner::Playwright | Runner::NodeTest | Runner::Bun => "package.json",
        Runner::Deno => "deno.json",
        Runner::Go => "go.mod",
        Runner::Pytest => "conftest.py",
        Runner::Phpunit | Runner::Pest => "composer.json",
        Runner::Rspec => "Gemfile",
        Runner::Maven => "pom.xml",
        Runner::Gradle => "build.gradle",
        Runner::Dotnet => "App.csproj",
        Runner::Ctest => "CMakeLists.txt",
    })
}

/// One line of the test target picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub detail: String,
    /// What picking it selects: Auto with a runner, or a script target (its id).
    pub pick: Pick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Auto(Runner),
    Script(String),
}

/// The test target picker, in order: Auto (one entry per detected runner), the scripts that run
/// with full results, then the log-only ones.
pub fn target_entries(
    detected: &[Runner],
    runner: Option<Runner>,
    scripts: &[ScriptTarget],
    cur: Option<&str>,
) -> Vec<Entry> {
    let mut v = Vec::new();
    let mark = |d: String, on: bool| if on { format!("{d} · current") } else { d };
    for r in detected {
        let what = if *r == Runner::Custom { "test_command from the settings" } else { "detected runner" };
        let detail = mark(what.into(), cur.is_none() && runner == Some(*r));
        v.push(Entry { label: format!("Auto ({})", r.label()), detail, pick: Pick::Auto(*r) });
    }
    let place = |t: &ScriptTarget| if t.rel.is_empty() { String::new() } else { format!(" · {}", t.rel) };
    for t in scripts.iter().filter(|t| t.run.is_some()) {
        let r = t.runner().map(|r| r.label()).unwrap_or_default();
        let detail = mark(format!("{r} · {}{}", t.command, place(t)), cur == Some(t.id.as_str()));
        v.push(Entry { label: t.name.clone(), detail, pick: Pick::Script(t.id.clone()) });
    }
    for t in scripts.iter().filter(|t| t.run.is_none()) {
        let detail = mark(format!("log only · {}{}", t.command, place(t)), cur == Some(t.id.as_str()));
        v.push(Entry { label: t.name.clone(), detail, pick: Pick::Script(t.id.clone()) });
    }
    v
}

// ───────────── rows ─────────────

pub enum Row {
    Group { key: String, pass: usize, fail: usize, skip: usize, run: usize, total: usize, ms: f64 },
    Case(usize),
}

/// The tree as it is listed: groups (failing ones first once the run is over), their cases
/// unless folded, honoring the status filter.
pub fn build_rows(cases: &[Case], filter: Filter, collapsed: &HashSet<String>, failures_first: bool) -> Vec<Row> {
    let mut order: Vec<String> = Vec::new();
    let mut by: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, c) in cases.iter().enumerate() {
        by.entry(c.group.as_str())
            .or_insert_with(|| {
                order.push(c.group.clone());
                Vec::new()
            })
            .push(i);
    }
    let failed = |g: &String| by[g.as_str()].iter().any(|&i| cases[i].status == Status::Failed);
    if failures_first {
        order.sort_by_key(|g| !failed(g));
    }
    let mut rows = Vec::new();
    for g in &order {
        let ids = &by[g.as_str()];
        let mut visible: Vec<usize> = ids.iter().copied().filter(|&i| filter.accepts(cases[i].status)).collect();
        if failures_first {
            visible.sort_by_key(|&i| cases[i].status != Status::Failed);
        }
        if visible.is_empty() {
            continue;
        }
        let (mut pass, mut fail, mut skip, mut run, mut ms) = (0, 0, 0, 0, 0.0);
        for &i in ids {
            match cases[i].status {
                Status::Passed => pass += 1,
                Status::Failed => fail += 1,
                Status::Skipped => skip += 1,
                Status::Running => run += 1,
                Status::Pending => {}
            }
            ms += cases[i].ms.unwrap_or(0.0);
        }
        rows.push(Row::Group { key: g.clone(), pass, fail, skip, run, total: ids.len(), ms });
        if !collapsed.contains(g) {
            rows.extend(visible.into_iter().map(Row::Case));
        }
    }
    rows
}

pub fn fmt_ms(ms: f64) -> String {
    if ms < 1.0 {
        "<1 ms".to_string()
    } else if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 60_000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        let s = (ms / 1000.0) as u64;
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

/// `(name, dir)` of a group header: a file under the project is shown by file name with its folder
/// dimmed; modules, classes and packages are shown as they are.
pub fn group_label(root: &Path, key: &str) -> (String, String) {
    let p = Path::new(key);
    let rel = p.strip_prefix(root).unwrap_or(p);
    if key.contains('/') && !key.contains("::") && rel.extension().is_some() {
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = rel.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        (name, if dir == "." { String::new() } else { dir })
    } else {
        (key.to_string(), String::new())
    }
}

pub fn wrap_line(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(8);
    let s = s.replace('\t', "    ");
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= cols {
        return vec![s];
    }
    chars.chunks(cols).map(|c| c.iter().collect()).collect()
}

pub fn row_h(r: &Row) -> i32 {
    match r {
        Row::Group { .. } => GROUP_H,
        Row::Case(_) => ROW_H,
    }
}

/// Top of every row from the start of the list's content, and the content's total height.
pub fn row_tops(rows: &[Row]) -> (Vec<i32>, i32) {
    let mut y = LIST_PAD;
    let tops = rows
        .iter()
        .map(|r| {
            let t = y;
            y += row_h(r);
            t
        })
        .collect();
    (tops, y + LIST_PAD)
}

/// Scroll offset that brings `[top, top + h)` into a `view` px viewport, moving as little as possible.
pub fn scroll_to_show(scroll: f64, top: i32, h: i32, view: i32) -> f64 {
    if view <= 0 {
        return scroll;
    }
    if (top as f64) < scroll {
        top as f64
    } else if (top + h) as f64 > scroll + view as f64 {
        (top + h - view).max(0) as f64
    } else {
        scroll
    }
}

/// Keyboard moves inside the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nav {
    Up,
    Down,
    PageUp(usize),
    PageDown(usize),
    Home,
    End,
    Left,
    Right,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NavOut {
    /// Put the cursor on row `k`.
    To(usize),
    Fold(String),
    Unfold(String),
    Stay,
}

/// What a navigation key does with the cursor at row `at` (`None`: nothing selected yet).
/// Left on a test climbs to its group, Left on an open group folds it, Right unfolds a folded
/// group and steps into an open one.
pub fn nav(rows: &[Row], cases: &[Case], collapsed: &HashSet<String>, at: Option<usize>, n: Nav) -> NavOut {
    if rows.is_empty() {
        return NavOut::Stay;
    }
    let last = rows.len() - 1;
    let Some(k) = at.filter(|&k| k <= last) else {
        return match n {
            Nav::End => NavOut::To(last),
            Nav::Left | Nav::Right => NavOut::Stay,
            _ => NavOut::To(0),
        };
    };
    let to = |j: usize| if j == k { NavOut::Stay } else { NavOut::To(j) };
    match n {
        Nav::Up => to(k.saturating_sub(1)),
        Nav::Down => to((k + 1).min(last)),
        Nav::PageUp(p) => to(k.saturating_sub(p.max(1))),
        Nav::PageDown(p) => to((k + p.max(1)).min(last)),
        Nav::Home => to(0),
        Nav::End => to(last),
        Nav::Left => match &rows[k] {
            Row::Group { key, .. } if !collapsed.contains(key) => NavOut::Fold(key.clone()),
            Row::Group { .. } => NavOut::Stay,
            Row::Case(i) => {
                let g = &cases[*i].group;
                rows[..k]
                    .iter()
                    .rposition(|r| matches!(r, Row::Group { key, .. } if key == g))
                    .map_or(NavOut::Stay, NavOut::To)
            }
        },
        Nav::Right => match &rows[k] {
            Row::Group { key, .. } if collapsed.contains(key) => NavOut::Unfold(key.clone()),
            Row::Group { .. } if matches!(rows.get(k + 1), Some(Row::Case(_))) => NavOut::To(k + 1),
            _ => NavOut::Stay,
        },
    }
}

/// Row of the keyboard cursor: the group it is parked on, else the selected test.
pub fn cursor_row(rows: &[Row], cases: &[Case], cur_group: Option<&str>, sel: Option<&str>) -> Option<usize> {
    if let Some(g) = cur_group
        && let Some(k) = rows.iter().position(|r| matches!(r, Row::Group { key, .. } if key == g))
    {
        return Some(k);
    }
    let sel = sel?;
    rows.iter().position(|r| matches!(r, Row::Case(i) if cases[*i].id == sel))
}

/// Width of `s` drawn at `size` px (the cached measure is at the UI size; widths scale linearly).
fn sized_w(tx: &mut TextCtx, s: &str, bold: bool, size: f32) -> i32 {
    (tx.ui_width(s, bold) as f32 * size / crate::ui::ui_size()).ceil() as i32
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Primary,
    Secondary,
    Danger,
    Toggle(bool),
}

/// One line of the detail pane.
struct DLine {
    segs: Vec<(String, Rgba)>,
    plain: String,
    bg: Option<Rgba>,
    bar: Option<Rgba>,
}

impl DLine {
    fn text(t: &str, col: Rgba) -> DLine {
        DLine { segs: vec![(t.to_string(), col)], plain: t.to_string(), bg: None, bar: None }
    }
}

impl App {
    // ───────────── detection / runner ─────────────

    fn tests_ensure_detect(&mut self) {
        if self.tests.detected_root.as_deref() == Some(self.root.as_path()) {
            return;
        }
        let saved = state_path(&self.root).and_then(|p| load_target(&p));
        self.tests_detect();
        self.tests.target = restore_target(saved.as_deref(), &self.tests.scripts);
        self.tests_sync_runner();
    }

    /// Look for runners and test scripts again (the files may have changed).
    fn tests_detect(&mut self) {
        self.tests.detected = testrun::detect(&self.root, !self.config.test_command.is_empty());
        self.tests.scripts = testrun::script_targets(&self.root);
        self.tests.detected_root = Some(self.root.clone());
        if self.tests.target().is_none() {
            self.tests.target = None;
        }
        self.tests_sync_runner();
    }

    /// The runner follows a structured target; Auto keeps a detected one.
    fn tests_sync_runner(&mut self) {
        if let Some(r) = self.tests.target().and_then(|t| t.runner()) {
            self.tests.runner = Some(r);
        } else if !self.tests.runner.is_some_and(|r| self.tests.detected.contains(&r)) {
            self.tests.runner = self.tests.detected.first().copied();
        }
    }

    /// Results of another target do not belong to the new one.
    fn tests_forget_results(&mut self) {
        if self.tests.proc.is_some() {
            return;
        }
        self.tests.clear_cases();
        self.tests.sel = None;
        self.tests.cur_group = None;
        self.tests.last = None;
        self.tests.exit = None;
        self.tests.log_only = false;
        self.tests.raw.clear();
        self.tests.raw_plain.clear();
        self.tests_apply_marks();
    }

    fn tests_remember_target(&self) {
        if let Some(p) = state_path(&self.root) {
            save_target(&p, self.tests.target.as_deref().unwrap_or(""));
        }
    }

    /// Auto with runner `r` (picked from the target picker).
    pub fn tests_set_runner(&mut self, r: Runner) {
        self.tests_ensure_detect();
        let changed = self.tests.target.is_some() || self.tests.runner != Some(r);
        self.tests.target = None;
        self.tests.runner = Some(r);
        self.tests_remember_target();
        if changed {
            self.tests_forget_results();
        }
        self.say(format!("tests: Auto ({})", r.label()));
    }

    /// Run the package.json script `id` from now on (every action of the panel honors it).
    pub fn tests_set_target(&mut self, id: &str) {
        self.tests_ensure_detect();
        let Some(t) = self.tests.scripts.iter().find(|t| t.id == id).cloned() else { return };
        let changed = self.tests.target.as_deref() != Some(id);
        self.tests.target = Some(id.to_string());
        self.tests_sync_runner();
        self.tests_remember_target();
        if changed {
            self.tests_forget_results();
        }
        let kind = t.runner().map_or("log only", |r| r.label());
        self.say(format!("tests: {} ({kind})", t.name));
    }

    /// The items of the target picker (see [`target_entries`]).
    pub fn tests_target_items(&self) -> Vec<Item> {
        target_entries(&self.tests.detected, self.tests.runner, &self.tests.scripts, self.tests.target.as_deref())
            .into_iter()
            .map(|e| Item {
                label: e.label,
                detail: e.detail,
                action: match e.pick {
                    Pick::Auto(r) => Action::TestRunner(r),
                    Pick::Script(id) => Action::TestTarget(id),
                },
            })
            .collect()
    }

    /// The test target picker: Auto, then the package.json scripts (structured, then log only).
    pub fn tests_pick_runner(&mut self) {
        self.tests_ensure_detect();
        self.tests_detect();
        let items = self.tests_target_items();
        if items.is_empty() {
            return self
                .say("no test runner or test script found in this project (see test_command in the settings file)");
        }
        self.open_locations("Test target", items);
    }

    pub fn tests_open_panel(&mut self) {
        self.tests_ensure_detect();
        self.panel.open = true;
        self.panel.tab = PanelTab::Tests;
        self.focus = crate::app::Focus::Editor;
        if self.panel.height < MIN_PANEL_H {
            self.panel.height = MIN_PANEL_H;
        }
    }

    pub fn tests_toggle_watch(&mut self) {
        self.config.test_watch = !self.config.test_watch;
        self.config.save();
        let on = self.config.test_watch;
        self.say(if on { "tests: watch on — re-running on save" } else { "tests: watch off" });
    }

    // ───────────── running ─────────────

    pub fn tests_run_all(&mut self) {
        self.tests_start(Last::All);
    }

    /// "Tests: Run with coverage": run everything with the runner's coverage on; later runs keep
    /// collecting it (so the gutter refreshes) until "Tests: Hide coverage".
    pub fn tests_run_coverage(&mut self) {
        self.tests_ensure_detect();
        let runner = match self.tests.target() {
            Some(t) => t.runner(),
            None => self.tests.runner,
        };
        let Some(runner) = runner else {
            return self
                .say("coverage: no structured test runner for this project (log-only scripts cannot collect it)");
        };
        if let Err(e) = testrun::coverage_supported(runner) {
            return self.say(format!("coverage: {e}"));
        }
        self.tests.cov.run = true;
        self.tests_start(Last::All);
    }

    pub fn tests_run_failed(&mut self) {
        if self.tests.real_failures().is_empty() {
            return self.say("tests: nothing failed");
        }
        self.tests_start(Last::Failed);
    }

    /// Run the file in the active tab, or only the test at its cursor.
    pub fn tests_run_file(&mut self, at_cursor: bool) {
        self.tests_ensure_detect();
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(PathBuf::from) else { return };
        let rel = self.rel(&path);
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let (cur, _) = d.buf.line_col(d.ed.head());
        let lines: Vec<String> = (0..d.buf.line_count()).map(|l| d.buf.line(l)).collect();
        let name = if at_cursor { testrun::test_name_at_cursor(&ext, &lines, cur) } else { None };
        let runner = match self.tests.target() {
            Some(t) => match t.runner() {
                Some(r) if r.exts().contains(&ext.as_str()) => Some(r),
                Some(r) => {
                    let msg =
                        format!("tests: {} ({}) does not run .{ext} files — pick another target", t.name, r.label());
                    return self.say(msg);
                }
                None => {
                    let msg = format!(
                        "tests: {} is log only — pick a structured target to run a file or a single test",
                        t.name
                    );
                    return self.say(msg);
                }
            },
            None => testrun::runner_for_ext(&self.tests.detected, &ext).or(self.tests.runner),
        };
        let Some(runner) = runner else {
            return self.say(format!("no test runner detected for .{ext} files"));
        };
        self.tests.runner = Some(runner);
        let last = match name {
            Some(n) => Last::Test { file: Some(rel), name: n, id: None },
            None => {
                if at_cursor {
                    self.say("no test at the cursor: running the whole file");
                }
                Last::File(rel)
            }
        };
        self.tests_start(last);
    }

    pub fn tests_run_case(&mut self, id: &str) {
        let Some(c) = self.tests.case(id).cloned() else { return };
        if c.id == RUN_FAILED {
            // The placeholder is not a test: "run" repeats the whole last run.
            let last = self.tests.last.clone().unwrap_or(Last::All);
            return self.tests_start(last);
        }
        let file = self.tests_case_rel(&c);
        self.tests_start(Last::Test { file, name: c.name.clone(), id: Some(c.id.clone()) });
    }

    fn tests_case_rel(&self, c: &Case) -> Option<String> {
        let f = c.file.as_deref().filter(|f| !f.is_empty())?;
        let p = Path::new(f);
        Some(p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().into_owned())
    }

    fn tests_spawn(&mut self, cmd: &str, cwd: &Path, mode: Mode) -> bool {
        let wake = self.wake();
        match tasks::spawn(cmd, cwd, wake) {
            Ok(r) => {
                let t = &mut self.tests;
                t.proc = Some(r);
                t.mode = mode;
                t.started = Some(Instant::now());
                t.started_sys = Some(SystemTime::now());
                t.exit = None;
                t.raw.clear();
                t.raw_plain.clear();
                t.raw_ansi = Ansi::new();
                t.dbg_json.clear();
                true
            }
            Err(e) => {
                self.say(format!("could not run: {e}"));
                false
            }
        }
    }

    fn tests_start(&mut self, last: Last) {
        self.tests_stop_quiet();
        self.tests_ensure_detect();
        let target = self.tests.target().cloned();
        let runner = self.tests.runner;
        if target.is_none() && runner.is_none() {
            self.tests_open_panel();
            return self.say("no test runner found in this project (see test_command in the settings file)");
        }
        if let Some(t) = target.as_ref().filter(|t| t.run.is_none())
            && last != Last::All
        {
            let msg = format!("tests: {} is log only — it runs as a whole; pick a structured target for this", t.name);
            return self.say(msg);
        }
        let failed = self.tests.real_failures();
        let last = if last == Last::Failed && failed.is_empty() { Last::All } else { last };
        self.tests.drop_placeholder();
        let one = match &last {
            Last::Test { id: Some(id), .. } => self.tests.case(id).cloned(),
            _ => None,
        };
        // A script runs in its package's folder: project-relative files become relative to it.
        let dir = target.as_ref().map_or_else(|| self.root.clone(), |t| t.dir.clone());
        let in_dir = |f: &str| -> String {
            if dir == self.root {
                return f.to_string();
            }
            let abs = self.root.join(f);
            abs.strip_prefix(&dir).unwrap_or(&abs).to_string_lossy().into_owned()
        };
        let scoped = match &last {
            Last::File(f) => Last::File(in_dir(f)),
            Last::Test { file, name, id } => {
                Last::Test { file: file.as_deref().map(in_dir), name: name.clone(), id: id.clone() }
            }
            other => other.clone(),
        };
        let scope = match &scoped {
            Last::All => Scope::All,
            Last::File(f) => Scope::File(f),
            Last::Test { file, name, .. } => Scope::Test { file: file.as_deref(), name, case: one.as_ref() },
            Last::Failed => Scope::Failed(&failed),
        };
        self.tests.seq += 1;
        let report = std::env::temp_dir().join(format!("lumen-tests-{}-{}.dat", std::process::id(), self.tests.seq));
        let mut plan = match (&target, runner) {
            (Some(t), _) => testrun::plan_script(t, &scope, &report),
            (None, Some(r)) => {
                testrun::plan(r, &self.root, &scope, &report, (&self.config.test_command, &self.config.test_report))
            }
            (None, None) => return,
        };
        if plan.cmd.trim().is_empty() {
            return self.say("tests: no command to run");
        }
        if self.tests.cov.run
            && let Err(e) = testrun::with_coverage(&mut plan, &dir, &report)
        {
            self.say(format!("coverage: {e} — running without it"));
        }
        let cmd = plan.cmd.clone();
        if !self.tests_spawn(&cmd, &dir, Mode::Run) {
            plan.remove_temp();
            return;
        }
        // The rows this run will report on go back to "running"; a full run starts from scratch.
        match &last {
            Last::All => {
                self.tests.clear_cases();
                self.tests.sel = None;
                self.tests.collapsed.clear();
            }
            Last::File(f) => {
                let f = f.clone();
                for c in &mut self.tests.cases {
                    if c.group.ends_with(&f) || c.file.as_deref().is_some_and(|x| x.ends_with(&f)) {
                        c.status = Status::Running;
                    }
                }
            }
            Last::Test { id, name, .. } => {
                for c in &mut self.tests.cases {
                    if id.as_deref() == Some(c.id.as_str()) || (id.is_none() && testrun::leaf_of(&c.name) == *name) {
                        c.status = Status::Running;
                    }
                }
            }
            Last::Failed => {
                for c in &mut self.tests.cases {
                    if c.status == Status::Failed && c.id != RUN_FAILED {
                        c.status = Status::Running;
                    }
                }
            }
        }
        self.tests.collector = plan.stream.map(Collector::new);
        self.tests.log_only = plan.log_only;
        self.tests.run_dir = dir;
        self.tests.plan = Some(plan);
        self.tests.last = Some(last);
        self.tests.detail_scroll = 0.0;
        self.tests_push_raw(&format!("$ {cmd}"));
        self.tests_open_panel();
        self.request_redraw();
    }

    fn tests_push_raw(&mut self, line: &str) {
        let (fg, dim) = (self.theme.foreground, self.theme.chrome.tab_inactive_fg);
        let t = &mut self.tests;
        let segs = t.raw_ansi.line(line, fg, dim);
        let plain: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        t.raw.push(segs);
        t.raw_plain.push(plain);
        if t.raw.len() > MAX_RAW {
            t.raw.remove(0);
            t.raw_plain.remove(0);
        }
    }

    fn tests_stop_quiet(&mut self) -> bool {
        let Some(mut r) = self.tests.proc.take() else { return false };
        tasks::kill_group(r.pid);
        let _ = r.child.wait();
        if let Some(p) = self.tests.plan.take() {
            p.remove_temp();
        }
        for c in &mut self.tests.cases {
            if c.status == Status::Running {
                c.status = Status::Pending;
            }
        }
        self.tests.collector = None;
        true
    }

    pub fn tests_stop(&mut self) {
        if self.tests_stop_quiet() {
            self.tests_push_raw("■ stopped");
            self.tests.exit = Some(-1);
            self.say("tests stopped");
            self.request_redraw();
        }
    }

    /// Drain the runner's output; called whenever the event loop is woken.
    pub fn pump_tests(&mut self) {
        if self.tests.proc.is_none() {
            return;
        }
        let mut lines: Vec<String> = Vec::new();
        let mut done = false;
        if let Some(r) = &self.tests.proc {
            while let Ok(ev) = r.rx.try_recv() {
                match ev {
                    Event::Line(l) => lines.push(l),
                    Event::Done => done = true,
                }
            }
        }
        let debug_build = matches!(self.tests.mode, Mode::DebugBuild(_));
        for l in lines {
            if debug_build && l.trim_start().starts_with('{') {
                self.tests.dbg_json.push(l);
                continue;
            }
            self.tests_push_raw(&l);
            if let Some(col) = self.tests.collector.as_mut() {
                for c in col.feed(&l) {
                    self.tests.upsert(c);
                }
            }
        }
        if done {
            self.tests_finish();
        }
        self.request_redraw();
    }

    fn tests_finish(&mut self) {
        let Some(mut r) = self.tests.proc.take() else { return };
        let code = r.child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
        self.tests.exit = Some(code);
        let secs = self.tests.started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        self.tests.last_ms = Some(secs * 1000.0);
        if let Mode::DebugBuild(id) = std::mem::replace(&mut self.tests.mode, Mode::Run) {
            return self.tests_finish_debug_build(&id, code);
        }
        let mut cov_note: Option<String> = None;
        if let Some(plan) = self.tests.plan.take() {
            let since = self.tests.started_sys.unwrap_or_else(SystemTime::now);
            let dir =
                if self.tests.run_dir.as_os_str().is_empty() { self.root.clone() } else { self.tests.run_dir.clone() };
            for c in testrun::read_report(&plan, &dir, since) {
                self.tests.upsert(c);
            }
            if let Some(cov) = plan.coverage.clone() {
                cov_note = Some(self.coverage_after_run(&cov, &dir, since));
            }
            plan.remove_temp();
        }
        self.tests.collector = None;
        for c in &mut self.tests.cases {
            if c.status == Status::Running {
                c.status = Status::Pending;
            }
        }
        if self.tests.log_only {
            let mark = if code == 0 { "\u{f00c}" } else { "\u{f00d}" };
            self.tests_push_raw(&format!("{mark} exited with code {code} in {secs:.1}s"));
            self.tests.detail_scroll = 0.0;
            let name = self.tests.target().map_or_else(|| "the script".to_string(), |t| t.name.clone());
            let how = if code == 0 { "passed" } else { "failed" };
            return self.say(format!("tests: {name} {how} (exit code {code}) in {}", testrun::fmt_secs(secs)));
        }
        // Nothing reported but a bad exit (build error, missing tool...): show why instead of an empty panel.
        if self.tests.cases.iter().all(|c| c.status == Status::Pending) && code != 0 {
            let tail: Vec<&str> = self.tests.raw_plain.iter().rev().take(60).map(String::as_str).collect();
            let msg = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
            self.tests.upsert(Case {
                group: "run".into(),
                name: format!("The test run failed (exit code {code})"),
                id: RUN_FAILED.into(),
                status: Status::Failed,
                message: msg,
                ..Default::default()
            });
        }
        let counts = Counts::of(&self.tests.cases);
        let mark = if code == 0 { "\u{f00c}" } else { "\u{f00d}" };
        self.tests_push_raw(&format!("{mark} exited with code {code} in {secs:.1}s"));
        // Open on what matters: fold the groups that are all green when something failed.
        if counts.failed > 0 {
            let mut green: HashSet<String> = HashSet::new();
            let mut red: HashSet<&str> = HashSet::new();
            for c in &self.tests.cases {
                if c.status == Status::Failed {
                    red.insert(c.group.as_str());
                }
            }
            for c in &self.tests.cases {
                if !red.contains(c.group.as_str()) {
                    green.insert(c.group.clone());
                }
            }
            self.tests.collapsed.extend(green);
            if self.tests.sel.as_ref().and_then(|s| self.tests.case(s)).is_none_or(|c| c.status != Status::Failed)
                && let Some(f) = self.tests.cases.iter().find(|c| c.status == Status::Failed)
            {
                self.tests.sel = Some(f.id.clone());
            }
        }
        self.tests.scroll = 0.0;
        self.tests.detail_scroll = 0.0;
        self.tests_apply_marks();
        let summary = if counts.total() == 0 {
            "tests: nothing was reported".to_string()
        } else if counts.failed == 0 {
            format!("tests: {} passed in {}", counts.passed, testrun::fmt_secs(secs))
        } else {
            format!("tests: {} passed, {} failed in {}", counts.passed, counts.failed, testrun::fmt_secs(secs))
        };
        self.say(summary + cov_note.as_deref().unwrap_or(""));
    }

    /// ✓/✗ in the gutter of every open file that has results.
    pub fn tests_apply_marks(&mut self) {
        let Some(runner) = self.tests.runner else { return };
        for d in &mut self.docs {
            let Some(path) = d.path().map(PathBuf::from) else { continue };
            if d.buf.line_count() > 60_000 {
                continue;
            }
            let lines: Vec<String> = (0..d.buf.line_count()).map(|l| d.buf.line(l)).collect();
            d.test_marks = testrun::marks_for(&self.root, runner, &self.tests.cases, &path, &lines);
        }
    }

    pub fn tests_on_save(&mut self) {
        if self.config.test_watch && self.tests.last.is_some() && self.tests.proc.is_none() {
            self.tests.watch_due = Some(Instant::now() + Duration::from_millis(350));
        }
    }

    pub fn tests_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if self.tests.proc.is_some() {
            want(now + Duration::from_millis(90));
            if self.panel.open && self.panel.tab == PanelTab::Tests {
                self.request_redraw();
            }
        }
        if let Some(due) = self.tests.watch_due {
            if due <= now {
                self.tests.watch_due = None;
                if self.tests.proc.is_none()
                    && let Some(last) = self.tests.last.clone()
                {
                    self.tests_start(last);
                }
            } else {
                want(due);
            }
        }
    }

    // ───────────── debugging one test ─────────────

    pub fn tests_debug_cursor(&mut self) {
        self.tests_ensure_detect();
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(PathBuf::from) else { return };
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let (cur, _) = d.buf.line_col(d.ed.head());
        let lines: Vec<String> = (0..d.buf.line_count()).map(|l| d.buf.line(l)).collect();
        let Some(name) = testrun::test_name_at_cursor(&ext, &lines, cur) else {
            return self.say("no test at the cursor");
        };
        let rel = self.rel(&path);
        let known = self.tests.cases.iter().find(|c| testrun::leaf_of(&c.name) == name).cloned();
        let case = known.unwrap_or(Case { name: name.clone(), id: name, file: Some(rel), ..Default::default() });
        let runner = testrun::runner_for_ext(&self.tests.detected, &ext).or(self.tests.runner);
        self.tests_debug(case, runner);
    }

    pub fn tests_debug_case(&mut self, id: &str) {
        let Some(c) = self.tests.case(id).cloned() else { return };
        let runner = self.tests.runner;
        self.tests_debug(c, runner);
    }

    fn tests_debug(&mut self, case: Case, runner: Option<Runner>) {
        let Some(runner) = runner else { return self.say("no test runner found") };
        match runner {
            Runner::Nextest | Runner::Cargo => {
                self.tests_stop_quiet();
                let root = self.root.clone();
                let cmd = "cargo test --no-run --message-format=json";
                if self.tests_spawn(cmd, &root, Mode::DebugBuild(case.id.clone())) {
                    self.tests.collector = None;
                    self.tests.plan = None;
                    self.tests_push_raw("$ cargo test --no-run  (building the test binary for gdb)");
                    self.tests_open_panel();
                }
            }
            Runner::Vitest | Runner::Jest | Runner::NodeTest => self.tests_debug_js(runner, &case),
            r => self.say(format!(
                "no debugger adapter for {} yet (Rust via gdb, Vitest / Jest / node --test via js-debug)",
                r.label()
            )),
        }
    }

    fn tests_debug_js(&mut self, runner: Runner, case: &Case) {
        let Some((file, _)) = testrun::locate(&self.root, runner, case) else {
            return self.say("could not find the test's file");
        };
        let rel = file.strip_prefix(&self.root).unwrap_or(&file).to_string_lossy().into_owned();
        let title = case.name.rsplit(" › ").next().unwrap_or(&case.name).to_string();
        let nm = self.root.join("node_modules");
        let (program, args, rt): (PathBuf, Vec<String>, Vec<String>) = match runner {
            Runner::Vitest => (
                nm.join("vitest/vitest.mjs"),
                ["run", &rel, "-t", &title, "--no-file-parallelism"].map(String::from).to_vec(),
                vec![],
            ),
            Runner::Jest => (
                nm.join("jest/bin/jest.js"),
                [&rel, "-t", &title, "--runInBand"].map(|s| s.to_string()).to_vec(),
                vec![],
            ),
            _ => (file.clone(), vec![], vec![format!("--test-name-pattern={}", regex::escape(&title))]),
        };
        if !program.exists() {
            return self.say(format!("{} not found in node_modules", program.display()));
        }
        let root = self.root.clone();
        self.start_node_debug_args(&program, args, rt, &root);
    }

    fn tests_finish_debug_build(&mut self, id: &str, code: i32) {
        let json = std::mem::take(&mut self.tests.dbg_json);
        let mut exes: Vec<PathBuf> = Vec::new();
        let mut rendered: Vec<String> = Vec::new();
        for l in &json {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else { continue };
            if v["reason"] == "compiler-message"
                && let Some(r) = v["message"]["rendered"].as_str()
            {
                rendered.push(r.to_string());
            }
            if v["reason"] == "compiler-artifact"
                && v["profile"]["test"] == true
                && let Some(e) = v["executable"].as_str()
            {
                exes.push(PathBuf::from(e));
            }
        }
        if code != 0 {
            for r in rendered {
                for l in r.lines() {
                    self.tests_push_raw(l);
                }
            }
            return self.say("the test build failed — see the output");
        }
        let needle = format!("{id}: test");
        let found = exes.into_iter().find(|exe| {
            std::process::Command::new(exe)
                .arg("--list")
                .current_dir(&self.root)
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == needle))
        });
        let Some(exe) = found else { return self.say(format!("could not find a test binary that has {id}")) };
        let root = self.root.clone();
        self.start_native_debug_args(exe, vec![id.to_string(), "--exact".into(), "--nocapture".into()], root);
    }

    // ───────────── input ─────────────

    fn tests_hit_at(&self, x: f64, y: f64) -> Option<&Hit> {
        if !self.panel.open || self.panel.tab != PanelTab::Tests {
            return None;
        }
        self.tests.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h)
    }

    /// The toolbar item under the pointer that has a tooltip.
    pub fn tests_tip_at(&self, x: f64, y: f64) -> Option<(Rect, Tip)> {
        if !self.overlay.is_none() || !self.panel.open || self.panel.tab != PanelTab::Tests {
            return None;
        }
        self.tests.tips.iter().rev().find(|(r, _)| r.contains(x, y)).copied()
    }

    pub fn tests_tip(&self, k: Tip) -> crate::app_tooltip::TipText {
        let (title, detail, more) = match k {
            Tip::Target => match self.tests.target() {
                Some(t) => {
                    let how = match t.runner() {
                        Some(r) => format!("Runs with {} — full results in the tree", r.label()),
                        None => "Log only — its results can't be parsed, the panel shows its output".to_string(),
                    };
                    let mut more = vec![t.command.clone()];
                    if !t.rel.is_empty() {
                        more.push(format!("in {}", t.rel));
                    }
                    (format!("Test target: {}", t.name), Some(how), more)
                }
                None => {
                    let r = self.tests.runner.map_or("no runner", |r| r.label());
                    (format!("Test target: Auto ({r})"), Some("Choose a package.json test script".into()), Vec::new())
                }
            },
            Tip::Failed => (
                "Run failed tests".to_string(),
                Some("Not for a log-only script: it only runs as a whole".to_string()),
                Vec::new(),
            ),
        };
        crate::app_tooltip::TipText { title, keys: None, detail, more }
    }

    pub fn tests_clickable_at(&self, x: f64, y: f64) -> bool {
        self.overlay.is_none() && self.tests_hit_at(x, y).is_some()
    }

    /// Hover tint for everything clickable in the panel (rows, chips, buttons), in the same shape
    /// as the thing drawn. The row that already carries the selection gets none, like the git panel.
    pub fn tests_hover_target(&self) -> Option<(Rect, i32)> {
        if !self.overlay.is_none() || !self.panel.open || self.panel.tab != PanelTab::Tests {
            return None;
        }
        let (x, y) = self.mouse;
        let (r, h) = self.tests.hits.iter().rev().find(|(r, _)| r.contains(x, y))?;
        match h {
            Hit::Link(..) => None,
            Hit::Filter(f) if self.tests.filter == *f => None,
            Hit::Case(id) if self.tests.sel.as_deref() == Some(id.as_str()) => None,
            Hit::Group(k) if self.tests.focused && self.tests.cur_group.as_deref() == Some(k.as_str()) => None,
            _ => Some((*r, RAD)),
        }
    }

    /// Is the pointer over `r` (for brightening labels; the tint itself is the app's hover rect)?
    fn t_hv(&self, r: Rect) -> bool {
        self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1)
    }

    pub fn tests_click(&mut self, x: f64, y: f64) {
        self.tests.focused = true;
        let Some(hit) = self.tests_hit_at(x, y).cloned() else { return };
        match hit {
            Hit::RunAll => self.tests_run_all(),
            Hit::RunFailed => self.tests_run_failed(),
            Hit::Stop => self.tests_stop(),
            Hit::Watch => self.tests_toggle_watch(),
            Hit::Runner => self.tests_pick_runner(),
            Hit::Filter(f) => {
                self.tests.filter = f;
                self.tests.scroll = 0.0;
            }
            Hit::Fold(collapse) => {
                if collapse {
                    let groups: Vec<String> = self.tests.cases.iter().map(|c| c.group.clone()).collect();
                    self.tests.collapsed.extend(groups);
                } else {
                    self.tests.collapsed.clear();
                }
                self.tests.scroll = 0.0;
            }
            Hit::Group(k) => {
                if !self.tests.collapsed.remove(&k) {
                    self.tests.collapsed.insert(k.clone());
                }
                self.tests.cur_group = Some(k);
            }
            Hit::Case(id) => {
                let double = self
                    .tests
                    .last_click
                    .as_ref()
                    .is_some_and(|(i, t)| *i == id && t.elapsed() < Duration::from_millis(400));
                self.tests.last_click = Some((id.clone(), Instant::now()));
                self.tests_select(&id);
                if double {
                    self.tests_open_case(&id);
                }
            }
            Hit::RunOne(id) => self.tests_run_case(&id),
            Hit::DebugOne(id) => self.tests_debug_case(&id),
            Hit::OpenOne(id) => self.tests_open_case(&id),
            Hit::ShowOutput(on) => {
                self.tests.show_output = on;
                self.tests.detail_scroll = 0.0;
            }
            Hit::Link(path, line, col) => match tasks::resolve_link_path(&self.root, &path) {
                Some(p) => {
                    self.open_path(&p, Some((line, col)));
                }
                None => self.say(format!("file not found: {path}")),
            },
        }
    }

    /// Select test `id` (the detail pane follows) and park the keyboard cursor on it.
    fn tests_select(&mut self, id: &str) {
        if self.tests.sel.as_deref() != Some(id) {
            self.tests.detail_scroll = 0.0;
        }
        self.tests.sel = Some(id.to_string());
        self.tests.cur_group = None;
        self.tests.show_output = false;
    }

    /// Give the list the keyboard (clicking the Tests tab, or the panel itself).
    pub fn tests_focus(&mut self) {
        self.tests.focused = true;
    }

    /// Keys while the list has the focus: arrows move the cursor (Left/Right fold and unfold
    /// groups), Enter opens the test (or folds the group), Space runs it, Esc gives the
    /// keyboard back. Anything else hands the focus back to the editor unconsumed.
    pub fn tests_key(&mut self, ev: &KeyIn) -> bool {
        if !self.tests.focused {
            return false;
        }
        if !self.panel.open || self.panel.tab != PanelTab::Tests || self.focus != Focus::Editor {
            self.tests.focused = false;
            return false;
        }
        let Key::Named(k) = &ev.key else {
            self.tests.focused = false;
            return false;
        };
        let (ctrl, alt) = (self.mods.control_key(), self.mods.alt_key());
        if matches!(
            k,
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::CapsLock
        ) {
            return true;
        }
        if ctrl || alt {
            self.tests.focused = false;
            return false;
        }
        let running = self.tests.proc.is_some();
        let rows = build_rows(&self.tests.cases, self.tests.filter, &self.tests.collapsed, !running);
        let at = cursor_row(&rows, &self.tests.cases, self.tests.cur_group.as_deref(), self.tests.sel.as_deref());
        let page = ((self.tests.list_view / ROW_H).max(2) - 1) as usize;
        let n = match k {
            NamedKey::ArrowUp => Nav::Up,
            NamedKey::ArrowDown => Nav::Down,
            NamedKey::PageUp => Nav::PageUp(page),
            NamedKey::PageDown => Nav::PageDown(page),
            NamedKey::Home => Nav::Home,
            NamedKey::End => Nav::End,
            NamedKey::ArrowLeft => Nav::Left,
            NamedKey::ArrowRight => Nav::Right,
            NamedKey::Escape => {
                self.tests.focused = false;
                return true;
            }
            NamedKey::Enter | NamedKey::Space => {
                match at.map(|k| &rows[k]) {
                    Some(Row::Group { key, .. }) => {
                        let key = key.clone();
                        if !self.tests.collapsed.remove(&key) {
                            self.tests.collapsed.insert(key);
                        }
                    }
                    Some(Row::Case(i)) => {
                        let id = self.tests.cases[*i].id.clone();
                        if *k == NamedKey::Enter {
                            self.tests_open_case(&id);
                        } else if !running {
                            self.tests_run_case(&id);
                        }
                    }
                    None => {}
                }
                return true;
            }
            _ => {
                self.tests.focused = false;
                return false;
            }
        };
        match nav(&rows, &self.tests.cases, &self.tests.collapsed, at, n) {
            NavOut::To(j) => {
                match &rows[j] {
                    Row::Group { key, .. } => self.tests.cur_group = Some(key.clone()),
                    Row::Case(i) => {
                        let id = self.tests.cases[*i].id.clone();
                        self.tests_select(&id);
                    }
                }
                let (tops, _) = row_tops(&rows);
                let view = self.tests.list_view;
                self.tests.scroll = scroll_to_show(self.tests.scroll, tops[j], row_h(&rows[j]), view);
            }
            NavOut::Fold(g) => {
                self.tests.collapsed.insert(g.clone());
                self.tests.cur_group = Some(g);
            }
            NavOut::Unfold(g) => {
                self.tests.collapsed.remove(&g);
                self.tests.cur_group = Some(g);
            }
            NavOut::Stay => {}
        }
        true
    }

    fn tests_open_case(&mut self, id: &str) {
        let Some(c) = self.tests.case(id).cloned() else { return };
        let runner = self.tests.runner.unwrap_or(Runner::Cargo);
        match testrun::locate(&self.root, runner, &c) {
            Some((p, line)) => {
                self.open_path(&p, Some((line, 0)));
            }
            None => self.say("could not find where this test is defined"),
        }
    }

    // ───────────── drawing ─────────────

    /// A pill button in the git panel's style (tinted fill, 8 px corners, no hairline); returns
    /// its width. Disabled buttons are drawn faded and get no hit (so no hover either).
    #[allow(clippy::too_many_arguments)]
    fn t_button(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        label: &str,
        icon: Option<&str>,
        style: Btn,
        enabled: bool,
        hit: Hit,
        clip: Rect,
    ) -> i32 {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let bold = style == Btn::Primary;
        let tw = self.tx.ui_width(label, bold);
        let w = 24 + tw + if icon.is_some() { 20 } else { 0 };
        let r = Rect::new(x, y, w, BTN_H);
        let hov = enabled && self.t_hv(r);
        let (fill, text, ic) = match style {
            // The hover tint itself is the app's hover veil (see `tests_hover_target`); only labels brighten here.
            Btn::Primary if enabled => (c.accent, c.on_accent, c.on_accent),
            Btn::Primary => (c.accent.with_alpha(0x48), strong.with_alpha(0xa0), strong.with_alpha(0xa0)),
            Btn::Danger => (c.error.with_alpha(0x33), c.error, c.error),
            Btn::Toggle(true) => (c.accent.with_alpha(0x30), c.accent_text, c.accent_text),
            Btn::Toggle(false) | Btn::Secondary => {
                let col = if hov || (enabled && style == Btn::Secondary) { strong } else { dim };
                let ic = if style == Btn::Secondary && enabled { c.accent_text } else { col };
                (strong.with_alpha(0x0e), col, ic)
            }
        };
        let (text, ic) = if enabled { (text, ic) } else { (text.with_alpha(0x80), ic.with_alpha(0x80)) };
        cv.fill_round_clip(r, RAD, fill, clip);
        let mut tx_ = x + 12;
        if let Some(i) = icon {
            self.tx.icon(cv, i, tx_, y + (BTN_H - 14) / 2, 14, ic, clip);
            tx_ += 20;
        }
        self.tx.draw_ui(cv, tx_, y + (BTN_H - 18) / 2, label, text, clip, bold);
        if enabled {
            self.tests.hits.push((r, hit));
        }
        w
    }

    /// The target selector, like the Build panel's: the runner's icon, the target and its kind, a chevron.
    fn t_select(&mut self, cv: &mut Canvas, x: i32, y: i32, clip: Rect) -> i32 {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let enabled = !self.tests.scripts.is_empty() || self.tests.detected.len() > 1;
        let sel = self.tests.target().map(|t| (t.name.clone(), t.runner()));
        let (label, tag, runner) = match sel {
            Some((n, Some(r))) => (n, r.label().to_string(), Some(r)),
            Some((n, None)) => (n, "log only".to_string(), None),
            None => match self.tests.runner {
                Some(r) => ("Auto".to_string(), r.label().to_string(), Some(r)),
                None => ("No runner".to_string(), String::new(), None),
            },
        };
        let icon = match runner.and_then(runner_marker) {
            Some(m) => self.tx.icons.file_icon(m),
            None if self.tests.target.is_some() => "ui/terminal".to_string(),
            None => "ui/list_todo".to_string(),
        };
        let label = fit(&mut self.tx, &label, 220, false);
        let tw = self.tx.ui_width(&label, false);
        let tagw = if tag.is_empty() { 0 } else { 8 + sized_w(&mut self.tx, &tag, false, SMALL) };
        let w = 10 + 16 + 8 + tw + tagw + 10 + 14 + 10;
        let r = Rect::new(x, y, w, BTN_H);
        cv.fill_round_clip(r, RAD, strong.with_alpha(0x0e), clip);
        let (ic, text) = if enabled || self.tests.target.is_some() { (c.accent_text, strong) } else { (dim, dim) };
        self.tx.icon(cv, &icon, x + 10, y + (BTN_H - 16) / 2, 16, ic, clip);
        self.tx.draw_ui(cv, x + 34, y + (BTN_H - 18) / 2, &label, text, clip, false);
        if !tag.is_empty() {
            self.tx.draw_ui_sized(cv, x + 34 + tw + 8, y + (BTN_H - 17) / 2, &tag, dim, clip, false, SMALL);
        }
        let chev = if enabled && self.t_hv(r) { strong } else { dim };
        self.tx.icon(cv, "ui/chevron_down", r.right() - 24, y + (BTN_H - 14) / 2, 14, chev, clip);
        if enabled {
            self.tests.hits.push((r, Hit::Runner));
        }
        self.tests.tips.push((r, Tip::Target));
        w
    }

    pub fn draw_tests(&mut self, cv: &mut Canvas, body: Rect) {
        self.tests.hits.clear();
        self.tests.tips.clear();
        self.tests_ensure_detect();
        let c = self.theme.chrome;
        let counts = Counts::of(&self.tests.cases);
        let running = self.tests.proc.is_some();
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let bg = self.theme.background;

        // ── toolbar ──
        let ty = body.y + (TOOLBAR_H - BTN_H) / 2;
        let mut x = body.x + 10;
        let has_runner = self.tests.runner.is_some() || self.tests.target.is_some();
        let log_only = self.tests.log_only_target();
        let debug_build = matches!(self.tests.mode, Mode::DebugBuild(_));
        if running {
            x += self.t_button(cv, x, ty, "Stop", Some("ui/stop"), Btn::Danger, true, Hit::Stop, body) + 6;
        } else {
            let label = if self.tests.cases.is_empty() { "Run all" } else { "Run again" };
            x += self.t_button(cv, x, ty, label, Some("ui/play_filled"), Btn::Primary, has_runner, Hit::RunAll, body)
                + 6;
        }
        let any_failed = self.tests.cases.iter().any(|c| c.status == Status::Failed && c.id != RUN_FAILED);
        let fw = self.t_button(
            cv,
            x,
            ty,
            "Failed",
            Some("ui/history_rerun"),
            Btn::Secondary,
            !running && any_failed && !log_only,
            Hit::RunFailed,
            body,
        );
        if log_only {
            self.tests.tips.push((Rect::new(x, ty, fw, BTN_H), Tip::Failed));
        }
        x += fw + 6;
        let watch_on = self.config.test_watch;
        x += self.t_button(cv, x, ty, "Watch", Some("ui/eye"), Btn::Toggle(watch_on), true, Hit::Watch, body) + 6;
        x += self.t_select(cv, x, ty, body) + 12;

        // Right side: live status.
        let mut pieces: Vec<(String, Rgba)> = Vec::new();
        if running {
            let secs = self.tests.started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            let what = if debug_build { "Building for the debugger" } else { "Running" };
            pieces.push((format!("{what} · {}", testrun::fmt_secs(secs)), dim));
            if counts.passed + counts.failed > 0 {
                pieces.push(("   ".into(), dim));
                pieces.push((format!("{} passed", counts.passed), c.git_added));
                if counts.failed > 0 {
                    pieces.push(("  ".into(), dim));
                    pieces.push((format!("{} failed", counts.failed), c.error));
                }
            }
        } else if self.tests.log_only
            && counts.total() == 0
            && let Some(e) = self.tests.exit
        {
            pieces.push(match e {
                0 => ("Passed · exit code 0".to_string(), c.git_added),
                -1 => ("Stopped".to_string(), dim),
                e => (format!("Failed · exit code {e}"), c.error),
            });
            if let Some(ms) = self.tests.last_ms.filter(|_| e != -1) {
                pieces.push(("  ·  ".into(), dim));
                pieces.push((fmt_ms(ms), dim));
            }
        } else if counts.total() > 0 {
            pieces.push((format!("{} passed", counts.passed), c.git_added));
            if counts.failed > 0 {
                pieces.push(("  ·  ".into(), dim));
                pieces.push((format!("{} failed", counts.failed), c.error));
            }
            if counts.skipped > 0 {
                pieces.push(("  ·  ".into(), dim));
                pieces.push((format!("{} skipped", counts.skipped), dim));
            }
            if let Some(ms) = self.tests.last_ms {
                pieces.push(("  ·  ".into(), dim));
                pieces.push((fmt_ms(ms), dim));
            }
        }
        let total_w: i32 = pieces.iter().map(|(s, _)| self.tx.ui_width(s, false)).sum();
        let mut px = body.right() - total_w - 16;
        if px > x {
            if running {
                spinner(cv, px - 14, body.y + TOOLBAR_H / 2, 5, c.accent_text, bg, self.tests.started, body);
            }
            for (s, col) in &pieces {
                let w = self.tx.ui_width(s, false);
                self.tx.draw_ui(cv, px, body.y + (TOOLBAR_H - 18) / 2, s, *col, body, false);
                px += w;
            }
        }

        // ── progress bar ──
        let pb = Rect::new(body.x, body.y + TOOLBAR_H, body.w, PROG_H);
        cv.fill(pb, c.border.with_alpha(110).over(bg));
        let total = counts.total();
        if total == 0 && running {
            let t = self.tests.started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
            let bw = (body.w / 5).max(80);
            let pos = ((t * 0.8).fract() * (body.w + bw) as f32) as i32 - bw;
            cv.fill_clip(Rect::new(body.x + pos, pb.y, bw, PROG_H), c.accent, pb);
        } else if total == 0 && self.tests.log_only && matches!(self.tests.exit, Some(e) if e >= 0) {
            let col = if self.tests.exit == Some(0) { c.git_added } else { c.error };
            cv.fill_clip(pb, col, pb);
        } else if total > 0 {
            let mut sx = body.x;
            for (n, col) in
                [(counts.passed, c.git_added), (counts.failed, c.error), (counts.skipped, c.warning.with_alpha(150))]
            {
                if n == 0 {
                    continue;
                }
                let w = ((n as f64 / total as f64) * body.w as f64).round() as i32;
                cv.fill_clip(Rect::new(sx, pb.y, w.max(2), PROG_H), col, pb);
                sx += w;
            }
        }

        // ── filter bar: chips like the git panel's tabs, then Collapse / Expand all ──
        let fb = Rect::new(body.x, pb.bottom(), body.w, FILTER_H);
        cv.fill(Rect::new(fb.x, fb.bottom() - 1, fb.w, 1), c.border);
        let cy = fb.y + (FILTER_H - 1 - CHIP_H) / 2;
        let mut fx = fb.x + 8;
        for (label, f, n) in [
            ("All", Filter::All, counts.total()),
            ("Failed", Filter::Failed, counts.failed),
            ("Passed", Filter::Passed, counts.passed),
            ("Skipped", Filter::Skipped, counts.skipped),
        ] {
            let text = format!("{label}  {n}");
            // Measured bold so the chip keeps its width when it becomes the active one.
            let w = self.tx.ui_width(&text, true) + 20;
            let r = Rect::new(fx, cy, w, CHIP_H);
            let active = self.tests.filter == f;
            if active {
                cv.fill_round_clip(r, RAD, strong.with_alpha(0x1e), fb);
            }
            let col = if active || self.t_hv(r) {
                strong
            } else if f == Filter::Failed && n > 0 {
                c.error
            } else {
                dim
            };
            let tw = self.tx.ui_width(&text, active);
            self.tx.draw_ui(cv, r.x + (w - tw) / 2, r.y + (CHIP_H - 18) / 2, &text, col, fb, active);
            self.tests.hits.push((r, Hit::Filter(f)));
            fx += w + 4;
        }
        if counts.total() > 0 {
            let all_folded = self.tests.cases.iter().all(|c| self.tests.collapsed.contains(&c.group));
            let (label, icon) =
                if all_folded { ("Expand all", "ui/chevron_up_down") } else { ("Collapse all", "ui/chevron_down_up") };
            let w = self.tx.ui_width(label, false) + 18 + 20;
            let r = Rect::new(fb.right() - w - 8, cy, w, CHIP_H);
            if r.x > fx + 8 {
                let col = if self.t_hv(r) { strong } else { dim };
                self.tx.icon(cv, icon, r.x + 9, r.y + (CHIP_H - 14) / 2, 14, col, fb);
                self.tx.draw_ui(cv, r.x + 29, r.y + (CHIP_H - 18) / 2, label, col, fb, false);
                self.tests.hits.push((r, Hit::Fold(!all_folded)));
            }
        }

        // ── content ──
        let content = Rect::new(body.x, fb.bottom(), body.w, (body.bottom() - fb.bottom()).max(0));
        if content.h < 30 {
            return;
        }
        let log_view = self.tests.cases.is_empty() && self.tests.log_only && !self.tests.raw.is_empty();
        if self.tests.cases.is_empty() && !log_view {
            // Nothing to list: the empty state takes the whole width (no list/detail divider).
            self.draw_tests_empty(cv, content, running);
            return;
        }
        let lw = if content.w >= 720 { (content.w as f32 * 0.56) as i32 } else { content.w / 2 };
        let lw = if log_view { (content.w * 2 / 5).max(content.w.min(260)) } else { lw };
        let list = Rect::new(content.x, content.y, lw, content.h);
        let detail = Rect::new(content.x + lw, content.y, content.w - lw, content.h);
        cv.fill(Rect::new(detail.x, detail.y, 1, detail.h), c.border);
        if log_view {
            self.draw_tests_log_only(cv, list, running);
        } else {
            self.draw_tests_list(cv, list);
        }
        self.draw_tests_detail(cv, Rect::new(detail.x + 1, detail.y, detail.w - 1, detail.h));
    }

    /// Centered empty state: an icon, a title and up to a few dim lines, like an onboarding card.
    fn t_empty_state(
        &mut self,
        cv: &mut Canvas,
        area: Rect,
        icon: &str,
        icon_col: Rgba,
        title: &str,
        lines: &[(String, Rgba)],
    ) {
        let strong = self.theme.chrome.tab_active_fg;
        let block = 28 + 12 + 22 + lines.len() as i32 * 20;
        let mut y = area.y + ((area.h - block) / 2).clamp(16, 40);
        self.tx.icon(cv, icon, area.x + (area.w - 28) / 2, y, 28, icon_col, area);
        y += 28 + 12;
        let w = self.tx.ui_width(title, true);
        self.tx.draw_ui(cv, area.x + (area.w - w) / 2, y, title, strong, area, true);
        y += 24;
        for (s, col) in lines {
            let s = fit(&mut self.tx, s, area.w - 32, false);
            let w = self.tx.ui_width(&s, false);
            self.tx.draw_ui(cv, area.x + (area.w - w) / 2, y, &s, *col, area, false);
            y += 20;
        }
    }

    /// The tree's place for a log-only script: what ran and how it ended (the output is beside it).
    fn draw_tests_log_only(&mut self, cv: &mut Canvas, area: Rect, running: bool) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        let (name, cmd) = self
            .tests
            .target()
            .map_or_else(|| ("The script".to_string(), String::new()), |t| (t.name.clone(), t.pm.run(&t.name)));
        let (icon, col, title) = match self.tests.exit {
            _ if running => ("ui/terminal", c.accent_text, format!("Running {name}…")),
            Some(0) => ("ui/check", c.git_added, format!("{name} passed")),
            Some(-1) => ("ui/stop", dim, format!("{name} was stopped")),
            Some(e) => ("ui/close", c.error, format!("{name} failed (exit code {e})")),
            None => ("ui/terminal", dim, name.clone()),
        };
        let mut lines = vec![("This script's results can't be parsed — showing its output".to_string(), dim)];
        if !cmd.is_empty() {
            lines.push((format!("$ {cmd}"), dim));
        }
        self.t_empty_state(cv, area, icon, col, &title, &lines);
    }

    fn draw_tests_empty(&mut self, cv: &mut Canvas, area: Rect, running: bool) {
        let c = self.theme.chrome;
        let (dim, bg) = (c.tab_inactive_fg, self.theme.background);
        if running {
            // Placeholder rows that breathe while the first results are on their way.
            let t = self.tests.started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
            for k in 0..((area.h / ROW_H).min(7)) {
                let a = (0.5 + 0.5 * (t * 3.0 - k as f32 * 0.5).sin()) * 0.55 + 0.25;
                let col = c.border.with_alpha((a * 130.0) as u8).over(bg);
                let w = [0.5, 0.36, 0.62, 0.44, 0.55, 0.3, 0.48][k as usize % 7];
                let y = area.y + LIST_PAD + 4 + k * ROW_H;
                cv.fill_round_clip(Rect::new(area.x + 32, y + 8, 14, 14), 7, col, area);
                cv.fill_round_clip(Rect::new(area.x + 56, y + 11, (area.w as f32 * w * 0.6) as i32, 8), 4, col, area);
            }
            let msg = if matches!(self.tests.mode, Mode::DebugBuild(_)) {
                "Building the test binary…"
            } else {
                "Waiting for the first results…"
            };
            let w = self.tx.ui_width(msg, false) + 32;
            let pill = Rect::new(area.x + (area.w - w) / 2, area.y + area.h / 2 - 14, w, 28);
            cv.fill_round_clip(pill, RAD, c.input_bg.with_alpha(0xe0).over(bg), area);
            self.tx.draw_ui(cv, pill.x + 16, pill.y + 5, msg, dim, area, false);
            return;
        }
        if let Some(t) = self.tests.target().cloned() {
            let lines = match t.runner() {
                Some(r) => vec![
                    (format!("Runs {} with {} — press Run all or Ctrl+K A", t.name, r.label()), dim),
                    (t.command.clone(), dim),
                ],
                None => vec![
                    (format!("Runs {} — press Run all or Ctrl+K A", t.pm.run(&t.name)), dim),
                    ("Log only: its results can't be parsed, so the panel shows its output and exit code".into(), dim),
                ],
            };
            self.t_empty_state(cv, area, "ui/list_todo", c.accent_text, "No results yet", &lines);
        } else if let Some(r) = self.tests.runner {
            let mut lines = vec![(format!("Runs with {} — press Run all or Ctrl+K A", r.label()), dim)];
            if let Some(e) = self.tests.exit.filter(|e| *e != 0) {
                lines.push((format!("The last run ended with code {e}; see the Output tab for details"), c.error));
            }
            if self.tests.detected.len() > 1 {
                let names: Vec<&str> = self.tests.detected.iter().map(|r| r.label()).collect();
                lines.push((format!("Also found: {}", names.join(", ")), dim));
            }
            if !self.tests.scripts.is_empty() {
                let n = self.tests.scripts.len();
                lines.push((
                    format!(
                        "{n} package.json test script{} — pick one from the target menu",
                        if n == 1 { "" } else { "s" }
                    ),
                    dim,
                ));
            }
            self.t_empty_state(cv, area, "ui/list_todo", c.accent_text, "No results yet", &lines);
        } else {
            let lines = [
                ("Supported: cargo / nextest, vitest, jest, node --test, bun, deno, go, pytest,".to_string(), dim),
                ("phpunit / pest, rspec, maven, gradle, dotnet, ctest".to_string(), dim),
                ("Anything else: set test_command and test_report (a JUnit XML file) in the settings".to_string(), dim),
            ];
            self.t_empty_state(cv, area, "ui/flame", dim, "No test runner found", &lines);
        }
    }

    fn draw_tests_list(&mut self, cv: &mut Canvas, list: Rect) {
        let c = self.theme.chrome;
        let (strong, dim, bg) = (c.tab_active_fg, c.tab_inactive_fg, self.theme.background);
        let running = self.tests.proc.is_some();
        let rows = build_rows(&self.tests.cases, self.tests.filter, &self.tests.collapsed, !running);
        let (tops, content_h) = row_tops(&rows);
        let total = content_h as f64;
        let visible = list.h as f64;
        self.tests.list_view = list.h;
        let max = (total - visible).max(0.0);
        self.tests.scroll = self.tests.scroll.clamp(0.0, max);
        let scroll = self.tests.scroll;
        let max_ms = self.tests.cases.iter().filter_map(|c| c.ms).fold(1.0_f64, f64::max);
        let inner_w = list.w - BAR_W - 8;
        let t = self.tests.started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
        let focused = self.tests.focused;
        let cur = cursor_row(&rows, &self.tests.cases, self.tests.cur_group.as_deref(), self.tests.sel.as_deref());
        if rows.is_empty() {
            self.t_empty_state(cv, list, "ui/filter_funnel", dim, "Nothing matches this filter", &[]);
        }
        for (k, row) in rows.iter().enumerate() {
            let y = list.y + tops[k] - scroll as i32;
            if y + row_h(row) < list.y {
                continue;
            }
            if y > list.bottom() {
                break;
            }
            let is_cur = focused && cur == Some(k);
            match row {
                Row::Group { key, pass, fail, skip, run, total, ms } => {
                    let r = Rect::new(list.x + 6, y + GROUP_GAP, inner_w, ROW_H);
                    let vis = r.intersect(&list);
                    let gy = r.y;
                    if is_cur && self.tests.cur_group.is_some() {
                        cv.fill_round_clip(r, RAD, strong.with_alpha(0x1e), list);
                    }
                    let folded = self.tests.collapsed.contains(key);
                    let icon = if folded { "ui/chevron_right" } else { "ui/chevron_down" };
                    self.tx.icon(cv, icon, r.x + 6, gy + (ROW_H - 16) / 2, 16, dim, list);
                    let (name, dir) = group_label(&self.root, key);
                    // Right side first (duration, then counts), so the label knows how much room it has.
                    let mut rx = r.right() - 10;
                    if *run == 0 && *ms > 0.0 {
                        let s = fmt_ms(*ms);
                        rx -= sized_w(&mut self.tx, &s, false, SMALL);
                        self.tx.draw_ui_sized(cv, rx, gy + 7, &s, dim, list, false, SMALL);
                        rx -= 14;
                    }
                    if *run > 0 {
                        spinner(cv, rx - 6, gy + ROW_H / 2, 5, c.accent_text, bg, self.tests.started, list);
                        rx -= 22;
                    }
                    for (n, icon, col) in
                        [(*fail, "ui/close", c.error), (*skip, "ui/dash", c.warning), (*pass, "ui/check", c.git_added)]
                    {
                        if n == 0 {
                            continue;
                        }
                        let s = n.to_string();
                        rx -= self.tx.ui_width(&s, false);
                        self.tx.draw_ui(cv, rx, gy + (ROW_H - 18) / 2, &s, col, list, false);
                        rx -= 3 + 14;
                        self.tx.icon(cv, icon, rx, gy + (ROW_H - 14) / 2, 14, col, list);
                        rx -= 10;
                    }
                    if *run == 0 && *pass + *fail + *skip < *total {
                        let s = format!("{} pending", total - pass - fail - skip);
                        rx -= sized_w(&mut self.tx, &s, false, SMALL);
                        self.tx.draw_ui_sized(cv, rx, gy + 7, &s, dim, list, false, SMALL);
                        rx -= 10;
                    }
                    let nx = r.x + 28;
                    let room = rx - nx;
                    let name_s = fit(&mut self.tx, &name, room, true);
                    let nw = self.tx.ui_width(&name_s, true);
                    self.tx.draw_ui(cv, nx, gy + (ROW_H - 18) / 2, &name_s, strong, list, true);
                    if !dir.is_empty() && room - nw > 40 {
                        let d = fit(&mut self.tx, &dir, room - nw - 10, false);
                        self.tx.draw_ui_sized(cv, nx + nw + 8, gy + 8, &d, dim, list, false, SMALL);
                    }
                    if is_cur && self.tests.cur_group.is_some() && vis == r {
                        cv.ring(r, RAD, c.accent.with_alpha(0x90));
                    }
                    if vis.h > 0 {
                        self.tests.hits.push((vis, Hit::Group(key.clone())));
                    }
                }
                Row::Case(i) => {
                    let case = self.tests.cases[*i].clone();
                    let r = Rect::new(list.x + 6, y, inner_w, ROW_H);
                    let vis = r.intersect(&list);
                    let selected = self.tests.sel.as_deref() == Some(case.id.as_str());
                    if selected {
                        cv.fill_round_clip(r, RAD, strong.with_alpha(0x1e), list);
                        cv.fill_round_clip(
                            Rect::new(r.x + 3, y + 8, 2, ROW_H - 16),
                            1,
                            status_color(&c, case.status),
                            list,
                        );
                        if is_cur && vis == r {
                            cv.ring(r, RAD, c.accent.with_alpha(0x90));
                        }
                    }
                    let ix = r.x + 26;
                    let iy = y + (ROW_H - 16) / 2;
                    match case.status {
                        Status::Passed => self.tx.icon(cv, "ui/check", ix, iy, 16, c.git_added, list),
                        Status::Failed => self.tx.icon(cv, "ui/close", ix, iy, 16, c.error, list),
                        Status::Skipped => self.tx.icon(cv, "ui/dash", ix, iy, 16, c.warning, list),
                        Status::Pending => self.tx.icon(cv, "ui/circle", ix + 3, iy + 3, 10, dim.with_alpha(150), list),
                        Status::Running => spinner_t(cv, ix + 8, iy + 8, 5, c.accent_text, bg, t, list),
                    }
                    // Duration + a small bar relative to the slowest test.
                    let mut rx = r.right() - 10;
                    if let Some(ms) = case.ms.filter(|_| case.status != Status::Running) {
                        let s = fmt_ms(ms);
                        rx -= sized_w(&mut self.tx, &s, false, SMALL);
                        let slow = ms >= 1000.0;
                        self.tx.draw_ui_sized(
                            cv,
                            rx,
                            y + 7,
                            &s,
                            if slow { c.warning } else { dim },
                            list,
                            false,
                            SMALL,
                        );
                        let bw = ((ms / max_ms).sqrt() * 36.0).round() as i32;
                        if bw >= 2 {
                            let col = if slow { c.warning } else { status_color(&c, case.status) };
                            cv.fill_round_clip(
                                Rect::new(rx - 46, y + ROW_H / 2 - 1, bw, 3),
                                1,
                                col.with_alpha(150),
                                list,
                            );
                        }
                        rx -= 56;
                    }
                    let nx = ix + 24;
                    let room = rx - nx;
                    let (prefix, leaf) = match case.name.rsplit_once(" › ") {
                        Some((p, l)) => (format!("{p} › "), l.to_string()),
                        None => (String::new(), case.name.clone()),
                    };
                    let name_col = match case.status {
                        Status::Skipped | Status::Pending => dim,
                        Status::Passed => strong.mix(bg, 0.18),
                        _ => strong,
                    };
                    let ny = y + (ROW_H - 18) / 2;
                    let lw = self.tx.ui_width(&leaf, false);
                    if prefix.is_empty() || lw >= room {
                        let s = fit(&mut self.tx, &case.name, room, false);
                        self.tx.draw_ui(cv, nx, ny, &s, name_col, list, false);
                    } else {
                        // The describe path is secondary: smaller and dim, the test's own name stands out.
                        let p = fit(&mut self.tx, &prefix, room - lw, false);
                        let pw = sized_w(&mut self.tx, &p, false, SMALL);
                        self.tx.draw_ui_sized(cv, nx, y + 8, &p, dim, list, false, SMALL);
                        self.tx.draw_ui(cv, nx + pw, ny, &leaf, name_col, list, false);
                    }
                    if vis.h > 0 {
                        self.tests.hits.push((vis, Hit::Case(case.id.clone())));
                    }
                }
            }
        }
        self.region_bar(cv, Reg::Tests, list, total, visible, scroll, 1.0, false);
    }

    fn draw_tests_detail(&mut self, cv: &mut Canvas, area: Rect) {
        let c = self.theme.chrome;
        let (strong, dim, bg) = (c.tab_active_fg, c.tab_inactive_fg, self.theme.background);
        let sel = self.tests.sel.as_deref().and_then(|s| self.tests.case(s)).cloned();
        let ty0 = area.y + 10;
        let mut x = area.x + 14;
        let inner_r = area.right() - 12;

        // Header (fixed): badge + title, meta, actions.
        let head = Rect::new(area.x, area.y, area.w, DETAIL_HEAD);
        if let Some(case) = &sel {
            let (label, col) = match case.status {
                Status::Passed => ("PASSED", c.git_added),
                Status::Failed => ("FAILED", c.error),
                Status::Skipped => ("SKIPPED", c.warning),
                Status::Running => ("RUNNING", c.accent_text),
                Status::Pending => ("NOT RUN", dim),
            };
            // Badge and title share one 24 px line, both vertically centered on it.
            const BADGE_H: i32 = 24;
            const BADGE_TEXT: f32 = 11.0;
            let bw = sized_w(&mut self.tx, label, true, BADGE_TEXT) + 20;
            let br = Rect::new(x, ty0, bw, BADGE_H);
            cv.fill_round_clip(br, RAD, col.with_alpha(0x2a).over(bg), head);
            let blh = (BADGE_TEXT * 1.4).round() as i32;
            self.tx.draw_ui_sized(cv, br.x + 10, br.y + (BADGE_H - blh) / 2, label, col, head, true, BADGE_TEXT);
            x += bw + 10;
            let title = case.name.rsplit_once(" › ").map(|x| x.1).unwrap_or(&case.name).to_string();
            let t = fit(&mut self.tx, &title, inner_r - x, true);
            self.tx.draw_ui(cv, x, ty0 + (BADGE_H - 18) / 2, &t, strong, head, true);

            let mut meta = case.group.clone();
            let (gn, gd) = group_label(&self.root, &case.group);
            if !gd.is_empty() {
                meta = format!("{gd}/{gn}");
            }
            if let Some(l) = case.line {
                meta = format!("{meta}:{}", l + 1);
            }
            if let Some(ms) = case.ms {
                meta = format!("{meta}  ·  {}", fmt_ms(ms));
            }
            let m = fit(&mut self.tx, &meta, area.w - 28, false);
            self.tx.draw_ui_sized(cv, area.x + 14, ty0 + 31, &m, dim, head, false, SMALL);

            let by = ty0 + 54;
            let mut bx = area.x + 14;
            let id = case.id.clone();
            let running = self.tests.proc.is_some();
            bx += self.t_button(
                cv,
                bx,
                by,
                "Run",
                Some("ui/play_filled"),
                Btn::Secondary,
                !running,
                Hit::RunOne(id.clone()),
                head,
            ) + 6;
            let can_dbg = self.tests.runner.is_some_and(|r| r.debuggable()) && !running;
            bx += self.t_button(
                cv,
                bx,
                by,
                "Debug",
                Some("ui/debug"),
                Btn::Secondary,
                can_dbg,
                Hit::DebugOne(id.clone()),
                head,
            ) + 6;
            bx += self.t_button(
                cv,
                bx,
                by,
                "Open",
                Some("ui/arrow_up_right"),
                Btn::Secondary,
                true,
                Hit::OpenOne(id),
                head,
            ) + 6;
            let on = self.tests.show_output;
            self.t_button(cv, bx, by, "Output", Some("ui/terminal"), Btn::Toggle(on), true, Hit::ShowOutput(!on), head);
        } else {
            let hint = if self.tests.log_only { "The script's output" } else { "Select a test to see its details" };
            self.tx.draw_ui(cv, x, ty0 + 3, hint, dim, head, false);
            let on = self.tests.show_output;
            self.t_button(
                cv,
                x,
                ty0 + 32,
                "Output",
                Some("ui/terminal"),
                Btn::Toggle(on),
                true,
                Hit::ShowOutput(!on),
                head,
            );
        }
        cv.fill(Rect::new(area.x, area.y + DETAIL_HEAD - 1, area.w, 1), c.border);

        // Body (scrolls).
        let body = Rect::new(area.x, area.y + DETAIL_HEAD, area.w, (area.h - DETAIL_HEAD).max(0));
        if body.h < ROW_H {
            return;
        }
        let lh = line_h() as i32;
        let cols = ((body.w - 36 - BAR_W) as f32 / self.tx.cell_w) as usize;
        let lines = self.detail_lines(sel.as_ref(), cols);
        let total = (lines.len() as i32 * lh + 16) as f64;
        let visible = body.h as f64;
        let max = (total - visible).max(0.0);
        self.tests.detail_scroll = self.tests.detail_scroll.clamp(0.0, max);
        let scroll = self.tests.detail_scroll;
        let first = (scroll / lh as f64).floor().max(0.0) as usize;
        let off = -((scroll as i32) % lh);
        for (k, l) in lines.iter().enumerate().skip(first).take((body.h / lh + 2) as usize) {
            let y = body.y + 8 + off + (k - first) as i32 * lh;
            let row = Rect::new(body.x + 10, y, body.w - 20 - BAR_W, lh);
            if let Some(b) = l.bg {
                cv.fill_clip(row, b, body);
            }
            if let Some(b) = l.bar {
                cv.fill_clip(Rect::new(row.x, y, 3, lh), b, body);
            }
            let pieces: Vec<(String, Rgba)> =
                l.segs.iter().map(|(s, col)| (s.clone(), tasks::legible(*col, bg))).collect();
            self.tx.draw(cv, row.x + 12, y, &pieces, body);
            if l.plain.contains(':') || l.plain.contains('(') {
                for lk in tasks::find_links(&l.plain) {
                    let x0 = row.x + 12 + self.tx.cells(lk.start);
                    let x1 = row.x + 12 + self.tx.cells(lk.end);
                    let ur = Rect::new(x0, y + lh - 3, (x1 - x0).max(0), 1);
                    cv.fill_clip(ur, c.accent, body);
                    let hit = Rect::new(x0, y, (x1 - x0).max(0), lh).intersect(&body);
                    if hit.w > 0 && hit.h > 0 {
                        self.tests.hits.push((hit, Hit::Link(lk.path.clone(), lk.line, lk.col)));
                    }
                }
            }
        }
        self.region_bar(cv, Reg::TestsDetail, body, total, visible, scroll, 1.0, false);
    }

    /// The text of the detail pane: the raw output, or the selected test's failure.
    fn detail_lines(&self, sel: Option<&Case>, cols: usize) -> Vec<DLine> {
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let mut out: Vec<DLine> = Vec::new();
        if self.tests.show_output || sel.is_none() {
            if self.tests.raw.is_empty() {
                out.push(DLine::text("No output yet", dim));
            }
            for (segs, plain) in self.tests.raw.iter().zip(&self.tests.raw_plain) {
                out.push(DLine { segs: segs.clone(), plain: plain.clone(), bg: None, bar: None });
            }
            return out;
        }
        let Some(case) = sel else { return out };
        match case.status {
            Status::Passed => out.push(DLine::text("This test passed.", dim)),
            Status::Skipped => out.push(DLine::text("This test was skipped.", dim)),
            Status::Pending => out.push(DLine::text("Not run yet — press Run to execute it.", dim)),
            Status::Running => out.push(DLine::text("Running…", dim)),
            Status::Failed => {}
        }
        if case.status == Status::Failed && case.message.trim().is_empty() {
            out.push(DLine::text("The runner did not report a failure message — open Output for the full log.", dim));
        }
        if case.message.trim().is_empty() {
            return out;
        }
        let err_bg = c.error.with_alpha(20);
        if case.status == Status::Failed
            && let Some(d) = testrun::diff_pair(&case.message)
        {
            for (label, text, bgc, col) in
                [(d.a_label, &d.a, c.diff_add, c.git_added), (d.b_label, &d.b, c.diff_del, c.error)]
            {
                out.push(DLine::text(label, col));
                for w in wrap_line(text, cols.saturating_sub(2)) {
                    out.push(DLine { segs: vec![(format!(" {w}"), fg)], plain: w, bg: Some(bgc), bar: Some(col) });
                }
            }
            out.push(DLine::text("", dim));
        }
        let failed = case.status == Status::Failed;
        for raw in case.message.lines() {
            let t = raw.trim_end();
            let is_stack = t.trim_start().starts_with("at ") || t.trim_start().starts_with("at\t");
            let col = if is_stack { dim } else { fg };
            for w in wrap_line(t, cols) {
                out.push(DLine {
                    segs: vec![(w.clone(), col)],
                    plain: w,
                    bg: failed.then_some(err_bg),
                    bar: failed.then_some(c.error),
                });
            }
        }
        out
    }
}

fn status_color(c: &crate::theme::Chrome, s: Status) -> Rgba {
    match s {
        Status::Passed => c.git_added,
        Status::Failed => c.error,
        Status::Skipped => c.warning,
        Status::Running => c.accent,
        Status::Pending => c.border,
    }
}

/// Eight dots on a circle with a bright head that travels around.
#[allow(clippy::too_many_arguments)]
fn spinner(cv: &mut Canvas, cx: i32, cy: i32, r: i32, col: Rgba, bg: Rgba, started: Option<Instant>, clip: Rect) {
    spinner_t(cv, cx, cy, r, col, bg, started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0), clip);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spinner_t(cv: &mut Canvas, cx: i32, cy: i32, r: i32, col: Rgba, bg: Rgba, t: f32, clip: Rect) {
    let head = ((t * 10.0) as i32).rem_euclid(8);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::TAU / 8.0;
        let age = (head - k).rem_euclid(8);
        let alpha = (255 - age * 30).clamp(40, 255) as u8;
        let (x, y) = (cx + (a.cos() * r as f32).round() as i32, cy + (a.sin() * r as f32).round() as i32);
        cv.fill_round_clip(Rect::new(x - 1, y - 1, 3, 3), 1, col.with_alpha(alpha).over(bg), clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(group: &str, name: &str, s: Status) -> Case {
        Case {
            group: group.into(),
            name: name.into(),
            id: format!("{group}::{name}"),
            status: s,
            ms: Some(5.0),
            ..Default::default()
        }
    }

    #[test]
    fn rows_group_filter_and_fold() {
        let cases = vec![
            case("a.rs", "one", Status::Passed),
            case("b.rs", "two", Status::Failed),
            case("b.rs", "three", Status::Passed),
            case("c.rs", "four", Status::Skipped),
        ];
        let none = HashSet::new();
        let rows = build_rows(&cases, Filter::All, &none, false);
        assert_eq!(rows.len(), 4 + 3, "3 groups + 4 cases");
        // Failing groups come first when asked.
        let rows = build_rows(&cases, Filter::All, &none, true);
        assert!(matches!(&rows[0], Row::Group { key, fail: 1, pass: 1, total: 2, .. } if key == "b.rs"));
        // The filter hides groups with nothing to show.
        let rows = build_rows(&cases, Filter::Failed, &none, false);
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[1], Row::Case(1)));
        // Folded groups keep their header only.
        let fold: HashSet<String> = ["b.rs".to_string()].into();
        let rows = build_rows(&cases, Filter::All, &fold, false);
        assert_eq!(rows.len(), 3 + 1 + 1);
    }

    #[test]
    fn labels_durations_and_wrapping() {
        let root = Path::new("/p");
        assert_eq!(group_label(root, "/p/src/a.test.ts"), ("a.test.ts".to_string(), "src".to_string()));
        assert_eq!(group_label(root, "tasks::tests"), ("tasks::tests".to_string(), String::new()));
        assert_eq!(group_label(root, "tests/test_a.py"), ("test_a.py".to_string(), "tests".to_string()));
        assert_eq!(group_label(root, "example.com/m/pkg"), ("example.com/m/pkg".to_string(), String::new()));
        assert_eq!(fmt_ms(0.2), "<1 ms");
        assert_eq!(fmt_ms(12.4), "12 ms");
        assert_eq!(fmt_ms(1500.0), "1.50 s");
        assert_eq!(fmt_ms(65_000.0), "1m 05s");
        assert_eq!(
            wrap_line("abcdefghij", 8),
            vec!["abcdefghij".to_string()].into_iter().flat_map(|s| wrap_line(&s, 8)).collect::<Vec<_>>()
        );
        assert_eq!(wrap_line(&"x".repeat(20), 8).len(), 3);
        assert_eq!(wrap_line("a\tb", 20), vec!["a    b".to_string()]);
    }

    #[test]
    fn row_layout_matches_the_hit_rows() {
        let cases = vec![case("a.rs", "one", Status::Passed), case("a.rs", "two", Status::Passed)];
        let rows = build_rows(&cases, Filter::All, &HashSet::new(), false);
        let (tops, total) = row_tops(&rows);
        // A group row carries its air above it; tests follow back to back.
        assert_eq!(tops, vec![LIST_PAD, LIST_PAD + GROUP_H, LIST_PAD + GROUP_H + ROW_H]);
        assert_eq!(total, LIST_PAD + GROUP_H + 2 * ROW_H + LIST_PAD);
        assert_eq!(row_h(&rows[0]), ROW_H + GROUP_GAP);
        // Keeping a row in view scrolls as little as possible.
        assert_eq!(scroll_to_show(0.0, 10, 30, 100), 0.0);
        assert_eq!(scroll_to_show(0.0, 90, 30, 100), 20.0);
        assert_eq!(scroll_to_show(50.0, 20, 30, 100), 20.0);
        assert_eq!(scroll_to_show(50.0, 20, 30, 0), 50.0, "not drawn yet: leave it");
    }

    #[test]
    fn keyboard_navigation_over_rows() {
        let cases = vec![
            case("a.rs", "one", Status::Passed),
            case("a.rs", "two", Status::Failed),
            case("b.rs", "three", Status::Passed),
        ];
        let none = HashSet::new();
        let rows = build_rows(&cases, Filter::All, &none, false);
        // rows: [a.rs, one, two, b.rs, three]
        assert_eq!(nav(&rows, &cases, &none, None, Nav::Down), NavOut::To(0));
        assert_eq!(nav(&rows, &cases, &none, None, Nav::End), NavOut::To(4));
        assert_eq!(nav(&rows, &cases, &none, Some(0), Nav::Up), NavOut::Stay);
        assert_eq!(nav(&rows, &cases, &none, Some(1), Nav::Down), NavOut::To(2));
        assert_eq!(nav(&rows, &cases, &none, Some(4), Nav::Down), NavOut::Stay);
        assert_eq!(nav(&rows, &cases, &none, Some(1), Nav::PageDown(10)), NavOut::To(4));
        assert_eq!(nav(&rows, &cases, &none, Some(3), Nav::PageUp(2)), NavOut::To(1));
        // Left climbs from a test to its group, then folds the group.
        assert_eq!(nav(&rows, &cases, &none, Some(2), Nav::Left), NavOut::To(0));
        assert_eq!(nav(&rows, &cases, &none, Some(4), Nav::Left), NavOut::To(3));
        assert_eq!(nav(&rows, &cases, &none, Some(0), Nav::Left), NavOut::Fold("a.rs".into()));
        // Right steps into an open group and unfolds a folded one.
        assert_eq!(nav(&rows, &cases, &none, Some(0), Nav::Right), NavOut::To(1));
        assert_eq!(nav(&rows, &cases, &none, Some(1), Nav::Right), NavOut::Stay);
        let fold: HashSet<String> = ["a.rs".to_string()].into();
        let folded = build_rows(&cases, Filter::All, &fold, false);
        assert_eq!(nav(&folded, &cases, &fold, Some(0), Nav::Right), NavOut::Unfold("a.rs".into()));
        assert_eq!(nav(&folded, &cases, &fold, Some(0), Nav::Left), NavOut::Stay);
        assert_eq!(nav(&[], &cases, &none, None, Nav::Down), NavOut::Stay);
        // The cursor is the parked group, else the selected test.
        assert_eq!(cursor_row(&rows, &cases, Some("b.rs"), Some("a.rs::two")), Some(3));
        assert_eq!(cursor_row(&rows, &cases, None, Some("a.rs::two")), Some(2));
        assert_eq!(cursor_row(&rows, &cases, Some("gone"), None), None);
    }

    #[test]
    fn arrow_keys_drive_the_list_while_it_has_the_focus() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("testskeys");
        let mut app = test_app(&dir);
        for c in [case("a.rs", "one", Status::Passed), case("a.rs", "two", Status::Failed)] {
            app.tests.upsert(c);
        }
        app.panel.open = true;
        app.panel.tab = PanelTab::Tests;
        app.focus = Focus::Editor;
        let key = |app: &mut App, k: NamedKey| {
            app.handle_key(&KeyIn { key: Key::Named(k), text: None });
        };
        // Without the focus the arrows belong to the editor.
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!((app.tests.sel.as_deref(), app.tests.cur_group.as_deref()), (None, None));
        app.tests_focus();
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!(app.tests.cur_group.as_deref(), Some("a.rs"), "first stop: the group row");
        // With no run in progress failures are listed first, as drawn: two (failed), then one.
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!((app.tests.sel.as_deref(), app.tests.cur_group.as_deref()), (Some("a.rs::two"), None));
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!(app.tests.sel.as_deref(), Some("a.rs::one"));
        key(&mut app, NamedKey::ArrowLeft);
        assert_eq!(app.tests.cur_group.as_deref(), Some("a.rs"));
        key(&mut app, NamedKey::ArrowLeft);
        assert!(app.tests.collapsed.contains("a.rs"), "Left on an open group folds it");
        key(&mut app, NamedKey::ArrowRight);
        assert!(!app.tests.collapsed.contains("a.rs"), "Right unfolds it");
        assert!(app.tests.focused);
        // Esc hands the keyboard back.
        key(&mut app, NamedKey::Escape);
        assert!(!app.tests.focused);
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!(app.tests.cur_group.as_deref(), Some("a.rs"), "unchanged once the focus is gone");
        // A key the list does not use gives the focus back too.
        app.tests_focus();
        key(&mut app, NamedKey::Tab);
        assert!(!app.tests.focused);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_failed_run_placeholder_is_never_a_test() {
        let mut t = TestsState::default();
        t.upsert(Case { id: RUN_FAILED.into(), status: Status::Failed, ..Default::default() });
        t.upsert(Case { id: "a::b".into(), status: Status::Failed, ..Default::default() });
        assert_eq!(t.real_failures().len(), 1);
        t.drop_placeholder();
        assert_eq!(t.cases.len(), 1);
        assert!(t.case("a::b").is_some() && t.case(RUN_FAILED).is_none());
    }

    #[test]
    fn upsert_keeps_known_location() {
        let mut t = TestsState::default();
        t.upsert(Case {
            id: "x".into(),
            file: Some("f.rs".into()),
            line: Some(3),
            status: Status::Failed,
            ..Default::default()
        });
        t.upsert(Case { id: "x".into(), status: Status::Running, ..Default::default() });
        assert_eq!(t.cases.len(), 1);
        assert_eq!(
            (t.cases[0].file.as_deref(), t.cases[0].line, t.cases[0].status),
            (Some("f.rs"), Some(3), Status::Running)
        );
        t.upsert(Case { id: "y".into(), ..Default::default() });
        assert_eq!(t.cases.len(), 2);
        assert!(t.case("y").is_some() && t.case("z").is_none());
    }

    #[test]
    fn target_picker_lists_auto_then_structured_then_log_only() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("teststargets");
        std::fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"test":"vitest --project unit","test:e2e":"playwright test","test:visual":"./run.sh","lint":"eslint ."},"devDependencies":{"vitest":"1"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        let mut app = test_app(&dir);
        app.tests_open_panel();
        let items = app.tests_target_items();
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, vec!["Auto (vitest)", "test", "test:e2e", "test:visual"]);
        assert_eq!(items[0].detail, "detected runner · current");
        assert_eq!(items[1].detail, "vitest · vitest --project unit");
        assert_eq!(items[2].detail, "playwright · playwright test");
        assert_eq!(items[3].detail, "log only · ./run.sh");
        assert!(matches!(&items[0].action, Action::TestRunner(Runner::Vitest)));
        assert!(matches!(&items[3].action, Action::TestTarget(id) if id == "script::test:visual"));
        // A structured script becomes the target and the runner follows it.
        app.tests_set_target("script::test:e2e");
        assert_eq!(
            (app.tests.target.as_deref(), app.tests.runner),
            (Some("script::test:e2e"), Some(Runner::Playwright))
        );
        assert!(app.tests_target_items()[2].detail.ends_with(" · current"));
        assert!(!app.tests.log_only_target());
        // Log only: file and single-test runs are refused (nothing is started).
        app.tests_set_target("script::test:visual");
        assert!(app.tests.log_only_target());
        app.tests_start(Last::File("a.test.ts".into()));
        assert!(app.tests.proc.is_none());
        // An unknown id changes nothing; Auto clears the target.
        app.tests_set_target("script::gone");
        assert_eq!(app.tests.target.as_deref(), Some("script::test:visual"));
        app.tests_set_runner(Runner::Vitest);
        assert_eq!((app.tests.target.as_deref(), app.tests.runner), (None, Some(Runner::Vitest)));
        // The remembered choice comes back only while the script exists.
        let ts = app.tests.scripts.clone();
        assert_eq!(restore_target(Some("script::test:e2e"), &ts).as_deref(), Some("script::test:e2e"));
        assert_eq!(restore_target(Some("script::gone"), &ts), None);
        assert_eq!(restore_target(Some(""), &ts), None);
        assert_eq!(restore_target(None, &ts), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
