//! Input and side effects shared by the terminals (the bottom panel's shells and the AI panel's
//! agent): typing (xterm key encoding, or the kitty keyboard protocol when the program pushed its
//! flags; Ctrl+Shift+C / Ctrl+Shift+V), mouse selection, mouse
//! reporting, the wheel (history, or arrow keys / reports on the alternate screen), OSC 8 links
//! (Ctrl+click), OSC 52 clipboard writes and OSC 9 / 777 notifications.

use crate::{
    app::{App, KeyIn},
    terminal::{MouseEv, Sel, Term},
    termlink::{self, Target, TermLink},
    termview::{self, Look},
};
use winit::{event::MouseScrollDelta, keyboard::Key};

/// Said when a command needs the shell's OSC 133 marks and has none yet.
const NO_MARKS: &str = "no finished command to use yet (bash, zsh and fish report them automatically)";

/// Which terminal an action is for: one of the bottom panel's (by id, see `app_terms`) or the AI panel's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Bottom(u32),
    Ai,
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
    /// Top-left of each terminal's grid at its last draw.
    origin: Vec<(Which, (i32, i32))>,
    drag: Option<(Which, Drag)>,
    /// OSC 8 link under the pointer and its terminal (underlined while hovered).
    hover_link: Option<(Which, u16)>,
    /// A detected link (URL, `path:line:col`) under the pointer, and the cell it was computed for.
    hover: Option<(Which, TermLink)>,
    hover_key: Option<(Which, usize, usize)>,
}

impl TermIo {
    fn origin(&self, w: Which) -> Option<(i32, i32)> {
        self.origin.iter().find(|(x, _)| *x == w).map(|(_, o)| *o)
    }

    /// A terminal went away: drop its pointer state.
    pub fn forget(&mut self, w: Which) {
        self.origin.retain(|(x, _)| *x != w);
        if self.drag.is_some_and(|(x, _)| x == w) {
            self.drag = None;
        }
        if self.hover_link.is_some_and(|(x, _)| x == w) {
            self.hover_link = None;
        }
        if self.hover.as_ref().is_some_and(|(x, _)| *x == w) {
            self.hover = None;
        }
        self.hover_key = None;
    }

    /// The detected link under the pointer.
    pub fn hover_link(&self) -> Option<&TermLink> {
        self.hover.as_ref().map(|(_, l)| l)
    }
}

/// The `http(s)://` URL in `line` that covers column `col` (plain text, for Ctrl+click).
#[cfg(test)]
pub fn url_at(line: &str, col: usize) -> Option<String> {
    crate::termlink::url_span(line, col).map(|(_, _, u)| u)
}

impl App {
    pub fn term_ref(&self, w: Which) -> Option<&Term> {
        match w {
            Which::Bottom(id) => self.terms.get(id).map(|t| &t.term),
            Which::Ai => self.ai.term(),
        }
    }

    pub fn term_mut(&mut self, w: Which) -> Option<&mut Term> {
        match w {
            Which::Bottom(id) => self.terms.get_mut(id).map(|t| &mut t.term),
            Which::Ai => self.ai.term_mut(),
        }
    }

    /// Theme colors for drawing terminal `w` (and remembers where its grid starts).
    pub fn term_look(&mut self, w: Which, focused: bool, origin: (i32, i32)) -> Look {
        match self.termio.origin.iter_mut().find(|(x, _)| *x == w) {
            Some(slot) => slot.1 = origin,
            None => self.termio.origin.push((w, origin)),
        }
        Look {
            fg: self.theme.foreground,
            bg: self.theme.background,
            cursor: self.theme.cursor,
            selection: self.theme.selection,
            focused,
            hover_link: self.termio.hover_link.filter(|(x, _)| *x == w).map_or(0, |(_, l)| l),
            hover_span: self.termio.hover.as_ref().filter(|(x, _)| *x == w).map(|(_, l)| (l.abs, l.c0, l.c1)),
        }
    }

    /// The cell of terminal `w` under (x, y), if it is on the grid.
    fn term_cell(&self, w: Which, x: f64, y: f64) -> Option<(usize, usize)> {
        let (ox, oy) = self.termio.origin(w)?;
        termview::cell_at(self.term_ref(w)?, self.tx.cell_w, ox, oy, x, y)
    }

    /// Folders a relative `path:line` in terminal `w` can mean: the shell's own, then the project.
    fn link_bases(&self, w: Which) -> Vec<std::path::PathBuf> {
        let mut v: Vec<std::path::PathBuf> = self.term_ref(w).and_then(Term::cwd).into_iter().collect();
        v.push(self.root.clone());
        v
    }

    /// The URL or existing `path:line:col` at screen cell (row, col) of terminal `w`.
    fn link_in(&self, w: Which, row: usize, col: usize) -> Option<TermLink> {
        let t = self.term_ref(w)?;
        termlink::at(&t.grid, t.grid.abs_row(row, t.scroll_offset), col, &self.link_bases(w))
    }

    /// Open what a link points to: a URL with the system's opener, a file in the editor.
    fn follow_link(&mut self, target: &Target) {
        match target {
            Target::Url(u) => crate::paths::open_url(u),
            Target::File { path, line, col } => {
                if self.open_path(path, Some((*line, *col))).is_some() {
                    self.focus = crate::app::Focus::Editor;
                }
            }
        }
        self.request_redraw();
    }

    /// Tooltip target for the link under the pointer: its rect on screen and a key that changes
    /// with the link.
    pub fn term_link_tip(&self) -> Option<(crate::ui::Rect, u32)> {
        let (w, l) = self.termio.hover.as_ref()?;
        let (ox, oy) = self.termio.origin(*w)?;
        let t = self.term_ref(*w)?;
        let row = l.abs.checked_sub(t.grid.abs_row(0, t.scroll_offset))?;
        let (cw, lh) = (self.tx.cell_w, crate::ui::line_h() as i32);
        let x = ox + (l.c0 as f32 * cw) as i32;
        let r = crate::ui::Rect::new(x, oy + row as i32 * lh, ((l.c1 - l.c0) as f32 * cw) as i32, lh);
        Some((r, (l.abs as u32).wrapping_mul(1024).wrapping_add(l.c0 as u32)))
    }

    /// Text of the link tooltip.
    pub fn term_link_tip_text(&self) -> crate::app_tooltip::TipText {
        let Some(l) = self.termio.hover_link() else { return Default::default() };
        let what = if matches!(l.target, Target::Url(_)) { "open in the browser" } else { "open in the editor" };
        crate::app_tooltip::TipText {
            title: termlink::describe(&l.target, &self.root),
            detail: Some(format!("Ctrl+click to {what}")),
            ..Default::default()
        }
    }

    /// Terminals on screen now: the bottom panel's visible panes and the AI panel's agent.
    fn terms_on_screen(&self) -> Vec<Which> {
        let mut v: Vec<Which> = Vec::new();
        if self.panel.open && self.panel.tab == crate::app_panel::PanelTab::Terminal {
            v.extend(self.terms.panes.iter().map(|id| Which::Bottom(*id)));
        }
        if self.ai.open && self.ai.term().is_some() {
            v.push(Which::Ai);
        }
        v
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
        let sup = self.mods.super_key();
        // The search bar, when open, takes every key.
        if self.term_ref(w).is_some_and(|t| t.find.is_some()) {
            return self.term_find_key(w, ev);
        }
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
                // Ctrl+Shift+F searches the scrollback.
                "f" => {
                    self.term_find_open(w);
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
        // Ctrl+Up / Ctrl+Down jump between the shell's commands (when it reports them).
        if ctrl
            && !shift
            && !alt
            && matches!(named.as_deref(), Some("ArrowUp" | "ArrowDown"))
            && !t.alt_screen()
            && !t.grid.cmds.is_empty()
        {
            t.jump_command(named.as_deref() == Some("ArrowUp"));
            return true;
        }
        // Shift+PageUp/PageDown scroll the terminal's own history instead of going to the program.
        if shift && !ctrl && matches!(named.as_deref(), Some("PageUp" | "PageDown")) && !t.alt_screen() {
            let rows = t.grid.rows as isize;
            t.scroll_view(if named.as_deref() == Some("PageUp") { rows - 1 } else { -(rows - 1) });
            return true;
        }
        let app_cursor = t.grid.app_cursor;
        // A program that pushed kitty keyboard flags gets that encoding; everything else the xterm one.
        let flags = t.grid.kitty_flags();
        let bytes = if flags != 0 {
            let m = crate::kittykeys::Mods { shift, alt, ctrl, sup };
            crate::kittykeys::encode(flags, named.as_deref(), text.as_deref(), m)
        } else {
            crate::terminal::key_bytes_app(named.as_deref(), text.as_deref(), ctrl, shift, alt, app_cursor)
        };
        if let Some(bytes) = bytes {
            t.sel = None;
            t.write(&bytes);
        }
        true
    }

    /// Open the scrollback search of terminal `w`, starting from the selected text if it is one line.
    pub fn term_find_open(&mut self, w: Which) {
        let Some(t) = self.term_mut(w) else { return };
        let sel = t.selected_text();
        t.find_open();
        if !sel.is_empty()
            && !sel.contains('\n')
            && let Some(f) = t.find.as_mut()
        {
            f.query = sel;
        }
        t.find_changed();
        self.request_redraw();
    }

    /// A key while the search bar of terminal `w` is open: typing edits the query, Enter / Shift+Enter
    /// (or F3, Down / Up) step through the matches, Alt+C / Alt+R toggle case and regex, Esc closes.
    fn term_find_key(&mut self, w: Which, ev: &KeyIn) -> bool {
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        let named = match &ev.key {
            Key::Named(n) => Some(format!("{n:?}")),
            _ => None,
        };
        let letter = match &ev.key {
            Key::Character(c) => Some(c.to_lowercase()),
            _ => None,
        };
        let pasted = (ctrl && letter.as_deref() == Some("v")).then(|| {
            let t = self.clip.get();
            t.lines().next().unwrap_or_default().to_string()
        });
        let typed = match &ev.key {
            Key::Character(c) => Some(c.to_string()),
            _ => ev.text.clone(),
        }
        .filter(|t| !ctrl && !alt && !t.is_empty() && !t.chars().any(char::is_control));
        let Some(t) = self.term_mut(w) else { return true };
        let mut changed = false;
        match named.as_deref() {
            Some("Escape") => t.find_close(),
            Some("Enter" | "F3") => t.find_step(!shift),
            Some("ArrowDown") => t.find_step(true),
            Some("ArrowUp") => t.find_step(false),
            Some("Backspace") => {
                if let Some(f) = t.find.as_mut() {
                    if ctrl {
                        f.query.clear();
                    } else {
                        f.query.pop();
                    }
                }
                changed = true;
            }
            _ if alt && letter.as_deref() == Some("c") => {
                if let Some(f) = t.find.as_mut() {
                    f.case = !f.case;
                }
                changed = true;
            }
            _ if alt && letter.as_deref() == Some("r") => {
                if let Some(f) = t.find.as_mut() {
                    f.regex = !f.regex;
                }
                changed = true;
            }
            _ => {
                if let Some(text) = pasted.or(typed) {
                    if let Some(f) = t.find.as_mut() {
                        f.query.push_str(&text);
                    }
                    changed = true;
                }
            }
        }
        if changed {
            t.find_changed();
        }
        self.request_redraw();
        true
    }

    /// The bottom terminal the palette's terminal commands act on (opening one if needed).
    fn cmd_term(&mut self) -> Which {
        self.open_terminal();
        Which::Bottom(self.terms.active)
    }

    /// Palette: search the scrollback.
    pub fn term_cmd_find(&mut self) {
        let w = self.cmd_term();
        self.term_find_open(w);
    }

    /// Palette / Ctrl+Up / Ctrl+Down: previous or next command.
    pub fn term_cmd_jump(&mut self, up: bool) {
        let w = self.cmd_term();
        if !self.term_mut(w).is_some_and(|t| t.jump_command(up)) {
            self.say(NO_MARKS);
        }
        self.request_redraw();
    }

    /// Palette: select the output of the last (or the navigated-to) command.
    pub fn term_cmd_select(&mut self) {
        let w = self.cmd_term();
        if !self.term_mut(w).is_some_and(|t| t.select_output()) {
            self.say(NO_MARKS);
        }
        self.request_redraw();
    }

    /// Palette: copy the output of the last (or the navigated-to) command.
    pub fn term_cmd_copy(&mut self) {
        let w = self.cmd_term();
        match self.term_ref(w).and_then(Term::output_text) {
            Some(text) if !text.is_empty() => {
                self.clip.set(&text);
                self.toast("Copied the last command's output", true);
            }
            _ => self.say(NO_MARKS),
        }
    }

    /// Mouse press in terminal `w`: Ctrl+click opens a link, a program that asked for the mouse
    /// gets a report (Shift forces a selection instead), otherwise a selection starts.
    pub fn term_press(&mut self, w: Which, x: f64, y: f64, button: u8) {
        let Some((row, col)) = self.term_cell(w, x, y) else { return };
        let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
        if ctrl && button == 0 {
            let osc8 = self.term_ref(w).and_then(|t| t.link_at(row, col).map(str::to_string));
            if let Some(url) = osc8 {
                crate::paths::open_url(&url);
                return;
            }
            if let Some(link) = self.link_in(w, row, col) {
                self.follow_link(&link.target);
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
        let link = self.terms_on_screen().into_iter().find_map(|w| {
            let (r, c) = self.term_cell(w, x, y)?;
            let id = self.term_ref(w).map(|t| t.grid.view_cell(r, c, t.scroll_offset).link)?;
            (id != 0).then_some((w, id))
        });
        if self.termio.hover_link != link {
            self.termio.hover_link = link;
            changed = true;
        }
        // URLs and `path:line:col` found in the text (checked on disk once per cell).
        let cell = self.terms_on_screen().into_iter().find_map(|w| {
            let (r, c) = self.term_cell(w, x, y)?;
            let t = self.term_ref(w)?;
            Some((w, r, c, t.grid.abs_row(r, t.scroll_offset)))
        });
        let key = cell.map(|(w, _, c, abs)| (w, abs, c));
        if key != self.termio.hover_key || self.termio.drag.is_some() {
            self.termio.hover_key = key;
            let found = match cell {
                Some((w, r, c, _)) if link.is_none() && self.termio.drag.is_none() => {
                    self.link_in(w, r, c).map(|l| (w, l))
                }
                _ => None,
            };
            if self.termio.hover != found {
                self.termio.hover = found;
                changed = true;
            }
        }
        let Some((w, drag)) = self.termio.drag else { return changed };
        let Some((ox, oy)) = self.termio.origin(w) else { return changed };
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
        let all = self.terms.tabs.iter().map(|t| Which::Bottom(t.id)).chain([Which::Ai]).collect::<Vec<_>>();
        for w in all {
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
