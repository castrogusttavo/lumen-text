//! Integrated terminal: a PTY running the user's shell (or an agent CLI in the AI panel) plus an
//! xterm-compatible emulator over `vte`: cursor movement, erase, scroll regions, SGR colors and
//! attributes (bold, dim, italic, underline, strikethrough), wide characters, the alternate screen,
//! mouse and focus reporting, synchronized output, cursor shapes, OSC titles / hyperlinks /
//! clipboard / notifications and the kitty keyboard protocol's flag stack (`kittykeys`). Sequences
//! it does not implement (modifyOtherKeys...) are ignored instead of being misread as other commands.

use crate::{
    lsp::Wake,
    pty::{EnvEdit, Pty},
    tasks::{PALETTE, xterm256},
    theme::Rgba,
};
use std::{
    collections::VecDeque,
    io::Read,
    path::Path,
    sync::mpsc::{Receiver, channel},
    thread,
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthChar;

/// Cell attribute flags (`Cell::flags`).
pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const UNDERLINE: u8 = 8;
pub const STRIKE: u8 = 16;
pub const INVERSE: u8 = 32;
pub const HIDDEN: u8 = 64;
/// First half of a double-width character; the next cell holds [`WIDE_TAIL`].
pub const WIDE: u8 = 128;
/// What the second cell of a double-width character holds (never drawn, never copied).
pub const WIDE_TAIL: char = '\0';

/// Longest `?2026` (synchronized output) hold before the screen is drawn anyway.
pub const SYNC_MAX: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    /// [`BOLD`], [`DIM`], [`ITALIC`], [`UNDERLINE`], [`STRIKE`], [`INVERSE`], [`HIDDEN`], [`WIDE`].
    pub flags: u8,
    /// OSC 8 hyperlink: index + 1 into `Grid::links` (0 = none).
    pub link: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: None, bg: None, flags: 0, link: 0 }
    }
}

impl Cell {
    pub fn has(&self, f: u8) -> bool {
        self.flags & f != 0
    }
}

/// DECSCUSR cursor shapes (blinking variants are drawn steady).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// A mouse event to report to the program (buttons: 0 left, 1 middle, 2 right).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseEv {
    Press(u8),
    Release(u8),
    Drag(u8),
    /// `true` = up.
    Wheel(bool),
}

/// Default scrollback size (lines).
pub const DEFAULT_HISTORY: usize = 5_000;
/// Most distinct OSC 8 targets kept per terminal (later links are shown as plain text).
const MAX_LINKS: usize = 4096;

/// The main screen parked while the alternate screen is active.
struct SavedMain {
    cells: Vec<Cell>,
    cx: usize,
    cy: usize,
    saved: (usize, usize),
    top: usize,
    bottom: usize,
    attr: Cell,
    wrap_pending: bool,
}

/// Re-lay `cells` (old_rows x old_cols) into rows x cols keeping the top-left,
/// or the bottom rows when shrinking vertically. Returns the cells and the row shift.
fn reshape(
    cells: &[Cell],
    old_rows: usize,
    old_cols: usize,
    cy: usize,
    rows: usize,
    cols: usize,
) -> (Vec<Cell>, usize) {
    let mut out = vec![Cell::default(); rows * cols];
    let shift = cy.saturating_sub(rows - 1).min(old_rows.saturating_sub(rows));
    for r in 0..rows.min(old_rows - shift) {
        for c in 0..cols.min(old_cols) {
            out[r * cols + c] = cells[(r + shift) * old_cols + c];
        }
    }
    (out, shift)
}

/// DEC special graphics (`ESC ( 0`): the line-drawing set ncurses programs use for boxes.
fn dec_graphics(ch: char) -> char {
    match ch {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => ch,
    }
}

/// The color of a colon-separated SGR 38/48/58 (`sub` starts after the 38): `5:n`, `2:r:g:b` or
/// `2:<colorspace>:r:g:b` (the colorspace id is often left empty, which reads as 0).
fn colon_color(sub: &[u16]) -> Option<Rgba> {
    match sub.first()? {
        5 => sub.get(1).map(|&n| xterm256((n as u32).min(255))),
        2 => {
            let rgb = if sub.len() >= 5 {
                &sub[2..5]
            } else if sub.len() == 4 {
                &sub[1..4]
            } else {
                return None;
            };
            Some(Rgba(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, 255))
        }
        _ => None,
    }
}

/// Standard base64 (OSC 52 payloads); `None` on anything that is not base64.
fn base64(data: &[u8]) -> Option<Vec<u8>> {
    std::str::from_utf8(data).ok().and_then(|s| crate::update::base64_decode(s).ok())
}

pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<Cell>,
    pub cx: usize,
    pub cy: usize,
    pub cursor_visible: bool,
    /// Answers to terminal queries (cursor position, device status/attributes), sent back to the shell.
    pub replies: Vec<u8>,
    attr: Cell,
    saved: (usize, usize),
    top: usize,
    bottom: usize,
    wrap_pending: bool,
    history: VecDeque<Vec<Cell>>,
    max_history: usize,
    /// Total lines ever pushed into history (monotonic; lets `Term` keep a scrolled view stable).
    pushed: usize,
    alt: Option<Box<SavedMain>>,
    /// DECCKM: arrow keys send `ESC O x`.
    pub app_cursor: bool,
    /// `?2004`: pasted text should be bracketed.
    pub bracketed_paste: bool,
    /// `?7` (DECAWM): printing past the last column wraps to the next line.
    pub autowrap: bool,
    /// `CSI 4 h` (IRM): printed characters push the rest of the line right.
    pub insert: bool,
    /// Mouse reporting: 0 off, 9 (X10, presses only), 1000 (press / release), 1002 (+ drags), 1003 (+ motion).
    pub mouse_mode: u16,
    /// `?1006`: mouse reports use the SGR encoding (`CSI < b ; x ; y M/m`).
    pub mouse_sgr: bool,
    /// `?1004`: the program wants `CSI I` / `CSI O` when the terminal gains / loses the focus.
    pub focus_events: bool,
    /// `?2026` (synchronized output) was set at this moment and not reset yet.
    sync_since: Option<Instant>,
    pub cursor_shape: CursorShape,
    /// OSC 0 / 2 window title.
    pub title: String,
    /// OSC 8 hyperlink targets (`Cell::link` - 1 indexes here).
    pub links: Vec<String>,
    /// OSC 52 text the program asked to put on the clipboard (taken by the app).
    pub clipboard: Option<String>,
    /// OSC 9 / 777 desktop notifications: (title, body), taken by the app.
    pub notices: Vec<(String, String)>,
    /// BEL characters received so far.
    pub bells: u64,
    last_char: Option<char>,
    /// `ESC ( 0` is active: ASCII letters print DEC line-drawing characters.
    line_drawing: bool,
    /// Default foreground / background, for OSC 10 / 11 color queries.
    pub colors: (Rgba, Rgba),
    /// Kitty keyboard protocol flags pushed by the program (one stack per screen).
    pub keys: crate::kittykeys::KeyStack,
}

impl Grid {
    pub fn new(rows: usize, cols: usize) -> Grid {
        let (rows, cols) = (rows.max(1), cols.max(1));
        Grid {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
            cx: 0,
            cy: 0,
            cursor_visible: true,
            replies: Vec::new(),
            attr: Cell::default(),
            saved: (0, 0),
            top: 0,
            bottom: rows - 1,
            wrap_pending: false,
            history: VecDeque::new(),
            max_history: DEFAULT_HISTORY,
            pushed: 0,
            alt: None,
            app_cursor: false,
            bracketed_paste: false,
            autowrap: true,
            insert: false,
            mouse_mode: 0,
            mouse_sgr: false,
            focus_events: false,
            sync_since: None,
            cursor_shape: CursorShape::Block,
            title: String::new(),
            links: Vec::new(),
            clipboard: None,
            notices: Vec::new(),
            bells: 0,
            last_char: None,
            line_drawing: false,
            colors: (Rgba(0xd4, 0xd4, 0xd4, 255), Rgba(0x1e, 0x1e, 0x1e, 255)),
            keys: Default::default(),
        }
    }

    /// Kitty keyboard flags in effect on the current screen (0 = legacy key encoding).
    pub fn kitty_flags(&self) -> u8 {
        self.keys.flags(self.alt.is_some())
    }

    pub fn at(&self, r: usize, c: usize) -> Cell {
        self.cells[r * self.cols + c]
    }

    #[cfg(test)]
    pub fn row_text(&self, r: usize) -> String {
        (0..self.cols).map(|c| self.at(r, c).ch).filter(|c| *c != WIDE_TAIL).collect::<String>().trim_end().to_string()
    }

    /// Keep the top-left content; the cursor is clamped.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if rows == self.rows && cols == self.cols {
            return;
        }
        if let Some(m) = self.alt.as_mut() {
            let (cells, shift) = reshape(&m.cells, self.rows, self.cols, m.cy, rows, cols);
            m.cells = cells;
            m.cy = (m.cy - shift).min(rows - 1);
            m.cx = m.cx.min(cols - 1);
            m.saved = (m.saved.0.min(rows - 1), m.saved.1.min(cols - 1));
            m.top = 0;
            m.bottom = rows - 1;
            m.wrap_pending = false;
        }
        // When shrinking vertically keep the bottom rows (where the prompt is).
        let (cells, shift) = reshape(&self.cells, self.rows, self.cols, self.cy, rows, cols);
        self.cells = cells;
        self.cy = (self.cy - shift).min(rows - 1);
        self.cx = self.cx.min(cols - 1);
        self.rows = rows;
        self.cols = cols;
        self.top = 0;
        self.bottom = rows - 1;
        self.wrap_pending = false;
    }

    /// Lines available to scroll back to (always 0 on the alternate screen).
    pub fn history_len(&self) -> usize {
        if self.alt.is_some() { 0 } else { self.history.len() }
    }

    pub fn alt_screen(&self) -> bool {
        self.alt.is_some()
    }

    /// The cell at screen row `row` when the view is `offset` lines back in history.
    pub fn view_cell(&self, row: usize, col: usize, offset: usize) -> Cell {
        let h = self.history_len();
        let v = h - offset.min(h) + row;
        if v < h {
            self.history[v].get(col).copied().unwrap_or_default()
        } else if v - h < self.rows && col < self.cols {
            self.at(v - h, col)
        } else {
            Cell::default()
        }
    }

    /// Absolute line number of screen row `row` with the view `offset` lines back: stable while
    /// output scrolls (selections are kept in these coordinates).
    pub fn abs_row(&self, row: usize, offset: usize) -> usize {
        self.pushed + row - offset.min(self.history_len())
    }

    /// The cell on absolute line `line` (see [`abs_row`](Self::abs_row)); blank when it is gone.
    pub fn abs_cell(&self, line: usize, col: usize) -> Cell {
        if line >= self.pushed {
            let r = line - self.pushed;
            return if r < self.rows && col < self.cols { self.at(r, col) } else { Cell::default() };
        }
        let back = self.pushed - line;
        let h = self.history_len();
        if back > h {
            return Cell::default();
        }
        self.history[h - back].get(col).copied().unwrap_or_default()
    }

    /// The text of a selection, one line per row (trailing blanks trimmed).
    pub fn sel_text(&self, sel: Sel) -> String {
        let ((l0, c0), (l1, c1)) = sel.ordered();
        let mut out: Vec<String> = Vec::new();
        for line in l0..=l1 {
            let (from, to) = (if line == l0 { c0 } else { 0 }, if line == l1 { c1 + 1 } else { self.cols });
            let s: String =
                (from..to.min(self.cols)).map(|c| self.abs_cell(line, c).ch).filter(|c| *c != WIDE_TAIL).collect();
            out.push(s.trim_end().to_string());
        }
        out.join("\n")
    }

    #[cfg(test)]
    pub fn view_row_text(&self, row: usize, offset: usize) -> String {
        (0..self.cols)
            .map(|c| self.view_cell(row, c, offset).ch)
            .filter(|c| *c != WIDE_TAIL)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Bytes to send for pasted text (bracketed when the program asked for it).
    pub fn paste_bytes(&self, text: &str) -> Vec<u8> {
        if self.bracketed_paste {
            let mut v = b"\x1b[200~".to_vec();
            v.extend_from_slice(text.as_bytes());
            v.extend_from_slice(b"\x1b[201~");
            v
        } else {
            text.as_bytes().to_vec()
        }
    }

    /// Is the screen held back by `?2026` (synchronized output, at most [`SYNC_MAX`])?
    pub fn sync_held(&self) -> bool {
        self.sync_since.is_some_and(|t| t.elapsed() < SYNC_MAX)
    }

    /// Synchronized-output timer: `(false, Some(deadline))` while a hold is on; a hold past its
    /// deadline is dropped and reported with `(true, None)` (draw the screen now).
    pub fn sync_check(&mut self) -> (bool, Option<Instant>) {
        match self.sync_since {
            Some(t) if t.elapsed() >= SYNC_MAX => {
                self.sync_since = None;
                (true, None)
            }
            Some(t) => (false, Some(t + SYNC_MAX)),
            None => (false, None),
        }
    }

    /// The report for a mouse event at a 0-based cell, or `None` when the program did not ask for it.
    pub fn mouse_report(
        &self,
        ev: MouseEv,
        col: usize,
        row: usize,
        shift: bool,
        alt: bool,
        ctrl: bool,
    ) -> Option<Vec<u8>> {
        let mode = self.mouse_mode;
        if mode == 0 {
            return None;
        }
        let (mut b, release) = match ev {
            MouseEv::Press(b) => (b as u32, false),
            MouseEv::Release(b) if mode != 9 => (b as u32, true),
            MouseEv::Drag(b) if mode >= 1002 => (b as u32 + 32, false),
            MouseEv::Wheel(up) if mode != 9 => (if up { 64 } else { 65 }, false),
            _ => return None,
        };
        if mode != 9 {
            b += 4 * shift as u32 + 8 * alt as u32 + 16 * ctrl as u32;
        }
        let (x, y) = (col + 1, row + 1);
        if self.mouse_sgr {
            return Some(format!("\x1b[<{b};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes());
        }
        // Legacy encoding: one byte per value (offset by 32), releases do not say which button.
        if release {
            b = (b & !3) | 3;
        }
        if x + 32 > 255 || y + 32 > 255 {
            return None;
        }
        Some(vec![0x1b, b'[', b'M', (32 + b) as u8, (32 + x) as u8, (32 + y) as u8])
    }

    /// Arrow keys standing in for wheel notches on the alternate screen (pagers, editors without
    /// mouse support scroll with them).
    pub fn wheel_keys(&self, up: bool, lines: usize) -> Vec<u8> {
        let k = match (self.app_cursor, up) {
            (true, true) => "\x1bOA",
            (true, false) => "\x1bOB",
            (false, true) => "\x1b[A",
            (false, false) => "\x1b[B",
        };
        k.repeat(lines.max(1)).into_bytes()
    }

    fn enter_alt(&mut self) {
        if self.alt.is_some() {
            return;
        }
        let blank = vec![Cell::default(); self.rows * self.cols];
        self.alt = Some(Box::new(SavedMain {
            cells: std::mem::replace(&mut self.cells, blank),
            cx: self.cx,
            cy: self.cy,
            saved: self.saved,
            top: self.top,
            bottom: self.bottom,
            attr: self.attr,
            wrap_pending: self.wrap_pending,
        }));
        self.cx = 0;
        self.cy = 0;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.wrap_pending = false;
    }

    fn leave_alt(&mut self) {
        if let Some(m) = self.alt.take() {
            self.cells = m.cells;
            self.cx = m.cx;
            self.cy = m.cy;
            self.saved = m.saved;
            self.top = m.top;
            self.bottom = m.bottom;
            self.attr = m.attr;
            self.wrap_pending = m.wrap_pending;
        }
    }

    fn blank(&self) -> Cell {
        Cell { bg: self.attr.bg, ..Cell::default() }
    }

    /// Writing over half of a wide character blanks its other half.
    fn clear_wide_at(&mut self, c: usize) {
        if c >= self.cols {
            return;
        }
        let row = self.cy * self.cols;
        let cell = self.cells[row + c];
        if cell.ch == WIDE_TAIL && c > 0 {
            let head = &mut self.cells[row + c - 1];
            head.ch = ' ';
            head.flags &= !WIDE;
        }
        if cell.has(WIDE) && c + 1 < self.cols {
            self.cells[row + c + 1].ch = ' ';
        }
    }

    fn put(&mut self, ch: char) {
        let ch = if self.line_drawing { dec_graphics(ch) } else { ch };
        // Combining marks and zero-width joiners have no cell of their own (one char per cell):
        // they are dropped and the base character stands alone.
        let w = match ch.width() {
            Some(0) => return,
            Some(2) => 2,
            _ => 1,
        };
        if self.wrap_pending && self.autowrap {
            self.cx = 0;
            self.linefeed();
        }
        self.wrap_pending = false;
        if w == 2 && self.cx + 1 >= self.cols {
            if self.cols < 2 {
                return;
            }
            if self.autowrap {
                // Does not fit at the end of the line: the last cell stays blank and it wraps.
                let i = self.cy * self.cols + self.cx;
                self.cells[i] = self.blank();
                self.cx = 0;
                self.linefeed();
            } else {
                self.cx = self.cols - 2;
            }
        }
        let row = self.cy * self.cols;
        if self.insert {
            let n = w.min(self.cols - self.cx);
            self.cells.copy_within(row + self.cx..row + self.cols - n, row + self.cx + n);
        }
        self.clear_wide_at(self.cx);
        if w == 2 {
            self.clear_wide_at(self.cx + 1);
        }
        let i = row + self.cx;
        self.cells[i] = Cell { ch, flags: self.attr.flags | if w == 2 { WIDE } else { 0 }, ..self.attr };
        if w == 2 {
            self.cells[i + 1] = Cell { ch: WIDE_TAIL, ..self.attr };
        }
        self.last_char = Some(ch);
        if self.cx + w >= self.cols {
            self.cx = self.cols - 1;
            self.wrap_pending = self.autowrap;
        } else {
            self.cx += w;
        }
    }

    fn linefeed(&mut self) {
        if self.cy == self.bottom {
            self.scroll_up(1, true);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    fn scroll_up(&mut self, n: usize, record: bool) {
        let n = n.min(self.bottom - self.top + 1);
        let cols = self.cols;
        if record && self.alt.is_none() && self.top == 0 && self.bottom == self.rows - 1 && self.max_history > 0 {
            for r in 0..n {
                let mut line = self.cells[r * cols..(r + 1) * cols].to_vec();
                while line.last() == Some(&Cell::default()) {
                    line.pop();
                }
                if self.history.len() >= self.max_history {
                    self.history.pop_front();
                }
                self.history.push_back(line);
                self.pushed += 1;
            }
        }
        self.cells.copy_within((self.top + n) * cols..(self.bottom + 1) * cols, self.top * cols);
        let blank = self.blank();
        for r in (self.bottom + 1 - n)..=self.bottom {
            self.cells[r * cols..(r + 1) * cols].fill(blank);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.bottom - self.top + 1);
        let cols = self.cols;
        self.cells.copy_within(self.top * cols..(self.bottom + 1 - n) * cols, (self.top + n) * cols);
        let blank = self.blank();
        for r in self.top..self.top + n {
            self.cells[r * cols..(r + 1) * cols].fill(blank);
        }
    }

    fn erase_range(&mut self, from: usize, to: usize) {
        let blank = self.blank();
        let n = self.cells.len();
        for c in &mut self.cells[from.min(n)..to.min(n)] {
            *c = blank;
        }
    }

    fn move_to(&mut self, r: usize, c: usize) {
        self.cy = r.min(self.rows - 1);
        self.cx = c.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn reply(&mut self, s: &str) {
        self.replies.extend_from_slice(s.as_bytes());
    }

    /// Index + 1 of a hyperlink target (0 when the table is full).
    fn link_id(&mut self, uri: String) -> u16 {
        if let Some(i) = self.links.iter().rposition(|l| *l == uri) {
            return (i + 1) as u16;
        }
        if self.links.len() >= MAX_LINKS {
            return 0;
        }
        self.links.push(uri);
        self.links.len() as u16
    }

    /// State of a DEC private mode for DECRQM (`None` = not recognized).
    fn private_mode(&self, m: u16) -> Option<bool> {
        Some(match m {
            1 => self.app_cursor,
            7 => self.autowrap,
            25 => self.cursor_visible,
            9 | 1000 | 1002 | 1003 => self.mouse_mode == m,
            1004 => self.focus_events,
            1006 => self.mouse_sgr,
            2004 => self.bracketed_paste,
            2026 => self.sync_since.is_some(),
            47 | 1047 | 1049 => self.alt.is_some(),
            _ => return None,
        })
    }

    /// `CSI ? Pm h` / `CSI ? Pm l`.
    fn private_set(&mut self, p: &[u16], on: bool) {
        for &mode in p {
            match mode {
                1 => self.app_cursor = on,
                7 => self.autowrap = on,
                25 => self.cursor_visible = on,
                9 | 1000 | 1002 | 1003 => {
                    if on {
                        self.mouse_mode = mode;
                    } else if self.mouse_mode == mode {
                        self.mouse_mode = 0;
                    }
                }
                1004 => self.focus_events = on,
                1006 => self.mouse_sgr = on,
                2004 => self.bracketed_paste = on,
                2026 => self.sync_since = if on { Some(Instant::now()) } else { None },
                47 | 1047 | 1049 => {
                    if on {
                        self.enter_alt()
                    } else {
                        self.leave_alt()
                    }
                }
                _ => {}
            }
        }
    }

    /// DECSTR (`CSI ! p`): modes and attributes back to their defaults, the screen untouched.
    fn soft_reset(&mut self) {
        self.cursor_visible = true;
        self.app_cursor = false;
        self.autowrap = true;
        self.insert = false;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.attr = Cell::default();
        self.saved = (0, 0);
        self.line_drawing = false;
        self.cursor_shape = CursorShape::Block;
    }

    fn sgr(&mut self, params: &[Vec<u16>]) {
        if params.is_empty() {
            self.attr = Cell { link: self.attr.link, ..Cell::default() };
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = &params[i];
            let code = p.first().copied().unwrap_or(0);
            if p.len() > 1 {
                // Colon form: `38:2::r:g:b`, `38:5:n`, `4:3` (curly underline) ...
                match code {
                    38 => self.attr.fg = colon_color(&p[1..]),
                    48 => self.attr.bg = colon_color(&p[1..]),
                    4 if p[1] == 0 => self.attr.flags &= !UNDERLINE,
                    4 => self.attr.flags |= UNDERLINE,
                    _ => {}
                }
                i += 1;
                continue;
            }
            match code {
                0 => self.attr = Cell { link: self.attr.link, ..Cell::default() },
                1 => self.attr.flags |= BOLD,
                2 => self.attr.flags |= DIM,
                3 => self.attr.flags |= ITALIC,
                4 | 21 => self.attr.flags |= UNDERLINE,
                7 => self.attr.flags |= INVERSE,
                8 => self.attr.flags |= HIDDEN,
                9 => self.attr.flags |= STRIKE,
                22 => self.attr.flags &= !(BOLD | DIM),
                23 => self.attr.flags &= !ITALIC,
                24 => self.attr.flags &= !UNDERLINE,
                27 => self.attr.flags &= !INVERSE,
                28 => self.attr.flags &= !HIDDEN,
                29 => self.attr.flags &= !STRIKE,
                n @ 30..=37 => self.attr.fg = Some(PALETTE[(n - 30) as usize]),
                n @ 90..=97 => self.attr.fg = Some(PALETTE[(n - 90 + 8) as usize]),
                n @ 40..=47 => self.attr.bg = Some(PALETTE[(n - 40) as usize]),
                n @ 100..=107 => self.attr.bg = Some(PALETTE[(n - 100 + 8) as usize]),
                39 => self.attr.fg = None,
                49 => self.attr.bg = None,
                // Semicolon form: the color arguments are separate parameters (58 = underline color:
                // not drawn, but its arguments must not be read as attributes).
                n @ (38 | 48 | 58) => {
                    let arg = |k: usize| params.get(i + k).and_then(|v| v.first().copied());
                    let (color, used) = match arg(1) {
                        Some(5) => (arg(2).map(|v| xterm256((v as u32).min(255))), 2),
                        Some(2) => {
                            let c = match (arg(2), arg(3), arg(4)) {
                                (Some(r), Some(g), Some(b)) => Some(Rgba(r as u8, g as u8, b as u8, 255)),
                                _ => None,
                            };
                            (c, 4)
                        }
                        _ => (None, 0),
                    };
                    i += used;
                    match n {
                        38 => self.attr.fg = color,
                        48 => self.attr.bg = color,
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

impl vte::Perform for Grid {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.bells += 1,
            0x08 => {
                self.cx = self.cx.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => {
                self.cx = ((self.cx / 8 + 1) * 8).min(self.cols - 1);
                self.wrap_pending = false;
            }
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.cx = 0;
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, inter: &[u8], _ignore: bool, action: char) {
        let all: Vec<Vec<u16>> = params.iter().map(|s| s.to_vec()).collect();
        let p: Vec<u16> = all.iter().map(|s| s.first().copied().unwrap_or(0)).collect();
        let get = |i: usize, d: usize| p.get(i).copied().filter(|v| *v != 0).map_or(d, |v| v as usize);
        let first = p.first().copied().unwrap_or(0);
        match inter {
            b"" => {}
            b"?" => {
                if matches!(action, 'h' | 'l') {
                    self.private_set(&p, action == 'h');
                }
                // `CSI ? u`: kitty keyboard query, answered with the flags in effect.
                if action == 'u' {
                    let f = self.kitty_flags();
                    self.reply(&format!("\x1b[?{f}u"));
                }
                return;
            }
            // DECRQM for a private mode: 1 = set, 2 = reset, 0 = unknown.
            b"?$" if action == 'p' => {
                let st = match self.private_mode(first) {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                };
                return self.reply(&format!("\x1b[?{first};{st}$y"));
            }
            b"$" if action == 'p' => {
                let st = match first {
                    4 if self.insert => 1,
                    4 => 2,
                    _ => 0,
                };
                return self.reply(&format!("\x1b[{first};{st}$y"));
            }
            // DECSCUSR: 0-2 block, 3-4 underline, 5-6 bar (blinking or not).
            b" " if action == 'q' => {
                self.cursor_shape = match first {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
                return;
            }
            b">" if action == 'c' => return self.reply("\x1b[>0;10;1c"),
            // Kitty keyboard protocol: push / pop / set the enhancement flags of this screen.
            b">" if action == 'u' => return self.keys.push(self.alt.is_some(), first.min(255) as u8),
            b"<" if action == 'u' => return self.keys.pop(self.alt.is_some(), get(0, 1)),
            b"=" if action == 'u' => {
                let mode = p.get(1).copied().unwrap_or(1);
                return self.keys.set(self.alt.is_some(), first.min(255) as u8, mode);
            }
            // XTVERSION.
            b">" if action == 'q' => return self.reply("\x1bP>|lumen-text\x1b\\"),
            b"!" if action == 'p' => return self.soft_reset(),
            // Any other private marker or intermediate (`CSI > 4;2 m` modifyOtherKeys, ...): not
            // supported, and never one of the plain commands below.
            _ => return,
        }
        match action {
            // ConPTY (Windows) waits for the answer to its cursor query before it prints anything.
            'n' if first == 6 => {
                let s = format!("\x1b[{};{}R", self.cy + 1, self.cx + 1);
                self.reply(&s);
            }
            'n' if first == 5 => self.reply("\x1b[0n"),
            'c' if first == 0 => self.reply("\x1b[?1;2c"),
            // Window size in characters (other window operations are ignored).
            't' if first == 18 => {
                let s = format!("\x1b[8;{};{}t", self.rows, self.cols);
                self.reply(&s);
            }
            'h' | 'l' if p.contains(&4) => self.insert = action == 'h',
            'A' => self.move_to(self.cy.saturating_sub(get(0, 1)), self.cx),
            'B' | 'e' => self.move_to(self.cy + get(0, 1), self.cx),
            'C' | 'a' => self.move_to(self.cy, self.cx + get(0, 1)),
            'D' => self.move_to(self.cy, self.cx.saturating_sub(get(0, 1))),
            'E' => self.move_to(self.cy + get(0, 1), 0),
            'F' => self.move_to(self.cy.saturating_sub(get(0, 1)), 0),
            'G' | '`' => self.move_to(self.cy, get(0, 1) - 1),
            'H' | 'f' => self.move_to(get(0, 1) - 1, get(1, 1) - 1),
            'd' => self.move_to(get(0, 1) - 1, self.cx),
            'I' => {
                for _ in 0..get(0, 1).min(self.cols) {
                    self.cx = ((self.cx / 8 + 1) * 8).min(self.cols - 1);
                }
                self.wrap_pending = false;
            }
            'Z' => {
                for _ in 0..get(0, 1).min(self.cols) {
                    self.cx = self.cx.saturating_sub(1) / 8 * 8;
                }
                self.wrap_pending = false;
            }
            'b' => {
                if let Some(c) = self.last_char {
                    for _ in 0..get(0, 1).min(self.rows * self.cols) {
                        self.put(c);
                    }
                }
            }
            'J' => {
                let here = self.cy * self.cols + self.cx;
                match first {
                    0 => self.erase_range(here, self.cells.len()),
                    1 => self.erase_range(0, here + 1),
                    3 => self.history.clear(),
                    _ => self.erase_range(0, self.cells.len()),
                }
            }
            'K' => {
                let (row, here) = (self.cy * self.cols, self.cy * self.cols + self.cx);
                match first {
                    0 => self.erase_range(here, row + self.cols),
                    1 => self.erase_range(row, here + 1),
                    _ => self.erase_range(row, row + self.cols),
                }
            }
            'L' => {
                if self.cy >= self.top && self.cy <= self.bottom {
                    let saved = self.top;
                    self.top = self.cy;
                    self.scroll_down(get(0, 1));
                    self.top = saved;
                }
            }
            'M' => {
                if self.cy >= self.top && self.cy <= self.bottom {
                    let saved = self.top;
                    self.top = self.cy;
                    self.scroll_up(get(0, 1), false);
                    self.top = saved;
                }
            }
            '@' => {
                let n = get(0, 1).min(self.cols - self.cx);
                let row = self.cy * self.cols;
                self.cells.copy_within(row + self.cx..row + self.cols - n, row + self.cx + n);
                let blank = self.blank();
                self.cells[row + self.cx..row + self.cx + n].fill(blank);
            }
            'P' => {
                let n = get(0, 1).min(self.cols - self.cx);
                let row = self.cy * self.cols;
                self.cells.copy_within(row + self.cx + n..row + self.cols, row + self.cx);
                let blank = self.blank();
                self.cells[row + self.cols - n..row + self.cols].fill(blank);
            }
            'X' => {
                let here = self.cy * self.cols + self.cx;
                self.erase_range(here, here + get(0, 1).min(self.cols - self.cx));
            }
            'S' => self.scroll_up(get(0, 1), false),
            'T' => self.scroll_down(get(0, 1)),
            'r' => {
                let (t, b) = (get(0, 1) - 1, get(1, self.rows) - 1);
                if t < b && b < self.rows {
                    self.top = t;
                    self.bottom = b;
                    self.move_to(0, 0);
                }
            }
            's' => self.saved = (self.cy, self.cx),
            'u' => self.move_to(self.saved.0, self.saved.1),
            'm' => self.sgr(&all),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, byte: u8) {
        match (inter, byte) {
            (b"(", b'0') => self.line_drawing = true,
            (b"(", _) => self.line_drawing = false,
            ([], b'7') => self.saved = (self.cy, self.cx),
            ([], b'8') => self.move_to(self.saved.0, self.saved.1),
            ([], b'M') => {
                if self.cy == self.top {
                    self.scroll_down(1);
                } else {
                    self.cy = self.cy.saturating_sub(1);
                }
            }
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.cx = 0;
                self.linefeed();
            }
            ([], b'c') => {
                let history = std::mem::take(&mut self.history);
                let links = std::mem::take(&mut self.links);
                let (pushed, max, colors) = (self.pushed, self.max_history, self.colors);
                *self = Grid::new(self.rows, self.cols);
                self.history = history;
                self.links = links;
                self.pushed = pushed;
                self.max_history = max;
                self.colors = colors;
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let Some(kind) = params.first() else { return };
        let text = |from: usize| -> String {
            params
                .get(from..)
                .unwrap_or_default()
                .iter()
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
                .join(";")
        };
        match *kind {
            b"0" | b"2" => {
                self.title = text(1).chars().filter(|c| !c.is_control()).take(256).collect();
            }
            // Hyperlink: `OSC 8 ; params ; uri`; an empty uri ends it (the uri may contain ';').
            b"8" => {
                let uri = text(2);
                self.attr.link = if uri.is_empty() { 0 } else { self.link_id(uri) };
            }
            // Set the clipboard: `OSC 52 ; c ; <base64>` (queries, `?`, are not answered).
            b"52" => {
                if let Some(data) = params.get(2)
                    && *data != &b"?"[..]
                    && let Some(bytes) = base64(data)
                {
                    self.clipboard = Some(String::from_utf8_lossy(&bytes).into_owned());
                }
            }
            // Notification (`OSC 9 ; 4 ; ...` is ConEmu's progress report, not a message).
            b"9" => {
                if params.get(1).is_some_and(|p| *p == &b"4"[..]) {
                    return;
                }
                let body = text(1);
                if !body.is_empty() {
                    self.notices.push((String::new(), body));
                }
            }
            b"777" if params.get(1).is_some_and(|p| *p == &b"notify"[..]) => {
                let title = params.get(2).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
                self.notices.push((title, text(3)));
            }
            // Default foreground / background color queries (programs pick light or dark styles).
            b"10" | b"11" if params.get(1).is_some_and(|p| *p == &b"?"[..]) => {
                let c = if *kind == &b"10"[..] { self.colors.0 } else { self.colors.1 };
                let end = if bell_terminated { "\x07" } else { "\x1b\\" };
                let k = String::from_utf8_lossy(kind).into_owned();
                let s =
                    format!("\x1b]{k};rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}{end}", c.0, c.0, c.1, c.1, c.2, c.2);
                self.reply(&s);
            }
            _ => {}
        }
    }
}

/// A selection in absolute line coordinates (`Grid::abs_row`), anchor where the drag started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sel {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl Sel {
    /// (start, end) in reading order; `end` is inclusive.
    pub fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    pub fn contains(&self, line: usize, col: usize) -> bool {
        let (a, b) = self.ordered();
        a != b && (line, col) >= a && (line, col) <= b
    }
}

pub struct Term {
    pub grid: Grid,
    parser: vte::Parser,
    pty: Pty,
    rx: Receiver<Vec<u8>>,
    pub alive: bool,
    /// Lines the view is scrolled back into history (0 = live).
    pub scroll_offset: usize,
    /// When the program started, last printed something, and last got a key from us.
    pub started: Instant,
    pub last_output: Option<Instant>,
    pub last_input: Option<Instant>,
    /// Exit code once the program has ended.
    pub exit_code: Option<i32>,
    /// Text selected with the mouse.
    pub sel: Option<Sel>,
    /// Last focus state reported with `?1004`.
    focus_sent: Option<bool>,
}

impl Term {
    pub fn spawn(cwd: &Path, rows: usize, cols: usize, wake: Wake) -> std::io::Result<Term> {
        let (pty, reader) = Pty::spawn(cwd, rows, cols)?;
        Ok(Self::start(pty, reader, rows, cols, wake))
    }

    /// Run `program` (not the shell) on a terminal of its own, with `env` edits on top of the
    /// user's environment.
    pub fn spawn_cmd(
        cwd: &Path,
        program: &str,
        args: &[&str],
        env: &[EnvEdit],
        rows: usize,
        cols: usize,
        wake: Wake,
    ) -> std::io::Result<Term> {
        let (pty, reader) = Pty::spawn_cmd(cwd, program, args, env, rows, cols)?;
        Ok(Self::start(pty, reader, rows, cols, wake))
    }

    fn start(pty: Pty, mut reader: Box<dyn Read + Send>, rows: usize, cols: usize, wake: Wake) -> Term {
        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Vec::new()); // empty = the program exited
                        wake();
                        break;
                    }
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        wake();
                    }
                }
            }
        });
        Term {
            grid: Grid::new(rows, cols),
            parser: vte::Parser::new(),
            pty,
            rx,
            alive: true,
            scroll_offset: 0,
            started: Instant::now(),
            last_output: None,
            last_input: None,
            exit_code: None,
            sel: None,
            focus_sent: None,
        }
    }

    /// Feed pending output through the emulator; true if the screen should be redrawn (false
    /// while a synchronized-output hold is on: the program is still painting).
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(bytes) = self.rx.try_recv() {
            if bytes.is_empty() {
                self.alive = false;
            } else {
                self.ingest(&bytes);
                self.last_output = Some(Instant::now());
            }
            changed = true;
        }
        if self.exit_code.is_none() {
            self.exit_code = self.pty.try_wait();
            if self.exit_code.is_some() {
                self.alive = false;
                changed = true;
            }
        }
        changed && !self.grid.sync_held()
    }

    /// Run bytes through the emulator, keeping a scrolled-back view anchored on the same text.
    fn ingest(&mut self, bytes: &[u8]) {
        let before = self.grid.pushed;
        self.parser.advance(&mut self.grid, bytes);
        if !self.grid.replies.is_empty() {
            let replies = std::mem::take(&mut self.grid.replies);
            self.pty.write(&replies);
        }
        if self.scroll_offset > 0 {
            self.scroll_offset += self.grid.pushed - before;
        }
        self.scroll_offset = self.scroll_offset.min(self.grid.history_len());
    }

    /// Scroll the view; positive goes back into history.
    pub fn scroll_view(&mut self, delta: isize) {
        let max = self.grid.history_len();
        self.scroll_offset = if delta >= 0 {
            self.scroll_offset.saturating_add(delta as usize).min(max)
        } else {
            self.scroll_offset.saturating_sub(delta.unsigned_abs())
        };
    }

    #[cfg(test)]
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn alt_screen(&self) -> bool {
        self.grid.alt_screen()
    }

    pub fn paste_bytes(&self, text: &str) -> Vec<u8> {
        self.grid.paste_bytes(text)
    }

    /// Bytes typed or pasted by the user: the view snaps back to the live screen.
    pub fn write(&mut self, bytes: &[u8]) {
        self.scroll_offset = 0;
        self.last_input = Some(Instant::now());
        self.pty.write(bytes);
    }

    /// End the program (SIGHUP to its process group on Unix); its last screen stays readable.
    pub fn hangup(&mut self) {
        self.pty.hangup();
    }

    /// Name of the foreground process (the shell, or the command it is running), when known.
    pub fn foreground_name(&self) -> Option<String> {
        self.pty.foreground_name()
    }

    /// Command line of the job the shell is running in the foreground (`None` at the prompt, after
    /// the program ended, and always on Windows).
    pub fn foreground_job(&self) -> Option<String> {
        if !self.alive {
            return None;
        }
        self.pty.foreground_job()
    }

    /// Bytes the terminal itself sends (mouse / focus reports): no side effects on the view.
    pub fn send(&mut self, bytes: &[u8]) {
        self.pty.write(bytes);
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        if rows == self.grid.rows && cols == self.grid.cols {
            return;
        }
        self.grid.resize(rows, cols);
        self.scroll_offset = self.scroll_offset.min(self.grid.history_len());
        self.pty.resize(rows.max(1), cols.max(1));
    }

    /// Report a mouse event when the program asked for mouse reporting; true when it was sent.
    pub fn mouse(&mut self, ev: MouseEv, col: usize, row: usize, mods: (bool, bool, bool)) -> bool {
        match self.grid.mouse_report(ev, col, row, mods.0, mods.1, mods.2) {
            Some(b) => {
                self.send(&b);
                true
            }
            None => false,
        }
    }

    /// Tell a program that asked for focus events (`?1004`) that the focus moved.
    pub fn set_focus(&mut self, focused: bool) {
        if !self.grid.focus_events {
            self.focus_sent = None;
            return;
        }
        if self.focus_sent != Some(focused) {
            self.focus_sent = Some(focused);
            self.send(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// The selected text, one line per row (trailing blanks trimmed).
    pub fn selected_text(&self) -> String {
        self.sel.map(|s| self.grid.sel_text(s)).unwrap_or_default()
    }

    /// The hyperlink (OSC 8) under a screen cell of the current view.
    pub fn link_at(&self, row: usize, col: usize) -> Option<&str> {
        let id = self.grid.view_cell(row, col, self.scroll_offset).link;
        if id == 0 {
            return None;
        }
        self.grid.links.get(id as usize - 1).map(String::as_str)
    }
}

#[cfg(test)]
/// Bytes a key sends to the shell (None when the key has no terminal meaning).
pub fn key_bytes(named: Option<&str>, text: Option<&str>, ctrl: bool, alt: bool) -> Option<Vec<u8>> {
    key_bytes_app(named, text, ctrl, false, alt, false)
}

/// Bytes a key sends to the program, xterm style: modified arrows / Home / End / F-keys carry the
/// modifier parameter (`CSI 1;5D` is Ctrl+Left), Shift+Tab is `CSI Z`, Shift+Enter is `CSI 13;2u`
/// (what agent CLIs read as "new line"), and application cursor keys (`Grid::app_cursor`) are honored.
/// `named` is a winit `NamedKey` name ("Enter", "ArrowUp", "F5"...); `text` the typed text.
pub fn key_bytes_app(
    named: Option<&str>,
    text: Option<&str>,
    ctrl: bool,
    shift: bool,
    alt: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    if let Some(n) = named {
        let cursor = |x: char| -> Vec<u8> {
            if m > 1 {
                format!("\x1b[1;{m}{x}").into_bytes()
            } else if app_cursor {
                format!("\x1bO{x}").into_bytes()
            } else {
                format!("\x1b[{x}").into_bytes()
            }
        };
        let tilde = |k: u8| -> Vec<u8> {
            if m > 1 { format!("\x1b[{k};{m}~").into_bytes() } else { format!("\x1b[{k}~").into_bytes() }
        };
        let ss3 = |x: char| -> Vec<u8> {
            if m > 1 { format!("\x1b[1;{m}{x}").into_bytes() } else { format!("\x1bO{x}").into_bytes() }
        };
        let v: Vec<u8> = match n {
            "Enter" if shift && !ctrl && !alt => b"\x1b[13;2u".to_vec(),
            "Enter" if ctrl => format!("\x1b[13;{m}u").into_bytes(),
            "Enter" if alt => b"\x1b\r".to_vec(),
            "Enter" => b"\r".to_vec(),
            "Tab" if shift && !ctrl => b"\x1b[Z".to_vec(),
            "Tab" if alt => b"\x1b\t".to_vec(),
            "Tab" => b"\t".to_vec(),
            // Ctrl+Backspace deletes the previous word (^W) in shells and agent prompts alike.
            "Backspace" if ctrl => vec![0x17],
            "Backspace" if alt => b"\x1b\x7f".to_vec(),
            "Backspace" => vec![0x7f],
            "Escape" if alt => b"\x1b\x1b".to_vec(),
            "Escape" => vec![0x1b],
            "ArrowUp" => cursor('A'),
            "ArrowDown" => cursor('B'),
            "ArrowRight" => cursor('C'),
            "ArrowLeft" => cursor('D'),
            "Home" => cursor('H'),
            "End" => cursor('F'),
            "Insert" => tilde(2),
            "Delete" => tilde(3),
            "PageUp" => tilde(5),
            "PageDown" => tilde(6),
            "F1" => ss3('P'),
            "F2" => ss3('Q'),
            "F3" => ss3('R'),
            "F4" => ss3('S'),
            "F5" => tilde(15),
            "F6" => tilde(17),
            "F7" => tilde(18),
            "F8" => tilde(19),
            "F9" => tilde(20),
            "F10" => tilde(21),
            "F11" => tilde(23),
            "F12" => tilde(24),
            "Space" if ctrl => vec![0],
            "Space" if alt => b"\x1b ".to_vec(),
            "Space" => b" ".to_vec(),
            _ => return None,
        };
        return Some(v);
    }
    let t = text?;
    if ctrl {
        let c = t.chars().next()?;
        let lc = c.to_ascii_lowercase();
        let code = if lc.is_ascii_lowercase() {
            lc as u8 - b'a' + 1
        } else {
            match c {
                '@' | ' ' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '/' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return None,
            }
        };
        return Some(if alt { vec![0x1b, code] } else { vec![code] });
    }
    let mut v = Vec::new();
    if alt {
        v.push(0x1b);
    }
    v.extend_from_slice(t.as_bytes());
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(g: &mut Grid, s: &str) {
        let mut p = vte::Parser::new();
        p.advance(g, s.as_bytes());
    }

    #[test]
    fn text_wraps_scrolls_and_colors() {
        let mut g = Grid::new(3, 5);
        feed(&mut g, "ab\r\n\x1b[31mred\x1b[0m!\r\nxyz123");
        // "xyz123" does not fit in 5 columns: the 6th char wraps and scrolls the screen.
        assert_eq!(g.row_text(0), "red!");
        assert_eq!(g.row_text(1), "xyz12");
        assert_eq!(g.row_text(2), "3");
        assert_eq!(g.at(0, 0).fg, Some(PALETTE[1]));
        assert_eq!(g.at(0, 3).fg, None, "reset ends the color");
        feed(&mut g, "\r\nnext");
        assert_eq!(g.row_text(0), "xyz12");
        assert_eq!(g.row_text(1), "3");
        assert_eq!(g.row_text(2), "next");
    }

    #[test]
    fn cursor_moves_and_erase() {
        let mut g = Grid::new(4, 10);
        feed(&mut g, "hello world\x1b[2;3Hxx");
        assert_eq!(g.row_text(0), "hello worl");
        assert_eq!(g.row_text(1), "d xx", "CUP puts the cursor at row 2, column 3");
        assert_eq!((g.cy, g.cx), (1, 4));
        feed(&mut g, "\x1b[1;1H\x1b[K");
        assert_eq!(g.row_text(0), "", "erase to end of line");
        feed(&mut g, "\x1b[2J\x1b[Hok");
        assert_eq!(g.row_text(0), "ok");
        assert_eq!(g.row_text(1), "");
        feed(&mut g, "\x1b[3D\x1b[P");
        assert_eq!(g.row_text(0), "k", "delete char shifts left");
    }

    #[test]
    fn scroll_region_and_256_color() {
        let mut g = Grid::new(4, 4);
        feed(&mut g, "1\r\n2\r\n3\r\n4");
        feed(&mut g, "\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(g.row_text(0), "1", "outside the region stays");
        assert_eq!(g.row_text(1), "3", "region scrolled");
        assert_eq!(g.row_text(3), "4");
        feed(&mut g, "\x1b[r\x1b[38;5;196mX");
        // Resetting the region homes the cursor, so X lands at the top-left, in xterm-256 red.
        assert_eq!(g.at(0, 0).ch, 'X');
        assert_eq!(g.at(0, 0).fg, Some(Rgba(255, 0, 0, 255)));
    }

    #[test]
    fn resize_keeps_the_bottom_when_shrinking() {
        let mut g = Grid::new(4, 6);
        feed(&mut g, "a\r\nb\r\nc\r\nprompt");
        g.resize(2, 6);
        assert_eq!(g.row_text(0), "c");
        assert_eq!(g.row_text(1), "prompt");
    }

    #[test]
    fn keys_map_to_bytes() {
        assert_eq!(key_bytes(Some("Enter"), None, false, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Some("ArrowUp"), None, false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(None, Some("c"), true, false), Some(vec![3]));
        assert_eq!(key_bytes(None, Some("é"), false, false), Some("é".as_bytes().to_vec()));
        assert_eq!(key_bytes(None, Some("b"), false, true), Some(vec![0x1b, b'b']));
    }

    #[test]
    fn shift_enter_shift_tab_and_modified_keys() {
        let k = |n: &str, ctrl: bool, shift: bool, alt: bool| key_bytes_app(Some(n), None, ctrl, shift, alt, false);
        assert_eq!(k("Enter", false, true, false), Some(b"\x1b[13;2u".to_vec()), "Shift+Enter = new line for agents");
        assert_eq!(k("Enter", true, false, false), Some(b"\x1b[13;5u".to_vec()));
        assert_eq!(k("Enter", false, false, true), Some(b"\x1b\r".to_vec()));
        assert_eq!(k("Tab", false, true, false), Some(b"\x1b[Z".to_vec()), "Shift+Tab is back-tab");
        assert_eq!(k("ArrowLeft", true, false, false), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(k("ArrowRight", false, true, false), Some(b"\x1b[1;2C".to_vec()));
        assert_eq!(k("ArrowUp", false, false, true), Some(b"\x1b[1;3A".to_vec()));
        assert_eq!(k("Home", true, true, false), Some(b"\x1b[1;6H".to_vec()));
        assert_eq!(k("End", false, true, false), Some(b"\x1b[1;2F".to_vec()));
        assert_eq!(k("Delete", true, false, false), Some(b"\x1b[3;5~".to_vec()));
        assert_eq!(k("PageUp", false, true, false), Some(b"\x1b[5;2~".to_vec()));
        assert_eq!(k("PageDown", false, false, false), Some(b"\x1b[6~".to_vec()));
        assert_eq!(k("F1", false, false, false), Some(b"\x1bOP".to_vec()));
        assert_eq!(k("F1", true, false, false), Some(b"\x1b[1;5P".to_vec()));
        assert_eq!(k("F5", false, false, false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(k("F12", false, true, false), Some(b"\x1b[24;2~".to_vec()));
        assert_eq!(k("Backspace", true, false, false), Some(vec![0x17]));
        assert_eq!(k("Escape", false, false, false), Some(vec![0x1b]));
        // Modifiers win over application cursor mode (SS3 cannot carry them).
        assert_eq!(key_bytes_app(Some("ArrowLeft"), None, true, false, false, true), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(key_bytes_app(None, Some("j"), true, false, false, false), Some(vec![0x0a]), "Ctrl+J is a newline");
        assert_eq!(key_bytes_app(None, Some("b"), true, false, false, false), Some(vec![0x02]));
        assert_eq!(key_bytes_app(None, Some("x"), true, false, true, false), Some(vec![0x1b, 0x18]));
    }

    #[test]
    fn prefixed_csi_sequences_are_ignored() {
        let mut g = Grid::new(3, 10);
        feed(&mut g, "ab\x1b[s");
        feed(&mut g, "\x1b[>4;2m");
        assert_eq!(g.attr.flags, 0, "modifyOtherKeys must not turn on bold / dim");
        feed(&mut g, "\x1b[2;5H\x1b[>1u");
        assert_eq!((g.cy, g.cx), (1, 4), "kitty keyboard push is not restore-cursor");
        feed(&mut g, "\x1b[<u\x1b[=1;1u\x1b[?u");
        assert_eq!((g.cy, g.cx), (1, 4), "kitty pop / set / query leave the cursor alone");
        assert_eq!(g.replies, b"\x1b[?1u".to_vec(), "the kitty query answers with the flags in effect");
        feed(&mut g, "x");
        assert_eq!(g.at(1, 4).flags, 0);
        feed(&mut g, "\x1b#8");
        assert_eq!((g.cy, g.cx), (1, 5), "ESC # 8 is not restore-cursor either");
    }

    #[test]
    fn kitty_keyboard_flags_per_screen() {
        let mut g = Grid::new(3, 10);
        feed(&mut g, "\x1b[?u");
        assert_eq!(g.replies, b"\x1b[?0u".to_vec(), "nothing pushed: legacy encoding");
        g.replies.clear();
        feed(&mut g, "\x1b[>1u");
        assert_eq!(g.kitty_flags(), 1);
        // A full-screen program pushes its own flags on the alternate screen; leaving it restores ours.
        feed(&mut g, "\x1b[?1049h");
        assert_eq!(g.kitty_flags(), 0, "the alternate screen starts with its own empty stack");
        feed(&mut g, "\x1b[>11u\x1b[?u");
        assert_eq!(g.replies, b"\x1b[?9u".to_vec(), "flag 2 (event types) is not implemented");
        g.replies.clear();
        feed(&mut g, "\x1b[=16;2u");
        assert_eq!(g.kitty_flags(), 25);
        feed(&mut g, "\x1b[?1049l");
        assert_eq!(g.kitty_flags(), 1);
        feed(&mut g, "\x1b[<u");
        assert_eq!(g.kitty_flags(), 0);
        feed(&mut g, "\x1b[>9u\x1bc");
        assert_eq!(g.kitty_flags(), 0, "a full reset (RIS) drops the flags");
    }

    #[test]
    fn sgr_attributes_set_and_reset() {
        let mut g = Grid::new(2, 20);
        feed(&mut g, "\x1b[1;2;3;4;9mA\x1b[22mB\x1b[23;24mC\x1b[29mD\x1b[7;8mE\x1b[27;28mF");
        let f = |g: &Grid, c: usize| g.at(0, c).flags;
        assert_eq!(f(&g, 0), BOLD | DIM | ITALIC | UNDERLINE | STRIKE);
        assert_eq!(f(&g, 1), ITALIC | UNDERLINE | STRIKE, "22 ends bold and dim");
        assert_eq!(f(&g, 2), STRIKE);
        assert_eq!(f(&g, 3), 0);
        assert_eq!(f(&g, 4), INVERSE | HIDDEN);
        assert_eq!(f(&g, 5), 0);
        feed(&mut g, "\x1b[4:3mG\x1b[4:0mH\x1b[1mI\x1b[0mJ");
        assert_eq!(f(&g, 6), UNDERLINE, "curly underline reads as underline");
        assert_eq!(f(&g, 7), 0);
        assert_eq!(f(&g, 8), BOLD);
        assert_eq!(f(&g, 9), 0);
    }

    #[test]
    fn truecolor_in_both_notations() {
        let mut g = Grid::new(2, 10);
        feed(&mut g, "\x1b[38;2;10;20;30mA\x1b[38:2::40:50:60mB\x1b[48:2:1:2:3mC\x1b[38:5:196mD");
        assert_eq!(g.at(0, 0).fg, Some(Rgba(10, 20, 30, 255)));
        assert_eq!(g.at(0, 1).fg, Some(Rgba(40, 50, 60, 255)), "colon form with an empty colorspace");
        assert_eq!(g.at(0, 2).bg, Some(Rgba(1, 2, 3, 255)), "colon form without a colorspace");
        assert_eq!(g.at(0, 3).fg, Some(Rgba(255, 0, 0, 255)));
        // Underline color (58) arguments are consumed, not read as attributes (2 = dim, 5...).
        feed(&mut g, "\x1b[0;58;2;2;5;9mE");
        assert_eq!(g.at(0, 4).flags, 0);
    }

    #[test]
    fn wide_and_combining_characters() {
        let mut g = Grid::new(2, 6);
        feed(&mut g, "a日b");
        assert_eq!(g.at(0, 1).ch, '日');
        assert!(g.at(0, 1).has(WIDE));
        assert_eq!(g.at(0, 2).ch, WIDE_TAIL);
        assert_eq!(g.at(0, 3).ch, 'b');
        assert_eq!(g.cx, 4, "a wide char takes two columns");
        feed(&mut g, "e\u{301}");
        assert_eq!(g.at(0, 4).ch, 'e');
        assert_eq!(g.cx, 5, "a combining mark takes none");
        feed(&mut g, "語");
        assert_eq!(g.row_text(0), "a日be", "no room at the end of the line: it wraps");
        assert_eq!(g.at(1, 0).ch, '語');
        // Overwriting half of a wide char blanks the other half.
        feed(&mut g, "\x1b[1;3Hx");
        assert_eq!(g.at(0, 1).ch, ' ');
        assert!(!g.at(0, 1).has(WIDE));
        assert_eq!(g.row_text(0), "a xbe");
    }

    #[test]
    fn synchronized_output_holds_and_reports() {
        let mut g = Grid::new(2, 6);
        feed(&mut g, "\x1b[?2026$p");
        assert_eq!(g.replies, b"\x1b[?2026;2$y".to_vec(), "supported, currently reset");
        g.replies.clear();
        feed(&mut g, "\x1b[?2026h");
        assert!(g.sync_held());
        assert!(matches!(g.sync_check(), (false, Some(_))));
        feed(&mut g, "\x1b[?2026$p");
        assert_eq!(g.replies, b"\x1b[?2026;1$y".to_vec());
        feed(&mut g, "\x1b[?2026l");
        assert!(!g.sync_held());
        g.replies.clear();
        feed(&mut g, "\x1b[?9999$p");
        assert_eq!(g.replies, b"\x1b[?9999;0$y".to_vec(), "unknown mode");
        g.sync_since = Some(Instant::now() - SYNC_MAX - Duration::from_millis(1));
        assert!(!g.sync_held(), "a forgotten hold expires");
        assert_eq!(g.sync_check(), (true, None), "and the screen gets drawn");
        assert_eq!(g.sync_check(), (false, None));
    }

    #[test]
    fn osc_title_hyperlink_clipboard_and_notifications() {
        let mut g = Grid::new(2, 20);
        feed(&mut g, "\x1b]0;✳ Claude Code\x07");
        assert_eq!(g.title, "✳ Claude Code");
        feed(&mut g, "\x1b]2;build: ok\x1b\\");
        assert_eq!(g.title, "build: ok");
        feed(&mut g, "\x1b]8;;https://example.com/a;b\x1b\\link\x1b]8;;\x1b\\ no");
        let id = g.at(0, 0).link;
        assert!(id > 0);
        assert_eq!(g.links[id as usize - 1], "https://example.com/a;b");
        assert_eq!(g.at(0, 3).link, id);
        assert_eq!(g.at(0, 5).link, 0, "the link ended");
        feed(&mut g, "\x1b]52;c;aGVsbG8gd29ybGQ=\x07");
        assert_eq!(g.clipboard.as_deref(), Some("hello world"));
        g.clipboard = None;
        feed(&mut g, "\x1b]52;c;?\x07");
        assert_eq!(g.clipboard, None, "clipboard queries are not answered");
        feed(&mut g, "\x1b]9;Claude needs your permission\x07\x1b]9;4;1;50\x07");
        feed(&mut g, "\x1b]777;notify;Done;All tests pass\x07");
        assert_eq!(
            g.notices,
            vec![
                (String::new(), "Claude needs your permission".to_string()),
                ("Done".to_string(), "All tests pass".to_string())
            ]
        );
        feed(&mut g, "\x07\x07");
        assert_eq!(g.bells, 2);
        g.colors = (Rgba(0xff, 0xff, 0xff, 255), Rgba(0x10, 0x20, 0x30, 255));
        feed(&mut g, "\x1b]11;?\x07");
        assert_eq!(g.replies, b"\x1b]11;rgb:1010/2020/3030\x07".to_vec());
    }

    #[test]
    fn mouse_reporting_encodings() {
        let mut g = Grid::new(10, 40);
        assert_eq!(g.mouse_report(MouseEv::Press(0), 0, 0, false, false, false), None, "off by default");
        feed(&mut g, "\x1b[?1000h\x1b[?1006h");
        assert_eq!(g.mouse_report(MouseEv::Press(0), 4, 2, false, false, false), Some(b"\x1b[<0;5;3M".to_vec()));
        assert_eq!(g.mouse_report(MouseEv::Release(0), 4, 2, false, false, false), Some(b"\x1b[<0;5;3m".to_vec()));
        assert_eq!(g.mouse_report(MouseEv::Drag(0), 5, 2, false, false, false), None, "1000 has no drags");
        assert_eq!(g.mouse_report(MouseEv::Wheel(true), 1, 1, false, false, true), Some(b"\x1b[<80;2;2M".to_vec()));
        feed(&mut g, "\x1b[?1002h");
        assert_eq!(g.mouse_report(MouseEv::Drag(0), 5, 2, true, false, false), Some(b"\x1b[<36;6;3M".to_vec()));
        feed(&mut g, "\x1b[?1006l");
        assert_eq!(
            g.mouse_report(MouseEv::Press(2), 0, 0, false, false, false),
            Some(vec![0x1b, b'[', b'M', 34, 33, 33])
        );
        assert_eq!(
            g.mouse_report(MouseEv::Release(2), 0, 0, false, false, false),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        feed(&mut g, "\x1b[?1002l");
        assert_eq!(g.mouse_mode, 0);
        // Wheel without mouse reporting on the alternate screen: arrow keys.
        assert_eq!(g.wheel_keys(true, 3), b"\x1b[A\x1b[A\x1b[A".to_vec());
        feed(&mut g, "\x1b[?1h");
        assert_eq!(g.wheel_keys(false, 1), b"\x1bOB".to_vec());
    }

    #[test]
    fn focus_events_and_cursor_shape_and_insert_mode() {
        let mut g = Grid::new(2, 8);
        assert!(!g.focus_events);
        feed(&mut g, "\x1b[?1004h");
        assert!(g.focus_events);
        feed(&mut g, "\x1b[5 q");
        assert_eq!(g.cursor_shape, CursorShape::Bar);
        feed(&mut g, "\x1b[4 q");
        assert_eq!(g.cursor_shape, CursorShape::Underline);
        feed(&mut g, "\x1b[0 q");
        assert_eq!(g.cursor_shape, CursorShape::Block);
        feed(&mut g, "abc\x1b[1;2H\x1b[4hX\x1b[4lY");
        assert_eq!(g.row_text(0), "aXYc", "insert pushes right, replace overwrites");
        feed(&mut g, "\x1b[2;1H\x1b(0lqk\x1b(Bq");
        assert_eq!(g.row_text(1), "┌─┐q", "DEC line drawing");
    }

    #[test]
    fn selection_survives_scrolling_and_copies_text() {
        let mut g = Grid::new(3, 10);
        feed(&mut g, "one\r\ntwo 日本\r\nthree");
        let mut sel = Sel { anchor: (g.abs_row(1, 0), 4), head: (g.abs_row(0, 0), 1) };
        assert_eq!(sel.ordered().0, (g.abs_row(0, 0), 1));
        assert!(sel.contains(g.abs_row(0, 0), 2) && !sel.contains(g.abs_row(2, 0), 0));
        feed(&mut g, "\r\nfour\r\nfive");
        // The lines moved into history; their absolute numbers did not change.
        assert_eq!(g.abs_cell(sel.anchor.0, 4).ch, '日');
        sel.head = (sel.head.0, 0);
        let t = g.sel_text(sel);
        assert_eq!(t, "one\ntwo 日");
    }

    #[test]
    fn real_shell_through_a_pty() {
        let wake: Wake = std::sync::Arc::new(|| {});
        let mut t = Term::spawn(&std::env::temp_dir(), 10, 60, wake).unwrap();
        if cfg!(windows) {
            // PowerShell takes a while to print its first prompt through ConPTY.
            let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while std::time::Instant::now() < end && (0..t.grid.rows).all(|r| t.grid.row_text(r).is_empty()) {
                t.pump();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        t.write(b"echo lumen-pty-$((20+22))\r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(if cfg!(windows) { 30 } else { 5 });
        let mut found = false;
        while std::time::Instant::now() < deadline && !found {
            t.pump();
            found = (0..t.grid.rows).any(|r| t.grid.row_text(r).contains("lumen-pty-42"));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(found, "rows: {:?}", (0..t.grid.rows).map(|r| t.grid.row_text(r)).collect::<Vec<_>>());
        t.resize(5, 40);
        assert_eq!((t.grid.rows, t.grid.cols), (5, 40));
    }

    #[test]
    #[cfg(unix)]
    fn spawned_program_reports_its_exit_code() {
        let wake: Wake = std::sync::Arc::new(|| {});
        let mut t =
            Term::spawn_cmd(&std::env::temp_dir(), "sh", &["-c", "printf ready; exit 7"], &[], 5, 30, wake).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < end && (t.alive || t.grid.row_text(0).is_empty()) {
            t.pump();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!t.alive);
        assert_eq!(t.grid.row_text(0), "ready");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while t.exit_code.is_none() && std::time::Instant::now() < end {
            t.pump();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(t.exit_code, Some(7));
    }

    fn lines(g: &Grid, off: usize) -> Vec<String> {
        (0..g.rows).map(|r| g.view_row_text(r, off)).collect()
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\r\n")
    }

    fn test_term(rows: usize, cols: usize) -> Term {
        let wake: Wake = std::sync::Arc::new(|| {});
        Term::spawn(&std::env::temp_dir(), rows, cols, wake).unwrap()
    }

    #[test]
    fn scrolled_lines_enter_history_and_are_viewable() {
        let mut g = Grid::new(3, 6);
        feed(&mut g, &numbered(6));
        assert_eq!(g.history_len(), 3);
        assert_eq!(lines(&g, 0), ["l4", "l5", "l6"]);
        assert_eq!(lines(&g, 3), ["l1", "l2", "l3"]);
        assert_eq!(lines(&g, 1), ["l3", "l4", "l5"]);
        assert_eq!(g.view_cell(0, 1, 3).ch, '1');
        assert_eq!(g.view_cell(0, 5, 3), Cell::default(), "beyond stored width is blank");
        assert_eq!(lines(&g, 99), lines(&g, 3), "offset is clamped");
    }

    #[test]
    fn history_is_bounded() {
        let mut g = Grid::new(2, 5);
        g.max_history = 4;
        feed(&mut g, &numbered(20));
        assert_eq!(g.history_len(), 4);
        assert_eq!(g.view_row_text(0, 4), "l15");
    }

    #[test]
    fn partial_scroll_regions_do_not_record_history() {
        let mut g = Grid::new(4, 4);
        feed(&mut g, "1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[3;1H\n\n\n");
        assert_eq!(g.history_len(), 0);
        feed(&mut g, "\x1b[r\x1b[1;1H\x1b[M\x1b[S");
        assert_eq!(g.history_len(), 0, "delete-line and CSI S do not feed scrollback");
    }

    #[test]
    fn scrollback_colors_survive_and_csi_3j_clears() {
        let mut g = Grid::new(2, 5);
        feed(&mut g, "\x1b[31mred\x1b[0m\r\na\r\nb");
        assert_eq!(g.view_cell(0, 0, 1).fg, Some(PALETTE[1]));
        feed(&mut g, "\x1b[3J");
        assert_eq!(g.history_len(), 0);
        assert_eq!(g.row_text(1), "b", "screen untouched");
    }

    #[test]
    fn view_offset_clamps_and_stays_stable_under_output() {
        let mut t = test_term(3, 8);
        t.ingest(numbered(6).as_bytes());
        t.scroll_view(2);
        assert_eq!(t.scroll_offset, 2);
        assert_eq!(lines(&t.grid, t.scroll_offset), ["l2", "l3", "l4"]);
        t.ingest(b"\r\nl7\r\nl8");
        assert_eq!(t.scroll_offset, 4, "offset grows by the lines that scrolled away");
        assert_eq!(lines(&t.grid, t.scroll_offset), ["l2", "l3", "l4"], "same text on screen");
        t.scroll_view(1000);
        assert_eq!(t.scroll_offset, t.grid.history_len());
        t.scroll_view(-1);
        assert_eq!(t.scroll_offset, t.grid.history_len() - 1);
        t.scroll_view(-1000);
        assert_eq!(t.scroll_offset, 0);
        t.ingest(b"\r\nl9");
        assert_eq!(t.scroll_offset, 0, "live view keeps following");
    }

    #[test]
    fn typing_snaps_back_to_the_bottom() {
        let mut t = test_term(3, 8);
        t.ingest(numbered(8).as_bytes());
        t.scroll_view(3);
        assert_eq!(t.scroll_offset, 3);
        t.write(b"x");
        assert_eq!(t.scroll_offset, 0);
        t.scroll_view(2);
        t.scroll_to_bottom();
        assert_eq!(t.scroll_offset, 0);
    }

    #[test]
    fn resize_keeps_history_sane() {
        let mut t = test_term(4, 10);
        t.ingest(numbered(10).as_bytes());
        t.scroll_view(100);
        t.resize(2, 4);
        assert!(t.scroll_offset <= t.grid.history_len());
        let _ = lines(&t.grid, t.scroll_offset);
        t.resize(8, 30);
        let _ = lines(&t.grid, t.scroll_offset);
        assert_eq!(t.grid.view_row_text(0, 0), t.grid.row_text(0));
    }

    #[test]
    fn alt_screen_saves_and_restores_main_exactly() {
        let mut g = Grid::new(3, 8);
        feed(&mut g, "one\r\ntwo\x1b[31m");
        let (cells, cx, cy) = (g.cells.clone(), g.cx, g.cy);
        feed(&mut g, "\x1b[?1049h");
        assert!(g.alt_screen());
        assert_eq!(g.row_text(0), "");
        assert_eq!(g.row_text(1), "");
        feed(&mut g, "\x1b[2;2Hvim\x1b[?1049l");
        assert!(!g.alt_screen());
        assert_eq!(g.cells, cells);
        assert_eq!((g.cx, g.cy), (cx, cy));
        feed(&mut g, "!");
        assert_eq!(g.at(1, 3).fg, Some(PALETTE[1]), "attributes restored too");
    }

    #[test]
    fn alt_screen_variants_and_no_history_growth() {
        for (on, off) in [("\x1b[?47h", "\x1b[?47l"), ("\x1b[?1047h", "\x1b[?1047l")] {
            let mut g = Grid::new(2, 5);
            feed(&mut g, "a\r\nb\r\nc");
            let h = g.history_len();
            assert_eq!(h, 1);
            feed(&mut g, on);
            assert_eq!(g.history_len(), 0, "no scrollback while alternate");
            feed(&mut g, &numbered(10));
            feed(&mut g, off);
            assert_eq!(g.history_len(), h, "nothing leaked into history");
            assert_eq!((g.row_text(0), g.row_text(1)), ("b".into(), "c".into()));
        }
    }

    #[test]
    fn resize_during_alt_screen_keeps_both_screens_consistent() {
        let mut g = Grid::new(4, 6);
        feed(&mut g, "main\x1b[?1049hALT");
        g.resize(3, 5);
        assert_eq!(g.row_text(0), "ALT");
        feed(&mut g, "\x1b[?1049l");
        assert_eq!((g.rows, g.cols), (3, 5));
        assert_eq!(g.row_text(0), "main");
        assert_eq!(g.cells.len(), 15);
    }

    #[test]
    fn app_cursor_mode_and_keys() {
        let mut g = Grid::new(2, 5);
        assert!(!g.app_cursor);
        feed(&mut g, "\x1b[?1h");
        assert!(g.app_cursor);
        let app = |n: &str, on: bool| key_bytes_app(Some(n), None, false, false, false, on);
        assert_eq!(app("ArrowUp", g.app_cursor), Some(b"\x1bOA".to_vec()));
        assert_eq!(app("ArrowLeft", true), Some(b"\x1bOD".to_vec()));
        assert_eq!(app("Home", true), Some(b"\x1bOH".to_vec()));
        assert_eq!(app("End", true), Some(b"\x1bOF".to_vec()));
        assert_eq!(app("PageUp", true), Some(b"\x1b[5~".to_vec()));
        assert_eq!(app("ArrowDown", false), Some(b"\x1b[B".to_vec()));
        assert_eq!(key_bytes(Some("ArrowRight"), None, false, false), Some(b"\x1b[C".to_vec()));
        feed(&mut g, "\x1b[?1l");
        assert!(!g.app_cursor);
    }

    #[test]
    fn bracketed_paste_wraps_only_when_enabled() {
        let mut g = Grid::new(2, 5);
        assert_eq!(g.paste_bytes("hi"), b"hi".to_vec());
        feed(&mut g, "\x1b[?2004h");
        assert!(g.bracketed_paste);
        assert_eq!(g.paste_bytes("a\nb"), b"\x1b[200~a\nb\x1b[201~".to_vec());
        feed(&mut g, "\x1b[?2004l");
        assert_eq!(g.paste_bytes("hi"), b"hi".to_vec());
    }

    #[test]
    fn multiple_private_modes_in_one_sequence() {
        let mut g = Grid::new(2, 5);
        feed(&mut g, "\x1b[?1;25;2004l");
        assert!(!g.cursor_visible && !g.app_cursor && !g.bracketed_paste);
        feed(&mut g, "\x1b[?25;2004h");
        assert!(g.cursor_visible && g.bracketed_paste);
    }

    #[test]
    #[cfg(unix)]
    fn real_pty_output_lands_in_scrollback() {
        let mut t = test_term(10, 40);
        t.write(b"for i in $(seq 1 60); do echo L$i; done\r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut done = false;
        while std::time::Instant::now() < deadline && !done {
            t.pump();
            done = (0..t.grid.rows).any(|r| t.grid.row_text(r) == "L60");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(done, "rows: {:?}", lines(&t.grid, 0));
        assert!(t.grid.history_len() >= 50, "history {}", t.grid.history_len());
        t.scroll_view(isize::MAX);
        let mut seen = false;
        let mut off = t.scroll_offset;
        loop {
            if (0..t.grid.rows).any(|r| t.grid.view_row_text(r, off) == "L3") {
                seen = true;
                break;
            }
            if off == 0 {
                break;
            }
            off = off.saturating_sub(5);
        }
        assert!(seen, "an early line is readable when scrolled back");
    }
}
