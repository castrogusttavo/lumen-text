//! "Explain" and "Fix with AI": a compact prompt about one problem (a diagnostic, a failed test, a
//! build error, a terminal selection) goes to the agent session of the AI panel, typed like a
//! paste and never submitted, so the user reads it and presses Enter.
//!
//! The prompt text is built by plain functions (unit-tested here): the problem's message, a code
//! frame of a few lines around it, `file:line`, the source / code and related locations, filled
//! into a template (`ai_prompt_explain` / `ai_prompt_fix`, built-in ones when empty). Nothing in
//! this module runs, allocates or reads a file until one of its commands is used, so Lumen
//! without any agent installed pays nothing; with none installed the command only shows the
//! install hint.

use super::*;
use crate::{
    app_overlay::{Action, Overlay},
    doc::Diag,
    testrun,
    testrun::Status as TestStatus,
};

/// Longest line kept in a code frame.
const LINE_CAP: usize = 220;
/// A file bigger than this is not read for a code frame.
const FILE_CAP: u64 = 2 * 1024 * 1024;
/// Longest test failure message that goes into a prompt.
const MSG_CAP: usize = 1500;
/// Problems listed by "Send Problems Summary".
const SUMMARY_CAP: usize = 40;
/// Longest terminal selection sent (the end is kept: the error is usually last).
const TERM_CAP: usize = 6000;

pub const DEFAULT_EXPLAIN: &str = "Explain this {severity} and what causes it. Do not change any files.\n\n{location}{source}\n{message}\n\n{frame}\n{related}";
pub const DEFAULT_FIX: &str = "Fix this {severity} with the smallest change that resolves it.\n\n{location}{source}\n{message}\n\n{frame}\n{related}";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Explain,
    Fix,
}

/// Everything known about one problem, ready to fill a template.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Problem {
    pub message: String,
    /// "error", "warning", "hint", "failing test"...
    pub severity: String,
    pub source: String,
    pub code: String,
    /// Project-relative path ("" when unknown).
    pub file: String,
    /// 0-based; shown 1-based.
    pub line: usize,
    pub lang: String,
    /// Numbered lines around the problem (see [`code_frame`]); empty when the file is unreadable.
    pub frame: String,
    /// Other locations the server related to it (`file:line message`).
    pub related: Vec<String>,
}

pub fn severity_name(s: u8) -> &'static str {
    match s {
        1 => "error",
        2 => "warning",
        3 => "information",
        _ => "hint",
    }
}

/// The lines `first..=last` around `line` (0-based), `n` on each side, inside `0..count`.
pub fn frame_range(count: usize, line: usize, n: usize) -> (usize, usize) {
    let last = count.saturating_sub(1);
    let line = line.min(last);
    (line.saturating_sub(n), (line + n).min(last))
}

fn clip_line(s: &str) -> String {
    let s = s.trim_end();
    match s.char_indices().nth(LINE_CAP) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// `lines` are file lines starting at 0-based `first`; the one at `at` is marked with `>`.
pub fn code_frame(first: usize, lines: &[String], at: usize) -> String {
    let width = (first + lines.len()).to_string().len();
    let mut out = String::new();
    for (k, l) in lines.iter().enumerate() {
        let n = first + k;
        let mark = if n == at { '>' } else { ' ' };
        out.push_str(&format!("{mark} {:>width$} | {}\n", n + 1, clip_line(l)));
    }
    out.trim_end().to_string()
}

/// Fill `{name}` placeholders in one pass (values are never expanded again; unknown names stay
/// as typed), then squeeze runs of blank lines.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        match tail.find('}') {
            Some(close) if vars.iter().any(|(k, _)| *k == &tail[..close]) => {
                let name = &tail[..close];
                out.push_str(vars.iter().find(|(k, _)| *k == name).map_or("", |(_, v)| *v));
                rest = &tail[close + 1..];
            }
            _ => {
                out.push('{');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    let mut squeezed = String::with_capacity(out.len());
    let mut blank = 0;
    for l in out.lines() {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        squeezed.push_str(l.trim_end());
        squeezed.push('\n');
    }
    squeezed.trim().to_string()
}

/// The prompt for `p`: `template` (empty = the built-in one of `mode`) filled in.
pub fn prompt(mode: Mode, template: &str, p: &Problem) -> String {
    let template = match (template.trim(), mode) {
        ("", Mode::Explain) => DEFAULT_EXPLAIN,
        ("", Mode::Fix) => DEFAULT_FIX,
        (t, _) => t,
    };
    let location = match (p.file.is_empty(), p.line) {
        (true, _) => String::new(),
        (false, l) => format!("{}:{}", p.file, l + 1),
    };
    let source = match (p.source.is_empty(), p.code.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(" ({})", p.source),
        (true, false) => format!(" ({})", p.code),
        (false, false) => format!(" ({} {})", p.source, p.code),
    };
    let frame = if p.frame.is_empty() { String::new() } else { format!("```{}\n{}\n```", p.lang, p.frame) };
    let related = if p.related.is_empty() {
        String::new()
    } else {
        format!("Related:\n{}", p.related.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n"))
    };
    let line = (p.line + 1).to_string();
    render(
        template,
        &[
            ("message", &p.message),
            ("severity", &p.severity),
            ("location", &location),
            ("file", &p.file),
            ("line", &line),
            ("source", &source),
            ("code", &p.code),
            ("lang", &p.lang),
            ("frame", &frame),
            ("related", &related),
        ],
    )
}

/// One line per problem for "Send Problems Summary": `file:line severity message`.
pub fn problems_summary(rows: &[(String, usize, u8, String)], total: usize) -> String {
    let mut out =
        String::from("These are the problems in my project right now. Tell me which to fix first and why.\n\n");
    for (file, line, sev, msg) in rows.iter().take(SUMMARY_CAP) {
        out.push_str(&format!("- {file}:{} {}: {msg}\n", line + 1, severity_name(*sev)));
    }
    if total > rows.len().min(SUMMARY_CAP) {
        out.push_str(&format!("... and {} more\n", total - rows.len().min(SUMMARY_CAP)));
    }
    out.trim_end().to_string()
}

/// A terminal selection as a prompt: fenced, the end kept when it is long.
pub fn terminal_prompt(mode: Mode, text: &str) -> String {
    let text = text.trim_matches('\n');
    let text: String = if text.chars().count() > TERM_CAP {
        let skip = text.chars().count() - TERM_CAP;
        format!("…{}", text.chars().skip(skip).collect::<String>())
    } else {
        text.to_string()
    };
    let ask = match mode {
        Mode::Explain => "Explain this terminal output and what causes it. Do not change any files.",
        Mode::Fix => "This terminal output shows a failure. Find the cause in the project and fix it.",
    };
    format!("{ask}\n\n```\n{text}\n```")
}

fn clip_message(s: &str, cap: usize) -> String {
    match s.char_indices().nth(cap) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// The problem a failed test stands for.
pub fn test_problem(c: &testrun::Case, file: &str, frame: String, lang: &str) -> Problem {
    let msg = testrun::strip_ansi(&c.message);
    let msg = clip_message(msg.trim(), MSG_CAP);
    Problem {
        message: if msg.is_empty() { format!("`{}` failed", c.name) } else { format!("`{}` failed:\n{msg}", c.name) },
        severity: "test failure".into(),
        file: file.to_string(),
        line: c.line.unwrap_or(0),
        lang: lang.to_string(),
        frame,
        ..Problem::default()
    }
}

/// A code frame and the language it is in.
struct Around {
    frame: String,
    lang: String,
}

impl App {
    fn ai_context(&self) -> usize {
        self.config.ai_prompt_context.min(30) as usize
    }

    fn ai_template(&self, mode: Mode) -> &str {
        match mode {
            Mode::Explain => &self.config.ai_prompt_explain,
            Mode::Fix => &self.config.ai_prompt_fix,
        }
    }

    /// The code frame (and language name) around `line` of `path`: from the open document when
    /// there is one (unsaved edits included), else from the file on disk.
    fn ai_around(&self, path: &Path, line: usize) -> Around {
        let n = self.ai_context();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
        if let Some(d) = self.docs.iter().find(|d| d.path() == Some(path)) {
            let (a, b) = frame_range(d.buf.line_count(), line, n);
            let lines: Vec<String> = (a..=b).map(|l| d.buf.line(l)).collect();
            return Around { frame: code_frame(a, &lines, line), lang: d.lang.map_or(ext, str::to_string) };
        }
        let ok = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= FILE_CAP);
        let text = if ok { std::fs::read_to_string(path).ok() } else { None };
        let Some(text) = text else { return Around { frame: String::new(), lang: ext } };
        let all: Vec<&str> = text.lines().collect();
        let (a, b) = frame_range(all.len(), line, n);
        let lines: Vec<String> = all.get(a..=b).unwrap_or(&[]).iter().map(|s| s.to_string()).collect();
        Around { frame: code_frame(a, &lines, line), lang: ext }
    }

    /// A [`Problem`] about `path:line` with its code frame.
    fn ai_problem(&self, path: &Path, line: usize, sev: u8, message: &str, source: &str, code: &str) -> Problem {
        let around = self.ai_around(path, line);
        Problem {
            message: message.to_string(),
            severity: severity_name(sev).to_string(),
            source: source.to_string(),
            code: code.to_string(),
            file: self.rel(path),
            line,
            lang: around.lang,
            frame: around.frame,
            related: Vec::new(),
        }
    }

    /// The diagnostic of the active document under the caret (else the most severe on its line).
    fn ai_diag_at_cursor(&self) -> Option<(PathBuf, Diag)> {
        let d = self.docs.get(self.active)?;
        let path = d.path()?.to_path_buf();
        let (l, c) = d.buf.line_col(d.ed.sel.head);
        let under = d.diags.iter().filter(|g| (g.sl, g.sc) <= (l, c) && (l, c) <= (g.el, g.ec.max(g.sc + 1)));
        let pick = under
            .min_by_key(|g| g.severity)
            .or_else(|| d.diags.iter().filter(|g| g.sl <= l && l <= g.el).min_by_key(|g| g.severity))?;
        Some((path, pick.clone()))
    }

    /// Is there a diagnostic at the caret (the editor menu shows its AI entries only then)?
    pub fn ai_has_diag_at_cursor(&self) -> bool {
        self.ai_diag_at_cursor().is_some()
    }

    /// The quick-fix menu's "Explain with AI" / "Fix with AI" entries.
    pub fn ai_action_items(&self) -> Vec<crate::app_overlay::Item> {
        use crate::app_overlay::{Cmd, Item};
        let item = |label: &str, cmd| Item { label: label.into(), detail: "AI agent".into(), action: Action::Cmd(cmd) };
        vec![item("Explain with AI", Cmd::AiExplainProblem), item("Fix with AI", Cmd::AiFixProblem)]
    }

    /// "AI: Explain Problem at Cursor" / "AI: Fix Problem at Cursor".
    pub fn ai_problem_at_cursor(&mut self, mode: Mode) {
        let Some((path, g)) = self.ai_diag_at_cursor() else {
            return self.say("No problem at the cursor — put it on an underlined error or warning");
        };
        let p = self.ai_problem(&path, g.sl, g.severity, &g.message, &g.source, &g.code);
        let text = prompt(mode, self.ai_template(mode), &p);
        self.ai_ask(&text);
    }

    /// The Problems list: the row selected (Alt+E explains, Alt+X fixes).
    pub fn ai_problem_row(&mut self, mode: Mode) {
        match self.ai_problem_row_prompt(mode) {
            Ok(text) => {
                self.overlay = Overlay::None;
                self.ai_ask(&text);
            }
            Err(why) => self.say(why),
        }
    }

    /// The prompt for the selected row of the open Problems list.
    pub(crate) fn ai_problem_row_prompt(&self, mode: Mode) -> Result<String, &'static str> {
        let Overlay::Picker(p) = &self.overlay else { return Err("Open the Problems list first") };
        let Some((i, _)) = p.filtered.get(p.sel) else { return Err("No problem selected") };
        let item = p.items[*i].clone();
        let Action::Open(path, Some((line, _))) = item.action else {
            return Err("Pick a problem row, not a file header");
        };
        let msg = item.label.trim_start();
        // "<icon> message": the severity icon leads the label.
        let (icon, msg) = msg.split_once(' ').unwrap_or(("", msg));
        let sev = match icon {
            "\u{f057}" => 1,
            "\u{f071}" => 2,
            _ => 3,
        };
        // Detail is "line:col · source".
        let source = item.detail.split_once(" · ").map_or("", |(_, s)| s);
        // The full message and code come from the open document when it has the diagnostic.
        let diag = self
            .docs
            .iter()
            .find(|d| d.path() == Some(path.as_path()))
            .and_then(|d| d.diags.iter().find(|g| g.sl == line && g.message.starts_with(msg.trim_end_matches('…'))))
            .cloned();
        let p = match diag {
            Some(g) => self.ai_problem(&path, line, g.severity, &g.message, &g.source, &g.code),
            None => self.ai_problem(&path, line, sev, msg, source, ""),
        };
        Ok(prompt(mode, self.ai_template(mode), &p))
    }

    /// The failed test selected in the Tests panel (else the first failure).
    pub fn ai_test(&mut self, mode: Mode) {
        match self.ai_test_prompt(mode) {
            Ok(text) => self.ai_ask(&text),
            Err(why) => self.say(why),
        }
    }

    pub(crate) fn ai_test_prompt(&self, mode: Mode) -> Result<String, &'static str> {
        let t = &self.tests;
        let sel = t.sel.as_deref().and_then(|id| t.cases.iter().find(|c| c.id == id));
        let Some(c) = sel
            .filter(|c| c.status == TestStatus::Failed)
            .or_else(|| t.cases.iter().find(|c| c.status == TestStatus::Failed))
            .cloned()
        else {
            return Err("No failed test — run the tests first");
        };
        let runner = t.runner.unwrap_or(testrun::Runner::Cargo);
        let p = match testrun::locate(&self.root, runner, &c) {
            Some((path, line)) => {
                let around = self.ai_around(&path, line);
                let c = testrun::Case { line: Some(line), ..c };
                test_problem(&c, &self.rel(&path), around.frame, &around.lang)
            }
            None => test_problem(&c, "", String::new(), ""),
        };
        Ok(prompt(mode, self.ai_template(mode), &p))
    }

    /// The compiler error selected in the Build panel (else the first error, else the first problem).
    pub fn ai_build(&mut self, mode: Mode) {
        let b = &self.build.problems;
        let pick = b
            .get(self.build.sel.unwrap_or(usize::MAX))
            .or_else(|| b.iter().find(|p| p.sev == crate::buildrun::Sev::Error))
            .or_else(|| b.first())
            .cloned();
        let Some(bp) = pick else { return self.say("No build problems — run a build first") };
        let p = self.ai_problem(&bp.file, bp.line, bp.sev.lsp(), &bp.message, "build", &bp.code);
        let text = prompt(mode, self.ai_template(mode), &p);
        self.ai_ask(&text);
    }

    /// "AI: Send Terminal Selection to Agent": the selected output of a terminal (the bottom
    /// panel's, else the build / task output has no selection of its own).
    pub fn ai_send_terminal(&mut self, mode: Mode) {
        let mut text = String::new();
        for id in self.terms.panes.clone() {
            if let Some(t) = self.terms.get(id).map(|t| t.term.selected_text()).filter(|s| !s.trim().is_empty()) {
                text = t;
                break;
            }
        }
        if text.trim().is_empty() {
            return self.say("Select some terminal output first");
        }
        self.ai_ask(&terminal_prompt(mode, &text));
    }

    /// "AI: Send File to Agent": mentions the whole active file (no copy of its text).
    pub fn ai_send_file(&mut self) {
        let Some(path) = self.docs.get(self.active).and_then(|d| d.path()).map(Path::to_path_buf) else {
            return self.say("Open a saved file to send it to the agent");
        };
        let text = format!("Look at {}", paste_path(&path));
        self.ai_ask(&text);
    }

    /// "AI: Send Problems Summary to Agent": the problems of the open files (and the last
    /// build), most severe first, one line each.
    pub fn ai_send_problems(&mut self) {
        let mut rows: Vec<(String, usize, u8, String)> = Vec::new();
        for d in &self.docs {
            let Some(p) = d.path() else { continue };
            let rel = self.rel(p);
            rows.extend(d.diags.iter().map(|g| (rel.clone(), g.sl, g.severity, crate::problems::clip_msg(&g.message))));
        }
        let open: Vec<&Path> = self.docs.iter().filter_map(|d| d.path()).collect();
        for p in self.build.problems.iter().filter(|p| !open.contains(&p.file.as_path())) {
            rows.push((self.rel(&p.file), p.line, p.sev.lsp(), crate::problems::clip_msg(&p.message)));
        }
        if rows.is_empty() {
            return self.say("No problems to send");
        }
        rows.sort_by(|a, b| (a.2, &a.0, a.1).cmp(&(b.2, &b.0, b.1)));
        let total = rows.len();
        let text = problems_summary(&rows, total);
        self.ai_ask(&text);
    }

    /// Hand `text` to the agent: the session shown (a new one when none runs) receives it as one
    /// paste, waiting for a starting agent to be ready; the panel is shown with the keyboard on
    /// the agent. Without an installed agent it shows the install hint instead of blocking.
    pub fn ai_ask(&mut self, text: &str) {
        self.ai_detect(false);
        if self.ai.root != self.root {
            self.ai_close_all();
        }
        let alive = self.ai.cur().is_some_and(|s| s.term.alive);
        if !alive {
            match self.ai_default_agent().or(AGENTS.first()) {
                Some(a) if self.ai.path_of(a).is_some() => {
                    let started = if self.ai.sessions.is_empty() {
                        self.ai_new_session(a, None)
                    } else {
                        self.ai_restart(None);
                        self.ai.cur().is_some_and(|s| s.term.alive)
                    };
                    if !started {
                        return;
                    }
                }
                Some(a) => return self.ai_not_installed(a),
                None => return,
            }
        }
        let Some(id) = self.ai.cur().map(|s| s.id) else { return };
        self.ai.pending_prompt = Some((id, text.to_string()));
        self.ai_show();
        self.ai_flush_prompt(Instant::now(), &mut |_| {});
    }

    /// Deliver the waiting prompt once its session is past the start-up screen.
    pub(crate) fn ai_flush_prompt(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some((id, _)) = &self.ai.pending_prompt else { return };
        let Some(i) = self.ai.index_of(*id) else {
            self.ai.pending_prompt = None;
            return;
        };
        match self.ai.sessions[i].status() {
            Status::Starting => want(now + Duration::from_millis(200)),
            Status::Exited(_) => self.ai.pending_prompt = None,
            _ => {
                let Some((_, text)) = self.ai.pending_prompt.take() else { return };
                self.ai_activate(i);
                self.ai_paste_text(&text);
                self.say("Sent to the agent — press Enter there to submit");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Problem {
        Problem {
            message: "mismatched types".into(),
            severity: "error".into(),
            source: "rustc".into(),
            code: "E0308".into(),
            file: "src/a.rs".into(),
            line: 9,
            lang: "rs".into(),
            frame: "  9 | let a = 1;\n> 10 | let b: u8 = \"x\";".into(),
            related: vec!["src/b.rs:3 expected due to this".into()],
        }
    }

    #[test]
    fn frame_range_is_clamped_to_the_file() {
        assert_eq!(frame_range(100, 50, 5), (45, 55));
        assert_eq!(frame_range(100, 2, 5), (0, 7));
        assert_eq!(frame_range(10, 9, 5), (4, 9));
        assert_eq!(frame_range(10, 99, 2), (7, 9), "a line past the end clamps");
        assert_eq!(frame_range(0, 0, 3), (0, 0));
        assert_eq!(frame_range(5, 2, 0), (2, 2));
    }

    #[test]
    fn code_frame_numbers_lines_and_marks_the_problem() {
        let lines: Vec<String> = ["fn a() {", "    bad();", "}"].iter().map(|s| s.to_string()).collect();
        assert_eq!(code_frame(8, &lines, 9), "   9 | fn a() {\n> 10 |     bad();\n  11 | }");
        let long = vec!["x".repeat(500)];
        assert!(code_frame(0, &long, 0).ends_with('…'));
        assert!(code_frame(0, &long, 0).chars().count() < 260);
    }

    #[test]
    fn render_fills_placeholders_once() {
        assert_eq!(render("{a}-{b}", &[("a", "{b}"), ("b", "2")]), "{b}-2", "values are not expanded again");
        assert_eq!(render("{nope} {a", &[("a", "1")]), "{nope} {a", "unknown names stay");
        assert_eq!(render("x\n\n\n\n{e}\n\ny", &[("e", "")]), "x\n\ny", "blank runs are squeezed");
    }

    #[test]
    fn default_prompts_carry_message_location_frame_and_related() {
        let t = prompt(Mode::Explain, "", &p());
        assert!(t.starts_with("Explain this error"), "{t}");
        assert!(t.contains("src/a.rs:10 (rustc E0308)"), "{t}");
        assert!(t.contains("mismatched types"));
        assert!(t.contains("```rs\n  9 | let a = 1;"), "{t}");
        assert!(t.contains("Related:\n- src/b.rs:3 expected due to this"));
        assert!(!t.contains('{'), "{t}");
        let f = prompt(Mode::Fix, "  ", &p());
        assert!(f.starts_with("Fix this error"), "{f}");
    }

    #[test]
    fn a_custom_template_and_missing_parts() {
        let bare = Problem { message: "oops".into(), severity: "warning".into(), ..Problem::default() };
        let t = prompt(Mode::Fix, "{severity} at {file}:{line}: {message}{source}", &bare);
        assert_eq!(t, "warning at :1: oops");
        let d = prompt(Mode::Explain, "", &bare);
        assert!(!d.contains("```") && !d.contains("Related"), "{d}");
        assert!(prompt(Mode::Explain, "Why {code}?", &p()).contains("Why E0308?"));
    }

    #[test]
    fn problems_summary_lists_and_counts_the_rest() {
        let rows: Vec<(String, usize, u8, String)> =
            (0..45).map(|i| ("a.rs".to_string(), i, 1, format!("m{i}"))).collect();
        let s = problems_summary(&rows, 45);
        assert!(s.contains("- a.rs:1 error: m0"));
        assert!(!s.contains("m40"));
        assert!(s.ends_with("... and 5 more"), "{s}");
    }

    #[test]
    fn terminal_prompt_keeps_the_end() {
        let big = format!("{}END", "x".repeat(TERM_CAP * 2));
        let t = terminal_prompt(Mode::Fix, &big);
        assert!(t.contains("END\n```") && t.contains("…x"));
        assert!(t.chars().count() < TERM_CAP + 200);
        assert!(terminal_prompt(Mode::Explain, "boom\n").contains("```\nboom\n```"));
    }

    #[test]
    fn a_failed_test_becomes_a_problem() {
        let c = testrun::Case {
            name: "adds".into(),
            status: TestStatus::Failed,
            line: Some(4),
            message: "\x1b[31massertion failed\x1b[0m: 1 != 2".into(),
            ..Default::default()
        };
        let pr = test_problem(&c, "src/m.rs", "> 5 | assert_eq!(1, 2);".into(), "rs");
        assert_eq!(pr.message, "`adds` failed:\nassertion failed: 1 != 2");
        let t = prompt(Mode::Explain, "", &pr);
        assert!(t.contains("test failure") && t.contains("src/m.rs:5"), "{t}");
    }
}
