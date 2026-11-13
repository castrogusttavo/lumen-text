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
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

/// Most results of a project symbol search that are listed.
const WS_LIMIT: usize = 200;

static SERIAL: AtomicU64 = AtomicU64::new(0);

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
        if self.file_index.is_none() {
            self.file_index = Some(crate::search::list_files(&self.root, 50_000));
        }
        let files = self.file_index.clone().unwrap_or_default();
        let open: Vec<PathBuf> = self.docs.iter().filter_map(|d| d.path().map(PathBuf::from)).collect();
        let mut items: Vec<Item> = Vec::new();
        for p in open.iter().rev().chain(files.iter().filter(|f| !open.contains(f))) {
            items.push(Item { label: self.rel(p), detail: String::new(), action: Action::Open(p.clone(), None) });
        }
        let mut p = Picker::new(PickKind::QuickOpen, title_for(Mode::Files), items);
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
                Pending::WorkspaceSymbols { query: query.clone(), serial },
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
