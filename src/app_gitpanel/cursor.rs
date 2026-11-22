//! Pointer regions of the right-hand dock (the Git | AI switch, the Git panel, the AI panel, its
//! terminal) and of the terminals' links, registered in the cursor registry every frame (see
//! `app_cursor`). What the dock drew is what it answers: a hit list entry is a hand (a beam on the
//! text fields), a control drawn dimmed is "not allowed", the left edge resizes, everything else
//! in the slot is an arrow. Nothing here reads the pointer: the registry is rebuilt per frame and
//! `cursor_refresh` re-asks it once the frame is done, so a layout that moved under a still pointer
//! is picked up at once.

use super::*;
use crate::{
    app_aipanel::AiHit,
    app_cursor::{CursorReg, Z_BASE, Z_EDGE},
};

/// The dock's dead area: below every control, above nothing (the editor never shows through).
const Z_DEAD: u8 = Z_BASE - 2;
/// The terminal body: above the dead area, below the controls.
const Z_BODY: u8 = Z_BASE - 1;
/// An open dropdown of the Git panel covers the rows under it.
const Z_MENU: u8 = Z_BASE + 2;

/// The shape, level and label a Git panel hit answers with.
pub(crate) fn git_hit_icon(h: Hit) -> (CursorIcon, u8, &'static str) {
    match h {
        Hit::Msg | Hit::Filter => (CursorIcon::Text, Z_BASE, "git-input"),
        // The padding of the open dropdown only swallows the click.
        Hit::MenuItem(n) if n == usize::MAX => (CursorIcon::Default, Z_MENU, "git-menu-pad"),
        Hit::MenuItem(_) => (CursorIcon::Pointer, Z_MENU, "git-menu-row"),
        Hit::Tab(_) => (CursorIcon::Pointer, Z_BASE, "git-tab"),
        Hit::Row(_) | Hit::Commit(_) | Hit::CFile(_) | Hit::SFile(_) | Hit::Stash(_) => {
            (CursorIcon::Pointer, Z_BASE, "git-row")
        }
        _ => (CursorIcon::Pointer, Z_BASE, "git-button"),
    }
}

impl App {
    /// Register the dock's and the terminals' regions of this frame.
    pub(crate) fn dock_cursor_collect(&self, reg: &mut CursorReg) {
        self.term_cursor_collect(reg);
        if !self.slot_shown() {
            return;
        }
        let slot = self.slot_rect();
        reg.push(slot, CursorIcon::Default, Z_DEAD, "dock-dead");
        reg.push(Rect::new(slot.x - GP_GRAB, slot.y, 2 * GP_GRAB, slot.h), CursorIcon::ColResize, Z_EDGE, "dock-edge");
        // The Git | AI switch and, when the AI panel is shown, its tabs, header and filter.
        for (r, h) in &self.ai.hits {
            let (icon, label) = match h {
                AiHit::Filter => (CursorIcon::Text, "ai-filter"),
                AiHit::TabGit | AiHit::TabAi => (CursorIcon::Pointer, "dock-switch"),
                AiHit::Session(_) | AiHit::CloseSession(_) | AiHit::New => (CursorIcon::Pointer, "ai-session"),
                _ => (CursorIcon::Pointer, "ai-button"),
            };
            reg.push(*r, icon, Z_BASE, label);
        }
        for r in &self.ai.disabled {
            reg.push(*r, CursorIcon::NotAllowed, Z_BASE, "ai-disabled");
        }
        if self.ai.open {
            let p = self.aipanel_rect();
            let top = p.y + self.ai_top_h();
            if self.ai.term().is_some() {
                reg.push(Rect::new(p.x, top, p.w, (p.bottom() - top).max(0)), CursorIcon::Text, Z_BODY, "ai-terminal");
            }
        } else if let Some(gp) = self.gitpanel.as_ref() {
            for (r, h) in &gp.hits {
                let (icon, z, label) = git_hit_icon(*h);
                reg.push(*r, icon, z, label);
            }
            for r in &gp.disabled {
                reg.push(*r, CursorIcon::NotAllowed, Z_BASE, "git-disabled");
            }
        }
    }

    /// Ask the registry again after a frame: the controls may have moved under a still pointer, and
    /// a terminal link only exists in it once the frame that underlined it was drawn.
    pub(crate) fn cursor_refresh(&mut self) {
        let (want, why) = self.cursor_probe();
        self.cursor_debug_title(want, why);
        if want != self.cursor_icon {
            self.cursor_icon = want;
            if let Some(g) = &self.gfx {
                g.window.set_cursor(winit::window::Cursor::Icon(want));
            }
        }
    }
}
