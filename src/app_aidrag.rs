//! Dragging an AI session's tab: along the strip to reorder, or onto the left / right half of the
//! page to show it there, side by side with the other session (at most two). Same gesture as the
//! terminals' tabs (`app_termdrag`): a press arms it, `DRAG_START` px lift the tab, Esc or a release
//! anywhere else puts everything back.

use crate::{
    app::App,
    app_aipanel::{AiHit, TABS_H},
    app_termdrag::Side,
    ui::{Canvas, Rect},
};
use winit::keyboard::{Key, NamedKey};

const DRAG_START: f64 = 5.0;

/// A press on a session tab that may become a drag.
#[derive(Clone, Copy, Debug)]
pub struct AiDrag {
    pub id: u64,
    pub press: (f64, f64),
    pub grab: f64,
    pub active: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Landing {
    /// Into group `g`'s strip, before its `k`-th other tab.
    Reorder(usize, usize),
    /// One group only: the dragged tab becomes a group of its own on this side.
    Side(Side),
    /// Two groups: into the group under the pointer.
    MoveTo(usize),
}

impl App {
    /// Left press on session tab `i` (already shown): arm a drag.
    pub fn ai_drag_press(&mut self, i: usize, x: f64, y: f64) {
        let Some(id) = self.ai.sessions.get(i).map(|s| s.id) else { return };
        let left = self.ai_tab_rect(id).map_or(x, |r| r.x as f64);
        self.ai.drag = Some(AiDrag { id, press: (x, y), grab: x - left, active: false });
    }

    pub fn ai_dragging(&self) -> bool {
        self.ai.drag.is_some_and(|d| d.active)
    }

    fn ai_tab_rect(&self, id: u64) -> Option<Rect> {
        let i = self.ai.index_of(id)?;
        self.ai.hits.iter().find(|(_, h)| *h == AiHit::Session(i)).map(|(r, _)| *r)
    }

    /// Group `g`'s tabs other than `id`, left to right: (session id, rect).
    fn ai_other_tabs(&self, g: usize, id: u64) -> Vec<(u64, Rect)> {
        let Some(group) = self.ai.groups.get(g) else { return Vec::new() };
        self.ai
            .hits
            .iter()
            .filter_map(|(r, h)| match h {
                AiHit::Session(i) => self.ai.sessions.get(*i).map(|s| (s.id, *r)),
                _ => None,
            })
            .filter(|(sid, _)| *sid != id && group.tabs.contains(sid))
            .collect()
    }

    /// Pointer moved with the button down. True when a session tab drag consumed the move.
    pub fn ai_drag_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let Some(d) = self.ai.drag.as_mut() else { return false };
        if !d.active {
            if (x - d.press.0).hypot(y - d.press.1) < DRAG_START {
                return false;
            }
            d.active = true;
            self.dismiss_tooltip();
        }
        self.request_redraw();
        true
    }

    fn ai_landing(&self) -> Option<Landing> {
        let d = self.ai.drag.filter(|d| d.active)?;
        let (x, y) = self.mouse;
        if !self.ai.open {
            return None;
        }
        let (gr, g) = *self.ai.pane_rects.iter().find(|(r, _)| r.contains(x, y))?;
        if y < (gr.y + TABS_H) as f64 {
            let w = self.ai_tab_rect(d.id).map_or(0, |r| r.w);
            let cx = x - d.grab + w as f64 / 2.0;
            let k = self.ai_other_tabs(g, d.id).iter().filter(|(_, r)| (r.x as f64 + r.w as f64 / 2.0) < cx).count();
            return Some(Landing::Reorder(g, k));
        }
        if self.ai.groups.len() > 1 {
            return Some(Landing::MoveTo(g));
        }
        let mid = (gr.x + gr.w / 2) as f64;
        Some(Landing::Side(if x < mid { Side::Left } else { Side::Right }))
    }

    /// Button released: drop the dragged tab. True when a drag (not a plain click) ended.
    pub fn ai_drag_release(&mut self) -> bool {
        let landing = self.ai_landing();
        let Some(d) = self.ai.drag.take() else { return false };
        if !d.active {
            return false;
        }
        let from = self.ai.group_of(d.id);
        let alone = from.is_some_and(|g| self.ai.groups[g].tabs.len() == 1);
        match landing {
            Some(Landing::Reorder(g, k)) => {
                for gr in &mut self.ai.groups {
                    gr.tabs.retain(|t| *t != d.id);
                }
                if let Some(gr) = self.ai.groups.get_mut(g) {
                    gr.tabs.insert(k.min(gr.tabs.len()), d.id);
                    gr.active = d.id;
                }
            }
            Some(Landing::MoveTo(g)) if from != Some(g) => {
                for gr in &mut self.ai.groups {
                    gr.tabs.retain(|t| *t != d.id);
                }
                if let Some(gr) = self.ai.groups.get_mut(g) {
                    gr.tabs.push(d.id);
                    gr.active = d.id;
                }
            }
            Some(Landing::Side(side)) if !alone => {
                for gr in &mut self.ai.groups {
                    gr.tabs.retain(|t| *t != d.id);
                }
                let group = crate::app_aipanel::Group { tabs: vec![d.id], active: d.id };
                if side == Side::Left {
                    self.ai.groups.insert(0, group);
                } else {
                    self.ai.groups.push(group);
                }
            }
            _ => {}
        }
        if let Some(i) = self.ai.index_of(d.id) {
            self.ai.active = i;
        }
        self.ai.sync_groups();
        self.ai.focused = true;
        self.request_redraw();
        true
    }

    /// Esc while dragging puts the tab back. True when the key was used.
    pub fn ai_drag_key(&mut self, key: &Key) -> bool {
        if self.ai_dragging() && matches!(key, Key::Named(NamedKey::Escape)) {
            self.ai.drag = None;
            self.request_redraw();
            return true;
        }
        false
    }

    /// Drop marker, tinted area and floating tab, drawn on top of the page.
    pub fn draw_ai_drag(&mut self, cv: &mut Canvas) {
        let Some(d) = self.ai.drag.filter(|d| d.active) else { return };
        let Some(i) = self.ai.index_of(d.id) else { return };
        let (name, icon) = (self.ai.sessions[i].name.clone(), self.ai.sessions[i].agent.icon);
        let c = self.theme.chrome;
        let p = self.aipanel_rect();
        let alone = self.ai.group_of(d.id).is_some_and(|g| self.ai.groups[g].tabs.len() == 1);
        match self.ai_landing() {
            Some(Landing::Reorder(g, k)) => {
                let others = self.ai_other_tabs(g, d.id);
                let strip = self.ai.pane_rects.get(g).map(|(r, _)| Rect::new(r.x, r.y, r.w, TABS_H));
                let mx = match (others.get(k), k.checked_sub(1).and_then(|j| others.get(j))) {
                    (Some((_, n)), _) => Some(n.x - 2),
                    (None, Some((_, pr))) => Some(pr.right() + 1),
                    _ => None,
                };
                if let (Some(mx), Some(strip)) = (mx, strip) {
                    cv.fill_round_clip(Rect::new(mx, strip.y + 6, 3, strip.h - 12), 1, c.accent, strip);
                }
            }
            Some(Landing::Side(side)) if !alone => {
                let b = Rect::new(p.x, p.y + TABS_H, p.w, (p.h - TABS_H).max(0));
                let half = b.w / 2;
                let z = if side == Side::Left {
                    Rect::new(b.x + 6, b.y + 6, half - 8, b.h - 12)
                } else {
                    Rect::new(b.x + half + 2, b.y + 6, b.w - half - 8, b.h - 12)
                };
                self.ai_drop_zone(cv, z, if side == Side::Left { "Show on the left" } else { "Show on the right" });
            }
            Some(Landing::MoveTo(g)) if self.ai.group_of(d.id) != Some(g) => {
                if let Some((gr, _)) = self.ai.pane_rects.get(g).copied() {
                    let z = Rect::new(gr.x + 6, gr.y + TABS_H + 6, gr.w - 12, (gr.h - TABS_H - 12).max(0));
                    self.ai_drop_zone(cv, z, "Move here");
                }
            }
            _ => {}
        }
        // Floating tab, lifted with a soft shadow.
        let w = (self.tx.ui_width(&name, true) + 52).min(240);
        let fx = ((self.mouse.0 - d.grab).round() as i32).clamp(p.x, (p.right() - w).max(p.x));
        let fr = Rect::new(fx, p.y + 4, w, 28);
        for k in 1..=4 {
            let g = Rect::new(fr.x - k, fr.y - k + 3, fr.w + 2 * k, fr.h + 2 * k);
            cv.fill_round(g, 8 + k, crate::theme::Rgba(0, 0, 0, 10));
        }
        cv.card(fr, 8, c.tab_active_bg, c.border);
        let fg = c.tab_active_fg;
        self.tx.icon(cv, icon, fr.x + 10, fr.y + 7, 14, fg, fr);
        let text = crate::ui::fit_ellipsis(&mut self.tx, &name, w - 40, true);
        self.tx.draw_ui(cv, fr.x + 30, fr.y + 5, &text, fg, fr, true);
    }

    fn ai_drop_zone(&mut self, cv: &mut Canvas, z: Rect, label: &str) {
        let c = self.theme.chrome;
        cv.card(z, 10, c.accent.with_alpha(0x26), c.accent.with_alpha(0xa0));
        let lw = self.tx.ui_width(label, true);
        self.tx.draw_ui(cv, z.x + (z.w - lw) / 2, z.y + z.h / 2 - 9, label, c.tab_active_fg, z, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_needs_a_few_pixels_before_it_lifts() {
        assert!(DRAG_START > 0.0);
    }
}
