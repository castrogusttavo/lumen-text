//! User and project snippets in VS Code's format: `*.code-snippets` (global, optional `scope`)
//! and per-language `<language>.json` files. Read lazily the first time a snippet is needed and
//! refreshed when the files change; nothing is loaded at startup.
//!
//! Where they live: `<config>/lumen-text/snippets`, `<project>/.lumen/snippets` and
//! `<project>/.vscode/*.code-snippets`. Later sources win over earlier ones for the same prefix
//! (user, then `.vscode`, then `.lumen`).

use std::{
    cell::RefCell,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq)]
pub struct UserSnippet {
    pub name: String,
    pub prefixes: Vec<String>,
    pub body: String,
    /// Language ids it applies to (already canonical); empty = every language.
    pub scope: Vec<String>,
    pub description: String,
}

/// Language ids as VS Code writes them, mapped to ours (`typescriptreact` is our `tsx`…).
pub fn canon_lang(id: &str) -> String {
    match id.trim().to_ascii_lowercase().as_str() {
        "typescriptreact" => "tsx".into(),
        "javascriptreact" => "jsx".into(),
        "dockerfile" => "docker".into(),
        "makefile" => "make".into(),
        "jsonc" | "json5" => "json".into(),
        "plaintext" | "text" => "plaintext".into(),
        other => other.to_string(),
    }
}

impl UserSnippet {
    pub fn applies_to(&self, lang: Option<&str>) -> bool {
        self.scope.is_empty()
            || self.scope.iter().any(|s| Some(s.as_str()) == lang || (lang.is_none() && s == "plaintext"))
    }
}

/// Block comment delimiters of a language (for `$BLOCK_COMMENT_START` / `_END`).
pub fn block_comment(lang: Option<&str>) -> (&'static str, &'static str) {
    match lang {
        Some("html" | "xml" | "vue" | "svelte" | "markdown" | "mdx" | "ghp") => ("<!--", "-->"),
        Some("python") => ("\"\"\"", "\"\"\""),
        Some("lua") => ("--[[", "]]"),
        Some("haskell" | "elm") => ("{-", "-}"),
        Some("ruby") => ("=begin", "=end"),
        Some("shellscript" | "yaml" | "toml" | "make" | "docker" | "dotenv" | "ini" | "fish" | "just") | None => {
            ("", "")
        }
        _ => ("/*", "*/"),
    }
}

/// JSON with comments and trailing commas (what VS Code snippet files are) as plain JSON.
pub fn strip_jsonc(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    while i < c.len() {
        let ch = c[i];
        if in_str {
            out.push(ch);
            if ch == '\\' && i + 1 < c.len() {
                out.push(c[i + 1]);
                i += 1;
            } else if ch == '"' {
                in_str = false;
            }
        } else if ch == '"' {
            in_str = true;
            out.push(ch);
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else if ch == ',' {
            let mut j = i + 1;
            loop {
                while j < c.len() && c[j].is_whitespace() {
                    j += 1;
                }
                if c.get(j) == Some(&'/') && c.get(j + 1) == Some(&'/') {
                    while j < c.len() && c[j] != '\n' {
                        j += 1;
                    }
                } else if c.get(j) == Some(&'/') && c.get(j + 1) == Some(&'*') {
                    j += 2;
                    while j + 1 < c.len() && !(c[j] == '*' && c[j + 1] == '/') {
                        j += 1;
                    }
                    j += 2;
                } else {
                    break;
                }
            }
            if !matches!(c.get(j), Some('}' | ']')) {
                out.push(ch);
            }
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

fn string_or_lines(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(a) => Some(a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("\n")),
        _ => None,
    }
}

/// Snippets in one file's text. `lang_file` is the language of a `<language>.json` file, `None` for
/// a `*.code-snippets` file (where each snippet may carry its own `scope`).
pub fn parse(text: &str, lang_file: Option<&str>) -> Vec<UserSnippet> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(text)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, v) in map {
        let Some(body) = v.get("body").and_then(string_or_lines) else { continue };
        let prefixes: Vec<String> = match v.get("prefix") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        let scope: Vec<String> = match lang_file {
            Some(l) => vec![canon_lang(l)],
            None => v
                .get("scope")
                .and_then(|s| s.as_str())
                .map(|s| s.split(',').map(canon_lang).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
        };
        let description = v.get("description").and_then(string_or_lines).unwrap_or_default();
        out.push(UserSnippet { name, prefixes, body, scope, description });
    }
    out
}

/// `<config>/lumen-text/snippets`.
pub fn user_dir() -> Option<PathBuf> {
    Some(crate::paths::config_home()?.join("lumen-text").join("snippets"))
}

/// Snippet files in `dir` (`code_only`: only `*.code-snippets`), sorted by name.
fn files_in(dir: &Path, code_only: bool) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    n.ends_with(".code-snippets") || (!code_only && n.ends_with(".json") && n != "package.json")
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Every snippet source for a project, lowest priority first.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = user_dir() {
        v.extend(files_in(&d, false));
    }
    v.extend(files_in(&root.join(".vscode"), true));
    v.extend(files_in(&root.join(".lumen").join("snippets"), false));
    v
}

fn lang_of_file(p: &Path) -> Option<String> {
    let n = p.file_name()?.to_str()?;
    if n.ends_with(".code-snippets") { None } else { Some(n.strip_suffix(".json")?.to_string()) }
}

#[derive(Default)]
pub struct Store {
    pub items: Vec<UserSnippet>,
}

impl Store {
    pub fn load(root: &Path) -> Store {
        let mut items = Vec::new();
        for p in sources(root) {
            if let Ok(t) = std::fs::read_to_string(&p) {
                items.extend(parse(&t, lang_of_file(&p).as_deref()));
            }
        }
        Store { items }
    }

    /// The snippet whose prefix ends the text before the cursor (longest wins; later sources beat
    /// earlier ones). Returns how many chars of `before` it covers.
    pub fn lookup(&self, lang: Option<&str>, before: &str) -> Option<(usize, &UserSnippet)> {
        let run: Vec<char> = {
            let rev: Vec<char> = before.chars().rev().take_while(|c| !c.is_whitespace()).take(64).collect();
            rev.into_iter().rev().collect()
        };
        for i in 0..run.len() {
            let suffix: String = run[i..].iter().collect();
            if let Some(s) = self.items.iter().rev().find(|s| s.applies_to(lang) && s.prefixes.contains(&suffix)) {
                return Some((run.len() - i, s));
            }
        }
        None
    }

    /// Every (prefix, snippet) usable in `lang`; snippets without a prefix are listed under their name.
    pub fn candidates(&self, lang: Option<&str>) -> Vec<(String, &UserSnippet)> {
        let mut out: Vec<(String, &UserSnippet)> = Vec::new();
        for s in self.items.iter().filter(|s| s.applies_to(lang)) {
            if s.prefixes.is_empty() {
                continue;
            }
            for p in &s.prefixes {
                // A later definition of the same prefix replaces the earlier one.
                out.retain(|(q, _)| q != p);
                out.push((p.clone(), s));
            }
        }
        out
    }
}

struct Cache {
    root: PathBuf,
    checked: Instant,
    stamp: u64,
    store: Rc<Store>,
}

thread_local! {
    static CACHE: RefCell<Option<Cache>> = const { RefCell::new(None) };
}

/// Cheap fingerprint of the snippet files (names, sizes, mtimes).
fn stamp(root: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    for p in sources(root) {
        p.hash(&mut h);
        if let Ok(m) = std::fs::metadata(&p) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    }
    h.finish()
}

/// The snippets for a project, re-read when their files changed (checked at most every 2 s).
pub fn current(root: &Path) -> Rc<Store> {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(cache) = c.as_mut()
            && cache.root == root
        {
            if cache.checked.elapsed() < Duration::from_secs(2) {
                return cache.store.clone();
            }
            let st = stamp(root);
            cache.checked = Instant::now();
            if st != cache.stamp {
                cache.stamp = st;
                cache.store = Rc::new(Store::load(root));
            }
            return cache.store.clone();
        }
        let store = Rc::new(Store::load(root));
        *c =
            Some(Cache { root: root.to_path_buf(), checked: Instant::now(), stamp: stamp(root), store: store.clone() });
        store
    })
}

/// Forget the cache (after writing snippet files ourselves).
pub fn invalidate() {
    CACHE.with(|c| *c.borrow_mut() = None);
}

// ───────────── import from VS Code ─────────────

/// User snippet folders of the VS Code family installed here, in preference order.
pub fn vscode_snippet_dirs() -> Vec<PathBuf> {
    let Some(cfg) = crate::paths::config_home() else { return Vec::new() };
    ["Code", "Code - Insiders", "VSCodium", "VSCodium - Insiders", "Cursor", "Windsurf"]
        .iter()
        .map(|n| cfg.join(n).join("User").join("snippets"))
        .filter(|d| d.is_dir())
        .collect()
}

/// Copies the snippet files of `from` into `to` without overwriting: a file that exists under the
/// same name gets a numeric suffix unless its contents are identical. Returns the files written.
pub fn copy_snippets(from: &Path, to: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(to)?;
    let mut written = Vec::new();
    for src in files_in(from, false) {
        let Some(name) = src.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let data = std::fs::read(&src)?;
        let (stem, ext) = match name.split_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (name.clone(), String::new()),
        };
        let mut dest = to.join(&name);
        let mut n = 1;
        while dest.exists() {
            if std::fs::read(&dest).is_ok_and(|d| d == data) {
                break;
            }
            n += 1;
            dest = to.join(format!("{stem}-vscode{}{ext}", if n > 2 { format!("-{}", n - 1) } else { String::new() }));
        }
        if dest.exists() {
            continue;
        }
        std::fs::write(&dest, data)?;
        written.push(dest);
    }
    Ok(written)
}

/// Contents of a new snippet file.
pub const TEMPLATE: &str = "{\n\t// Snippets use VS Code syntax: $1, ${2:placeholder}, ${3|a,b|}, $TM_FILENAME, $CLIPBOARD ...\n\t// \"Log to console\": {\n\t// \t\"prefix\": \"log\",\n\t// \t\"body\": [\"console.log('$1');\", \"$0\"],\n\t// \t\"description\": \"Log output to console\"\n\t// }\n}\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "lumen-snip-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn jsonc_comments_and_trailing_commas() {
        let t = "{\n // c\n \"a\": [1, 2,], /* x */\n \"b\": \"//not a comment, }\",\n}";
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(t)).unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(v["b"], "//not a comment, }");
    }

    #[test]
    fn parses_prefix_arrays_bodies_and_scopes() {
        let t = r#"{
            // comment
            "Print": {"prefix": ["pr", "print"], "body": ["a", "b$0"], "scope": "javascript, typescriptreact", "description": "d"},
            "NoPrefix": {"body": "x"},
            "Bad": {"prefix": "z"},
        }"#;
        let v = parse(t, None);
        assert_eq!(v.len(), 2, "a snippet without a body is skipped");
        let p = v.iter().find(|s| s.name == "Print").unwrap();
        assert_eq!(p.prefixes, vec!["pr", "print"]);
        assert_eq!(p.body, "a\nb$0");
        assert_eq!(p.scope, vec!["javascript", "tsx"]);
        assert!(p.applies_to(Some("tsx")) && !p.applies_to(Some("rust")));
        let l = parse(t, Some("rust"));
        assert_eq!(l[0].scope, vec!["rust"], "the file name decides in language files");
        assert!(parse("not json", None).is_empty());
    }

    #[test]
    fn lookup_prefers_longest_prefix_and_later_sources() {
        let store = Store {
            items: vec![
                UserSnippet {
                    name: "a".into(),
                    prefixes: vec!["log".into()],
                    body: "A".into(),
                    scope: vec![],
                    description: String::new(),
                },
                UserSnippet {
                    name: "b".into(),
                    prefixes: vec!["log".into()],
                    body: "B".into(),
                    scope: vec!["rust".into()],
                    description: String::new(),
                },
                UserSnippet {
                    name: "c".into(),
                    prefixes: vec!["@@".into(), "x.log".into()],
                    body: "C".into(),
                    scope: vec![],
                    description: String::new(),
                },
            ],
        };
        assert_eq!(store.lookup(Some("rust"), "  log").unwrap().1.body, "B", "the later source wins");
        assert_eq!(store.lookup(Some("go"), "  log").unwrap().1.body, "A");
        let (n, s) = store.lookup(None, "foo x.log").unwrap();
        assert_eq!((n, s.body.as_str()), (5, "C"), "the longest prefix wins");
        assert_eq!(store.lookup(None, "a @@").unwrap().0, 2);
        assert!(store.lookup(None, "loge").is_none());
        let c = store.candidates(Some("rust"));
        assert_eq!(c.iter().filter(|(p, _)| p == "log").count(), 1);
    }

    #[test]
    fn loads_user_and_project_files_with_project_winning() {
        let root = tmp("root");
        std::fs::create_dir_all(root.join(".vscode")).unwrap();
        std::fs::create_dir_all(root.join(".lumen").join("snippets")).unwrap();
        std::fs::write(root.join(".vscode").join("p.code-snippets"), r#"{"x":{"prefix":"hi","body":"vscode"}}"#)
            .unwrap();
        std::fs::write(root.join(".vscode").join("ignored.json"), r#"{"x":{"prefix":"hi","body":"no"}}"#).unwrap();
        std::fs::write(
            root.join(".lumen").join("snippets").join("rust.json"),
            r#"{"x":{"prefix":"hi","body":"lumen"}}"#,
        )
        .unwrap();
        let s = Store::load(&root);
        assert_eq!(s.lookup(Some("rust"), "hi").unwrap().1.body, "lumen");
        assert_eq!(s.lookup(Some("go"), "hi").unwrap().1.body, "vscode");
        let c = current(&root);
        assert!(c.lookup(Some("go"), "hi").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_copies_without_overwriting() {
        let from = tmp("from");
        let to = tmp("to");
        std::fs::write(from.join("rust.json"), "{}").unwrap();
        std::fs::write(from.join("a.code-snippets"), "{}").unwrap();
        std::fs::write(from.join("package.json"), "{}").unwrap();
        std::fs::write(from.join("notes.txt"), "x").unwrap();
        std::fs::write(to.join("rust.json"), "{\"mine\": 1}").unwrap();
        let w = copy_snippets(&from, &to).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(std::fs::read_to_string(to.join("rust.json")).unwrap(), "{\"mine\": 1}", "own file untouched");
        assert!(to.join("rust-vscode.json").exists() && to.join("a.code-snippets").exists());
        assert!(!to.join("package.json").exists() && !to.join("notes.txt").exists());
        // Running it again changes nothing.
        assert!(copy_snippets(&from, &to).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&from);
        let _ = std::fs::remove_dir_all(&to);
    }

    #[test]
    fn template_is_valid_jsonc() {
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(TEMPLATE)).unwrap();
        assert!(v.as_object().unwrap().is_empty());
    }

    #[test]
    fn block_comments_by_language() {
        assert_eq!(block_comment(Some("rust")), ("/*", "*/"));
        assert_eq!(block_comment(Some("html")), ("<!--", "-->"));
        assert_eq!(block_comment(None), ("", ""));
    }
}
