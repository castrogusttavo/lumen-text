//! Language-server glue: lifecycle, text sync, and the editor features built on LSP
//! (diagnostics, hover, completion, definition, references, rename, format, actions).

use crate::{
    app::App,
    app_overlay::{Action, Item, Overlay, PickKind},
    buffer::EditKind,
    doc::{Diag, Doc, LspEdit, path_to_uri, uri_to_path},
    fuzzy::fuzzy,
    lsp::{self, DefKind, Lsp, Msg, Pending},
    lspmerge::MultiKind,
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
/// After an edit only the edited and the active document are checked again at once; the other open
/// documents of the server wait for this long without edits (each pull type-checks a whole file).
const PULL_REST_DELAY: Duration = Duration::from_millis(1000);
/// Go soft memory limit: what the server may keep before the GC works harder, and in practice its
/// resident size once a file is checked. It must stay well above the live heap or the GC runs
/// flat out. Measured with tsgo 7.0.2 on a lean mirror holding one file that imports the Prisma
/// client (live heap ~450 MB): at 512 MiB each edit took 10.8 s to re-check and burned 18
/// CPU-seconds; at 1024 MiB 1.9 s; at 1536 or 2048 MiB no faster, only more resident memory.
const MEM_LIMIT_LEAN_MIB: u32 = 1024;
/// Lean with auto-import: tsgo's index of package exports adds ~400 MB of live heap.
const MEM_LIMIT_LEAN_AUTO_MIB: u32 = 1536;
const MEM_LIMIT_FULL_MIB: u32 = 6144;

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

/// One `Ctrl+.` round. The menu opens with the first items there are and every later answer is
/// merged into it; `serial` and `key` tell an answer nobody waits for any more.
#[derive(Default)]
pub(crate) struct ActionsRun {
    pub serial: u64,
    /// (document, its version, selection anchor and head) the actions were asked for; `None` when
    /// no answer is awaited.
    pub key: Option<(usize, i32, usize, usize)>,
    /// The menu of this round is on screen: answers are merged into it.
    pub opened: bool,
}

/// A scroll must rest this long before the lenses of the new viewport are resolved.
const LENS_SETTLE: Duration = Duration::from_millis(150);
/// At most this many `codeLens/resolve` requests per viewport.
const LENS_BATCH: usize = 120;

/// The code lenses of one document as last listed by the server, and which were resolved. A lens
/// needs a `codeLens/resolve` round-trip for its title ("3 references"): only those in (and
/// around) the viewport are asked, and an edit or a scroll cancels what is still unanswered.
#[derive(Default)]
pub(crate) struct LensRun {
    /// Numbers the list: an answer for an older list is dropped.
    pub run: u64,
    /// Document version the list was received at.
    pub version: i32,
    /// Declaration text under each lens (parallel to `Doc::lenses`), to keep titles across lists.
    pub names: Vec<String>,
    /// Resolved (or asked) since the list arrived.
    pub done: Vec<bool>,
    /// Unanswered resolves: (server process id, request id, lens index).
    pub inflight: Vec<(u32, i64, usize)>,
    /// Line span the resolves were last decided for, and since when the view shows it.
    pub span: Option<((usize, usize), Instant)>,
    /// Nothing left to ask for `span`.
    pub settled: bool,
}

/// Text a lens sits on: its range on its first line (UTF-16 columns), trimmed.
fn lens_name(line: &str, range: &Value) -> String {
    let units: Vec<u16> = line.encode_utf16().collect();
    let col = |p: &Value| (p["character"].as_u64().unwrap_or(0) as usize).min(units.len());
    let a = col(&range["start"]);
    let b = if range["end"]["line"] == range["start"]["line"] { col(&range["end"]).max(a) } else { units.len() };
    String::from_utf16_lossy(&units[a..b]).trim().to_string()
}

/// Give the new lenses the titles the old list had: a lens on the same line over the same text
/// keeps showing its title (no blink while the server recounts). Several lenses of one line
/// (references, implementations) match in order.
fn carry_lens_titles(
    old: &[crate::doc::Lens],
    old_names: &[String],
    new: &mut [crate::doc::Lens],
    new_names: &[String],
) {
    if old.len() != old_names.len() || new.len() != new_names.len() {
        return;
    }
    let mut titles: HashMap<(usize, &str), std::collections::VecDeque<&str>> = HashMap::new();
    for (l, name) in old.iter().zip(old_names) {
        // Every lens takes its place in the queue, so the second of a line never gets the first's title.
        titles.entry((l.line, name.as_str())).or_default().push_back(l.title.as_deref().unwrap_or(""));
    }
    for (l, name) in new.iter_mut().zip(new_names) {
        let kept = titles.get_mut(&(l.line, name.as_str())).and_then(|q| q.pop_front());
        if l.title.is_none()
            && let Some(t) = kept.filter(|t| !t.is_empty())
        {
            l.title = Some(t.to_string());
        }
    }
}

/// Lenses to resolve for the view showing lines `first..=last`: those not done yet in the viewport
/// and half a screen around it, nearest the top first, at most `LENS_BATCH`.
fn lenses_to_resolve(lines: &[usize], done: &[bool], (first, last): (usize, usize)) -> Vec<usize> {
    let margin = (last.saturating_sub(first) / 2).max(4);
    let (lo, hi) = (first.saturating_sub(margin), last + margin);
    let mut idx: Vec<usize> =
        (0..lines.len()).filter(|&k| !done.get(k).copied().unwrap_or(true) && (lo..=hi).contains(&lines[k])).collect();
    idx.sort_by_key(|&k| lines[k]);
    idx.truncate(LENS_BATCH);
    idx
}

/// Does `label` start with `prefix_lower` (already lowercased), ignoring case? The prefix is
/// lowercased once per list, and no lowercase copy of each of the thousands of labels is made.
fn starts_with_lower(label: &str, prefix_lower: &str) -> bool {
    let mut l = label.chars().flat_map(char::to_lowercase);
    prefix_lower.chars().all(|p| l.next() == Some(p))
}

/// Diagnostics to pull after an edit: of the server's open documents (`open`, server-side URIs),
/// those in `first` (the edited and the active one) now, the others later.
fn pull_split(open: Vec<String>, first: &[String]) -> (Vec<String>, Vec<String>) {
    open.into_iter().partition(|u| first.contains(u))
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

/// One entry of a `textDocument/codeAction` answer as a menu item. A `CodeAction` with an edit
/// applies it; one with only `data` / a `command` is resolved or executed when chosen; a bare
/// `Command` (harper-ls: "Add … to the user dictionary") is executed when chosen. `server` names the
/// registry server that answered, so the right one gets the follow-up request.
fn code_action_item(a: &Value, server: Option<&str>) -> Option<Item> {
    let title = a["title"].as_str()?;
    let lazy = |mut a: Value| {
        if let Some(n) = server {
            a["__server"] = json!(n);
        }
        Action::WorkspaceEdit(json!({"__resolve": a}))
    };
    let action = if a["edit"].is_object() {
        Action::WorkspaceEdit(a["edit"].clone())
    } else if a["command"].is_string() {
        lazy(json!({"title": title, "command": a}))
    } else if a["data"].is_object() || a["command"].is_object() {
        lazy(a.clone())
    } else {
        return None;
    };
    Some(Item { label: title.to_string(), detail: a["kind"].as_str().unwrap_or("").to_string(), action })
}

impl App {
    // ───────────── lifecycle ─────────────

    fn lsp_index_for_root(&self, root: &std::path::Path) -> Option<usize> {
        self.lsps.iter().position(|l| l.root == root && l.name == "tsgo")
    }

    pub(crate) fn doc_lsp(&self, doc: usize) -> Option<usize> {
        if let Some(li) = self.lspx_doc_lsp(doc) {
            return Some(li);
        }
        let d = self.docs.get(doc)?;
        if !ts_family(d.lang) {
            return None;
        }
        self.lsp_index_for_root(d.lsp_root.as_ref()?)
    }

    pub(crate) fn ensure_lsp(&mut self, doc: usize) -> Option<usize> {
        if !self.lsp_enabled() {
            return None;
        }
        if let Some(generic) = self.lspx_ensure_primary(doc) {
            return generic;
        }
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
        let mut heavy = None;
        if !self.config.lsp_narrow && self.config.lsp_auto_lean && !self.lsp_guard.lean.contains(&root) {
            heavy = crate::lsp_guard::heavy_reason(&root);
        }
        let narrow = (self.config.lsp_narrow || heavy.is_some() || self.lsp_guard.lean.contains(&root))
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
            match (mirror.is_some(), self.auto_import_on()) {
                (true, true) => MEM_LIMIT_LEAN_AUTO_MIB,
                (true, false) => MEM_LIMIT_LEAN_MIB,
                (false, _) => MEM_LIMIT_FULL_MIB,
            },
            self.wake(),
        ) {
            Ok(mut l) => {
                l.mirror = mirror;
                l.includes = includes;
                self.lsps.push(l);
                let ver = lsp::tsgo_version(&bin).unwrap_or_default();
                let mode = if self.lsps.last().is_some_and(|l| l.mirror.is_some()) { " (lean mode)" } else { "" };
                match heavy {
                    Some(why) if !mode.is_empty() => {
                        self.say(format!("starting tsgo {ver} in lean mode ({why}) to save RAM; set lsp_auto_lean to false for the full project"))
                    }
                    _ => self.say(format!("starting tsgo {ver}{mode}…")),
                }
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
        let mut touched = self.lspx_fs_events(evs);
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

    /// Restart a full-project tsgo in lean mode when it grows past the memory budget. The check
    /// and its timer run only while there is a tsgo to guard and the window is focused.
    pub(crate) fn guard_lsp_memory(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let has_tsgo = self.lsps.iter().any(|l| l.name == "tsgo");
        if !crate::lsp_guard::runs(self.config.lsp_auto_lean, self.gitbg.focused, has_tsgo) {
            return;
        }
        let due = self.lsp_guard.due();
        want(self.lsp_guard.next_check().max(now));
        if !due {
            return;
        }
        // The numbers below are the ones the background sampler published (`mem::watch`); each
        // sample counts once. With no sampler (tests) they are read here and now. They are
        // resident memory (anonymous + shmem), not PSS: see `mem` for why that is close enough.
        if let Some(generation) = crate::mem::generation()
            && !self.lsp_guard.fresh(generation)
        {
            return;
        }
        // A lean server that balloons is broken: restart it rather than let it eat the machine.
        let lean_mb = self
            .lsps
            .iter()
            .filter(|l| l.name == "tsgo" && l.mirror.is_some())
            .filter_map(|l| crate::mem::resident_of(l.pid()).map(|kb| kb / 1024))
            .max();
        if let Some(mb) = lean_mb.filter(|mb| self.lsp_guard.lean_over(*mb)) {
            self.restart_lsp();
            self.say(format!("tsgo (lean) used {mb} MB: restarted"));
            return;
        }
        let Some(l) = self.lsps.iter().find(|l| l.name == "tsgo" && l.mirror.is_none()) else { return };
        let Some(mb) = crate::mem::resident_of(l.pid()).map(|kb| kb / 1024) else { return };
        if !self.lsp_guard.over_budget(mb, crate::lsp_guard::budget_mb(crate::lsp_guard::total_ram_mb())) {
            return;
        }
        let root = l.root.clone();
        if !root.join("tsconfig.json").exists() {
            return;
        }
        self.lsp_guard.lean.insert(root);
        self.restart_lsp();
        self.say(format!("tsgo used {mb} MB: restarted in lean mode (open files + imports). Toggle it in Settings"));
    }

    pub fn restart_lsp(&mut self) {
        self.comp_inflight = None;
        self.lspx_restart_all();
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

    pub(crate) fn doc_open_params(d: &Doc, uri: String) -> Option<Value> {
        let lang = crate::lspreg::language_id(d.lang, d.path());
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
        if !self.lsp_enabled()
            || self.biome_disabled
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

    pub(crate) fn biome_index(&self, doc: usize) -> Option<usize> {
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
        self.lspx_sync_config();
        self.lspx_tick(now, want);
        self.problems_tick(now, want);
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
            self.lspx_companions(i, now, want, &mut pull);
            if !ts_family(self.docs[i].lang) && self.lspx_primary_entry(i).is_none() {
                continue;
            }
            let Some(li) = self.ensure_lsp(i) else { continue };
            if !self.docs[i].lsp_open {
                if let Some(p) = Self::doc_text_params(&self.docs[i]) {
                    let uri = p["textDocument"]["uri"].clone();
                    self.lsps[li].notify("textDocument/didOpen", p);
                    self.docs[i].lsp_open = true;
                    self.docs[i].changed_at = None;
                    pull.push(li);
                    self.warm_auto_import(li, uri);
                }
                continue;
            }
            if let Some(t) = self.docs[i].changed_at {
                if t + SYNC_DEBOUNCE <= now {
                    // The pull comes from the mark `send_change` leaves (see `pull_after_edits`).
                    self.send_change(i);
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
                                    self.lsps[li].request_single(
                                        format!("sem:{uri}"),
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
            if i == self.active && self.config.code_lens {
                self.lens_tick(i, li, now, want);
            }
            // Inlay hints for the visible lines only (debounced), once the text is synced.
            if self.config.inlay_hints_for(self.docs[i].lang) && i == self.active && self.docs[i].changed_at.is_none() {
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
                pull.extend(
                    self.lsps.iter().enumerate().filter(|(_, l)| l.name == "tsgo" || l.is_generic()).map(|(li, _)| li),
                );
            } else {
                want(t);
            }
        }
        // The completion request is written first: the server answers in arrival order, and a pull
        // type-checks the whole file (2 s on one that imports the Prisma client).
        if let Some(trig) = comp {
            if std::env::var_os("LUMEN_DEBUG").is_some() {
                eprintln!("[lsp] firing completion {trig:?}");
            }
            self.request_completion(trig);
        }
        pull.sort_unstable();
        pull.dedup();
        for &li in &pull {
            self.pull_diagnostics(li);
        }
        self.pull_after_edits(&pull, now, want);
    }

    /// Diagnostics after edits. Every document whose text was just sent (`send_change`, from the
    /// debounce or from a `flush` before a request) is pulled now, with the active one; the other
    /// open documents of its servers follow after `PULL_REST_DELAY` without edits. `whole` are the
    /// servers this tick already pulled completely.
    fn pull_after_edits(&mut self, whole: &[usize], now: Instant, want: &mut impl FnMut(Instant)) {
        for uri in std::mem::take(&mut self.lspx.text_sent) {
            let Some(i) = self.doc_by_uri(&uri) else { continue };
            let mut servers = self.lspx_open_companions(i);
            servers.extend(self.doc_lsp(i).filter(|_| self.docs[i].lsp_open));
            for li in servers {
                let pid = self.lsps[li].pid();
                self.lspx.pull_rest.retain(|r| r.0 != pid);
                if whole.contains(&li) {
                    continue;
                }
                let mut first = Vec::new();
                for d in [i, self.active] {
                    first.extend(self.docs.get(d).into_iter().flat_map(|d| [d.uri(), d.real_uri()]).flatten());
                }
                let (first, rest) = pull_split(self.pull_targets(li), &first);
                for uri in first {
                    self.pull_uri(li, uri);
                }
                if !rest.is_empty() {
                    self.lspx.pull_rest.push((pid, now + PULL_REST_DELAY, rest));
                }
            }
        }
        let mut k = 0;
        while k < self.lspx.pull_rest.len() {
            let (pid, due, _) = &self.lspx.pull_rest[k];
            if *due > now {
                want(*due);
                k += 1;
                continue;
            }
            let pid = *pid;
            let (_, _, rest) = self.lspx.pull_rest.remove(k);
            let Some(li) = self.lsps.iter().position(|l| l.pid() == pid) else { continue };
            // Documents closed meanwhile are no longer among the server's.
            let open = self.pull_targets(li);
            for uri in rest.into_iter().filter(|u| open.contains(u)) {
                self.pull_uri(li, uri);
            }
        }
    }

    /// Resolve the code lenses the active document shows. An edit cancels the unanswered resolves
    /// (a new list follows); a scroll cancels them too and, once it rests, asks for the new viewport.
    fn lens_tick(&mut self, i: usize, li: usize, now: Instant, want: &mut impl FnMut(Instant)) {
        if self.docs[i].lenses.is_empty() || self.lspx.lens.is_empty() {
            return;
        }
        let Some(uri) = self.docs[i].uri() else { return };
        let Some(run) = self.lspx.lens.get(&uri) else { return };
        let d = &self.docs[i];
        if run.names.len() != d.lenses.len() {
            return; // the lenses were replaced or cleared by someone else
        }
        if d.version != run.version || d.changed_at.is_some() {
            return self.lens_cancel(&uri);
        }
        let span = d.visible_line_span();
        match run.span {
            Some((s, _)) if s == span => {}
            // The first viewport of a new list is asked at once; a scroll has to rest first.
            first => {
                let since = if first.is_some() { now } else { now.checked_sub(LENS_SETTLE).unwrap_or(now) };
                self.lens_cancel(&uri);
                if let Some(run) = self.lspx.lens.get_mut(&uri) {
                    run.span = Some((span, since));
                    run.settled = false;
                }
            }
        }
        let Some(run) = self.lspx.lens.get_mut(&uri) else { return };
        let Some((_, since)) = run.span else { return };
        if run.settled {
            return;
        }
        if since + LENS_SETTLE > now {
            return want(since + LENS_SETTLE);
        }
        run.settled = true;
        let lines: Vec<usize> = self.docs[i].lenses.iter().map(|l| l.line).collect();
        let (serial, pid) = (run.run, self.lsps[li].pid());
        for idx in lenses_to_resolve(&lines, &run.done, span) {
            run.done[idx] = true;
            let what = Pending::ResolveLens { uri: uri.clone(), line: lines[idx], idx, run: serial };
            let id = self.lsps[li].request("codeLens/resolve", self.docs[i].lenses[idx].raw.clone(), what);
            run.inflight.push((pid, id, idx));
        }
    }

    /// Cancel the unanswered `codeLens/resolve` requests of a document; their lenses count as not
    /// asked, so they are resolved again if they are still on screen.
    fn lens_cancel(&mut self, uri: &str) {
        let Some(run) = self.lspx.lens.get_mut(uri) else { return };
        for (pid, id, idx) in std::mem::take(&mut run.inflight) {
            let Some(l) = self.lsps.iter_mut().find(|l| l.pid() == pid) else { continue };
            if l.pending.contains_key(&id) {
                l.cancel(id);
                if let Some(d) = run.done.get_mut(idx) {
                    *d = false;
                }
                run.settled = false;
            }
        }
    }

    /// The server listed the code lenses of document `i`.
    fn on_code_lens(&mut self, i: usize, uri: &str, v: &Value) {
        let d = &self.docs[i];
        let mut lenses = Vec::new();
        let mut names = Vec::new();
        for it in v.as_array().into_iter().flatten() {
            let r = &it["range"]["start"];
            let line = r["line"].as_u64().unwrap_or(0) as usize;
            names.push(if line < d.buf.line_count() {
                lens_name(&d.buf.line(line), &it["range"])
            } else {
                String::new()
            });
            lenses.push(crate::doc::Lens {
                line,
                col: r["character"].as_u64().unwrap_or(0) as usize,
                title: it["command"]["title"].as_str().map(str::to_string),
                raw: it.clone(),
            });
        }
        // Counts need a resolve round-trip (`lens_tick` asks for the visible ones); until the
        // answer, a lens that did not move keeps the title it had.
        let done: Vec<bool> = lenses.iter().map(|l| l.title.is_some()).collect();
        self.lens_cancel(uri);
        self.lspx.lens.retain(|u, _| u == uri || self.docs.iter().any(|d| d.uri().as_deref() == Some(u.as_str())));
        let run = self.lspx.lens.entry(uri.to_string()).or_default();
        carry_lens_titles(&self.docs[i].lenses, &run.names, &mut lenses, &names);
        *run = LensRun { run: run.run + 1, version: self.docs[i].version, names, done, ..Default::default() };
        self.docs[i].lenses = lenses;
        self.request_redraw();
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
        if d.version != version || !self.config.inlay_hints_for(d.lang) {
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

    pub(crate) fn send_change(&mut self, i: usize) {
        let li = self.doc_lsp(i);
        let d = &mut self.docs[i];
        if li.is_none() && d.real_uri().is_none() {
            return;
        }
        d.changed_at = None;
        let Some(uri) = d.uri() else { return };
        // The servers have new text: `lsp_tick` pulls the diagnostics again (also when this was a
        // `flush` ahead of the debounce, which used to leave them stale).
        if !self.lspx.text_sent.contains(&uri) {
            self.lspx.text_sent.push(uri.clone());
        }
        let d = &self.docs[i];
        let params = json!({"textDocument": {"uri": uri, "version": d.version}, "contentChanges": [{"text": d.buf.to_string()}]});
        self.lspx_send_change(i, &params);
        if let Some(li) = li {
            self.lsps[li].notify("textDocument/didChange", params);
        }
    }

    /// Make sure the server has our latest text before asking it something.
    pub(crate) fn flush(&mut self, i: usize) {
        if self.docs.get(i).is_some_and(|d| d.changed_at.is_some() && d.lsp_open) {
            self.send_change(i);
        }
    }

    /// Server-side URIs of the documents open on server `li` whose diagnostics are pulled.
    fn pull_targets(&self, li: usize) -> Vec<String> {
        let l = &self.lsps[li];
        if l.is_generic() {
            return if l.has_cap("diagnosticProvider") { l.open_uris.iter().cloned().collect() } else { Vec::new() };
        }
        self.docs
            .iter()
            .filter(|d| d.lsp_open && d.lsp_root.as_ref() == Some(&l.root))
            .filter_map(|d| d.uri())
            .collect()
    }

    fn pull_uri(&mut self, li: usize, uri: String) {
        self.lsps[li].request_single(
            format!("diag:{uri}"),
            "textDocument/diagnostic",
            json!({"textDocument": {"uri": uri}}),
            Pending::Diagnostic { uri },
        );
    }

    /// Pull the diagnostics of every document open on server `li`.
    pub(crate) fn pull_diagnostics(&mut self, li: usize) {
        for uri in self.pull_targets(li) {
            self.pull_uri(li, uri);
        }
    }

    pub fn lsp_did_save(&mut self, i: usize) {
        self.ws_after_save();
        if let (Some(lb), Some(uri)) = (self.biome_index(i), self.docs.get(i).and_then(|d| d.real_uri())) {
            self.lsps[lb].notify("textDocument/didSave", json!({"textDocument": {"uri": uri}}));
        }
        let Some(li) = self.doc_lsp(i) else {
            for c in self.lspx_did_save(i) {
                self.pull_diagnostics(c);
            }
            return;
        };
        let Some(uri) = self.docs[i].uri() else { return };
        self.flush(i);
        self.lsps[li].notify("textDocument/didSave", json!({"textDocument": {"uri": uri}}));
        self.pull_diagnostics(li);
        for c in self.lspx_did_save(i) {
            self.pull_diagnostics(c);
        }
        // Every server of the document was just pulled completely: the tick need not do it again.
        self.lspx.text_sent.retain(|u| *u != uri);
    }

    /// Tell the server a document is gone from this URI, without touching server lifetime.
    pub fn lsp_notify_close(&mut self, i: usize) {
        self.lspx_close_doc(i);
        // A registry primary was just closed by `lspx_close_doc`; only tsgo is left to tell.
        let Some(li) = self.doc_lsp(i).filter(|&li| !self.lsps[li].is_generic()) else { return };
        let d = &self.docs[i];
        if d.lsp_open
            && let Some(uri) = d.uri()
        {
            self.lsps[li].notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}));
        }
    }

    pub fn lsp_did_close(&mut self, i: usize) {
        self.problems_keep_closing(i);
        self.biome_close(i);
        // Registry servers: didClose everywhere; they are stopped after an idle grace period.
        let was_generic = self.doc_lsp(i).is_some_and(|li| self.lsps[li].is_generic());
        self.lspx_close_doc(i);
        if was_generic {
            return;
        }
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
                self.lsps[li].single_done(id);
                if matches!(what, Pending::Initialize)
                    && let (true, Err(e)) = (self.lsps[li].is_generic(), &result)
                {
                    let msg = e["message"].as_str().unwrap_or("?").to_string();
                    self.lsps[li].log_line("app", &format!("initialize failed: {msg}"));
                    self.say_for(format!("{} failed to start: {msg}", self.lsps[li].name), 6);
                    dead.push(li);
                    return;
                }
                self.on_response(li, what, result);
            }
            Msg::Symbols { id, syms } => {
                let Some(what) = self.lsps[li].pending.remove(&id) else { return };
                self.lsps[li].single_done(id);
                self.on_symbol_list(what, syms);
            }
            Msg::Tokens { id, data } => {
                let Some(what) = self.lsps[li].pending.remove(&id) else { return };
                self.lsps[li].single_done(id);
                self.on_tokens(li, what, data);
            }
            Msg::WsDiag { id, report } => {
                let Some(what) = self.lsps[li].pending.remove(&id) else { return };
                self.lsps[li].single_done(id);
                if let Pending::WorkspaceDiag { token } = what {
                    self.ws_report(li, &token, report, true);
                }
            }
            Msg::WsPartial { token, report } => self.ws_report(li, &token, report, false),
            Msg::Notification { method, params } => self.on_notification(li, &method, &params),
            Msg::Request { id, method, params } => self.on_server_request(li, id, &method, &params),
        }
    }

    pub(crate) fn drop_dead_servers(&mut self, mut dead: Vec<usize>) {
        dead.sort_unstable();
        dead.dedup();
        for li in dead.into_iter().rev() {
            if li < self.lsps.len() && self.lsps[li].is_generic() {
                self.lspx_server_gone(li);
                continue;
            }
            if li < self.lsps.len() {
                let root = self.lsps[li].root.clone();
                let name = self.lsps[li].name.clone();
                self.lspx.problems.clear_source(&name);
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
        if self.lspx_on_notification(li, method, params) {
            return;
        }
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
        if self.lspx_on_request(li, &id, method, params) {
            return;
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
                crate::workspace::folders_json(&crate::workspace::lsp_folders(&root))
            }
            "workspace/diagnostic/refresh" => {
                self.ws_refresh_requested();
                Value::Null
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
        self.set_diags_from(uri, items, if lint { Some(true) } else { None }, "");
    }

    /// Diagnostics from tsgo (`lint = None`), Biome (`Some(true)`), or a registry server whose id is
    /// `source` (`lint = Some(false)`: stored per server, merged by the document).
    pub(crate) fn set_diags_from(&mut self, uri: &str, items: &Value, lint: Option<bool>, source: &str) {
        let Some(i) = self.doc_by_uri(uri) else {
            // A file that is not open (servers publish for the whole project): the Problems panel's
            // project-wide store keeps it, bounded.
            let key = match lint {
                None => "tsgo",
                Some(true) => "biome",
                Some(false) => source,
            };
            return self.problems_store_closed(uri, key, lint == Some(false), items);
        };
        self.lspx.problems.generation += 1;
        let d = &mut self.docs[i];
        let mut out = Vec::new();
        // Spelling is never an error: whatever a spell checker says shows as "info" (dotted underline).
        let spell = crate::lspreg::is_spell_server(source);
        if lint == Some(false) {
            let with_data =
                items.as_array().into_iter().flatten().filter(|it| !it["data"].is_null()).cloned().collect();
            d.set_diag_data(source, with_data);
        }
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
                severity: if spell { 3 } else { it["severity"].as_u64().unwrap_or(1) as u8 },
                message: it["message"].as_str().unwrap_or("").to_string(),
                code,
                source: if source.is_empty() {
                    String::new()
                } else {
                    it["source"].as_str().filter(|s| !s.is_empty()).unwrap_or(source).to_string()
                },
            });
        }
        out.sort_by_key(|d| (d.sl, d.sc));
        match lint {
            Some(false) => d.set_ext_diags(source, out),
            l => d.set_source_diags(l.unwrap_or(false), out),
        }
    }

    fn on_response(&mut self, li: usize, what: Pending, result: Result<Value, Value>) {
        if matches!(what, Pending::Completion { .. }) {
            self.comp_inflight = None;
        }
        if let Pending::Multi { kind, serial } = what {
            // The primary's part of a merged completion is the request `comp_inflight` tracks.
            if kind == MultiKind::Completion && self.comp_inflight.as_ref().is_some_and(|c| c.0 == li) {
                self.comp_inflight = None;
            }
            return self.lspx_on_multi(li, kind, serial, result.unwrap_or(Value::Null));
        }
        if matches!(what, Pending::Highlight { .. } | Pending::LinkedEditing { .. }) {
            return self.assist_on_response(li, what, result);
        }
        if let Pending::WorkspaceDiag { token } = what {
            // Answers normally arrive parsed as `Msg::WsDiag`; this is an error (cancelled...).
            return self.ws_failed(li, &token, result.err());
        }
        if matches!(what, Pending::InlineCompletion { .. } | Pending::Copilot(_)) {
            return self.inline_on_response(li, what, result);
        }
        if let (Pending::Initialize, Ok(v), true) = (&what, &result, self.lsps[li].is_generic()) {
            return self.lspx_initialized(li, v);
        }
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
                if let Pending::CodeActions { serial } = what {
                    return self.actions_response(serial, Vec::new());
                }
                self.sem_range_failed(&what);
                if !matches!(
                    what,
                    Pending::Diagnostic { .. }
                        | Pending::Hover { .. }
                        | Pending::HoverExtra { .. }
                        | Pending::Completion { .. }
                        | Pending::Warm
                        | Pending::Signature { .. }
                        | Pending::SymbolsCache { .. }
                        | Pending::WorkspaceSymbols { .. }
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
                let generic = self.lsps[li].generic.as_ref().map(|g| g.id.clone());
                let put = |app: &mut App, u: &str, items: &Value| match &generic {
                    Some(id) => app.set_diags_from(u, items, Some(false), id),
                    None => app.set_diags(u, items, false),
                };
                if v["kind"].as_str() == Some("full") {
                    put(self, &uri, &v["items"]);
                }
                if let Some(rel) = v["relatedDocuments"].as_object() {
                    for (u, r) in rel {
                        if r["kind"].as_str() == Some("full") {
                            put(self, u, &r["items"]);
                        }
                    }
                }
            }
            Pending::Hover { uri, at } => self.on_hover(&uri, at, &v),
            Pending::HoverExtra { uri, at, serial, what } => self.hover_extra_response(&uri, at, serial, what, &v),
            Pending::Completion { uri, at } => self.on_completion(&uri, at, v),
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
            Pending::Blocking | Pending::Warm => {}
            Pending::CodeActions { serial } => self.on_code_actions(li, serial, &v),
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
            Pending::WorkspaceSymbols { serial } => self.goto_ws_answer(li, serial, &v),
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
                    self.on_code_lens(i, &uri, &v);
                }
            }
            Pending::ResolveLens { uri, line, idx, run } => {
                if let Some(i) = self.doc_by_uri(&uri)
                    && let Some(title) = v["command"]["title"].as_str()
                    && self.lspx.lens.get(&uri).is_some_and(|r| r.run == run)
                    && let Some(l) = self.docs[i].lenses.get_mut(idx).filter(|l| l.line == line)
                {
                    l.title = Some(title.to_string());
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
            Pending::ExecuteCommand
            | Pending::Multi { .. }
            | Pending::Highlight { .. }
            | Pending::LinkedEditing { .. }
            | Pending::WorkspaceDiag { .. }
            | Pending::InlineCompletion { .. }
            | Pending::Copilot(_) => {}
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
        // Stopped in the debugger: the value of the expression comes first.
        if self.dbg.is_some() && self.debug_hover_request(at) {
            return;
        }
        self.request_hover_lsp(at);
    }

    pub fn request_hover_lsp(&mut self, at: usize) {
        if self.hover_from_cache(at) {
            return;
        }
        let Some((i, li)) = self.with_server() else { return self.show_diag_hover(at) };
        let params = self.pos_params(i, at);
        let uri = self.docs[i].uri().unwrap_or_default();
        if self
            .lspx_request_multi(MultiKind::Hover, i, li, at, "textDocument/hover", params.clone(), "hoverProvider")
            .is_some()
        {
            return;
        }
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

    pub(crate) fn on_hover(&mut self, uri: &str, at: usize, v: &Value) {
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
                    self.goto_symbols_arrived();
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

    /// tsgo builds its auto-import index on the first completion with auto-imports (6-12 s and
    /// ~1 GB peak on a big project; a code action does not build it). Ask for a throwaway completion
    /// right after the first file opens so the first real one finds it ready. Only when auto-import
    /// is on and only once per server.
    fn warm_auto_import(&mut self, li: usize, uri: Value) {
        if !self.auto_import_on() || self.lsps[li].name != "tsgo" || self.lsps[li].warmed {
            return;
        }
        self.lsps[li].warmed = true;
        let params = json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}, "context": {"triggerKind": 1}});
        self.lsps[li].request("textDocument/completion", params, Pending::Warm);
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
                let mut o = json!({"range": {"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}}, "message": g.message, "severity": g.severity, "source": if g.source.is_empty() { "ts" } else { g.source.as_str() }});
                o["code"] = g.code.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::from(g.code.clone()));
                o
            })
            .collect();
        let mk = |uri: &str| json!({"textDocument": {"uri": uri}, "range": range.clone(), "context": {"diagnostics": diags.clone()}});
        let ts_uri = d.uri().unwrap_or_default();
        let real_uri = d.real_uri().unwrap_or_default();
        let from_index = self.import_action_items(i);
        let serial = self.actions_begin(i);
        self.actions_items = from_index;
        self.actions_expected = 0;
        if let Some((_, li)) = self.with_server() {
            self.lsps[li].request("textDocument/codeAction", mk(&ts_uri), Pending::CodeActions { serial });
            self.actions_expected += 1;
        }
        if let Some(lb) = self.biome_index(i) {
            self.lsps[lb].request("textDocument/codeAction", mk(&real_uri), Pending::CodeActions { serial });
            self.actions_expected += 1;
        }
        let params = mk(&real_uri);
        self.actions_expected += self.lspx_code_actions(i, &params, serial);
        if self.actions_expected == 0 {
            self.lspx.actions.key = None;
            if !self.actions_items.is_empty() {
                let items = std::mem::take(&mut self.actions_items);
                return self.open_code_actions_with(items);
            }
            if self.ai_has_diag_at_cursor() {
                let items = self.ai_action_items();
                return self.open_code_actions_with(items);
            }
            return self.say("actions need a language server (none is running for this file)");
        }
        // The import index already has something: show it now, the servers' answers join the list.
        if !self.actions_items.is_empty() {
            self.actions_show();
        }
    }

    /// Start a `Ctrl+.` round for document `i`: answers of earlier rounds are stale from here on.
    fn actions_begin(&mut self, i: usize) -> u64 {
        let run = &mut self.lspx.actions;
        run.serial += 1;
        run.opened = false;
        run.key = self.docs.get(i).map(|d| (i, d.version, d.ed.sel.anchor, d.ed.sel.head));
        run.serial
    }

    /// Is an answer of round `serial` still wanted? Not after a newer round, an edit, a caret move,
    /// a tab switch, or once the user closed the menu this round had opened.
    fn actions_live(&self, serial: u64) -> bool {
        let run = &self.lspx.actions;
        let Some((i, ver, anchor, head)) = run.key.filter(|_| run.serial == serial) else { return false };
        let same = self.active == i
            && self.docs.get(i).is_some_and(|d| d.version == ver && d.ed.sel.anchor == anchor && d.ed.sel.head == head);
        same && (!run.opened || matches!(&self.overlay, Overlay::Picker(p) if p.kind == PickKind::CodeActions))
    }

    fn on_code_actions(&mut self, li: usize, serial: u64, v: &Value) {
        let server = self.lsps.get(li).filter(|l| l.is_generic()).map(|l| l.name.clone());
        let items = v.as_array().into_iter().flatten().filter_map(|a| code_action_item(a, server.as_deref())).collect();
        self.actions_response(serial, items);
    }

    /// One server answered round `serial`: its items join the menu, which opens with the first
    /// answer that has any. Only when every server answered empty there is nothing to show.
    fn actions_response(&mut self, serial: u64, items: Vec<Item>) {
        if !self.actions_live(serial) {
            if self.lspx.actions.serial == serial {
                self.lspx.actions.key = None;
                self.actions_items.clear();
            }
            return;
        }
        self.actions_items.extend(items);
        Self::lspx_dedupe_actions(&mut self.actions_items);
        self.actions_expected = self.actions_expected.saturating_sub(1);
        let done = self.actions_expected == 0;
        if self.actions_items.is_empty() {
            if !done {
                return;
            }
            // A problem under the caret can still go to the agent (the items run the AI commands).
            if !self.ai_has_diag_at_cursor() {
                self.lspx.actions.key = None;
                return self.say("no actions available here");
            }
        }
        self.actions_show();
        if done {
            self.lspx.actions.key = None;
            self.actions_items.clear();
        }
    }

    /// Put what was collected so far in the quick-fix menu. An open menu is rebuilt in place: the
    /// typed filter, the selected item and its row on screen stay where they are.
    fn actions_show(&mut self) {
        let mut all = self.actions_items.clone();
        if self.ai_has_diag_at_cursor() {
            all.extend(self.ai_action_items());
        }
        let old = match &mut self.overlay {
            Overlay::Picker(p) if self.lspx.actions.opened && p.kind == PickKind::CodeActions => {
                let chosen =
                    p.filtered.get(p.sel).map(|(k, _)| (p.items[*k].label.clone(), p.items[*k].detail.clone()));
                Some((std::mem::take(&mut p.field), chosen, p.sel, p.scroll))
            }
            _ => None,
        };
        self.open_code_actions_with(all);
        self.lspx.actions.opened = true;
        if let (Some((field, chosen, sel, scroll)), Overlay::Picker(p)) = (old, &mut self.overlay) {
            p.field = field;
            p.refilter();
            let at = chosen
                .and_then(|c| {
                    p.filtered.iter().position(|(k, _)| p.items[*k].label == c.0 && p.items[*k].detail == c.1)
                })
                .unwrap_or(sel)
                .min(p.filtered.len().saturating_sub(1));
            p.scroll = (scroll + at).saturating_sub(sel);
            p.sel = at;
        }
        self.request_redraw();
    }

    /// Code actions that arrive without an edit are resolved (or executed) when chosen.
    pub fn resolve_code_action(&mut self, action: &Value) {
        let mut action = action.clone();
        let named =
            action.as_object_mut().and_then(|o| o.remove("__server")).and_then(|n| n.as_str().map(str::to_string));
        let action = &action;
        let li = match named.and_then(|n| self.lspx_server_named(&n, self.active)) {
            Some(li) => li,
            None => match self.with_server() {
                Some((_, li)) => li,
                None => return,
            },
        };
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
        let Some((i, li)) = self.with_server() else { return self.structural_expand() };
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
            // No server: the snippets still complete.
            self.snippet_completion(true);
            return;
        };
        self.cancel_completion();
        let at = self.docs[i].ed.head();
        let mut params = self.pos_params(i, at);
        params["context"] = match trigger {
            Some(c) => json!({"triggerKind": 2, "triggerCharacter": c.to_string()}),
            None => json!({"triggerKind": 1}),
        };
        self.comp_t0 = Some(Instant::now());
        let uri = self.docs[i].uri().unwrap_or_default();
        let mut ws = at;
        while ws > 0 && self.docs[i].buf.char_at(ws - 1).is_some_and(is_ident) {
            ws -= 1;
        }
        // With companions the request goes to all of them; the primary's part is the one tracked.
        let multi = self.lspx_request_multi(
            MultiKind::Completion,
            i,
            li,
            at,
            "textDocument/completion",
            params.clone(),
            "completionProvider",
        );
        let id = match multi {
            Some(id) => id,
            None => {
                self.lsps[li].request("textDocument/completion", params, Pending::Completion { uri: uri.clone(), at })
            }
        };
        self.comp_inflight = Some((li, id, uri, ws, Instant::now()));
    }

    /// Drop the unanswered completion request (the servers stop computing it).
    fn cancel_completion(&mut self) {
        if let Some((li, id, ..)) = self.comp_inflight.take()
            && let Some(l) = self.lsps.get_mut(li)
        {
            l.cancel(id);
        }
        self.lspx_cancel_multi(MultiKind::Completion);
    }

    /// True while a completion for the word starting at `ws` of `uri` is still being computed: its
    /// answer is filtered against whatever was typed meanwhile, so asking again only loads the server.
    fn completion_inflight_for(&self, uri: &str, ws: usize) -> bool {
        self.comp_inflight
            .as_ref()
            .is_some_and(|(_, _, u, w, t)| u == uri && *w == ws && t.elapsed() < Duration::from_secs(20))
    }

    /// A completion answer. It is taken by value: the items that survive the filter are moved into
    /// the popup's list (`CItem::raw`), not copied.
    pub(crate) fn on_completion(&mut self, uri: &str, at: usize, v: Value) {
        let Some(i) = self.doc_by_uri(uri) else { return };
        if i != self.active {
            return;
        }
        let all = crate::lspmerge::take_items(v);
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
        let lower = prefix.to_lowercase();

        // Servers return thousands of candidates (every auto-import in the program). Keep only
        // those that can still match what was typed, best first: later keystrokes only narrow it.
        let mut scored: Vec<(i32, Value)> = Vec::new();
        for it in all {
            let Some(label) = it["label"].as_str() else { continue };
            let score = if prefix.is_empty() {
                0
            } else {
                match fuzzy(&prefix, label) {
                    Some((s, _)) => s + if starts_with_lower(label, &lower) { 40 } else { 0 },
                    None => continue,
                }
            };
            scored.push((score, it));
        }
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        scored.truncate(1500);

        let mut items: Vec<CItem> = Vec::with_capacity(scored.len());
        for (_, it) in scored {
            let label = it["label"].as_str().unwrap_or("").to_string();
            let te = it.get("textEdit").filter(|t| t.is_object() && t.get("range").is_some());
            let insert = te
                .and_then(|t| t["newText"].as_str())
                .or_else(|| it["insertText"].as_str())
                .unwrap_or(&label)
                .to_string();
            let detail =
                it["detail"].as_str().or_else(|| it["labelDetails"]["description"].as_str()).unwrap_or("").to_string();
            let sort = it["sortText"].as_str().unwrap_or(&label).to_string();
            let (kind, range) = (it["kind"].as_u64().unwrap_or(0) as u32, te.and_then(parse_edit));
            items.push(CItem { label, detail, insert, kind, sort, range, raw: it });
        }
        // The user's, the project's and the built-in snippets ride along with the server's list.
        items.extend(crate::app_snippets::snippet_citems(&self.root, d.lang));
        // An empty server list can still get names from the import index (added by the refilter).
        let mut c =
            Completion { items, filtered: Vec::new(), sel: 0, scroll: 0, start, seed_len: prefix.chars().count() };
        self.refilter_completion_into(&mut c);
        self.completion = if c.filtered.is_empty() { None } else { Some(c) };
        // No `release_memory()` here: malloc_trim walks the whole heap, synchronously, right before
        // the popup is drawn. The periodic trim of `App::tick` returns the answer's memory.
        self.request_redraw();
    }

    pub(crate) fn refilter_completion_into(&self, c: &mut Completion) {
        let Some(d) = self.docs.get(self.active) else { return };
        let head = d.ed.head();
        let prefix = if head >= c.start { d.buf.slice(c.start, head) } else { String::new() };
        let lower = prefix.to_lowercase();
        // Names from the editor's import index follow what is typed (the server's list only narrows).
        c.items.retain(|it| it.raw.get("__import").is_none());
        let extra = {
            let have: std::collections::HashSet<&str> = c.items.iter().map(|it| it.label.as_str()).collect();
            self.import_citems(self.active, &prefix, &have)
        };
        c.items.extend(extra);
        let mut scored: Vec<(i32, usize)> = c
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                if prefix.is_empty() {
                    return Some((0, i));
                }
                let (mut s, _) = fuzzy(&prefix, &it.label)?;
                if starts_with_lower(&it.label, &lower) {
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
            if typed.is_some_and(is_ident) && self.completion.is_none() {
                self.snippet_completion(false);
            }
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
                let waiting = d.uri().is_some_and(|u| self.completion_inflight_for(&u, ws));
                // Keep the first deadline: re-arming on every key starved fast typists of a request.
                if !d.buf.char_at(ws).is_some_and(|c| c.is_ascii_digit()) && !waiting && self.comp_due.is_none() {
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
        // A snippet from our own files: expand it (tab stops, variables) instead of pasting text.
        if let Some(body) = item.raw["__snippet"].as_str() {
            let path = d.path().map(std::path::Path::to_path_buf);
            let ctx = crate::app_snippets::env(&self.root, &mut self.clip, body, path.as_deref());
            d.insert_snippet_ctx(s, e, body, ctx);
            self.request_redraw();
            return;
        }
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

        // A name from the editor's import index: the editor writes the import itself.
        if item.raw.get("__import").is_some() {
            self.import_apply(i, &item.raw["__import"]);
            self.request_redraw();
            return;
        }
        // Auto-imports and friends come in `additionalTextEdits`, often only after resolve.
        let extra = parse_edits(&item.raw["additionalTextEdits"]);
        if !extra.is_empty() {
            self.docs[i].apply_lsp_edits(&extra);
        } else if let Some(li) =
            item.raw["__server"].as_str().and_then(|n| self.lspx_server_named(n, i)).or_else(|| self.doc_lsp(i))
        {
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
    pub(crate) fn biome_notify_close(&mut self, i: usize) {
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
        self.lspx_language_changed(i);
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
                // `lsp_did_close` also closed it on Biome: don't send a second didClose below.
                self.docs[i].biome_open = false;
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
    fn code_actions_with_edits_commands_and_bare_commands_all_reach_the_menu() {
        let resolve = |it: &Item| match &it.action {
            Action::WorkspaceEdit(v) => v["__resolve"].clone(),
            _ => Value::Null,
        };
        // typos-lsp / harper-ls: "Replace with" carries its edit (applied directly).
        let fix = json!({"title": "Replace with: receive", "kind": "quickfix", "edit": {"changes": {}},
            "command": {"title": "Record lint statistic", "command": "HarperRecordLint"}});
        let it = code_action_item(&fix, Some("harper-ls")).unwrap();
        assert_eq!((it.label.as_str(), it.detail.as_str()), ("Replace with: receive", "quickfix"));
        assert!(matches!(&it.action, Action::WorkspaceEdit(v) if v.get("changes").is_some()));
        // harper-ls: "Add to the user dictionary" is a bare Command, executed on the server that sent it.
        let add = json!({"title": "Add \"recieve\" to the user dictionary.", "command": "HarperAddToUserDict",
            "arguments": ["recieve", "file:///x/a.md"]});
        let it = code_action_item(&add, Some("harper-ls")).unwrap();
        let r = resolve(&it);
        assert_eq!(r["command"]["command"], json!("HarperAddToUserDict"));
        assert_eq!(r["command"]["arguments"], json!(["recieve", "file:///x/a.md"]));
        assert_eq!(r["__server"], json!("harper-ls"));
        // typos-lsp: a CodeAction that only has a command object.
        let ignore = json!({"title": "Ignore `recieve` in the project", "command": {"command": "ignore-in-project",
            "arguments": [{"typo": "recieve"}]}});
        let r = resolve(&code_action_item(&ignore, Some("typos-lsp")).unwrap());
        assert_eq!(r["command"]["command"], json!("ignore-in-project"));
        // Nothing to do, or no title: not listed.
        assert!(code_action_item(&json!({"title": "noop"}), None).is_none());
        assert!(code_action_item(&json!({"command": "X"}), None).is_none());
    }

    fn act(label: &str) -> Item {
        Item { label: label.into(), detail: "quickfix".into(), action: Action::WorkspaceEdit(json!({"changes": {}})) }
    }

    fn menu(app: &App) -> Option<(Vec<String>, usize)> {
        match &app.overlay {
            Overlay::Picker(p) if p.kind == PickKind::CodeActions => {
                Some((p.items.iter().map(|it| it.label.clone()).collect(), p.sel))
            }
            _ => None,
        }
    }

    #[test]
    fn quick_fix_menu_opens_with_the_first_items_and_merges_later_answers() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("actions-progressive");
        let mut app = test_app(&dir);
        let i = app.active;

        // Three servers asked: an empty answer shows nothing, the first items open the menu.
        let s = app.actions_begin(i);
        app.actions_expected = 3;
        app.actions_response(s, Vec::new());
        assert!(menu(&app).is_none());
        app.actions_response(s, vec![act("Fix A"), act("Fix B")]);
        assert_eq!(menu(&app), Some((vec!["Fix A".into(), "Fix B".into()], 0)));
        // The user moved to the second row; the last answer joins without moving the selection.
        if let Overlay::Picker(p) = &mut app.overlay {
            p.sel = 1;
        }
        app.actions_response(s, vec![act("Fix B"), act("Fix C")]);
        assert_eq!(menu(&app), Some((vec!["Fix A".into(), "Fix B".into(), "Fix C".into()], 1)));
        assert!(app.lspx.actions.key.is_none() && app.actions_items.is_empty());

        // Items known before any answer (the import index) open the menu at once.
        app.overlay = Overlay::None;
        let s = app.actions_begin(i);
        app.actions_expected = 1;
        app.actions_items = vec![act("Add import")];
        app.actions_show();
        assert_eq!(menu(&app), Some((vec!["Add import".into()], 0)));
        app.actions_response(s, vec![act("Fix A")]);
        assert_eq!(menu(&app), Some((vec!["Add import".into(), "Fix A".into()], 0)));

        // Everyone answered empty: the message, no menu.
        app.overlay = Overlay::None;
        app.status = None;
        let s = app.actions_begin(i);
        app.actions_expected = 2;
        app.actions_response(s, Vec::new());
        assert!(app.status.is_none());
        app.actions_response(s, Vec::new());
        assert!(menu(&app).is_none());
        assert_eq!(app.status.as_ref().map(|m| m.0.as_str()), Some("no actions available here"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn after_an_edit_the_edited_and_active_documents_are_pulled_first() {
        let open = vec!["file:///a.ts".to_string(), "file:///b.ts".to_string(), "file:///c.ts".to_string()];
        let first =
            ["file:///c.ts".to_string(), "file:///a.ts".to_string(), "file:///not-on-this-server.ts".to_string()];
        let (now, later) = pull_split(open.clone(), &first);
        assert_eq!(now, vec!["file:///a.ts", "file:///c.ts"]);
        assert_eq!(later, vec!["file:///b.ts"]);
        let (now, later) = pull_split(open, &[]);
        assert!(now.is_empty() && later.len() == 3);
    }

    fn lens(line: usize, title: Option<&str>) -> crate::doc::Lens {
        crate::doc::Lens { line, col: 0, title: title.map(str::to_string), raw: Value::Null }
    }

    #[test]
    fn only_lenses_near_the_viewport_are_resolved() {
        // One lens every 10 lines of a 2000-line file; the view shows lines 400..=440.
        let lines: Vec<usize> = (0..200).map(|k| k * 10).collect();
        let mut done = vec![false; lines.len()];
        let ask = lenses_to_resolve(&lines, &done, (400, 440));
        let asked: Vec<usize> = ask.iter().map(|&k| lines[k]).collect();
        assert_eq!(asked, vec![380, 390, 400, 410, 420, 430, 440, 450, 460]);
        // What was asked is not asked again; scrolling a little only adds the new lines.
        for k in ask {
            done[k] = true;
        }
        assert!(lenses_to_resolve(&lines, &done, (400, 440)).is_empty());
        let more: Vec<usize> = lenses_to_resolve(&lines, &done, (420, 460)).iter().map(|&k| lines[k]).collect();
        assert_eq!(more, vec![470, 480]);
        // A viewport full of lenses is capped.
        let dense: Vec<usize> = (0..1000).collect();
        assert_eq!(lenses_to_resolve(&dense, &vec![false; 1000], (0, 999)).len(), LENS_BATCH);
    }

    #[test]
    fn a_lens_keeps_its_title_while_line_and_text_are_the_same() {
        let range = |a: u64, b: u64| json!({"start": {"line": 3, "character": a}, "end": {"line": 3, "character": b}});
        assert_eq!(lens_name("export function soma(a: number) {", &range(16, 20)), "soma");
        // UTF-16 columns, and a range that runs past the line or onto the next ones.
        assert_eq!(lens_name("const 😀x = 1", &range(8, 9)), "x");
        assert_eq!(lens_name("ab", &range(1, 40)), "b");
        let multi = json!({"start": {"line": 3, "character": 6}, "end": {"line": 9, "character": 1}});
        assert_eq!(lens_name("class Repo {", &multi), "Repo {");

        let old = [lens(3, Some("2 references")), lens(3, Some("1 implementation")), lens(8, Some("5 references"))];
        let old_names: Vec<String> = ["Repo", "Repo", "find"].map(String::from).to_vec();
        // Same lines and names: titles stay. A renamed symbol or a moved line starts empty.
        let mut new = [lens(3, None), lens(3, None), lens(8, None), lens(12, None)];
        let names: Vec<String> = ["Repo", "Repo", "search", "find"].map(String::from).to_vec();
        carry_lens_titles(&old, &old_names, &mut new, &names);
        let titles: Vec<Option<&str>> = new.iter().map(|l| l.title.as_deref()).collect();
        assert_eq!(titles, vec![Some("2 references"), Some("1 implementation"), None, None]);
        // A title the server already sent is not replaced.
        let mut new = [lens(3, Some("9 references"))];
        carry_lens_titles(&old, &old_names, &mut new, &["Repo".to_string()]);
        assert_eq!(new[0].title.as_deref(), Some("9 references"));
    }

    #[test]
    fn prefix_match_ignores_case_without_lowercasing_each_label() {
        for (label, prefix) in
            [("useState", "uses"), ("USE_STATE", "use_"), ("Ünïcode", "ünï"), ("İstanbul", "i\u{307}s")]
        {
            assert!(starts_with_lower(label, prefix), "{label} / {prefix}");
            assert!(label.to_lowercase().starts_with(prefix));
        }
        for (label, prefix) in [("useState", "state"), ("us", "use"), ("", "a")] {
            assert!(!starts_with_lower(label, prefix), "{label} / {prefix}");
            assert!(!label.to_lowercase().starts_with(prefix));
        }
        assert!(starts_with_lower("anything", "") && starts_with_lower("", ""));
    }

    #[test]
    fn a_completion_answer_is_filtered_ranked_and_its_items_kept_whole() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("completion-answer");
        let mut app = test_app(&dir);
        let i = app.active;
        let uri = app.docs[i].uri().expect("the seed file has a URI");
        // "se|ed": the word being completed is "se".
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, 2, false);
        let answer = json!({"isIncomplete": false, "items": [
            {"label": "xsxxe", "data": {"n": 1}},
            {"label": "zzz"},
            {"label": "SEcond", "data": {"n": 2}, "additionalTextEdits": [{"newText": "import x;\n",
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}}]},
        ]});
        app.on_completion(&uri, 2, answer);
        let c = app.completion.as_ref().expect("the popup");
        let shown: Vec<&str> = c.filtered.iter().map(|&k| c.items[k].label.as_str()).collect();
        assert_eq!(shown.first(), Some(&"SEcond"), "a case-insensitive prefix match ranks first: {shown:?}");
        assert!(shown.contains(&"xsxxe") && !shown.contains(&"zzz"), "{shown:?}");
        assert!((c.start, c.seed_len) == (0, 2));
        // The raw item (what resolve and the import edits need) is the server's, untouched.
        let k = c.items.iter().position(|it| it.label == "SEcond").unwrap();
        assert_eq!(c.items[k].raw["data"]["n"], 2);
        assert_eq!(parse_edits(&c.items[k].raw["additionalTextEdits"]).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_quick_fix_answers_are_dropped() {
        use crate::app_unsaved::testkit::{lock, test_app, tmp};
        let _g = lock();
        let dir = tmp("actions-stale");
        let mut app = test_app(&dir);
        let i = app.active;

        // Asked again: the answer of the first round is not shown.
        let old = app.actions_begin(i);
        let new = app.actions_begin(i);
        app.actions_expected = 1;
        app.actions_response(old, vec![act("Old")]);
        assert!(menu(&app).is_none());
        assert_eq!(app.actions_expected, 1, "a stale answer does not count for the new round");
        app.actions_response(new, vec![act("New")]);
        assert_eq!(menu(&app).map(|m| m.0), Some(vec!["New".to_string()]));

        // The user closed the menu while a server was still working.
        let s = app.actions_begin(i);
        app.actions_expected = 2;
        app.actions_response(s, vec![act("Fix A")]);
        assert!(menu(&app).is_some());
        app.overlay = Overlay::None;
        app.actions_response(s, vec![act("Fix B")]);
        assert!(menu(&app).is_none(), "a closed menu must not come back");

        // The caret moved before the first answer.
        let s = app.actions_begin(i);
        app.actions_expected = 1;
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, 2, false);
        app.actions_response(s, vec![act("Fix A")]);
        assert!(menu(&app).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
