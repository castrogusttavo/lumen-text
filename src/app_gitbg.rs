//! Git work that outlives the Git panel: the result of a fetch / pull / push whose panel was
//! closed while it ran, the ahead / behind counts refreshed in the background, and the small toast
//! that announces results.
//!
//! * A command started from the panel lives in `GitPanel::job`. Closing the panel hands it over to
//!   [`GitBg::detached`]; `App::git_bg_tick` keeps polling it and, when it ends, reports the result
//!   in the status bar and in a toast (and refreshes the tree, the gutter and the counts). Opening
//!   the panel again adopts a job that is still running.
//! * `git rev-list --left-right --count @{u}...HEAD` is cheap (it only reads local refs), so the
//!   counts are recomputed every [`POLL_EVERY`] while the window has the focus, and right when it
//!   gets the focus back. The work happens on a worker thread; the UI only sees the answer.

use crate::{
    app::App,
    git,
    gitsync::{self, Fail, FailKind, Job, Op, SyncInfo},
    ui::{Canvas, Rect},
};
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

/// Interval between background refreshes of the ahead / behind counts.
pub const POLL_EVERY: Duration = Duration::from_secs(60);

/// [`POLL_EVERY`], unless `LUMEN_GIT_POLL_MS` says otherwise (headless runs and tests).
pub fn poll_every() -> Duration {
    static EVERY: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| {
        std::env::var("LUMEN_GIT_POLL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(POLL_EVERY, Duration::from_millis)
    })
}
/// How long a toast stays (failures stay longer: they need reading).
pub const TOAST_OK: Duration = Duration::from_secs(4);
pub const TOAST_ERR: Duration = Duration::from_secs(8);

pub struct Toast {
    pub text: String,
    pub ok: bool,
    pub since: Instant,
}

impl Toast {
    pub fn life(&self) -> Duration {
        if self.ok { TOAST_OK } else { TOAST_ERR }
    }
}

pub struct GitBg {
    /// A command whose panel was closed while it ran.
    pub detached: Option<Job>,
    /// Last counts the background refresh (or the panel) found for `info_root`.
    pub info: Option<SyncInfo>,
    pub info_root: Option<PathBuf>,
    /// When the next refresh is due.
    pub next_poll: Instant,
    /// The window has the keyboard focus (no polling in the background).
    pub focused: bool,
    pending: Option<Receiver<(PathBuf, SyncInfo)>>,
    pub toast: Option<Toast>,
    /// Refreshes started so far (tests, diagnostics).
    pub polls: usize,
    /// The project is a linked git worktree of this main worktree (see `worktree::linked_main`);
    /// refreshed with the git status.
    pub worktree_main: Option<PathBuf>,
}

impl Default for GitBg {
    fn default() -> Self {
        GitBg {
            detached: None,
            info: None,
            info_root: None,
            next_poll: Instant::now(),
            focused: true,
            pending: None,
            toast: None,
            polls: 0,
            worktree_main: None,
        }
    }
}

/// Should a background refresh start now?
pub fn should_poll(focused: bool, pending: bool, busy: bool, next_poll: Instant, now: Instant) -> bool {
    focused && !pending && !busy && next_poll <= now
}

/// The sentence a finished command is reported with.
pub fn report_text(op: &Op, res: &Result<String, Fail>) -> (String, bool) {
    match res {
        Ok(m) => (m.clone(), true),
        Err(f) => {
            let hint = match f.kind {
                FailKind::Rejected | FailKind::Diverged if !matches!(op, Op::Fetch) => {
                    " · open the Git panel (Ctrl+Shift+G)"
                }
                _ => "",
            };
            (format!("{}: {}{hint}", op.label(), f.text), false)
        }
    }
}

impl App {
    /// A toast under the status bar (and nothing else: callers also `say` when they want it kept).
    pub fn toast(&mut self, text: impl Into<String>, ok: bool) {
        self.gitbg.toast = Some(Toast { text: text.into(), ok, since: Instant::now() });
        self.request_redraw();
    }

    /// The panel is going away: a running command keeps going and is reported later.
    pub fn git_detach_job(&mut self, gp: &mut crate::app_gitpanel::GitPanel) {
        if let Some(j) = gp.job.take() {
            self.gitbg.detached = Some(j);
        }
    }

    /// Window focus changed: refresh the counts right away when it comes back.
    pub fn git_bg_focus(&mut self, focused: bool) {
        self.gitbg.focused = focused;
        if focused {
            self.gitbg.next_poll = Instant::now();
        }
    }

    fn git_busy(&self) -> bool {
        self.gitbg.detached.is_some() || self.gitpanel.as_ref().is_some_and(|g| g.job.is_some())
    }

    /// Per-event hook (`App::tick`): collect a detached command, the refresh answer, expire the toast.
    pub fn git_bg_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        // 1. A command that outlived its panel.
        if let Some(job) = self.gitbg.detached.as_mut() {
            match job.poll() {
                Some(res) => {
                    let op = job.op.clone();
                    self.gitbg.detached = None;
                    self.git_report_detached(op, res);
                }
                // The bottom bar spinner and its seconds counter keep turning.
                None => {
                    self.request_redraw();
                    want(now + Duration::from_millis(120));
                }
            }
        }
        // 2. The answer of the background refresh.
        let mut answer = None;
        if let Some(rx) = &self.gitbg.pending {
            match rx.try_recv() {
                Ok(a) => answer = Some(Some(a)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => answer = Some(None),
                Err(std::sync::mpsc::TryRecvError::Empty) => want(now + Duration::from_millis(250)),
            }
        }
        if let Some(a) = answer {
            self.gitbg.pending = None;
            if let Some((root, info)) = a
                && root == self.git_root()
            {
                self.git_apply_info(info);
            }
        }
        // 3. Start the next one.
        let busy = self.git_busy();
        if should_poll(self.gitbg.focused, self.gitbg.pending.is_some(), busy, self.gitbg.next_poll, now) {
            self.git_poll_now();
        }
        if self.gitbg.focused && self.gitbg.pending.is_none() && !busy {
            want(self.gitbg.next_poll);
        }
        // 4. Toast lifetime.
        if let Some(t) = &self.gitbg.toast {
            let end = t.since + t.life();
            if end <= now {
                self.gitbg.toast = None;
                self.request_redraw();
            } else {
                want(end);
            }
        }
    }

    /// Start a refresh of the ahead / behind counts on a worker thread.
    pub fn git_poll_now(&mut self) {
        let (tx, rx) = channel();
        let (root, wake) = (self.git_root(), self.wake());
        self.gitbg.pending = Some(rx);
        self.gitbg.next_poll = Instant::now() + poll_every();
        self.gitbg.polls += 1;
        std::thread::spawn(move || {
            let info = git::toplevel(&root).map(|top| gitsync::info(&top)).unwrap_or_default();
            let _ = tx.send((root, info));
            wake();
        });
    }

    fn git_apply_info(&mut self, info: SyncInfo) {
        let changed = self.gitbg.info.as_ref() != Some(&info);
        if let Some(gp) = self.gitpanel.as_mut() {
            // Never replace the numbers while a command is changing them.
            if gp.job.is_none() && gp.sync != info {
                gp.sync = info.clone();
                self.request_redraw();
            }
        }
        self.gitbg.info_root = Some(self.git_root());
        self.gitbg.info = Some(info);
        if changed {
            self.request_redraw();
        }
    }

    /// Counts to show right now: the open panel's, else the last background answer.
    pub fn git_counts(&self) -> Option<SyncInfo> {
        match &self.gitpanel {
            Some(g) if g.top.is_some() => Some(g.sync.clone()),
            _ => self.gitbg.info.clone().filter(|_| self.gitbg.info_root.as_ref() == Some(&self.git_root())),
        }
    }

    /// A command finished with no panel around to show it.
    fn git_report_detached(&mut self, op: Op, res: Result<String, Fail>) {
        let (text, ok) = report_text(&op, &res);
        self.say(&text);
        self.toast(text, ok);
        // HEAD, the files and the counts may all have moved.
        self.sync_docs_with_head();
        self.refresh_tree();
        self.gitbg.next_poll = Instant::now();
        self.worktree_job_done(&op, &res, None);
    }

    /// Spinner and "Enviando… 4s" in the bottom bar, at (`x`, bar), while a command runs without its
    /// panel. Returns the width taken (0 when nothing runs or there is no `room` for it).
    pub fn draw_git_bg(&mut self, cv: &mut Canvas, x: i32, bar: Rect, room: i32) -> i32 {
        let Some(job) = &self.gitbg.detached else { return 0 };
        let el = job.started.elapsed();
        let label = format!("{}… {}s", job.op.busy(), el.as_secs());
        let phase = (el.as_millis() / 90) as usize;
        let w = 22 + self.tx.ui_width(&label, false) + 14;
        if room < w {
            return 0;
        }
        let c = self.theme.chrome;
        let (cx, cy) = (x + 8, bar.y + bar.h / 2);
        // Eight dots on a ring; the head dot is solid and the ones behind it fade out.
        for k in 0..8usize {
            let a = std::f32::consts::TAU * k as f32 / 8.0;
            let age = (phase + 8 - k) % 8;
            let alpha = (0xff >> (age / 2)).max(0x30) as u8;
            let (dx, dy) = ((a.cos() * 6.0).round() as i32, (a.sin() * 6.0).round() as i32);
            cv.fill_round(Rect::new(cx + dx - 1, cy + dy - 1, 3, 3), 1, c.accent_text.with_alpha(alpha));
        }
        self.tx.draw_ui(
            cv,
            x + 22,
            bar.y + (bar.h - 18) / 2,
            &label,
            c.status_fg,
            Rect::new(x, bar.y, w, bar.h),
            false,
        );
        w
    }

    /// The toast, bottom right, just above the status bar.
    pub fn draw_toast(&mut self, cv: &mut Canvas) {
        let Some(t) = &self.gitbg.toast else { return };
        let (text, ok, age) = (t.text.clone(), t.ok, t.since.elapsed());
        let c = self.theme.chrome;
        let max_w = (self.size.0 as i32 - 32).clamp(160, 440);
        let line = crate::app_gitpanel::fit(&mut self.tx, &text, max_w - 36, false);
        let w = (self.tx.ui_width(&line, false) + 36).min(max_w);
        let h = 34;
        let r = Rect::new(self.size.0 as i32 - w - 16, self.layout.status.y - h - 12, w, h);
        let a = crate::app_tooltip::fade_alpha(age).max(0.0);
        let faded = |col: crate::theme::Rgba| crate::theme::Rgba(col.0, col.1, col.2, (col.3 as f32 * a) as u8);
        if a > 0.6 {
            cv.shadow(r, 10);
        }
        cv.card(r, 10, faded(c.widget_bg), faded(c.border));
        let bar = if ok { c.git_added } else { c.error };
        cv.fill_round(Rect::new(r.x + 10, r.y + 9, 3, h - 18), 1, faded(bar));
        self.tx.draw_ui(cv, r.x + 22, r.y + (h - 18) / 2, &line, faded(c.tab_active_fg), r, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_waits_for_focus_idle_and_the_deadline() {
        let now = Instant::now();
        let later = now + Duration::from_secs(5);
        assert!(should_poll(true, false, false, now, now));
        assert!(!should_poll(false, false, false, now, now), "window not focused");
        assert!(!should_poll(true, true, false, now, now), "one refresh at a time");
        assert!(!should_poll(true, false, true, now, now), "a fetch / pull / push is running");
        assert!(!should_poll(true, false, false, later, now), "not due yet");
        assert!(POLL_EVERY >= Duration::from_secs(30) && POLL_EVERY <= Duration::from_secs(120));
    }

    #[test]
    fn results_are_reported_with_the_command_name() {
        let ok = report_text(&Op::Push, &Ok("Push complete".into()));
        assert_eq!(ok, ("Push complete".to_string(), true));
        let e = Fail { kind: FailKind::Rejected, text: "Rejected".into() };
        let (t, ok) = report_text(&Op::Push, &Err(e.clone()));
        assert!(!ok && t.starts_with("Push: Rejected") && t.contains("Git panel"), "{t}");
        let (t, _) = report_text(&Op::Fetch, &Err(Fail { kind: FailKind::Network, text: "No access".into() }));
        assert_eq!(t, "Fetch: No access");
        let (t, _) = report_text(
            &Op::PullWith(gitsync::PullStrategy::Rebase),
            &Err(Fail { kind: FailKind::Conflict, text: "Conflitos".into() }),
        );
        assert!(t.starts_with("Pull: "), "{t}");
    }

    #[test]
    fn toasts_expire_and_errors_last_longer() {
        let t = |ok| Toast { text: "x".into(), ok, since: Instant::now() };
        assert!(t(true).life() < t(false).life());
    }
}
