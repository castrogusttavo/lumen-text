//! Welcome screen: what the editor card shows while no file is open. A launchpad with a staggered
//! entrance: the app mark, quick actions with their shortcuts as keycap chips (each row runs the
//! same command the palette would), the recent projects with their path and when they were last
//! touched, and a developer tip. Without recent projects the right column shows a friendly empty
//! state (`ui_empty`).

use crate::{
    anim::{Hot, fade, lerp_rgba, stagger},
    app::App,
    app_gitpanel::fit,
    app_overlay::Cmd,
    ds::{ICON_M, ICON_S, R_CARD, R_CTRL, R_ROW, S1, S2, S3, S4, S5},
    ui::{Canvas, Rect, ui_line},
    ui_empty::{Empty, draw_empty},
    ui_keycap::{draw_keys, keys_width, split_keys},
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// What a click on a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Cmd(Cmd),
    Project(PathBuf),
    NextTip,
}

const ROW_A: i32 = 36;
const ROW_R: i32 = 52;
const HEAD_H: i32 = 48;
const TILE: i32 = 28;
const TIP_H: i32 = 60;
const COL_W: i32 = 360;
const COL_GAP: i32 = 48;
const RECENT_WIDE: usize = 4;
const RECENT_NARROW: usize = 3;
/// Staggered entrance: gap between items and length of each item's fade/rise, in seconds.
const STAG_GAP: f32 = 0.045;
const STAG_LEN: f32 = 0.32;
/// After this the entrance is over and the screen stops asking for frames.
const ENTER_SECS: f32 = 1.2;
const HOTS: usize = 16;

/// (icon, label, command id for the shortcut, command to run).
const ACTIONS: [(&str, &str, &str, Cmd); 6] = [
    ("ui/folder_open", "Open folder", "ProjectMenu", Cmd::ProjectMenu),
    ("ui/file_multiple", "Open file", "QuickOpen", Cmd::QuickOpen),
    ("ui/magnifying_glass", "Search in project", "ProjectSearch", Cmd::ProjectSearch),
    ("ui/terminal", "Command palette", "Commands", Cmd::Commands),
    ("ui/settings", "Settings", "OpenSettings", Cmd::OpenSettings),
    ("ui/code", "Keyboard shortcuts", "OpenKeybindings", Cmd::OpenKeybindings),
];

/// (command id whose shortcut is appended as keycaps, the tip).
const TIPS: [(&str, &str); 8] = [
    ("QuickOpen", "Jump to any file by typing a few letters of its name."),
    ("Commands", "Every feature lives in the command palette."),
    ("ProjectSearch", "Search the whole project, with results as you type."),
    ("GitPanel", "Review, stage and commit from the Git dock."),
    ("OpenKeybindings", "See and remap every shortcut."),
    ("OpenSettings", "Make the editor yours: themes, fonts, keymaps."),
    ("", "The AI dock runs your own CLI agent in a real terminal."),
    ("", "Drop a folder on the window to open it as a project."),
];

/// A recent project and when it was last touched (git activity, else the folder itself).
#[derive(Clone)]
struct Recent {
    path: PathBuf,
    stamp: Option<SystemTime>,
}

/// Animation state of the screen (the UI runs on one thread; the screen is not part of `App`).
struct Wel {
    /// Drawn in the previous frame: false after a file was open, so the entrance replays.
    shown: bool,
    /// Seconds since the screen appeared.
    t: f32,
    tip: usize,
    hots: [Hot; HOTS],
}

thread_local! {
    static WEL: RefCell<Wel> = RefCell::new(Wel { shown: false, t: 0.0, tip: 0, hots: [Hot::default(); HOTS] });
}

/// A document is showing: the next time the welcome screen appears it plays its entrance again.
pub fn mark_hidden() {
    WEL.with(|w| w.borrow_mut().shown = false);
}

/// Advance the entrance and the hover fades; true while anything still moves.
fn tick(dt: f64, hover: Option<usize>) -> bool {
    WEL.with(|w| {
        let mut w = w.borrow_mut();
        if !w.shown {
            w.shown = true;
            w.t = 0.0;
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            w.tip = tip_for(secs);
        }
        w.t += dt as f32;
        let mut busy = w.t < ENTER_SECS;
        for (i, h) in w.hots.iter_mut().enumerate() {
            h.set(hover == Some(i));
            busy |= h.step(dt);
        }
        busy
    })
}

fn tip_for(secs: u64) -> usize {
    (secs / 60) as usize % TIPS.len()
}

/// "just now", "5m ago", "3h ago", "2d ago", "4mo ago", "1y ago".
pub(crate) fn rel_time(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        86_400..=2_591_999 => format!("{}d ago", secs / 86_400),
        2_592_000..=31_535_999 => format!("{}mo ago", secs / 2_592_000),
        _ => format!("{}y ago", secs / 31_536_000),
    }
}

/// Last activity of a project: the git index / HEAD, else the folder.
fn stamp(p: &Path) -> Option<SystemTime> {
    let modified = |f: PathBuf| std::fs::metadata(f).and_then(|m| m.modified()).ok();
    modified(p.join(".git/index")).or_else(|| modified(p.join(".git/HEAD"))).or_else(|| modified(p.to_path_buf()))
}

/// Recent projects, re-read from disk at most every few seconds (the screen draws every frame).
fn recents() -> Vec<Recent> {
    static CACHE: Mutex<Option<(Instant, Vec<Recent>)>> = Mutex::new(None);
    let Ok(mut g) = CACHE.lock() else { return Vec::new() };
    if let Some((t, v)) = g.as_ref()
        && t.elapsed() < Duration::from_secs(5)
    {
        return v.clone();
    }
    let v: Vec<Recent> =
        crate::app_topbar::load_recents().into_iter().map(|path| Recent { stamp: stamp(&path), path }).collect();
    *g = Some((Instant::now(), v.clone()));
    v
}

/// The `n` most recent projects other than `skip` (the one already open).
pub(crate) fn recent_projects(skip: &Path, n: usize) -> Vec<PathBuf> {
    recents().into_iter().map(|r| r.path).filter(|p| p != skip).take(n).collect()
}

/// One column of the launchpad.
struct Col {
    area: Rect,
    x: i32,
    w: i32,
    /// Seconds since the screen appeared.
    el: f32,
}

/// One row of a list.
struct Row<'a> {
    icon: &'a str,
    label: &'a str,
    sub: &'a str,
    keys: Vec<String>,
    right: &'a str,
    h: i32,
    hit: Hit,
}

impl App {
    /// Draw the welcome screen over `area` (the empty editor card) and record its click rects.
    /// True while it animates.
    pub fn draw_welcome(&mut self, cv: &mut Canvas, area: Rect, dt: f64) -> bool {
        let busy = tick(dt, self.welcome_hot);
        let el = WEL.with(|w| w.borrow().t);
        self.layout.welcome.clear();
        cv.fill(area, self.theme.background);
        let wide = area.w >= 2 * COL_W + COL_GAP + 2 * S4 && area.h >= 440;
        let now = SystemTime::now();
        let line = ui_line() as i32;
        let label_h = line + S1 + S1;
        let mut n = 0usize;

        if wide {
            let recent: Vec<Recent> = self.recent_list(RECENT_WIDE);
            let x0 = area.x + (area.w - (2 * COL_W + COL_GAP)) / 2;
            let top = HEAD_H + S5;
            let left = label_h + ACTIONS.len() as i32 * ROW_A;
            let right = label_h + RECENT_WIDE as i32 * ROW_R + S4 + TIP_H;
            let y0 = area.y + ((area.h - top - left.max(right)) / 2).max(S5);
            let lc = Col { area, x: x0, w: COL_W, el };
            let rc = Col { area, x: x0 + COL_W + COL_GAP, w: COL_W, el };
            let y = self.wel_head(cv, &lc, y0, &mut n);
            self.wel_actions(cv, &lc, y, &mut n);
            let y = self.wel_label(cv, &rc, y, "Recent projects", &mut n);
            let y = if recent.is_empty() {
                self.wel_empty(cv, &rc, y, &mut n)
            } else {
                self.wel_recents(cv, &rc, y, &mut n, &recent, now)
            };
            let y = y.max(top + y0 + label_h + RECENT_WIDE as i32 * ROW_R) + S4;
            self.wel_tip(cv, &rc, y, &mut n);
        } else {
            let w = (COL_W + 2 * S5).min(area.w - 2 * S4);
            if w < 160 {
                return busy;
            }
            let col = Col { area, x: area.x + (area.w - w) / 2, w, el };
            let recent = self.recent_list(RECENT_NARROW);
            let rec_h = if recent.is_empty() { 0 } else { label_h + S4 + recent.len() as i32 * ROW_R };
            let body = HEAD_H + S5 + label_h + ACTIONS.len() as i32 * ROW_A + rec_h;
            let with_tip = area.h >= body + S4 + TIP_H + 2 * S5;
            let total = body + if with_tip { S4 + TIP_H } else { 0 };
            let y0 = area.y + ((area.h - total) / 2).max(S4);
            let y = self.wel_head(cv, &col, y0, &mut n);
            let mut y = self.wel_actions(cv, &col, y, &mut n);
            if !recent.is_empty() {
                y = self.wel_label(cv, &col, y + S4, "Recent projects", &mut n);
                y = self.wel_recents(cv, &col, y, &mut n, &recent, now);
            }
            if with_tip {
                self.wel_tip(cv, &col, y + S4, &mut n);
            }
        }
        busy
    }

    fn recent_list(&self, max: usize) -> Vec<Recent> {
        recents().into_iter().filter(|r| r.path != self.root).take(max).collect()
    }

    /// Entrance progress of the next item and its vertical rise in px.
    fn wel_k(col: &Col, n: &mut usize) -> (f32, i32) {
        let k = stagger(col.el, *n, STAG_GAP, STAG_LEN);
        *n += 1;
        (k, ((1.0 - k) * 10.0) as i32)
    }

    /// Mark, name and tagline. Returns the y below it.
    fn wel_head(&mut self, cv: &mut Canvas, col: &Col, y: i32, n: &mut usize) -> i32 {
        let c = self.theme.chrome;
        let (k, dy) = Self::wel_k(col, n);
        let mw = crate::brand::width_for(HEAD_H);
        crate::brand::draw_mark(cv, col.x, y + dy, HEAD_H, col.area);
        if k < 1.0 {
            cv.fill(Rect::new(col.x - 2, y + dy - 2, mw + 4, HEAD_H + 4), fade(self.theme.background, 1.0 - k));
        }
        let tx0 = col.x + mw + S3;
        self.tx.draw_ui_sized(cv, tx0, y + dy - 2, "Lumen", fade(c.tab_active_fg, k), col.area, true, 22.0);
        let tag = fit(&mut self.tx, "A native code editor", col.w - mw - S3, false);
        self.tx.draw_ui(cv, tx0, y + dy + 29, &tag, fade(c.tab_inactive_fg, k), col.area, false);
        y + HEAD_H + S5
    }

    fn wel_label(&mut self, cv: &mut Canvas, col: &Col, y: i32, text: &str, n: &mut usize) -> i32 {
        let c = self.theme.chrome;
        let (k, dy) = Self::wel_k(col, n);
        self.tx.draw_ui(cv, col.x + S3, y + dy, text, fade(c.tab_inactive_fg, k), col.area, true);
        y + ui_line() as i32 + S1 + S1
    }

    /// The quick actions with their shortcuts. Returns the y below them.
    fn wel_actions(&mut self, cv: &mut Canvas, col: &Col, y: i32, n: &mut usize) -> i32 {
        let mut y = self.wel_label(cv, col, y, "Get started", n);
        for (icon, label, id, cmd) in ACTIONS {
            let keys = self.shortcut_text(id).map(|s| split_keys(&s)).unwrap_or_default();
            let row = Row { icon, label, sub: "", keys, right: "", h: ROW_A, hit: Hit::Cmd(cmd) };
            y = self.wel_row(cv, col, y, n, row);
        }
        y
    }

    /// Recent projects with their path and last activity. Returns the y below them.
    fn wel_recents(
        &mut self,
        cv: &mut Canvas,
        col: &Col,
        y: i32,
        n: &mut usize,
        list: &[Recent],
        now: SystemTime,
    ) -> i32 {
        let mut y = y;
        for r in list {
            let name = crate::app_topbar::project_name(&r.path);
            let dir = crate::app_topbar::tilde(&r.path);
            let ago =
                r.stamp.and_then(|t| now.duration_since(t).ok()).map(|d| rel_time(d.as_secs())).unwrap_or_default();
            let row = Row {
                icon: "ui/folder",
                label: &name,
                sub: &dir,
                keys: Vec::new(),
                right: &ago,
                h: ROW_R,
                hit: Hit::Project(r.path.clone()),
            };
            y = self.wel_row(cv, col, y, n, row);
        }
        y
    }

    /// Friendly empty state for the recents column. Returns the y below it.
    fn wel_empty(&mut self, cv: &mut Canvas, col: &Col, y: i32, n: &mut usize) -> i32 {
        let c = self.theme.chrome;
        let (k, dy) = Self::wel_k(col, n);
        let h = RECENT_WIDE as i32 * ROW_R;
        let r = Rect::new(col.x, y + dy, col.w, h);
        cv.ring(r, R_CARD, fade(c.border, k));
        let e = Empty {
            icon: "ui/folder_add",
            title: "No recent projects yet",
            hint: "Projects you open will show up here.",
            action: Some("Open folder"),
        };
        let btn = draw_empty(&mut self.tx, cv, &self.theme, r, &e, self.mouse, k);
        if let Some(b) = btn {
            self.layout.welcome.push((Rect::new(b.x, b.y - dy, b.w, b.h), Hit::Cmd(Cmd::ProjectMenu)));
        }
        y + h
    }

    /// One list row: icon tile, label (and a dim sub line), keycaps or a dim note at the right.
    fn wel_row(&mut self, cv: &mut Canvas, col: &Col, y: i32, n: &mut usize, row: Row) -> i32 {
        let c = self.theme.chrome;
        let (k, dy) = Self::wel_k(col, n);
        let r = Rect::new(col.x, y, col.w, row.h);
        let hot = WEL.with(|w| w.borrow().hots[self.layout.welcome.len().min(HOTS - 1)].v());
        let d = Rect::new(r.x, r.y + dy, r.w, r.h);
        if hot > 0.0 {
            cv.fill_round(d, R_ROW, fade(c.list_hover_bg, hot * k));
        }
        let tile = Rect::new(d.x + S3, d.y + (d.h - TILE) / 2, TILE, TILE);
        cv.fill_round(tile, R_CTRL, fade(c.widget_bg, k));
        cv.ring(tile, R_CTRL, fade(c.border, k));
        let ic = lerp_rgba(c.tab_inactive_fg, c.accent_text, hot);
        self.tx.icon(
            cv,
            row.icon,
            tile.x + (TILE - ICON_M) / 2,
            tile.y + (TILE - ICON_M) / 2,
            ICON_M,
            fade(ic, k),
            col.area,
        );

        // Right side first, so the text knows how much room is left.
        let mut right_w = 0;
        if !row.keys.is_empty() {
            right_w = keys_width(&mut self.tx, &row.keys);
            draw_keys(&mut self.tx, cv, d.right() - S3 - right_w, d.y + (d.h - 18) / 2, &row.keys, c, k);
        } else if !row.right.is_empty() {
            let s = fit(&mut self.tx, row.right, col.w / 4, false);
            right_w = self.tx.ui_width(&s, false);
            self.tx.draw_ui(
                cv,
                d.right() - S3 - right_w,
                d.y + (d.h - ui_line() as i32) / 2,
                &s,
                fade(c.tab_inactive_fg, k),
                col.area,
                false,
            );
        }
        let tx0 = tile.right() + S3;
        let avail = (d.right() - S3 - right_w - S3 - tx0).max(20);
        let line = ui_line() as i32;
        let label = fit(&mut self.tx, row.label, avail, !row.sub.is_empty());
        if row.sub.is_empty() {
            self.tx.draw_ui(cv, tx0, d.y + (d.h - line) / 2, &label, fade(c.tab_active_fg, k), col.area, false);
        } else {
            let top = d.y + (d.h - 2 * line - S1) / 2;
            self.tx.draw_ui(cv, tx0, top, &label, fade(c.tab_active_fg, k), col.area, true);
            let sub = fit(&mut self.tx, row.sub, avail, false);
            self.tx.draw_ui(cv, tx0, top + line + S1, &sub, fade(c.tab_inactive_fg, k), col.area, false);
        }
        self.layout.welcome.push((r, row.hit));
        y + row.h
    }

    /// The rotating developer tip with a "next tip" button. Returns the y below it.
    fn wel_tip(&mut self, cv: &mut Canvas, col: &Col, y: i32, n: &mut usize) -> i32 {
        let c = self.theme.chrome;
        let (k, dy) = Self::wel_k(col, n);
        let (id, text) = TIPS[WEL.with(|w| w.borrow().tip) % TIPS.len()];
        let keys =
            if id.is_empty() { Vec::new() } else { self.shortcut_text(id).map(|s| split_keys(&s)).unwrap_or_default() };
        let r = Rect::new(col.x, y, col.w, TIP_H);
        let d = Rect::new(r.x, r.y + dy, r.w, r.h);
        cv.fill_round(d, R_CARD, fade(c.sidebar_bg, k));
        cv.ring(d, R_CARD, fade(c.border, k));
        self.tx.icon(cv, "ui/sparkle", d.x + S3, d.y + S3, ICON_S, fade(c.accent_text, k), col.area);
        self.tx.draw_ui(cv, d.x + S3 + ICON_S + S2, d.y + S3 - 2, "Tip", fade(c.accent_text, k), col.area, true);

        let btn = Rect::new(r.right() - S2 - 24, r.y + S2, 24, 24);
        let hot = WEL.with(|w| w.borrow().hots[self.layout.welcome.len().min(HOTS - 1)].v());
        let bd = Rect::new(btn.x, btn.y + dy, btn.w, btn.h);
        cv.fill_round(bd, R_CTRL, fade(c.list_hover_bg, hot * k));
        let ic = lerp_rgba(c.tab_inactive_fg, c.tab_active_fg, hot);
        self.tx.icon(
            cv,
            "ui/chevron_right",
            bd.x + (24 - ICON_S) / 2,
            bd.y + (24 - ICON_S) / 2,
            ICON_S,
            fade(ic, k),
            col.area,
        );
        self.layout.welcome.push((btn, Hit::NextTip));

        let kw = if keys.is_empty() { 0 } else { keys_width(&mut self.tx, &keys) + S2 };
        let line = ui_line() as i32;
        let avail = (col.w - 2 * S3 - kw).max(40);
        let t = fit(&mut self.tx, text, avail, false);
        let tw = self.tx.ui_width(&t, false);
        let ty = d.y + TIP_H - S3 - line;
        self.tx.draw_ui(cv, d.x + S3, ty, &t, fade(c.tab_active_fg, k), col.area, false);
        if kw > 0 {
            draw_keys(&mut self.tx, cv, d.x + S3 + tw + S2, ty + (line - 18) / 2, &keys, c, k);
        }
        y + TIP_H
    }

    /// Mouse press on the welcome screen; true when a row took it.
    pub fn welcome_click(&mut self, x: f64, y: f64) -> bool {
        let Some((_, hit)) = self.layout.welcome.iter().find(|(r, _)| r.contains(x, y)).cloned() else {
            return false;
        };
        match hit {
            Hit::Cmd(c) => self.run_cmd(c),
            Hit::Project(p) => self.switch_project(p),
            Hit::NextTip => {
                WEL.with(|w| {
                    let mut w = w.borrow_mut();
                    w.tip = (w.tip + 1) % TIPS.len();
                });
                self.request_redraw();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_pick_the_largest_unit() {
        assert_eq!(rel_time(5), "just now");
        assert_eq!(rel_time(60), "1m ago");
        assert_eq!(rel_time(3 * 3600), "3h ago");
        assert_eq!(rel_time(2 * 86_400), "2d ago");
        assert_eq!(rel_time(40 * 86_400), "1mo ago");
        assert_eq!(rel_time(800 * 86_400), "2y ago");
    }

    #[test]
    fn tips_rotate_over_the_whole_list() {
        let seen: std::collections::HashSet<usize> = (0..TIPS.len() as u64).map(|m| tip_for(m * 60)).collect();
        assert_eq!(seen.len(), TIPS.len());
    }

    #[test]
    fn entrance_ends_and_hover_fades_back() {
        mark_hidden();
        assert!(tick(0.016, None));
        assert!(tick(0.016, Some(2)));
        for _ in 0..200 {
            tick(0.016, None);
        }
        assert!(!tick(0.016, None), "idle once the entrance is over");
        mark_hidden();
        assert!(tick(0.016, None), "entrance replays after a file was open");
    }

    #[test]
    fn actions_are_commands_with_icons() {
        assert!(ACTIONS.iter().all(|a| a.0.starts_with("ui/") && !a.1.is_empty()));
    }
}
