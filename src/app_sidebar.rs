//! Left sidebar chrome: its resizable width (drag the right edge like the Git panel's left edge; a
//! double click on the edge goes back to the default) and the room a row's name has before the
//! git-status dot, so names that do not fit end in "…" (horizontal scroll reveals the rest).

use crate::{
    app::{App, SIDEBAR_W},
    ui::{Canvas, Rect},
};
use std::time::{Duration, Instant};

/// Narrowest the sidebar may be dragged to (less in tiny windows).
pub const SIDEBAR_MIN_W: i32 = 160;
/// Half-width of the grab band around the sidebar's right edge.
const GRAB: i32 = 4;
/// Two presses on the edge this close together reset the width.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Editor width a wide sidebar never takes away next to the Git panel.
const MIN_EDITOR_W: i32 = 240;
/// The git-status dot sits this far left of the sidebar's right edge (see `draw_sidebar`), and a cut
/// name stops `NAME_GAP` px before it.
const DOT_X: i32 = 24;
const NAME_GAP: i32 = 6;

/// Sidebar width for a requested `want` px inside a window `win` px wide: at least `SIDEBAR_MIN_W`
/// (less in tiny windows) and at most half the window.
pub fn clamp_sidebar_width(want: i32, win: i32) -> i32 {
    let hi = (win / 2).max(1);
    want.clamp(SIDEBAR_MIN_W.min(hi), hi)
}

/// Rightmost x a row's name may reach: left of the git-status dot in the file tree, left of the
/// scrollbar lane in the outline.
pub fn name_limit(side: Rect, outline: bool) -> i32 {
    side.right() - if outline { 16 } else { DOT_X + NAME_GAP }
}

/// Px a name drawn at `text_x` has before `name_limit`.
pub fn name_room(side: Rect, text_x: i32, outline: bool) -> i32 {
    name_limit(side, outline) - text_x
}

/// Resize state (lives in `InputState`).
#[derive(Default)]
pub struct SidebarEdge {
    /// Dragging the edge: pointer offset from the edge at the press.
    pub grab: Option<f64>,
    /// Pointer over the edge (for the highlight).
    pub hot: bool,
    /// Last press on the edge (double click detection).
    pub last_press: Option<Instant>,
}

impl App {
    /// Width of the open sidebar (narrower when an open Git panel would squeeze the editor).
    pub fn sidebar_w(&self) -> i32 {
        let win = self.size.0 as i32;
        let w = clamp_sidebar_width(self.config.sidebar_width, win);
        let room = win - self.gitpanel_w() - MIN_EDITOR_W;
        w.min(room.max(SIDEBAR_MIN_W.min(win / 2)))
    }

    /// The grab band around the sidebar's right edge (None when it is closed).
    fn sidebar_edge(&self) -> Option<Rect> {
        let r = self.layout.sidebar;
        (self.sidebar_open && r.w > 0).then(|| Rect::new(r.right() - GRAB, r.y, 2 * GRAB, r.h))
    }

    pub fn sidebar_on_edge(&self, x: f64, y: f64) -> bool {
        self.sidebar_edge().is_some_and(|e| e.contains(x, y))
    }

    pub fn sidebar_resizing(&self) -> bool {
        self.inp.sb_edge.grab.is_some()
    }

    /// Press: on the edge it starts a resize, or resets the width on a double click. True when the
    /// press was on the edge.
    pub fn sidebar_edge_press(&mut self, x: f64, y: f64) -> bool {
        if !self.sidebar_on_edge(x, y) {
            return false;
        }
        let now = Instant::now();
        if self.inp.sb_edge.last_press.take().is_some_and(|t| now.duration_since(t) < DOUBLE_CLICK) {
            if self.config.sidebar_width != SIDEBAR_W {
                self.config.sidebar_width = SIDEBAR_W;
                self.config.save();
            }
            return true;
        }
        self.inp.sb_edge.last_press = Some(now);
        self.inp.sb_edge.grab = Some(x - self.layout.sidebar.right() as f64);
        true
    }

    /// Pointer moved: follows an edge drag (true = consumed) and tracks the hover highlight.
    pub fn sidebar_edge_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        if let Some(grab) = self.inp.sb_edge.grab {
            let want = (x - grab).round() as i32 - self.layout.sidebar.x;
            let w = clamp_sidebar_width(want, self.size.0 as i32);
            if w != self.config.sidebar_width {
                self.config.sidebar_width = w;
                self.request_redraw();
            }
            return true;
        }
        let hot = self.overlay.is_none() && self.sidebar_on_edge(x, y);
        if hot != self.inp.sb_edge.hot {
            self.inp.sb_edge.hot = hot;
            self.request_redraw();
        }
        false
    }

    /// Button released: ends an edge drag and stores the width in `config.json`.
    pub fn sidebar_edge_release(&mut self) {
        if self.inp.sb_edge.grab.take().is_some() {
            self.config.save();
            self.request_redraw();
        }
    }

    /// The edge lights up in the accent color under the pointer and while dragging (as the Git
    /// panel's edge does).
    pub fn draw_sidebar_edge(&mut self, cv: &mut Canvas) {
        let Some(e) = self.sidebar_edge() else { return };
        let dragging = self.sidebar_resizing();
        let hot = self.overlay.is_none() && e.contains(self.mouse.0, self.mouse.1);
        if dragging || hot {
            let r = self.layout.sidebar;
            let a = if dragging { 0xc0 } else { 0x70 };
            cv.fill(Rect::new(r.right() - 2, r.y, 3, r.h), self.theme.chrome.accent.with_alpha(a));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_clamped_to_the_window() {
        assert_eq!(clamp_sidebar_width(256, 1600), 256);
        assert_eq!(clamp_sidebar_width(20, 1600), SIDEBAR_MIN_W, "never narrower than the minimum");
        assert_eq!(clamp_sidebar_width(1200, 1600), 800, "at most half the window");
        assert_eq!(clamp_sidebar_width(256, 200), 100, "tiny window: half of it");
        assert_eq!(clamp_sidebar_width(10, 0), 1);
    }

    #[test]
    fn names_stop_before_the_git_dot() {
        let side = Rect::new(0, 40, 256, 600);
        assert_eq!(name_limit(side, false), 256 - DOT_X - NAME_GAP);
        assert!(name_limit(side, true) > name_limit(side, false), "the outline has no dot");
        assert_eq!(name_room(side, 100, false), 256 - 30 - 100);
        // Scrolling right moves the text left, which gives the name more room.
        assert!(name_room(side, 60, false) > name_room(side, 100, false));
    }
}
