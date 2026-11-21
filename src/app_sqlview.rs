//! SQLite viewer tab: the list of tables, a paginated grid and a one-line SELECT box. Everything
//! goes through the system `sqlite3` CLI in read-only mode on a worker thread (see `sqlview`),
//! so there is no SQLite library in the binary and the data can never be modified.

use crate::{
    app::{App, Focus, KeyIn},
    app_gitpanel::fit,
    field::TextField,
    lsp::Wake,
    sqlview::{self, CellKind, Rows, Table},
    theme::{Rgba, Theme},
    ui::{Canvas, HBar, Rect, TextCtx, VBar},
};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender, channel},
};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
};

const LIST_ROW: i32 = 26;
const LIST_HEAD: i32 = 30;
const BAR_H: i32 = 40;
const FOOT_H: i32 = 30;
const HEAD_H: i32 = 26;
const ROW_H: i32 = 24;
const CELL_PAD: i32 = 10;
const COL_MIN: i32 = 60;
const COL_MAX: i32 = 360;
const SCROLL_STEP: f64 = 48.0;

/// What the grid shows.
#[derive(Clone, Debug)]
enum Source {
    Table(String),
    Query(String),
}

enum Msg {
    Tables(Result<Vec<Table>, Option<String>>),
    Counts(String),
    Page(u64, Result<Rows, Option<String>>),
}

pub struct SqlView {
    path: PathBuf,
    wake: Wake,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    tables: Vec<Table>,
    loaded: bool,
    no_sqlite: bool,
    sel: Option<usize>,
    cur: Option<Source>,
    offset: usize,
    result: Option<Rows>,
    error: Option<String>,
    busy: bool,
    gen_id: u64,
    col_px: Option<Vec<i32>>,
    scroll: (f64, f64),
    list_scroll: f64,
    input: TextField,
    input_focus: bool,
    // Hit areas of the last frame.
    pub rect: Rect,
    list: Rect,
    grid: Rect,
    input_r: Rect,
    prev_r: Rect,
    next_r: Rect,
    rows_r: Vec<(Rect, usize)>,
}

impl SqlView {
    pub fn new(path: &Path, wake: Wake) -> Self {
        let (tx, rx) = channel();
        let mut v = SqlView {
            path: path.to_path_buf(),
            wake,
            tx,
            rx,
            tables: Vec::new(),
            loaded: false,
            no_sqlite: false,
            sel: None,
            cur: None,
            offset: 0,
            result: None,
            error: None,
            busy: true,
            gen_id: 0,
            col_px: None,
            scroll: (0.0, 0.0),
            list_scroll: 0.0,
            input: TextField::default(),
            input_focus: false,
            rect: Rect::default(),
            list: Rect::default(),
            grid: Rect::default(),
            input_r: Rect::default(),
            prev_r: Rect::default(),
            next_r: Rect::default(),
            rows_r: Vec::new(),
        };
        v.load();
        v
    }

    /// The file changed on disk: read the table list and the current page again.
    pub fn reload(&mut self) {
        self.tables.clear();
        self.loaded = false;
        self.busy = true;
        self.load();
    }

    /// List the tables, then count their rows (the list shows up first).
    fn load(&mut self) {
        let (db, tx, wake) = (self.path.clone(), self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let tables = sqlview::run(&db, sqlview::LIST_SQL).and_then(|o| sqlview::parse_tables(&o).map_err(Some));
            let counts = tables.as_ref().ok().and_then(|t| sqlview::counts_sql(t));
            let _ = tx.send(Msg::Tables(tables));
            wake();
            if let Some(sql) = counts
                && let Ok(out) = sqlview::run(&db, &sql)
            {
                let _ = tx.send(Msg::Counts(out));
                wake();
            }
        });
    }

    fn fetch(&mut self) {
        let Some(src) = self.cur.clone() else { return };
        self.gen_id += 1;
        self.busy = true;
        self.error = None;
        let (sql, skip) = match &src {
            Source::Table(n) => (sqlview::table_sql(n, self.offset), 0),
            Source::Query(q) => {
                (sqlview::page_sql(q, self.offset), if sqlview::wrappable(q) { 0 } else { self.offset })
            }
        };
        let (db, tx, wake, g) = (self.path.clone(), self.tx.clone(), self.wake.clone(), self.gen_id);
        std::thread::spawn(move || {
            let r = sqlview::run(&db, &sql).and_then(|o| sqlview::parse_rows(&o, skip).map_err(Some));
            let _ = tx.send(Msg::Page(g, r));
            wake();
        });
    }

    fn open_table(&mut self, i: usize) {
        let Some(t) = self.tables.get(i) else { return };
        self.sel = Some(i);
        self.cur = Some(Source::Table(t.name.clone()));
        self.offset = 0;
        self.fetch();
    }

    fn go(&mut self, offset: usize) {
        self.offset = offset;
        self.fetch();
    }

    fn submit(&mut self) {
        match sqlview::guard(&self.input.text) {
            Ok(sql) => {
                self.sel = None;
                self.cur = Some(Source::Query(sql));
                self.offset = 0;
                self.fetch();
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Tables(Ok(t)) => {
                    self.tables = t;
                    self.loaded = true;
                    self.busy = false;
                    if self.cur.is_some() {
                        self.fetch();
                    } else if !self.tables.is_empty() {
                        self.open_table(0);
                    }
                }
                Msg::Tables(Err(None)) => {
                    self.no_sqlite = true;
                    self.busy = false;
                }
                Msg::Tables(Err(Some(e))) => {
                    self.loaded = true;
                    self.busy = false;
                    self.error = Some(e);
                }
                Msg::Counts(out) => sqlview::apply_counts(&mut self.tables, &out),
                Msg::Page(g, r) if g == self.gen_id => {
                    self.busy = false;
                    match r {
                        Ok(rows) => {
                            self.result = Some(rows);
                            self.error = None;
                            self.col_px = None;
                            self.scroll = (0.0, 0.0);
                        }
                        Err(None) => self.no_sqlite = true,
                        Err(Some(e)) => self.error = Some(e),
                    }
                }
                Msg::Page(..) => {}
            }
        }
    }

    /// Total rows of the table on screen, when counted.
    fn total(&self) -> Option<u64> {
        let Some(Source::Table(n)) = &self.cur else { return None };
        self.tables.iter().find(|t| t.name == *n)?.rows
    }

    // ── drawing ──

    /// Draw the viewer into `rect`. True while a query is running.
    pub fn draw(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, rect: Rect) -> bool {
        self.poll();
        self.rect = rect;
        cv.fill(rect, theme.background);
        let dim = theme.chrome.tab_inactive_fg;
        if self.no_sqlite {
            let lines = [
                ("This viewer talks to the sqlite3 command line tool, which is not installed.".to_string(), dim),
                ("Install it (e.g. apt install sqlite3, brew install sqlite) and reopen the file.".to_string(), dim),
            ];
            empty_state(cv, tx, theme, rect, "sqlite3 not found", &lines);
            return false;
        }
        let list_w = (rect.w / 3).clamp(120, 240);
        self.list = Rect::new(rect.x, rect.y, list_w, rect.h);
        let main = Rect::new(rect.x + list_w, rect.y, (rect.w - list_w).max(0), rect.h);
        self.draw_list(cv, tx, theme);
        self.draw_main(cv, tx, theme, main);
        self.busy
    }

    fn draw_list(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme) {
        let c = theme.chrome;
        let r = self.list;
        cv.fill(r, c.sidebar_bg);
        cv.fill(Rect::new(r.right() - 1, r.y, 1, r.h), c.border);
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let head = fit(tx, &name, r.w - 24, true);
        tx.draw_ui(cv, r.x + 12, r.y + (LIST_HEAD - 18) / 2 + 2, &head, c.tab_active_fg, r, true);
        let body = Rect::new(r.x, r.y + LIST_HEAD, r.w - 1, (r.h - LIST_HEAD).max(0));
        let content = self.tables.len() as i32 * LIST_ROW;
        let max = (content - body.h).max(0) as f64;
        self.list_scroll = self.list_scroll.clamp(0.0, max);
        self.rows_r.clear();
        if self.tables.is_empty() {
            let msg = if self.loaded { "No tables" } else { "Loading…" };
            tx.draw_ui(cv, r.x + 12, body.y + 6, msg, c.tab_inactive_fg, body, false);
            return;
        }
        let mut y = body.y - self.list_scroll as i32;
        for (i, t) in self.tables.iter().enumerate() {
            let row = Rect::new(r.x, y, body.w, LIST_ROW);
            y += LIST_ROW;
            if row.bottom() <= body.y || row.y >= body.bottom() {
                continue;
            }
            let on = self.sel == Some(i);
            if on {
                cv.fill(row.intersect(&body), c.list_active_bg);
            }
            let count = t.rows.map(sqlview::group_digits).unwrap_or_default();
            let cw = tx.ui_width(&count, false);
            let fg = if on { c.tab_active_fg } else { c.sidebar_fg };
            let label = if t.view { format!("{} (view)", t.name) } else { t.name.clone() };
            let label = fit(tx, &label, row.w - cw - 36, false);
            let ty = row.y + (LIST_ROW - 18) / 2;
            tx.draw_ui(cv, row.x + 12, ty, &label, fg, body, false);
            tx.draw_ui(cv, row.right() - cw - 12, ty, &count, c.tab_inactive_fg, body, false);
            self.rows_r.push((row.intersect(&body), i));
        }
        if let Some(b) =
            VBar::new(Rect::new(body.right() - 10, body.y, 10, body.h), content as f64, body.h as f64, self.list_scroll)
        {
            b.draw(cv, theme.foreground, false, false);
        }
    }

    fn draw_main(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, main: Rect) {
        let c = theme.chrome;
        // The query box.
        let bar = Rect::new(main.x, main.y, main.w, BAR_H);
        self.input_r = Rect::new(bar.x + 10, bar.y + 7, (bar.w - 20).max(0), BAR_H - 14);
        cv.fill_round(self.input_r, 6, c.input_bg);
        cv.ring(self.input_r, 6, if self.input_focus { c.accent } else { c.border });
        let tr = self.input_r.inset(1);
        let ty = tr.y + (tr.h - 18) / 2;
        if self.input.text.is_empty() {
            tx.draw_ui(cv, tr.x + 8, ty, "SELECT … (Enter runs it; read-only)", c.tab_inactive_fg, tr, false);
        } else {
            tx.draw_ui(cv, tr.x + 8, ty, &self.input.text, c.tab_active_fg, tr, false);
        }
        if self.input_focus {
            let before: String = self.input.text.chars().take(self.input.cursor).collect();
            let cx = tr.x + 8 + tx.ui_width(&before, false);
            cv.fill_clip(Rect::new(cx, tr.y + 4, 1, tr.h - 8), c.tab_active_fg, tr);
        }
        cv.fill(Rect::new(main.x, bar.bottom() - 1, main.w, 1), c.border.with_alpha(0x50));

        // The footer: range or error, and the page buttons.
        let foot = Rect::new(main.x, main.bottom() - FOOT_H, main.w, FOOT_H);
        cv.fill(Rect::new(foot.x, foot.y, foot.w, 1), c.border.with_alpha(0x50));
        self.draw_foot(cv, tx, theme, foot);

        // The grid.
        let grid = Rect::new(main.x, bar.bottom(), main.w, (main.h - BAR_H - FOOT_H).max(0));
        self.grid = grid;
        self.draw_grid(cv, tx, theme, grid);
    }

    fn draw_foot(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, foot: Rect) {
        let c = theme.chrome;
        let ty = foot.y + (foot.h - 18) / 2 + 1;
        let more = self.result.as_ref().is_some_and(|r| r.more);
        let n = self.result.as_ref().map_or(0, |r| r.rows.len());
        let (prev_on, next_on) = (sqlview::prev_offset(self.offset).is_some(), more);
        // Buttons at the right: ‹ Prev, Next ›.
        let mut x = foot.right() - 8;
        let mut btn = |tx: &mut TextCtx, cv: &mut Canvas, label: &str, on: bool| -> Rect {
            let w = tx.ui_width(label, false) + 20;
            x -= w;
            let r = Rect::new(x, foot.y + 3, w, foot.h - 6);
            x -= 6;
            cv.fill_round(r, 5, if on { c.widget_bg } else { c.widget_bg.with_alpha(0x60) });
            let fg = if on { c.tab_active_fg } else { c.tab_inactive_fg.with_alpha(0x80) };
            tx.draw_ui(cv, r.x + 10, ty - 1, label, fg, r, false);
            r
        };
        self.next_r = btn(tx, cv, "Next ›", next_on);
        self.prev_r = btn(tx, cv, "‹ Prev", prev_on);
        let left = (x - foot.x - 20).max(0);
        let (text, col) = if let Some(e) = &self.error {
            (e.clone(), c.error)
        } else if self.busy {
            ("Running…".to_string(), c.tab_inactive_fg)
        } else {
            let range = sqlview::range_label(self.offset, n);
            let s = match self.total() {
                Some(t) if n > 0 => format!("{range} of {} rows", sqlview::group_digits(t)),
                _ => range,
            };
            (s, c.tab_inactive_fg)
        };
        let text = fit(tx, &text, left, false);
        tx.draw_ui(cv, foot.x + 12, ty - 1, &text, col, foot, false);
    }

    fn draw_grid(&mut self, cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, grid: Rect) {
        let c = theme.chrome;
        let dim = c.tab_inactive_fg;
        let Some(res) = self.result.take() else {
            let msg = if self.busy || !self.loaded { "Loading…" } else { "Pick a table on the left" };
            let w = tx.ui_width(msg, false);
            tx.draw_ui(cv, grid.x + (grid.w - w) / 2, grid.y + 40, msg, dim, grid, false);
            return;
        };
        if res.cols.is_empty() {
            let msg = "No rows";
            let w = tx.ui_width(msg, false);
            tx.draw_ui(cv, grid.x + (grid.w - w) / 2, grid.y + 40, msg, dim, grid, false);
            self.result = Some(res);
            return;
        }
        let widths = self.col_px.get_or_insert_with(|| col_widths(tx, &res)).clone();
        let body = Rect::new(grid.x, grid.y + HEAD_H, grid.w, (grid.h - HEAD_H).max(0));
        let content_w = widths.iter().sum::<i32>();
        let content_h = res.rows.len() as i32 * ROW_H;
        self.scroll.0 = self.scroll.0.clamp(0.0, (content_w - grid.w).max(0) as f64);
        self.scroll.1 = self.scroll.1.clamp(0.0, (content_h - body.h).max(0) as f64);
        let (sx, sy) = (self.scroll.0 as i32, self.scroll.1 as i32);
        let first = (sy / ROW_H) as usize;
        let ty = (ROW_H - 18) / 2;
        for (ri, row) in res.rows.iter().enumerate().skip(first) {
            let y = body.y + ri as i32 * ROW_H - sy;
            if y >= body.bottom() {
                break;
            }
            if ri % 2 == 1 {
                cv.fill(Rect::new(grid.x, y, grid.w, ROW_H).intersect(&body), c.list_hover_bg.with_alpha(0x28));
            }
            let mut x = grid.x - sx;
            for (ci, cell) in row.iter().enumerate() {
                let w = widths.get(ci).copied().unwrap_or(COL_MIN);
                if x + w > grid.x && x < grid.right() {
                    let clip = Rect::new(x, y, w, ROW_H).intersect(&body);
                    let (fg, right) = match cell.kind {
                        CellKind::Null => (dim.with_alpha(0xA0), false),
                        CellKind::Num => (theme.foreground, true),
                        CellKind::Text => (theme.foreground, false),
                    };
                    let text = fit(tx, &cell.text, w - 2 * CELL_PAD, false);
                    let tw = tx.ui_width(&text, false);
                    let tx0 = if right { x + w - CELL_PAD - tw } else { x + CELL_PAD };
                    if cell.kind == CellKind::Null {
                        tx.draw_ui_italic(cv, tx0, y + ty, &text, fg, clip, false);
                    } else {
                        tx.draw_ui(cv, tx0, y + ty, &text, fg, clip, false);
                    }
                }
                x += w;
            }
        }
        // Column separators, then the sticky header over the rows.
        let sep = c.border.with_alpha(0x50);
        let head = Rect::new(grid.x, grid.y, grid.w, HEAD_H);
        cv.fill(head, c.tabbar_bg);
        let mut x = grid.x - sx;
        for (name, w) in res.cols.iter().zip(&widths) {
            if x + w > grid.x && x < grid.right() {
                let clip = Rect::new(x, head.y, *w, HEAD_H).intersect(&head);
                let text = fit(tx, name, w - 2 * CELL_PAD, true);
                tx.draw_ui(cv, x + CELL_PAD, head.y + (HEAD_H - 18) / 2, &text, c.tab_active_fg, clip, true);
                cv.fill(Rect::new(x + w - 1, grid.y, 1, grid.h).intersect(&grid), sep);
            }
            x += w;
        }
        cv.fill(Rect::new(grid.x, head.bottom() - 1, grid.w, 1), c.border);
        if let Some(b) =
            VBar::new(Rect::new(grid.right() - 10, body.y, 10, body.h), content_h as f64, body.h as f64, self.scroll.1)
        {
            b.draw(cv, theme.foreground, false, false);
        }
        if let Some(b) = HBar::new(
            Rect::new(grid.x, grid.bottom() - 10, (grid.w - 10).max(0), 10),
            content_w as f64,
            grid.w as f64,
            self.scroll.0,
        ) {
            b.draw(cv, theme.foreground, false, false);
        }
        self.result = Some(res);
    }

    // ── input ──

    pub fn wheel(&mut self, x: f64, y: f64, delta: MouseScrollDelta, shift: bool) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        let (mut dx, mut dy) = crate::scroller::wheel_px_xy(delta, SCROLL_STEP);
        if self.list.contains(x, y) {
            self.list_scroll = (self.list_scroll + dy).max(0.0);
        } else {
            if shift && dx == 0.0 {
                (dx, dy) = (dy, 0.0);
            }
            self.scroll.0 = (self.scroll.0 + dx).max(0.0);
            self.scroll.1 = (self.scroll.1 + dy).max(0.0);
        }
        true
    }

    pub fn mouse_down(&mut self, x: f64, y: f64) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        self.input_focus = self.input_r.contains(x, y);
        if self.input_focus {
            self.input.end();
            self.input.sel_all = false;
        } else if let Some(&(_, i)) = self.rows_r.iter().find(|(r, _)| r.contains(x, y)) {
            self.open_table(i);
        } else if self.prev_r.contains(x, y)
            && let Some(o) = sqlview::prev_offset(self.offset)
        {
            self.go(o);
        } else if self.next_r.contains(x, y)
            && let Some(o) = sqlview::next_offset(self.offset, self.result.as_ref().is_some_and(|r| r.more))
        {
            self.go(o);
        }
        true
    }

    /// A key for the viewer. `cmd` = Ctrl / Alt / Super held (never typed text).
    pub fn key(&mut self, ev: &KeyIn, cmd: bool) -> bool {
        let page = self.grid.h as f64 - HEAD_H as f64;
        match &ev.key {
            Key::Named(NamedKey::Enter) if self.input_focus => self.submit(),
            Key::Named(NamedKey::Escape) => self.input_focus = false,
            Key::Named(NamedKey::Backspace) if self.input_focus => {
                self.input.backspace();
            }
            Key::Named(NamedKey::Delete) if self.input_focus => {
                self.input.delete();
            }
            Key::Named(NamedKey::ArrowLeft) if self.input_focus => self.input.left(),
            Key::Named(NamedKey::ArrowRight) if self.input_focus => self.input.right(),
            Key::Named(NamedKey::Home) if self.input_focus => self.input.home(),
            Key::Named(NamedKey::End) if self.input_focus => self.input.end(),
            Key::Named(NamedKey::ArrowLeft) => self.scroll.0 = (self.scroll.0 - SCROLL_STEP).max(0.0),
            Key::Named(NamedKey::ArrowRight) => self.scroll.0 += SCROLL_STEP,
            Key::Named(NamedKey::ArrowUp) => self.scroll.1 = (self.scroll.1 - SCROLL_STEP).max(0.0),
            Key::Named(NamedKey::ArrowDown) => self.scroll.1 += SCROLL_STEP,
            Key::Named(NamedKey::PageUp) => self.scroll.1 = (self.scroll.1 - page).max(0.0),
            Key::Named(NamedKey::PageDown) => self.scroll.1 += page,
            _ => {
                if !cmd && let Some(t) = ev.text.as_deref().filter(|t| !t.chars().any(char::is_control)) {
                    // Typing anywhere goes to the query box.
                    self.input_focus = true;
                    self.input.insert(t);
                }
            }
        }
        true
    }
}

/// Pixel width of every column: the widest of its header and its cells.
fn col_widths(tx: &mut TextCtx, r: &Rows) -> Vec<i32> {
    let mut w: Vec<i32> = r.cols.iter().map(|c| tx.ui_width(c, true)).collect();
    for row in &r.rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(slot) = w.get_mut(i) {
                *slot = (*slot).max(tx.ui_width(&cell.text, false));
            }
        }
    }
    w.iter().map(|v| (v + 2 * CELL_PAD).clamp(COL_MIN, COL_MAX)).collect()
}

/// A calm centered message with an icon (the viewer's own `b_empty_state`).
fn empty_state(cv: &mut Canvas, tx: &mut TextCtx, theme: &Theme, area: Rect, title: &str, lines: &[(String, Rgba)]) {
    let c = theme.chrome;
    let block = 28 + 12 + 22 + lines.len() as i32 * 20;
    let mut y = area.y + ((area.h - block) / 2).clamp(16, 40);
    tx.icon(cv, "ui/terminal", area.x + (area.w - 28) / 2, y, 28, c.tab_inactive_fg, area);
    y += 28 + 12;
    let t = fit(tx, title, area.w - 32, true);
    let w = tx.ui_width(&t, true);
    tx.draw_ui(cv, area.x + (area.w - w) / 2, y, &t, c.tab_active_fg, area, true);
    y += 24;
    for (s, col) in lines {
        let s = fit(tx, s, area.w - 32, false);
        let w = tx.ui_width(&s, false);
        tx.draw_ui(cv, area.x + (area.w - w) / 2, y, &s, *col, area, false);
        y += 20;
    }
}

impl App {
    fn sql_active(&mut self) -> Option<&mut SqlView> {
        self.docs.get_mut(self.active).and_then(|d| d.sql.as_deref_mut())
    }

    /// Wheel over the viewer of the active tab.
    pub fn sql_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let ((x, y), shift) = (self.mouse, self.mods.shift_key());
        self.sql_active().is_some_and(|v| v.wheel(x, y, delta, shift))
    }

    pub fn sql_mouse_down(&mut self, x: f64, y: f64) -> bool {
        let taken = self.sql_active().is_some_and(|v| v.mouse_down(x, y));
        if taken {
            self.focus = Focus::Editor;
        }
        taken
    }

    /// The pointer is over the viewer: no text hover there.
    pub fn sql_mouse_move(&mut self, x: f64, y: f64) -> bool {
        self.sql_active().is_some_and(|v| v.rect.contains(x, y))
    }

    /// Keys on a database tab (it has no text to edit).
    pub fn sql_key(&mut self, ev: &KeyIn) -> bool {
        let cmd = self.mods.control_key() || self.mods.alt_key() || self.mods.super_key();
        self.sql_active().is_some_and(|v| v.key(ev, cmd))
    }

    /// "Database: Open SQLite file…": ask for the path.
    pub fn sql_open_prompt(&mut self) {
        let start = format!("{}/", self.root.display());
        self.overlay = crate::app_overlay::Overlay::Prompt(crate::app_overlay::Prompt::new(
            crate::app_overlay::PromptKind::SqliteOpen,
            "Open SQLite file (.db, .sqlite, .sqlite3; ~ and relative paths work)".to_string(),
            TextField::with(&start),
        ));
        self.request_redraw();
    }

    pub fn sql_open_answer(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let path = crate::app_workspace::resolve_typed(text, &self.root.clone());
        if !path.is_file() {
            return self.say(format!("{}: no such file", path.display()));
        }
        if !sqlview::is_sqlite_file(&path) {
            return self.say(format!("{}: not a SQLite database", path.display()));
        }
        self.open_path(&path, None);
    }
}
