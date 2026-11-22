//! The pointer shape of the whole IDE, in one place.
//!
//! Every frame, `draw_all` ends with [`App::cursor_collect`], which fills a registry of
//! `(rect, icon, z, label)` entries from what the frame drew (title-bar buttons, tabs, status
//! chips, dividers, open menus ...). `cursor_for` (in `app/mouse.rs`) asks the registry first and
//! takes the topmost region under the pointer; what is not registered yet still goes through the
//! old per-module cascade, so nothing regresses while surfaces migrate one by one.
//!
//! Modals shadow everything below them: while a menu, picker, prompt or the settings is open
//! (or the onboarding tour), only regions at [`Z_MODAL`] and above count, and the cascade then
//! answers Default for the dimmed backdrop, never the hand of the covered UI.
//!
//! Debug aid: `LUMEN_CURSOR_DEBUG=1` appends `cursor=<Icon> region=<label>` to the window title
//! while the pointer moves (the label is a registry label, or `cascade:<branch>`).

use crate::{app::App, app_debug::DebugHit, app_overlay::Overlay, app_panel::PanelTab, ui::Rect};
use winit::window::CursorIcon;

/// Regular chrome: buttons, tabs, chips.
pub const Z_BASE: u8 = 10;
/// Resize edges: win over the chrome they sit on.
pub const Z_EDGE: u8 = 20;
/// Popups floating over the editor (completion list).
pub const Z_FLOAT: u8 = 50;
/// Modals and menus: nothing below this counts while one is open.
pub const Z_MODAL: u8 = 100;

/// Grab band added on both sides of the editor split divider.
const DIVIDER_SLOP: i32 = 4;

/// One pointer region of the current frame.
#[derive(Clone, Copy, Debug)]
pub struct CurRegion {
    pub rect: Rect,
    pub icon: CursorIcon,
    pub z: u8,
    pub label: &'static str,
}

/// The regions of the last frame plus the last title written by the debug aid.
#[derive(Default)]
pub struct CursorReg {
    pub list: Vec<CurRegion>,
    dbg: String,
}

impl CursorReg {
    pub fn push(&mut self, rect: Rect, icon: CursorIcon, z: u8, label: &'static str) {
        if rect.w > 0 && rect.h > 0 {
            self.list.push(CurRegion { rect, icon, z, label });
        }
    }

    /// The region under (x, y) with the highest z at or above `min_z`; among equals the one
    /// registered last (drawn last) wins.
    pub fn topmost(&self, x: f64, y: f64, min_z: u8) -> Option<&CurRegion> {
        self.list.iter().filter(|r| r.z >= min_z && r.rect.contains(x, y)).max_by_key(|r| r.z)
    }
}

/// The window title with the debug read-out appended.
pub fn debug_title(base: &str, icon: CursorIcon, label: &str) -> String {
    format!("{base}  cursor={icon:?} region={label}")
}

fn debug_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("LUMEN_CURSOR_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0"))
}

impl App {
    /// Register a region of the current frame at the regular chrome level.
    #[allow(dead_code)] // the entry point for surfaces that migrate off the cascade
    pub fn cur_region(&mut self, rect: Rect, icon: CursorIcon, label: &'static str) {
        self.cur.push(rect, icon, Z_BASE, label);
    }

    /// Same, at an explicit level ([`Z_EDGE`], [`Z_FLOAT`], [`Z_MODAL`]).
    #[allow(dead_code)]
    pub fn cur_region_z(&mut self, rect: Rect, icon: CursorIcon, z: u8, label: &'static str) {
        self.cur.push(rect, icon, z, label);
    }

    /// The lowest level that still counts: a modal hides all the chrome below it.
    pub(crate) fn cursor_floor(&self) -> u8 {
        if !self.overlay.is_none() || self.onboarding.is_some() { Z_MODAL } else { 0 }
    }

    /// Rebuild the registry from what this frame laid out. Called once at the end of `draw_all`.
    pub(crate) fn cursor_collect(&mut self) {
        use CursorIcon::{ColResize, Pointer, Text};
        let mut reg = std::mem::take(&mut self.cur);
        reg.list.clear();
        let l = &self.layout;
        for (r, _) in &l.top_btns {
            reg.push(*r, Pointer, Z_BASE, "top-button");
        }
        for (r, _) in &l.status_btns {
            reg.push(*r, Pointer, Z_BASE, "status-button");
        }
        for (r, _) in &l.rail_btns {
            reg.push(*r, Pointer, Z_BASE, "rail-button");
        }
        reg.push(l.quick, Pointer, Z_BASE, "quick-settings");
        for (r, _) in &l.welcome {
            reg.push(*r, Pointer, Z_BASE, "welcome-row");
        }
        for (tab, close, _) in &l.tabs {
            reg.push(*tab, Pointer, Z_BASE, "tab");
            reg.push(*close, Pointer, Z_BASE + 1, "tab-close");
        }
        for (r, _) in &l.sidebar_tabs {
            reg.push(*r, Pointer, Z_BASE, "sidebar-tab");
        }
        for (r, _) in &l.crumbs {
            reg.push(*r, Pointer, Z_BASE, "breadcrumb");
        }
        for (r, _) in &l.panel_tabs {
            reg.push(*r, Pointer, Z_BASE, "panel-tab");
        }
        if self.panel.open && self.panel.tab == PanelTab::Debug {
            for (r, h) in &l.debug_hits {
                let icon = if matches!(h, DebugHit::ConsoleInput | DebugHit::WatchInput) { Text } else { Pointer };
                reg.push(*r, icon, Z_BASE, "debug-control");
            }
        }
        for (r, _) in &l.completion_rows {
            reg.push(*r, Pointer, Z_FLOAT, "completion-row");
        }
        if l.divider.w > 0 {
            let d = l.divider;
            reg.push(
                Rect::new(d.x - DIVIDER_SLOP, d.y, d.w + 2 * DIVIDER_SLOP, d.h),
                ColResize,
                Z_EDGE,
                "split-divider",
            );
        }
        // Open modals: their own rows. Settings answers per control (see `settings_cursor`).
        match &self.overlay {
            Overlay::TopMenu(m) => {
                for (r, _) in &m.rows {
                    reg.push(*r, Pointer, Z_MODAL, "menu-row");
                }
            }
            Overlay::Picker(_) | Overlay::Prompt(_) | Overlay::Ctx(_) | Overlay::Font(_) => {
                for (r, _) in &l.overlay_rows {
                    reg.push(*r, Pointer, Z_MODAL, "modal-row");
                }
            }
            Overlay::Settings(_) | Overlay::None => {}
        }
        self.dock_cursor_collect(&mut reg);
        self.cur = reg;
        self.cursor_refresh();
    }

    /// `LUMEN_CURSOR_DEBUG=1`: show the pointer shape and its region in the window title.
    pub(crate) fn cursor_debug_title(&mut self, icon: CursorIcon, label: &str) {
        if !debug_on() {
            return;
        }
        let title = debug_title(&self.last_title, icon, label);
        if title == self.cur.dbg {
            return;
        }
        if let Some(g) = &self.gfx {
            g.window.set_title(&title);
        }
        self.cur.dbg = title;
    }
}
