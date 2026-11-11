//! AI panel model: the agent session, its status heuristic (pure, unit-tested) and the hit targets.

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
}

pub struct AiState {
    /// The panel is the visible half of the right-hand slot.
    pub open: bool,
    /// Keys go to the agent.
    pub focused: bool,
    pub term: Option<Term>,
    /// The agent of the session (also the one shown when nothing runs).
    pub agent: Option<&'static Agent>,
    /// Agents found at the last look (`None` = not looked yet: detection is lazy).
    pub detected: Option<Vec<(&'static Agent, Option<PathBuf>)>>,
    /// Directories searched instead of PATH (tests).
    pub search: Option<Vec<PathBuf>>,
    /// The project the session runs in.
    pub root: PathBuf,
    pub status: Status,
    /// Last output that was not an echo of our keys.
    pub last_activity: Option<Instant>,
    /// A bell / notification since the user last typed.
    pub attention: bool,
    pub worked: bool,
    seen_bells: u64,
    /// It finished (or asked for attention) while the panel was hidden: the status bar shows a badge.
    pub unseen: bool,
    pub hits: Vec<(Rect, AiHit)>,
    /// The left edge is being dragged.
    pub resizing: bool,
    /// The IDE bridge (Claude Code's diffs, selection, diagnostics, hooks).
    pub bridge: super::bridge::Bridge,
}

impl Default for AiState {
    fn default() -> Self {
        AiState {
            open: false,
            focused: false,
            term: None,
            agent: None,
            detected: None,
            search: None,
            root: PathBuf::new(),
            status: Status::Starting,
            last_activity: None,
            attention: false,
            worked: false,
            seen_bells: 0,
            unseen: false,
            hits: Vec::new(),
            resizing: false,
            bridge: Default::default(),
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

    /// Fold the terminal's latest state into the status; true when the chip changed.
    pub fn refresh_status(&mut self, now: Instant) -> bool {
        let Some(t) = self.term.as_ref() else { return false };
        if let Some(out) = t.last_output {
            let echo = t.last_input.is_some_and(|i| out >= i && out.duration_since(i).as_millis() as u64 <= ECHO_MS);
            if !echo && self.last_activity.is_none_or(|a| out > a) {
                self.last_activity = Some(out);
            }
        }
        if t.grid.bells != self.seen_bells {
            self.seen_bells = t.grid.bells;
            self.attention = true;
        }
        let ms = |i: Instant| now.saturating_duration_since(i).as_millis() as u64;
        let s = status(&Signals {
            alive: t.alive,
            exit: t.exit_code,
            any_output: t.last_output.is_some(),
            since_start_ms: ms(t.started),
            idle_ms: self.last_activity.map_or(u64::MAX, ms),
            title: &t.grid.title,
            attention: self.attention,
            worked: self.worked,
            hook: self.bridge.hook,
        });
        if s == Status::Working {
            self.worked = true;
        }
        let was = self.status;
        self.status = s;
        if (was == Status::Working && s != Status::Working) || (self.attention && s == Status::Waiting && was != s) {
            // Finished or asking: worth a badge when nobody is looking.
            self.unseen = !(self.open && self.focused);
        }
        was != s
    }

    /// A new process started: forget what the last one did.
    pub fn reset_session(&mut self) {
        self.status = Status::Starting;
        self.last_activity = None;
        self.attention = false;
        self.worked = false;
        self.seen_bells = 0;
        self.unseen = false;
        self.bridge.hook = None;
    }

    /// The user typed: whatever it asked for is being answered.
    pub fn on_input(&mut self) {
        self.attention = false;
        self.worked = false;
        self.unseen = false;
        // The heuristic takes over until the agent's next hook (an answered prompt resumes work).
        self.bridge.hook = None;
    }
}
