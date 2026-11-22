//! Activity rail: a thin column of icons at the far left that switches what the sidebar shows
//! (Files, Outline), opens the project search and the Git panel, and reaches the bottom panel
//! (Terminal, Debug) and the settings. The same destinations the bottom bar used to carry, in
//! one predictable place.

use crate::{
    app::{App, SidebarMode},
    ui::{Canvas, Rect},
};

/// Width of the rail (it is left of the sidebar and always shown outside zen mode).
pub const RAIL_W: i32 = 44;
/// Side of a button and of its icon.
const BTN: i32 = 32;
const ICON: i32 = 18;
const GAP: i32 = 4;
/// Height of the brand mark at the top of the rail.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RailBtn {
    Files,
    Search,
    Git,
    Ai,
    Outline,
    Docker,
    Terminal,
    Debug,
    Settings,
}

impl RailBtn {
    /// Top group, then bottom group.
    pub const TOP: [RailBtn; 5] = [RailBtn::Files, RailBtn::Search, RailBtn::Git, RailBtn::Ai, RailBtn::Outline];
    pub const BOTTOM: [RailBtn; 4] = [RailBtn::Docker, RailBtn::Terminal, RailBtn::Debug, RailBtn::Settings];

    pub fn icon(self) -> &'static str {
        match self {
            RailBtn::Files => "ui/file_tree",
            RailBtn::Search => "hugeicons/search",
            RailBtn::Git => "ui/git_branch",
            RailBtn::Ai => "ui/sparkle",
            RailBtn::Outline => "ui/list_tree",
            RailBtn::Docker => "ui/box",
            RailBtn::Terminal => "ui/terminal",
            RailBtn::Debug => "ui/debug",
            RailBtn::Settings => "ui/settings",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            RailBtn::Files => "Explorer",
            RailBtn::Search => "Search in project",
            RailBtn::Git => "Source control",
            RailBtn::Ai => "AI",
            RailBtn::Outline => "Outline",
            RailBtn::Docker => "Docker",
            RailBtn::Terminal => "Terminal",
            RailBtn::Debug => "Debug",
            RailBtn::Settings => "Settings",
        }
    }

    /// Id of the command whose shortcut the tooltip shows.
    pub fn command(self) -> Option<&'static str> {
        match self {
            RailBtn::Files => Some("ToggleSidebar"),
            RailBtn::Search => Some("ProjectSearch"),
            RailBtn::Git => Some("GitPanel"),
            RailBtn::Terminal => Some("OpenTerminal"),
            _ => None,
        }
    }
}

impl App {
    /// Width the rail takes (none in zen mode).
    pub fn rail_w(&self) -> i32 {
        if self.zen.active() { 0 } else { RAIL_W }
    }

    fn rail_active(&self, b: RailBtn) -> bool {
        use crate::app_panel::PanelTab;
        match b {
            RailBtn::Files => self.sidebar_open && self.sidebar_mode == SidebarMode::Files,
            RailBtn::Outline => self.sidebar_open && self.sidebar_mode == SidebarMode::Outline,
            RailBtn::Git => self.gitpanel.is_some(),
            RailBtn::Ai => self.ai.open,
            RailBtn::Docker => self.panel.open && self.panel.tab == PanelTab::Docker,
            RailBtn::Terminal => self.panel.open && self.panel.tab == PanelTab::Terminal,
            RailBtn::Debug => self.panel.open && self.panel.tab == PanelTab::Debug,
            RailBtn::Search | RailBtn::Settings => false,
        }
    }

    pub fn draw_rail(&mut self, cv: &mut Canvas) {
        let r = self.layout.rail;
        self.layout.rail_btns.clear();
        if r.w <= 0 {
            return;
        }
        let c = self.theme.chrome;
        cv.fill(r, c.sidebar_bg);
        cv.fill(Rect::new(r.right() - 1, r.y, 1, r.h), c.border);
        let x = r.x + (r.w - 1 - BTN) / 2;
        let mut y = r.y + 10;
        for b in RailBtn::TOP {
            self.rail_button(cv, Rect::new(x, y, BTN, BTN), b);
            y += BTN + GAP;
        }
        let mut y = r.bottom() - 8 - BTN;
        for b in RailBtn::BOTTOM.iter().rev() {
            self.rail_button(cv, Rect::new(x, y, BTN, BTN), *b);
            y -= BTN + GAP;
        }
    }

    fn rail_button(&mut self, cv: &mut Canvas, b: Rect, kind: RailBtn) {
        let c = self.theme.chrome;
        let on = self.rail_active(kind);
        let hot = b.contains(self.mouse.0, self.mouse.1);
        if on {
            cv.fill_round(b, 8, c.list_active_bg);
            cv.fill_round(Rect::new(b.x - 6, b.y + 8, 2, b.h - 16), 1, c.accent_text);
        } else if hot {
            cv.fill_round(b, 8, c.list_hover_bg);
        }
        let tint = if on {
            c.accent_text
        } else if hot {
            c.tab_active_fg
        } else {
            c.tab_inactive_fg
        };
        let clip = self.layout.rail;
        self.tx.icon(cv, kind.icon(), b.x + (b.w - ICON) / 2, b.y + (b.h - ICON) / 2, ICON, tint, clip);
        self.layout.rail_btns.push((b, kind));
    }

    /// Mouse press on the rail; true when it was consumed.
    pub fn rail_click(&mut self, x: f64, y: f64) -> bool {
        if !self.layout.rail.contains(x, y) {
            return false;
        }
        let Some((_, kind)) = self.layout.rail_btns.iter().find(|(r, _)| r.contains(x, y)).copied() else {
            return true;
        };
        use crate::app_panel::PanelTab;
        match kind {
            RailBtn::Files | RailBtn::Outline => {
                let mode = if kind == RailBtn::Files { SidebarMode::Files } else { SidebarMode::Outline };
                if self.sidebar_open && self.sidebar_mode == mode {
                    self.sidebar_open = false;
                    self.focus = crate::app::Focus::Editor;
                } else {
                    self.sidebar_mode = mode;
                    self.sidebar_open = true;
                    self.tree.scroll = 0.0;
                }
            }
            RailBtn::Search => self.open_project_search(),
            RailBtn::Git => self.git_button(),
            RailBtn::Ai => {
                if self.ai.open {
                    self.ai_hide();
                    self.focus = crate::app::Focus::Editor;
                } else {
                    self.ai_show();
                }
            }
            RailBtn::Docker => {
                if self.rail_active(kind) {
                    self.toggle_panel();
                } else {
                    self.docker_open_panel();
                }
            }
            RailBtn::Terminal => {
                if self.rail_active(kind) {
                    self.toggle_panel();
                } else {
                    self.open_terminal();
                }
            }
            RailBtn::Debug => {
                if self.rail_active(kind) {
                    self.toggle_panel();
                } else {
                    self.panel.open = true;
                    self.panel.tab = PanelTab::Debug;
                }
            }
            RailBtn::Settings => self.open_settings(crate::settingsui::Page::Settings),
        }
        true
    }
}
