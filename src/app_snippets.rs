//! App glue of the VS Code compatible snippets (`snippet_store`, `snippet_syntax`): expansion on
//! Tab, suggestions in the completion popup, "Snippets: Insert / Configure / Import from VS Code".

use crate::{
    app::App,
    app_lsp::{CItem, Completion},
    app_overlay::{Action, Cmd, Item, Overlay, choice_picker},
    clipboard::Clipboard,
    snippet_store::{self, TEMPLATE},
    snippet_syntax::Ctx,
};
use serde_json::json;
use std::path::{Path, PathBuf};

/// What the snippet pickers hand back.
#[derive(Clone, Debug, PartialEq)]
pub enum SnipPick {
    /// Insert this body at the cursor.
    Insert(String),
    /// Open this snippet file, creating it from the template first if it does not exist.
    Open(PathBuf),
}

/// The part of the variable context only the app knows. The clipboard is read only when the
/// body asks for it.
pub fn env(root: &Path, clip: &mut Clipboard, body: &str, file: Option<&Path>) -> Ctx {
    Ctx {
        clipboard: if body.contains("CLIPBOARD") { clip.get() } else { String::new() },
        workspace_folder: root.to_string_lossy().into_owned(),
        workspace_name: root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        relative: file
            .map(|p| p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/"))
            .unwrap_or_default(),
        ..Ctx::default()
    }
}

/// Snippets usable in `lang` as completion items: the user's and project's first, then the
/// built-in ones whose prefix they do not redefine.
pub fn snippet_citems(root: &Path, lang: Option<&str>) -> Vec<CItem> {
    let store = snippet_store::current(root);
    let mut items: Vec<CItem> = Vec::new();
    let mut push = |label: &str, detail: String, body: &str| {
        items.push(CItem {
            label: label.to_string(),
            detail,
            insert: body.to_string(),
            kind: 15,
            sort: format!("~{label}"),
            range: None,
            raw: json!({"label": label, "__snippet": body}),
        })
    };
    let user = store.candidates(lang);
    for (prefix, s) in &user {
        let d = if s.description.is_empty() { s.name.clone() } else { s.description.clone() };
        push(prefix, format!("snippet: {d}"), &s.body);
    }
    for b in crate::snippets::for_lang(lang) {
        if !user.iter().any(|(p, _)| p == b.prefix) {
            push(b.prefix, "snippet".into(), b.body);
        }
    }
    items
}

/// Languages where prose is typed: no automatic snippet popup there.
fn prose(lang: Option<&str>) -> bool {
    matches!(lang, None | Some("markdown" | "mdx" | "plaintext"))
}

impl App {
    pub(crate) fn run_snippet_cmd(&mut self, c: Cmd) -> bool {
        match c {
            Cmd::SnippetsInsert => self.snippets_insert_picker(),
            Cmd::SnippetsConfigure => self.snippets_configure_picker(),
            Cmd::SnippetsImport => self.snippets_import(),
            _ => return false,
        }
        true
    }

    pub(crate) fn snippet_pick(&mut self, p: SnipPick) {
        match p {
            SnipPick::Insert(body) => self.snippet_insert_body(&body),
            SnipPick::Open(path) => {
                if !path.exists() {
                    let made = path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(&path, TEMPLATE));
                    if let Err(e) = made {
                        return self.say(format!("cannot create {}: {e}", path.display()));
                    }
                    snippet_store::invalidate();
                }
                self.run_action(Action::Open(path, None));
            }
        }
    }

    /// Expand `body` at the cursor (replacing the selection, which `$SELECTION` then holds).
    pub(crate) fn snippet_insert_body(&mut self, body: &str) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let (a, b) = {
            let s = d.ed.sel;
            (s.anchor.min(s.head), s.anchor.max(s.head))
        };
        let path = d.path().map(Path::to_path_buf);
        let ctx = env(&self.root, &mut self.clip, body, path.as_deref());
        d.insert_snippet_ctx(a, b, body, ctx);
        self.request_redraw();
    }

    fn snippets_insert_picker(&mut self) {
        let Some(lang) = self.docs.get(self.active).map(|d| d.lang) else { return };
        let mut items: Vec<Item> = Vec::new();
        let store = snippet_store::current(&self.root);
        for s in store.items.iter().rev().filter(|s| s.applies_to(lang)) {
            let prefix = s.prefixes.first().cloned().unwrap_or_default();
            let mut detail = prefix;
            if !s.description.is_empty() {
                detail = format!("{detail} · {}", s.description);
            }
            items.push(Item {
                label: s.name.clone(),
                detail: detail.trim_start_matches(" · ").to_string(),
                action: Action::Snippet(SnipPick::Insert(s.body.clone())),
            });
        }
        for b in crate::snippets::for_lang(lang) {
            items.push(Item {
                label: b.prefix.to_string(),
                detail: "built-in".into(),
                action: Action::Snippet(SnipPick::Insert(b.body.to_string())),
            });
        }
        if items.is_empty() {
            return self.say("no snippets for this language (Snippets: Configure creates a file)");
        }
        self.overlay = Overlay::Picker(choice_picker("Insert snippet", items, 0));
        self.request_redraw();
    }

    fn snippets_configure_picker(&mut self) {
        let lang = self.docs.get(self.active).and_then(|d| d.lang);
        let user = snippet_store::user_dir();
        let project = self.root.join(".lumen").join("snippets");
        let mut targets: Vec<(String, PathBuf)> = Vec::new();
        if let Some(u) = &user {
            if let Some(l) = lang {
                targets.push((format!("User: {l}.json"), u.join(format!("{l}.json"))));
            }
            targets.push(("User: global snippets (lumen.code-snippets)".into(), u.join("lumen.code-snippets")));
        }
        if let Some(l) = lang {
            targets.push((format!("Project: .lumen/snippets/{l}.json"), project.join(format!("{l}.json"))));
        }
        targets.push(("Project: .lumen/snippets/project.code-snippets".into(), project.join("project.code-snippets")));
        // Files that already exist, not listed above.
        let mut existing: Vec<PathBuf> = Vec::new();
        for dir in user.iter().chain([&project]) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if (n.ends_with(".json") || n.ends_with(".code-snippets")) && !targets.iter().any(|(_, t)| *t == p)
                    {
                        existing.push(p);
                    }
                }
            }
        }
        existing.sort();
        let mut items: Vec<Item> = targets
            .into_iter()
            .map(|(label, p)| Item {
                detail: if p.exists() { String::new() } else { "new file".into() },
                label,
                action: Action::Snippet(SnipPick::Open(p)),
            })
            .collect();
        items.extend(existing.into_iter().map(|p| Item {
            label: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            detail: p.parent().map(|d| d.display().to_string()).unwrap_or_default(),
            action: Action::Snippet(SnipPick::Open(p)),
        }));
        self.overlay = Overlay::Picker(choice_picker("Configure snippets", items, 0));
        self.request_redraw();
    }

    fn snippets_import(&mut self) {
        let dirs = snippet_store::vscode_snippet_dirs();
        let has_files = |d: &PathBuf| {
            std::fs::read_dir(d).is_ok_and(|rd| {
                rd.flatten().any(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.ends_with(".code-snippets") || (n.ends_with(".json") && n != "package.json")
                })
            })
        };
        let Some(from) = dirs.iter().find(|d| has_files(d)).or(dirs.first()) else {
            return self.say("no VS Code (or VSCodium) user snippets folder found");
        };
        let Some(to) = snippet_store::user_dir() else { return self.say("no user config folder") };
        match snippet_store::copy_snippets(from, &to) {
            Ok(w) => {
                snippet_store::invalidate();
                let app = from.parent().and_then(Path::parent).and_then(|p| p.file_name()).map(|n| n.to_string_lossy());
                self.say(format!("{} snippet file(s) imported from {}", w.len(), app.as_deref().unwrap_or("VS Code")));
            }
            Err(e) => self.say(format!("snippet import failed: {e}")),
        }
    }

    /// Opens the completion popup with snippets only (no language server, or a non-TypeScript
    /// language). `force` also shows it for an empty prefix (Ctrl+Space); otherwise it needs a
    /// typed prefix and a language where code is written.
    pub(crate) fn snippet_completion(&mut self, force: bool) {
        let Some(d) = self.docs.get(self.active) else { return };
        if !force && prose(d.lang) {
            return;
        }
        let head = d.ed.head();
        let mut start = head;
        while start > 0 && d.buf.char_at(start - 1).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$') {
            start -= 1;
        }
        if !force && (start == head || d.buf.char_at(start).is_some_and(|c| c.is_ascii_digit())) {
            return;
        }
        let items = snippet_citems(&self.root, d.lang);
        if items.is_empty() {
            return;
        }
        let seed_len = head - start;
        let mut c = Completion { items, filtered: Vec::new(), sel: 0, scroll: 0, start, seed_len };
        self.refilter_completion_into(&mut c);
        self.completion = if c.filtered.is_empty() { None } else { Some(c) };
        self.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{buffer::Buffer, doc::Doc};

    fn doc(text: &str) -> Doc {
        Doc::new(Buffer::from_str(text), None, None, "//", 8.0)
    }

    fn type_str(d: &mut Doc, s: &str) {
        for ch in s.chars() {
            d.ed.type_char(&mut d.buf, ch);
            d.snippet_sync();
        }
    }

    #[test]
    fn env_reads_the_clipboard_only_when_asked() {
        let mut clip = Clipboard::in_process();
        clip.set("clip!");
        let root = Path::new("/work/proj");
        let e = env(root, &mut clip, "x $CLIPBOARD", Some(Path::new("/work/proj/src/a.rs")));
        assert_eq!(e.clipboard, "clip!");
        assert_eq!((e.workspace_name.as_str(), e.relative.as_str()), ("proj", "src/a.rs"));
        assert_eq!(env(root, &mut clip, "plain", None).clipboard, "");
    }

    #[test]
    fn tab_stops_visit_in_order_and_mirrors_follow_typing() {
        let mut d = doc("");
        d.insert_snippet_ctx(0, 0, "let ${1:name} = $2; // $1\n$0", Ctx::default());
        assert_eq!(d.buf.to_string(), "let name = ; // name\n");
        assert_eq!(d.ed.selected_text(&d.buf), "name", "the first placeholder is selected");
        type_str(&mut d, "count");
        assert_eq!(d.buf.to_string(), "let count = ; // count\n", "the mirror updates while typing");
        assert!(d.snippet_next());
        type_str(&mut d, "5");
        assert_eq!(d.buf.to_string(), "let count = 5; // count\n");
        assert!(d.snippet_next());
        assert_eq!(d.ed.head(), d.buf.len_chars(), "$0 is the last stop");
        assert!(!d.snippet_next(), "the session ended");
    }

    #[test]
    fn mirror_before_the_stop_and_transforms() {
        let mut d = doc("");
        d.insert_snippet_ctx(0, 0, "$1: ${1:foo_bar} ${1/(\\w+)_(\\w+)/$2-$1/}", Ctx::default());
        assert_eq!(d.buf.to_string(), "foo_bar: foo_bar bar-foo");
        assert_eq!(d.ed.selected_text(&d.buf), "foo_bar");
        type_str(&mut d, "a_b");
        assert_eq!(d.buf.to_string(), "a_b: a_b b-a");
        assert_eq!(d.ed.selected_text(&d.buf), "", "the caret is still after what was typed");
        assert_eq!(d.ed.head(), 8);
    }

    #[test]
    fn typing_elsewhere_ends_the_session() {
        let mut d = doc("tail");
        d.insert_snippet_ctx(0, 0, "${1:a} $1 ", Ctx::default());
        let end = d.buf.len_chars();
        d.ed.click(&mut d.buf, end, false);
        type_str(&mut d, "x");
        assert!(d.snippet.is_none());
        assert_eq!(d.buf.to_string(), "a a tailx", "the mirror stayed as it was");
    }

    #[test]
    fn variables_come_from_the_document() {
        let mut d = doc("one\ntwo");
        d.ed.click(&mut d.buf, 5, false);
        d.insert_snippet_ctx(4, 4, "$TM_LINE_NUMBER/$TM_LINE_INDEX [$TM_CURRENT_WORD] $LINE_COMMENT", Ctx::default());
        assert_eq!(d.buf.line(1), "2/1 [two] //two");
    }

    #[test]
    fn selection_is_replaced_and_available() {
        let mut d = doc("hello");
        d.ed.select_all(&mut d.buf);
        d.insert_snippet_ctx(0, 5, "<b>$TM_SELECTED_TEXT</b>$0", Ctx::default());
        assert_eq!(d.buf.to_string(), "<b>hello</b>");
    }

    #[test]
    fn completion_items_merge_user_and_builtin_snippets() {
        let root = std::env::temp_dir().join(format!("lumen-snip-ci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".lumen").join("snippets")).unwrap();
        std::fs::write(
            root.join(".lumen").join("snippets").join("rust.json"),
            r#"{"mine": {"prefix": ["fn", "mine"], "body": "mine($1)", "description": "my snippet"}}"#,
        )
        .unwrap();
        let items = snippet_citems(&root, Some("rust"));
        let fns: Vec<&CItem> = items.iter().filter(|i| i.label == "fn").collect();
        assert_eq!(fns.len(), 1, "a user prefix replaces the built-in one");
        assert_eq!(fns[0].raw["__snippet"], "mine($1)");
        assert_eq!(fns[0].detail, "snippet: my snippet");
        assert!(items.iter().any(|i| i.label == "mine" && i.kind == 15));
        assert!(items.iter().any(|i| i.label == "impl"), "built-ins stay");
        assert!(snippet_citems(&root, Some("go")).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
