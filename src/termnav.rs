//! Working with a terminal's scrollback: search (plain or regex, case toggle) with highlighted
//! matches, and the shell-integration marks (OSC 133 prompt / command / exit code, OSC 7 cwd) that
//! let the app jump between commands, select or copy a command's output and paint a gutter mark.
//!
//! Everything is kept in the grid's absolute line coordinates (`Grid::abs_row`), so marks and
//! matches stay on their text while output scrolls. Memory is flat: the scrollback itself is capped
//! (`DEFAULT_HISTORY`), at most [`MAX_CMDS`] command marks and [`MAX_MATCHES`] matches are kept.

use crate::terminal::{Grid, Sel, Term, WIDE_TAIL};
use regex::{Regex, RegexBuilder};
use std::{path::PathBuf, time::Instant};

/// Most command marks kept per terminal (the oldest go first).
pub const MAX_CMDS: usize = 512;
/// Most search matches kept (the rest of a huge scrollback is reported as "N+").
pub const MAX_MATCHES: usize = 5000;
/// How often a live search re-reads the scrollback while output keeps coming.
const REFRESH: std::time::Duration = std::time::Duration::from_millis(250);

/// One shell command seen through OSC 133, in absolute lines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CmdMark {
    /// First line of the prompt (`OSC 133 ; A`).
    pub prompt: usize,
    /// First line of the output (`OSC 133 ; C`); `None` while the command was not run.
    pub output: Option<usize>,
    /// Line after the last output line (`OSC 133 ; D`).
    pub end: Option<usize>,
    pub exit: Option<i32>,
}

impl CmdMark {
    /// A command that ran and has finished.
    pub fn done(&self) -> bool {
        self.output.is_some() && self.end.is_some()
    }

    /// Lines (first, last inclusive) of the output; `None` when there is none.
    pub fn output_lines(&self) -> Option<(usize, usize)> {
        let (s, e) = (self.output?, self.end?);
        (e > s).then(|| (s, e - 1))
    }
}

/// `file://host/some%20dir` -> `/some dir` (OSC 7).
pub fn osc7_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok().filter(|p| !p.is_empty())
}

// ───────────── search ─────────────

/// A search hit: line (absolute) and the columns `c0..c1` it covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub line: usize,
    pub c0: usize,
    pub c1: usize,
}

/// The text of absolute line `line` (without the second cell of wide characters or trailing
/// blanks) and, for each char, its column; one extra entry marks the end.
pub fn line_chars(g: &Grid, line: usize) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut cols = Vec::new();
    for c in 0..g.cols {
        let ch = g.abs_cell(line, c).ch;
        if ch != WIDE_TAIL {
            text.push(ch);
            cols.push(c);
        }
    }
    while text.ends_with(' ') {
        text.pop();
        cols.pop();
    }
    // The end marker: the column after the last char (a wide char takes two).
    let end = cols.last().map_or(0, |&c| c + if g.abs_cell(line, c).has(crate::terminal::WIDE) { 2 } else { 1 });
    cols.push(end);
    (text, cols)
}

/// Compile a query: plain text is escaped; `Err` for a bad regex.
pub fn compile(query: &str, case: bool, regex: bool) -> Result<Regex, regex::Error> {
    let pat = if regex { query.to_string() } else { regex::escape(query) };
    RegexBuilder::new(&pat).case_insensitive(!case).size_limit(1 << 20).build()
}

/// Matches of `re` inside one line's text, as column ranges.
pub fn matches_in(line: usize, text: &str, cols: &[usize], re: &Regex) -> Vec<Match> {
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        if m.is_empty() {
            continue;
        }
        let a = text[..m.start()].chars().count();
        let b = a + text[m.start()..m.end()].chars().count();
        out.push(Match { line, c0: cols[a], c1: cols[b.min(cols.len() - 1)] });
    }
    out
}

/// Every match in the scrollback and the screen, oldest first, up to [`MAX_MATCHES`]; the flag says
/// the list was cut.
pub fn search(g: &Grid, re: &Regex) -> (Vec<Match>, bool) {
    let mut out = Vec::new();
    for line in g.first_line()..g.end_line() {
        let (text, cols) = line_chars(g, line);
        if text.is_empty() {
            continue;
        }
        out.extend(matches_in(line, &text, &cols, re));
        if out.len() >= MAX_MATCHES {
            out.truncate(MAX_MATCHES);
            return (out, true);
        }
    }
    (out, false)
}

/// The scrollback search of one terminal.
pub struct Find {
    pub query: String,
    pub case: bool,
    pub regex: bool,
    pub matches: Vec<Match>,
    pub cur: Option<usize>,
    pub bad_regex: bool,
    pub capped: bool,
    stale: bool,
    refreshed: Instant,
}

impl Find {
    pub fn new() -> Find {
        Find {
            query: String::new(),
            case: false,
            regex: false,
            matches: Vec::new(),
            cur: None,
            bad_regex: false,
            capped: false,
            stale: false,
            refreshed: Instant::now(),
        }
    }

    /// The matches on absolute line `line`.
    pub fn on_line(&self, line: usize) -> &[Match] {
        let a = self.matches.partition_point(|m| m.line < line);
        let b = self.matches.partition_point(|m| m.line <= line);
        &self.matches[a..b]
    }

    /// "3/17", "No results", "17+" ...
    pub fn status(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        if self.bad_regex {
            return "Bad regex".into();
        }
        if self.matches.is_empty() {
            return "No results".into();
        }
        let n = self.matches.len();
        let plus = if self.capped { "+" } else { "" };
        format!("{}/{n}{plus}", self.cur.map_or(0, |c| c + 1))
    }
}

impl Default for Find {
    fn default() -> Self {
        Find::new()
    }
}

impl Term {
    /// Open the search bar (keeping an earlier query).
    pub fn find_open(&mut self) {
        if self.find.is_none() {
            self.find = Some(Box::new(Find::new()));
        }
    }

    pub fn find_close(&mut self) {
        self.find = None;
    }

    /// Re-run the search over the scrollback; the current match stays on its text when it can.
    pub fn find_refresh(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        let keep = f.cur.and_then(|i| f.matches.get(i)).copied();
        f.stale = false;
        f.refreshed = Instant::now();
        f.capped = false;
        f.bad_regex = false;
        if f.query.is_empty() {
            f.matches.clear();
            f.cur = None;
            return;
        }
        match compile(&f.query, f.case, f.regex) {
            Ok(re) => {
                let (m, capped) = search(&self.grid, &re);
                f.matches = m;
                f.capped = capped;
            }
            Err(_) => {
                f.matches.clear();
                f.bad_regex = true;
            }
        }
        f.cur = match keep {
            Some(k) => f.matches.iter().position(|m| *m == k).or(if f.matches.is_empty() { None } else { Some(0) }),
            None => None,
        };
    }

    /// The query, case or regex flag changed: search again and land on the match nearest the
    /// bottom of the view (the most recent output), scrolling to it.
    pub fn find_changed(&mut self) {
        if let Some(f) = self.find.as_mut() {
            f.cur = None;
        }
        self.find_refresh();
        let bottom = self.grid.abs_row(self.grid.rows - 1, self.scroll_offset);
        let Some(f) = self.find.as_mut() else { return };
        if f.matches.is_empty() {
            return;
        }
        let at = f.matches.partition_point(|m| m.line <= bottom);
        let i = if at == 0 { 0 } else { at - 1 };
        f.cur = Some(i);
        let line = f.matches[i].line;
        self.reveal(line);
    }

    /// Next (`down`) or previous match, wrapping around.
    pub fn find_step(&mut self, down: bool) {
        if self.find.as_ref().is_some_and(|f| f.stale) {
            self.find_refresh();
        }
        let Some(f) = self.find.as_mut() else { return };
        let n = f.matches.len();
        if n == 0 {
            return;
        }
        let i = match (f.cur, down) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(c), true) => (c + 1) % n,
            (Some(c), false) => (c + n - 1) % n,
        };
        f.cur = Some(i);
        let line = f.matches[i].line;
        self.reveal(line);
    }

    /// Called as output arrives: the search results are re-read soon (not on every chunk).
    pub fn find_note_output(&mut self) {
        if let Some(f) = self.find.as_mut() {
            f.stale = true;
            if f.refreshed.elapsed() >= REFRESH {
                self.find_refresh();
            }
        }
    }

    /// Scroll the view so absolute line `line` is on screen (about a third from the top).
    pub fn reveal(&mut self, line: usize) {
        let rows = self.grid.rows;
        let top = self.grid.abs_row(0, self.scroll_offset);
        if line >= top && line < top + rows {
            return;
        }
        let live_top = self.grid.abs_row(0, 0);
        self.scroll_offset =
            if line >= live_top { 0 } else { (live_top + rows / 3).saturating_sub(line).min(self.grid.history_len()) };
    }

    // ───────────── shell integration ─────────────

    /// The shell's working folder: what it told us (OSC 7) when that is a folder, else the
    /// process's own (Linux: `/proc`).
    pub fn cwd(&self) -> Option<PathBuf> {
        if let Some(p) = self.grid.cwd.as_deref().map(PathBuf::from).filter(|p| p.is_dir()) {
            return Some(p);
        }
        #[cfg(target_os = "linux")]
        if self.alive
            && let Some(pid) = self.pid()
        {
            return std::fs::read_link(format!("/proc/{pid}/cwd")).ok().filter(|p| p.is_dir());
        }
        None
    }

    /// Commands that ran, oldest first.
    fn ran(&self) -> impl DoubleEndedIterator<Item = &CmdMark> {
        self.grid.cmds.iter().filter(|c| c.output.is_some())
    }

    /// Jump to the previous (`up`) or next command's prompt, scrolling it into view.
    /// Returns false when there are no marks (the shell has no integration).
    pub fn jump_command(&mut self, up: bool) -> bool {
        let rows = self.grid.rows;
        let top = self.grid.abs_row(0, self.scroll_offset);
        let anchor = self.nav_line.unwrap_or(if up { top + rows } else { top });
        let target = if up {
            self.ran().map(|c| c.prompt).filter(|p| *p < anchor && *p >= self.grid.first_line()).next_back()
        } else {
            self.ran().map(|c| c.prompt).find(|p| *p > anchor)
        };
        if self.ran().next().is_none() {
            return false;
        }
        match target {
            Some(p) => {
                self.nav_line = Some(p);
                // Put it near the top of the view.
                let live_top = self.grid.abs_row(0, 0);
                let back = if p >= live_top { 0 } else { (live_top + 1).saturating_sub(p) };
                self.scroll_offset = back.min(self.grid.history_len());
            }
            None if !up => {
                self.nav_line = None;
                self.scroll_offset = 0;
            }
            None => {}
        }
        true
    }

    /// The finished command navigation points at, else the last finished one.
    fn focus_command(&self) -> Option<CmdMark> {
        let by_nav = self.nav_line.and_then(|l| self.grid.cmds.iter().find(|c| c.prompt == l && c.done()));
        by_nav.or_else(|| self.grid.cmds.iter().rev().find(|c| c.done() && c.output_lines().is_some())).copied()
    }

    /// Select the output of the focused command; false when there is none.
    pub fn select_output(&mut self) -> bool {
        let Some((s, e)) = self.focus_command().and_then(|c| c.output_lines()) else { return false };
        if s < self.grid.first_line() {
            return false;
        }
        self.sel = Some(Sel { anchor: (s, 0), head: (e, self.grid.cols.saturating_sub(1)) });
        self.reveal(s);
        true
    }

    /// The output of the focused command, as text.
    pub fn output_text(&self) -> Option<String> {
        let (s, e) = self.focus_command()?.output_lines()?;
        let sel = Sel { anchor: (s.max(self.grid.first_line()), 0), head: (e, self.grid.cols.saturating_sub(1)) };
        Some(self.grid.sel_text(sel))
    }
}

/// The mark covering absolute line `line` (prompt through output) when the command finished.
pub fn mark_at(g: &Grid, line: usize) -> Option<&CmdMark> {
    let i = g.cmds.partition_point(|c| c.prompt <= line);
    let c = g.cmds.get(i.checked_sub(1)?)?;
    (c.output.is_some() && line < c.end?).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(g: &mut Grid, s: &str) {
        let mut p = vte::Parser::new();
        p.advance(g, s.as_bytes());
    }

    /// A little session: two commands with OSC 133 marks, the second failing.
    fn session(g: &mut Grid) {
        feed(g, "\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07one\r\ntwo\r\n\x1b]133;D;0\x07");
        feed(g, "\x1b]133;A\x07$ \x1b]133;B\x07bad\r\n\x1b]133;C\x07boom\r\n\x1b]133;D;2\x07");
        feed(g, "\x1b]133;A\x07$ \x1b]133;B\x07");
    }

    #[test]
    fn osc7_decodes_the_path() {
        assert_eq!(osc7_path("file://host/home/a%20b/c").as_deref(), Some("/home/a b/c"));
        assert_eq!(osc7_path("file:///tmp").as_deref(), Some("/tmp"));
        assert_eq!(osc7_path("http://x/y"), None);
        assert_eq!(osc7_path("file://host"), None);
        assert_eq!(osc7_path("file://h/%zz").as_deref(), Some("/%zz"), "bad escapes stay as typed");
    }

    #[test]
    fn osc133_marks_commands_with_their_exit_codes() {
        let mut g = Grid::new(10, 20);
        session(&mut g);
        let c: Vec<CmdMark> = g.cmds.iter().copied().collect();
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].prompt, c[0].output, c[0].end, c[0].exit), (0, Some(1), Some(3), Some(0)));
        assert_eq!((c[1].prompt, c[1].output, c[1].end, c[1].exit), (3, Some(4), Some(5), Some(2)));
        assert!(!c[2].done(), "the prompt being typed has not run");
        assert_eq!(c[0].output_lines(), Some((1, 2)));
        assert_eq!(mark_at(&g, 2).map(|m| m.exit), Some(Some(0)));
        assert_eq!(mark_at(&g, 4).map(|m| m.exit), Some(Some(2)));
        assert!(mark_at(&g, 6).is_none());
    }

    #[test]
    fn a_redrawn_prompt_replaces_the_unused_mark_and_marks_stay_bounded() {
        let mut g = Grid::new(4, 20);
        feed(&mut g, "\x1b]133;A\x07$ \x1b]133;A\x07$ ");
        assert_eq!(g.cmds.len(), 1);
        for _ in 0..(MAX_CMDS + 50) {
            feed(&mut g, "\x1b]133;A\x07$ \x1b]133;C\x07x\r\n\x1b]133;D;0\x07");
        }
        assert!(g.cmds.len() <= MAX_CMDS);
        // Lines that left the scrollback take their marks with them.
        assert!(g.cmds.iter().all(|c| c.prompt + 5_000 + 4 >= g.end_line()));
    }

    #[test]
    fn marks_follow_their_text_when_output_scrolls() {
        let mut g = Grid::new(4, 20);
        session(&mut g);
        let first = g.cmds[0];
        feed(&mut g, "a\r\nb\r\nc\r\nd\r\ne\r\n");
        assert_eq!(g.cmds[0], first, "absolute lines do not move");
        assert_eq!(line_chars(&g, 1).0, "one");
        assert_eq!(line_chars(&g, 4).0, "boom");
    }

    #[test]
    fn osc7_sets_the_cwd() {
        let mut g = Grid::new(2, 20);
        feed(&mut g, "\x1b]7;file://host/tmp/my%20dir\x07");
        assert_eq!(g.cwd.as_deref(), Some("/tmp/my dir"));
    }

    #[test]
    fn search_finds_plain_regex_and_case() {
        let mut g = Grid::new(6, 30);
        feed(&mut g, "Error: one\r\nok\r\nerror two ERROR\r\n");
        let re = compile("error", false, false).unwrap();
        let (m, capped) = search(&g, &re);
        assert!(!capped);
        assert_eq!(
            m,
            vec![Match { line: 0, c0: 0, c1: 5 }, Match { line: 2, c0: 0, c1: 5 }, Match { line: 2, c0: 10, c1: 15 }]
        );
        assert_eq!(search(&g, &compile("error", true, false).unwrap()).0.len(), 1, "case sensitive");
        assert_eq!(search(&g, &compile("e.ror", false, false).unwrap()).0.len(), 0, "plain text is literal");
        assert_eq!(search(&g, &compile("e.ror", false, true).unwrap()).0.len(), 3, "regex");
        assert!(compile("(", false, true).is_err());
    }

    #[test]
    fn search_columns_account_for_wide_characters() {
        let mut g = Grid::new(2, 20);
        feed(&mut g, "漢字 abc");
        let (m, _) = search(&g, &compile("abc", true, false).unwrap());
        assert_eq!(m, vec![Match { line: 0, c0: 5, c1: 8 }]);
        let (m, _) = search(&g, &compile("字", true, false).unwrap());
        assert_eq!(m, vec![Match { line: 0, c0: 2, c1: 4 }], "a wide char covers two cells");
    }

    #[test]
    fn find_rows_status_and_cap() {
        let mut f = Find::new();
        assert_eq!(f.status(), "");
        f.query = "x".into();
        assert_eq!(f.status(), "No results");
        f.matches =
            vec![Match { line: 1, c0: 0, c1: 1 }, Match { line: 1, c0: 4, c1: 5 }, Match { line: 3, c0: 0, c1: 1 }];
        f.cur = Some(1);
        assert_eq!(f.status(), "2/3");
        assert_eq!(f.on_line(1).len(), 2);
        assert_eq!(f.on_line(2).len(), 0);
        assert_eq!(f.on_line(3).len(), 1);
        f.bad_regex = true;
        assert_eq!(f.status(), "Bad regex");
        let mut g = Grid::new(3, 10);
        for _ in 0..(MAX_MATCHES + 10) {
            feed(&mut g, "aa\r\n");
        }
        let (m, capped) = search(&g, &compile("a", true, false).unwrap());
        assert!(capped && m.len() == MAX_MATCHES);
    }

    /// A real PTY printing the marks of two commands: jump, select, copy and search work on a `Term`.
    #[test]
    #[cfg(unix)]
    fn term_navigates_commands_and_searches() {
        let script = "printf '\\033]133;A\\007$ \\r\\n\\033]133;C\\007alpha\\r\\nbeta\\r\\n\\033]133;D;0\\007'; \\
             printf '\\033]133;A\\007$ \\r\\n\\033]133;C\\007gamma beta\\r\\n\\033]133;D;3\\007\\033]133;A\\007$ '; sleep 5";
        let wake: crate::lsp::Wake = std::sync::Arc::new(|| {});
        let mut t = Term::spawn_cmd(std::path::Path::new("/"), "/bin/sh", &["-c", script], &[], 4, 30, wake).unwrap();
        let end = Instant::now() + std::time::Duration::from_secs(15);
        while t.grid.cmds.len() < 3 && Instant::now() < end {
            t.pump();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(t.grid.cmds.len(), 3, "three prompts reported");
        assert_eq!(t.grid.cmds[0].exit, Some(0));
        assert_eq!(t.grid.cmds[1].exit, Some(3));
        // The last finished command is the second one.
        assert_eq!(t.output_text().as_deref(), Some("gamma beta"));
        assert!(t.select_output());
        assert_eq!(t.selected_text(), "gamma beta");
        // Jump back to the first command, then forward past the last one (back to the bottom).
        assert!(t.jump_command(true));
        assert_eq!(t.nav_line, Some(t.grid.cmds[1].prompt));
        assert!(t.jump_command(true));
        assert_eq!(t.nav_line, Some(t.grid.cmds[0].prompt));
        assert_eq!(t.output_text().as_deref(), Some("alpha\nbeta"), "output of the command navigated to");
        assert!(t.jump_command(false));
        assert!(t.jump_command(false));
        assert_eq!(t.nav_line, None);
        assert_eq!(t.scroll_offset, 0);
        // Search: two "beta" hits, stepping wraps around, the view reveals the hit.
        t.find_open();
        t.find.as_mut().unwrap().query = "beta".into();
        t.find_changed();
        let f = t.find.as_ref().unwrap();
        assert_eq!(f.matches.len(), 2);
        assert_eq!(f.cur, Some(1), "starts at the newest hit");
        t.find_step(true);
        assert_eq!(t.find.as_ref().unwrap().cur, Some(0), "wraps to the first");
        t.find_step(false);
        assert_eq!(t.find.as_ref().unwrap().cur, Some(1));
        t.find.as_mut().unwrap().regex = true;
        t.find.as_mut().unwrap().query = "(".into();
        t.find_changed();
        assert!(t.find.as_ref().unwrap().bad_regex);
        t.find_close();
        assert!(t.find.is_none());
        t.hangup();
    }
}
