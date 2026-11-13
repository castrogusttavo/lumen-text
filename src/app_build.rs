//! The Build panel: build, run and clean the project with the tool it is set up for, read the
//! compiler's problems grouped by file next to the colored log, and jump to them. The problems
//! also reach the editor as diagnostics (source "build"). Detection, commands and the problem
//! matchers live in `buildrun.rs`.

use crate::{
    app::{App, Focus, KeyIn},
    app_gitpanel::fit,
    app_overlay::{Action, Item},
    app_panel::PanelTab,
    app_tests::{group_label, scroll_to_show, spinner_t},
    app_topbar::StatusBtn,
    buildrun::{self, Counts, Kind, Matcher, Problem, Sev, Target},
    doc::Diag,
    scrollreg::Reg,
    tasks::{self, Ansi, Event, Running, Segs},
    theme::{Chrome, Rgba},
    ui::{Canvas, Rect, line_h},
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use winit::keyboard::{Key, NamedKey};

const TOOLBAR_H: i32 = 40;
const PROG_H: i32 = 3;
const FILTER_H: i32 = 32;
/// Height of a problem row; a file row adds `GROUP_GAP` of air above it (no separator lines).
const ROW_H: i32 = 30;
const GROUP_GAP: i32 = 4;
const GROUP_H: i32 = ROW_H + GROUP_GAP;
/// Padding above the first and below the last row of the list.
const LIST_PAD: i32 = 2;
/// Buttons and chips: the git panel's pill height and corner radius.
const BTN_H: i32 = 26;
const CHIP_H: i32 = 24;
const RAD: i32 = 8;
/// Secondary text (folders, positions, codes).
const SMALL: f32 = 12.0;
const MIN_PANEL_H: i32 = 340;
const MAX_LOG: usize = 8000;
const BAR_W: i32 = 11;
/// Source name of the diagnostics the editor gets from a build.
pub const SOURCE: &str = "build";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Errors,
    Warnings,
}

impl Filter {
    fn accepts(self, s: Sev) -> bool {
        match self {
            Filter::All => true,
            Filter::Errors => s == Sev::Error,
            Filter::Warnings => s == Sev::Warning,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Job {
    Build,
    Run,
    Clean,
}

impl Job {
    /// The log its output goes to.
    fn log(self) -> LogView {
        if self == Job::Run { LogView::Run } else { LogView::Build }
    }

    fn verb(self) -> &'static str {
        match self {
            Job::Build => "Building",
            Job::Run => "Running",
            Job::Clean => "Cleaning",
        }
    }
}

/// Which output the log pane shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogView {
    Build = 0,
    Run = 1,
}

/// How a job ended.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Done {
    pub job: Job,
    pub code: i32,
    pub ms: f64,
    pub stopped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Target,
    Build,
    Run,
    Stop,
    Clean,
    Filter(Filter),
    View(LogView),
    File(PathBuf),
    Problem(usize),
    Link(String, usize, usize),
}

/// One job's output: colored lines, their plain text (links), and the scroll state.
pub struct Log {
    pub lines: Vec<Segs>,
    pub plain: Vec<String>,
    ansi: Ansi,
    /// Lines dropped from the front, so problems keep pointing at their line.
    pub base: usize,
    pub scroll: f64,
    /// Stick to the newest line (scrolling up drops it, scrolling to the end brings it back).
    pub follow: bool,
}

impl Log {
    fn new() -> Log {
        Log { lines: Vec::new(), plain: Vec::new(), ansi: Ansi::new(), base: 0, scroll: 0.0, follow: true }
    }

    fn clear(&mut self) {
        *self = Log::new();
    }

    /// Append a raw output line; returns its number counted from the first line of the job.
    fn push(&mut self, raw: &str, fg: Rgba, dim: Rgba) -> usize {
        let segs = self.ansi.line(raw, fg, dim);
        let plain: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        self.lines.push(segs);
        self.plain.push(plain);
        if self.lines.len() > MAX_LOG {
            let drop = self.lines.len() - MAX_LOG;
            self.lines.drain(..drop);
            self.plain.drain(..drop);
            self.base += drop;
        }
        self.base + self.lines.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

pub struct BuildState {
    pub targets: Vec<Target>,
    /// (root, build_command, run_command) the targets were detected for.
    detected_for: Option<(PathBuf, String, String)>,
    /// Id of the chosen target (remembered per project).
    pub selected: Option<String>,
    proc: Option<Running>,
    pub job: Option<Job>,
    job_label: String,
    /// Folder of the running (or last) job: relative paths in its output start there.
    cwd: PathBuf,
    matcher: Option<Matcher>,
    pub started: Option<Instant>,
    anim_at: Option<Instant>,
    /// The last job that ended, whatever it was (the toolbar status).
    pub last: Option<Done>,
    /// The last job that produced the problems (a build, a run that compiles, a clean).
    pub outcome: Option<Done>,
    pub problems: Vec<Problem>,
    /// Files that have problems (documents opened later pick theirs up).
    files: HashSet<PathBuf>,
    /// The log the problems were read from.
    prob_log: LogView,
    pub logs: [Log; 2],
    pub view: LogView,
    pub filter: Filter,
    pub collapsed: HashSet<PathBuf>,
    /// Selected problem (index into `problems`).
    pub sel: Option<usize>,
    /// Keyboard cursor parked on a file row; `None` means it is on the selected problem.
    pub cur_file: Option<PathBuf>,
    pub scroll: f64,
    pub hits: Vec<(Rect, Hit)>,
    /// The list has the keyboard (a click in the panel; any other click or an unused key drops it).
    pub focused: bool,
    list_view: i32,
    log_view: i32,
    /// The problems changed: the documents' build diagnostics are rebuilt on the next tick.
    diags_dirty: bool,
}

impl Default for BuildState {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            detected_for: None,
            selected: None,
            proc: None,
            job: None,
            job_label: String::new(),
            cwd: PathBuf::new(),
            matcher: None,
            started: None,
            anim_at: None,
            last: None,
            outcome: None,
            problems: Vec::new(),
            files: HashSet::new(),
            prob_log: LogView::Build,
            logs: [Log::new(), Log::new()],
            view: LogView::Build,
            filter: Filter::All,
            collapsed: HashSet::new(),
            sel: None,
            cur_file: None,
            scroll: 0.0,
            hits: Vec::new(),
            focused: false,
            list_view: 0,
            log_view: 0,
            diags_dirty: false,
        }
    }
}

impl BuildState {
    /// Forget everything about the previous project (the running job is killed).
    pub fn reset(&mut self) {
        if let Some(mut r) = self.proc.take() {
            tasks::kill_group(r.pid);
            let _ = r.child.wait();
        }
        let had = !self.files.is_empty();
        *self = BuildState::default();
        self.diags_dirty = had;
    }

    pub fn running(&self) -> bool {
        self.proc.is_some()
    }

    /// Process group of the running job (it leads its own session), for the exit teardown.
    pub fn job_group(&self) -> Option<u32> {
        self.proc.as_ref().and_then(|r| u32::try_from(r.pid).ok())
    }

    /// The job that is alive, for a question naming it ("my-app (run)"); `None` when idle.
    pub fn running_label(&self) -> Option<String> {
        let job = self.job.filter(|_| self.running())?;
        let what = match job {
            Job::Build => "build",
            Job::Run => "run",
            Job::Clean => "clean",
        };
        Some(if self.job_label.is_empty() {
            format!("Build & Run ({what})")
        } else {
            format!("{} ({what})", self.job_label)
        })
    }

    pub fn counts(&self) -> Counts {
        Counts::of(&self.problems)
    }

    fn target(&self, id: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.id == id)
    }

    pub fn selected_target(&self) -> Option<&Target> {
        self.selected.as_deref().and_then(|id| self.target(id))
    }

    fn clear_problems(&mut self) {
        self.problems.clear();
        self.files.clear();
        self.sel = None;
        self.cur_file = None;
        self.collapsed.clear();
        self.scroll = 0.0;
        self.diags_dirty = true;
    }

    fn add_problem(&mut self, p: Problem) {
        self.files.insert(p.file.clone());
        self.problems.push(p);
        self.diags_dirty = true;
    }

    /// The last build (or compiling run) failed: what the status bar keeps showing.
    pub fn failed(&self) -> bool {
        self.proc.is_none()
            && self.outcome.is_some_and(|d| {
                !d.stopped && d.job != Job::Clean && (self.counts().errors > 0 || (d.job == Job::Build && d.code != 0))
            })
    }
}

// ───────────── rows ─────────────

pub enum Row {
    File { file: PathBuf, errors: usize, warnings: usize, infos: usize },
    Item(usize),
}

/// The problems as listed: one row per file (files with errors first), its problems in line
/// order unless folded, honoring the severity filter.
pub fn build_rows(ps: &[Problem], filter: Filter, collapsed: &HashSet<PathBuf>) -> Vec<Row> {
    let mut order: Vec<PathBuf> = Vec::new();
    let mut by: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, p) in ps.iter().enumerate() {
        by.entry(p.file.clone())
            .or_insert_with(|| {
                order.push(p.file.clone());
                Vec::new()
            })
            .push(i);
    }
    order.sort_by_key(|f| !by[f].iter().any(|&i| ps[i].sev == Sev::Error));
    let mut rows = Vec::new();
    for f in order {
        let ids = &by[&f];
        let mut vis: Vec<usize> = ids.iter().copied().filter(|&i| filter.accepts(ps[i].sev)).collect();
        if vis.is_empty() {
            continue;
        }
        vis.sort_by_key(|&i| (ps[i].line, ps[i].col));
        let c = Counts::of(ids.iter().map(|&i| &ps[i]));
        let open = !collapsed.contains(&f);
        rows.push(Row::File { file: f, errors: c.errors, warnings: c.warnings, infos: c.infos });
        if open {
            rows.extend(vis.into_iter().map(Row::Item));
        }
    }
    rows
}

pub fn row_h(r: &Row) -> i32 {
    match r {
        Row::File { .. } => GROUP_H,
        Row::Item(_) => ROW_H,
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
    To(usize),
    Fold(PathBuf),
    Unfold(PathBuf),
    Stay,
}

/// What a navigation key does with the cursor at row `at`. Left on a problem climbs to its file,
/// Left on an open file folds it, Right unfolds a folded file and steps into an open one.
pub fn nav(rows: &[Row], ps: &[Problem], collapsed: &HashSet<PathBuf>, at: Option<usize>, n: Nav) -> NavOut {
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
            Row::File { file, .. } if !collapsed.contains(file) => NavOut::Fold(file.clone()),
            Row::File { .. } => NavOut::Stay,
            Row::Item(i) => {
                let f = &ps[*i].file;
                rows[..k]
                    .iter()
                    .rposition(|r| matches!(r, Row::File { file, .. } if file == f))
                    .map_or(NavOut::Stay, NavOut::To)
            }
        },
        Nav::Right => match &rows[k] {
            Row::File { file, .. } if collapsed.contains(file) => NavOut::Unfold(file.clone()),
            Row::File { .. } if matches!(rows.get(k + 1), Some(Row::Item(_))) => NavOut::To(k + 1),
            _ => NavOut::Stay,
        },
    }
}

/// Row of the keyboard cursor: the file it is parked on, else the selected problem.
pub fn cursor_row(rows: &[Row], cur_file: Option<&Path>, sel: Option<usize>) -> Option<usize> {
    if let Some(f) = cur_file
        && let Some(k) = rows.iter().position(|r| matches!(r, Row::File { file, .. } if file.as_path() == f))
    {
        return Some(k);
    }
    let sel = sel?;
    rows.iter().position(|r| matches!(r, Row::Item(i) if *i == sel))
}

/// `3.2 s`, `1m 05s`.
pub fn fmt_secs(ms: f64) -> String {
    if ms < 60_000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        let s = (ms / 1000.0) as u64;
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn sev_color(c: &Chrome, s: Sev) -> Rgba {
    match s {
        Sev::Error => c.error,
        Sev::Warning => c.warning,
        Sev::Info => c.info,
    }
}

fn sev_icon(s: Sev) -> &'static str {
    match s {
        Sev::Error => "ui/x_circle",
        Sev::Warning => "ui/warning",
        Sev::Info => "ui/info",
    }
}

/// A problem as an editor diagnostic: the word at its column is underlined.
fn to_diag(p: &Problem, buf: &crate::buffer::Buffer) -> Diag {
    let line = p.line.min(buf.line_count().saturating_sub(1));
    let text: Vec<char> = buf.line(line).chars().collect();
    let (sc, ec) = buildrun::span(&text, p.col);
    Diag {
        sl: line,
        sc,
        el: line,
        ec,
        severity: p.sev.lsp(),
        message: p.message.clone(),
        code: p.code.clone(),
        source: SOURCE.to_string(),
    }
}

/// Where the chosen target of a project is remembered (`None` in tests: nothing is written).
fn state_path(root: &Path) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let base = crate::paths::state_home()?;
    let h =
        root.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    Some(base.join("lumen-text").join(format!("build-{h:016x}.json")))
}

pub fn load_target(path: &Path) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?).ok()?;
    v["target"].as_str().map(str::to_string)
}

pub fn save_target(path: &Path, id: &str) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(path, serde_json::json!({ "target": id }).to_string());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Primary,
    Secondary,
    Danger,
}

/// What leads the toolbar status: the dot spinner, a still dot (a Run), an icon, or nothing.
enum Glyph {
    Spin,
    Dot(Rgba),
    Icon(&'static str, Rgba),
    None,
}

/// Elapsed time of a Run in whole seconds (it repaints once a second, so no tenths).
fn fmt_elapsed(ms: f64) -> String {
    let s = (ms / 1000.0) as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s / 60) % 60),
    }
}

/// A still status dot centered on (`cx`, `cy`).
fn status_dot(cv: &mut Canvas, cx: i32, cy: i32, col: Rgba, clip: Rect) {
    cv.fill_round_clip(Rect::new(cx - 4, cy - 4, 8, 8), 4, col, clip);
}

impl App {
    // ───────────── targets ─────────────

    /// Detect the targets again when the project or the custom commands changed (`force`: always,
    /// the files may have changed).
    fn build_detect(&mut self, force: bool) {
        let key = (self.root.clone(), self.config.build_command.clone(), self.config.run_command.clone());
        let prev = self.build.detected_for.clone();
        if !force && prev.as_ref() == Some(&key) {
            return;
        }
        self.build.targets = buildrun::detect(&key.0, &key.1, &key.2);
        let same_root = prev.as_ref().is_some_and(|p| p.0 == key.0);
        if !same_root {
            self.build.selected = state_path(&key.0).and_then(|p| load_target(&p));
        }
        // A custom build command that was just set is what the user wants Build to run.
        if same_root && prev.as_ref().is_some_and(|p| p.1 != key.1) && !key.1.trim().is_empty() {
            self.build.selected = Some(buildrun::CUSTOM_BUILD.to_string());
        }
        let known = self.build.selected.as_deref().is_some_and(|id| self.build.target(id).is_some());
        if !known {
            self.build.selected = buildrun::default_target(&self.build.targets).map(|t| t.id.clone());
        }
        self.build.detected_for = Some(key);
    }

    fn build_ensure_detect(&mut self) {
        self.build_detect(false);
    }

    /// The target picker: everything detected, grouped by tool (cleans are the Clean button's).
    pub fn build_pick_target(&mut self) {
        self.build_detect(true);
        if self.build.targets.is_empty() {
            self.build_open_panel();
            return self.say("no build system detected — set build_command in Settings → Build");
        }
        let cur = self.build.selected.clone();
        let items: Vec<Item> = self
            .build
            .targets
            .iter()
            .filter(|t| t.kind != Kind::Clean)
            .map(|t| {
                let mut detail = format!("{} · {}", t.group, t.kind.label());
                if cur.as_deref() == Some(t.id.as_str()) {
                    detail.push_str(" · current");
                }
                Item { label: t.label.clone(), detail, action: Action::BuildTarget(t.id.clone()) }
            })
            .collect();
        self.open_locations("Build target", items);
    }

    pub fn build_set_target(&mut self, id: &str) {
        self.build_ensure_detect();
        let Some(label) = self.build.target(id).map(|t| t.label.clone()) else { return };
        self.build.selected = Some(id.to_string());
        if let Some(p) = state_path(&self.root) {
            save_target(&p, id);
        }
        self.say(format!("build target: {label}"));
    }

    pub fn build_open_panel(&mut self) {
        self.build_ensure_detect();
        self.panel.open = true;
        self.panel.tab = PanelTab::Build;
        self.focus = Focus::Editor;
        if self.panel.height < MIN_PANEL_H {
            self.panel.height = MIN_PANEL_H;
        }
    }

    /// The status bar indicator: open the panel, or close it when it already shows.
    pub fn build_toggle_panel(&mut self) {
        if self.panel.open && self.panel.tab == PanelTab::Build {
            self.toggle_panel();
        } else {
            self.build_open_panel();
        }
    }

    /// Give the problem list the keyboard (clicking the Build tab or the panel).
    pub fn build_focus(&mut self) {
        self.build.focused = true;
    }

    // ───────────── jobs ─────────────

    pub fn build_build(&mut self) {
        self.build_start(Job::Build);
    }

    pub fn build_run(&mut self) {
        self.build_start(Job::Run);
    }

    pub fn build_clean(&mut self) {
        self.build_start(Job::Clean);
    }

    fn build_push(&mut self, log: LogView, line: &str) -> usize {
        let (fg, dim) = (self.theme.foreground, self.theme.chrome.tab_inactive_fg);
        self.build.logs[log as usize].push(line, fg, dim)
    }

    fn build_start(&mut self, job: Job) {
        self.build_stop_quiet();
        self.build_ensure_detect();
        let target = {
            let ts = &self.build.targets;
            let sel = self.build.selected_target();
            let t = match job {
                Job::Build => buildrun::build_for(ts, sel),
                Job::Run => buildrun::run_for(ts, sel),
                Job::Clean => buildrun::clean_for(ts, sel),
            };
            t.cloned()
        };
        let Some(t) = target else {
            self.build_open_panel();
            let msg = match job {
                Job::Build => "no build system detected — set build_command in Settings → Build",
                Job::Run => "nothing to run — set run_command in Settings → Build",
                Job::Clean => {
                    self.build.clear_problems();
                    self.build.outcome = None;
                    self.build.last = None;
                    "build problems cleared (no clean command in this project)"
                }
            };
            return self.say(msg);
        };
        let cmd = buildrun::prepare(&t);
        let r = match tasks::spawn(&cmd, &t.cwd, self.wake()) {
            Ok(r) => r,
            Err(e) => return self.say(format!("could not run {}: {e}", t.label)),
        };
        // A build, a run that compiles and a clean all start the problems over.
        let matches = job == Job::Build || (job == Job::Run && t.compiles);
        let log = job.log();
        let b = &mut self.build;
        if matches || job == Job::Clean {
            b.clear_problems();
            b.prob_log = log;
            b.outcome = None;
        }
        b.matcher = matches.then(|| Matcher::new(&self.root, &t.cwd));
        b.logs[log as usize].clear();
        b.view = log;
        b.proc = Some(r);
        b.job = Some(job);
        b.job_label = t.label.clone();
        b.cwd = t.cwd.clone();
        b.started = Some(Instant::now());
        b.anim_at = None;
        self.build_push(log, &format!("$ {cmd}"));
        self.build_open_panel();
        self.request_redraw();
    }

    /// Drain the running job's output; called whenever the event loop is woken.
    pub fn pump_build(&mut self) {
        let Some(r) = &self.build.proc else { return };
        let mut lines: Vec<String> = Vec::new();
        let mut done = false;
        while let Ok(ev) = r.rx.try_recv() {
            match ev {
                Event::Line(l) => lines.push(l),
                Event::Done => done = true,
            }
        }
        let log = self.build.job.unwrap_or(Job::Build).log();
        for l in lines {
            let n = self.build_push(log, &l);
            if let Some(p) = self.build.matcher.as_mut().and_then(|m| m.feed(&l, n)) {
                self.build.add_problem(p);
            }
        }
        if done {
            self.build_finish();
        }
        self.request_redraw();
    }

    fn build_end(&mut self, code: i32, stopped: bool) -> Done {
        let ms = self.build.started.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        let job = self.build.job.take().unwrap_or(Job::Build);
        let matched = self.build.matcher.take().is_some();
        let done = Done { job, code, ms, stopped };
        self.build.last = Some(done);
        if matched || job == Job::Clean {
            self.build.outcome = Some(done);
        }
        done
    }

    fn build_finish(&mut self) {
        let Some(mut r) = self.build.proc.take() else { return };
        let code = r.child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
        let d = self.build_end(code, false);
        let mark = if code == 0 { "\u{f00c}" } else { "\u{f00d}" };
        self.build_push(d.job.log(), &format!("{mark} exited with code {code} in {}", fmt_secs(d.ms)));
        let c = self.build.counts();
        if c.total() > 0 && self.build.sel.is_none() {
            // The first error as listed is selected (the log keeps showing the end).
            let rows = build_rows(&self.build.problems, self.build.filter, &self.build.collapsed);
            let items: Vec<usize> = rows
                .iter()
                .filter_map(|r| match r {
                    Row::Item(i) => Some(*i),
                    Row::File { .. } => None,
                })
                .collect();
            let ps = &self.build.problems;
            self.build.sel =
                items.iter().copied().find(|&i| ps[i].sev == Sev::Error).or_else(|| items.first().copied());
        }
        let t = fmt_secs(d.ms);
        let label = self.build.job_label.clone();
        self.say(match d.job {
            Job::Build if code == 0 && c.warnings > 0 => {
                format!("build succeeded in {t} with {}", plural(c.warnings, "warning"))
            }
            Job::Build if code == 0 => format!("build succeeded in {t}"),
            Job::Build if c.total() == 0 => format!("build failed (exit code {code})"),
            Job::Build => format!("build failed — {}", buildrun::summary(c)),
            Job::Run if c.errors > 0 => format!("{label}: {}", buildrun::summary(c)),
            Job::Run => format!("{label} exited with code {code}"),
            Job::Clean if code == 0 => format!("clean done in {t}"),
            Job::Clean => format!("clean failed (exit code {code})"),
        });
        self.request_redraw();
    }

    pub(crate) fn build_stop_quiet(&mut self) -> bool {
        let Some(mut r) = self.build.proc.take() else { return false };
        tasks::kill_group(r.pid);
        let _ = r.child.wait();
        let d = self.build_end(-1, true);
        self.build_push(d.job.log(), "■ stopped");
        true
    }

    pub fn build_stop(&mut self) {
        if self.build_stop_quiet() {
            self.say("build: stopped");
            self.request_redraw();
        }
    }

    /// Timers: the spinners while a job runs, and the documents' build diagnostics.
    pub fn build_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if let Some(step) = self.build_anim_step() {
            let due = self.build.anim_at.map_or(now, |t| t + step);
            if due <= now {
                self.build.anim_at = Some(now);
                self.request_redraw();
                want(now + step);
            } else {
                want(due);
            }
        }
        self.build_sync_diags();
    }

    /// How often the running job repaints. A build or clean ends soon: spinner and progress at
    /// ~11 fps. A Run is often a dev server that lives for hours: no animation at all (a static
    /// "running" dot), only the elapsed time ticking once a second while the Build panel shows it —
    /// otherwise an idle editor would repaint forever.
    fn build_anim_step(&self) -> Option<Duration> {
        match self.build.job.filter(|_| self.build.running())? {
            Job::Run => (self.panel.open && self.panel.tab == PanelTab::Build).then_some(Duration::from_secs(1)),
            _ => Some(Duration::from_millis(90)),
        }
    }

    /// Give open documents the build problems of their file as diagnostics (source "build"):
    /// all of them after the problems changed, and documents opened since then.
    pub fn build_sync_diags(&mut self) {
        let dirty = std::mem::take(&mut self.build.diags_dirty);
        if !dirty && self.build.files.is_empty() {
            return;
        }
        for d in &mut self.docs {
            let Some(path) = d.path().map(Path::to_path_buf) else { continue };
            if dirty {
                d.set_ext_diags(SOURCE, Vec::new());
            } else if d.has_ext_diags(SOURCE) {
                continue;
            }
            if !self.build.files.contains(&path) {
                continue;
            }
            let diags: Vec<Diag> =
                self.build.problems.iter().filter(|p| p.file == path).map(|p| to_diag(p, &d.buf)).collect();
            d.set_ext_diags(SOURCE, diags);
        }
    }

    // ───────────── input ─────────────

    fn build_hit_at(&self, x: f64, y: f64) -> Option<&Hit> {
        if !self.panel.open || self.panel.tab != PanelTab::Build {
            return None;
        }
        self.build.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h)
    }

    pub fn build_clickable_at(&self, x: f64, y: f64) -> bool {
        self.overlay.is_none() && self.build_hit_at(x, y).is_some()
    }

    /// Hover tint for everything clickable in the panel, in the shape of the thing drawn; the row
    /// that already carries the selection gets none, like the git panel.
    pub fn build_hover_target(&self) -> Option<(Rect, i32)> {
        if !self.overlay.is_none() || !self.panel.open || self.panel.tab != PanelTab::Build {
            return None;
        }
        let (x, y) = self.mouse;
        let (r, h) = self.build.hits.iter().rev().find(|(r, _)| r.contains(x, y))?;
        match h {
            Hit::Link(..) => None,
            Hit::Filter(f) if self.build.filter == *f => None,
            Hit::View(v) if self.build.view == *v => None,
            Hit::Problem(i) if self.build.sel == Some(*i) => None,
            Hit::File(f) if self.build.focused && self.build.cur_file.as_ref() == Some(f) => None,
            _ => Some((*r, RAD)),
        }
    }

    /// Is the pointer over `r` (labels brighten; the tint itself is the app's hover rect)?
    fn b_hv(&self, r: Rect) -> bool {
        self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1)
    }

    pub fn build_click(&mut self, x: f64, y: f64) {
        self.build.focused = true;
        let Some(hit) = self.build_hit_at(x, y).cloned() else { return };
        match hit {
            Hit::Target => self.build_pick_target(),
            Hit::Build => self.build_build(),
            Hit::Run => self.build_run(),
            Hit::Stop => self.build_stop(),
            Hit::Clean => self.build_clean(),
            Hit::Filter(f) => {
                self.build.filter = f;
                self.build.scroll = 0.0;
            }
            Hit::View(v) => self.build.view = v,
            Hit::File(f) => {
                if !self.build.collapsed.remove(&f) {
                    self.build.collapsed.insert(f.clone());
                }
                self.build.cur_file = Some(f);
            }
            Hit::Problem(i) => {
                self.build_select(i);
                self.build_open_problem(i);
            }
            Hit::Link(path, line, col) => {
                let cwd =
                    if self.build.cwd.as_os_str().is_empty() { self.root.clone() } else { self.build.cwd.clone() };
                match buildrun::resolve(&self.root, &cwd, &path).filter(|p| p.is_file()) {
                    Some(p) => {
                        self.open_path(&p, Some((line, col)));
                    }
                    None => self.say(format!("file not found: {path}")),
                }
            }
        }
    }

    /// Select problem `i`, park the keyboard cursor on it and show its line in the log.
    fn build_select(&mut self, i: usize) {
        self.build.sel = Some(i);
        self.build.cur_file = None;
        let Some(at) = self.build.problems.get(i).map(|p| p.log) else { return };
        let view = self.build.prob_log;
        self.build.view = view;
        let lh = line_h() as i32;
        let height = self.build.log_view;
        let log = &mut self.build.logs[view as usize];
        if at >= log.base && height > 0 {
            let k = (at - log.base) as i32;
            log.follow = false;
            log.scroll = scroll_to_show(log.scroll, 8 + (k - 1) * lh, 4 * lh, height);
        }
    }

    fn build_open_problem(&mut self, i: usize) {
        let Some(p) = self.build.problems.get(i).cloned() else { return };
        if p.file.is_file() {
            self.open_path(&p.file, Some((p.line, p.col)));
        } else {
            let rel = self.rel(&p.file);
            self.say(format!("file not found: {rel}"));
        }
    }

    /// Keys while the list has the focus: arrows move the cursor (Left/Right fold and unfold
    /// files), Enter opens the problem (or folds the file), Space shows it in the log, Esc gives
    /// the keyboard back. Anything else hands the focus back to the editor unconsumed.
    pub fn build_key(&mut self, ev: &KeyIn) -> bool {
        if !self.build.focused {
            return false;
        }
        if !self.panel.open || self.panel.tab != PanelTab::Build || self.focus != Focus::Editor {
            self.build.focused = false;
            return false;
        }
        let Key::Named(k) = &ev.key else {
            self.build.focused = false;
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
            self.build.focused = false;
            return false;
        }
        let rows = build_rows(&self.build.problems, self.build.filter, &self.build.collapsed);
        let at = cursor_row(&rows, self.build.cur_file.as_deref(), self.build.sel);
        let page = ((self.build.list_view / ROW_H).max(2) - 1) as usize;
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
                self.build.focused = false;
                return true;
            }
            NamedKey::Enter | NamedKey::Space => {
                match at.map(|j| &rows[j]) {
                    Some(Row::File { file, .. }) => {
                        let f = file.clone();
                        if !self.build.collapsed.remove(&f) {
                            self.build.collapsed.insert(f);
                        }
                    }
                    Some(Row::Item(i)) => {
                        let i = *i;
                        if *k == NamedKey::Enter {
                            self.build_open_problem(i);
                        } else {
                            self.build_select(i);
                        }
                    }
                    None => {}
                }
                return true;
            }
            _ => {
                self.build.focused = false;
                return false;
            }
        };
        match nav(&rows, &self.build.problems, &self.build.collapsed, at, n) {
            NavOut::To(j) => {
                match &rows[j] {
                    Row::File { file, .. } => self.build.cur_file = Some(file.clone()),
                    Row::Item(i) => {
                        let i = *i;
                        self.build_select(i);
                    }
                }
                let (tops, _) = row_tops(&rows);
                self.build.scroll = scroll_to_show(self.build.scroll, tops[j], row_h(&rows[j]), self.build.list_view);
            }
            NavOut::Fold(f) => {
                self.build.collapsed.insert(f.clone());
                self.build.cur_file = Some(f);
            }
            NavOut::Unfold(f) => {
                self.build.collapsed.remove(&f);
                self.build.cur_file = Some(f);
            }
            NavOut::Stay => {}
        }
        true
    }

    // ───────────── drawing ─────────────

    /// A pill button in the git panel's style (tinted fill, 8 px corners); returns its width.
    /// Disabled buttons are drawn faded and get no hit (so no hover either).
    #[allow(clippy::too_many_arguments)]
    fn b_button(
        &mut self,
        cv: &mut Canvas,
        x: i32,
        y: i32,
        label: &str,
        icon: &str,
        style: Btn,
        enabled: bool,
        hit: Hit,
        clip: Rect,
    ) -> i32 {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let bold = style == Btn::Primary;
        let tw = self.tx.ui_width(label, bold);
        let w = 24 + tw + 20;
        let r = Rect::new(x, y, w, BTN_H);
        let (fill, text, ic) = match style {
            Btn::Primary if enabled => (c.accent, c.on_accent, c.on_accent),
            Btn::Primary => (c.accent.with_alpha(0x48), strong.with_alpha(0xa0), strong.with_alpha(0xa0)),
            Btn::Danger => (c.error.with_alpha(0x33), c.error, c.error),
            Btn::Secondary => {
                let col = if enabled { strong } else { dim };
                (strong.with_alpha(0x0e), col, if enabled { c.accent_text } else { dim })
            }
        };
        let (text, ic) = if enabled { (text, ic) } else { (text.with_alpha(0x80), ic.with_alpha(0x80)) };
        cv.fill_round_clip(r, RAD, fill, clip);
        self.tx.icon(cv, icon, x + 12, y + (BTN_H - 14) / 2, 14, ic, clip);
        self.tx.draw_ui(cv, x + 32, y + (BTN_H - 18) / 2, label, text, clip, bold);
        if enabled {
            self.build.hits.push((r, hit));
        }
        w
    }

    /// The target selector: the tool's icon, the chosen target and a chevron.
    fn b_select(&mut self, cv: &mut Canvas, x: i32, y: i32, clip: Rect) -> i32 {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let enabled = self.build.targets.iter().any(|t| t.kind != Kind::Clean);
        let sel = self.build.selected_target().map(|t| (t.label.clone(), t.tool.marker()));
        let (label, icon) = match sel {
            Some((l, Some(m))) => (l, self.tx.icons.file_icon(m)),
            Some((l, None)) => (l, "ui/terminal".to_string()),
            None => ("No target".to_string(), "ui/tool_hammer".to_string()),
        };
        let label = fit(&mut self.tx, &label, 240, false);
        let tw = self.tx.ui_width(&label, false);
        let w = 10 + 16 + 8 + tw + 10 + 14 + 10;
        let r = Rect::new(x, y, w, BTN_H);
        cv.fill_round_clip(r, RAD, strong.with_alpha(0x0e), clip);
        let (ic, text) = if enabled { (c.accent_text, strong) } else { (dim, dim) };
        self.tx.icon(cv, &icon, x + 10, y + (BTN_H - 16) / 2, 16, ic, clip);
        self.tx.draw_ui(cv, x + 34, y + (BTN_H - 18) / 2, &label, text, clip, false);
        let chev = if enabled && self.b_hv(r) { strong } else { dim };
        self.tx.icon(cv, "ui/chevron_down", r.right() - 24, y + (BTN_H - 14) / 2, 14, chev, clip);
        if enabled {
            self.build.hits.push((r, Hit::Target));
        }
        w
    }

    /// The toolbar's live status: what runs and for how long, or how the last job ended.
    fn build_status(&self) -> (Glyph, Vec<(String, Rgba)>) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        let counts = self.build.counts();
        let mut v: Vec<(String, Rgba)> = Vec::new();
        if let Some(job) = self.build.job {
            let ms = self.build.started.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
            let elapsed = if job == Job::Run { fmt_elapsed(ms) } else { fmt_secs(ms) };
            v.push((format!("{}…  {elapsed}", job.verb()), dim));
            if counts.errors > 0 {
                v.push(("   ".into(), dim));
                v.push((plural(counts.errors, "error"), c.error));
            }
            if counts.warnings > 0 {
                v.push(("  ".into(), dim));
                v.push((plural(counts.warnings, "warning"), c.warning));
            }
            let glyph = if job == Job::Run { Glyph::Dot(c.git_added) } else { Glyph::Spin };
            return (glyph, v);
        }
        let Some(d) = self.build.last else { return (Glyph::None, v) };
        if d.stopped {
            v.push(("Stopped".into(), dim));
            return (Glyph::Icon("ui/stop", dim), v);
        }
        let t = fmt_secs(d.ms);
        match d.job {
            Job::Build if d.code == 0 => {
                v.push((format!("Succeeded in {t}"), c.git_added));
                if counts.warnings > 0 {
                    v.push(("  ·  ".into(), dim));
                    v.push((plural(counts.warnings, "warning"), c.warning));
                }
                (Glyph::Icon("ui/check", c.git_added), v)
            }
            Job::Build => {
                let what =
                    if counts.total() == 0 { format!("exit code {}", d.code) } else { buildrun::summary(counts) };
                v.push((format!("Failed — {what}"), c.error));
                (Glyph::Icon("ui/x_circle", c.error), v)
            }
            Job::Run if counts.errors > 0 && self.build.outcome == Some(d) => {
                v.push((format!("Failed — {}", buildrun::summary(counts)), c.error));
                (Glyph::Icon("ui/x_circle", c.error), v)
            }
            Job::Run => {
                let col = if d.code == 0 { c.git_added } else { c.error };
                v.push((format!("Exited with code {}  ·  {t}", d.code), col));
                (Glyph::Icon(if d.code == 0 { "ui/check" } else { "ui/x_circle" }, col), v)
            }
            Job::Clean if d.code == 0 => {
                v.push((format!("Cleaned in {t}"), dim));
                (Glyph::Icon("ui/check", dim), v)
            }
            Job::Clean => {
                v.push((format!("Clean failed — exit code {}", d.code), c.error));
                (Glyph::Icon("ui/x_circle", c.error), v)
            }
        }
    }

    pub fn draw_build(&mut self, cv: &mut Canvas, body: Rect) {
        self.build.hits.clear();
        self.build_ensure_detect();
        let c = self.theme.chrome;
        let (strong, dim, bg) = (c.tab_active_fg, c.tab_inactive_fg, self.theme.background);
        let running = self.build.running();
        let counts = self.build.counts();
        let t = self.build.started.map_or(0.0, |t| t.elapsed().as_secs_f32());

        // ── toolbar: target, Build, Run, Stop, Clean; the live status on the right ──
        let ty = body.y + (TOOLBAR_H - BTN_H) / 2;
        let mut x = body.x + 10;
        x += self.b_select(cv, x, ty, body) + 8;
        let (can_build, can_run, can_clean) = {
            let ts = &self.build.targets;
            let sel = self.build.selected_target();
            (
                buildrun::build_for(ts, sel).is_some(),
                buildrun::run_for(ts, sel).is_some(),
                buildrun::clean_for(ts, sel).is_some() || !self.build.problems.is_empty(),
            )
        };
        x += self.b_button(cv, x, ty, "Build", "ui/tool_hammer", Btn::Primary, can_build && !running, Hit::Build, body)
            + 6;
        x += self.b_button(cv, x, ty, "Run", "ui/play_filled", Btn::Secondary, can_run && !running, Hit::Run, body) + 6;
        if running {
            x += self.b_button(cv, x, ty, "Stop", "ui/stop", Btn::Danger, true, Hit::Stop, body) + 6;
        }
        x += self.b_button(cv, x, ty, "Clean", "ui/eraser", Btn::Secondary, can_clean && !running, Hit::Clean, body)
            + 12;
        let (glyph, pieces) = self.build_status();
        let total_w: i32 = pieces.iter().map(|(s, _)| self.tx.ui_width(s, false)).sum();
        let mut px = body.right() - total_w - 16;
        let gy = body.y + TOOLBAR_H / 2;
        if px - 24 > x {
            match glyph {
                Glyph::Spin => spinner_t(cv, px - 14, gy, 5, c.accent_text, bg, t, body),
                Glyph::Dot(col) => status_dot(cv, px - 14, gy, col, body),
                Glyph::Icon(name, col) => self.tx.icon(cv, name, px - 22, gy - 8, 16, col, body),
                Glyph::None => {}
            }
            for (s, col) in &pieces {
                let w = self.tx.ui_width(s, false);
                self.tx.draw_ui(cv, px, body.y + (TOOLBAR_H - 18) / 2, s, *col, body, false);
                px += w;
            }
        }

        // ── progress: indeterminate while running, then the outcome's color ──
        let pb = Rect::new(body.x, body.y + TOOLBAR_H, body.w, PROG_H);
        cv.fill(pb, c.border.with_alpha(110).over(bg));
        if running && self.build.job == Some(Job::Run) {
            // A Run may never end: a still bar, not a moving one.
            cv.fill_clip(pb, c.git_added.with_alpha(0x60), pb);
        } else if running {
            let bw = (body.w / 5).max(80);
            let pos = ((t * 0.8).fract() * (body.w + bw) as f32) as i32 - bw;
            cv.fill_clip(Rect::new(body.x + pos, pb.y, bw, PROG_H), c.accent, pb);
        } else if let Some(d) = self.build.last.filter(|d| !d.stopped) {
            let ok = d.code == 0 && !(self.build.outcome == Some(d) && counts.errors > 0);
            let col = if ok { c.git_added } else { c.error };
            cv.fill_clip(pb, col.with_alpha(0x90), pb);
        }

        let below = Rect::new(body.x, pb.bottom(), body.w, (body.bottom() - pb.bottom()).max(0));
        if below.h < 30 {
            return;
        }
        let started = !self.build.logs[0].is_empty() || !self.build.logs[1].is_empty();
        if !started {
            // Nothing built yet: the empty state takes the whole width.
            return self.draw_build_fresh(cv, below);
        }

        // ── filter bar: severity chips over the list, Build | Run over the log ──
        let fb = Rect::new(body.x, pb.bottom(), body.w, FILTER_H);
        cv.fill(Rect::new(fb.x, fb.bottom() - 1, fb.w, 1), c.border);
        let content = Rect::new(body.x, fb.bottom(), body.w, (body.bottom() - fb.bottom()).max(0));
        let lw = if content.w >= 900 { (content.w as f32 * 0.46) as i32 } else { content.w / 2 };
        let cy = fb.y + (FILTER_H - 1 - CHIP_H) / 2;
        let mut fx = fb.x + 8;
        for (label, f, n, hot) in [
            ("All", Filter::All, counts.total(), strong),
            ("Errors", Filter::Errors, counts.errors, c.error),
            ("Warnings", Filter::Warnings, counts.warnings, c.warning),
        ] {
            let text = format!("{label}  {n}");
            // Measured bold so the chip keeps its width when it becomes the active one.
            let w = self.tx.ui_width(&text, true) + 20;
            if fx + w > fb.x + lw - 8 {
                break;
            }
            let r = Rect::new(fx, cy, w, CHIP_H);
            let active = self.build.filter == f;
            if active {
                cv.fill_round_clip(r, RAD, strong.with_alpha(0x1e), fb);
            }
            let col = if active || self.b_hv(r) {
                strong
            } else if f != Filter::All && n > 0 {
                hot
            } else {
                dim
            };
            let tw = self.tx.ui_width(&text, active);
            self.tx.draw_ui(cv, r.x + (w - tw) / 2, r.y + (CHIP_H - 18) / 2, &text, col, fb, active);
            self.build.hits.push((r, Hit::Filter(f)));
            fx += w + 4;
        }
        let live = self.build.job.map(Job::log);
        let mut vx = content.x + lw + 10;
        for (label, v, icon) in [("Build", LogView::Build, "ui/tool_hammer"), ("Run", LogView::Run, "ui/play_filled")] {
            let dot = live == Some(v);
            let w = self.tx.ui_width(label, true) + 40 + if dot { 12 } else { 0 };
            let r = Rect::new(vx, cy, w, CHIP_H);
            let active = self.build.view == v;
            if active {
                cv.fill_round_clip(r, RAD, strong.with_alpha(0x1e), fb);
            }
            let col = if active || self.b_hv(r) { strong } else { dim };
            self.tx.icon(cv, icon, r.x + 10, r.y + (CHIP_H - 14) / 2, 14, col, fb);
            let tw = self.tx.ui_width(label, active);
            self.tx.draw_ui(cv, r.x + 30, r.y + (CHIP_H - 18) / 2, label, col, fb, active);
            if dot {
                // This output is still growing.
                let a = (0.55 + 0.45 * (t * 4.0).sin()) * 255.0;
                cv.fill_round_clip(
                    Rect::new(r.x + 34 + tw, r.y + CHIP_H / 2 - 3, 6, 6),
                    3,
                    c.accent_text.with_alpha(a as u8),
                    fb,
                );
            }
            self.build.hits.push((r, Hit::View(v)));
            vx += w + 4;
        }

        // ── content: problems | log ──
        if content.h < 30 {
            return;
        }
        let list = Rect::new(content.x, content.y, lw, content.h);
        let log = Rect::new(content.x + lw + 1, content.y, content.w - lw - 1, content.h);
        cv.fill(Rect::new(content.x + lw, content.y, 1, content.h), c.border);
        self.draw_build_list(cv, list);
        self.draw_build_log(cv, log);
    }

    /// Centered empty state: an icon, a title and a few dim lines, like an onboarding card.
    fn b_empty_state(
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
        let t = fit(&mut self.tx, title, area.w - 32, true);
        let w = self.tx.ui_width(&t, true);
        self.tx.draw_ui(cv, area.x + (area.w - w) / 2, y, &t, strong, area, true);
        y += 24;
        for (s, col) in lines {
            let s = fit(&mut self.tx, s, area.w - 32, false);
            let w = self.tx.ui_width(&s, false);
            self.tx.draw_ui(cv, area.x + (area.w - w) / 2, y, &s, *col, area, false);
            y += 20;
        }
    }

    /// Before the first job: what Build will run, or how to set a build up.
    fn draw_build_fresh(&mut self, cv: &mut Canvas, area: Rect) {
        let c = self.theme.chrome;
        let dim = c.tab_inactive_fg;
        if self.build.targets.is_empty() {
            let lines = [
                ("Supported: Cargo, npm / pnpm / yarn / bun, Maven, Gradle, .NET, Go, CMake, Make".to_string(), dim),
                ("Anything else: set a build command (and a run command) in Settings → Build".to_string(), dim),
            ];
            return self.b_empty_state(cv, area, "ui/tool_hammer", dim, "No build system detected", &lines);
        }
        let mut lines: Vec<(String, Rgba)> = Vec::new();
        if let Some(t) = buildrun::build_for(&self.build.targets, self.build.selected_target()) {
            lines.push((format!("Builds with {}  ·  {}", t.label, t.group), dim));
        }
        let keys = self.keymap.entries();
        let b = crate::app_tooltip::shortcut_in(&keys, "BuildBuild");
        let r = crate::app_tooltip::shortcut_in(&keys, "BuildRun");
        match (b, r) {
            (Some(b), Some(r)) => lines.push((format!("{b} builds  ·  {r} runs"), dim)),
            (Some(b), None) => lines.push((format!("{b} builds"), dim)),
            (None, Some(r)) => lines.push((format!("{r} runs"), dim)),
            (None, None) => {}
        }
        let n = self.build.targets.iter().filter(|t| t.kind != Kind::Clean).count();
        if n > 1 {
            lines.push((format!("{n} targets found — choose another one with the selector"), dim));
        }
        self.b_empty_state(cv, area, "ui/tool_hammer", c.accent_text, "No build yet — press Build", &lines);
    }

    /// The list side when it has no rows: a job on its way, or how the last one ended.
    fn draw_build_list_empty(&mut self, cv: &mut Canvas, area: Rect) {
        let c = self.theme.chrome;
        let (dim, bg) = (c.tab_inactive_fg, self.theme.background);
        if !self.build.problems.is_empty() {
            return self.b_empty_state(cv, area, "ui/filter_funnel", dim, "Nothing matches this filter", &[]);
        }
        let job = self.build.job.filter(|j| *j == Job::Clean || self.build.matcher.is_some());
        if let Some(job) = job {
            // Placeholder rows that breathe while the first problems may be on their way.
            let t = self.build.started.map_or(0.0, |t| t.elapsed().as_secs_f32());
            for k in 0..((area.h / ROW_H).min(6)) {
                let a = (0.5 + 0.5 * (t * 3.0 - k as f32 * 0.5).sin()) * 0.55 + 0.25;
                let col = c.border.with_alpha((a * 130.0) as u8).over(bg);
                let w = [0.5, 0.36, 0.62, 0.44, 0.55, 0.3][k as usize % 6];
                let y = area.y + LIST_PAD + 4 + k * ROW_H;
                let indent = if k % 3 == 0 { 0 } else { 18 };
                cv.fill_round_clip(Rect::new(area.x + 32 + indent, y + 8, 14, 14), 7, col, area);
                cv.fill_round_clip(
                    Rect::new(area.x + 56 + indent, y + 11, (area.w as f32 * w * 0.6) as i32, 8),
                    4,
                    col,
                    area,
                );
            }
            let msg = format!("{}…", job.verb());
            let w = self.tx.ui_width(&msg, false) + 32;
            let pill = Rect::new(area.x + (area.w - w) / 2, area.y + area.h / 2 - 14, w, 28);
            cv.fill_round_clip(pill, RAD, c.input_bg.with_alpha(0xe0).over(bg), area);
            self.tx.draw_ui(cv, pill.x + 16, pill.y + 5, &msg, dim, area, false);
            return;
        }
        let Some(d) = self.build.outcome else {
            let lines = [("Compiler errors and warnings show up here after a build".to_string(), dim)];
            return self.b_empty_state(cv, area, "ui/check", dim, "No problems", &lines);
        };
        if d.stopped {
            let lines = [("Press Build to start again".to_string(), dim)];
            return self.b_empty_state(cv, area, "ui/stop", dim, "Stopped", &lines);
        }
        match (d.job, d.code) {
            (Job::Clean, 0) => {
                let lines = [("Build output removed — press Build to build again".to_string(), dim)];
                self.b_empty_state(cv, area, "ui/eraser", dim, "Cleaned", &lines)
            }
            (Job::Clean, code) => {
                let lines = [("The log has the details".to_string(), dim)];
                self.b_empty_state(
                    cv,
                    area,
                    "ui/x_circle",
                    c.error,
                    &format!("Clean failed (exit code {code})"),
                    &lines,
                )
            }
            (_, 0) => {
                let lines = [(format!("Finished in {}", fmt_secs(d.ms)), dim)];
                self.b_empty_state(cv, area, "ui/check", c.git_added, "Build succeeded — no problems", &lines)
            }
            (_, code) => {
                let lines = [("No compiler problem could be matched — the log has the details".to_string(), dim)];
                self.b_empty_state(
                    cv,
                    area,
                    "ui/x_circle",
                    c.error,
                    &format!("Build failed (exit code {code})"),
                    &lines,
                )
            }
        }
    }

    fn draw_build_list(&mut self, cv: &mut Canvas, list: Rect) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let rows = build_rows(&self.build.problems, self.build.filter, &self.build.collapsed);
        self.build.list_view = list.h;
        if rows.is_empty() {
            return self.draw_build_list_empty(cv, list);
        }
        let (tops, content_h) = row_tops(&rows);
        let total = content_h as f64;
        let visible = list.h as f64;
        self.build.scroll = self.build.scroll.clamp(0.0, (total - visible).max(0.0));
        let scroll = self.build.scroll;
        let inner_w = list.w - BAR_W - 8;
        let focused = self.build.focused;
        let cur = cursor_row(&rows, self.build.cur_file.as_deref(), self.build.sel);
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
                Row::File { file, errors, warnings, infos } => {
                    let r = Rect::new(list.x + 6, y + GROUP_GAP, inner_w, ROW_H);
                    let vis = r.intersect(&list);
                    let on_file = is_cur && self.build.cur_file.is_some();
                    if on_file {
                        cv.fill_round_clip(r, RAD, strong.with_alpha(0x1e), list);
                    }
                    let folded = self.build.collapsed.contains(file);
                    let chev = if folded { "ui/chevron_right" } else { "ui/chevron_down" };
                    self.tx.icon(cv, chev, r.x + 6, r.y + (ROW_H - 16) / 2, 16, dim, list);
                    let (name, dir) = group_label(&self.root, &file.to_string_lossy());
                    let icon = self.tx.icons.file_icon(&name);
                    self.tx.icon(cv, &icon, r.x + 26, r.y + (ROW_H - 16) / 2, 16, dim, list);
                    // Counts first (right side), so the name knows how much room it has.
                    let mut rx = r.right() - 10;
                    for (n, icon, col) in [
                        (*infos, "ui/info", c.info),
                        (*warnings, "ui/warning", c.warning),
                        (*errors, "ui/x_circle", c.error),
                    ] {
                        if n == 0 {
                            continue;
                        }
                        let s = n.to_string();
                        rx -= self.tx.ui_width(&s, false);
                        self.tx.draw_ui(cv, rx, r.y + (ROW_H - 18) / 2, &s, col, list, false);
                        rx -= 4 + 14;
                        self.tx.icon(cv, icon, rx, r.y + (ROW_H - 14) / 2, 14, col, list);
                        rx -= 10;
                    }
                    let nx = r.x + 50;
                    let room = rx - nx;
                    let name_s = fit(&mut self.tx, &name, room, true);
                    let nw = self.tx.ui_width(&name_s, true);
                    self.tx.draw_ui(cv, nx, r.y + (ROW_H - 18) / 2, &name_s, strong, list, true);
                    if !dir.is_empty() && room - nw > 40 {
                        let d = fit(&mut self.tx, &dir, room - nw - 10, false);
                        self.tx.draw_ui_sized(cv, nx + nw + 8, r.y + 8, &d, dim, list, false, SMALL);
                    }
                    if on_file && vis == r {
                        cv.ring(r, RAD, c.accent.with_alpha(0x90));
                    }
                    if vis.h > 0 {
                        self.build.hits.push((vis, Hit::File(file.clone())));
                    }
                }
                Row::Item(i) => {
                    let i = *i;
                    let (sev, pos, msg, code) = {
                        let p = &self.build.problems[i];
                        (
                            p.sev,
                            format!("{}:{}", p.line + 1, p.col + 1),
                            p.message.lines().next().unwrap_or("").to_string(),
                            p.code.clone(),
                        )
                    };
                    let r = Rect::new(list.x + 6, y, inner_w, ROW_H);
                    let vis = r.intersect(&list);
                    let scol = sev_color(&c, sev);
                    if self.build.sel == Some(i) {
                        cv.fill_round_clip(r, RAD, strong.with_alpha(0x1e), list);
                        cv.fill_round_clip(Rect::new(r.x + 3, y + 8, 2, ROW_H - 16), 1, scol, list);
                        if is_cur && vis == r {
                            cv.ring(r, RAD, c.accent.with_alpha(0x90));
                        }
                    }
                    let ix = r.x + 30;
                    self.tx.icon(cv, sev_icon(sev), ix, y + (ROW_H - 16) / 2, 16, scol, list);
                    let px = ix + 24;
                    let pw = self.tx.ui_width_sized(&pos, false, SMALL);
                    self.tx.draw_ui_sized(cv, px, y + 8, &pos, dim, list, false, SMALL);
                    let mx = px + pw.max(30) + 10;
                    let mut rx = r.right() - 10;
                    if !code.is_empty() {
                        let cw = self.tx.ui_width_sized(&code, false, SMALL);
                        if rx - cw - 10 > mx + 120 {
                            rx -= cw;
                            self.tx.draw_ui_sized(cv, rx, y + 8, &code, dim, list, false, SMALL);
                            rx -= 10;
                        }
                    }
                    let m = fit(&mut self.tx, &msg, rx - mx, false);
                    self.tx.draw_ui(cv, mx, y + (ROW_H - 18) / 2, &m, strong, list, false);
                    if vis.h > 0 {
                        self.build.hits.push((vis, Hit::Problem(i)));
                    }
                }
            }
        }
        self.region_bar(cv, Reg::Build, list, total, visible, scroll, 1.0, false);
    }

    fn draw_build_log(&mut self, cv: &mut Canvas, area: Rect) {
        let c = self.theme.chrome;
        let (dim, bg) = (c.tab_inactive_fg, self.theme.background);
        let lh = line_h() as i32;
        let v = self.build.view as usize;
        self.build.log_view = area.h;
        let n = self.build.logs[v].lines.len();
        let total = (n as i32 * lh + 16) as f64;
        let visible = area.h as f64;
        let max = (total - visible).max(0.0);
        let scroll = {
            let log = &mut self.build.logs[v];
            if log.follow {
                log.scroll = max;
            }
            log.scroll = log.scroll.clamp(0.0, max);
            log.scroll
        };
        if n == 0 {
            let msg = if self.build.view == LogView::Run {
                "Nothing has run yet — press Run"
            } else {
                "No build output yet"
            };
            self.tx.draw_ui(cv, area.x + 16, area.y + 12, msg, dim, area, false);
            return;
        }
        // The selected problem's line is marked.
        let hl = self
            .build
            .sel
            .filter(|_| self.build.prob_log == self.build.view)
            .and_then(|i| self.build.problems.get(i))
            .map(|p| p.log);
        let base = self.build.logs[v].base;
        let first = (scroll / lh as f64).floor().max(0.0) as usize;
        let off = -((scroll as i32) % lh);
        let max_cols = ((area.w - 28 - BAR_W) as f32 / self.tx.cell_w).max(1.0) as usize;
        let rows = (area.h / lh + 2) as usize;
        for k in first..(first + rows).min(n) {
            let y = area.y + 8 + off + (k - first) as i32 * lh;
            let row = Rect::new(area.x + 6, y, area.w - 12 - BAR_W, lh);
            if hl == Some(base + k) {
                cv.fill_clip(row, c.accent.with_alpha(0x22).over(bg), area);
                cv.fill_clip(Rect::new(row.x, y, 3, lh), c.accent, area);
            }
            let mut pieces: Vec<(String, Rgba)> = Vec::new();
            let mut used = 0;
            for (s, col) in &self.build.logs[v].lines[k] {
                if used >= max_cols {
                    break;
                }
                let take: String = s.chars().take(max_cols - used).collect();
                used += take.chars().count();
                pieces.push((take, tasks::legible(*col, bg)));
            }
            self.tx.draw(cv, row.x + 10, y, &pieces, area);
            let plain = &self.build.logs[v].plain[k];
            if plain.contains(':') || plain.contains('(') {
                for lk in tasks::find_links(plain) {
                    let x0 = row.x + 10 + self.tx.cells(lk.start.min(max_cols));
                    let x1 = row.x + 10 + self.tx.cells(lk.end.min(max_cols));
                    if x1 <= x0 {
                        continue;
                    }
                    cv.fill_clip(Rect::new(x0, y + lh - 3, x1 - x0, 1), c.accent, area);
                    let hit = Rect::new(x0, y, x1 - x0, lh).intersect(&area);
                    if hit.w > 0 && hit.h > 0 {
                        self.build.hits.push((hit, Hit::Link(lk.path.clone(), lk.line, lk.col)));
                    }
                }
            }
        }
        self.region_bar(cv, Reg::BuildLog, area, total, visible, scroll, 1.0, false);
    }

    /// Scroll position of a Build region (see `scrollreg`).
    pub fn build_region_value(&self, log: bool) -> f64 {
        if log { self.build.logs[self.build.view as usize].scroll } else { self.build.scroll }
    }

    /// The wheel or the scrollbar moved a Build region; scrolling the log to its end follows it again.
    pub fn build_set_region(&mut self, log: bool, v: f64, max: f64) {
        if log {
            let l = &mut self.build.logs[self.build.view as usize];
            l.scroll = v;
            l.follow = v >= max - 1.0;
        } else {
            self.build.scroll = v;
        }
    }

    /// The bottom bar indicator, after the left group: a spinner while a job runs, the error count
    /// in red after a failed build; nothing otherwise. Returns the x after it.
    pub fn draw_build_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        let running = self.build.job;
        if running.is_none() && !self.build.failed() {
            return x;
        }
        let c = self.theme.chrome;
        let r = self.layout.status;
        let errors = self.build.counts().errors;
        let (label, col) = match running {
            Some(j) => (j.verb().to_string(), c.accent_text),
            None if errors > 0 => (errors.to_string(), c.error),
            None => ("Build failed".to_string(), c.error),
        };
        let lw = self.tx.ui_width(&label, false);
        let b = Rect::new(x, r.y + 3, 8 + 14 + 6 + lw + 8, r.h - 6);
        if self.panel.open && self.panel.tab == PanelTab::Build {
            cv.fill_round(b, 6, c.list_active_bg);
        }
        if running == Some(Job::Run) {
            status_dot(cv, b.x + 8 + 7, r.y + r.h / 2, c.git_added, r);
        } else if running.is_some() {
            let t = self.build.started.map_or(0.0, |t| t.elapsed().as_secs_f32());
            spinner_t(cv, b.x + 8 + 7, r.y + r.h / 2, 5, col, c.status_bg, t, r);
        } else {
            self.tx.icon(cv, "ui/x_circle", b.x + 8, r.y + (r.h - 14) / 2, 14, col, r);
        }
        self.tx.draw_ui(cv, b.x + 8 + 14 + 6, r.y + (r.h - 18) / 2, &label, col, r, false);
        self.layout.status_btns.push((b, StatusBtn::Build));
        b.right() + 2
    }

    /// Tooltip text of the bottom bar indicator.
    pub fn build_status_tip(&self) -> String {
        match self.build.job {
            Some(j) => format!("{}: {} — click to open the Build panel", j.verb(), self.build.job_label),
            None => format!("Last build: {} — click to open the Build panel", buildrun::summary(self.build.counts())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prob(file: &str, line: usize, sev: Sev) -> Problem {
        Problem {
            file: PathBuf::from(file),
            line,
            col: 0,
            sev,
            message: format!("{file}:{line}"),
            code: String::new(),
            log: line,
        }
    }

    #[test]
    fn rows_group_by_file_errors_first_and_filter() {
        let ps = vec![
            prob("/p/a.rs", 9, Sev::Warning),
            prob("/p/b.rs", 4, Sev::Error),
            prob("/p/b.rs", 1, Sev::Warning),
            prob("/p/a.rs", 2, Sev::Warning),
        ];
        let none = HashSet::new();
        let rows = build_rows(&ps, Filter::All, &none);
        // b.rs (has an error) first, its problems by line; then a.rs.
        assert_eq!(rows.len(), 2 + 4);
        assert!(matches!(&rows[0], Row::File { file, errors: 1, warnings: 1, .. } if file == Path::new("/p/b.rs")));
        assert!(matches!(rows[1], Row::Item(2)));
        assert!(matches!(rows[2], Row::Item(1)));
        assert!(matches!(&rows[3], Row::File { file, errors: 0, warnings: 2, .. } if file == Path::new("/p/a.rs")));
        assert!(matches!(rows[4], Row::Item(3)));
        assert!(matches!(rows[5], Row::Item(0)));
        // The filter hides files with nothing to show; counts stay those of the file.
        let rows = build_rows(&ps, Filter::Errors, &none);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0], Row::File { warnings: 1, .. }));
        // Folded files keep their row only.
        let fold: HashSet<PathBuf> = [PathBuf::from("/p/b.rs")].into();
        assert_eq!(build_rows(&ps, Filter::All, &fold).len(), 1 + 1 + 2);
        let (tops, total) = row_tops(&build_rows(&ps, Filter::All, &fold));
        assert_eq!(tops, vec![LIST_PAD, LIST_PAD + GROUP_H, LIST_PAD + 2 * GROUP_H, LIST_PAD + 2 * GROUP_H + ROW_H]);
        assert_eq!(total, LIST_PAD + 2 * GROUP_H + 2 * ROW_H + LIST_PAD);
    }

    #[test]
    fn keyboard_navigation_over_rows() {
        let ps = vec![prob("/p/a.rs", 1, Sev::Error), prob("/p/a.rs", 2, Sev::Error), prob("/p/b.rs", 3, Sev::Error)];
        let none = HashSet::new();
        let rows = build_rows(&ps, Filter::All, &none);
        // rows: [a.rs, 0, 1, b.rs, 2]
        assert_eq!(nav(&rows, &ps, &none, None, Nav::Down), NavOut::To(0));
        assert_eq!(nav(&rows, &ps, &none, None, Nav::End), NavOut::To(4));
        assert_eq!(nav(&rows, &ps, &none, Some(0), Nav::Up), NavOut::Stay);
        assert_eq!(nav(&rows, &ps, &none, Some(1), Nav::PageDown(10)), NavOut::To(4));
        assert_eq!(nav(&rows, &ps, &none, Some(2), Nav::Left), NavOut::To(0));
        assert_eq!(nav(&rows, &ps, &none, Some(0), Nav::Left), NavOut::Fold("/p/a.rs".into()));
        assert_eq!(nav(&rows, &ps, &none, Some(3), Nav::Right), NavOut::To(4));
        let fold: HashSet<PathBuf> = [PathBuf::from("/p/a.rs")].into();
        let folded = build_rows(&ps, Filter::All, &fold);
        assert_eq!(nav(&folded, &ps, &fold, Some(0), Nav::Right), NavOut::Unfold("/p/a.rs".into()));
        assert_eq!(cursor_row(&rows, Some(Path::new("/p/b.rs")), Some(1)), Some(3));
        assert_eq!(cursor_row(&rows, None, Some(1)), Some(2));
        assert_eq!(cursor_row(&rows, None, None), None);
        assert_eq!(fmt_secs(3240.0), "3.2 s");
        assert_eq!(fmt_secs(65_000.0), "1m 05s");
    }

    #[test]
    fn remembered_target_roundtrips() {
        let p = std::env::temp_dir().join(format!("lumen-build-state-{}/b.json", std::process::id()));
        assert_eq!(load_target(&p), None);
        save_target(&p, "cargo::cargo check");
        assert_eq!(load_target(&p).as_deref(), Some("cargo::cargo check"));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    fn wait_done(app: &mut App) {
        let t0 = Instant::now();
        while app.build.running() && t0.elapsed() < Duration::from_secs(20) {
            app.pump_build();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.build.running(), "the job never ended");
    }

    #[test]
    #[cfg(unix)]
    fn a_failing_custom_build_lists_its_problem_and_marks_the_file() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("buildrun-e2e");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/x.rs"), "fn main() {\n    let a = 1;\n    let total = nope;\n}\n").unwrap();
        let mut app = test_app(&dir);
        app.config.build_command =
            "echo \"src/x.rs:3:17: error: boom\"; echo 'src/x.rs:2:9: warning: unused'; exit 1".into();
        app.build_build();
        assert_eq!(app.panel.tab, PanelTab::Build);
        assert!(app.panel.open);
        assert_eq!(app.build.selected.as_deref(), Some(buildrun::CUSTOM_BUILD));
        wait_done(&mut app);
        let d = app.build.last.expect("finished");
        assert_eq!((d.job, d.code, d.stopped), (Job::Build, 1, false));
        assert!(app.build.failed());
        let c = app.build.counts();
        assert_eq!((c.errors, c.warnings), (1, 1));
        let p = app.build.problems.iter().find(|p| p.sev == Sev::Error).unwrap();
        assert_eq!((p.file.clone(), p.line, p.col, p.message.as_str()), (dir.join("src/x.rs"), 2, 16, "boom"));
        // The toolbar says it failed; the first problem as listed (the error) is selected.
        let (_, status) = app.build_status();
        let text: String = status.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(text, "Failed — 1 error, 1 warning");
        assert_eq!(app.build.sel.map(|i| app.build.problems[i].sev), Some(Sev::Error));
        // A file opened after the build gets its problems as editor diagnostics.
        let i = app.open_path(&dir.join("src/x.rs"), None).unwrap();
        app.build_sync_diags();
        let diags: Vec<(usize, usize, usize, u8, String)> = app.docs[i]
            .diags
            .iter()
            .filter(|g| g.source == SOURCE)
            .map(|g| (g.sl, g.sc, g.ec, g.severity, g.message.clone()))
            .collect();
        assert_eq!(diags, vec![(1, 8, 9, 2, "unused".to_string()), (2, 16, 20, 1, "boom".to_string())]);
        // ...and the Problems picker lists nothing twice for an open file.
        let (_, items) = app.problem_items(false);
        assert_eq!(items.iter().filter(|it| it.label.ends_with("boom")).count(), 1, "listed once");
        // Clean without a clean command clears the problems and the diagnostics.
        app.build_clean();
        app.build_sync_diags();
        assert!(app.build.problems.is_empty());
        assert!(!app.docs[i].diags.iter().any(|g| g.source == SOURCE));
        assert!(!app.build.failed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn a_passing_build_then_stop_of_a_long_run() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("buildrun-ok");
        let mut app = test_app(&dir);
        app.config.build_command = "echo compiled".into();
        app.config.run_command = "sleep 30".into();
        app.build_build();
        wait_done(&mut app);
        assert_eq!(app.build.last.map(|d| d.code), Some(0));
        assert!(app.build.problems.is_empty() && !app.build.failed());
        let (_, status) = app.build_status();
        assert!(status[0].0.starts_with("Succeeded in "), "{status:?}");
        assert!(app.build.logs[LogView::Build as usize].plain.iter().any(|l| l == "compiled"));
        // Run goes to its own log and Stop kills it.
        app.build_run();
        assert_eq!((app.build.job, app.build.view), (Some(Job::Run), LogView::Run));
        assert!(app.build.running());
        // A Run (often a dev server that never ends) doesn't keep an idle editor repainting: no
        // timer while the panel is hidden, one a second for the elapsed time while it shows.
        app.panel.open = false;
        assert_eq!(app.build_anim_step(), None, "hidden panel: no repaint timer during a Run");
        app.panel.open = true;
        app.panel.tab = PanelTab::Build;
        assert_eq!(app.build_anim_step(), Some(Duration::from_secs(1)));
        assert!(matches!(app.build_status().0, Glyph::Dot(_)), "a still dot, not the spinner");
        app.build_stop();
        assert!(!app.build.running());
        assert_eq!(app.build.last.map(|d| (d.job, d.stopped)), Some((Job::Run, true)));
        assert!(app.build.logs[LogView::Run as usize].plain.iter().any(|l| l == "■ stopped"));
        // The build's log is still there to switch back to.
        assert!(app.build.logs[LogView::Build as usize].plain.iter().any(|l| l == "compiled"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_elapsed_time_is_whole_seconds() {
        assert_eq!(fmt_elapsed(400.0), "0s");
        assert_eq!(fmt_elapsed(59_900.0), "59s");
        assert_eq!(fmt_elapsed(61_000.0), "1m 01s");
        assert_eq!(fmt_elapsed(3_725_000.0), "1h 02m");
    }

    #[test]
    fn arrow_keys_drive_the_problem_list_while_it_has_the_focus() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("buildkeys");
        let mut app = test_app(&dir);
        for p in [prob("/p/a.rs", 1, Sev::Error), prob("/p/a.rs", 2, Sev::Warning)] {
            app.build.add_problem(p);
        }
        app.panel.open = true;
        app.panel.tab = PanelTab::Build;
        app.focus = Focus::Editor;
        let key = |app: &mut App, k: NamedKey| {
            app.handle_key(&KeyIn { key: Key::Named(k), text: None });
        };
        // Without the focus the arrows belong to the editor.
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!((app.build.sel, app.build.cur_file.as_deref()), (None, None));
        app.build_focus();
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!(app.build.cur_file.as_deref(), Some(Path::new("/p/a.rs")), "first stop: the file row");
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!((app.build.sel, app.build.cur_file.as_deref()), (Some(0), None));
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!(app.build.sel, Some(1));
        key(&mut app, NamedKey::ArrowLeft);
        assert_eq!(app.build.cur_file.as_deref(), Some(Path::new("/p/a.rs")));
        key(&mut app, NamedKey::ArrowLeft);
        assert!(app.build.collapsed.contains(Path::new("/p/a.rs")), "Left on an open file folds it");
        key(&mut app, NamedKey::ArrowRight);
        assert!(!app.build.collapsed.contains(Path::new("/p/a.rs")), "Right unfolds it");
        assert!(app.build.focused);
        key(&mut app, NamedKey::Escape);
        assert!(!app.build.focused);
        key(&mut app, NamedKey::ArrowDown);
        assert_eq!(app.build.cur_file.as_deref(), Some(Path::new("/p/a.rs")), "unchanged once the focus is gone");
        app.build_focus();
        key(&mut app, NamedKey::Tab);
        assert!(!app.build.focused, "a key the list does not use gives the focus back");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
