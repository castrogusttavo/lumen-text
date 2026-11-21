//! `@` mentions in the chat composer: finding the one being typed at the caret, ranking the
//! project's files for it, and what picking one inserts. Plain code with no App: unit-tested below.

use crate::fuzzy;
use std::path::Path;

/// The mention under the caret: where its `@` is (byte offset) and what is typed after it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Active {
    pub start: usize,
    pub query: String,
}

/// The `@word` the caret is in the middle of or at the end of. The `@` has to start the text or
/// follow whitespace (so an e-mail address is not a mention) and the word has no whitespace yet
/// (a quoted `@"my file` may hold spaces until its closing quote). `caret` is a byte offset.
pub fn active(text: &str, caret: usize) -> Option<Active> {
    let caret = caret.min(text.len());
    if !text.is_char_boundary(caret) {
        return None;
    }
    let before = &text[..caret];
    let at = before.rfind('@')?;
    if before[..at].chars().next_back().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let typed = &before[at + 1..];
    let (quoted, q) = match typed.strip_prefix('"') {
        Some(r) => (true, r),
        None => (false, typed),
    };
    if q.contains('"') || (!quoted && q.contains(char::is_whitespace)) || q.contains('\n') {
        return None;
    }
    Some(Active { start: at, query: q.to_string() })
}

/// Up to `limit` files best matching `query`, best first (an empty query lists the first ones).
pub fn rank(files: &[String], query: &str, limit: usize) -> Vec<String> {
    if query.is_empty() {
        return files.iter().take(limit).cloned().collect();
    }
    fuzzy::rank(query, files.len(), |i| files.get(i).map(String::as_str), limit)
        .into_iter()
        .map(|(i, _)| files[i].clone())
        .collect()
}

/// The text a picked file puts in place of what was typed: the mention, quoted when the path has
/// spaces, and a space after it so typing goes on.
pub fn insertion(rel: &str) -> String {
    super::composer::mention(rel)
}

/// The project's files as paths relative to `root`, with `/` separators.
pub fn relative_files(root: &Path, files: &[std::path::PathBuf]) -> Vec<String> {
    files.iter().filter_map(|p| p.strip_prefix(root).ok()).map(|p| p.to_string_lossy().replace('\\', "/")).collect()
}

/// Is `path` an image the agent can look at (by extension)?
pub fn image_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_string_lossy().to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        ["src/main.rs", "src/app_aipanel/chat.rs", "src/app_aipanel/composer.rs", "README.md", "docs/chat notes.md"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn the_mention_at_the_caret_is_found() {
        let a = |t: &str| active(t, t.len());
        assert_eq!(a("look at @src/ma"), Some(Active { start: 8, query: "src/ma".into() }));
        assert_eq!(a("@"), Some(Active { start: 0, query: String::new() }));
        assert_eq!(a("mail me@example.com"), None, "an address is not a mention");
        assert_eq!(a("@done and more"), None, "the word ended");
        assert_eq!(a("see @\"my dir/a b"), Some(Active { start: 4, query: "my dir/a b".into() }));
        assert_eq!(a("see @\"my dir/a b\" now"), None, "a closed quote ends it");
        // The caret in the middle: only what is before it counts.
        assert_eq!(active("fix @src/main.rs please", 9), Some(Active { start: 4, query: "src/".into() }));
        assert_eq!(active("héllo @x", 2), None, "inside a character");
    }

    #[test]
    fn files_rank_by_the_query() {
        let f = files();
        assert_eq!(rank(&f, "", 2), ["src/main.rs", "src/app_aipanel/chat.rs"]);
        assert_eq!(rank(&f, "comp", 5)[0], "src/app_aipanel/composer.rs");
        assert_eq!(rank(&f, "readme", 5), ["README.md"]);
        assert!(rank(&f, "zzz", 5).is_empty());
        assert_eq!(rank(&f, "chat", 1).len(), 1);
    }

    #[test]
    fn paths_are_made_relative_and_images_recognised() {
        let root = Path::new("/p");
        let v = relative_files(root, &[root.join("a/b.rs"), Path::new("/elsewhere/c.rs").to_path_buf()]);
        assert_eq!(v, ["a/b.rs"]);
        assert_eq!(image_type(Path::new("x/Shot.PNG")), Some("image/png"));
        assert_eq!(image_type(Path::new("a.jpeg")), Some("image/jpeg"));
        assert_eq!(image_type(Path::new("a.rs")), None);
    }
}
