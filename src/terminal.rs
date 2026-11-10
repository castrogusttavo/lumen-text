//! Integrated terminal: a PTY running the user's shell plus a small VT100/xterm emulator
//! (cursor movement, erase, scroll regions, SGR colors) over `vte`.

use crate::{
    lsp::Wake,
    pty::Pty,
    tasks::{PALETTE, xterm256},
    theme::Rgba,
};
use std::{
    collections::VecDeque,
    io::Read,
    path::Path,
    sync::mpsc::{Receiver, channel},
    thread,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub bold: bool,
    pub inverse: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: None, bg: None, bold: false, inverse: false }
    }
}

/// Default scrollback size (lines).
pub const DEFAULT_HISTORY: usize = 5_000;

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
        }
    }

    pub fn at(&self, r: usize, c: usize) -> Cell {
        self.cells[r * self.cols + c]
    }

    #[cfg(test)]
    pub fn row_text(&self, r: usize) -> String {
        (0..self.cols).map(|c| self.at(r, c).ch).collect::<String>().trim_end().to_string()
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

    #[cfg(test)]
    pub fn view_row_text(&self, row: usize, offset: usize) -> String {
        (0..self.cols).map(|c| self.view_cell(row, c, offset).ch).collect::<String>().trim_end().to_string()
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

    fn put(&mut self, ch: char) {
        if self.wrap_pending {
            self.cx = 0;
            self.linefeed();
            self.wrap_pending = false;
        }
        let i = self.cy * self.cols + self.cx;
        self.cells[i] = Cell { ch, ..self.attr };
        if self.cx + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.cx += 1;
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

    fn sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        let ps: Vec<u16> = if params.is_empty() { vec![0] } else { params.to_vec() };
        while i < ps.len() {
            match ps[i] {
                0 => self.attr = Cell::default(),
                1 => self.attr.bold = true,
                22 => self.attr.bold = false,
                7 => self.attr.inverse = true,
                27 => self.attr.inverse = false,
                n @ 30..=37 => self.attr.fg = Some(PALETTE[(n - 30) as usize]),
                n @ 90..=97 => self.attr.fg = Some(PALETTE[(n - 90 + 8) as usize]),
                n @ 40..=47 => self.attr.bg = Some(PALETTE[(n - 40) as usize]),
                n @ 100..=107 => self.attr.bg = Some(PALETTE[(n - 100 + 8) as usize]),
                39 => self.attr.fg = None,
                49 => self.attr.bg = None,
                n @ (38 | 48) => {
                    let color = match ps.get(i + 1) {
                        Some(5) => {
                            let c = ps.get(i + 2).map(|&v| xterm256(v as u32));
                            i += 2;
                            c
                        }
                        Some(2) => {
                            let c = match (ps.get(i + 2), ps.get(i + 3), ps.get(i + 4)) {
                                (Some(&r), Some(&g), Some(&b)) => Some(Rgba(r as u8, g as u8, b as u8, 255)),
                                _ => None,
                            };
                            i += 4;
                            c
                        }
                        _ => None,
                    };
                    if n == 38 { self.attr.fg = color } else { self.attr.bg = color }
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
        let p: Vec<u16> = params.iter().map(|s| s.first().copied().unwrap_or(0)).collect();
        let get = |i: usize, d: usize| p.get(i).copied().filter(|v| *v != 0).map_or(d, |v| v as usize);
        if inter == b"?" {
            if matches!(action, 'h' | 'l') {
                let on = action == 'h';
                for &mode in &p {
                    match mode {
                        1 => self.app_cursor = on,
                        25 => self.cursor_visible = on,
                        2004 => self.bracketed_paste = on,
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
            return;
        }
        match action {
            // ConPTY (Windows) waits for the answer to its cursor query before it prints anything.
            'n' if p.first() == Some(&6) => {
                self.replies.extend(format!("\x1b[{};{}R", self.cy + 1, self.cx + 1).into_bytes());
            }
            'n' if p.first() == Some(&5) => self.replies.extend_from_slice(b"\x1b[0n"),
            'c' if inter.is_empty() && p.first().is_none_or(|v| *v == 0) => {
                self.replies.extend_from_slice(b"\x1b[?1;2c")
            }
            'A' => self.move_to(self.cy.saturating_sub(get(0, 1)), self.cx),
            'B' | 'e' => self.move_to(self.cy + get(0, 1), self.cx),
            'C' | 'a' => self.move_to(self.cy, self.cx + get(0, 1)),
            'D' => self.move_to(self.cy, self.cx.saturating_sub(get(0, 1))),
            'E' => self.move_to(self.cy + get(0, 1), 0),
            'F' => self.move_to(self.cy.saturating_sub(get(0, 1)), 0),
            'G' | '`' => self.move_to(self.cy, get(0, 1) - 1),
            'H' | 'f' => self.move_to(get(0, 1) - 1, get(1, 1) - 1),
            'd' => self.move_to(get(0, 1) - 1, self.cx),
            'J' => {
                let here = self.cy * self.cols + self.cx;
                match p.first().copied().unwrap_or(0) {
                    0 => self.erase_range(here, self.cells.len()),
                    1 => self.erase_range(0, here + 1),
                    3 => self.history.clear(),
                    _ => self.erase_range(0, self.cells.len()),
                }
            }
            'K' => {
                let (row, here) = (self.cy * self.cols, self.cy * self.cols + self.cx);
                match p.first().copied().unwrap_or(0) {
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
            'm' => self.sgr(&p),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _inter: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved = (self.cy, self.cx),
            b'8' => self.move_to(self.saved.0, self.saved.1),
            b'M' => {
                if self.cy == self.top {
                    self.scroll_down(1);
                } else {
                    self.cy -= 1;
                }
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.cx = 0;
                self.linefeed();
            }
            b'c' => {
                let history = std::mem::take(&mut self.history);
                let (pushed, max) = (self.pushed, self.max_history);
                *self = Grid::new(self.rows, self.cols);
                self.history = history;
                self.pushed = pushed;
                self.max_history = max;
            }
            _ => {}
        }
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
}

impl Term {
    pub fn spawn(cwd: &Path, rows: usize, cols: usize, wake: Wake) -> std::io::Result<Term> {
        let (pty, mut reader) = Pty::spawn(cwd, rows, cols)?;
        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Vec::new()); // empty = shell exited
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
        Ok(Term { grid: Grid::new(rows, cols), parser: vte::Parser::new(), pty, rx, alive: true, scroll_offset: 0 })
    }

    /// Feed pending output through the emulator; true if anything changed.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(bytes) = self.rx.try_recv() {
            if bytes.is_empty() {
                self.alive = false;
            } else {
                self.ingest(&bytes);
            }
            changed = true;
        }
        changed
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

    pub fn write(&mut self, bytes: &[u8]) {
        self.scroll_offset = 0;
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
}

#[cfg(test)]
/// Bytes a key sends to the shell (None when the key has no terminal meaning).
pub fn key_bytes(named: Option<&str>, text: Option<&str>, ctrl: bool, alt: bool) -> Option<Vec<u8>> {
    key_bytes_app(named, text, ctrl, alt, false)
}

/// Like `key_bytes`, honoring application cursor keys (`Grid::app_cursor`).
pub fn key_bytes_app(
    named: Option<&str>,
    text: Option<&str>,
    ctrl: bool,
    alt: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    if let Some(n) = named {
        if app_cursor {
            let s: Option<&[u8]> = match n {
                "ArrowUp" => Some(b"\x1bOA"),
                "ArrowDown" => Some(b"\x1bOB"),
                "ArrowRight" => Some(b"\x1bOC"),
                "ArrowLeft" => Some(b"\x1bOD"),
                "Home" => Some(b"\x1bOH"),
                "End" => Some(b"\x1bOF"),
                _ => None,
            };
            if let Some(s) = s {
                return Some(s.to_vec());
            }
        }
        let s: &[u8] = match n {
            "Enter" => b"\r",
            "Backspace" => b"\x7f",
            "Tab" => b"\t",
            "Escape" => b"\x1b",
            "ArrowUp" => b"\x1b[A",
            "ArrowDown" => b"\x1b[B",
            "ArrowRight" => b"\x1b[C",
            "ArrowLeft" => b"\x1b[D",
            "Home" => b"\x1b[H",
            "End" => b"\x1b[F",
            "PageUp" => b"\x1b[5~",
            "PageDown" => b"\x1b[6~",
            "Delete" => b"\x1b[3~",
            "Space" => {
                if ctrl {
                    b"\x00"
                } else {
                    b" "
                }
            }
            _ => return None,
        };
        return Some(s.to_vec());
    }
    let t = text?;
    if ctrl {
        let c = t.chars().next()?.to_ascii_lowercase();
        if c.is_ascii_lowercase() {
            return Some(vec![c as u8 - b'a' + 1]);
        }
        return match c {
            '[' => Some(vec![0x1b]),
            '\\' => Some(vec![0x1c]),
            ']' => Some(vec![0x1d]),
            _ => None,
        };
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
        assert_eq!(key_bytes_app(Some("ArrowUp"), None, false, false, g.app_cursor), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes_app(Some("ArrowLeft"), None, false, false, true), Some(b"\x1bOD".to_vec()));
        assert_eq!(key_bytes_app(Some("Home"), None, false, false, true), Some(b"\x1bOH".to_vec()));
        assert_eq!(key_bytes_app(Some("End"), None, false, false, true), Some(b"\x1bOF".to_vec()));
        assert_eq!(key_bytes_app(Some("PageUp"), None, false, false, true), Some(b"\x1b[5~".to_vec()));
        assert_eq!(key_bytes_app(Some("ArrowDown"), None, false, false, false), Some(b"\x1b[B".to_vec()));
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
