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

pub fn path_to_uri(p: &Path) -> String {
    let mut s = String::from("file://");
    let text = p.to_string_lossy();
    // Windows: `C:\a b\x.ts` becomes `file:///C:/a%20b/x.ts`.
    let text = if cfg!(windows) { format!("/{}", text.replace('\\', "/")) } else { text.into_owned() };
    let drive = cfg!(windows) && text.as_bytes().get(2) == Some(&b':');
    for (i, b) in text.bytes().enumerate() {
        if drive && i == 2 {
            s.push(':');
            continue;
        }
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let text = String::from_utf8(out).ok()?;
    // Windows: `/C:/a/x.ts` is `C:\a\x.ts`.
    #[cfg(windows)]
    let text = {
        let t = text.strip_prefix('/').filter(|t| t.as_bytes().get(1) == Some(&b':')).unwrap_or(&text);
        t.replace('/', "\\")
    };
    Some(from_mirror(PathBuf::from(text)))
}
