use super::*;

/// (mirror dir, real project root) pairs: documents of a project served from a narrow mirror
/// are addressed to the language server under the mirror path and mapped back on the way in.
pub(super) static MIRRORS: std::sync::Mutex<Vec<(PathBuf, PathBuf)>> = std::sync::Mutex::new(Vec::new());

pub fn register_mirror(mirror: PathBuf, root: PathBuf) {
    let mut m = MIRRORS.lock().unwrap();
    m.retain(|(_, r)| *r != root);
    m.push((mirror, root));
}

pub fn unregister_mirror(root: &Path) {
    MIRRORS.lock().unwrap().retain(|(_, r)| r != root);
}

pub fn to_mirror(p: &Path) -> PathBuf {
    for (m, r) in MIRRORS.lock().unwrap().iter() {
        if let Ok(rel) = p.strip_prefix(r) {
            return m.join(rel);
        }
    }
    p.to_path_buf()
}

pub fn from_mirror(p: PathBuf) -> PathBuf {
    for (m, r) in MIRRORS.lock().unwrap().iter() {
        if let Ok(rel) = p.strip_prefix(m) {
            return r.join(rel);
        }
    }
    p
}

/// `file://` URI of an absolute path: `/a b/x.ts` -> `file:///a%20b/x.ts`; on Windows
/// `C:\a b\x.ts` -> `file:///C:/a%20b/x.ts` and `\\srv\share\x` -> `file://srv/share/x`. A
/// verbatim `\\?\` prefix (from `canonicalize`) is dropped first: servers don't understand it.
pub fn path_to_uri(p: &Path) -> String {
    let p = crate::paths::simplify(p.to_path_buf());
    let text = p.to_string_lossy();
    if cfg!(windows) { windows_uri(&text) } else { format!("file://{}", encode_uri_path(&text)) }
}

/// Percent-encode everything but unreserved characters and `/`.
fn encode_uri_path(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

/// The URI of a Windows path (its text: `C:\x`, `\\srv\share\x`).
fn windows_uri(text: &str) -> String {
    let t = text.replace('\\', "/");
    if let Some(unc) = t.strip_prefix("//") {
        let (host, rest) = unc.split_once('/').unwrap_or((unc, ""));
        return format!("file://{host}{}", encode_uri_path(&format!("/{rest}")));
    }
    let b = t.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return format!("file:///{}:{}", &t[..1], encode_uri_path(&t[2..]));
    }
    let t = if t.starts_with('/') { t } else { format!("/{t}") };
    format!("file://{}", encode_uri_path(&t))
}

/// `%XX` escapes decoded (a truncated or invalid one stays literal); `None` when not UTF-8.
fn decode_uri(rest: &str) -> Option<String> {
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// The Windows path of a `file://` URI's part after the scheme: `/C:/a/x.ts` (or `/c%3A/a/x.ts`,
/// as VS Code-style servers send it) is `C:\a\x.ts`; `srv/share/x` is the UNC path `\\srv\share\x`.
fn windows_path(rest: &str) -> Option<String> {
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let path = decode_uri(path)?;
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return Some(format!(r"\\{}{}", decode_uri(host)?, path.replace('/', "\\")));
    }
    let b = path.as_bytes();
    let path = if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        &path[1..]
    } else {
        path.as_str()
    };
    Some(path.replace('/', "\\"))
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let text = if cfg!(windows) { windows_path(rest)? } else { decode_uri(rest)? };
    Some(from_mirror(crate::paths::simplify(PathBuf::from(text))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_and_uris_convert_both_ways() {
        for (path, uri) in [
            (r"C:\home\a b\ç\x.ts", "file:///C:/home/a%20b/%C3%A7/x.ts"),
            (r"d:\p\#1\a+b.c", "file:///d:/p/%231/a%2Bb.c"),
            (r"\\srv\share\a b.rs", "file://srv/share/a%20b.rs"),
        ] {
            assert_eq!(windows_uri(path), uri);
            assert_eq!(windows_path(uri.strip_prefix("file://").unwrap()).as_deref(), Some(path));
        }
        // What servers send back: an encoded drive colon, `localhost`, lower-case drive letters.
        assert_eq!(windows_path("/c%3A/Users/x.ts").as_deref(), Some(r"c:\Users\x.ts"));
        assert_eq!(windows_path("localhost/C:/x.ts").as_deref(), Some(r"C:\x.ts"));
        assert_eq!(windows_path("/C:").as_deref(), Some("C:"));
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_paths_get_plain_uris() {
        let uri = path_to_uri(Path::new(r"\\?\C:\Users\me\proj\main.c"));
        assert_eq!(uri, "file:///C:/Users/me/proj/main.c");
        assert_eq!(uri_to_path(&uri), Some(PathBuf::from(r"C:\Users\me\proj\main.c")));
        // A canonical project path (what the editor opens files as) round-trips unchanged.
        let tmp = crate::paths::canonical(&std::env::temp_dir()).unwrap();
        assert_eq!(uri_to_path(&path_to_uri(&tmp)), Some(tmp.clone()));
        assert!(path_to_uri(&tmp).starts_with("file:///"), "{tmp:?}");
    }
}
