//! Language-server glue: lifecycle, text sync, and the editor features built on LSP
//! (diagnostics, hover, completion, definition, references, rename, format, actions).

use crate::{
    app::App,
    app_overlay::{Action, Item},
    buffer::EditKind,
    doc::{Diag, Doc, LspEdit, path_to_uri, uri_to_path},
    fuzzy::fuzzy,
    lsp::{self, DefKind, Lsp, Msg, Pending},
    theme::Rgba,
    ui::{Canvas, Rect, line_h},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};
use winit::keyboard::{Key, NamedKey};

const SYNC_DEBOUNCE: Duration = Duration::from_millis(120);
const MEM_LIMIT_MIB: u32 = 300;

pub use crate::app_hover::HoverTip;

#[derive(Clone)]
pub struct CItem {
    pub label: String,
    pub detail: String,
    pub insert: String,
    pub kind: u32,
    pub sort: String,
    pub range: Option<LspEdit>,
    pub raw: Value,
}

pub struct Completion {
    pub items: Vec<CItem>,
    pub filtered: Vec<usize>,
    pub sel: usize,
    pub scroll: usize,
    pub start: usize,
    /// Length of the prefix the list was fetched for; typing less than this needs a new request.
    pub seed_len: usize,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

pub(crate) fn ts_family(lang: Option<&str>) -> bool {
    matches!(lang, Some("typescript" | "tsx" | "javascript" | "jsx"))
}

fn lsp_range(d: &Doc, a: usize, b: usize) -> Value {
    let (sl, sc) = d.lsp_pos(a);
    let (el, ec) = d.lsp_pos(b);
    json!({"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}})
}

fn parse_edit(e: &Value) -> Option<LspEdit> {
    let r = &e["range"];
    Some(LspEdit {
        sl: r["start"]["line"].as_u64()? as usize,
        sc: r["start"]["character"].as_u64()? as usize,
        el: r["end"]["line"].as_u64()? as usize,
        ec: r["end"]["character"].as_u64()? as usize,
        text: e["newText"].as_str().unwrap_or("").to_string(),
    })
}

pub(crate) fn parse_edits(v: &Value) -> Vec<LspEdit> {
    v.as_array().map(|a| a.iter().filter_map(parse_edit).collect()).unwrap_or_default()
}

impl App {
    // ───────────── lifecycle ─────────────

    fn lsp_index_for_root(&self, root: &std::path::Path) -> Option<usize> {
        self.lsps.iter().position(|l| l.root == root && l.name == "tsgo")
    }

    pub(crate) fn doc_lsp(&self, doc: usize) -> Option<usize> {
        let d = self.docs.get(doc)?;
        if !ts_family(d.lang) {
            return None;
        }
        self.lsp_index_for_root(d.lsp_root.as_ref()?)
    }

    pub(crate) fn ensure_lsp(&mut self, doc: usize) -> Option<usize> {
        let d = self.docs.get_mut(doc)?;
        if !ts_family(d.lang) {
            return None;
        }
        if d.lsp_root.is_none() {
            d.lsp_root = d.path().and_then(lsp::project_root);
        }
        let root = d.lsp_root.clone()?;
        if let Some(i) = self.lsp_index_for_root(&root) {
            return Some(i);
        }
        let Some(bin) = lsp::find_tsgo(&root, &self.config.tsgo_source) else {
            if !self.lsp_warned {
                self.lsp_warned = true;
                self.say("tsgo not found (npm i @typescript/native-preview in ~/.local/share/lumen-text/tsgo)");
            }
            return None;
        };
        // Narrow mode: the server runs in a mirror whose tsconfig includes only the open files.
        let narrow = self.config.lsp_narrow
            && root.join("tsconfig.json").exists()
            && std::env::var_os("LUMEN_NO_MIRROR").is_none();
        let includes = self.ts_includes(&root);
        let mirror = if narrow {
            let m = lsp::mirror_dir(&root);
            lsp::write_mirror(&root, &m, &includes).ok().map(|_| m)
        } else {
            None
        };
        if let Some(m) = &mirror {
            crate::doc::register_mirror(m.clone(), root.clone());
        }
        match Lsp::spawn(
            "tsgo",
            &bin,
            &["--lsp", "--stdio"],
            &root,
            mirror.as_deref().unwrap_or(&root),
            MEM_LIMIT_MIB,
            self.wake(),
        ) {
            Ok(mut l) => {
                l.mirror = mirror;
                l.includes = includes;
                self.lsps.push(l);
                let ver = lsp::tsgo_version(&bin).unwrap_or_default();
                let mode = if self.lsps.last().is_some_and(|l| l.mirror.is_some()) { " (lean mode)" } else { "" };
                self.say(format!("starting tsgo {ver}{mode}…"));
                Some(self.lsps.len() - 1)
            }
            Err(e) => {
                self.say(format!("failed to start tsgo: {e}"));
                None
            }
        }
    }

    /// Auto-import candidates make completion ~200x slower and add hundreds of MB, so they are
    /// opt-in. Quick fixes (Ctrl+.) still offer "add import" for unresolved names.
    pub fn toggle_auto_import(&mut self) {
        let on = !self.config.auto_import;
        self.apply_setting("auto_import", &json!(on));
        self.say(if on {
            "auto-import in completion: on (slower, uses more RAM)"
        } else {
            "auto-import in completion: off"
        });
    }

    /// Real paths of the TS/JS documents belonging to `root`.
    fn ts_includes(&self, root: &std::path::Path) -> Vec<PathBuf> {
        self.docs
            .iter()
            .filter(|d| ts_family(d.lang))
            .filter_map(|d| d.path().map(PathBuf::from))
            .filter(|p| lsp::project_root(p).as_deref() == Some(root))
            .collect()
    }

    /// Keep each narrow mirror's `include` equal to the open documents, and tell the server.
    fn sync_mirrors(&mut self) {
        for li in 0..self.lsps.len() {
            let Some(mirror) = self.lsps[li].mirror.clone() else { continue };
            let root = self.lsps[li].root.clone();
            let mut inc = self.ts_includes(&root);
            inc.sort();
            let mut cur = self.lsps[li].includes.clone();
            cur.sort();
            if inc == cur {
                continue;
            }
            if let Ok(true) = lsp::write_mirror(&root, &mirror, &inc) {
                let uri = path_to_uri(&mirror.join("tsconfig.json"));
                self.lsps[li].notify("workspace/didChangeWatchedFiles", json!({"changes": [{"uri": uri, "type": 2}]}));
            }
            self.lsps[li].includes = inc;
        }
    }

    /// Disk changes (new/edited/deleted sources, `tsconfig`, lockfiles) reach every tsgo server and
    /// the diagnostics of the open documents are re-pulled shortly after, so importers of a changed
    /// dependency never keep stale squiggles.
    pub fn lsp_fs_events(&mut self, evs: &[crate::watcher::FsEvent]) {
        use crate::watcher::FsKind;
        let mut restart = false;
        let mut touched = false;
        for li in 0..self.lsps.len() {
            if self.lsps[li].name != "tsgo" {
                continue;
            }
            let root = self.lsps[li].root.clone();
            let mut changes = Vec::new();
            for e in evs {
                if !e.path.starts_with(&root) || !is_watched_source(&e.path) {
                    continue;
                }
                let ty = match e.kind {
                    FsKind::Created => 1,
                    FsKind::Removed => 3,
                    FsKind::Modified | FsKind::Moved => {
                        if e.path.exists() {
                            2
                        } else {
                            3
                        }
                    }
                };
                if is_project_config(&e.path) {
                    restart = true;
                }
                changes.push(json!({"uri": path_to_uri(&crate::doc::to_mirror(&e.path)), "type": ty}));
            }
            if !changes.is_empty() {
                self.lsps[li].notify("workspace/didChangeWatchedFiles", json!({"changes": changes}));
                touched = true;
            }
        }
        if restart {
            self.restart_lsp();
        } else if touched {
            self.lsp_refresh_at = Some(Instant::now() + std::time::Duration::from_millis(300));
        }
    }

    pub fn cycle_tsgo_source(&mut self) {
        self.config.tsgo_source = match self.config.tsgo_source.as_str() {
            "auto" => "project",
            "project" => "bundled",
            _ => "auto",
        }
        .to_string();
        self.config.save();
        let src = self.config.tsgo_source.clone();
        self.restart_lsp();
        self.say(format!(
            "tsgo: {src} — {}",
            match src.as_str() {
                "project" => "only the version installed in the project",
                "bundled" => "only the editor's version",
                _ => "the project's if present, otherwise the editor's",
            }
        ));
    }

    pub fn toggle_narrow(&mut self) {
        self.config.lsp_narrow = !self.config.lsp_narrow;
        self.config.save();
        let on = self.config.lsp_narrow;
        self.restart_lsp();
        self.say(if on {
            "tsgo in lean mode (opt-in: open files + imports only): less RAM, incomplete cross-file info"
        } else {
            "tsgo in full-project mode (default): sees the whole project"
        });
    }

    pub fn restart_lsp(&mut self) {
        for l in &mut self.lsps {
            crate::doc::unregister_mirror(&l.root);
            l.shutdown();
        }
        self.lsps.clear();
        for d in &mut self.docs {
            d.lsp_open = false;
            d.biome_open = false;
            d.set_source_diags(false, Vec::new());
            d.set_source_diags(true, Vec::new());
        }
        self.biome_disabled = false;
        self.say("servers restarted");
    }

    pub(crate) fn doc_text_params(d: &Doc) -> Option<Value> {
        Self::doc_open_params(d, d.uri()?)
    }

    fn doc_open_params(d: &Doc, uri: String) -> Option<Value> {
        let lang = lsp_language_id(d.lang);
        Some(json!({"textDocument": {"uri": uri, "languageId": lang, "version": d.version, "text": d.buf.to_string()}}))
    }

    // ───────────── biome (lint) ─────────────

    fn biome_root_for(&mut self, doc: usize) -> Option<PathBuf> {
        let d = self.docs.get_mut(doc)?;
        if d.biome_root.is_none() {
            let found = d.path().and_then(|p| p.parent()).and_then(|dir| {
                dir.ancestors()
                    .find(|a| a.join("biome.json").exists() || a.join("biome.jsonc").exists())
                    .map(std::path::Path::to_path_buf)
            });
            d.biome_root = Some(found);
        }
        d.biome_root.clone().flatten()
    }

    /// The Biome language server for this document's project (spawned on demand).
    fn ensure_biome(&mut self, doc: usize) -> Option<usize> {
        if self.biome_disabled
            || !matches!(self.docs.get(doc)?.lang, Some("typescript" | "tsx" | "javascript" | "jsx" | "json" | "css"))
        {
            return None;
        }
        let root = self.biome_root_for(doc)?;
        if let Some(i) = self.lsps.iter().position(|l| l.name == "biome" && l.root == root) {
            return Some(i);
        }
        let bin = root.ancestors().map(|d| d.join("node_modules/.bin/biome")).find(|p| p.exists())?;
        match Lsp::spawn("biome", &bin, &["lsp-proxy"], &root, &root, 256, self.wake()) {
            Ok(l) => {
                self.lsps.push(l);
                Some(self.lsps.len() - 1)
            }
            Err(_) => {
                self.biome_disabled = true;
                None
            }
        }
    }

    fn biome_index(&self, doc: usize) -> Option<usize> {
        let root = self.docs.get(doc)?.biome_root.clone().flatten()?;
        self.lsps.iter().position(|l| l.name == "biome" && l.root == root)
    }

    fn biome_sync(&mut self, i: usize, now: Instant, want: &mut impl FnMut(Instant)) {
        let Some(lb) = self.ensure_biome(i) else { return };
        let Some(uri) = self.docs[i].real_uri() else { return };
        if !self.docs[i].biome_open {
            if let Some(p) = Self::doc_open_params(&self.docs[i], uri) {
                self.lsps[lb].notify("textDocument/didOpen", p);
                self.docs[i].biome_open = true;
                self.docs[i].lint_changed_at = None;
            }
        } else if let Some(t) = self.docs[i].lint_changed_at {
            if t + SYNC_DEBOUNCE <= now {
                let d = &mut self.docs[i];
                d.lint_changed_at = None;
                let params = json!({"textDocument": {"uri": uri, "version": d.version}, "contentChanges": [{"text": d.buf.to_string()}]});
                self.lsps[lb].notify("textDocument/didChange", params);
            } else {
                want(t + SYNC_DEBOUNCE);
            }
        }
    }

    fn biome_close(&mut self, i: usize) {
        let Some(lb) = self.biome_index(i) else { return };
        let d = &self.docs[i];
        if d.biome_open
            && let Some(uri) = d.real_uri()
        {
            self.lsps[lb].notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}));
        }
        let root = self.lsps[lb].root.clone();
        let others = self
            .docs
            .iter()
            .enumerate()
            .any(|(j, x)| j != i && x.biome_open && x.biome_root.clone().flatten().as_ref() == Some(&root));
        if !others {
            let mut l = self.lsps.remove(lb);
            l.shutdown();
        }
    }

    /// Open new TS documents, send debounced changes, and pull diagnostics.
    pub fn lsp_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let n = self.docs.len();
        self.sync_mirrors();
        let mut pull: Vec<usize> = Vec::new(); // server indexes needing a diagnostics refresh
        let mut comp: Option<Option<char>> = None;
        if let Some((due, trig)) = self.comp_due {
            if due <= now {
                self.comp_due = None;
                comp = Some(trig);
            } else {
                want(due);
            }
        }
        for i in 0..n {
            self.biome_sync(i, now, want);
            if !ts_family(self.docs[i].lang) {
                continue;
            }
            let Some(li) = self.ensure_lsp(i) else { continue };
            if !self.docs[i].lsp_open {
                if let Some(p) = Self::doc_text_params(&self.docs[i]) {
                    self.lsps[li].notify("textDocument/didOpen", p);
                    self.docs[i].lsp_open = true;
                    self.docs[i].changed_at = None;
                    pull.push(li);
                }
                continue;
            }
            if let Some(t) = self.docs[i].changed_at {
                if t + SYNC_DEBOUNCE <= now {
                    self.send_change(i);
                    pull.push(li);
                } else {
                    want(t + SYNC_DEBOUNCE);
                }
            }
            // Semantic tokens (and, when enabled, code lens) once the text is synced.
            if self.docs[i].changed_at.is_none() {
                if let Some(t) = self.docs[i].sem_due {
                    if t <= now {
                        self.docs[i].sem_due = None;
                        if self.lsps[li].caps["semanticTokensProvider"].is_object() {
                            if self.docs[i].buf.line_count() <= crate::bigfile::BIG_LINES {
                                if let Some(uri) = self.docs[i].uri() {
                                    self.lsps[li].request(
                                        "textDocument/semanticTokens/full",
                                        json!({"textDocument": {"uri": uri}}),
                                        Pending::SemTokens { uri },
                                    );
                                }
                            } else {
                                // Big files: only a window around the viewport (below), never the whole document.
                                self.docs[i].big.semwin.reset();
                            }
                        }
                    } else {
                        want(t);
                    }
                }
                if i == self.active
                    && self.docs[i].buf.line_count() > crate::bigfile::BIG_LINES
                    && self.docs[i].sem_due.is_none()
                {
                    let range = &self.lsps[li].caps["semanticTokensProvider"]["range"];
                    if !range.is_null() && range.as_bool() != Some(false) {
                        self.sem_range_tick(i, li, now, want);
                    }
                }
                if let Some(t) = self.docs[i].lens_due {
                    if t <= now {
                        self.docs[i].lens_due = None;
                        if self.config.code_lens
                            && self.lsps[li].caps["codeLensProvider"].is_object()
                            && let Some(uri) = self.docs[i].uri()
                        {
                            self.lsps[li].request(
                                "textDocument/codeLens",
                                json!({"textDocument": {"uri": uri}}),
                                Pending::CodeLens { uri },
                            );
                        }
                    } else {
                        want(t);
                    }
                }
            }
            // Inlay hints for the visible lines only (debounced), once the text is synced.
            if self.config.inlay_hints && i == self.active && self.docs[i].changed_at.is_none() {
                self.inlay_tick(i, li, now, want);
            }
            // Outline for breadcrumbs / sticky scroll, once the text is synced.
            if let (Some(t), None) = (self.docs[i].symbols_due, self.docs[i].changed_at) {
                if t <= now {
                    self.docs[i].symbols_due = None;
                    if let Some(uri) = self.docs[i].uri() {
                        self.lsps[li].request(
                            "textDocument/documentSymbol",
                            json!({"textDocument": {"uri": uri}}),
                            Pending::SymbolsCache { uri: uri.clone() },
                        );
                        if self.lsps[li].caps["foldingRangeProvider"].as_bool() != Some(false)
                            && !self.lsps[li].caps["foldingRangeProvider"].is_null()
                        {
                            self.lsps[li].request(
                                "textDocument/foldingRange",
                                json!({"textDocument": {"uri": uri}}),
                                Pending::Folding { uri },
                            );
                        }
                    }
                } else {
                    want(t);
                }
            }
        }
        if let Some(t) = self.lsp_refresh_at {
            if t <= now {
                self.lsp_refresh_at = None;
                for li in 0..self.lsps.len() {
                    if self.lsps[li].name == "tsgo" {
                        pull.push(li);
                    }
                }
            } else {
                want(t);
            }
        }
        pull.sort_unstable();
        pull.dedup();
        for li in pull {
            self.pull_diagnostics(li);
        }
        if let Some(trig) = comp {
            if std::env::var_os("LUMEN_DEBUG").is_some() {
                eprintln!("[lsp] firing completion {trig:?}");
            }
            self.request_completion(trig);
        }
    }

    /// Ask the server for the hints of the visible lines when they are missing, the view scrolled
    /// out of the last answer, or an edit made them stale (`inlay.due`).
    fn inlay_tick(&mut self, i: usize, li: usize, now: Instant, want: &mut impl FnMut(Instant)) {
        let caps = &self.lsps[li].caps["inlayHintProvider"];
        if caps.is_null() || caps.as_bool() == Some(false) {
            return;
        }
        let d = &mut self.docs[i];
        let count = d.buf.line_count();
        let (vf, vl) = d.visible_line_span();
        if d.inlay.due.is_none() && d.inlay.needs_request(vf, vl, count) {
            // The viewport itself has no hints yet (fast scroll): ask at once (at most every 60 ms);
            // a margin refresh waits for the scroll to settle.
            let wait = if d.inlay.viewport_uncovered(vf, vl, count) {
                d.inlay.since_last_request().map_or(0, |e| 60u64.saturating_sub(e.as_millis() as u64))
            } else {
                180
            };
            d.inlay.due = Some(now + Duration::from_millis(wait));
        }
        let Some(t) = d.inlay.due else { return };
        if t > now {
            want(t);
            return;
        }
        d.inlay.due = None;
        let Some(uri) = d.uri() else { return };
        let (a, b) = crate::inlay::visible_range_request(vf, vl, count);
        let end_col: usize = d.buf.line(b).chars().map(char::len_utf16).sum();
        let params = json!({"textDocument": {"uri": uri}, "range": {"start": {"line": a, "character": 0}, "end": {"line": b, "character": end_col}}});
        let version = d.version;
        d.inlay.cover = Some((a, b));
        d.inlay.mark_requested();
        self.lsps[li].request("textDocument/inlayHint", params, Pending::InlayHint { uri, version, first: a, last: b });
    }

    /// The inlay hints setting changed: forget current hints, tell the servers, ask again.
    pub fn refresh_inlay_hints(&mut self) {
        for d in &mut self.docs {
            d.inlay.reset();
        }
        for l in &mut self.lsps {
            l.notify("workspace/didChangeConfiguration", json!({"settings": {}}));
        }
        self.request_redraw();
    }

    fn on_inlay_hints(&mut self, uri: &str, version: i32, first: usize, last: usize, v: &Value) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        let d = &mut self.docs[i];
        if d.version != version || !self.config.inlay_hints {
            return;
        }
        let mut cache: Option<(usize, String)> = None;
        let hints = crate::inlay::parse(v, &mut |line, u16col| {
            if cache.as_ref().is_none_or(|(l, _)| *l != line) {
                cache = Some((line, if line < d.buf.line_count() { d.buf.line(line) } else { String::new() }));
            }
            crate::inlay::utf16_to_char_col(&cache.as_ref().unwrap().1, u16col)
        });
        let mut hints = hints;
        // Parameter names / types are separate switches (enum member values count as types).
        let (names, types) = (self.config.inlay_hints_params, self.config.inlay_hints_types);
        hints.retain(|h| if h.kind == crate::inlay::HintKind::Parameter { names } else { types });
        let count = d.buf.line_count();
        d.inlay.apply(first, last, hints, count);
        self.request_redraw();
    }

    fn send_change(&mut self, i: usize) {
        let Some(li) = self.doc_lsp(i) else { return };
        let d = &mut self.docs[i];
        d.changed_at = None;
        let Some(uri) = d.uri() else { return };
        let params = json!({"textDocument": {"uri": uri, "version": d.version}, "contentChanges": [{"text": d.buf.to_string()}]});
        self.lsps[li].notify("textDocument/didChange", params);
    }

    /// Make sure the server has our latest text before asking it something.
    pub(crate) fn flush(&mut self, i: usize) {
        if self.docs.get(i).is_some_and(|d| d.changed_at.is_some() && d.lsp_open) {
            self.send_change(i);
        }
    }

    fn pull_diagnostics(&mut self, li: usize) {
        let root = self.lsps[li].root.clone();
        for i in 0..self.docs.len() {
            let d = &self.docs[i];
            if !d.lsp_open || d.lsp_root.as_ref() != Some(&root) {
                continue;
            }
            let Some(uri) = d.uri() else { continue };
            self.lsps[li].request(
                "textDocument/diagnostic",
                json!({"textDocument": {"uri": uri}}),
                Pending::Diagnostic { uri },
            );
        }
    }

    pub fn lsp_did_save(&mut self, i: usize) {
        if let (Some(lb), Some(uri)) = (self.biome_index(i), self.docs.get(i).and_then(|d| d.real_uri())) {
            self.lsps[lb].notify("textDocument/didSave", json!({"textDocument": {"uri": uri}}));
        }
        let Some(li) = self.doc_lsp(i) else { return };
        let Some(uri) = self.docs[i].uri() else { return };
        self.flush(i);
        self.lsps[li].notify("textDocument/didSave", json!({"textDocument": {"uri": uri}}));
        self.pull_diagnostics(li);
    }

    /// Tell the server a document is gone from this URI, without touching server lifetime.
    pub fn lsp_notify_close(&mut self, i: usize) {
        let Some(li) = self.doc_lsp(i) else { return };
        let d = &self.docs[i];
        if d.lsp_open
            && let Some(uri) = d.uri()
        {
            self.lsps[li].notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}));
        }
    }

    pub fn request_workspace_symbols(&mut self, query: String) {
        let Some((_, li)) = self.with_server() else { return };
        if query.is_empty() {
            return;
        }
        self.lsps[li].request("workspace/symbol", json!({"query": query.clone()}), Pending::WorkspaceSymbols { query });
    }

    fn on_workspace_symbols(&mut self, query: &str, v: &Value) {
        let mut items: Vec<Item> = Vec::new();
        for s in v.as_array().into_iter().flatten().take(300) {
            let name = s["name"].as_str().unwrap_or("?");
            let Some((uri, l, c)) = lsp::locations(&json!([s["location"].clone()])).into_iter().next() else {
                continue;
            };
            let Some(path) = uri_to_path(&uri) else { continue };
            let container = s["containerName"].as_str().unwrap_or("");
            let detail = if container.is_empty() {
                format!("{}:{}", self.rel(&path), l + 1)
            } else {
                format!("{container} · {}:{}", self.rel(&path), l + 1)
            };
            items.push(Item { label: name.to_string(), detail, action: Action::Open(path, Some((l, c))) });
        }
        if let crate::app_overlay::Overlay::Picker(p) = &mut self.overlay {
            // Drop answers for a query the user has already changed.
            if p.kind == crate::app_overlay::PickKind::WorkspaceSymbols && p.field.text.trim() == query {
                p.items = items;
                p.refilter();
            }
        }
    }

    pub fn lsp_did_close(&mut self, i: usize) {
        self.biome_close(i);
        let Some(li) = self.doc_lsp(i) else { return };
        let d = &self.docs[i];
        if d.lsp_open
            && let Some(uri) = d.uri()
        {
            self.lsps[li].notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}));
        }
        // Last TS document of this project gone: shut the server down and give the RAM back.
        let root = self.lsps[li].root.clone();
        let others =
            self.docs.iter().enumerate().any(|(j, x)| j != i && x.lsp_open && x.lsp_root.as_ref() == Some(&root));
        if !others {
            let mut l = self.lsps.remove(li);
            crate::doc::unregister_mirror(&l.root);
            l.shutdown();
        }
    }

    // ───────────── incoming messages ─────────────

    pub fn pump_lsp(&mut self) {
        let mut all: Vec<(usize, Msg)> = Vec::new();
        for (li, l) in self.lsps.iter().enumerate() {
            while let Ok(m) = l.rx.try_recv() {
                all.push((li, m));
            }
        }
        let mut dead: Vec<usize> = Vec::new();
        for (li, m) in all {
            self.handle_lsp_msg(li, m, &mut dead);
        }
        self.drop_dead_servers(dead);
    }

    /// Route one message from server `li` (servers that exited are only noted in `dead`).
    pub(crate) fn handle_lsp_msg(&mut self, li: usize, m: Msg, dead: &mut Vec<usize>) {
        if li >= self.lsps.len() {
            return;
        }
        match m {
            Msg::Exited => dead.push(li),
            Msg::Response { id, result } => {
                let Some(what) = self.lsps[li].pending.remove(&id) else { return };
                self.on_response(li, what, result);
            }
            Msg::Symbols { id, syms } => {
                let Some(what) = self.lsps[li].pending.remove(&id) else { return };
                self.on_symbol_list(what, syms);
            }
            Msg::Tokens { id, data } => {
                let Some(what) = self.lsps[li].pending.remove(&id) else { return };
                self.on_tokens(li, what, data);
            }
            Msg::Notification { method, params } => self.on_notification(li, &method, &params),
            Msg::Request { id, method, params } => self.on_server_request(li, id, &method, &params),
        }
    }

    pub(crate) fn drop_dead_servers(&mut self, mut dead: Vec<usize>) {
        dead.sort_unstable();
        dead.dedup();
        for li in dead.into_iter().rev() {
            if li < self.lsps.len() {
                let root = self.lsps[li].root.clone();
                crate::doc::unregister_mirror(&root);
                self.lsps.remove(li);
                for d in &mut self.docs {
                    if d.lsp_root.as_ref() == Some(&root) {
                        d.lsp_open = false;
                    }
                    if d.biome_root.clone().flatten().as_ref() == Some(&root) {
                        d.biome_open = false;
                        self.biome_disabled = true; // don't respawn a crashing server in a loop
                    }
                }
                self.say("language server exited");
            }
        }
    }

    fn on_notification(&mut self, li: usize, method: &str, params: &Value) {
        match method {
            "textDocument/publishDiagnostics" => {
                let lint = self.lsps.get(li).is_some_and(|l| l.name == "biome");
                if let Some(uri) = params["uri"].as_str() {
                    self.set_diags(uri, &params["diagnostics"], lint);
                }
            }
            "window/showMessage" => {
                if let Some(m) = params["message"].as_str() {
                    self.say(m.to_string());
                }
            }
            _ => {}
        }
    }

    fn on_server_request(&mut self, li: usize, id: Value, method: &str, params: &Value) {
        if std::env::var_os("LUMEN_DEBUG").is_some() {
            eprintln!("[lsp] server request {method}: {}", params.to_string().chars().take(400).collect::<String>());
        }
        let result = match method {
            "workspace/configuration" => {
                // tsgo asks for "js/ts", "typescript", "javascript" and "editor" settings.
                let auto = self.auto_import_on();
                let ts = json!({
                    "suggest": {"autoImports": auto, "includeCompletionsForModuleExports": auto},
                    "referencesCodeLens": {"enabled": self.config.code_lens, "showOnAllFunctions": true},
                    "implementationsCodeLens": {"enabled": self.config.code_lens},
                    "inlayHints": if self.config.inlay_hints {
                        let (names, types) = (self.config.inlay_hints_params, self.config.inlay_hints_types);
                        json!({
                            "parameterNames": {"enabled": if names { "literals" } else { "none" }, "suppressWhenArgumentMatchesName": true},
                            "parameterTypes": {"enabled": types},
                            "variableTypes": {"enabled": types, "suppressWhenTypeMatchesName": true},
                            "propertyDeclarationTypes": {"enabled": types},
                            "functionLikeReturnTypes": {"enabled": types},
                            "enumMemberValues": {"enabled": types}
                        })
                    } else {
                        json!({"parameterNames": {"enabled": "none"}, "parameterTypes": {"enabled": false}, "variableTypes": {"enabled": false}, "propertyDeclarationTypes": {"enabled": false}, "functionLikeReturnTypes": {"enabled": false}, "enumMemberValues": {"enabled": false}})
                    },
                    "preferences": {"includeCompletionsForModuleExports": auto, "includePackageJsonAutoImports": if auto { "auto" } else { "off" }}
                });
                let items = params["items"].as_array().cloned().unwrap_or_default();
                Value::Array(
                    items
                        .iter()
                        .map(|it| match it["section"].as_str() {
                            Some("js/ts" | "typescript" | "javascript") => ts.clone(),
                            _ => Value::Null,
                        })
                        .collect(),
                )
            }
            "workspace/applyEdit" => {
                self.apply_workspace_edit(&params["edit"]);
                json!({"applied": true})
            }
            "workspace/workspaceFolders" => {
                let root = self.lsps[li].root.clone();
                json!([{"uri": path_to_uri(&root), "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}])
            }
            _ => Value::Null,
        };
        self.lsps[li].respond(id, result);
    }

    pub(crate) fn doc_by_uri(&self, uri: &str) -> Option<usize> {
        let path = uri_to_path(uri)?;
        self.docs.iter().position(|d| d.path() == Some(path.as_path()))
    }

    fn set_diags(&mut self, uri: &str, items: &Value, lint: bool) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        let d = &mut self.docs[i];
        let mut out = Vec::new();
        for it in items.as_array().into_iter().flatten() {
            let r = &it["range"];
            let (Some(sl), Some(sc), Some(el), Some(ec)) = (
                r["start"]["line"].as_u64(),
                r["start"]["character"].as_u64(),
                r["end"]["line"].as_u64(),
                r["end"]["character"].as_u64(),
            ) else {
                continue;
            };
            let a = d.pos_from_lsp(sl as usize, sc as usize);
            let b = d.pos_from_lsp(el as usize, ec as usize);
            let (l0, c0) = d.buf.line_col(a);
            let (l1, c1) = d.buf.line_col(b);
            let code = match &it["code"] {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            out.push(Diag {
                sl: l0,
                sc: c0,
                el: l1,
                ec: c1,
                severity: it["severity"].as_u64().unwrap_or(1) as u8,
                message: it["message"].as_str().unwrap_or("").to_string(),
                code,
            });
        }
        out.sort_by_key(|d| (d.sl, d.sc));
        d.set_source_diags(lint, out);
    }

    fn on_response(&mut self, li: usize, what: Pending, result: Result<Value, Value>) {
        if let Pending::Initialize = what {
            match result {
                Ok(v) => {
                    self.lsps[li].on_initialized(&v);
                    self.say("tsgo ready");
                }
                Err(e) => self.say(format!("tsgo failed to start: {}", e["message"].as_str().unwrap_or("?"))),
            }
            return;
        }
        let v = match result {
            Ok(v) => v,
            Err(e) => {
                if std::env::var_os("LUMEN_DEBUG").is_some() {
                    eprintln!("[lsp] error response for {what:?}: {e}");
                }
                let msg = e["message"].as_str().unwrap_or("error").to_string();
                if matches!(what, Pending::CodeActions) {
                    return self.actions_response(Vec::new());
                }
                self.sem_range_failed(&what);
                if !matches!(
                    what,
                    Pending::Diagnostic { .. }
                        | Pending::Hover { .. }
                        | Pending::HoverExtra { .. }
                        | Pending::Completion { .. }
                        | Pending::Signature { .. }
                        | Pending::SymbolsCache { .. }
                        | Pending::Folding { .. }
                        | Pending::SemTokens { .. }
                        | Pending::SemRange { .. }
                ) {
                    self.say(format!("LSP: {msg}"));
                }
                return;
            }
        };
        match what {
            Pending::Initialize => {}
            Pending::Diagnostic { uri } => {
                if v["kind"].as_str() == Some("full") {
                    self.set_diags(&uri, &v["items"], false);
                }
                if let Some(rel) = v["relatedDocuments"].as_object() {
                    for (u, r) in rel {
                        if r["kind"].as_str() == Some("full") {
                            self.set_diags(u, &r["items"], false);
                        }
                    }
                }
            }
            Pending::Hover { uri, at } => self.on_hover(&uri, at, &v),
            Pending::HoverExtra { uri, at, serial, what } => self.hover_extra_response(&uri, at, serial, what, &v),
            Pending::Completion { uri, at } => self.on_completion(&uri, at, &v),
            Pending::Definition { kind } => {
                let locs = lsp::locations(&v);
                match locs.len() {
                    0 => self.say(match kind {
                        DefKind::Definition => "definition not found",
                        DefKind::TypeDefinition => "type definition not found",
                        DefKind::Implementation => "implementation not found",
                    }),
                    1 => self.goto_location(&locs[0]),
                    _ => self.show_locations("Definitions", &locs),
                }
            }
            Pending::References => {
                let locs = lsp::locations(&v);
                if locs.is_empty() {
                    self.say("no references");
                } else {
                    let title = format!("{} references", locs.len());
                    self.show_locations(&title, &locs);
                }
            }
            Pending::Rename => self.apply_workspace_edit(&v),
            Pending::Blocking => {}
            Pending::CodeActions => self.on_code_actions(li, &v),
            Pending::Signature { uri } => self.on_signature(&uri, &v),
            what @ (Pending::Symbols { .. } | Pending::SymbolsCache { .. }) => {
                self.on_symbol_list(what, crate::doc::flatten_symbols(&v))
            }
            Pending::ResolveCompletion { uri, version } => {
                if let Some(i) = self.doc_by_uri(&uri) {
                    let extra = parse_edits(&v["additionalTextEdits"]);
                    if !extra.is_empty() && self.docs[i].version == version {
                        self.docs[i].apply_lsp_edits(&extra);
                    }
                }
            }
            Pending::WorkspaceSymbols { query } => self.on_workspace_symbols(&query, &v),
            Pending::Organize => {
                let first = v.as_array().and_then(|a| a.first()).cloned();
                match first {
                    Some(a) if a["edit"].is_object() => self.apply_workspace_edit(&a["edit"]),
                    Some(a) => self.resolve_code_action(&a),
                    None => self.say("imports already organized"),
                }
            }
            Pending::Folding { uri } => {
                if let Some(i) = self.doc_by_uri(&uri) {
                    self.set_lsp_folds_all(i, crate::rowmap::fold_regions_from_lsp(&v));
                    self.request_redraw();
                }
            }
            what @ (Pending::SemTokens { .. } | Pending::SemRange { .. }) => {
                // Normally delivered packed as `Msg::Tokens`; a null result lands here.
                let data: Vec<u32> = v["data"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect())
                    .unwrap_or_default();
                self.on_tokens(li, what, data);
            }
            Pending::CodeLens { uri } => {
                if let Some(i) = self.doc_by_uri(&uri) {
                    let mut lenses = Vec::new();
                    for it in v.as_array().into_iter().flatten() {
                        let r = &it["range"]["start"];
                        let title = it["command"]["title"].as_str().map(str::to_string);
                        lenses.push(crate::doc::Lens {
                            line: r["line"].as_u64().unwrap_or(0) as usize,
                            col: r["character"].as_u64().unwrap_or(0) as usize,
                            title,
                            raw: it.clone(),
                        });
                    }
                    // Counts need a resolve round-trip; ask for the first batch.
                    let pending: Vec<(usize, Value)> = lenses
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| l.title.is_none())
                        .take(120)
                        .map(|(k, l)| (k, l.raw.clone()))
                        .collect();
                    self.docs[i].lenses = lenses;
                    for (_, raw) in pending {
                        let line = raw["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
                        self.lsps[li].request("codeLens/resolve", raw, Pending::ResolveLens { uri: uri.clone(), line });
                    }
                }
            }
            Pending::ResolveLens { uri, line } => {
                if let Some(i) = self.doc_by_uri(&uri)
                    && let Some(title) = v["command"]["title"].as_str()
                {
                    let c = v["range"]["start"]["character"].as_u64().unwrap_or(0) as usize;
                    if let Some(l) =
                        self.docs[i].lenses.iter_mut().find(|l| l.line == line && l.col == c && l.title.is_none())
                    {
                        l.title = Some(title.to_string());
                    }
                    self.request_redraw();
                }
            }
            Pending::InlayHint { uri, version, first, last } => self.on_inlay_hints(&uri, version, first, last, &v),
            Pending::SelectionRange => self.on_selection_range(&v),
            Pending::PrepareCalls { incoming } => self.on_prepare_calls(incoming, &v),
            Pending::Calls { incoming } => self.on_calls(incoming, &v),
            Pending::ResolveAction => {
                if v["edit"].is_object() {
                    self.apply_workspace_edit(&v["edit"]);
                } else {
                    self.say("action made no changes");
                }
            }
            Pending::ExecuteCommand => {}
        }
    }

    // ───────────── edits across files ─────────────

    pub fn apply_workspace_edit(&mut self, edit: &Value) {
        let mut per_file: Vec<(String, Vec<LspEdit>)> = Vec::new();
        if let Some(ch) = edit["changes"].as_object() {
            for (uri, edits) in ch {
                per_file.push((uri.clone(), parse_edits(edits)));
            }
        }
        for dc in edit["documentChanges"].as_array().into_iter().flatten() {
            if let Some(uri) = dc["textDocument"]["uri"].as_str() {
                per_file.push((uri.to_string(), parse_edits(&dc["edits"])));
            }
        }
        if per_file.is_empty() {
            self.say("nothing to change");
            return;
        }
        let prev = self.active;
        let (mut n_edits, mut n_files) = (0, 0);
        for (uri, edits) in per_file {
            if edits.is_empty() {
                continue;
            }
            let Some(path) = uri_to_path(&uri) else { continue };
            let idx = match self.docs.iter().position(|d| d.path() == Some(path.as_path())) {
                Some(i) => i,
                None => match self.open_path(&path, None) {
                    Some(i) => i,
                    None => continue,
                },
            };
            n_edits += edits.len();
            n_files += 1;
            self.docs[idx].apply_lsp_edits(&edits);
        }
        if prev < self.docs.len() {
            self.active = prev;
        }
        self.say(format!("{n_edits} changes in {n_files} file(s) — unsaved, Ctrl+Z undoes"));
    }

    // ───────────── requests ─────────────

    pub fn with_server_pub(&mut self) -> Option<(usize, usize)> {
        self.with_server()
    }

    fn with_server(&mut self) -> Option<(usize, usize)> {
        let i = self.active;
        let li = self.doc_lsp(i)?;
        if !self.docs[i].lsp_open {
            return None;
        }
        self.flush(i);
        Some((i, li))
    }

    fn pos_params(&self, i: usize, at: usize) -> Value {
        let d = &self.docs[i];
        let (l, c) = d.lsp_pos(at);
        json!({"textDocument": {"uri": d.uri().unwrap_or_default()}, "position": {"line": l, "character": c}})
    }

    pub fn request_hover(&mut self, at: usize) {
        if self.hover_from_cache(at) {
            return;
        }
        let Some((i, li)) = self.with_server() else { return self.show_diag_hover(at) };
        let params = self.pos_params(i, at);
        let uri = self.docs[i].uri().unwrap_or_default();
        self.lsps[li].request("textDocument/hover", params, Pending::Hover { uri, at });
    }

    pub fn request_hover_at_cursor(&mut self) {
        if let Some(d) = self.docs.get(self.active) {
            let h = d.ed.head();
            self.request_hover(h);
        }
    }

    fn show_diag_hover(&mut self, at: usize) {
        let i = self.active;
        if i < self.docs.len() {
            self.hover_show_diags(i, at);
        }
    }

    fn on_hover(&mut self, uri: &str, at: usize, v: &Value) {
        self.hover_response(uri, at, v);
    }

    pub fn goto_definition(&mut self, kind: DefKind) {
        let Some((i, li)) = self.with_server() else { return self.say("no language server for this file") };
        let at = self.docs[i].ed.head();
        let params = self.pos_params(i, at);
        let method = match kind {
            DefKind::Definition => "textDocument/definition",
            DefKind::TypeDefinition => "textDocument/typeDefinition",
            DefKind::Implementation => "textDocument/implementation",
        };
        self.lsps[li].request(method, params, Pending::Definition { kind });
    }

    pub fn find_references(&mut self) {
        let Some((i, li)) = self.with_server() else { return self.say("no language server for this file") };
        let at = self.docs[i].ed.head();
        let mut params = self.pos_params(i, at);
        params["context"] = json!({"includeDeclaration": true});
        self.lsps[li].request("textDocument/references", params, Pending::References);
    }

    fn word_at(&self, i: usize) -> Option<(String, usize)> {
        let d = &self.docs[i];
        let h = d.ed.head();
        let mut s = h;
        while s > 0 && d.buf.char_at(s - 1).is_some_and(is_ident) {
            s -= 1;
        }
        let mut e = h;
        while d.buf.char_at(e).is_some_and(is_ident) {
            e += 1;
        }
        (s < e).then(|| (d.buf.slice(s, e), s))
    }

    pub fn start_rename(&mut self) {
        let i = self.active;
        if i >= self.docs.len() || self.doc_lsp(i).is_none() {
            return self.say("rename needs the language server (TS/JS file)");
        }
        let Some((word, start)) = self.word_at(i) else { return self.say("place the cursor on a symbol") };
        let (l, c) = self.docs[i].lsp_pos(start);
        self.open_prompt_rename(&word, l, c);
    }

    pub fn send_rename(&mut self, line: usize, col: usize, name: String) {
        if name.is_empty() {
            return;
        }
        let Some((i, li)) = self.with_server() else { return };
        let uri = self.docs[i].uri().unwrap_or_default();
        let params =
            json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": col}, "newName": name});
        self.lsps[li].request("textDocument/rename", params, Pending::Rename);
    }

    pub fn organize_imports(&mut self) {
        let Some((i, li)) = self.with_server() else {
            return self.say("organize imports needs the language server (TS/JS file)");
        };
        let d = &self.docs[i];
        let at = d.ed.head();
        let range = lsp_range(d, at, at);
        let uri = d.uri().unwrap_or_default();
        let params = json!({"textDocument": {"uri": uri}, "range": range, "context": {"diagnostics": [], "only": ["source.organizeImports"]}});
        self.lsps[li].request("textDocument/codeAction", params, Pending::Organize);
    }

    pub fn open_symbols(&mut self) {
        let Some((i, li)) = self.with_server() else {
            return self.say("symbols need the language server (TS/JS file)");
        };
        let uri = self.docs[i].uri().unwrap_or_default();
        self.lsps[li].request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
            Pending::Symbols { uri },
        );
    }

    /// A flattened outline arrived: for breadcrumbs / sticky scroll, or for the "go to symbol" picker.
    fn on_symbol_list(&mut self, what: Pending, syms: Vec<crate::doc::Sym>) {
        match what {
            Pending::SymbolsCache { uri } => {
                if let Some(i) = self.doc_by_uri(&uri) {
                    if std::env::var_os("LUMEN_DEBUG").is_some() {
                        eprintln!("[lsp] outline: {} symbols", syms.len());
                    }
                    self.docs[i].symbols = syms;
                    self.request_redraw();
                }
            }
            Pending::Symbols { uri } => self.on_symbols(&uri, &syms),
            _ => {}
        }
    }

    fn on_symbols(&mut self, uri: &str, syms: &[crate::doc::Sym]) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        let d = &self.docs[i];
        let mut items: Vec<Item> = Vec::with_capacity(syms.len());
        for s in syms {
            let kind = match s.kind {
                2 => "module",
                5 => "class",
                6 => "method",
                7 => "property",
                8 => "field",
                9 => "constructor",
                10 => "enum",
                11 => "interface",
                12 => "function",
                13 => "variable",
                14 => "constant",
                22 => "enum member",
                23 => "struct",
                26 => "type",
                _ => "",
            };
            items.push(Item {
                label: format!("{}{}", "  ".repeat(s.depth), s.name),
                detail: kind.to_string(),
                action: Action::Goto(d.pos_from_lsp(s.sel_l, s.sel_c)),
            });
        }
        if items.is_empty() {
            return self.say("no symbols");
        }
        self.open_symbols_with(items);
    }

    pub fn code_actions(&mut self) {
        let i = self.active;
        let Some(d) = self.docs.get(i) else { return };
        let sel = d.ed.sel;
        let range = lsp_range(d, sel.start(), sel.end());
        let (l0, c0) = d.buf.line_col(sel.start());
        let (l1, c1) = d.buf.line_col(sel.end());
        let diags: Vec<Value> = d
            .diags
            .iter()
            .filter(|g| (g.sl, g.sc) <= (l1, c1) && (l0, c0) <= (g.el, g.ec.max(g.sc + 1)))
            .map(|g| {
                let (sl, sc) = d.lsp_pos(d.buf.pos(g.sl, g.sc));
                let (el, ec) = d.lsp_pos(d.buf.pos(g.el, g.ec));
                let mut o = json!({"range": {"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}}, "message": g.message, "severity": g.severity, "source": "ts"});
                o["code"] = g.code.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::from(g.code.clone()));
                o
            })
            .collect();
        let mk = |uri: &str| json!({"textDocument": {"uri": uri}, "range": range.clone(), "context": {"diagnostics": diags.clone()}});
        let ts_uri = d.uri().unwrap_or_default();
        let real_uri = d.real_uri().unwrap_or_default();
        self.actions_items.clear();
        self.actions_expected = 0;
        if let Some((_, li)) = self.with_server() {
            self.lsps[li].request("textDocument/codeAction", mk(&ts_uri), Pending::CodeActions);
            self.actions_expected += 1;
        }
        if let Some(lb) = self.biome_index(i) {
            self.lsps[lb].request("textDocument/codeAction", mk(&real_uri), Pending::CodeActions);
            self.actions_expected += 1;
        }
        if self.actions_expected == 0 {
            self.say("actions need a language server (TS/JS file or project with Biome)");
        }
    }

    fn on_code_actions(&mut self, _li: usize, v: &Value) {
        let mut items = Vec::new();
        for a in v.as_array().into_iter().flatten() {
            let Some(title) = a["title"].as_str() else { continue };
            let action = if a["edit"].is_object() {
                Action::WorkspaceEdit(a["edit"].clone())
            } else if a["data"].is_object() || a["command"].is_object() {
                // Resolved lazily when chosen.
                Action::WorkspaceEdit(json!({"__resolve": a}))
            } else {
                continue;
            };
            items.push(Item { label: title.to_string(), detail: a["kind"].as_str().unwrap_or("").to_string(), action });
        }
        self.actions_response(items);
    }

    /// One server answered; when all have, show the combined list.
    fn actions_response(&mut self, items: Vec<Item>) {
        self.actions_items.extend(items);
        self.actions_expected = self.actions_expected.saturating_sub(1);
        if self.actions_expected > 0 {
            return;
        }
        let all = std::mem::take(&mut self.actions_items);
        if all.is_empty() {
            return self.say("no actions available here");
        }
        self.open_code_actions_with(all);
    }

    /// Code actions that arrive without an edit are resolved (or executed) when chosen.
    pub fn resolve_code_action(&mut self, action: &Value) {
        let Some((_, li)) = self.with_server() else { return };
        if action["data"].is_object() && !action["command"].is_object() {
            self.lsps[li].request("codeAction/resolve", action.clone(), Pending::ResolveAction);
        } else if action["command"].is_object() {
            let cmd = &action["command"];
            self.lsps[li].request(
                "workspace/executeCommand",
                json!({"command": cmd["command"], "arguments": cmd["arguments"]}),
                Pending::ExecuteCommand,
            );
        }
    }

    // ───────────── selection range / call hierarchy ─────────────

    pub fn expand_selection(&mut self) {
        let Some((i, li)) = self.with_server() else { return self.say("expand selection needs the language server") };
        let at = self.docs[i].ed.head();
        let (l, c) = self.docs[i].lsp_pos(at);
        let uri = self.docs[i].uri().unwrap_or_default();
        let params = json!({"textDocument": {"uri": uri}, "positions": [{"line": l, "character": c}]});
        self.lsps[li].request("textDocument/selectionRange", params, Pending::SelectionRange);
    }

    pub fn shrink_selection(&mut self) {
        if let Some(d) = self.docs.get_mut(self.active)
            && let Some(s) = d.sel_stack.pop()
        {
            d.ed.sel = s;
            d.reveal = true;
        }
    }

    fn on_selection_range(&mut self, v: &Value) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let cur = d.ed.sel;
        let mut node = v.get(0);
        while let Some(n) = node {
            let r = &n["range"];
            if let (Some(sl), Some(sc), Some(el), Some(ec)) = (
                r["start"]["line"].as_u64(),
                r["start"]["character"].as_u64(),
                r["end"]["line"].as_u64(),
                r["end"]["character"].as_u64(),
            ) {
                let (s, e) = (d.pos_from_lsp(sl as usize, sc as usize), d.pos_from_lsp(el as usize, ec as usize));
                if s <= cur.start() && e >= cur.end() && (s, e) != (cur.start(), cur.end()) {
                    d.sel_stack.push(cur);
                    d.ed.sel = crate::buffer::Sel { anchor: s, head: e };
                    d.reveal = true;
                    return;
                }
            }
            node = n.get("parent").filter(|p| p.is_object());
        }
    }

    pub fn call_hierarchy(&mut self, incoming: bool) {
        let Some((i, li)) = self.with_server() else { return self.say("call hierarchy needs the language server") };
        let at = self.docs[i].ed.head();
        let params = self.pos_params(i, at);
        self.lsps[li].request("textDocument/prepareCallHierarchy", params, Pending::PrepareCalls { incoming });
    }

    fn on_prepare_calls(&mut self, incoming: bool, v: &Value) {
        let Some(item) = v.as_array().and_then(|a| a.first()).cloned() else {
            return self.say("no function/method under the cursor");
        };
        let Some((_, li)) = self.with_server() else { return };
        let method = if incoming { "callHierarchy/incomingCalls" } else { "callHierarchy/outgoingCalls" };
        self.lsps[li].request(method, json!({"item": item}), Pending::Calls { incoming });
    }

    fn on_calls(&mut self, incoming: bool, v: &Value) {
        let mut items = Vec::new();
        for c in v.as_array().into_iter().flatten() {
            let it = if incoming { &c["from"] } else { &c["to"] };
            let (Some(name), Some(uri)) = (it["name"].as_str(), it["uri"].as_str()) else { continue };
            let Some(path) = uri_to_path(uri) else { continue };
            let r = &it["selectionRange"]["start"];
            let (l, col) = (r["line"].as_u64().unwrap_or(0) as usize, r["character"].as_u64().unwrap_or(0) as usize);
            items.push(Item {
                label: name.to_string(),
                detail: format!("{}:{}", self.rel(&path), l + 1),
                action: Action::Open(path, Some((l, col))),
            });
        }
        if items.is_empty() {
            return self.say(if incoming { "nothing calls this" } else { "calls nothing" });
        }
        self.open_locations(if incoming { "Callers" } else { "Callees" }, items);
    }

    pub fn request_signature(&mut self) {
        let Some((i, li)) = self.with_server() else { return };
        let at = self.docs[i].ed.head();
        let params = self.pos_params(i, at);
        let uri = self.docs[i].uri().unwrap_or_default();
        self.lsps[li].request("textDocument/signatureHelp", params, Pending::Signature { uri });
    }

    fn on_signature(&mut self, uri: &str, v: &Value) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        let sigs = v["signatures"].as_array();
        let Some(sig) = sigs.and_then(|s| s.get(v["activeSignature"].as_u64().unwrap_or(0) as usize)) else { return };
        let label = sig["label"].as_str().unwrap_or("");
        let doc = lsp::hover_text(&json!({"contents": sig["documentation"].clone()}));
        let _ = i;
        let text = if doc.is_empty() {
            format!("```typescript\n{label}\n```")
        } else {
            format!("```typescript\n{label}\n```\n\n{doc}")
        };
        self.hover_set_markdown(&text);
    }

    // ───────────── locations ─────────────

    pub(crate) fn goto_location(&mut self, loc: &(String, usize, usize)) {
        let Some(path) = uri_to_path(&loc.0) else { return };
        self.push_nav();
        if let Some(i) = self.open_path(&path, None) {
            let d = &mut self.docs[i];
            let p = d.pos_from_lsp(loc.1, loc.2);
            d.ed.click(&mut d.buf, p, false);
            d.reveal = true;
        }
    }

    fn show_locations(&mut self, title: &str, locs: &[(String, usize, usize)]) {
        let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let mut items = Vec::new();
        for (uri, line, col) in locs.iter().take(500) {
            let Some(path) = uri_to_path(uri) else { continue };
            let text = cache
                .entry(path.clone())
                .or_insert_with(|| {
                    std::fs::read_to_string(&path).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
                })
                .get(*line)
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            // Columns here are UTF-16; for display and cursor placement treat them as char columns.
            items.push(Item {
                label: text,
                detail: format!("{}:{}", self.rel(&path), line + 1),
                action: Action::Open(path, Some((*line, *col))),
            });
        }
        self.open_locations(title, items);
    }

    pub fn next_diagnostic(&mut self, forward: bool) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if d.diags.is_empty() {
            return self.say("no problems in this file");
        }
        let (l, c) = d.buf.line_col(d.ed.head());
        let cur = (l, c);
        let pick = if forward {
            d.diags.iter().find(|g| (g.sl, g.sc) > cur).or_else(|| d.diags.first())
        } else {
            d.diags.iter().rev().find(|g| (g.sl, g.sc) < cur).or_else(|| d.diags.last())
        };
        let Some(g) = pick.cloned() else { return };
        let p = d.buf.pos(g.sl, g.sc);
        d.ed.click(&mut d.buf, p, false);
        d.reveal = true;
        let icon = if g.severity == 1 { "\u{f057}" } else { "\u{f071}" };
        self.say(format!("{icon} {}", g.message.lines().next().unwrap_or("")));
    }

    // ───────────── completion ─────────────

    pub fn request_completion(&mut self, trigger: Option<char>) {
        let Some((i, li)) = self.with_server() else {
            if std::env::var_os("LUMEN_DEBUG").is_some() {
                eprintln!("[lsp] completion skipped: no server");
            }
            return;
        };
        let at = self.docs[i].ed.head();
        let mut params = self.pos_params(i, at);
        params["context"] = match trigger {
            Some(c) => json!({"triggerKind": 2, "triggerCharacter": c.to_string()}),
            None => json!({"triggerKind": 1}),
        };
        self.comp_t0 = Some(Instant::now());
        let uri = self.docs[i].uri().unwrap_or_default();
        self.lsps[li].request("textDocument/completion", params, Pending::Completion { uri, at });
    }

    fn on_completion(&mut self, uri: &str, at: usize, v: &Value) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        if i != self.active {
            return;
        }
        let list = v.get("items").unwrap_or(v);
        let all = list.as_array().map(Vec::as_slice).unwrap_or(&[]);
        if std::env::var_os("LUMEN_DEBUG").is_some() {
            eprintln!("[lsp] completion n={} in {:?}", all.len(), self.comp_t0.map(|t| t.elapsed()));
        }
        let d = &self.docs[i];
        let head = d.ed.head();
        if head < at {
            return;
        }
        // The word being typed starts where identifier characters stop.
        let mut start = head;
        while start > 0 && d.buf.char_at(start - 1).is_some_and(is_ident) {
            start -= 1;
        }
        let prefix = d.buf.slice(start, head);

        // Servers return thousands of candidates (every auto-import in the program). Keep only
        // those that can still match what was typed, best first: later keystrokes only narrow it.
        let mut scored: Vec<(i32, &Value)> = Vec::new();
        for it in all {
            let Some(label) = it["label"].as_str() else { continue };
            let score = if prefix.is_empty() {
                0
            } else {
                match fuzzy(&prefix, label) {
                    Some((s, _)) => s + if label.to_lowercase().starts_with(&prefix.to_lowercase()) { 40 } else { 0 },
                    None => continue,
                }
            };
            scored.push((score, it));
        }
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        scored.truncate(1500);

        let mut items: Vec<CItem> = Vec::with_capacity(scored.len());
        for (_, it) in scored {
            let label = it["label"].as_str().unwrap_or("");
            let te = it.get("textEdit").filter(|t| t.is_object() && t.get("range").is_some());
            let insert = te
                .and_then(|t| t["newText"].as_str())
                .or_else(|| it["insertText"].as_str())
                .unwrap_or(label)
                .to_string();
            items.push(CItem {
                label: label.to_string(),
                detail: it["detail"]
                    .as_str()
                    .or_else(|| it["labelDetails"]["description"].as_str())
                    .unwrap_or("")
                    .to_string(),
                insert,
                kind: it["kind"].as_u64().unwrap_or(0) as u32,
                sort: it["sortText"].as_str().unwrap_or(label).to_string(),
                range: te.and_then(parse_edit),
                raw: it.clone(),
            });
        }
        if items.is_empty() {
            self.completion = None;
            return;
        }
        let mut c =
            Completion { items, filtered: Vec::new(), sel: 0, scroll: 0, start, seed_len: prefix.chars().count() };
        self.refilter_completion_into(&mut c);
        self.completion = if c.filtered.is_empty() { None } else { Some(c) };
        release_memory();
        self.request_redraw();
    }

    fn refilter_completion_into(&self, c: &mut Completion) {
        let Some(d) = self.docs.get(self.active) else { return };
        let head = d.ed.head();
        let prefix = if head >= c.start { d.buf.slice(c.start, head) } else { String::new() };
        let mut scored: Vec<(i32, usize)> = c
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                if prefix.is_empty() {
                    return Some((0, i));
                }
                let (mut s, _) = fuzzy(&prefix, &it.label)?;
                if it.label.to_lowercase().starts_with(&prefix.to_lowercase()) {
                    s += 40;
                }
                Some((s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| c.items[a.1].sort.cmp(&c.items[b.1].sort)));
        c.filtered = scored.into_iter().take(200).map(|(_, i)| i).collect();
        c.sel = 0;
        c.scroll = 0;
    }

    /// Called after every keystroke that may have changed text or cursor.
    pub fn on_typed(&mut self, typed: Option<char>) {
        let i = self.active;
        let Some(d) = self.docs.get(i) else { return };
        let ts = ts_family(d.lang);
        let head = d.ed.head();
        if std::env::var_os("LUMEN_DEBUG").is_some() {
            eprintln!("[lsp] on_typed {typed:?} ts={ts} lang={:?} comp_open={}", d.lang, self.completion.is_some());
        }
        if let Some(mut c) = self.completion.take() {
            let still = head >= c.start && (c.start..head).all(|p| d.buf.char_at(p).is_some_and(is_ident));
            match typed {
                Some(ch) if is_ident(ch) && still => {
                    self.refilter_completion_into(&mut c);
                    if !c.filtered.is_empty() {
                        self.completion = Some(c);
                    }
                }
                None if still && head - c.start >= c.seed_len => {
                    self.refilter_completion_into(&mut c);
                    if !c.filtered.is_empty() {
                        self.completion = Some(c);
                    }
                }
                // Deleted past the prefix the list was fetched for: ask the server again.
                None if still && head > c.start => {
                    self.comp_due = Some((Instant::now() + Duration::from_millis(90), None))
                }
                _ => {}
            }
        }
        if !ts {
            return;
        }
        match typed {
            Some('.') => self.comp_due = Some((Instant::now() + Duration::from_millis(40), Some('.'))),
            Some(ch) if is_ident(ch) && self.completion.is_none() => {
                // Typing a number literal (the word starts with a digit) is not a completion context.
                let mut ws = head;
                while ws > 0 && d.buf.char_at(ws - 1).is_some_and(is_ident) {
                    ws -= 1;
                }
                if !d.buf.char_at(ws).is_some_and(|c| c.is_ascii_digit()) {
                    self.comp_due = Some((Instant::now() + Duration::from_millis(90), None));
                }
            }
            _ => {}
        }
    }

    pub fn completion_key(&mut self, key: &Key) -> bool {
        let Some(c) = self.completion.as_mut() else { return false };
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                c.sel = (c.sel + 1) % c.filtered.len();
            }
            Key::Named(NamedKey::ArrowUp) => {
                c.sel = (c.sel + c.filtered.len() - 1) % c.filtered.len();
            }
            Key::Named(NamedKey::Escape) => self.completion = None,
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => self.accept_completion(),
            _ => return false,
        }
        if let Some(c) = self.completion.as_mut() {
            if c.sel < c.scroll {
                c.scroll = c.sel;
            } else if c.sel >= c.scroll + 10 {
                c.scroll = c.sel + 1 - 10;
            }
        }
        true
    }

    pub fn completion_click(&mut self, x: f64, y: f64) -> bool {
        let rows = self.layout.completion_rows.clone();
        if self.completion.is_none() {
            return false;
        }
        if let Some((_, k)) = rows.iter().find(|(r, _)| r.contains(x, y)) {
            if let Some(c) = self.completion.as_mut() {
                c.sel = *k;
            }
            self.accept_completion();
            return true;
        }
        self.completion = None;
        false
    }

    fn accept_completion(&mut self) {
        let Some(c) = self.completion.take() else { return };
        let Some(&idx) = c.filtered.get(c.sel) else { return };
        let item = c.items[idx].clone();
        let i = self.active;
        let d = &mut self.docs[i];
        let head = d.ed.head();
        let (s, e) = match &item.range {
            Some(r) => (d.pos_from_lsp(r.sl, r.sc), d.pos_from_lsp(r.el, r.ec).max(head)),
            None => (c.start, head),
        };
        d.buf.close_group();
        let before = d.ed.sel;
        d.buf.replace(s, e, &item.insert, EditKind::Other, before);
        d.ed.sel = crate::buffer::Sel::caret(s + item.insert.chars().count());
        d.buf.set_after(d.ed.sel);
        d.buf.close_group();
        d.reveal = true;
        d.sync_edits();
        let version = d.version;
        let uri = d.uri().unwrap_or_default();

        // Auto-imports and friends come in `additionalTextEdits`, often only after resolve.
        let extra = parse_edits(&item.raw["additionalTextEdits"]);
        if !extra.is_empty() {
            self.docs[i].apply_lsp_edits(&extra);
        } else if let Some(li) = self.doc_lsp(i) {
            let can = self.lsps[li].caps["completionProvider"]["resolveProvider"].as_bool().unwrap_or(false);
            if can {
                self.lsps[li].request(
                    "completionItem/resolve",
                    item.raw.clone(),
                    Pending::ResolveCompletion { uri, version },
                );
            }
        }
    }

    // ───────────── drawing ─────────────

    pub fn draw_completion(&mut self, cv: &mut Canvas) {
        self.layout.completion_rows.clear();
        // Drop a stale popup: the cursor left the word it was opened for (jump, click, goto…).
        if let (Some(c), Some(d)) = (&self.completion, self.docs.get(self.active)) {
            let head = d.ed.head();
            let still = head >= c.start && (c.start..head).all(|p| d.buf.char_at(p).is_some_and(is_ident));
            if !still {
                self.completion = None;
            }
        }
        let Some(c) = &self.completion else { return };
        let Some(d) = self.docs.get(self.active) else { return };
        let Some((x, y)) = d.screen_pos(c.start) else { return };
        let chrome = self.theme.chrome;
        let cell = self.tx.cell_w;
        let ed = self.layout.editor;
        let below = ed.bottom() - (y + line_h() as i32 + 2) - 6;
        let above = y - ed.y - 2 - 6;
        let go_below = below >= 22 * 4 || below >= above;
        let room = if go_below { below } else { above };
        let rows = c.filtered.len().min(10).min((room / 22).max(1) as usize);
        let widest = c
            .filtered
            .iter()
            .map(|&i| c.items[i].label.chars().count() + c.items[i].detail.chars().count().min(30) + 4)
            .max()
            .unwrap_or(20)
            .clamp(24, 70);
        let w = (widest as f32 * cell) as i32 + 28;
        let h = rows as i32 * 22 + 6;
        let ry = if go_below { y + line_h() as i32 + 2 } else { y - h - 2 };
        let rx = x.min(ed.right() - w - 4).max(ed.x);
        let r = Rect::new(rx, ry, w, h);
        cv.shadow(r, 10);
        cv.card(r, 10, chrome.widget_bg, chrome.border);
        let fg = self.theme.foreground;
        let dim = self.theme.chrome.tab_inactive_fg;
        let mut layout_rows = Vec::new();
        // Mid-glide: rows shift up by a fraction of a row, one more shows at the bottom, all clipped.
        let off = self.region_off(crate::scrollreg::Reg::Completion, 22);
        let lclip = Rect::new(r.x + 2, r.y + 3, r.w - 4, rows as i32 * 22);
        for (k, &i) in c.filtered.iter().enumerate().skip(c.scroll).take(rows + (off > 0) as usize) {
            let it = &c.items[i];
            let yy = r.y + 3 + (k - c.scroll) as i32 * 22 - off;
            let row_full = Rect::new(r.x + 2, yy, r.w - 4, 22);
            let rr = row_full.intersect(&lclip);
            if k == c.sel {
                cv.fill_clip(row_full, chrome.list_active_bg, lclip);
            }
            let (glyph, col) = kind_glyph(it.kind, &self.theme);
            self.tx.draw_line(cv, rr.x + 6, yy + 1, glyph, col, rr);
            self.tx.draw_line(cv, rr.x + 6 + self.tx.cells(2), yy + 1, &it.label, fg, rr);
            if !it.detail.is_empty() {
                let dx = rr.x + 6 + self.tx.cells(2 + it.label.chars().count() + 1);
                let room = ((rr.right() - dx - 6) as f32 / cell) as usize;
                let det: String = it.detail.chars().take(room).collect();
                self.tx.draw_line(cv, dx, yy + 1, &det, dim, rr);
            }
            layout_rows.push((rr, k));
        }
        let (total, pos) = (c.filtered.len(), c.scroll);
        self.layout.completion_rows = layout_rows;
        if total > rows {
            self.region_bar(
                cv,
                crate::scrollreg::Reg::Completion,
                r,
                total as f64,
                rows as f64,
                pos as f64,
                22.0,
                true,
            );
        }
    }
}

/// Hand freed heap pages back to the OS (large JSON responses otherwise stay resident).
pub fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}

fn kind_glyph(kind: u32, t: &crate::theme::Theme) -> (&'static str, Rgba) {
    let c = &t.chrome;
    match kind {
        2..=4 => ("ƒ", Rgba(0xb1, 0x80, 0xd7, 0xff)),
        5 | 10 => ("o", Rgba(0x75, 0xbe, 0xff, 0xff)),
        6 => ("x", Rgba(0x75, 0xbe, 0xff, 0xff)),
        7 | 22 => ("C", Rgba(0xee, 0x9d, 0x28, 0xff)),
        8 => ("I", c.info),
        9 => ("M", c.info),
        13 | 20 => ("E", Rgba(0xee, 0x9d, 0x28, 0xff)),
        14 => ("k", t.foreground),
        21 => ("c", Rgba(0x75, 0xbe, 0xff, 0xff)),
        25 => ("T", c.info),
        _ => ("·", t.chrome.tab_inactive_fg),
    }
}

// ───────────── a document's language changed (bottom bar picker) ─────────────

/// Files whose on-disk changes the TypeScript server must hear about.
fn is_watched_source(p: &std::path::Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if is_project_config(p) || name == "package.json" {
        return true;
    }
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "json")
    )
}

/// Changes that alter how the whole project is resolved: the server is restarted.
fn is_project_config(p: &std::path::Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    ((name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json"))
        || matches!(name, "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "bun.lockb")
}

/// The LSP `languageId` sent in `textDocument/didOpen` for a document language.
pub fn lsp_language_id(lang: Option<&str>) -> &'static str {
    match lang {
        Some("tsx") => "typescriptreact",
        Some("jsx") => "javascriptreact",
        Some("javascript") => "javascript",
        Some("json") => "json",
        Some("css") => "css",
        _ => "typescript",
    }
}

fn biome_lang(lang: Option<&str>) -> bool {
    matches!(lang, Some("typescript" | "tsx" | "javascript" | "jsx" | "json" | "css"))
}

/// What changing a document's language means for the servers: which ones must be told the old
/// document is gone, and which ones will pick it up again (the regular sync tick sends `didOpen`
/// with the new `languageId` and starts a server that is not running yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LangPlan {
    pub ts_close: bool,
    pub ts_after: bool,
    pub biome_close: bool,
    pub biome_after: bool,
}

pub fn lang_change_plan(old: Option<&str>, new: Option<&str>) -> LangPlan {
    let same_id = lsp_language_id(old) == lsp_language_id(new);
    LangPlan {
        ts_close: ts_family(old) && (!ts_family(new) || !same_id),
        ts_after: ts_family(new),
        biome_close: biome_lang(old) && (!biome_lang(new) || !same_id),
        biome_after: biome_lang(new),
    }
}

impl App {
    /// `didClose` only: the server keeps running (the document is reopened with another language id).
    fn biome_notify_close(&mut self, i: usize) {
        let Some(lb) = self.biome_index(i) else { return };
        if let Some(uri) = self.docs.get(i).and_then(|d| d.real_uri()) {
            self.lsps[lb].notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}));
        }
    }

    /// Document `i` switched from language `old` to `new` (already stored in `Doc::lang`): close it
    /// on the servers that no longer fit, forget what they told us and let the sync tick reopen it
    /// with the new `languageId` (starting tsgo / Biome if the new language needs one).
    pub fn lsp_language_changed(&mut self, i: usize, old: Option<&'static str>, new: Option<&'static str>) {
        if i >= self.docs.len() {
            return;
        }
        let plan = lang_change_plan(old, new);
        if plan.ts_close && self.docs[i].lsp_open {
            if plan.ts_after {
                // `doc_lsp` keys off the *current* language, which is already the new one: still tsgo.
                self.lsp_notify_close(i);
            } else {
                // Old language was served by tsgo, the new one is not: close it (the last document
                // of a project also shuts the server down). `doc_lsp` needs the old language back.
                self.docs[i].lang = old;
                self.lsp_did_close(i);
                self.docs[i].lang = new;
            }
        }
        if plan.biome_close && self.docs[i].biome_open {
            if plan.biome_after {
                self.biome_notify_close(i);
            } else {
                self.biome_close(i);
            }
        }
        let d = &mut self.docs[i];
        if plan.ts_close {
            d.lsp_open = false;
        }
        if plan.biome_close {
            d.biome_open = false;
        }
        if !plan.ts_after {
            d.set_source_diags(false, Vec::new());
            d.sem.clear();
            d.symbols.clear();
            d.lenses.clear();
        } else {
            let soon = Instant::now() + Duration::from_millis(400);
            d.symbols_due = Some(soon);
            d.sem_due = Some(soon);
        }
        if !plan.biome_after {
            d.set_source_diags(true, Vec::new());
        }
        self.request_redraw();
    }
}

#[cfg(test)]
mod lang_tests {
    use super::*;

    #[test]
    fn language_ids_follow_the_document_language() {
        assert_eq!(lsp_language_id(Some("tsx")), "typescriptreact");
        assert_eq!(lsp_language_id(Some("jsx")), "javascriptreact");
        assert_eq!(lsp_language_id(Some("javascript")), "javascript");
        assert_eq!(lsp_language_id(Some("typescript")), "typescript");
        assert_eq!(lsp_language_id(None), "typescript");
    }

    #[test]
    fn changing_language_closes_and_reopens_on_the_right_servers() {
        // Plain text -> TypeScript: nothing to close, both servers pick the document up.
        let p = lang_change_plan(None, Some("typescript"));
        assert_eq!((p.ts_close, p.ts_after, p.biome_close, p.biome_after), (false, true, false, true));
        // TypeScript -> TSX: same servers, new languageId => close and reopen.
        let p = lang_change_plan(Some("typescript"), Some("tsx"));
        assert_eq!((p.ts_close, p.ts_after, p.biome_close, p.biome_after), (true, true, true, true));
        // TypeScript -> Python: leaves both.
        let p = lang_change_plan(Some("typescript"), Some("python"));
        assert_eq!((p.ts_close, p.ts_after, p.biome_close, p.biome_after), (true, false, true, false));
        // TypeScript -> JSON: tsgo is left, Biome keeps it with the json languageId.
        let p = lang_change_plan(Some("typescript"), Some("json"));
        assert_eq!((p.ts_close, p.ts_after, p.biome_close, p.biome_after), (true, false, true, true));
        // Python -> Markdown: no server involved at all.
        let p = lang_change_plan(Some("python"), Some("markdown"));
        assert_eq!((p.ts_close, p.ts_after, p.biome_close, p.biome_after), (false, false, false, false));
        // Same language id: nothing to do (a no-op pick).
        let p = lang_change_plan(Some("javascript"), Some("javascript"));
        assert!(!p.ts_close && !p.biome_close && p.ts_after);
    }
}
