//! The editor's import index (see `importidx`) wired into the app: built on a worker thread,
//! offered in completion and in the quick-fix menu, and written into the file on accept.

use crate::{
    app::App,
    app_lsp::{CItem, ts_family},
    app_overlay::{Action, Item},
    doc::LspEdit,
    importidx::{self, Index},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// How many index names ride along in a completion list.
const MAX_ITEMS: usize = 40;
/// Typing less than this offers nothing from the index (too many names match one letter).
const MIN_PREFIX: usize = 2;
/// A change on disk updates the index this long after the last one (only what changed is read
/// again, and packages come from the cache on disk, see `importidx::build_from`).
const REBUILD_AFTER: Duration = Duration::from_secs(3);

/// State of the import index of the open project.
pub struct ImportIdx {
    /// The index and the root it was built for.
    pub current: Option<(PathBuf, Arc<Index>)>,
    pub ch:
        (std::sync::mpsc::Sender<(PathBuf, Index, Duration)>, std::sync::mpsc::Receiver<(PathBuf, Index, Duration)>),
    pub busy: bool,
    pub due: Option<Instant>,
}

impl Default for ImportIdx {
    fn default() -> Self {
        ImportIdx { current: None, ch: std::sync::mpsc::channel(), busy: false, due: None }
    }
}

/// The package a module specifier belongs to (`@prisma/client/edge` -> `@prisma/client`): a name
/// is offered once per package, from its best ranked entry point.
fn package_of(spec: &str) -> &str {
    if spec.starts_with('.') || spec.starts_with("@/") {
        return spec;
    }
    let cut = if spec.starts_with('@') { 2 } else { 1 };
    match spec.match_indices('/').nth(cut - 1) {
        Some((i, _)) => &spec[..i],
        None => spec,
    }
}

fn enabled() -> bool {
    !cfg!(test) && std::env::var_os("LUMEN_NO_IMPORT_INDEX").is_none()
}

impl App {
    /// Something that can change the exports happened (a save, files created or removed, a new
    /// `package.json`): update shortly.
    pub(crate) fn import_idx_dirty(&mut self) {
        if self.import_idx.current.is_some() || self.import_idx.busy {
            self.import_idx.due = Some(Instant::now() + REBUILD_AFTER);
        }
    }

    pub(crate) fn import_idx_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        while let Ok((root, idx, took)) = self.import_idx.ch.1.try_recv() {
            self.import_idx.busy = false;
            if std::env::var_os("LUMEN_DEBUG").is_some() {
                eprintln!(
                    "[imports] {} names, {} files and {} packages read in {took:?}",
                    idx.len(),
                    idx.files_scanned,
                    idx.packages_scanned
                );
            }
            if root == self.root {
                self.import_idx.current = Some((root, Arc::new(idx)));
            }
        }
        if !enabled() || self.import_idx.busy {
            return;
        }
        let stale = self.import_idx.current.as_ref().is_none_or(|(r, _)| *r != self.root);
        // First build: as soon as a TypeScript/JavaScript file is open in this project.
        if stale && self.import_idx.due.is_none() && self.docs.iter().any(|d| ts_family(d.lang)) {
            self.import_idx.due = Some(now);
        }
        let Some(due) = self.import_idx.due else { return };
        if due > now {
            return want(due);
        }
        self.import_idx.due = None;
        self.import_idx.busy = true;
        let (root, tx, wake) = (self.root.clone(), self.import_idx.ch.0.clone(), self.wake());
        // The index in use is the base of the next one: what did not change is not read again.
        let old = self.import_index().cloned();
        std::thread::spawn(move || {
            let t = Instant::now();
            let idx = importidx::build_from(&root, old.as_deref(), Some(&importidx::cache_file(&root)));
            drop(old);
            let _ = tx.send((root, idx, t.elapsed()));
            wake();
        });
    }

    fn import_index(&self) -> Option<&Arc<Index>> {
        self.import_idx.current.as_ref().filter(|(r, _)| *r == self.root).map(|(_, i)| i)
    }

    /// Names from the index for what is typed in document `i`, as completion items. `have` are the
    /// labels already in the list (in scope or offered by the server).
    pub(crate) fn import_citems(&self, i: usize, prefix: &str, have: &std::collections::HashSet<&str>) -> Vec<CItem> {
        let (Some(idx), Some(d)) = (self.import_index(), self.docs.get(i)) else { return Vec::new() };
        let Some(path) = d.path().filter(|_| ts_family(d.lang)) else { return Vec::new() };
        if prefix.chars().count() < MIN_PREFIX {
            return Vec::new();
        }
        // With tsgo's auto-import on, the server already lists importable names: never twice. With it
        // off, a label in the server's list may be a global of the same name (`Image`, `Request`), so
        // only what this file already imports is left out.
        let server_imports = self.auto_import_on();
        let mut imported: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut in_import = false;
        for l in 0..d.buf.line_count().min(400) {
            let line = d.buf.line(l);
            if line.starts_with("import ") || line.starts_with("import{") {
                in_import = true;
            }
            if in_import {
                let head = line.split(" from ").next().unwrap_or("");
                imported.extend(
                    head.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                        .filter(|w| !w.is_empty() && !matches!(*w, "import" | "type" | "as" | "from"))
                        .map(str::to_string),
                );
                if line.contains(['"', '\'']) {
                    in_import = false;
                }
            }
        }
        let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for h in idx.search(prefix, MAX_ITEMS * 2) {
            if imported.contains(&h.name)
                || (server_imports && have.contains(h.name.as_str()))
                || h.source == importidx::Source::File(path.to_path_buf())
            {
                continue;
            }
            let spec = idx.specifier(&h.source, path);
            if !seen.insert((h.name.clone(), package_of(&spec).to_string())) {
                continue;
            }
            out.push(CItem {
                label: h.name.clone(),
                detail: spec.clone(),
                insert: h.name.clone(),
                // LSP CompletionItemKind: interface for types, variable otherwise.
                kind: if h.is_type { 8 } else { 6 },
                sort: format!("~~{}", h.name),
                range: None,
                raw: json!({"label": h.name, "__import": {"name": h.name, "spec": spec, "type": h.is_type, "default": h.is_default}}),
            });
            if out.len() >= MAX_ITEMS {
                break;
            }
        }
        out
    }

    /// Write the import an accepted index item needs into document `i`.
    pub(crate) fn import_apply(&mut self, i: usize, imp: &Value) {
        let Some(d) = self.docs.get_mut(i) else { return };
        let (Some(name), Some(spec)) = (imp["name"].as_str(), imp["spec"].as_str()) else { return };
        let text = d.buf.to_string();
        let (is_type, is_default) = (imp["type"].as_bool().unwrap_or(false), imp["default"].as_bool().unwrap_or(false));
        if let Some(ins) = importidx::import_edit(&text, name, spec, is_type, is_default) {
            d.apply_lsp_edits(&[LspEdit { sl: ins.line, sc: ins.col, el: ins.line, ec: ins.col, text: ins.text }]);
        }
    }

    /// "Add import" quick fixes from the index for the unresolved names under the selection of
    /// document `i` (TS2304 and friends), ready before any server answers.
    pub(crate) fn import_action_items(&self, i: usize) -> Vec<Item> {
        let (Some(idx), Some(d)) = (self.import_index(), self.docs.get(i)) else { return Vec::new() };
        let (Some(path), Some(uri)) = (d.path().filter(|_| ts_family(d.lang)), d.real_uri()) else { return Vec::new() };
        let sel = d.ed.sel;
        let (l0, c0) = d.buf.line_col(sel.start());
        let (l1, c1) = d.buf.line_col(sel.end());
        let text = d.buf.to_string();
        let mut out: Vec<Item> = Vec::new();
        let mut packages: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
        for g in d.diags.iter().filter(|g| (g.sl, g.sc) <= (l1, c1) && (l0, c0) <= (g.el, g.ec.max(g.sc + 1))) {
            if !matches!(g.code.as_str(), "2304" | "2552" | "2503" | "2582" | "2580" | "2686") {
                continue;
            }
            let Some(name) = g.message.split('\'').nth(1) else { continue };
            for h in idx.exact(name) {
                if h.source == importidx::Source::File(path.to_path_buf()) {
                    continue;
                }
                let spec = idx.specifier(&h.source, path);
                if !packages.insert((name.to_string(), package_of(&spec).to_string())) {
                    continue;
                }
                let Some(ins) = importidx::import_edit(&text, name, &spec, h.is_type, h.is_default) else { continue };
                let label = format!("Add import from \"{spec}\"");
                if out.iter().any(|it| it.label == label) {
                    continue;
                }
                let pos = json!({"line": ins.line, "character": ins.col});
                let edit =
                    json!({"changes": {uri.clone(): [{"range": {"start": pos, "end": pos}, "newText": ins.text}]}});
                out.push(Item { label, detail: name.to_string(), action: Action::WorkspaceEdit(edit) });
                if out.len() >= 8 {
                    return out;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, scratch_base, test_app};

    #[test]
    fn package_of_strips_subpaths_only() {
        assert_eq!(package_of("@prisma/client/edge"), "@prisma/client");
        assert_eq!(package_of("@prisma/client"), "@prisma/client");
        assert_eq!(package_of("next/link"), "next");
        assert_eq!(package_of("next"), "next");
        assert_eq!(package_of("@/src/h"), "@/src/h");
        assert_eq!(package_of("./h"), "./h");
        assert_eq!(package_of("node:fs"), "node:fs");
    }

    #[test]
    fn index_names_complete_import_and_fix_unresolved_names() {
        let _g = lock();
        let dir = scratch_base().join(format!("importidx-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("app")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/next")).unwrap();
        let dir = crate::paths::canonical(&dir).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"dependencies":{"next":"1"}}"#).unwrap();
        std::fs::write(dir.join("tsconfig.json"), r#"{"compilerOptions":{"paths":{"@/*":["./*"]}}}"#).unwrap();
        std::fs::write(dir.join("src/h.ts"), "export function expectOk() {}\n").unwrap();
        std::fs::write(dir.join("node_modules/next/package.json"), r#"{"types":"index.d.ts"}"#).unwrap();
        std::fs::write(dir.join("node_modules/next/index.d.ts"), "export type { Metadata } from './types';\n").unwrap();
        let page = dir.join("app/page.ts");
        std::fs::write(&page, "import { a } from \"./a\";\n\nconst m: Metadata = {};\nexok\n").unwrap();

        let mut app = test_app(&dir);
        let i = app.open_path(&page, None).expect("open");
        app.active = i;
        app.import_idx.current = Some((app.root.clone(), Arc::new(importidx::build(&app.root))));

        // Completion: the abbreviation finds the project's helper, with its alias path.
        let none = std::collections::HashSet::new();
        let items = app.import_citems(i, "exok", &none);
        assert_eq!(items.len(), 1, "{:?}", items.iter().map(|c| &c.label).collect::<Vec<_>>());
        assert_eq!((items[0].label.as_str(), items[0].detail.as_str()), ("expectOk", "@/src/h"));
        assert!(app.import_citems(i, "e", &none).is_empty(), "one letter offers nothing");
        assert!(app.import_citems(i, "a", &none).is_empty());

        // Accepting it writes the import below the existing one.
        let imp = items[0].raw["__import"].clone();
        app.import_apply(i, &imp);
        let text = app.docs[i].buf.to_string();
        assert!(text.starts_with("import { a } from \"./a\";\nimport { expectOk } from \"@/src/h\";\n"), "{text}");
        // Now imported: not offered again.
        assert!(app.import_citems(i, "exok", &none).is_empty());

        // Quick fix: an unresolved name under the caret gets "Add import".
        let d = &mut app.docs[i];
        d.diags = vec![crate::doc::Diag {
            sl: 3,
            sc: 9,
            el: 3,
            ec: 17,
            severity: 1,
            message: "Cannot find name 'Metadata'.".into(),
            code: "2304".into(),
            source: String::new(),
        }];
        let p = d.buf.pos(3, 10);
        d.ed.click(&mut d.buf, p, false);
        let actions = app.import_action_items(i);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].label, "Add import from \"next\"");
        let Action::WorkspaceEdit(edit) = &actions[0].action else { panic!("not an edit") };
        let edits = edit["changes"].as_object().unwrap().values().next().unwrap();
        assert_eq!(edits[0]["newText"], "import type { Metadata } from \"next\";\n");
        assert_eq!(edits[0]["range"]["start"]["line"], 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
