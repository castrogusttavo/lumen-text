//! REST client for `.http` / `.rest` files: a "Send Request" lens on every request line, a
//! shortcut, and the response in the bottom Output panel (status, time, size, headers, the body
//! pretty-printed and colored). Parsing and variables live in `httpfile`, the wire in `httpclient`.
//!
//! The request is sent from a worker thread; `pump_http` (called with the other background
//! pumps) collects the answer. Named requests (`# @name x`) keep their response in memory so
//! later requests can chain on it: `{{x.response.body.$.token}}`.

use crate::{
    app::App,
    app_panel::PanelTab,
    doc::Lens,
    httpclient::{self, Resp},
    httpfile::{self, Ctx},
    tasks::{PALETTE, Segs},
    theme::Rgba,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

/// Key in a lens' `raw` JSON that marks it as ours (its value is the request line).
pub const LENS_KEY: &str = "lumen_http";
const TIMEOUT: Duration = Duration::from_secs(120);
/// Body lines shown in the panel; the rest is one click away in the editor.
const MAX_SHOWN: usize = 4000;

pub struct Inflight {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Result<Resp, String>>,
    doc: Option<PathBuf>,
    line: usize,
    name: Option<String>,
    label: String,
    dir: PathBuf,
}

pub struct Last {
    resp: Resp,
    label: String,
    name: Option<String>,
    dir: PathBuf,
}

#[derive(Default)]
pub struct HttpState {
    /// Selected environment of `http-client.env.json`.
    pub env: Option<String>,
    /// Responses of named requests.
    pub responses: HashMap<String, Resp>,
    pub inflight: Option<Inflight>,
    pub last: Option<Last>,
}

fn status_color(code: u16) -> Rgba {
    match code / 100 {
        2 => PALETTE[2],
        3 => PALETTE[4],
        4 => PALETTE[3],
        _ => PALETTE[1],
    }
}

/// One JSON line split into colored pieces: keys, strings, numbers, literals, punctuation.
fn json_segs(line: &str, fg: Rgba, dim: Rgba) -> Segs {
    let ch: Vec<char> = line.chars().collect();
    let (mut out, mut i): (Segs, usize) = (Vec::new(), 0);
    while i < ch.len() {
        let st = i;
        let color = match ch[i] {
            '"' => {
                i += 1;
                while i < ch.len() && ch[i] != '"' {
                    i += if ch[i] == '\\' { 2 } else { 1 };
                }
                i = (i + 1).min(ch.len());
                let key = ch[i..].iter().find(|c| !c.is_whitespace()) == Some(&':');
                if key { PALETTE[6] } else { PALETTE[2] }
            }
            c if c.is_whitespace() => {
                while i < ch.len() && ch[i].is_whitespace() {
                    i += 1;
                }
                fg
            }
            c if c.is_ascii_digit() || c == '-' => {
                while i < ch.len() && (ch[i].is_ascii_alphanumeric() || matches!(ch[i], '.' | '-' | '+')) {
                    i += 1;
                }
                PALETTE[3]
            }
            c if c.is_ascii_alphabetic() => {
                while i < ch.len() && ch[i].is_ascii_alphabetic() {
                    i += 1;
                }
                PALETTE[5]
            }
            _ => {
                i += 1;
                dim
            }
        };
        out.push((ch[st..i].iter().collect(), color));
    }
    out
}

/// The body as shown: pretty JSON when it is JSON, the text otherwise.
fn body_text(r: &Resp) -> String {
    if r.body.contains(&0) {
        return format!("(binary, {})", httpfile::human_size(r.body.len()));
    }
    let t = r.text();
    let is_json = r.header("content-type").is_none_or(|c| c.to_ascii_lowercase().contains("json"));
    if is_json && let Some(p) = httpfile::pretty_json(&t) {
        return p;
    }
    t
}

/// Everything the panel shows for a response.
pub fn response_lines(label: &str, r: &Resp, fg: Rgba, dim: Rgba) -> Vec<Segs> {
    let mut v: Vec<Segs> = Vec::new();
    let reason = if r.reason.is_empty() { String::new() } else { format!(" {}", r.reason) };
    v.push(vec![
        (format!("HTTP {}{reason}", r.status), status_color(r.status)),
        (format!("   {} ms   {}", r.ms, httpfile::human_size(r.body.len())), dim),
    ]);
    v.push(vec![(label.to_string(), dim)]);
    v.push(Vec::new());
    for (k, val) in &r.headers {
        v.push(vec![(format!("{k}: "), PALETTE[6]), (val.clone(), fg)]);
    }
    if !r.body.is_empty() {
        v.push(Vec::new());
        let text = body_text(r);
        let json = text.starts_with(['{', '[']);
        let n = text.lines().count();
        for l in text.lines().take(MAX_SHOWN) {
            v.push(if json { json_segs(l, fg, dim) } else { vec![(l.to_string(), fg)] });
        }
        if n > MAX_SHOWN {
            v.push(vec![(format!("… {} more lines (HTTP: Open response in editor)", n - MAX_SHOWN), dim)]);
        }
    }
    v
}

impl App {
    fn http_dir(&self) -> PathBuf {
        self.docs
            .get(self.active)
            .and_then(|d| d.path())
            .and_then(Path::parent)
            .map_or_else(|| self.root.clone(), Path::to_path_buf)
    }

    /// Show `lines` in the Output panel (unless a task is running there).
    fn http_panel(&mut self, lines: Vec<Segs>, exit: Option<i32>) -> bool {
        if self.panel.running.is_some() {
            return false;
        }
        let p = &mut self.panel;
        p.plain = lines.iter().map(|l| l.iter().map(|(s, _)| s.as_str()).collect()).collect();
        p.lines = lines;
        p.tab = PanelTab::Output;
        p.open = true;
        p.title = "HTTP".into();
        p.scroll = 0.0;
        p.follow = false;
        p.exit = exit;
        p.cmd = None;
        true
    }

    /// `HTTP: Send request at cursor`.
    pub fn http_send(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        if d.lang != Some("http") {
            return self.say("HTTP: open a .http or .rest file to send requests");
        }
        let line = d.buf.line_col(d.ed.head()).0;
        self.http_send_line(line);
    }

    pub fn http_send_line(&mut self, line: usize) {
        if self.http.inflight.is_some() {
            return self.say("An HTTP request is already running (HTTP: Cancel request)");
        }
        let Some(d) = self.docs.get(self.active) else { return };
        let (text, doc_path) = (d.buf.to_string(), d.path().map(Path::to_path_buf));
        let file = httpfile::parse(&text);
        let Some(ri) = httpfile::req_at(&file, line) else {
            return self.say("HTTP: no request here");
        };
        let req = &file.reqs[ri];
        let dir = self.http_dir();
        let envs = httpfile::load_envs(&dir, &self.root);
        let env = httpfile::env_vars(&envs, self.http.env.as_deref());
        let prepared = {
            let ctx = Ctx { vars: &file.vars, env: &env, responses: &self.http.responses, dir: &dir };
            httpfile::prepare(req, &ctx)
        };
        let p = match prepared {
            Ok(p) => p,
            Err(e) => {
                let hint =
                    if self.http.env.is_none() && !httpfile::env_names(&envs).is_empty() && e.contains("undefined") {
                        " (choose an environment with “HTTP: Next environment”)"
                    } else {
                        ""
                    };
                return self.say_for(format!("HTTP: {e}{hint}"), 4);
            }
        };
        let label = format!("{} {}", p.method, p.url);
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (wake, c2) = (self.wake(), cancel.clone());
        std::thread::spawn(move || {
            let _ = tx.send(httpclient::send(&p, &c2, TIMEOUT));
            wake();
        });
        let sending = vec![vec![(format!("Sending {label} …"), self.theme.chrome.tab_inactive_fg)]];
        self.http_panel(sending, None);
        self.http.inflight =
            Some(Inflight { cancel, rx, doc: doc_path, line: req.line, name: req.name.clone(), label, dir });
        self.http_sync_lenses();
    }

    /// `HTTP: Cancel request`.
    pub fn http_cancel(&mut self) {
        let Some(i) = self.http.inflight.take() else {
            return self.say("No HTTP request is running");
        };
        i.cancel.store(true, Ordering::SeqCst);
        let dim = self.theme.chrome.tab_inactive_fg;
        self.http_panel(vec![vec![(format!("■ cancelled {}", i.label), dim)]], Some(1));
        self.http_sync_lenses();
        self.say("HTTP request cancelled");
    }

    /// A click on a lens: send, or cancel when that very request is running.
    pub fn http_lens_click(&mut self, line: usize) {
        match self.http.inflight.as_ref().map(|i| i.line) {
            Some(l) if l == line => self.http_cancel(),
            Some(_) => self.say("An HTTP request is already running (click its “Cancel request”)"),
            None => self.http_send_line(line),
        }
    }

    /// Collect a finished request and refresh the lenses of the open `.http` documents.
    pub fn pump_http(&mut self) {
        self.http_sync_lenses();
        let Some(inf) = &self.http.inflight else { return };
        let got = match inf.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the request thread ended".to_string()),
        };
        let Some(inf) = self.http.inflight.take() else { return };
        let (fg, dim) = (self.theme.foreground, self.theme.chrome.tab_inactive_fg);
        match got {
            Ok(resp) => {
                let lines = response_lines(&inf.label, &resp, fg, dim);
                self.http_panel(lines, Some(i32::from(resp.status >= 400)));
                self.say(format!("HTTP {} in {} ms", resp.status, resp.ms));
                if let Some(n) = &inf.name {
                    self.http.responses.insert(n.clone(), resp.clone());
                }
                self.http.last = Some(Last { resp, label: inf.label, name: inf.name, dir: inf.dir });
            }
            Err(e) => {
                let lines = vec![vec![(format!("✗ {}", inf.label), PALETTE[1])], vec![(e.clone(), fg)]];
                self.http_panel(lines, Some(1));
                self.say_for(format!("HTTP: {e}"), 3);
            }
        }
        self.http_sync_lenses();
        self.request_redraw();
    }

    /// Keep one "Send Request" lens per request of every open `.http` document.
    pub fn http_sync_lenses(&mut self) {
        let running = self.http.inflight.as_ref().map(|i| (i.doc.clone(), i.line));
        for d in self.docs.iter_mut().filter(|d| d.lang == Some("http")) {
            let here = running.as_ref().filter(|(p, _)| p.as_deref() == d.path()).map(|(_, l)| *l);
            let ver = d.buf.version();
            if let Some((v, r, has)) = d.http_sig
                && v == ver
                && r == here
                && (!has || !d.lenses.is_empty())
            {
                continue;
            }
            let file = httpfile::parse(&d.buf.to_string());
            d.lenses = file
                .reqs
                .iter()
                .map(|q| {
                    let title = if here == Some(q.line) { "■ Cancel request" } else { "▶ Send Request" };
                    let mut raw = serde_json::Map::new();
                    raw.insert(LENS_KEY.to_string(), q.line.into());
                    Lens { line: q.line, col: 0, title: Some(title.to_string()), raw: serde_json::Value::Object(raw) }
                })
                .collect();
            d.http_sig = Some((ver, here, !file.reqs.is_empty()));
        }
    }

    /// `HTTP: Next environment`: cycle through the environments of `http-client.env.json`.
    pub fn http_next_env(&mut self) {
        let envs = httpfile::load_envs(&self.http_dir(), &self.root);
        let names = httpfile::env_names(&envs);
        if names.is_empty() {
            return self.say("HTTP: no http-client.env.json next to the file or in the project root");
        }
        let next = match self.http.env.as_ref().and_then(|e| names.iter().position(|n| n == e)) {
            None if self.http.env.is_none() => Some(names[0].clone()),
            Some(i) => names.get(i + 1).cloned(),
            None => Some(names[0].clone()),
        };
        match &next {
            Some(n) => self.say(format!("HTTP environment: {n}")),
            None => self.say("HTTP environment: none (only $shared)"),
        }
        self.http.env = next;
    }

    /// `HTTP: Copy response body`.
    pub fn http_copy(&mut self) {
        let Some(l) = &self.http.last else {
            return self.say("HTTP: no response yet");
        };
        let t = body_text(&l.resp);
        self.clip.set(&t);
        self.say("Response body copied");
    }

    /// `HTTP: Open response in editor` (pretty-printed, with the language's highlighting).
    pub fn http_open_response(&mut self) {
        let Some(l) = &self.http.last else {
            return self.say("HTTP: no response yet");
        };
        let ext = httpfile::extension(l.resp.header("content-type"), &l.resp.text());
        let (text, base) = (body_text(&l.resp), cache_dir());
        let _ = std::fs::create_dir_all(&base);
        let Ok(base) = crate::paths::canonical(&base) else { return self.say("HTTP: cannot create the cache folder") };
        let path = base.join(format!("response.{ext}"));
        // A previous response of any type is replaced, not stacked up in tabs.
        let stale: Vec<usize> = (0..self.docs.len())
            .rev()
            .filter(|&i| self.docs[i].path().is_some_and(|p| p.parent() == Some(base.as_path())))
            .collect();
        for i in stale {
            self.close_tab(i, true);
        }
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.open_path(&path, None);
            }
            Err(e) => self.say(format!("HTTP: cannot write {}: {e}", path.display())),
        }
    }

    /// `HTTP: Save response to file`: the raw body under `responses/` next to the `.http` file.
    pub fn http_save_response(&mut self) {
        let Some(l) = &self.http.last else {
            return self.say("HTTP: no response yet");
        };
        let ext = httpfile::extension(l.resp.header("content-type"), &l.resp.text());
        let stem: String = l
            .name
            .clone()
            .unwrap_or_else(|| l.label.split_whitespace().next().unwrap_or("response").to_ascii_lowercase())
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
            .collect();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let dir = l.dir.join("responses");
        let path = dir.join(format!("{stem}-{}-{secs}.{ext}", l.resp.status));
        let body = l.resp.body.clone();
        let res = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, body));
        match res {
            Ok(()) => {
                self.say(format!("Response saved to {}", path.display()));
                self.open_path(&path, None);
            }
            Err(e) => self.say(format!("HTTP: cannot save: {e}")),
        }
    }
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("lumen-text")
        .join("http")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn resp(ct: &str, body: &str) -> Resp {
        Resp {
            status: 200,
            reason: "OK".into(),
            headers: vec![("Content-Type".into(), ct.into())],
            body: body.as_bytes().to_vec(),
            ms: 12,
        }
    }

    #[test]
    fn panel_lines_show_status_headers_and_pretty_body() {
        let (fg, dim) = (Rgba(255, 255, 255, 255), Rgba(1, 1, 1, 255));
        let lines = response_lines("GET http://x/y", &resp("application/json", r#"{"a":[1,"b",null,true]}"#), fg, dim);
        let plain: Vec<String> = lines.iter().map(|l| l.iter().map(|(s, _)| s.as_str()).collect()).collect();
        assert_eq!(plain[0], "HTTP 200 OK   12 ms   23 B");
        assert_eq!(plain[1], "GET http://x/y");
        assert_eq!(plain[3], "Content-Type: application/json");
        assert_eq!(plain[5], "{");
        assert_eq!(plain[6], "  \"a\": [");
        assert_eq!(lines[0][0].1, PALETTE[2]);
        // the key is cyan, the string value green, the number yellow
        let key = lines[6].iter().find(|(s, _)| s == "\"a\"").unwrap();
        assert_eq!(key.1, PALETTE[6]);
        let s = lines[8].iter().find(|(s, _)| s == "\"b\"").unwrap();
        assert_eq!(s.1, PALETTE[2]);
        let n = lines[7].iter().find(|(s, _)| s == "1").unwrap();
        assert_eq!(n.1, PALETTE[3]);
        // text bodies are left alone, errors are red
        let t = response_lines("GET /", &resp("text/plain", "hello\nworld"), fg, dim);
        assert_eq!(t.last().unwrap()[0].0, "world");
        let mut e = resp("text/plain", "");
        e.status = 500;
        assert_eq!(response_lines("GET /", &e, fg, dim)[0][0].1, PALETTE[1]);
        assert_eq!(body_text(&Resp { body: vec![0, 1, 2], ..Default::default() }), "(binary, 3 B)");
    }

    /// Mock server answering each connection with `reply(request_text)`.
    fn mock(n: usize, reply: fn(&str) -> String) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..n {
                let (mut s, _) = l.accept().unwrap();
                s.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
                let (mut got, mut b) = (Vec::new(), [0u8; 4096]);
                while let Ok(k) = s.read(&mut b) {
                    if k == 0 {
                        break;
                    }
                    got.extend_from_slice(&b[..k]);
                    let t = String::from_utf8_lossy(&got).into_owned();
                    if let Some((_, body)) = t.split_once("\r\n\r\n")
                        && (!t.contains("Content-Length") || body.len() >= 1)
                    {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&got).into_owned();
                s.write_all(reply(&req).as_bytes()).unwrap();
                seen.push(req);
            }
            seen
        });
        (port, h)
    }

    fn wait_http(app: &mut App) {
        for _ in 0..200 {
            app.pump_http();
            if app.http.inflight.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the request never finished");
    }

    #[test]
    fn sends_chains_and_cancels_end_to_end() {
        let _g = lock();
        let dir = tmp("http-e2e");
        let (port, srv) = mock(2, |req| {
            let body =
                if req.starts_with("POST /login") { r#"{"token":"s3cret"}"#.to_string() } else { "{}".to_string() };
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        });
        std::fs::write(dir.join("http-client.env.json"), format!(r#"{{"dev":{{"host":"http://127.0.0.1:{port}"}}}}"#))
            .unwrap();
        let src = "### login\n# @name login\nPOST {{host}}/login\nContent-Type: application/json\n\n{\"u\":1}\n\n### me\nGET {{host}}/me\nAuthorization: Bearer {{login.response.body.$.token}}\n";
        let f = dir.join("api.http");
        std::fs::write(&f, src).unwrap();
        let mut app = test_app(&dir);
        app.open_path(&f, None).unwrap();
        app.pump_http();
        let lens: Vec<_> = app.docs[app.active].lenses.iter().map(|l| (l.line, l.title.clone().unwrap())).collect();
        assert_eq!(lens, vec![(2, "▶ Send Request".to_string()), (8, "▶ Send Request".to_string())]);

        // No environment chosen yet: the error names the fix and nothing is sent.
        app.http_send_line(2);
        assert!(app.http.inflight.is_none());
        assert!(app.status.as_ref().unwrap().0.contains("Next environment"), "{:?}", app.status);
        app.http_next_env();
        assert_eq!(app.http.env.as_deref(), Some("dev"));

        // The second request needs the first one's response.
        app.http_send_line(8);
        assert!(app.status.as_ref().unwrap().0.contains("send it first"), "{:?}", app.status);

        app.http_send_line(2);
        assert!(app.http.inflight.is_some());
        assert_eq!(app.docs[app.active].lenses[0].title.as_deref(), Some("■ Cancel request"));
        wait_http(&mut app);
        assert!(app.panel.plain[0].starts_with("HTTP 200 OK"), "{:?}", app.panel.plain);
        assert!(app.panel.open && app.panel.exit == Some(0));
        assert_eq!(app.docs[app.active].lenses[0].title.as_deref(), Some("▶ Send Request"));

        // Click on the second lens (line 8): the token travels in the header.
        app.http_lens_click(8);
        wait_http(&mut app);
        let seen = srv.join().unwrap();
        assert!(seen[0].starts_with("POST /login HTTP/1.1"));
        assert!(seen[0].ends_with("{\"u\":1}"));
        assert!(seen[1].contains("Authorization: Bearer s3cret"), "{}", seen[1]);

        app.http_copy();
        assert_eq!(app.clip.get(), "{}");
        let n = app.docs.len();
        app.http_save_response();
        assert_eq!(app.docs.len(), n + 1);
        assert!(app.docs[app.active].path().unwrap().starts_with(dir.join("responses")));

        // Cancel a request to a server that never answers.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        std::fs::write(&f, format!("GET http://127.0.0.1:{p}/slow\n")).unwrap();
        app.close_tab(app.active, true);
        app.open_path(&f, None).unwrap();
        let idx = app.active;
        let text = std::fs::read_to_string(&f).unwrap();
        app.docs[idx].replace_text(&text);
        app.http_send_line(0);
        assert!(app.http.inflight.is_some());
        let t = std::time::Instant::now();
        app.http_lens_click(0);
        assert!(app.http.inflight.is_none());
        assert!(app.panel.plain[0].contains("cancelled"));
        assert!(t.elapsed() < Duration::from_secs(1));
        drop(l);
    }
}
