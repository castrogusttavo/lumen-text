//! The unified "Go to" picker: Ctrl+P (files), `@` symbols of this file, `#` symbols of the whole
//! workspace (Ctrl+T opens it with `#`), `:` a line. Built on the quick open picker; the prefix
//! typed in its field picks the mode (see `gotopick`).
//!
//! * `#` asks every running server that offers `workspace/symbol`, debounced; a newer query cancels
//!   the requests still in flight, answers of an older one are ignored, and the answers of several
//!   servers are merged and ranked as they arrive.
//! * Moving through `@`, `#` and `:` results previews the target (the cursor moves, or the file is
//!   opened in a temporary tab); Esc puts everything back, Enter keeps the jump.

use crate::{
    app::App,
    app_overlay::{Action, Item, Overlay, PickKind, Picker},
    buffer::Sel,
    doc::uri_to_path,
    field::TextField,
    gotopick::{self, Mode},
    lsp::Pending,
    search::FileIndex,
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

/// Most results of a project symbol search that are listed.
const WS_LIMIT: usize = 200;

static SERIAL: AtomicU64 = AtomicU64::new(0);

/// The first build of the file index starts this long after the editor does (the first frame is
/// on screen by then); opening Quick Open starts it at once.
const INDEX_START_AFTER: Duration = Duration::from_millis(400);
/// A full rebuild waits this long, so a burst of events costs one walk.
const INDEX_SETTLE: Duration = Duration::from_millis(300);
/// More created / removed paths than this in one go are a burst: one walk instead of patches.
const INDEX_BURST: usize = 64;
/// Opening Quick Open refreshes an index older than this in the background (folders nobody
/// watches change without an event).
const INDEX_FRESH: Duration = Duration::from_secs(5);
/// A worker that died is tried again after this long.
const INDEX_RETRY: Duration = Duration::from_secs(5);

/// The file index of the open workspace and the worker that keeps it current. Setting the app's
/// `file_index` to `None` invalidates it (other roots, other ignore rules): a build in flight is
/// dropped with it and a new one starts on the next tick.
pub struct FileIdx {
    /// What Quick Open lists; shared with an open picker, replaced (never changed) by the worker.
    pub index: Arc<FileIndex>,
    /// The first build arrived.
    ready: bool,
    /// The job on the worker (`None` from it: nothing changed) and whether it is a full walk.
    job: Option<(Receiver<Option<FileIndex>>, bool)>,
    /// A full rebuild is wanted at this time.
    full_due: Option<Instant>,
    /// Paths created, removed or moved since the last job.
    pending: Vec<PathBuf>,
    /// File-system events arrived since the explorer was last rebuilt.
    events: bool,
    /// When the last full walk finished.
    walked: Instant,
}

impl FileIdx {
    fn new(first_build: Instant) -> FileIdx {
        FileIdx {
            index: Arc::default(),
            ready: false,
            job: None,
            full_due: Some(first_build),
            pending: Vec::new(),
            events: false,
            walked: first_build,
        }
    }

    /// A full rebuild `after` from now, unless one is already wanted sooner.
    fn want_full(&mut self, after: Duration) {
        let at = Instant::now() + after;
        self.full_due = Some(self.full_due.map_or(at, |t| t.min(at)));
    }

    /// Note file-system events: created, removed and moved paths wait to be patched in; a burst,
    /// or a change of the ignore rules, asks for one walk instead.
    fn note(&mut self, evs: &[crate::watcher::FsEvent]) {
        self.events = true;
        for e in evs {
            if crate::ignore_filter::IgnoreFilter::is_ignore_file(&e.path) {
                self.want_full(INDEX_SETTLE);
            } else if e.kind != crate::watcher::FsKind::Modified {
                self.pending.push(e.path.clone());
            }
        }
        if self.pending.len() > INDEX_BURST {
            self.pending.clear();
            self.want_full(INDEX_SETTLE);
        }
    }

    /// What the worker should do at `now`, if it is free and something is due: the paths to
    /// patch, or `None` for a full walk.
    fn next_job(&mut self, now: Instant) -> Option<Option<Vec<PathBuf>>> {
        if self.job.is_some() {
            return None;
        }
        if self.full_due.is_some_and(|t| t <= now) {
            // A walk sees everything that is pending; events from now on are patched after it.
            self.full_due = None;
            self.pending.clear();
            return Some(None);
        }
        (self.ready && !self.pending.is_empty()).then(|| Some(std::mem::take(&mut self.pending)))
    }
}

/// What the Go to picker lists in file mode: the open documents (most recent first), then the
/// file index without them. Rows are made only for what is shown (see `Picker::refilter`).
pub struct FileSource {
    index: Arc<FileIndex>,
    /// Open documents as (path, label).
    open: Vec<(PathBuf, String)>,
    /// Rows of the index that are open documents (listed already).
    skip: HashSet<usize>,
}

impl FileSource {
    pub fn new(index: Arc<FileIndex>, open: Vec<(PathBuf, String)>) -> FileSource {
        let mut s = FileSource { index: Arc::default(), open, skip: HashSet::new() };
        s.set_index(index);
        s
    }

    /// Another index (it was rebuilt or patched while the picker is open).
    pub fn set_index(&mut self, index: Arc<FileIndex>) {
        self.skip = self.open.iter().filter_map(|(p, _)| index.position(p)).collect();
        self.index = index;
    }

    fn label(&self, i: usize) -> Option<&str> {
        match i.checked_sub(self.open.len()) {
            None => Some(self.open[i].1.as_str()),
            Some(k) => (!self.skip.contains(&k)).then(|| self.index.label(k)),
        }
    }

    fn item(&self, i: usize) -> Item {
        let (path, label) = match i.checked_sub(self.open.len()) {
            None => (self.open[i].0.clone(), self.open[i].1.clone()),
            Some(k) => (self.index.path(k).to_path_buf(), self.index.label(k).to_string()),
        };
        Item { label, detail: String::new(), action: Action::Open(path, None) }
    }

    /// The rows to show for `query` and their matched label positions: the best 300, or the
    /// start of the list while nothing is typed.
    pub fn rows(&self, query: &str) -> (Vec<Item>, Vec<(usize, Vec<usize>)>) {
        let n = self.open.len() + self.index.len();
        let rows: Vec<(usize, Vec<usize>)> = if query.is_empty() {
            (0..n)
                .filter(|&i| self.label(i).is_some())
                .take(crate::search::MAX_REPLACE_HITS)
                .map(|i| (i, Vec::new()))
                .collect()
        } else {
            crate::fuzzy::rank(query, n, |i| self.label(i), 300)
        };
        let items = rows.iter().map(|(i, _)| self.item(*i)).collect();
        (items, rows.into_iter().enumerate().map(|(k, (_, pos))| (k, pos)).collect())
    }
}

/// A `workspace/symbol` search in flight: requests still unanswered and answers so far.
struct WsRun {
    serial: u64,
    query: String,
    reqs: Vec<(usize, i64)>,
    parts: Vec<(usize, String, Value)>,
}

/// State of an open Go to picker (kept in `Picker::goto`).
pub struct GotoState {
    mode: Mode,
    /// The file list, kept aside while another mode shows its own items.
    files: Option<Vec<Item>>,
    /// The document that was active when the picker opened.
    origin: Option<usize>,
    /// Documents the preview moved: index, selection, scroll (`top`, `target`) to put back.
    touched: Vec<(usize, Sel, f64, f64)>,
    /// Files the preview opened in a tab of their own (closed again on cancel).
    opened: Vec<PathBuf>,
    /// The row last previewed (label, detail).
    shown: Option<(String, String)>,
    ws: Option<WsRun>,
}

fn title_for(mode: Mode) -> &'static str {
    match mode {
        Mode::Files => "Go to file  (@ symbol in file  # symbol in project  : line)",
        Mode::DocSymbols => "Symbol in this file",
        Mode::WorkspaceSymbols => "Symbol in project (type to search)",
        Mode::Line => "Go to line (line[:column])",
    }
}

impl App {
    /// Open the Go to picker with `prefix` typed ("" = files, "@", "#", ":").
    pub fn open_goto(&mut self, prefix: &str) {
        let now = Instant::now();
        if cfg!(test) {
            // Tests read the list right after opening: build it here.
            if self.file_index.is_none() {
                let mut fi = FileIdx::new(now);
                (fi.index, fi.ready, fi.full_due) = (Arc::new(FileIndex::build(&self.roots())), true, None);
                self.file_index = Some(fi);
            }
        } else {
            // Never on this thread: the picker opens with what there is and fills in.
            let fi = self.file_index.get_or_insert_with(|| FileIdx::new(now));
            if !fi.ready || fi.walked + INDEX_FRESH <= now {
                fi.full_due = Some(now);
            }
            self.file_index_work(now);
        }
        let index = self.file_index.as_ref().map(|fi| fi.index.clone()).unwrap_or_default();
        let open: Vec<(PathBuf, String)> =
            self.docs.iter().rev().filter_map(|d| d.path()).map(|p| (p.to_path_buf(), self.rel(p))).collect();
        let mut p = Picker::new(PickKind::QuickOpen, title_for(Mode::Files), Vec::new());
        p.files = Some(Box::new(FileSource::new(index, open)));
        p.refilter();
        p.goto = Some(Box::new(GotoState {
            mode: Mode::Files,
            files: None,
            origin: (!self.docs.is_empty()).then_some(self.active),
            touched: Vec::new(),
            opened: Vec::new(),
            shown: None,
            ws: None,
        }));
        if !prefix.is_empty() {
            p.field = TextField::with(prefix);
        }
        self.overlay = Overlay::Picker(p);
        if !prefix.is_empty() {
            let _ = self.goto_changed();
            self.goto_preview();
        }
    }

    // ───────────── file index ─────────────

    /// File-system events (see `pump_watcher`): the index is patched on the worker.
    pub(crate) fn file_index_events(&mut self, evs: &[crate::watcher::FsEvent]) {
        let Some(fi) = self.file_index.as_mut() else { return };
        fi.note(evs);
        if !cfg!(test) {
            self.file_index_work(Instant::now());
        }
    }

    /// The explorer was rebuilt. After events the index is already being patched; without any
    /// (the window got the focus back, "Refresh explorer") nobody knows what changed: one walk in
    /// the background, the list in use stays until it is done.
    pub(crate) fn file_index_refresh(&mut self) {
        if cfg!(test) {
            // No worker in tests: the next Quick Open builds the list again.
            self.file_index = None;
            return;
        }
        if let Some(fi) = self.file_index.as_mut()
            && !std::mem::take(&mut fi.events)
        {
            fi.want_full(INDEX_SETTLE);
        }
    }

    /// Start the job that is due, if the worker is free: a full walk, or a patch of the pending
    /// paths. Returns when to look again.
    fn file_index_work(&mut self, now: Instant) -> Option<Instant> {
        let fi = self.file_index.as_mut()?;
        let Some(job) = fi.next_job(now) else { return fi.full_due };
        let (tx, rx) = std::sync::mpsc::channel();
        fi.job = Some((rx, job.is_none()));
        let old = fi.index.clone();
        let (roots, wake) = (self.roots(), self.wake());
        std::thread::spawn(move || {
            let new = match job {
                None => FileIndex::build(&roots),
                Some(paths) => {
                    let mut patched = (*old).clone();
                    patched.patch(&paths);
                    patched
                }
            };
            let _ = tx.send((new != *old).then_some(new));
            wake();
        });
        None
    }

    pub(crate) fn file_index_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if cfg!(test) {
            return;
        }
        let fi = self.file_index.get_or_insert_with(|| FileIdx::new(now + INDEX_START_AFTER));
        let done = fi.job.as_ref().and_then(|(rx, full)| match rx.try_recv() {
            Ok(new) => Some((Ok(new), *full)),
            Err(TryRecvError::Disconnected) => Some((Err(()), *full)),
            Err(TryRecvError::Empty) => None,
        });
        if let Some((res, full)) = done {
            fi.job = None;
            match res {
                Ok(new) => {
                    fi.ready = true;
                    if full {
                        fi.walked = now;
                    }
                    if let Some(new) = new {
                        fi.index = Arc::new(new);
                        self.file_index_publish();
                    }
                }
                Err(()) => fi.want_full(INDEX_RETRY),
            }
        }
        if let Some(t) = self.file_index_work(now) {
            want(t);
        }
    }

    /// The index changed: an open Go to picker lists the new one.
    fn file_index_publish(&mut self) {
        let Some(index) = self.file_index.as_ref().map(|fi| fi.index.clone()) else { return };
        if let Overlay::Picker(p) = &mut self.overlay
            && let Some(src) = p.files.as_mut()
        {
            src.set_index(index);
            if gotopick::parse(&p.field.text).0 == Mode::Files {
                p.refilter();
                self.request_redraw();
            }
        }
    }

    fn goto_take(&mut self) -> Option<Box<GotoState>> {
        match &mut self.overlay {
            Overlay::Picker(p) if p.kind == PickKind::QuickOpen => p.goto.take(),
            _ => None,
        }
    }

    fn goto_put(&mut self, g: Box<GotoState>) {
        if let Overlay::Picker(p) = &mut self.overlay {
            p.goto = Some(g);
        }
    }

    /// Symbols of the active document as picker rows (document order).
    fn goto_doc_items(&self) -> Vec<Item> {
        let Some(d) = self.docs.get(self.active) else { return Vec::new() };
        d.symbols
            .iter()
            .map(|s| Item {
                label: s.name.clone(),
                detail: gotopick::kind_name(s.kind).to_string(),
                action: Action::Goto(d.pos_from_lsp(s.sel_l, s.sel_c)),
            })
            .collect()
    }

    /// The field changed: switch mode if the prefix says so, refill and refilter the list. True when
    /// a debounced project symbol search should follow (see `start_search`).
    pub fn goto_changed(&mut self) -> bool {
        let Some(mut g) = self.goto_take() else {
            if let Overlay::Picker(p) = &mut self.overlay {
                p.refilter();
            }
            return false;
        };
        let text = match &self.overlay {
            Overlay::Picker(p) => p.field.text.clone(),
            _ => String::new(),
        };
        let (mode, rest) = gotopick::parse(&text);
        let rest = rest.trim().to_string();
        let switched = mode != g.mode;
        if switched {
            self.goto_leave(&mut g, mode);
        }
        let mut schedule = false;
        let items: Option<Vec<Item>> = match mode {
            Mode::Files => None,
            Mode::DocSymbols => {
                if switched {
                    if self.docs.get(self.active).is_none_or(|d| d.symbols.is_empty()) {
                        self.goto_ask_doc_symbols();
                    }
                    Some(self.goto_doc_items())
                } else {
                    None
                }
            }
            Mode::Line => Some(self.goto_line_items(&rest)),
            Mode::WorkspaceSymbols => {
                if rest.is_empty() {
                    self.goto_cancel_ws(&mut g);
                    Some(Vec::new())
                } else {
                    schedule = true;
                    None
                }
            }
        };
        if let Overlay::Picker(p) = &mut self.overlay {
            p.title = title_for(mode).to_string();
            if let Some(items) = items {
                p.items = items;
                p.real = p.items.len();
            }
            if switched {
                p.sel = 0;
            }
            p.refilter();
        }
        g.shown = None;
        self.goto_put(g);
        schedule
    }

    /// Leaving the current mode for `to`: files are put aside or back, a running search is cancelled.
    fn goto_leave(&mut self, g: &mut GotoState, to: Mode) {
        if g.mode == Mode::WorkspaceSymbols {
            self.goto_cancel_ws(g);
        }
        if let Overlay::Picker(p) = &mut self.overlay {
            if g.mode == Mode::Files {
                g.files = Some(std::mem::take(&mut p.items));
            }
            if to == Mode::Files {
                p.items = g.files.take().unwrap_or_default();
                p.real = p.items.len();
            } else {
                p.items.clear();
                p.real = 0;
            }
        }
        g.mode = to;
    }

    fn goto_line_items(&self, rest: &str) -> Vec<Item> {
        let Some(d) = self.docs.get(self.active) else { return Vec::new() };
        let Some((line, col)) = gotopick::parse_line(rest) else { return Vec::new() };
        let n = d.buf.line_count();
        let line = line.min(n);
        let pos = d.buf.pos(line - 1, col.unwrap_or(1) - 1);
        let label = match col {
            Some(c) => format!("Go to line {line}, column {c}"),
            None => format!("Go to line {line}"),
        };
        vec![Item { label, detail: format!("of {n}"), action: Action::Goto(pos) }]
    }

    /// The active document has no outline yet: ask its server (the answer refills the list).
    fn goto_ask_doc_symbols(&mut self) {
        let Some((i, li)) = self.with_server_pub() else {
            return self.say("symbols need a language server for this file");
        };
        let uri = self.docs[i].uri().unwrap_or_default();
        self.lsps[li].request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
            Pending::SymbolsCache { uri },
        );
    }

    /// An outline arrived (see `on_symbol_list`): fill the `@` list if it is showing.
    pub(crate) fn goto_symbols_arrived(&mut self) {
        let Some(g) = self.goto_take() else { return };
        if g.mode == Mode::DocSymbols {
            let items = self.goto_doc_items();
            if let Overlay::Picker(p) = &mut self.overlay {
                p.items = items;
                p.real = p.items.len();
                p.refilter();
            }
            self.goto_put(g);
            self.goto_preview();
        } else {
            self.goto_put(g);
        }
    }

    // ───────────── project symbols ─────────────

    fn goto_cancel_ws(&mut self, g: &mut GotoState) {
        let Some(run) = g.ws.take() else { return };
        for (li, id) in run.reqs {
            if let Some(l) = self.lsps.get_mut(li)
                && matches!(l.pending.get(&id), Some(Pending::WorkspaceSymbols { .. }))
            {
                l.cancel(id);
            }
        }
    }

    /// Ask every running server that offers `workspace/symbol` (the debounced step of `start_search`).
    pub fn goto_ws_request(&mut self, query: String) {
        let Some(mut g) = self.goto_take() else { return };
        self.goto_cancel_ws(&mut g);
        if g.mode != Mode::WorkspaceSymbols || query.is_empty() {
            return self.goto_put(g);
        }
        let targets: Vec<usize> = (0..self.lsps.len())
            .filter(|&li| {
                let l = &self.lsps[li];
                l.ready && (l.has_cap("workspaceSymbolProvider") || !l.is_generic())
            })
            .collect();
        if targets.is_empty() {
            self.say("no running language server offers project symbols (open a file of the project first)");
            if let Overlay::Picker(p) = &mut self.overlay {
                p.items.clear();
                p.real = 0;
                p.refilter();
            }
            return self.goto_put(g);
        }
        let serial = SERIAL.fetch_add(1, Ordering::SeqCst) + 1;
        let mut reqs = Vec::new();
        for li in targets {
            let id = self.lsps[li].request(
                "workspace/symbol",
                json!({"query": query}),
                Pending::WorkspaceSymbols { serial },
            );
            reqs.push((li, id));
        }
        g.ws = Some(WsRun { serial, query, reqs, parts: Vec::new() });
        self.goto_put(g);
    }

    /// One server answered `workspace/symbol`: merge with the answers so far (stale ones are ignored).
    pub(crate) fn goto_ws_answer(&mut self, li: usize, serial: u64, v: &Value) {
        let Some(mut g) = self.goto_take() else { return };
        let name = self.lsps.get(li).map(|l| l.name.clone()).unwrap_or_default();
        let mut merged = None;
        if let Some(run) = g.ws.as_mut()
            && run.serial == serial
        {
            run.reqs.retain(|(l, _)| *l != li);
            run.parts.push((li, name, v.clone()));
            merged = Some((gotopick::merge_workspace_symbols(&run.parts, &run.query, WS_LIMIT), run.parts.len() == 1));
        }
        if let Some((syms, first)) = merged {
            let items: Vec<Item> = syms
                .iter()
                .filter_map(|s| {
                    let path = uri_to_path(&s.uri)?;
                    let kind = gotopick::kind_name(s.kind);
                    let place = format!("{}:{}", self.rel(&path), s.line + 1);
                    let detail = [kind, s.container.as_str(), place.as_str()]
                        .into_iter()
                        .filter(|x| !x.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    Some(Item { label: s.name.clone(), detail, action: Action::Open(path, Some((s.line, s.col))) })
                })
                .collect();
            if let Overlay::Picker(p) = &mut self.overlay {
                p.items = items;
                p.real = p.items.len();
                if first {
                    p.sel = 0;
                }
                p.refilter();
            }
            self.goto_put(g);
            self.goto_preview();
            self.request_redraw();
        } else {
            self.goto_put(g);
        }
    }

    // ───────────── preview ─────────────

    /// Remember where document `i` was before the preview first moves it.
    fn goto_touch(&self, g: &mut GotoState, i: usize) {
        if g.touched.iter().any(|t| t.0 == i) {
            return;
        }
        if let Some(d) = self.docs.get(i) {
            g.touched.push((i, d.ed.sel, d.top, d.target));
        }
    }

    /// Show the highlighted result: move the cursor (`@`, `:`) or open the file (`#`).
    pub fn goto_preview(&mut self) {
        let Some(mut g) = self.goto_take() else { return };
        let item = match &self.overlay {
            Overlay::Picker(p) => p.selected(),
            _ => None,
        };
        let key = item.as_ref().map(|it| (it.label.clone(), it.detail.clone()));
        if key != g.shown {
            g.shown = key;
            match (g.mode, item) {
                (Mode::DocSymbols | Mode::Line, Some(Item { action: Action::Goto(pos), .. })) => {
                    if let Some(o) = g.origin
                        && o < self.docs.len()
                    {
                        self.activate(o);
                        self.goto_touch(&mut g, o);
                        let d = &mut self.docs[o];
                        d.ed.click(&mut d.buf, pos, false);
                        d.reveal = true;
                    }
                }
                (Mode::WorkspaceSymbols, Some(Item { action: Action::Open(path, at), .. })) => {
                    let canon = crate::paths::canonical(&path).unwrap_or_else(|_| path.clone());
                    let idx = match self.docs.iter().position(|d| d.path() == Some(canon.as_path())) {
                        Some(i) => {
                            if !g.opened.contains(&canon) {
                                self.goto_touch(&mut g, i);
                            }
                            Some(i)
                        }
                        None => {
                            let i = self.open_path(&canon, at);
                            if i.is_some() {
                                g.opened.push(canon);
                            }
                            i
                        }
                    };
                    if let Some(i) = idx {
                        self.activate(i);
                        if let Some((l, c)) = at {
                            let d = &mut self.docs[i];
                            let p = d.buf.pos(l, c);
                            d.ed.click(&mut d.buf, p, false);
                            d.reveal = true;
                        }
                    }
                }
                _ => {}
            }
            self.request_redraw();
        }
        self.goto_put(g);
    }

    /// Put the editor back as it was before the preview (cursor, scroll, tabs, active document) and
    /// cancel searches in flight.
    fn goto_abort(&mut self, mut g: GotoState) {
        self.goto_cancel_ws(&mut g);
        for (i, sel, top, target) in &g.touched {
            if let Some(d) = self.docs.get_mut(*i) {
                d.ed.sel = *sel;
                d.ed.clear_extra();
                d.top = *top;
                d.target = *target;
                d.reveal = false;
            }
        }
        let kept = self.closed_tabs.len();
        for path in &g.opened {
            if let Some(i) = self.docs.iter().position(|d| d.path() == Some(path.as_path())) {
                self.close_tab(i, true);
            }
        }
        self.closed_tabs.truncate(kept);
        if let Some(o) = g.origin
            && o < self.docs.len()
        {
            self.activate(o);
        }
        self.request_redraw();
    }

    /// Close the open picker without choosing (Esc, click outside).
    pub fn dismiss_picker(&mut self) {
        let old = std::mem::replace(&mut self.overlay, Overlay::None);
        if let Overlay::Picker(mut p) = old
            && let Some(g) = p.goto.take()
        {
            self.goto_abort(*g);
        }
        self.restore_theme_preview();
    }

    /// A row was chosen: put the editor back first, so the jump is recorded from where we were.
    pub fn goto_commit(&mut self, p: &mut Picker) {
        if let Some(g) = p.goto.take() {
            self.goto_abort(*g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Item {
        Item { label: label.into(), detail: String::new(), action: Action::Goto(0) }
    }

    fn picker(text: &str) -> Picker {
        let mut p = Picker::new(PickKind::QuickOpen, "t", vec![item("zeta"), item("alpha_open"), item("open")]);
        p.field = TextField::with(text);
        p.refilter();
        p
    }

    #[test]
    fn file_source_lists_open_documents_first_and_only_once() {
        let d = std::env::temp_dir().join(format!("lumen-goto-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        for f in ["src/alpha.rs", "src/beta.rs", "src/gamma.rs", "notes.md"] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        let index = Arc::new(FileIndex::build(std::slice::from_ref(&d)));
        let open = vec![
            (d.join("src/gamma.rs"), "src/gamma.rs".to_string()),
            (d.join("untracked.txt"), "untracked.txt".into()),
        ];
        let src = FileSource::new(index, open);
        let labels = |q: &str| src.rows(q).0.iter().map(|i| i.label.replace('\\', "/")).collect::<Vec<_>>();
        assert_eq!(labels(""), vec!["src/gamma.rs", "untracked.txt", "notes.md", "src/alpha.rs", "src/beta.rs"]);
        assert_eq!(labels("gam"), vec!["src/gamma.rs"], "an open file is not listed again from the index");
        assert_eq!(labels("src a"), vec!["src/alpha.rs", "src/gamma.rs", "src/beta.rs"]);
        let (items, filtered) = src.rows("beta");
        assert_eq!(filtered, vec![(0, vec![4, 5, 6, 7])]);
        assert!(matches!(&items[0].action, Action::Open(p, None) if *p == d.join("src/beta.rs")));

        // The same rows through the picker: `items` holds only what is shown.
        let mut p = Picker::new(PickKind::QuickOpen, "t", Vec::new());
        p.files = Some(Box::new(src));
        p.field = TextField::with("beta");
        p.refilter();
        assert_eq!((p.items.len(), p.filtered.len()), (1, 1));
        assert_eq!(p.selected().map(|i| i.label.replace('\\', "/")).as_deref(), Some("src/beta.rs"));
        p.field = TextField::with("@beta");
        p.refilter();
        assert_eq!(p.filtered.len(), 1, "another mode filters the picker's own items");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn index_jobs_patch_small_changes_and_walk_on_bursts() {
        use crate::watcher::{FsEvent, FsKind};
        let t0 = Instant::now();
        let ev = |p: &str, kind| FsEvent { path: PathBuf::from(p), kind };
        let mut fi = FileIdx::new(t0 + Duration::from_secs(1));
        assert_eq!(fi.next_job(t0), None, "the first build waits for its time");
        fi.note(&[ev("/p/new.rs", FsKind::Created)]);
        assert_eq!(fi.next_job(t0), None, "nothing to patch before the first build");
        assert_eq!(fi.next_job(t0 + Duration::from_secs(1)), Some(None), "the first build is a walk");
        assert!(fi.pending.is_empty(), "the walk covers what was pending");
        fi.ready = true;

        // Saves change nothing in the list; created and removed paths are patched.
        fi.note(&[ev("/p/a.rs", FsKind::Modified)]);
        assert_eq!(fi.next_job(t0), None);
        fi.note(&[ev("/p/b.rs", FsKind::Created), ev("/p/c.rs", FsKind::Removed), ev("/p/d.rs", FsKind::Moved)]);
        let want: Vec<PathBuf> = ["/p/b.rs", "/p/c.rs", "/p/d.rs"].iter().map(PathBuf::from).collect();
        assert_eq!(fi.next_job(t0), Some(Some(want)));
        assert_eq!(fi.next_job(t0), None);

        // A burst, or new ignore rules: one walk, once things settle.
        let burst: Vec<FsEvent> = (0..=INDEX_BURST).map(|i| ev(&format!("/p/f{i}.rs"), FsKind::Created)).collect();
        fi.note(&burst);
        assert!(fi.pending.is_empty());
        assert_eq!(fi.next_job(Instant::now()), None, "not before the burst settles");
        assert_eq!(fi.next_job(Instant::now() + INDEX_SETTLE), Some(None));
        fi.note(&[ev("/p/.gitignore", FsKind::Modified)]);
        assert_eq!(fi.next_job(Instant::now() + INDEX_SETTLE), Some(None));

        // One job at a time: events during a job wait for it.
        let (_tx, rx) = std::sync::mpsc::channel();
        fi.job = Some((rx, false));
        fi.note(&[ev("/p/e.rs", FsKind::Created)]);
        assert_eq!(fi.next_job(Instant::now() + INDEX_SETTLE), None);
        fi.job = None;
        assert_eq!(fi.next_job(t0), Some(Some(vec![PathBuf::from("/p/e.rs")])));
    }

    #[test]
    fn plain_text_filters_files_fuzzily() {
        let p = picker("zeta");
        assert_eq!(p.filtered.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0]);
        assert!(picker("qqq").filtered.is_empty());
        assert_eq!(picker("").filtered.len(), 3);
    }

    #[test]
    fn at_prefix_filters_by_the_text_after_it() {
        let p = picker("@zeta");
        assert_eq!(p.filtered.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0]);
        assert_eq!(picker("@").filtered.len(), 3);
    }

    #[test]
    fn project_symbols_keep_the_order_of_the_merge() {
        let p = picker("#o");
        assert_eq!(p.filtered.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(p.filtered[2].1, vec![0]); // "open": the o is highlighted
        assert!(p.filtered[0].1.is_empty()); // "zeta" does not contain it: listed, not highlighted
    }

    #[test]
    fn line_rows_are_not_text_filtered() {
        assert_eq!(picker(":12").filtered.len(), 3);
    }
}
