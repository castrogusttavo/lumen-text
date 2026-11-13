//! Multi-root workspaces: the file formats and the path helpers (no App state here).
//!
//! A workspace is several folders in one project window. It is described by a JSON file:
//!
//! * `*.lumen-workspace` (or just `.lumen-workspace`): `{"folders": [{"path": "api"}, {"path": "../web"}]}`
//! * VS Code's `*.code-workspace` is read too (same `folders` list; comments and trailing commas
//!   are allowed there). Only the folder list is used.
//!
//! Relative folder paths are relative to the folder holding the file. A folder entry may also be a
//! plain string. The first folder is the primary root (session, settings, `.lumen/`, terminals).

use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

/// File name of a workspace file kept in a project folder.
pub const FILE_NAME: &str = ".lumen-workspace";

/// Is `p` named like a workspace file (`.lumen-workspace`, `x.lumen-workspace`, `x.code-workspace`)?
pub fn is_workspace_file(p: &Path) -> bool {
    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    n.ends_with(".lumen-workspace") || n.ends_with(".code-workspace")
}

/// `text` without `//` and `/* */` comments and without trailing commas (what VS Code accepts in
/// `.code-workspace`). Strings are left alone.
pub fn strip_jsonc(text: &str) -> String {
    drop_trailing_commas(&strip_comments(text))
}

fn strip_comments(text: &str) -> String {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            '"' => {
                out.push(c);
                i += 1;
                while i < cs.len() {
                    out.push(cs[i]);
                    if cs[i] == '\\' && i + 1 < cs.len() {
                        out.push(cs[i + 1]);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    if cs[i - 1] == '"' {
                        break;
                    }
                }
            }
            '/' if cs.get(i + 1) == Some(&'/') => {
                while i < cs.len() && cs[i] != '\n' {
                    i += 1;
                }
            }
            '/' if cs.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < cs.len() && !(cs[i] == '*' && cs.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// A comma followed (after blanks) by a closing bracket is dropped; strings are left alone.
fn drop_trailing_commas(text: &str) -> String {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < cs.len() {
        let c = cs[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < cs.len() {
                out.push(cs[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == ',' {
            let mut j = i + 1;
            while j < cs.len() && cs[j].is_whitespace() {
                j += 1;
            }
            if !matches!(cs.get(j), Some('}') | Some(']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Lexical clean-up of `.` and `..` components (no file system access).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

/// The folders listed by a workspace file, made absolute against `base` (the folder holding the
/// file). Entries without a usable path are skipped; duplicates are dropped.
pub fn parse(text: &str, base: &Path) -> Result<Vec<PathBuf>, String> {
    let v: Value = serde_json::from_str(&strip_jsonc(text)).map_err(|e| format!("not valid JSON: {e}"))?;
    let list = v.get("folders").and_then(Value::as_array).ok_or("no \"folders\" list")?;
    let mut out: Vec<PathBuf> = Vec::new();
    for f in list {
        let path = match f {
            Value::String(s) => Some(s.as_str()),
            o => o.get("path").and_then(Value::as_str),
        };
        let Some(path) = path.filter(|p| !p.trim().is_empty()) else { continue };
        let p = Path::new(path);
        let abs = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
        let abs = normalize(&abs);
        if !out.contains(&abs) {
            out.push(abs);
        }
    }
    Ok(out)
}

/// The existing folders of the workspace file `file` (canonical, first = primary root).
pub fn load(file: &Path) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let all = parse(&text, file.parent().unwrap_or(Path::new(".")))?;
    let mut out: Vec<PathBuf> = Vec::new();
    for f in all.into_iter().filter(|f| f.is_dir()) {
        let f = crate::paths::canonical(&f).unwrap_or(f);
        if !out.contains(&f) {
            out.push(f);
        }
    }
    if out.is_empty() { Err("none of its folders exists".to_string()) } else { Ok(out) }
}

/// `to` written relative to the directory `from` (`.`, `sub/x`, `../other`); `to` itself when the
/// two share no root.
pub fn relative(from: &Path, to: &Path) -> String {
    let (f, t): (Vec<_>, Vec<_>) = (from.components().collect(), to.components().collect());
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return to.to_string_lossy().into_owned();
    }
    let mut parts: Vec<String> = f[common..].iter().map(|_| "..".to_string()).collect();
    parts.extend(t[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() { ".".to_string() } else { parts.join("/") }
}

/// The text of a `.lumen-workspace` file for `folders`, paths relative to `dir` (the file's folder).
pub fn serialize(dir: &Path, folders: &[PathBuf]) -> String {
    let list: Vec<Value> = folders.iter().map(|f| json!({"path": relative(dir, f)})).collect();
    let mut s = serde_json::to_string_pretty(&json!({"folders": list})).unwrap_or_default();
    s.push('\n');
    s
}

/// The workspace root that holds `path` (the deepest one when they are nested).
pub fn root_of<'a>(roots: &'a [PathBuf], path: &Path) -> Option<&'a PathBuf> {
    roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.components().count())
}

/// Display names of the roots: the folder name, with the parent folder added when two share it.
pub fn names(roots: &[PathBuf]) -> Vec<String> {
    let base = |r: &PathBuf| r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    roots
        .iter()
        .map(|r| {
            let n = base(r);
            if roots.iter().filter(|o| base(o) == n).count() > 1 {
                let parent = r.parent().and_then(Path::file_name).map(|p| p.to_string_lossy().into_owned());
                match parent {
                    Some(p) => format!("{n} ({p})"),
                    None => n,
                }
            } else {
                n
            }
        })
        .collect()
}

/// `path` as shown in lists: relative to its root; with several roots prefixed by the root's name
/// (`api/src/main.rs`) so the same relative path in two folders stays distinguishable.
pub fn label(roots: &[PathBuf], path: &Path) -> String {
    let Some(first) = roots.first() else { return path.to_string_lossy().into_owned() };
    if roots.len() < 2 {
        return path.strip_prefix(first).unwrap_or(path).to_string_lossy().into_owned();
    }
    match root_of(roots, path) {
        Some(r) => {
            let i = roots.iter().position(|x| x == r).unwrap_or(0);
            let name = &names(roots)[i];
            let rel = path.strip_prefix(r).unwrap_or(path).to_string_lossy().into_owned();
            if rel.is_empty() { name.clone() } else { format!("{name}/{rel}") }
        }
        None => path.to_string_lossy().into_owned(),
    }
}

// ── language servers ──

/// The roots of the open workspace (primary first); empty while the project has a single root, so
/// single-root sessions send exactly the one folder they always did.
static LSP_ROOTS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Publish the roots language servers should be told about (`roots` is the whole set; a set of
/// one clears it).
pub fn set_lsp_roots(roots: &[PathBuf]) {
    if let Ok(mut g) = LSP_ROOTS.lock() {
        *g = if roots.len() > 1 { roots.to_vec() } else { Vec::new() };
    }
}

/// `workspaceFolders` of a server started in `server_root`: that folder first, then the workspace
/// roots that are not inside it and do not contain it.
pub fn lsp_folders_in(server_root: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut v = vec![server_root.to_path_buf()];
    for r in roots {
        if !r.starts_with(server_root) && !server_root.starts_with(r) && !v.contains(r) {
            v.push(r.clone());
        }
    }
    v
}

/// [`lsp_folders_in`] with the published roots.
pub fn lsp_folders(server_root: &Path) -> Vec<PathBuf> {
    let roots = LSP_ROOTS.lock().map(|g| g.clone()).unwrap_or_default();
    lsp_folders_in(server_root, &roots)
}

/// LSP `WorkspaceFolder[]` for `folders`.
pub fn folders_json(folders: &[PathBuf]) -> Value {
    Value::Array(
        folders
            .iter()
            .map(|f| {
                json!({
                    "uri": crate::doc::path_to_uri(f),
                    "name": f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                })
            })
            .collect(),
    )
}

/// Params of `workspace/didChangeWorkspaceFolders` going from `old` to `new`; None when equal.
pub fn folders_change(old: &[PathBuf], new: &[PathBuf]) -> Option<Value> {
    let added: Vec<PathBuf> = new.iter().filter(|f| !old.contains(f)).cloned().collect();
    let removed: Vec<PathBuf> = old.iter().filter(|f| !new.contains(f)).cloned().collect();
    if added.is_empty() && removed.is_empty() {
        return None;
    }
    Some(json!({"event": {"added": folders_json(&added), "removed": folders_json(&removed)}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn recognises_workspace_files() {
        assert!(is_workspace_file(Path::new("/a/.lumen-workspace")));
        assert!(is_workspace_file(Path::new("/a/my.lumen-workspace")));
        assert!(is_workspace_file(Path::new("/a/x.code-workspace")));
        assert!(!is_workspace_file(Path::new("/a/workspace.json")));
    }

    #[test]
    fn parses_lumen_workspace_with_relative_and_absolute_paths() {
        let t = r#"{"folders": [{"path": "api"}, {"path": "../web"}, "/opt/lib", {"name": "x"}, {"path": "api"}]}"#;
        let f = parse(t, Path::new("/home/u/proj")).unwrap();
        assert_eq!(f, vec![p("/home/u/proj/api"), p("/home/u/web"), p("/opt/lib")]);
    }

    #[test]
    fn parses_code_workspace_with_comments_and_trailing_commas() {
        let t = r#"{
            // two folders
            "folders": [
                { "path": ".", "name": "root" }, /* block */
                { "path": "packages/ui", },
            ],
            "settings": { "url": "http://x//y", },
        }"#;
        let f = parse(t, Path::new("/w")).unwrap();
        assert_eq!(f, vec![p("/w"), p("/w/packages/ui")]);
    }

    #[test]
    fn parse_errors_are_reported() {
        assert!(parse("{", Path::new("/w")).is_err());
        assert!(parse("{\"a\": 1}", Path::new("/w")).unwrap_err().contains("folders"));
    }

    #[test]
    fn strip_jsonc_keeps_strings_intact() {
        let s = strip_jsonc(r#"{"a": "// not a comment, ]", "b": [1,2,]}"#);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"], "// not a comment, ]");
        assert_eq!(v["b"], json!([1, 2]));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b")), ".");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b/c/d")), "c/d");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/x")), "../x");
        assert_eq!(relative(Path::new("/a/b/c"), Path::new("/a/x/y")), "../../x/y");
    }

    #[test]
    fn serialize_round_trips() {
        let dir = Path::new("/home/u/proj");
        let folders = vec![p("/home/u/proj"), p("/home/u/proj/api"), p("/home/u/web")];
        let text = serialize(dir, &folders);
        assert!(text.contains("\"../web\""), "{text}");
        assert_eq!(parse(&text, dir).unwrap(), folders);
    }

    #[test]
    fn root_of_picks_the_deepest_root() {
        let roots = vec![p("/w"), p("/w/inner"), p("/other")];
        assert_eq!(root_of(&roots, Path::new("/w/inner/a.rs")), Some(&roots[1]));
        assert_eq!(root_of(&roots, Path::new("/w/b.rs")), Some(&roots[0]));
        assert_eq!(root_of(&roots, Path::new("/elsewhere/c.rs")), None);
    }

    #[test]
    fn names_disambiguate_same_folder_names() {
        let roots = vec![p("/a/app"), p("/b/app"), p("/c/lib")];
        assert_eq!(names(&roots), ["app (a)", "app (b)", "lib"]);
    }

    #[test]
    fn labels_prefix_the_root_only_with_several_roots() {
        let one = vec![p("/w")];
        assert_eq!(label(&one, Path::new("/w/src/a.rs")), "src/a.rs");
        let two = vec![p("/w"), p("/x/api")];
        assert_eq!(label(&two, Path::new("/w/src/a.rs")), "w/src/a.rs");
        assert_eq!(label(&two, Path::new("/x/api/m.rs")), "api/m.rs");
        assert_eq!(label(&two, Path::new("/x/api")), "api");
        assert_eq!(label(&two, Path::new("/z/q.rs")), "/z/q.rs");
    }

    #[test]
    fn lsp_folders_skip_nested_roots() {
        let roots = vec![p("/w"), p("/x/api"), p("/w/pkg")];
        // A server in the primary root already covers the nested one.
        assert_eq!(lsp_folders_in(Path::new("/w"), &roots), vec![p("/w"), p("/x/api")]);
        // A server in a sub-project of the primary root: the primary root contains it, so skipped.
        assert_eq!(lsp_folders_in(Path::new("/w/pkg"), &roots), vec![p("/w/pkg"), p("/x/api")]);
        assert_eq!(lsp_folders_in(Path::new("/w"), &roots[..1]), vec![p("/w")]);
    }

    #[test]
    fn folder_changes_list_added_and_removed() {
        let (a, b) = (vec![p("/w")], vec![p("/w"), p("/x")]);
        assert_eq!(folders_change(&a, &a), None);
        let c = folders_change(&a, &b).unwrap();
        assert_eq!(c["event"]["added"].as_array().unwrap().len(), 1);
        assert_eq!(c["event"]["removed"].as_array().unwrap().len(), 0);
        let c = folders_change(&b, &a).unwrap();
        assert_eq!(c["event"]["removed"][0]["name"], "x");
    }
}
