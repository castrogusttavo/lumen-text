//! Docker panel (bottom panel tab): containers and images from the `docker` CLI, with Start / Stop /
//! Restart / Remove and Logs on each row. Every docker call runs on a worker thread and answers
//! over a channel drained from the tick, so the UI never waits on the daemon. Logs go to the
//! Output panel (`run_task`). Parsing and the CLI calls are in `docker`.

use crate::{
    app::App,
    app_gitpanel::fit,
    app_panel::PanelTab,
    docker::{self, Container, Fail, Image, Snapshot},
    ui::{Canvas, Rect},
};
use std::{
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};
use winit::event::MouseScrollDelta;

const MIN_PANEL_H: i32 = 220;
const TOOL_H: i32 = 40;
const HEAD_H: i32 = 28;
const ROW_H: i32 = 30;
const BTN_H: i32 = 26;
const ICO: i32 = 24;
/// How often the open panel refreshes itself, and how often while docker is missing or down.
const POLL: Duration = Duration::from_secs(5);
const POLL_DOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verb {
    Start,
    Stop,
    Restart,
    Rm,
    Rmi,
}

impl Verb {
    fn args(self) -> &'static [&'static str] {
        match self {
            Verb::Start => &["start"],
            Verb::Stop => &["stop"],
            Verb::Restart => &["restart"],
            Verb::Rm => &["rm"],
            Verb::Rmi => &["rmi"],
        }
    }

    fn done(self) -> &'static str {
        match self {
            Verb::Start => "started",
            Verb::Stop => "stopped",
            Verb::Restart => "restarted",
            Verb::Rm | Verb::Rmi => "removed",
        }
    }
}

#[derive(Clone, Debug)]
enum Hit {
    Refresh,
    Act(Verb, String, String),
    Logs(String, String),
}

enum Msg {
    Snap(Result<Snapshot, Fail>),
    Done(String, Result<(), String>),
}

pub struct DockerState {
    containers: Vec<Container>,
    images: Vec<Image>,
    loaded: bool,
    fail: Option<Fail>,
    loading: bool,
    /// An action finished while a refresh was running: refresh again when it ends.
    again: bool,
    busy: usize,
    last: Option<Instant>,
    scroll: f64,
    max_scroll: f64,
    hits: Vec<(Rect, Hit)>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Default for DockerState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            containers: Vec::new(),
            images: Vec::new(),
            loaded: false,
            fail: None,
            loading: false,
            again: false,
            busy: 0,
            last: None,
            scroll: 0.0,
            max_scroll: 0.0,
            hits: Vec::new(),
            tx,
            rx,
        }
    }
}

fn dot_color(c: &crate::theme::Chrome, state: &str) -> crate::theme::Rgba {
    match state {
        "running" => c.git_added,
        "paused" | "restarting" => c.warning,
        "dead" => c.error,
        _ => c.tab_inactive_fg.with_alpha(0x90),
    }
}

impl App {
    /// The "Docker: Show panel" command and the rail button: open the tab and refresh.
    pub fn docker_open_panel(&mut self) {
        self.panel.open = true;
        self.panel.tab = PanelTab::Docker;
        self.focus = crate::app::Focus::Editor;
        if self.panel.height < MIN_PANEL_H {
            self.panel.height = MIN_PANEL_H;
        }
        self.docker_refresh();
    }

    fn docker_visible(&self) -> bool {
        self.panel.open && self.panel.tab == PanelTab::Docker
    }

    /// Ask a worker for the containers and images (one refresh at a time).
    pub fn docker_refresh(&mut self) {
        if self.docker.loading {
            return;
        }
        self.docker.loading = true;
        self.docker.last = Some(Instant::now());
        let (tx, wake) = (self.docker.tx.clone(), self.wake());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Snap(docker::snapshot()));
            wake();
        });
    }

    fn docker_act(&mut self, verb: Verb, id: String, name: String) {
        self.docker.busy += 1;
        let (tx, wake) = (self.docker.tx.clone(), self.wake());
        std::thread::spawn(move || {
            let mut args: Vec<&str> = verb.args().to_vec();
            args.push(&id);
            let res = match docker::run(&args) {
                Ok(_) => Ok(()),
                Err(Fail::Missing) => Err("docker not found".to_string()),
                Err(Fail::Failed(e)) => Err(e),
            };
            let _ = tx.send(Msg::Done(format!("docker: {name} {}", verb.done()), res));
            wake();
        });
    }

    /// The last 500 log lines in the Output panel.
    fn docker_logs(&mut self, id: &str, name: &str) {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return self.say("docker: unexpected container id");
        }
        self.run_task(&format!("docker logs --tail 500 {id}"), &format!("Docker logs: {name}"));
        self.panel.tab = PanelTab::Output;
    }

    fn docker_pump(&mut self) {
        let mut changed = false;
        while let Ok(m) = self.docker.rx.try_recv() {
            changed = true;
            match m {
                Msg::Snap(Ok(s)) => {
                    self.docker.containers = s.containers;
                    self.docker.images = s.images;
                    self.docker.fail = None;
                    self.docker.loaded = true;
                    self.docker.loading = false;
                }
                Msg::Snap(Err(e)) => {
                    self.docker.containers.clear();
                    self.docker.images.clear();
                    self.docker.fail = Some(e);
                    self.docker.loaded = true;
                    self.docker.loading = false;
                }
                Msg::Done(msg, res) => {
                    self.docker.busy = self.docker.busy.saturating_sub(1);
                    match res {
                        Ok(()) => self.say(msg),
                        Err(e) => self.say(format!("docker: {e}")),
                    }
                    self.docker.again = true;
                }
            }
        }
        if self.docker.again && !self.docker.loading {
            self.docker.again = false;
            self.docker_refresh();
        }
        if changed {
            self.request_redraw();
        }
    }

    /// Timer: take in worker answers and, while the tab shows, poll the daemon.
    pub fn docker_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        self.docker_pump();
        if !self.docker_visible() || self.docker.loading {
            return;
        }
        let every = if self.docker.fail.is_some() { POLL_DOWN } else { POLL };
        let due = self.docker.last.map_or(now, |t| t + every);
        if due <= now {
            self.docker_refresh();
        } else {
            want(due);
        }
    }

    pub fn docker_wheel(&mut self, delta: MouseScrollDelta) {
        let w = self.wheel_in(delta);
        let px = if w.pixels { w.dy } else { w.dy * ROW_H as f64 * 2.0 };
        self.docker.scroll = (self.docker.scroll + px).clamp(0.0, self.docker.max_scroll);
    }

    pub fn docker_click(&mut self, x: f64, y: f64) {
        let hit = self.docker.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
        match hit {
            Some(Hit::Refresh) => self.docker_refresh(),
            Some(Hit::Act(v, id, name)) => self.docker_act(v, id, name),
            Some(Hit::Logs(id, name)) => self.docker_logs(&id, &name),
            None => {}
        }
    }

    // ───────────── drawing ─────────────

    fn d_icon_btn(&mut self, cv: &mut Canvas, r: Rect, icon: &str, danger: bool, hit: Hit, clip: Rect) {
        let c = self.theme.chrome;
        let hot = self.overlay.is_none() && r.contains(self.mouse.0, self.mouse.1);
        let tint = if hot {
            cv.fill_round_clip(r, 6, c.tab_active_fg.with_alpha(0x18), clip);
            if danger { c.error } else { c.tab_active_fg }
        } else {
            c.tab_inactive_fg
        };
        self.tx.icon(cv, icon, r.x + (r.w - 16) / 2, r.y + (r.h - 16) / 2, 16, tint, clip);
        self.docker.hits.push((r, hit));
    }

    fn d_section(&mut self, cv: &mut Canvas, area: Rect, y: i32, title: &str, n: usize) {
        let dim = self.theme.chrome.tab_inactive_fg;
        let label = format!("{title}  {n}");
        self.tx.draw_ui(cv, area.x + 12, y + (HEAD_H - 18) / 2 + 2, &label, dim, area, true);
    }

    pub fn draw_docker(&mut self, cv: &mut Canvas, body: Rect) {
        self.docker.hits.clear();
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let bar = Rect::new(body.x, body.y, body.w, TOOL_H);
        let area = Rect::new(body.x, body.y + TOOL_H, body.w, (body.h - TOOL_H).max(0));

        // Toolbar: refresh and a one-line summary.
        let label = if self.docker.loading { "Refreshing" } else { "Refresh" };
        let w = 24 + self.tx.ui_width(label, false) + 20;
        let btn = Rect::new(body.x + 12, body.y + (TOOL_H - BTN_H) / 2, w, BTN_H);
        let hot = self.overlay.is_none() && btn.contains(self.mouse.0, self.mouse.1);
        cv.fill_round_clip(btn, 8, strong.with_alpha(if hot { 0x1c } else { 0x0e }), bar);
        self.tx.icon(cv, "ui/rotate_cw", btn.x + 12, btn.y + (BTN_H - 14) / 2, 14, c.accent_text, bar);
        self.tx.draw_ui(cv, btn.x + 32, btn.y + (BTN_H - 18) / 2, label, strong, bar, false);
        self.docker.hits.push((btn, Hit::Refresh));
        if self.docker.fail.is_none() && self.docker.loaded {
            let (nc, ni) = (self.docker.containers.len(), self.docker.images.len());
            let running = self.docker.containers.iter().filter(|c| c.state == "running").count();
            let mut s = format!("{nc} containers ({running} running) · {ni} images");
            if self.docker.busy > 0 {
                s.push_str(" · working…");
            }
            self.tx.draw_ui(cv, btn.right() + 14, btn.y + (BTN_H - 18) / 2, &s, dim, bar, false);
        }
        cv.fill(Rect::new(body.x, body.y + TOOL_H - 1, body.w, 1), c.border);

        // Calm empty states.
        if !self.docker.loaded {
            return self.b_empty_state(cv, area, "ui/box", dim, "Loading Docker…", &[]);
        }
        match self.docker.fail.clone() {
            Some(Fail::Missing) => {
                let lines = [("Install Docker and make sure the docker command is on your PATH.".to_string(), dim)];
                return self.b_empty_state(cv, area, "ui/box", dim, "Docker not found", &lines);
            }
            Some(Fail::Failed(e)) => {
                let lines = [(e, dim), ("Start Docker, then press Refresh.".to_string(), dim)];
                return self.b_empty_state(cv, area, "ui/box", dim, "Docker is not running", &lines);
            }
            None => {}
        }
        if self.docker.containers.is_empty() && self.docker.images.is_empty() {
            let lines = [("Containers and images show up here once you create them.".to_string(), dim)];
            return self.b_empty_state(cv, area, "ui/box", dim, "No containers", &lines);
        }

        // Containers, then images, scrolled together.
        let containers = self.docker.containers.clone();
        let images = self.docker.images.clone();
        let top = area.y + 4 - self.docker.scroll as i32;
        let mut y = top;
        self.d_section(cv, area, y, "Containers", containers.len());
        y += HEAD_H;
        if containers.is_empty() {
            self.tx.draw_ui(cv, area.x + 12, y + 6, "No containers", dim, area, false);
            y += ROW_H;
        }
        for ct in &containers {
            if y + ROW_H > area.y && y < area.bottom() {
                self.d_container_row(cv, area, y, ct);
            }
            y += ROW_H;
        }
        y += 6;
        self.d_section(cv, area, y, "Images", images.len());
        y += HEAD_H;
        if images.is_empty() {
            self.tx.draw_ui(cv, area.x + 12, y + 6, "No images", dim, area, false);
            y += ROW_H;
        }
        for im in &images {
            if y + ROW_H > area.y && y < area.bottom() {
                self.d_image_row(cv, area, y, im);
            }
            y += ROW_H;
        }

        let total = (y - top + 8) as f64;
        self.docker.max_scroll = (total - area.h as f64).max(0.0);
        self.docker.scroll = self.docker.scroll.clamp(0.0, self.docker.max_scroll);
        if self.docker.max_scroll > 0.0 {
            let th = ((area.h as f64 * area.h as f64 / total) as i32).max(24);
            let ty = area.y + ((area.h - th) as f64 * self.docker.scroll / self.docker.max_scroll) as i32;
            cv.fill_round_clip(Rect::new(area.right() - 6, ty, 3, th), 1, strong.with_alpha(0x30), area);
        }
    }

    fn d_row_bg(&mut self, cv: &mut Canvas, area: Rect, row: Rect) {
        let hover = self.theme.chrome.hover_overlay;
        if self.overlay.is_none() && row.contains(self.mouse.0, self.mouse.1) {
            cv.fill_round_clip(row, crate::ds::R_CTRL, hover, area);
        }
    }

    fn d_container_row(&mut self, cv: &mut Canvas, area: Rect, y: i32, ct: &Container) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let row = Rect::new(area.x + 4, y, area.w - 8, ROW_H);
        self.d_row_bg(cv, area, row);
        let mut acts: Vec<(&str, bool, Hit)> = Vec::new();
        let act = |v: Verb| Hit::Act(v, ct.id.clone(), ct.name.clone());
        if ct.running() {
            acts.push(("ui/stop", false, act(Verb::Stop)));
            acts.push(("ui/rotate_cw", false, act(Verb::Restart)));
        } else {
            acts.push(("ui/play_filled", false, act(Verb::Start)));
        }
        acts.push(("ui/file_text_outlined", false, Hit::Logs(ct.id.clone(), ct.name.clone())));
        acts.push(("ui/trash", true, act(Verb::Rm)));
        let mut bx = row.right() - 8 - acts.len() as i32 * ICO;
        let text_right = bx - 8;
        for (icon, danger, hit) in acts {
            self.d_icon_btn(cv, Rect::new(bx, y + (ROW_H - ICO) / 2, ICO, ICO), icon, danger, hit, area);
            bx += ICO;
        }
        let x0 = area.x + 12;
        // Status dot; a running container gets a soft halo.
        let dot = dot_color(&c, &ct.state);
        if ct.running() {
            cv.fill_round_clip(Rect::new(x0 - 3, y + (ROW_H - 14) / 2, 14, 14), 7, dot.with_alpha(0x26), area);
        }
        cv.fill_round_clip(Rect::new(x0, y + (ROW_H - 8) / 2, 8, 8), 4, dot, area);
        let ty = y + (ROW_H - 18) / 2;
        let avail = (text_right - (x0 + 18)).max(0);
        let cols = [
            (ct.name.as_str(), if ct.running() { strong } else { dim }, true, avail * 28 / 100),
            (ct.image.as_str(), dim, false, avail * 24 / 100),
            (ct.status.as_str(), dim, false, avail * 22 / 100),
            (ct.ports.as_str(), dim, false, avail - avail * 74 / 100),
        ];
        let mut x = x0 + 18;
        for (s, col, bold, w) in cols {
            let t = fit(&mut self.tx, s, w - 10, bold);
            self.tx.draw_ui(cv, x, ty, &t, col, area, bold);
            x += w;
        }
    }

    fn d_image_row(&mut self, cv: &mut Canvas, area: Rect, y: i32, im: &Image) {
        let c = self.theme.chrome;
        let (strong, dim) = (c.tab_active_fg, c.tab_inactive_fg);
        let row = Rect::new(area.x + 4, y, area.w - 8, ROW_H);
        self.d_row_bg(cv, area, row);
        let bx = row.right() - 8 - ICO;
        let hit = Hit::Act(Verb::Rmi, im.id.clone(), im.label());
        self.d_icon_btn(cv, Rect::new(bx, y + (ROW_H - ICO) / 2, ICO, ICO), "ui/trash", true, hit, area);
        let x0 = area.x + 12 + 18;
        let ty = y + (ROW_H - 18) / 2;
        let avail = (bx - 8 - x0).max(0);
        let label = im.label();
        let cols = [
            (label.as_str(), strong, true, avail * 42 / 100),
            (im.size.as_str(), dim, false, avail * 14 / 100),
            (im.created.as_str(), dim, false, avail * 22 / 100),
            (im.id.as_str(), dim, false, avail - avail * 78 / 100),
        ];
        let mut x = x0;
        for (s, col, bold, w) in cols {
            let t = fit(&mut self.tx, s, w - 10, bold);
            self.tx.draw_ui(cv, x, ty, &t, col, area, bold);
            x += w;
        }
    }
}
