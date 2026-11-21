//! Welcome screen: what the editor card shows while no file is open. The app mark, a short list
//! of actions with their shortcuts (each row is clickable and runs the same command the palette
//! would), and the most recent projects.

use crate::{
    app::App,
    app_gitpanel::fit,
    app_overlay::Cmd,
    ui::{Canvas, Rect},
};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

/// What a click on a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Cmd(Cmd),
    Project(PathBuf),
}

const ROW_H: i32 = 34;
const COL_W: i32 = 420;
const RECENT_SHOWN: usize = 3;
/// Height of the brand mark above the title.
const LOGO_H: i32 = 56;

/// (icon, label, command id for the shortcut, command to run).
const ACTIONS: [(&str, &str, &str, Cmd); 6] = [
    ("ui/folder_open", "Open folder", "ProjectMenu", Cmd::ProjectMenu),
    ("ui/file_multiple", "Open file", "QuickOpen", Cmd::QuickOpen),
    ("ui/magnifying_glass", "Search in project", "ProjectSearch", Cmd::ProjectSearch),
    ("ui/terminal", "Command palette", "Commands", Cmd::Commands),
    ("ui/settings", "Settings", "OpenSettings", Cmd::OpenSettings),
    ("ui/code", "Keyboard shortcuts", "OpenKeybindings", Cmd::OpenKeybindings),
];

/// Recent projects, re-read from disk at most every few seconds (the screen draws every frame).
fn recents() -> Vec<PathBuf> {
    static CACHE: Mutex<Option<(Instant, Vec<PathBuf>)>> = Mutex::new(None);
    let Ok(mut g) = CACHE.lock() else { return Vec::new() };
    if let Some((t, v)) = g.as_ref()
        && t.elapsed() < Duration::from_secs(5)
    {
        return v.clone();
    }
    let v = crate::app_topbar::load_recents();
    *g = Some((Instant::now(), v.clone()));
    v
}

impl App {
    /// Draw the welcome screen over `area` (the empty editor card) and record its click rects.
    pub fn draw_welcome(&mut self, cv: &mut Canvas, area: Rect) {
        self.layout.welcome.clear();
        let c = self.theme.chrome;
        cv.fill(area, self.theme.background);
        let recent: Vec<PathBuf> = recents().into_iter().filter(|p| *p != self.root).take(RECENT_SHOWN).collect();
        let rows = ACTIONS.len() + recent.len();
        let head = LOGO_H + 12 + 24 + 20;
        let sec = if recent.is_empty() { 0 } else { 30 };
        let total = head + 20 + rows as i32 * ROW_H + sec;
        let w = COL_W.min(area.w - 32);
        if w < 160 {
            return;
        }
        let x = area.x + (area.w - w) / 2;
        let mut y = area.y + ((area.h - total) / 2).max(16);

        crate::brand::draw_mark(cv, area.x + (area.w - crate::brand::width_for(LOGO_H)) / 2, y, LOGO_H, area);
        y += LOGO_H + 12;
        let name = "Lumen";
        let nw = self.tx.ui_width(name, true);
        self.tx.draw_ui(cv, area.x + (area.w - nw) / 2, y, name, c.tab_active_fg, area, true);
        y += 24;
        let tag = "A native code editor";
        let tw = self.tx.ui_width(tag, false);
        self.tx.draw_ui(cv, area.x + (area.w - tw) / 2, y, tag, c.tab_inactive_fg, area, false);
        y += 20 + 20;

        for (icon, label, id, cmd) in ACTIONS {
            let r = Rect::new(x, y, w, ROW_H);
            let keys = self.shortcut_text(id).unwrap_or_default();
            self.welcome_row(cv, r, area, icon, label, &keys);
            self.layout.welcome.push((r, Hit::Cmd(cmd)));
            y += ROW_H;
        }
        if !recent.is_empty() {
            y += 10;
            self.tx.draw_ui(cv, x + 12, y, "Recent", c.tab_inactive_fg, area, true);
            y += 20;
            for p in recent {
                let r = Rect::new(x, y, w, ROW_H);
                let name = crate::app_topbar::project_name(&p);
                let dir = crate::app_topbar::tilde(&p);
                self.welcome_row(cv, r, area, "ui/clock", &name, &dir);
                self.layout.welcome.push((r, Hit::Project(p)));
                y += ROW_H;
            }
        }
    }

    /// One row: icon and label at the left, the shortcut (or path) dim at the right.
    fn welcome_row(&mut self, cv: &mut Canvas, r: Rect, clip: Rect, icon: &str, label: &str, right: &str) {
        let c = self.theme.chrome;
        let hot = r.contains(self.mouse.0, self.mouse.1);
        if hot {
            cv.fill_round(r, 8, c.list_hover_bg);
        }
        let fg = if hot { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.icon(cv, icon, r.x + 12, r.y + (r.h - 16) / 2, 16, fg, clip);
        let ty = r.y + (r.h - 18) / 2;
        let rw = if right.is_empty() { 0 } else { (r.w / 2).min(self.tx.ui_width(right, false)) };
        let label = fit(&mut self.tx, label, r.w - 52 - rw - 8, false);
        self.tx.draw_ui(cv, r.x + 40, ty, &label, c.tab_active_fg, clip, false);
        if rw > 0 {
            let s = fit(&mut self.tx, right, rw, false);
            let sw = self.tx.ui_width(&s, false);
            self.tx.draw_ui(cv, r.right() - 12 - sw, ty, &s, c.accent_text, clip, false);
        }
    }

    /// Mouse press on the welcome screen; true when a row took it.
    pub fn welcome_click(&mut self, x: f64, y: f64) -> bool {
        let Some((_, hit)) = self.layout.welcome.iter().find(|(r, _)| r.contains(x, y)).cloned() else {
            return false;
        };
        match hit {
            Hit::Cmd(c) => self.run_cmd(c),
            Hit::Project(p) => self.switch_project(p),
        }
        true
    }
}
