//! GUI side of the auto-updater: background checks, the small update card, the bottom bar
//! indicator and the restart. The engine itself is in `update.rs`.

use crate::{
    app::App,
    ui::{Canvas, Rect, TextCtx},
    update::{self, Announcer, Env, InstallKind, Release, Schedule},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::{Duration, Instant},
};
use winit::keyboard::{Key, NamedKey};

/// What the updater knows right now.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// A newer release exists; nothing was installed (notify mode, or this install can't update itself).
    Available(Release),
    /// The new version is installed on disk: a restart switches to it.
    Ready(Release),
}

/// The card floating above the bottom bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Card {
    Checking,
    Downloading(String),
    UpToDate,
    Available(Release),
    Ready(Release),
    Error(String),
    About,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    Restart,
    Close,
    Notes,
    Install,
    CopyCommand,
    CheckAgain,
}

enum Job {
    Check { manual: bool, install: bool },
    Install(Release),
}

enum Outcome {
    UpToDate,
    Available(Release),
    Ready(Release, PathBuf),
}

struct Msg {
    manual: bool,
    res: Result<Outcome, String>,
}

pub struct UpdateUi {
    env: Env,
    pub phase: Phase,
    pub card: Option<Card>,
    sched: Schedule,
    announcer: Announcer,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    busy: bool,
    progress: Arc<AtomicU64>,
    /// Version already installed by this session and the executable to start on restart.
    installed: Option<(String, PathBuf)>,
    card_until: Option<Instant>,
    pub(crate) rects: Vec<(Rect, Btn)>,
    logo: Option<crate::mdimg::Pix>,
    pub(crate) card_rect: Rect,
    headless: bool,
}

impl UpdateUi {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let delay = std::env::var("LUMEN_UPDATE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(update::STARTUP_DELAY, Duration::from_millis);
        UpdateUi {
            env: Env::detect(),
            phase: Phase::Idle,
            card: None,
            sched: Schedule::new(Instant::now(), delay),
            announcer: Announcer::default(),
            tx,
            rx,
            busy: false,
            progress: Arc::new(AtomicU64::new(0)),
            installed: None,
            card_until: None,
            rects: Vec::new(),
            logo: None,
            card_rect: Rect::default(),
            headless: std::env::var_os("LUMEN_HEADLESS").is_some(),
        }
    }

    /// Show the bottom bar indicator?
    pub fn indicator(&self) -> Option<&Release> {
        match &self.phase {
            Phase::Available(r) | Phase::Ready(r) => Some(r),
            Phase::Idle => None,
        }
    }

    pub fn ready(&self) -> bool {
        matches!(self.phase, Phase::Ready(_))
    }
}

/// Greedy word wrap to `max` px; words wider than the line are cut.
fn wrap(tx: &mut TextCtx, text: &str, max: i32, bold: bool) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let try_line = if cur.is_empty() { word.clone() } else { format!("{cur} {word}") };
            if tx.ui_width(&try_line, bold) <= max {
                cur = try_line;
                break;
            }
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                continue;
            }
            // A single word too wide for the line: cut it where it still fits.
            let mut n = word.chars().count();
            while n > 1 && tx.ui_width(&word.chars().take(n).collect::<String>(), bold) > max {
                n -= 1;
            }
            lines.push(word.chars().take(n).collect());
            word = word.chars().skip(n).collect();
            if word.is_empty() {
                break;
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn open_url(url: &str) {
    crate::paths::open_url(url);
}

/// Texts and buttons of a card: (title, body paragraphs, buttons left to right with the primary
/// one flagged, text link on the left).
type CardContent = (String, Vec<String>, Vec<(Btn, &'static str, bool)>, Option<(Btn, &'static str)>);

impl App {
    /// The setting changed in the settings modal: start (or stop) the periodic checks.
    pub fn update_setting_changed(&mut self) {
        if self.config.updates != "off" {
            self.upd.sched = Schedule::new(Instant::now(), Duration::from_secs(3));
        }
    }

    fn update_spawn(&mut self, job: Job) {
        if self.upd.busy {
            return;
        }
        self.upd.busy = true;
        self.upd.progress.store(0, Ordering::Relaxed);
        let (env, tx, progress, wake) =
            (self.upd.env.clone(), self.upd.tx.clone(), self.upd.progress.clone(), self.wake());
        let skip = self.upd.installed.clone();
        let manual = matches!(job, Job::Check { manual: true, .. });
        let spawned = std::thread::Builder::new().name("update".into()).stack_size(1 << 20).spawn(move || {
            let res = run_job(&env, job, skip, &progress);
            let _ = tx.send(Msg { manual, res });
            wake();
        });
        if spawned.is_err() {
            self.upd.busy = false;
        }
    }

    /// Timers: the startup check (after ~20 s), then every 4 hours; failures retry with backoff.
    pub fn update_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if let Some(t) = self.upd.card_until {
            if t <= now {
                self.upd.card_until = None;
                if matches!(self.upd.card, Some(Card::UpToDate)) {
                    self.upd.card = None;
                    self.request_redraw();
                }
            } else {
                want(t);
            }
        }
        if self.upd.busy {
            if matches!(self.upd.card, Some(Card::Downloading(_))) {
                self.request_redraw();
                want(now + Duration::from_millis(300));
            }
            return;
        }
        if self.config.updates == "off" || !self.upd.env.auto_checks() || (self.upd.headless && !self.upd.env.forced) {
            return;
        }
        if self.upd.sched.due(now) {
            let install = self.config.updates == "auto";
            self.update_spawn(Job::Check { manual: false, install });
        } else {
            want(self.upd.sched.next());
        }
    }

    /// Results of the worker thread (called from `pump_background`).
    pub fn pump_update(&mut self) {
        while let Ok(m) = self.upd.rx.try_recv() {
            self.upd.busy = false;
            let now = Instant::now();
            match m.res {
                Ok(Outcome::UpToDate) => {
                    self.upd.sched.on_success(now);
                    if m.manual {
                        self.upd.card = Some(Card::UpToDate);
                        self.upd.card_until = Some(now + Duration::from_secs(10));
                    } else if matches!(self.upd.card, Some(Card::Checking)) {
                        self.upd.card = None;
                    }
                }
                Ok(Outcome::Available(rel)) => {
                    self.upd.sched.on_success(now);
                    let first = self.upd.announcer.should_announce(&rel.version);
                    self.upd.phase = Phase::Available(rel.clone());
                    if m.manual || first {
                        self.upd.card = Some(Card::Available(rel));
                        self.upd.card_until = None;
                    } else if matches!(self.upd.card, Some(Card::Checking | Card::Downloading(_))) {
                        self.upd.card = None;
                    }
                }
                Ok(Outcome::Ready(rel, exe)) => {
                    self.upd.sched.on_success(now);
                    let first = self.upd.announcer.should_announce(&rel.version);
                    self.upd.installed = Some((rel.version.clone(), exe));
                    self.upd.phase = Phase::Ready(rel.clone());
                    if m.manual || first || matches!(self.upd.card, Some(Card::Downloading(_))) {
                        self.upd.card = Some(Card::Ready(rel));
                        self.upd.card_until = None;
                    }
                }
                Err(e) => {
                    eprintln!("[update] {e}");
                    self.upd.sched.on_failure(now);
                    if m.manual || matches!(self.upd.card, Some(Card::Downloading(_))) {
                        self.upd.card = Some(Card::Error(e));
                        self.upd.card_until = None;
                    } else if matches!(self.upd.card, Some(Card::Checking)) {
                        self.upd.card = None;
                    }
                }
            }
            self.request_redraw();
        }
    }

    /// Palette "Check for updates": always ends in a card (up to date / available / error).
    pub fn update_check_manual(&mut self) {
        if let Phase::Ready(r) = self.upd.phase.clone() {
            self.upd.card = Some(Card::Ready(r));
            return self.request_redraw();
        }
        self.upd.card = Some(Card::Checking);
        self.upd.card_until = None;
        let install = self.config.updates == "auto";
        self.update_spawn(Job::Check { manual: true, install });
        self.request_redraw();
    }

    pub fn update_show_about(&mut self) {
        self.upd.card = Some(Card::About);
        self.upd.card_until = None;
        self.request_redraw();
    }

    /// The bottom bar indicator was clicked: show what is going on.
    pub fn update_indicator_click(&mut self) {
        self.upd.card = match self.upd.phase.clone() {
            Phase::Ready(r) => Some(Card::Ready(r)),
            Phase::Available(r) => Some(Card::Available(r)),
            Phase::Idle => None,
        };
        self.upd.card_until = None;
    }

    pub fn update_indicator_tip(&self) -> (String, Option<String>) {
        match &self.upd.phase {
            Phase::Ready(r) => (format!("Lumen {} is ready", r.version), Some("Click to restart and update".into())),
            Phase::Available(r) => {
                (format!("Lumen {} available", r.version), Some("Click to see how to update".into()))
            }
            Phase::Idle => ("Updates".into(), None),
        }
    }

    /// Palette "Reiniciar para atualizar".
    pub fn update_restart_cmd(&mut self) {
        if self.upd.ready() {
            self.update_restart();
        } else {
            self.say("No update ready — use \"Check for updates\"");
        }
    }

    /// Start the new version and quit this one (the single-instance socket is released first so
    /// the new process doesn't hand its files over to this dying one).
    pub fn update_restart(&mut self) {
        let Some((_, exe)) = self.upd.installed.clone() else { return };
        // With hot exit the unsaved buffers come back after the restart.
        if self.restart_needs_save() {
            self.say("Save your files before restarting to update");
            return self.request_redraw();
        }
        if !exe.exists() {
            self.say(format!("Could not find {} — reopen Lumen manually", exe.display()));
            return self.request_redraw();
        }
        if !self.try_quit() {
            return;
        }
        let _ = std::fs::remove_file(crate::instance::socket_path());
        spawn_detached(&exe, &self.root, matches!(self.upd.env.kind, InstallKind::AppImage(_)));
        std::process::exit(0);
    }

    // ── card ──

    fn card_content(&self, card: &Card) -> CardContent {
        let env = &self.upd.env;
        let notes = |r: &Release| update::notes_summary(&r.notes, 3, 70);
        match card {
            Card::Checking => ("Checking for updates…".into(), vec![], vec![], None),
            Card::Downloading(v) => {
                let mb = self.upd.progress.load(Ordering::Relaxed) as f64 / 1_048_576.0;
                (format!("Baixando o Lumen {v}…"), vec![format!("{mb:.1} MB baixados")], vec![], None)
            }
            Card::UpToDate => {
                let local = if update::is_release_build(&env.current) { "" } else { " (local build)" };
                (
                    "Lumen is up to date".into(),
                    vec![format!("Version {}{local}", env.current)],
                    vec![(Btn::Close, "Close", true)],
                    None,
                )
            }
            Card::Ready(r) => {
                let mut body = vec!["Restart to update. Your tabs and session are restored.".to_string()];
                body.extend(notes(r));
                (
                    format!("Lumen {} is ready — Restart to update", r.version),
                    body,
                    vec![(Btn::Close, "Later", false), (Btn::Restart, "Restart now", true)],
                    Some((Btn::Notes, "Release notes")),
                )
            }
            Card::Available(r) => {
                let mut body = Vec::new();
                let mut buttons = vec![(Btn::Close, "Later", false)];
                if env.kind.can_self_update() && r.asset.is_some() {
                    body.push(
                        "The new version will be downloaded, verified and installed in the background.".to_string(),
                    );
                    buttons.push((Btn::Install, "Update now", true));
                } else if let Some(cmd) = &r.command {
                    body.push("Installed by the package manager. To update, run:".to_string());
                    body.push(format!("\u{1}{cmd}"));
                    buttons.push((Btn::CopyCommand, "Copy command", true));
                } else if matches!(env.kind, InstallKind::System) {
                    body.push(
                        "Installed by the package manager: download the new version's package from the releases page."
                            .to_string(),
                    );
                    buttons.push((Btn::Notes, "Open page", true));
                } else {
                    body.push("Download the new version from the releases page, or run \"lumen-text --install-user\" from it to get automatic updates.".to_string());
                    buttons.push((Btn::Notes, "Open page", true));
                }
                body.extend(notes(r));
                (format!("Lumen {} available", r.version), body, buttons, Some((Btn::Notes, "Release notes")))
            }
            Card::Error(e) => (
                "Could not check for updates".into(),
                vec![e.clone()],
                vec![(Btn::Close, "Close", false), (Btn::CheckAgain, "Try again", true)],
                None,
            ),
            Card::About => {
                let mode = match self.config.updates.as_str() {
                    "off" => "off",
                    "notify" => "notify only",
                    _ => "automatic",
                };
                let how = if env.kind.can_self_update() {
                    env.kind.label().to_string()
                } else {
                    format!("{} (no self-update)", env.kind.label())
                };
                (
                    format!("Lumen {}", env.current),
                    vec![
                        format!("Installation: {how}"),
                        format!("Updates: {mode}"),
                        format!("github.com/{}", update::REPO),
                    ],
                    vec![(Btn::Close, "Close", false), (Btn::CheckAgain, "Check for updates", true)],
                    None,
                )
            }
        }
    }

    pub fn draw_update_card(&mut self, cv: &mut Canvas) {
        self.upd.rects.clear();
        self.upd.card_rect = Rect::default();
        let Some(card) = self.upd.card.clone() else { return };
        let (title, body, buttons, link) = self.card_content(&card);
        let c = self.theme.chrome;
        let (fg, dim) = (self.theme.foreground, c.tab_inactive_fg);
        let sw = self.size.0 as i32;
        let w = (sw - 24).clamp(240, 420);
        let inner = w - 32;
        let logo = matches!(card, Card::About);
        let title_dx = if logo { 28 } else { 0 };
        let title_lines = wrap(&mut self.tx, &title, inner - 22 - title_dx, true);
        let mut lines: Vec<(String, bool)> = Vec::new();
        for p in &body {
            // A paragraph starting with U+0001 is a shell command: shown on a code-like block.
            let (text, cmd) = p.strip_prefix('\u{1}').map_or((p.as_str(), false), |t| (t, true));
            for l in wrap(&mut self.tx, text, inner, false) {
                lines.push((l, cmd));
            }
        }
        let has_row = !buttons.is_empty() || link.is_some();
        let btn_h = 30;
        let h = 14
            + title_lines.len() as i32 * 22
            + if lines.is_empty() { 0 } else { 6 + lines.len() as i32 * 20 }
            + if has_row { 14 + btn_h } else { 0 }
            + 14;
        let r = Rect::new(sw - w - 12, (self.layout.status.y - h - 10).max(0), w, h);
        cv.shadow(r, 12);
        cv.card(r, 12, c.widget_bg, c.border);
        // Title + close.
        for (i, t) in title_lines.iter().enumerate() {
            self.tx.draw_ui(cv, r.x + 16 + title_dx, r.y + 14 + i as i32 * 22, t, fg, r, true);
        }
        if logo {
            let px = crate::ui::sc(20).max(1) as u32;
            if self.upd.logo.as_ref().is_none_or(|l| l.w != px) {
                static PNG: &[u8] = include_bytes!("../packaging/icons/png/lumen-text-256.png");
                self.upd.logo = crate::mdimg::decode_png(PNG).map(|p| crate::mdimg::scale_to(p, px, px));
            }
            if let Some(l) = &self.upd.logo {
                let full = Rect::new(0, 0, cv.w, cv.h);
                crate::mdimg::blit(cv, crate::ui::sc(r.x + 16), crate::ui::sc(r.y + 15), l, full);
            }
        }
        let x_btn = Rect::new(r.right() - 30, r.y + 8, 22, 22);
        let hot = x_btn.contains(self.mouse.0, self.mouse.1);
        if hot {
            cv.fill_round(x_btn, 6, c.list_hover_bg);
        }
        self.tx.icon(cv, "ui/close", x_btn.x + 4, x_btn.y + 4, 14, if hot { fg } else { dim }, r);
        self.upd.rects.push((x_btn, Btn::Close));
        // Body.
        let mut y = r.y + 14 + title_lines.len() as i32 * 22 + 6;
        for (l, code) in &lines {
            if *code {
                let bg = Rect::new(r.x + 12, y - 1, r.w - 24, 20);
                cv.fill_round(bg, 4, fg.with_alpha(0x14));
            }
            self.tx.draw_ui(cv, r.x + 16, y, l, if *code { fg } else { dim }, r, false);
            y += 20;
        }
        // Buttons (right-aligned, primary last) and the text link on the left.
        if has_row {
            let by = r.bottom() - 14 - btn_h;
            let mut x = r.right() - 12;
            for (btn, label, primary) in buttons.iter().rev() {
                let bw = self.tx.ui_width(label, *primary) + 26;
                x -= bw;
                let rect = Rect::new(x, by, bw, btn_h);
                let hot = rect.contains(self.mouse.0, self.mouse.1);
                if *primary {
                    cv.fill_round(rect, 8, if hot { c.accent.shift(14) } else { c.accent });
                } else {
                    cv.card(rect, 8, if hot { c.list_hover_bg } else { c.widget_bg }, c.border);
                }
                let tw = self.tx.ui_width(label, *primary);
                self.tx.draw_ui(
                    cv,
                    rect.x + (rect.w - tw) / 2,
                    rect.y + (btn_h - 18) / 2,
                    label,
                    if *primary { c.on_accent } else { fg },
                    rect,
                    *primary,
                );
                self.upd.rects.push((rect, *btn));
                x -= 8;
            }
            if let Some((btn, label)) = link {
                let lw = self.tx.ui_width(label, false);
                let rect = Rect::new(r.x + 16, by, lw, btn_h);
                if x - (r.x + 16) >= lw {
                    let hot = rect.contains(self.mouse.0, self.mouse.1);
                    self.tx.draw_ui(
                        cv,
                        rect.x,
                        rect.y + (btn_h - 18) / 2,
                        label,
                        if hot { fg } else { c.accent_text },
                        rect,
                        false,
                    );
                    if hot {
                        cv.fill(Rect::new(rect.x, rect.y + (btn_h + 18) / 2, lw, 1), fg);
                    }
                    self.upd.rects.push((rect, btn));
                }
            }
        }
        self.upd.card_rect = r;
    }

    /// Mouse press: true when the card consumed it.
    pub fn update_card_mouse(&mut self, x: f64, y: f64) -> bool {
        if self.upd.card.is_none() || !self.upd.card_rect.contains(x, y) {
            return false;
        }
        if let Some((_, btn)) = self.upd.rects.iter().find(|(r, _)| r.contains(x, y)).copied() {
            self.update_card_action(btn);
        }
        true
    }

    /// Escape closes the card (when nothing modal is open).
    pub fn update_card_key(&mut self, key: &Key) -> bool {
        if self.upd.card.is_some()
            && matches!(key, Key::Named(NamedKey::Escape))
            && !matches!(self.upd.card, Some(Card::Checking | Card::Downloading(_)))
        {
            self.upd.card = None;
            return true;
        }
        false
    }

    pub fn update_card_action(&mut self, btn: Btn) {
        let rel = match &self.upd.card {
            Some(Card::Available(r) | Card::Ready(r)) => Some(r.clone()),
            _ => None,
        };
        match btn {
            Btn::Close => self.upd.card = None,
            Btn::Restart => self.update_restart(),
            Btn::Notes => {
                if let Some(r) = rel {
                    open_url(&r.page);
                }
            }
            Btn::CopyCommand => {
                if let Some(cmd) = rel.and_then(|r| r.command) {
                    self.clip.set(&cmd);
                    self.say("command copied");
                }
                self.upd.card = None;
            }
            Btn::Install => {
                if let Some(r) = rel {
                    self.upd.card = Some(Card::Downloading(r.version.clone()));
                    self.update_spawn(Job::Install(r));
                }
            }
            Btn::CheckAgain => self.update_check_manual(),
        }
        self.request_redraw();
    }
}

/// Run on the worker thread.
fn run_job(env: &Env, job: Job, installed: Option<(String, PathBuf)>, progress: &AtomicU64) -> Result<Outcome, String> {
    let finish = |rel: Release| -> Result<Outcome, String> {
        if let Some((_, exe)) = installed.clone().filter(|(v, _)| *v == rel.version) {
            return Ok(Outcome::Ready(rel, exe));
        }
        let exe = update::update(env, &rel, Some(progress))?;
        Ok(Outcome::Ready(rel, exe))
    };
    match job {
        Job::Check { install, .. } => match update::check(env)? {
            None => Ok(Outcome::UpToDate),
            Some(rel) if install && env.kind.can_self_update() && rel.asset.is_some() => finish(rel),
            Some(rel) => Ok(Outcome::Available(rel)),
        },
        Job::Install(rel) => finish(rel),
    }
}

/// Start `exe <root>` in a new session after a short delay, so it outlives this process and the
/// single-instance socket is long gone when it asks for it.
#[cfg(not(unix))]
fn spawn_detached(exe: &std::path::Path, root: &std::path::Path, _appimage: bool) {
    // Only reachable for self-updating installs, which exist on Linux only.
    let _ = std::process::Command::new(exe).arg(root).spawn();
}

#[cfg(unix)]
fn spawn_detached(exe: &std::path::Path, root: &std::path::Path, appimage: bool) {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg("sleep 0.6; exec \"$0\" \"$@\"").arg(exe).arg(root);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if appimage {
        // The old mount's AppRun exports these; the new AppImage sets its own.
        for k in ["APPIMAGE", "APPDIR", "ARGV0", "OWD", "LUMEN_ASSETS"] {
            cmd.env_remove(k);
        }
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use winit::window::CursorIcon;

    #[test]
    fn card_buttons_get_a_hand_and_the_rest_of_the_card_an_arrow() {
        let _g = lock();
        let dir = tmp("upd-cursor");
        let mut app = test_app(&dir);
        app.upd.card = Some(Card::UpToDate);
        // Pretend the card was just drawn: a card with one button.
        app.upd.card_rect = Rect::new(100, 100, 300, 120);
        let btn = Rect::new(300, 180, 80, 30);
        app.upd.rects = vec![(btn, Btn::Close)];
        app.mouse = (340.0, 195.0);
        assert!(matches!(app.cursor_for(), CursorIcon::Pointer), "over a button");
        app.mouse = (150.0, 130.0);
        assert!(matches!(app.cursor_for(), CursorIcon::Default), "over the card body");
    }
}
