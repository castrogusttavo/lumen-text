//! Input and side effects shared by the two terminals (the bottom panel's shell and the AI panel's
//! agent): typing (xterm key encoding, Ctrl+Shift+C / Ctrl+Shift+V), mouse selection, mouse
//! reporting, the wheel (history, or arrow keys / reports on the alternate screen), OSC 8 links
//! (Ctrl+click), OSC 52 clipboard writes and OSC 9 / 777 notifications.

use crate::{
    app::{App, KeyIn},
    terminal::{MouseEv, Sel, Term},
    termview::{self, Look},
};
use winit::{event::MouseScrollDelta, keyboard::Key};

/// Which terminal an action is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Bottom,
    Ai,
}

impl Which {
    fn idx(self) -> usize {
        match self {
            Which::Bottom => 0,
            Which::Ai => 1,
        }
    }
}

/// A mouse drag that started in a terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drag {
    /// Selecting text.
    Select,
    /// Reporting to a program that asked for the mouse: the button and the last cell sent.
    Report(u8, (usize, usize)),
}

/// Per-terminal pointer state, kept by the App.
#[derive(Default)]
pub struct TermIo {
    /// Top-left of each terminal's grid at the last draw.
    origin: [(i32, i32); 2],
    drag: Option<(Which, Drag)>,
    /// OSC 8 link under the pointer in each terminal (underlined while hovered).
    hover_link: [u16; 2],
}

/// The `http(s)://` URL in `line` that covers column `col` (plain text, for Ctrl+click).
pub fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let text: String = chars.iter().collect();
    for start in text.match_indices("http").map(|(i, _)| text[..i].chars().count()) {
        let rest: String = chars[start..].iter().collect();
        if !rest.starts_with("http://") && !rest.starts_with("https://") {
            continue;
        }
        let len = chars[start..]
            .iter()
            .position(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '│'))
            .unwrap_or(chars.len() - start);
        // Trailing punctuation belongs to the sentence, not the URL.
        let mut end = start + len;
        while end > start && matches!(chars[end - 1], '.' | ',' | ';' | ':' | ')' | ']' | '}' | '!' | '?') {
            end -= 1;
        }
        if col >= start && col < end {
            return Some(chars[start..end].iter().collect());
        }
    }
    None
}

impl App {
    pub fn term_ref(&self, w: Which) -> Option<&Term> {
        match w {
            Which::Bottom => self.term.as_ref(),
            Which::Ai => self.ai.term.as_ref(),
        }
    }

    pub fn term_mut(&mut self, w: Which) -> Option<&mut Term> {
        match w {
            Which::Bottom => self.term.as_mut(),
            Which::Ai => self.ai.term.as_mut(),
        }
    }

    /// Theme colors for drawing terminal `w` (and remembers where its grid starts).
    pub fn term_look(&mut self, w: Which, focused: bool, origin: (i32, i32)) -> Look {
        self.termio.origin[w.idx()] = origin;
        Look {
            fg: self.theme.foreground,
            bg: self.theme.background,
            cursor: self.theme.cursor,
            selection: self.theme.selection,
            focused,
            hover_link: self.termio.hover_link[w.idx()],
        }
    }

    /// The cell of terminal `w` under (x, y), if it is on the grid.
    fn term_cell(&self, w: Which, x: f64, y: f64) -> Option<(usize, usize)> {
        let (ox, oy) = self.termio.origin[w.idx()];
        termview::cell_at(self.term_ref(w)?, self.tx.cell_w, ox, oy, x, y)
    }

    /// Clipboard writes (OSC 52) and notifications (OSC 9 / 777) the program asked for; returns
    /// the notifications so a caller can react to them too.
    pub fn term_effects(&mut self, w: Which) -> Vec<(String, String)> {
        let Some(t) = self.term_mut(w) else { return Vec::new() };
        let clip = t.grid.clipboard.take();
        let notices = std::mem::take(&mut t.grid.notices);
        if let Some(c) = clip {
            self.clip.set(&c);
        }
        if let Some((title, body)) = notices.last() {
            let text = if title.is_empty() { body.clone() } else { format!("{title}: {body}") };
            self.toast(text, true);
        }
        notices
    }

    /// A key for terminal `w` (the caller already kept the editor's global shortcuts). Always consumed.
    pub fn term_type(&mut self, w: Which, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if let Key::Character(c) = &ev.key
            && ctrl
            && shift
            && !alt
        {
            match c.to_lowercase().as_str() {
                // Ctrl+Shift+C copies the selection; Ctrl+Shift+V pastes (bracketed when asked).
                "c" => {
                    let text = self.term_ref(w).map(Term::selected_text).unwrap_or_default();
                    if !text.is_empty() {
                        self.clip.set(&text);
                    }
                    return true;
                }
                "v" => {
                    let text = self.clip.get().replace("\r\n", "\r").replace('\n', "\r");
                    if let Some(t) = self.term_mut(w) {
                        let bytes = t.paste_bytes(&text);
                        t.sel = None;
                        t.write(&bytes);
                    }
                    return true;
                }
                _ => {}
            }
        }
        let named = match &ev.key {
            Key::Named(n) => Some(format!("{n:?}")),
            _ => None,
        };
        let text = match &ev.key {
            Key::Character(c) => Some(c.to_string()),
            _ => ev.text.clone(),
        };
        let Some(t) = self.term_mut(w) else { return true };
        // Shift+PageUp/PageDown scroll the terminal's own history instead of going to the program.
        if shift && !ctrl && matches!(named.as_deref(), Some("PageUp" | "PageDown")) && !t.alt_screen() {
            let rows = t.grid.rows as isize;
            t.scroll_view(if named.as_deref() == Some("PageUp") { rows - 1 } else { -(rows - 1) });
            return true;
        }
        let app_cursor = t.grid.app_cursor;
        if let Some(bytes) =
            crate::terminal::key_bytes_app(named.as_deref(), text.as_deref(), ctrl, shift, alt, app_cursor)
        {
            t.sel = None;
            t.write(&bytes);
        }
        true
    }

    /// Mouse press in terminal `w`: Ctrl+click opens a link, a program that asked for the mouse
    /// gets a report (Shift forces a selection instead), otherwise a selection starts.
    pub fn term_press(&mut self, w: Which, x: f64, y: f64, button: u8) {
        let Some((row, col)) = self.term_cell(w, x, y) else { return };
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if ctrl && button == 0 {
            let link = self.term_ref(w).and_then(|t| {
                t.link_at(row, col).map(str::to_string).or_else(|| {
                    // Plain-text URL: wide characters take two cells but are one char of the line.
                    let cells: Vec<char> =
                        (0..t.grid.cols).map(|c| t.grid.view_cell(row, c, t.scroll_offset).ch).collect();
                    let tail = crate::terminal::WIDE_TAIL;
                    let idx = cells[..col.min(cells.len())].iter().filter(|c| **c != tail).count();
                    let line: String = cells.iter().filter(|c| **c != tail).collect();
                    url_at(&line, idx)
                })
            });
            if let Some(url) = link {
                crate::paths::open_url(&url);
                return;
            }
        }
        if button == 1 && self.term_ref(w).is_some_and(|t| t.grid.mouse_mode == 0) {
            // Middle click: paste the primary selection.
            let text = self.clip.get_primary();
            if let Some(t) = self.term_mut(w) {
                let bytes = t.paste_bytes(&text);
                t.write(&bytes);
            }
            return;
        }
        let Some(t) = self.term_mut(w) else { return };
        if !shift && t.mouse(MouseEv::Press(button), col, row, (shift, alt, ctrl)) {
            t.sel = None;
            self.termio.drag = Some((w, Drag::Report(button, (row, col))));
            return;
        }
        if button == 0 {
            let at = (t.grid.abs_row(row, t.scroll_offset), col);
            t.sel = Some(Sel { anchor: at, head: at });
            self.termio.drag = Some((w, Drag::Select));
        }
    }

    /// Pointer moved: extend a selection / report a drag; true when something changed. Also keeps
    /// the hovered-link underline current.
    pub fn term_drag_move(&mut self) -> bool {
        let (x, y) = self.mouse;
        let mut changed = false;
        for w in [Which::Bottom, Which::Ai] {
            let link = self
                .term_cell(w, x, y)
                .and_then(|(r, c)| self.term_ref(w).map(|t| t.grid.view_cell(r, c, t.scroll_offset).link))
                .unwrap_or(0);
            if self.termio.hover_link[w.idx()] != link {
                self.termio.hover_link[w.idx()] = link;
                changed = true;
            }
        }
        let Some((w, drag)) = self.termio.drag else { return changed };
        let (ox, oy) = self.termio.origin[w.idx()];
        let (cw, lh) = (self.tx.cell_w as f64, crate::ui::line_h() as f64);
        let mods = (self.mods.shift_key(), self.mods.alt_key(), self.mods.control_key());
        let Some(t) = self.term_mut(w) else { return changed };
        // Clamped to the grid, so a drag past the edges selects to the first / last cell.
        let row = (((y - oy as f64) / lh).floor().max(0.0) as usize).min(t.grid.rows - 1);
        let col = (((x - ox as f64) / cw).floor().max(0.0) as usize).min(t.grid.cols - 1);
        match drag {
            Drag::Select => {
                let at = (t.grid.abs_row(row, t.scroll_offset), col);
                if let Some(s) = t.sel.as_mut()
                    && s.head != at
                {
                    s.head = at;
                    return true;
                }
            }
            Drag::Report(b, last) => {
                if last != (row, col) {
                    t.mouse(MouseEv::Drag(b), col, row, mods);
                    self.termio.drag = Some((w, Drag::Report(b, (row, col))));
                }
            }
        }
        changed
    }

    /// A selection or a reported drag is in progress.
    pub fn term_dragging(&self) -> bool {
        self.termio.drag.is_some()
    }

    /// Mouse button released: end a selection (copied to the primary selection) or a reported drag.
    pub fn term_release(&mut self) {
        let Some((w, drag)) = self.termio.drag.take() else { return };
        let mods = (self.mods.shift_key(), self.mods.alt_key(), self.mods.control_key());
        let Some(t) = self.term_mut(w) else { return };
        match drag {
            Drag::Report(b, (row, col)) => {
                t.mouse(MouseEv::Release(b), col, row, mods);
            }
            Drag::Select => {
                if t.sel.is_some_and(|s| s.anchor == s.head) {
                    t.sel = None;
                }
                let text = t.selected_text();
                if !text.is_empty() {
                    self.clip.set_primary(&text);
                }
            }
        }
        self.request_redraw();
    }

    /// Wheel over terminal `w`: reported to a program that asked for the mouse, arrow keys on the
    /// alternate screen, otherwise the history scrolls.
    pub fn term_wheel(&mut self, w: Which, delta: MouseScrollDelta) {
        let (x, y) = self.mouse;
        let cell = self.term_cell(w, x, y);
        let mods = (self.mods.shift_key(), self.mods.alt_key(), self.mods.control_key());
        let lines = -crate::scroller::wheel_rows_int(delta, crate::ui::line_h() as f64);
        let Some(t) = self.term_mut(w) else { return };
        if lines == 0 {
            return;
        }
        let up = lines > 0;
        let n = lines.unsigned_abs();
        if t.grid.mouse_mode != 0 {
            let (row, col) = cell.unwrap_or((0, 0));
            for _ in 0..n.min(5) {
                t.mouse(MouseEv::Wheel(up), col, row, mods);
            }
        } else if t.alt_screen() {
            let keys = t.grid.wheel_keys(up, n.min(10));
            t.send(&keys);
        } else {
            t.scroll_view(lines);
        }
    }

    /// Per-event hook: a synchronized-output hold that ran out gets the screen drawn.
    pub fn term_tick(&mut self, want: &mut impl FnMut(std::time::Instant)) {
        let mut redraw = false;
        for w in [Which::Bottom, Which::Ai] {
            if let Some(t) = self.term_mut(w) {
                let (expired, next) = t.grid.sync_check();
                redraw |= expired;
                if let Some(n) = next {
                    want(n);
                }
            }
        }
        if redraw {
            self.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_found_under_the_pointer() {
        let line = "Open https://claude.ai/oauth?code=1, then (see http://x.io/a).";
        assert_eq!(url_at(line, 10).as_deref(), Some("https://claude.ai/oauth?code=1"));
        assert_eq!(url_at(line, 2), None, "not on the URL");
        assert_eq!(url_at(line, 50).as_deref(), Some("http://x.io/a"), "closing punctuation is not part of it");
        assert_eq!(url_at("héllo https://é.com/x y", 8).as_deref(), Some("https://é.com/x"), "columns count chars");
        assert_eq!(url_at("httpfoo", 1), None);
    }
}
