//! AI panel model: the sessions, their status heuristic (pure, unit-tested) and the hit targets.

use super::*;

/// What the agent is doing, as far as the terminal shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Launched, nothing printed yet (or still painting its first screen).
    Starting,
    /// Output is streaming, or its title shows a spinner.
    Working,
    /// Idle at its prompt, or it rang / notified for attention (a question, a permission).
    Waiting,
    /// Went quiet after working: the turn is over.
    Done,
    /// The process ended (exit code; 128 + n for a signal).
    Exited(i32),
}

impl Status {
    pub fn label(self) -> String {
        match self {
            Status::Starting => "Starting".into(),
            Status::Working => "Working".into(),
            Status::Waiting => "Waiting for you".into(),
            Status::Done => "Done".into(),
            Status::Exited(c) => format!("Exited ({c})"),
        }
    }
}

/// Quiet for this long = the agent stopped streaming.
pub const IDLE_MS: u64 = 1200;
/// A bell / notification only means "waiting" once the output has been quiet this long.
pub const ATTENTION_MS: u64 = 300;
/// Output this soon after a key we sent is its echo, not the agent working.
pub const ECHO_MS: u64 = 250;
/// The first screen of an agent takes a moment to paint: "Starting" until then.
pub const STARTUP_MS: u64 = 1500;

/// Everything the status heuristic looks at.
pub struct Signals<'a> {
    pub alive: bool,
    pub exit: Option<i32>,
    /// Anything printed at all.
    pub any_output: bool,
    /// Since the process started.
    pub since_start_ms: u64,
    /// Since the last output that was not the echo of our keys (`u64::MAX`: never).
    pub idle_ms: u64,
    /// The terminal title (OSC 0 / 2).
    pub title: &'a str,
    /// A bell or a notification arrived since the user last typed.
    pub attention: bool,
    /// It worked since the user last typed.
    pub worked: bool,
    /// What the agent's own hooks last reported (Claude Code over the IDE bridge): when present it
    /// wins over every heuristic above, except the process having ended.
    pub hook: Option<Status>,
}

/// A title that starts with a spinner frame (braille dots, as agent CLIs animate while busy).
pub fn title_busy(title: &str) -> bool {
    title.chars().next().is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
}

/// The status chip, from process state, output activity and the title.
pub fn status(s: &Signals) -> Status {
    if !s.alive {
        return Status::Exited(s.exit.unwrap_or(-1));
    }
    if let Some(h) = s.hook {
        return h;
    }
    if !s.any_output {
        return Status::Starting;
    }
    if s.attention && s.idle_ms >= ATTENTION_MS {
        return Status::Waiting;
    }
    let busy = s.idle_ms < IDLE_MS || title_busy(s.title);
    if busy {
        return if s.since_start_ms < STARTUP_MS && !s.worked { Status::Starting } else { Status::Working };
    }
    if s.worked { Status::Done } else { Status::Waiting }
}

/// Clickable things of the panel (and of the Git | AI switch above it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AiHit {
    TabGit,
    TabAi,
    Agent,
    Status,
    /// `+` of the session tabs: start another session (choose the agent).
    New,
    Restart,
    Stop,
    Close,
    /// Copy the install command of agent `n` (empty state).
    Install(usize),
    /// Look for installed agents again.
    Refresh,
    /// The IDE bridge indicator (connected or not).
    Ide,
    /// Proposed changes waiting for Accept / Reject.
    Diffs,
    /// Session tab `n`: click shows it, double-click renames it, middle-click closes it.
    Session(usize),
    /// The close button of session tab `n`.
    CloseSession(usize),
}

/// What one session's process looked like at a moment (taken from its terminal).
pub struct Obs<'a> {
    pub alive: bool,
    pub exit: Option<i32>,
    pub started: Instant,
    pub last_output: Option<Instant>,
    pub last_input: Option<Instant>,
    /// Bells rung so far.
    pub bells: u64,
    pub title: &'a str,
}

/// What the status heuristic remembers about one session (no process inside: unit-tested).
pub struct Track {
    pub status: Status,
    /// Last output that was not an echo of our keys.
    pub last_activity: Option<Instant>,
    /// A bell / notification since the user last typed.
    pub attention: bool,
    pub worked: bool,
    seen_bells: u64,
    /// It finished (or asked for attention) while nobody looked at it: its tab and the status bar
    /// show it.
    pub unseen: bool,
    /// What the agent's hooks last reported (cleared when the user types).
    pub hook: Option<Status>,
    /// A desktop notification went out for this turn (until the user types again): one turn,
    /// one notification.
    pub notified: bool,
}

impl Default for Track {
    fn default() -> Self {
        Track {
            status: Status::Starting,
            last_activity: None,
            attention: false,
            worked: false,
            seen_bells: 0,
            unseen: false,
            hook: None,
            notified: false,
        }
    }
}

/// What folding an observation in did.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Change {
    /// The status chip changed.
    pub changed: bool,
    /// It just finished a turn or started asking for an answer.
    pub alert: bool,
}

impl Track {
    /// Fold the process's latest state into the status. `watching`: the user has this session in
    /// front of them (shown and focused), so an alert leaves no "unseen" mark.
    pub fn observe(&mut self, now: Instant, o: &Obs, watching: bool) -> Change {
        if let Some(out) = o.last_output {
            let echo = o.last_input.is_some_and(|i| out >= i && out.duration_since(i).as_millis() as u64 <= ECHO_MS);
            if !echo && self.last_activity.is_none_or(|a| out > a) {
                self.last_activity = Some(out);
            }
        }
        if o.bells != self.seen_bells {
            self.seen_bells = o.bells;
            self.attention = true;
        }
        let ms = |i: Instant| now.saturating_duration_since(i).as_millis() as u64;
        let s = status(&Signals {
            alive: o.alive,
            exit: o.exit,
            any_output: o.last_output.is_some(),
            since_start_ms: ms(o.started),
            idle_ms: self.last_activity.map_or(u64::MAX, ms),
            title: o.title,
            attention: self.attention,
            worked: self.worked,
            hook: self.hook,
        });
        if s == Status::Working {
            self.worked = true;
        }
        let was = self.status;
        self.status = s;
        // Finished or asking: worth a mark when nobody is looking.
        let alert =
            (was == Status::Working && s != Status::Working) || (self.attention && s == Status::Waiting && was != s);
        if alert {
            self.unseen = !watching;
        }
        Change { changed: was != s, alert }
    }

    /// Worth a desktop notification: it just finished or asked, and none went out this turn.
    pub fn wants_notice(&self, c: Change) -> bool {
        c.alert && matches!(self.status, Status::Waiting | Status::Done) && !self.notified
    }

    /// The user typed: whatever it asked for is being answered, a new turn begins.
    pub fn on_input(&mut self) {
        self.attention = false;
        self.worked = false;
        self.unseen = false;
        self.notified = false;
        // The heuristic takes over until the agent's next hook (an answered prompt resumes work).
        self.hook = None;
    }
}

/// One agent running in the panel: its own terminal, status and folder.
pub struct Session {
    /// Stable id (its hook URLs end in it).
    pub id: u64,
    pub agent: &'static Agent,
    /// The tab's name (the agent's name until renamed).
    pub name: String,
    /// Where the agent runs: the project, or another folder such as a worktree of it.
    pub cwd: PathBuf,
    pub term: Term,
    pub track: Track,
}

impl Session {
    pub fn status(&self) -> Status {
        self.track.status
    }

    /// Fold the terminal's latest state into the status.
    pub fn refresh(&mut self, now: Instant, watching: bool) -> Change {
        let t = &self.term;
        let o = Obs {
            alive: t.alive,
            exit: t.exit_code,
            started: t.started,
            last_output: t.last_output,
            last_input: t.last_input,
            bells: t.grid.bells,
            title: &t.grid.title,
        };
        self.track.observe(now, &o, watching)
    }
}

/// The name of a new session: the agent's, with the folder when it is not the project's, and a
/// number when that name is taken.
pub fn session_name(agent: &Agent, cwd: &Path, root: &Path, taken: &[&str]) -> String {
    let base = match cwd.file_name().filter(|_| cwd != root) {
        Some(dir) => format!("{} · {}", agent.name, dir.to_string_lossy()),
        None => agent.name.to_string(),
    };
    if !taken.contains(&base.as_str()) {
        return base;
    }
    (2..).map(|n| format!("{base} {n}")).find(|n| !taken.contains(&n.as_str())).unwrap_or(base)
}

/// The next session after `from` (wrapping, `from` itself last) whose agent waits for an answer.
pub fn next_waiting(statuses: &[Status], from: usize) -> Option<usize> {
    let n = statuses.len();
    (1..=n).map(|k| (from + k) % n).find(|&i| statuses[i] == Status::Waiting)
}

/// The status bar badge for sessions nobody is looking at, from (status, unseen) of each:
/// "Waiting for you", "2 waiting · 1 done", "Agent done", "3 done", "Agent exited". The status
/// returned picks the color.
pub fn badge(sessions: &[(Status, bool)]) -> Option<(String, Status)> {
    let count = |f: fn(Status) -> bool| sessions.iter().filter(|(s, unseen)| *unseen && f(*s)).count();
    let waiting = count(|s| s == Status::Waiting);
    let done = count(|s| s == Status::Done);
    let exited = count(|s| matches!(s, Status::Exited(_)));
    if waiting > 0 {
        let mut text = if waiting == 1 { "Waiting for you".to_string() } else { format!("{waiting} waiting") };
        if done > 0 {
            text.push_str(&format!(" · {done} done"));
        }
        return Some((text, Status::Waiting));
    }
    match (done, exited) {
        (1, _) => Some(("Agent done".into(), Status::Done)),
        (n, _) if n > 1 => Some((format!("{n} done"), Status::Done)),
        (_, n) if n > 0 => Some(("Agent exited".into(), Status::Exited(0))),
        _ => None,
    }
}

/// The task an agent shows in its title (`✳ Fixing the login bug`), without the leading glyph.
pub fn title_task(title: &str) -> &str {
    title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim_end()
}

/// The parent of a process from its `/proc/<pid>/stat` line (the name may hold spaces and
/// parentheses: the fields after the last `)` are what counts).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn ppid_from_stat(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Is `pid` the process `ancestor` or one it started? (Linux looks up the parents; elsewhere only
/// the same pid counts — agent CLIs `exec` into their runtime, which keeps it.)
pub fn same_or_child(pid: u32, ancestor: u32) -> bool {
    if pid == ancestor {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let mut p = pid;
        for _ in 0..16 {
            match std::fs::read_to_string(format!("/proc/{p}/stat")).ok().as_deref().and_then(ppid_from_stat) {
                Some(pp) if pp == ancestor => return true,
                Some(pp) if pp > 1 && pp != p => p = pp,
                _ => return false,
            }
        }
    }
    false
}

pub struct AiState {
    /// The panel is the visible half of the right-hand slot.
    pub open: bool,
    /// Keys go to the agent.
    pub focused: bool,
    /// Every agent running (or ended, its last screen kept) in the panel: one tab each.
    pub sessions: Vec<Session>,
    /// The session shown.
    pub active: usize,
    /// The agent picked last: started by default, shown when nothing runs.
    pub last_agent: Option<&'static Agent>,
    /// Agents found at the last look (`None` = not looked yet: detection is lazy).
    pub detected: Option<Vec<(&'static Agent, Option<PathBuf>)>>,
    /// Directories searched instead of PATH (tests).
    pub search: Option<Vec<PathBuf>>,
    /// The project the sessions belong to.
    pub root: PathBuf,
    next_id: u64,
    /// The Lumen window has the keyboard focus: desktop notifications only go out without it.
    pub window_focused: bool,
    /// The last click on a session tab (a second one soon after renames it).
    pub last_tab_click: Option<(u64, Instant)>,
    pub hits: Vec<(Rect, AiHit)>,
    /// The left edge is being dragged.
    pub resizing: bool,
    /// The IDE bridge (Claude Code's diffs, selection, diagnostics, hooks).
    pub bridge: super::bridge::Bridge,
    /// Desktop notifications "sent" (title, body): tests look here, nothing pops up.
    #[cfg(test)]
    pub sent: Vec<(String, String)>,
}

impl Default for AiState {
    fn default() -> Self {
        AiState {
            open: false,
            focused: false,
            sessions: Vec::new(),
            active: 0,
            last_agent: None,
            detected: None,
            search: None,
            root: PathBuf::new(),
            next_id: 0,
            window_focused: true,
            last_tab_click: None,
            hits: Vec::new(),
            resizing: false,
            bridge: Default::default(),
            #[cfg(test)]
            sent: Vec::new(),
        }
    }
}

impl AiState {
    pub fn hit_at(&self, x: f64, y: f64) -> Option<(Rect, AiHit)> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).copied()
    }

    /// The executable found for `a` at the last detection.
    pub fn path_of(&self, a: &Agent) -> Option<PathBuf> {
        self.detected.as_ref()?.iter().find(|(d, _)| d.id == a.id).and_then(|(_, p)| p.clone())
    }

    /// The session shown.
    pub fn cur(&self) -> Option<&Session> {
        self.sessions.get(self.active)
    }

    pub fn cur_mut(&mut self) -> Option<&mut Session> {
        self.sessions.get_mut(self.active)
    }

    /// The terminal of the session shown.
    pub fn term(&self) -> Option<&Term> {
        self.cur().map(|s| &s.term)
    }

    pub fn term_mut(&mut self) -> Option<&mut Term> {
        self.cur_mut().map(|s| &mut s.term)
    }

    /// The agent of the session shown, else the one picked last.
    pub fn agent(&self) -> Option<&'static Agent> {
        self.cur().map(|s| s.agent).or(self.last_agent)
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    pub(super) fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// The user has session `i` in front of them.
    pub fn watching(&self, i: usize) -> bool {
        self.open && self.focused && i == self.active
    }

    /// The user typed into the session shown.
    pub fn on_input(&mut self) {
        if let Some(s) = self.cur_mut() {
            s.track.on_input();
        }
    }

    /// The status bar badge: sessions that finished or ask while nobody looked.
    pub fn badge(&self) -> Option<(String, Status)> {
        let list: Vec<(Status, bool)> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.watching(*i))
            .map(|(_, s)| (s.status(), s.track.unseen))
            .collect();
        badge(&list)
    }
}
