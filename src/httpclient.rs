//! Small blocking HTTP client for the REST client (`.http` / `.rest` files).
//!
//! `http://` is spoken natively over `std::net` (HTTP/1.1, `Connection: close`, `Content-Length`
//! and chunked bodies). `https://` goes through the system `curl` (present on Linux, macOS and
//! Windows 10+), so the editor links no TLS stack and its binary does not grow. Both paths poll a
//! cancel flag every ~150 ms, so a request can be abandoned at any time.

use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Responses above this size are refused (the panel could not show them anyway).
const MAX_BODY: usize = 64 << 20;

#[derive(Clone, Debug, Default)]
pub struct Resp {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Wall time from connecting to the last byte.
    pub ms: u128,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A request with every `{{variable}}` already resolved.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with `/`.
    pub target: String,
    /// Value of the `Host` header (port only when it is not the default).
    pub host_header: String,
}

pub fn parse_url(u: &str) -> Result<Url, String> {
    let u = u.trim();
    let (https, rest) = match u.split_once("://") {
        Some((s, r)) if s.eq_ignore_ascii_case("https") => (true, r),
        Some((s, r)) if s.eq_ignore_ascii_case("http") => (false, r),
        Some((s, _)) => return Err(format!("unsupported scheme `{s}` (http and https only)")),
        None => (false, u),
    };
    let rest = rest.split('#').next().unwrap_or("");
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(cut);
    let auth = auth.rsplit('@').next().unwrap_or(auth); // drop `user:pw@`
    let (host, port) = if let Some(r) = auth.strip_prefix('[') {
        let (h, p) = r.split_once(']').ok_or("bad IPv6 address")?;
        (h.to_string(), p.strip_prefix(':').filter(|s| !s.is_empty()))
    } else {
        match auth.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p).filter(|s| !s.is_empty())),
            None => (auth.to_string(), None),
        }
    };
    if host.is_empty() {
        return Err("the URL has no host".into());
    }
    let default = if https { 443 } else { 80 };
    let port = match port {
        Some(p) => p.parse::<u16>().map_err(|_| format!("bad port `{p}`"))?,
        None => default,
    };
    let target = if tail.starts_with('/') { tail.to_string() } else { format!("/{tail}") };
    let shown = if host.contains(':') { format!("[{host}]") } else { host.clone() };
    let host_header = if port == default { shown } else { format!("{shown}:{port}") };
    Ok(Url { https, host, port, target, host_header })
}

struct Head {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    body_at: usize,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_head(buf: &[u8]) -> Option<Result<Head, String>> {
    let (end, sep) = match (find(buf, b"\r\n\r\n"), find(buf, b"\n\n")) {
        (Some(a), Some(b)) if b < a => (b, 2),
        (Some(a), _) => (a, 4),
        (None, Some(b)) => (b, 2),
        (None, None) => return None,
    };
    let text = String::from_utf8_lossy(&buf[..end]);
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut p = first.splitn(3, ' ');
    let (ver, code, reason) = (p.next().unwrap_or(""), p.next().unwrap_or(""), p.next().unwrap_or(""));
    if !ver.starts_with("HTTP/") {
        return Some(Err("the server did not answer with HTTP".into()));
    }
    let Ok(status) = code.trim().parse::<u16>() else {
        return Some(Err(format!("bad status line `{first}`")));
    };
    let headers =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    Some(Ok(Head { status, reason: reason.trim().to_string(), headers, body_at: end + sep }))
}

fn no_body(method: &str, status: u16) -> bool {
    method.eq_ignore_ascii_case("HEAD") || status / 100 == 1 || status == 204 || status == 304
}

/// Decode a chunked body; `Ok(None)` while the last chunk has not arrived.
fn decode_chunked(b: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let (mut pos, mut out) = (0, Vec::new());
    loop {
        let Some(nl) = find(&b[pos..], b"\r\n") else { return Ok(None) };
        let line = String::from_utf8_lossy(&b[pos..pos + nl]);
        let size_s = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_s, 16).map_err(|_| format!("bad chunk size `{size_s}`"))?;
        pos += nl + 2;
        if size == 0 {
            return Ok(Some(out));
        }
        if b.len() < pos + size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&b[pos..pos + size]);
        pos += size + 2;
    }
}

/// Parse what has been read so far. `None` means "more bytes needed". With `framed` the body is
/// delimited by `Content-Length` / chunking (native sockets); curl hands over the decoded body, so
/// there it is simply everything after the headers.
pub fn try_parse(buf: &[u8], method: &str, eof: bool, framed: bool) -> Option<Result<Resp, String>> {
    let mut at = 0;
    let head = loop {
        let h = match parse_head(&buf[at..]) {
            Some(Ok(h)) => h,
            Some(Err(e)) => return Some(Err(e)),
            None if eof => return Some(Err("the connection closed before a response arrived".into())),
            None => return None,
        };
        // Interim `100 Continue` style responses (and curl's proxy lines) are skipped.
        if h.status / 100 == 1 && h.status != 101 {
            at += h.body_at;
            continue;
        }
        break h;
    };
    let body = &buf[at + head.body_at..];
    let mut r = Resp { status: head.status, reason: head.reason, headers: head.headers, body: Vec::new(), ms: 0 };
    if no_body(method, r.status) {
        return Some(Ok(r));
    }
    if !framed {
        r.body = body.to_vec();
        return Some(Ok(r));
    }
    let chunked = r.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        if !eof && !body.ends_with(b"\r\n") {
            return None;
        }
        return match decode_chunked(body) {
            Ok(Some(b)) => {
                r.body = b;
                Some(Ok(r))
            }
            Ok(None) if eof => {
                r.body = body.to_vec();
                Some(Ok(r))
            }
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        };
    }
    if let Some(n) = r.header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if body.len() >= n || eof {
            r.body = body[..n.min(body.len())].to_vec();
            return Some(Ok(r));
        }
        return None;
    }
    if eof {
        r.body = body.to_vec();
        return Some(Ok(r));
    }
    None
}

fn request_bytes(p: &Prepared, url: &Url) -> Vec<u8> {
    let has = |n: &str| p.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
    let mut s = format!("{} {} HTTP/1.1\r\n", p.method, url.target);
    if !has("host") {
        s.push_str(&format!("Host: {}\r\n", url.host_header));
    }
    for (k, v) in &p.headers {
        let k = k.trim();
        if k.is_empty() || k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("connection") {
            continue;
        }
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has("user-agent") {
        s.push_str("User-Agent: lumen-text\r\n");
    }
    if !has("accept") {
        s.push_str("Accept: */*\r\n");
    }
    if !has("accept-encoding") {
        s.push_str("Accept-Encoding: identity\r\n");
    }
    s.push_str("Connection: close\r\n");
    if !p.body.is_empty() || matches!(p.method.as_str(), "POST" | "PUT" | "PATCH") {
        s.push_str(&format!("Content-Length: {}\r\n", p.body.len()));
    }
    s.push_str("\r\n");
    let mut out = s.into_bytes();
    out.extend_from_slice(&p.body);
    out
}

fn send_native(p: &Prepared, url: &Url, cancel: &AtomicBool, deadline: Instant) -> Result<Resp, String> {
    let addrs =
        (url.host.as_str(), url.port).to_socket_addrs().map_err(|e| format!("cannot resolve {}: {e}", url.host))?;
    let mut last = String::from("no address");
    let mut stream = None;
    for a in addrs {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        match TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let mut s = stream.ok_or_else(|| format!("cannot connect to {}:{}: {last}", url.host, url.port))?;
    let _ = s.set_nodelay(true);
    let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
    s.write_all(&request_bytes(p, url)).map_err(|e| format!("send failed: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(150)));
    let (mut buf, mut chunk) = (Vec::new(), vec![0u8; 16 << 10]);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("timed out".into());
        }
        match s.read(&mut chunk) {
            Ok(0) => {
                return try_parse(&buf, &p.method, true, true).unwrap_or_else(|| Err("empty response".into()));
            }
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_BODY {
                    return Err("the response is too large".into());
                }
                if let Some(r) = try_parse(&buf, &p.method, false, true) {
                    return r;
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                // A reset after a complete response still counts as success.
                return try_parse(&buf, &p.method, true, true).unwrap_or_else(|| Err(format!("read failed: {e}")));
            }
        }
    }
}

fn send_curl(p: &Prepared, cancel: &AtomicBool, deadline: Instant) -> Result<Resp, String> {
    let left = deadline.saturating_duration_since(Instant::now()).as_secs().max(1);
    let mut c = Command::new("curl");
    c.args(["-sS", "-i", "--connect-timeout", "15", "--max-time", &left.to_string()]);
    if p.method.eq_ignore_ascii_case("HEAD") {
        c.arg("-I");
    } else {
        c.args(["-X", &p.method]);
    }
    for (k, v) in &p.headers {
        let k = k.trim();
        if !k.is_empty() && !k.eq_ignore_ascii_case("content-length") {
            c.args(["-H", &format!("{k}: {v}")]);
        }
    }
    if !p.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        c.args(["-A", "lumen-text"]);
    }
    if !p.body.is_empty() {
        c.args(["--data-binary", "@-"]);
    }
    c.args(["--url", &p.url]);
    c.stdin(if p.body.is_empty() { Stdio::null() } else { Stdio::piped() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW
    let mut child = c.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "https:// needs `curl` on the PATH (the editor has no built-in TLS)".to_string()
        } else {
            format!("cannot run curl: {e}")
        }
    })?;
    if let Some(mut si) = child.stdin.take() {
        let body = p.body.clone();
        std::thread::spawn(move || {
            let _ = si.write_all(&body);
        });
    }
    let mut so = child.stdout.take().ok_or("no curl stdout")?;
    let mut se = child.stderr.take().ok_or("no curl stderr")?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut b = vec![0u8; 16 << 10];
        while let Ok(n) = so.read(&mut b) {
            if n == 0 || tx.send(b[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let mut buf = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => {
                buf.extend_from_slice(&c);
                if buf.len() > MAX_BODY {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("the response is too large".into());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::SeqCst) || Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(if cancel.load(Ordering::SeqCst) { "cancelled" } else { "timed out" }.into());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().ok();
    let stderr = err_t.join().unwrap_or_default();
    match try_parse(&buf, &p.method, true, false) {
        Some(Ok(r)) => Ok(r),
        _ => {
            let why = stderr.trim();
            Err(if why.is_empty() {
                format!("curl failed ({})", status.map_or("?".to_string(), |s| s.to_string()))
            } else {
                why.to_string()
            })
        }
    }
}

/// Send `p` and wait for the answer (call it on a worker thread). Setting `cancel` abandons it.
pub fn send(p: &Prepared, cancel: &AtomicBool, timeout: Duration) -> Result<Resp, String> {
    let t0 = Instant::now();
    let url = parse_url(&p.url)?;
    let deadline = t0 + timeout;
    let mut r = if url.https { send_curl(p, cancel, deadline)? } else { send_native(p, &url, cancel, deadline)? };
    r.ms = t0.elapsed().as_millis();
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;

    /// One-shot mock server: reads the request, answers `reply`, returns the request bytes.
    fn serve(reply: Vec<u8>) -> (u16, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            let mut got = Vec::new();
            let mut b = [0u8; 4096];
            while let Ok(n) = s.read(&mut b) {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&b[..n]);
                if let Some(i) = find(&got, b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&got[..i]).to_lowercase();
                    let want = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                        .unwrap_or(0);
                    if got.len() >= i + 4 + want {
                        break;
                    }
                }
            }
            s.write_all(&reply).unwrap();
            String::from_utf8_lossy(&got).into_owned()
        });
        (port, h)
    }

    fn prep(method: &str, url: String, body: &str) -> Prepared {
        Prepared {
            method: method.into(),
            url,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn urls() {
        let u = parse_url("https://example.com:8443/a/b?x=1#frag").unwrap();
        assert!(u.https);
        assert_eq!((u.host.as_str(), u.port, u.target.as_str()), ("example.com", 8443, "/a/b?x=1"));
        assert_eq!(u.host_header, "example.com:8443");
        let u = parse_url("localhost:3000?q=1").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.target.as_str()), ("localhost", 3000, "/?q=1"));
        assert_eq!(parse_url("http://[::1]:80/").unwrap().host_header, "[::1]");
        assert_eq!(parse_url("http://u:p@h/x").unwrap().host, "h");
        assert!(parse_url("ftp://x").is_err());
        assert!(parse_url("http://").is_err());
    }

    #[test]
    fn content_length_and_request_shape() {
        let (port, h) = serve(b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\nX-A: b\r\n\r\nhello".to_vec());
        let r = send(
            &prep("POST", format!("http://127.0.0.1:{port}/p?q=1"), "{\"a\":1}"),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!((r.status, r.reason.as_str()), (201, "Created"));
        assert_eq!(r.text(), "hello");
        assert_eq!(r.header("x-a"), Some("b"));
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /p?q=1 HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Content-Type: application/json\r\n"));
        assert!(req.contains("Content-Length: 7\r\n"));
        assert!(req.ends_with("{\"a\":1}"));
    }

    #[test]
    fn chunked_body() {
        let (port, _h) = serve(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n".to_vec(),
        );
        let r = send(
            &prep("GET", format!("http://127.0.0.1:{port}/"), ""),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(r.text(), "Wikipedia");
    }

    #[test]
    fn body_until_close_and_no_body_statuses() {
        let (port, _h) = serve(b"HTTP/1.0 200 OK\r\n\r\nuntil eof".to_vec());
        let r =
            send(&prep("GET", format!("http://127.0.0.1:{port}"), ""), &AtomicBool::new(false), Duration::from_secs(5))
                .unwrap();
        assert_eq!(r.text(), "until eof");
        let r = try_parse(b"HTTP/1.1 204 No Content\r\n\r\n", "GET", false, true).unwrap().unwrap();
        assert!(r.body.is_empty() && r.status == 204);
        // 100 Continue is skipped.
        let r = try_parse(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx",
            "POST",
            false,
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!((r.status, r.text().as_str()), (200, "x"));
        // Incomplete data asks for more.
        assert!(try_parse(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc", "GET", false, true).is_none());
        assert!(try_parse(b"junk", "GET", true, true).unwrap().is_err());
    }

    #[test]
    fn cancel_and_timeout_stop_a_silent_server() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hold = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            drop(s);
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            c2.store(true, Ordering::SeqCst);
        });
        let t = Instant::now();
        let e =
            send(&prep("GET", format!("http://127.0.0.1:{port}/"), ""), &cancel, Duration::from_secs(30)).unwrap_err();
        assert_eq!(e, "cancelled");
        assert!(t.elapsed() < Duration::from_secs(1));
        hold.join().unwrap();

        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hold = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            std::thread::sleep(Duration::from_millis(800));
            drop(s);
        });
        let e = send(
            &prep("GET", format!("http://127.0.0.1:{port}/"), ""),
            &AtomicBool::new(false),
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert_eq!(e, "timed out");
        hold.join().unwrap();
    }

    #[test]
    fn refused_connection_is_an_error() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = send(
            &prep("GET", format!("http://127.0.0.1:{port}/"), ""),
            &AtomicBool::new(false),
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(e.contains("cannot connect"), "{e}");
    }
}
