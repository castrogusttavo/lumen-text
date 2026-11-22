//! OpenAPI explorer tab: an OpenAPI / Swagger document (`openapi.json`, `openapi.yaml`,
//! `swagger.*`, detected by content) opens as an API reference with a "Try it" client, a response
//! validator and a spec lint, instead of as text. Everything is drawn natively (see
//! `app_openapi_draw`); the spec is read on a worker thread and requests go out through
//! `httpclient` on another, so the UI never blocks.
//!
//! The text buffer stays behind the view: the palette commands "OpenAPI: Show source" and
//! "OpenAPI: Open as API explorer" switch between the two, and the lint list jumps to the line.

use crate::{
    app::{App, Focus, KeyIn},
    field::TextField,
    httpclient::{self, Resp as HttpResp},
    lsp::Wake,
    openapi::{
        copy::{self, Flash},
        example,
        lint::{self, Finding},
        request::{self, Auth, Verdict},
        schema,
        snippets::{self, Req},
        spec::{Operation, Spec},
        tree::{scalar_text, to_json},
    },
    ui::Rect,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::{Duration, Instant},
};
use winit::{
    event::MouseScrollDelta,
    keyboard::{Key, NamedKey},
    window::CursorIcon,
};

const SCROLL_STEP: f64 = 48.0;
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
/// Body lines kept for the response view.
const MAX_BODY_LINES: usize = 3000;
/// Visible lines of the body editor.
pub const BODY_ROWS: usize = 12;

/// What the center column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sel {
    Overview,
    Op(usize),
    Schema(usize),
    Lint,
}

/// A clickable (or editable) area registered by the last frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Nav(Sel),
    Group(String),
    Field(String),
    Body,
    Send,
    Cancel,
    Server(usize),
    Scheme(Option<usize>),
    Eye,
    /// A client of the code snippets (index into `snippets::TARGETS`).
    Lang(usize),
    /// A language group of the picker: its remembered client, else its first.
    LangGroup(&'static str),
    CopySnippet,
    /// The received body (or the error).
    CopyBody,
    /// The whole received answer (status, headers, body).
    CopyResp,
    /// Quick copy of the request as the target `n` (cURL / HTTPie).
    CopyAs(usize),
    CopyUrl,
    CopyParams,
    CopyReqHeaders,
    /// Any block or field: the text and what it is (for the toast).
    Copy(Arc<str>, &'static str),
    RespTab(usize),
    Resp(usize),
    Toggle(String),
    /// Jump to a line of the source.
    Source(usize),
    ShowSource,
    /// Empty result list: wipe the search box.
    ClearSearch,
    Divider,
    /// Narrow layout: `false` = reference, `true` = Try it.
    Pane(bool),
    Media(usize),
    Example,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cur {
    Default,
    Pointer,
    Text,
    Resize,
}

impl Hit {
    pub fn cursor(&self) -> Cur {
        match self {
            Hit::Field(_) | Hit::Body => Cur::Text,
            Hit::Divider => Cur::Resize,
            _ => Cur::Pointer,
        }
    }
}

impl Cur {
    pub fn icon(self) -> CursorIcon {
        match self {
            Cur::Default => CursorIcon::Default,
            Cur::Pointer => CursorIcon::Pointer,
            Cur::Text => CursorIcon::Text,
            Cur::Resize => CursorIcon::ColResize,
        }
    }
}

/// The topmost registered area under the point.
pub fn hit_at(hits: &[(Rect, Hit)], x: f64, y: f64) -> Option<&Hit> {
    hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h)
}

/// The cursor for a point: `None` outside the view.
pub fn cursor_at(view: Rect, hits: &[(Rect, Hit)], dragging: bool, x: f64, y: f64) -> Option<Cur> {
    if !view.contains(x, y) {
        return None;
    }
    if dragging {
        return Some(Cur::Resize);
    }
    Some(hit_at(hits, x, y).map_or(Cur::Default, Hit::cursor))
}

pub enum NavKind {
    Item { label: String, sel: Sel },
    Head { key: String, label: String, count: usize, open: bool },
}

pub struct NavRow {
    pub kind: NavKind,
    pub y: i32,
    pub h: i32,
}

pub struct Inflight {
    cancel: Arc<AtomicBool>,
    pub op: usize,
    pub started: Instant,
}

/// The last answer (or failure) of "Send".
pub struct Last {
    pub op: usize,
    pub req: Req,
    pub result: Result<HttpResp, String>,
    pub verdict: Option<Verdict>,
    /// The body as shown (pretty JSON when it is JSON).
    pub lines: Vec<String>,
    pub json: bool,
}

enum Msg {
    Loaded(Box<Result<(Spec, Vec<Finding>), String>>),
    Done { gen_id: u64, op: usize, req: Req, result: Result<HttpResp, String> },
}

pub struct ApiView {
    wake: Wake,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub spec: Option<Spec>,
    pub findings: Vec<Finding>,
    pub loading: bool,
    pub load_err: Option<String>,
    /// True while the explorer is shown (false = the text of the document).
    pub preview: bool,
    pub sel: Sel,
    pub query: TextField,
    pub collapsed: HashSet<String>,
    pub nav: Vec<NavRow>,
    pub nav_h: i32,
    pub nav_scroll: f64,
    pub nav_w: i32,
    pub scroll_c: f64,
    pub scroll_r: f64,
    pub toggled: HashSet<String>,
    pub resp_sel: usize,
    pub body_media: usize,
    pub fields: HashMap<String, TextField>,
    pub focus: Option<String>,
    pub body: String,
    pub body_cur: usize,
    pub body_top: usize,
    pub body_rows: usize,
    /// Where the body editor was drawn: rect, cell width, line height (for caret placement).
    pub body_geom: std::cell::Cell<(Rect, f32, i32)>,
    pub auth: Option<usize>,
    pub show_secret: bool,
    pub lang: usize,
    pub resp_tab: usize,
    pub try_pane: bool,
    pub inflight: Option<Inflight>,
    pub last: Option<Last>,
    send_gen: u64,
    pub mouse: (f64, f64),
    pub drag: bool,
    pub hits: Vec<(Rect, Hit)>,
    pub rect: Rect,
    pub nav_r: Rect,
    pub center_r: Rect,
    pub right_r: Rect,
    /// Text for the app to put on the clipboard.
    pub copy: Option<(String, String)>,
    /// The last copy, for the "Copied" flash on its button.
    pub flash: Option<Flash>,
    /// Copyable blocks of the last frame (Ctrl+C over one copies it).
    pub blocks: Vec<(Rect, Hit)>,
    /// The client last used in each language group.
    pub lang_memo: HashMap<&'static str, usize>,
    /// Line the app should reveal in the source.
    pub jump: Option<usize>,
    pub want_source: bool,
}

/// Identity of the button behind a copy action (for the "Copied" flash).
pub fn flash_key(h: &Hit) -> u64 {
    match h {
        Hit::Copy(t, _) => copy::key_of(t),
        Hit::CopyAs(i) => copy::key_of(&format!("#as{i}")),
        other => copy::key_of(&format!("#{other:?}")),
    }
}

/// Where the last snippet target is remembered.
#[cfg(not(test))]
fn lang_file() -> Option<std::path::PathBuf> {
    Some(crate::paths::state_home()?.join("lumen-text").join("openapi-snippet-target"))
}

/// The remembered snippet target (cURL the first time).
#[cfg(not(test))]
fn load_lang() -> usize {
    let text = lang_file().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    (0..snippets::TARGETS.len()).find(|&i| snippets::label(i) == text.trim()).unwrap_or(0)
}

#[cfg(test)]
fn load_lang() -> usize {
    0
}

#[cfg(not(test))]
fn save_lang(i: usize) {
    if let Some(p) = lang_file() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, snippets::label(i));
    }
}

#[cfg(test)]
fn save_lang(_: usize) {}

fn byte_of(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map_or(s.len(), |(b, _)| b)
}

/// (line, column) of a char index.
pub fn line_col(text: &str, cur: usize) -> (usize, usize) {
    let (mut line, mut col) = (0, 0);
    for (i, c) in text.chars().enumerate() {
        if i == cur {
            return (line, col);
        }
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Char index of (line, column), clamped to the line.
pub fn pos_of(text: &str, line: usize, col: usize) -> usize {
    let mut idx = 0;
    for (l, t) in text.split('\n').enumerate() {
        let n = t.chars().count();
        if l == line {
            return idx + col.min(n);
        }
        idx += n + 1;
    }
    text.chars().count()
}

impl ApiView {
    pub fn new(text: String, wake: Wake) -> ApiView {
        let (tx, rx) = channel();
        let mut v = ApiView {
            wake,
            tx,
            rx,
            spec: None,
            findings: Vec::new(),
            loading: true,
            load_err: None,
            preview: true,
            sel: Sel::Overview,
            query: TextField::default(),
            collapsed: HashSet::new(),
            nav: Vec::new(),
            nav_h: 0,
            nav_scroll: 0.0,
            nav_w: 300,
            scroll_c: 0.0,
            scroll_r: 0.0,
            toggled: HashSet::new(),
            resp_sel: 0,
            body_media: 0,
            fields: HashMap::new(),
            focus: None,
            body: String::new(),
            body_cur: 0,
            body_top: 0,
            body_rows: BODY_ROWS,
            body_geom: std::cell::Cell::new((Rect::default(), 8.0, 16)),
            auth: None,
            show_secret: false,
            lang: load_lang(),
            resp_tab: 0,
            try_pane: false,
            inflight: None,
            last: None,
            send_gen: 0,
            mouse: (-1.0, -1.0),
            drag: false,
            hits: Vec::new(),
            rect: Rect::default(),
            nav_r: Rect::default(),
            center_r: Rect::default(),
            right_r: Rect::default(),
            copy: None,
            flash: None,
            blocks: Vec::new(),
            lang_memo: HashMap::new(),
            jump: None,
            want_source: false,
        };
        v.lang_memo.insert(snippets::group_of(v.lang), v.lang);
        v.load(text);
        v
    }

    /// Read the spec again (on a worker thread; the old one stays on screen meanwhile).
    pub fn load(&mut self, text: String) {
        self.loading = true;
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let r = Spec::parse(&text).map(|s| {
                let f = lint::lint(&s);
                (s, f)
            });
            let _ = tx.send(Msg::Loaded(Box::new(r)));
            wake();
        });
    }

    pub fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Loaded(b) => {
                    self.loading = false;
                    match *b {
                        Ok((spec, f)) => {
                            let first = self.spec.is_none();
                            self.spec = Some(spec);
                            self.findings = f;
                            self.load_err = None;
                            let ok = match self.sel {
                                Sel::Op(i) => self.spec.as_ref().is_some_and(|s| i < s.ops.len()),
                                Sel::Schema(i) => self.spec.as_ref().is_some_and(|s| i < s.schemas.len()),
                                _ => true,
                            };
                            let sel = if ok { self.sel } else { Sel::Overview };
                            self.select(sel);
                            if first || self.fields.get("base").is_none_or(|f| f.text.is_empty()) {
                                self.pick_server(0);
                            }
                            self.rebuild_nav();
                        }
                        Err(e) => {
                            self.spec = None;
                            self.load_err = Some(e);
                        }
                    }
                }
                Msg::Done { gen_id, op, req, result } if gen_id == self.send_gen => {
                    self.inflight = None;
                    self.last = Some(self.make_last(op, req, result));
                    self.resp_tab = 0;
                }
                Msg::Done { .. } => {}
            }
        }
    }

    fn make_last(&self, op: usize, req: Req, result: Result<HttpResp, String>) -> Last {
        let (mut verdict, mut lines, mut json) = (None, Vec::new(), false);
        if let Ok(r) = &result {
            let text = crate::app_http::body_text(r);
            json = text.starts_with(['{', '[']);
            lines = text.lines().take(MAX_BODY_LINES).map(String::from).collect();
            if let Some(o) = self.spec.as_ref().and_then(|s| s.ops.get(op).map(|o| (s, o))) {
                verdict = Some(request::check_response(&o.0.root, o.1, r.status, r.header("content-type"), &r.text()));
            }
        }
        Last { op, req, result, verdict, lines, json }
    }

    pub fn op(&self) -> Option<&Operation> {
        match (self.sel, self.spec.as_ref()) {
            (Sel::Op(i), Some(s)) => s.ops.get(i),
            _ => None,
        }
    }

    pub fn field_id(loc: &str, name: &str) -> String {
        format!("op:{loc}:{name}")
    }

    /// Show `sel` in the center column and prepare the "Try it" form for it.
    pub fn select(&mut self, sel: Sel) {
        self.sel = sel;
        self.scroll_c = 0.0;
        self.scroll_r = 0.0;
        self.toggled.clear();
        self.resp_sel = 0;
        self.body_media = 0;
        self.focus = None;
        self.fields.retain(|k, _| !k.starts_with("op:"));
        let Some(spec) = self.spec.as_ref() else { return };
        let Sel::Op(i) = sel else { return };
        let Some(op) = spec.ops.get(i) else { return };
        let mut init: Vec<(String, String)> = Vec::new();
        for p in &op.params {
            let ex = p
                .example
                .as_ref()
                .map(scalar_text)
                .or_else(|| schema::deref(&spec.root, &p.schema).get("default").map(scalar_text))
                .unwrap_or_default();
            if !ex.is_empty() {
                init.push((Self::field_id(&p.loc, &p.name), ex));
            }
        }
        let auth = op.security.first().and_then(|n| spec.schemes.iter().position(|s| &s.name == n));
        let resp = op.responses.iter().position(|r| r.code.starts_with('2')).unwrap_or(0);
        for (k, v) in init {
            self.fields.insert(k, TextField::with(&v));
        }
        if auth.is_some() {
            self.auth = auth;
        }
        self.resp_sel = resp;
        self.reset_body();
    }

    /// The body editor gets the example of the selected media type.
    pub fn reset_body(&mut self) {
        let text = match (self.op(), self.spec.as_ref()) {
            (Some(op), Some(spec)) => op
                .body
                .as_ref()
                .and_then(|b| b.media.get(self.body_media))
                .map(|m| match (&m.example, &m.schema) {
                    (Some(e), _) => to_json(e, true),
                    (None, Some(s)) => example::example_json(&spec.root, s),
                    _ => String::new(),
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.body_cur = text.chars().count();
        self.body = text;
        self.body_top = 0;
    }

    pub fn pick_server(&mut self, i: usize) {
        let url = self.spec.as_ref().and_then(|s| s.servers.get(i)).map(|s| s.url.clone());
        if let Some(u) = url {
            self.fields.insert("base".into(), TextField::with(&u));
        }
    }

    /// Rebuild the flat, filtered list of the left column.
    pub fn rebuild_nav(&mut self) {
        let mut rows: Vec<NavKind> = Vec::new();
        let Some(spec) = self.spec.as_ref() else {
            self.nav.clear();
            return;
        };
        let q = self.query.text.trim().to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        let searching = !words.is_empty();
        if !searching {
            rows.push(NavKind::Item { label: "Overview".into(), sel: Sel::Overview });
            let (e, w, _) = lint::counts(&self.findings);
            let label = if e + w == 0 { "Lint".to_string() } else { format!("Lint  ({} issues)", e + w) };
            rows.push(NavKind::Item { label, sel: Sel::Lint });
        }
        for g in spec.groups() {
            let ops: Vec<usize> =
                g.ops.iter().copied().filter(|&i| words.iter().all(|w| spec.ops[i].hay.contains(*w))).collect();
            if ops.is_empty() {
                continue;
            }
            let open = searching || !self.collapsed.contains(&g.name);
            rows.push(NavKind::Head { key: g.name.clone(), label: g.name.clone(), count: ops.len(), open });
            if open {
                for i in ops {
                    rows.push(NavKind::Item { label: String::new(), sel: Sel::Op(i) });
                }
            }
        }
        let models: Vec<usize> = (0..spec.schemas.len())
            .filter(|&i| words.iter().all(|w| spec.schemas[i].0.to_lowercase().contains(*w)))
            .collect();
        if !models.is_empty() {
            let open = searching || !self.collapsed.contains("\0models");
            rows.push(NavKind::Head { key: "\0models".into(), label: "Models".into(), count: models.len(), open });
            if open {
                for i in models {
                    rows.push(NavKind::Item { label: spec.schemas[i].0.clone(), sel: Sel::Schema(i) });
                }
            }
        }
        let mut y = 0;
        self.nav = rows
            .into_iter()
            .map(|kind| {
                let h = match &kind {
                    NavKind::Head { .. } => 38,
                    NavKind::Item { sel: Sel::Op(_), .. } => 34,
                    NavKind::Item { .. } => 32,
                };
                let r = NavRow { kind, y, h };
                y += h;
                r
            })
            .collect();
        self.nav_h = y;
    }

    // ── "Try it" ──

    fn auth_value(&self) -> Option<Auth> {
        let spec = self.spec.as_ref()?;
        let scheme = spec.schemes.get(self.auth?)?;
        let val = |k: &str| self.fields.get(k).map(|f| f.text.clone()).unwrap_or_default();
        Some(Auth { kind: scheme.kind.clone(), value: val("auth:value"), user: val("auth:user") })
    }

    /// The request the form describes. `strict` = errors for missing values (Send); otherwise
    /// placeholders (the snippets).
    pub fn build_req(&self, strict: bool) -> Result<Req, String> {
        let op = self.op().ok_or("no operation selected")?;
        let values = self.param_rows();
        let base = self.fields.get("base").map(|f| f.text.clone()).unwrap_or_default();
        let ct = op
            .body
            .as_ref()
            .and_then(|b| b.media.get(self.body_media))
            .map(|m| m.content_type.clone())
            .unwrap_or_default();
        request::build(op, &base, &values, &self.body, &ct, self.auth_value().as_ref(), strict)
    }

    pub fn send(&mut self) {
        if self.inflight.is_some() {
            return;
        }
        let Sel::Op(op) = self.sel else { return };
        let req = match self.build_req(true) {
            Ok(r) => r,
            Err(e) => {
                self.last =
                    Some(Last { op, req: Req::default(), result: Err(e), verdict: None, lines: vec![], json: false });
                self.resp_tab = 0;
                return;
            }
        };
        let prepared = httpclient::Prepared {
            method: req.method.clone(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: req.body.clone().unwrap_or_default().into_bytes(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        self.send_gen += 1;
        let (gen_id, tx, wake) = (self.send_gen, self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let result = httpclient::send(&prepared, &flag, SEND_TIMEOUT);
            let _ = tx.send(Msg::Done { gen_id, op, req, result });
            wake();
        });
        self.inflight = Some(Inflight { cancel, op, started: Instant::now() });
    }

    pub fn cancel(&mut self) {
        if let Some(f) = self.inflight.take() {
            f.cancel.store(true, Ordering::Relaxed);
            self.send_gen += 1;
        }
    }

    // ── input ──

    pub fn wheel(&mut self, x: f64, y: f64, delta: MouseScrollDelta) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        let (_, dy) = crate::scroller::wheel_px_xy(delta, SCROLL_STEP);
        if self.nav_r.contains(x, y) {
            self.nav_scroll = (self.nav_scroll + dy).max(0.0);
        } else if self.right_r.contains(x, y) {
            self.scroll_r = (self.scroll_r + dy).max(0.0);
        } else {
            self.scroll_c = (self.scroll_c + dy).max(0.0);
        }
        true
    }

    pub fn mouse_move(&mut self, x: f64, y: f64) -> bool {
        self.mouse = (x, y);
        if self.drag {
            let max = (self.rect.w / 2).max(200);
            self.nav_w = ((x - self.rect.x as f64) as i32).clamp(200, max);
            return true;
        }
        self.rect.contains(x, y)
    }

    pub fn release(&mut self) {
        self.drag = false;
    }

    pub fn cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        cursor_at(self.rect, &self.hits, self.drag, x, y).map(Cur::icon)
    }

    pub fn mouse_down(&mut self, x: f64, y: f64) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        self.mouse = (x, y);
        let hit = hit_at(&self.hits, x, y).cloned();
        match &hit {
            Some(Hit::Field(_)) | Some(Hit::Body) => {}
            _ => self.focus = None,
        }
        let Some(hit) = hit else { return true };
        match hit {
            Hit::Nav(sel) => {
                self.select(sel);
                if self.rect.w < NARROW {
                    self.try_pane = false;
                }
            }
            Hit::Group(key) => {
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.rebuild_nav();
            }
            Hit::Field(id) => {
                if let Some(f) = self.field_mut(&id) {
                    f.end();
                    f.sel_all = false;
                }
                self.focus = Some(id);
            }
            Hit::Body => {
                self.focus = Some("body".into());
                let (r, cw, lh) = self.body_geom.get();
                let line = self.body_top + ((y - r.y as f64 - 8.0) / lh.max(1) as f64).max(0.0) as usize;
                let col = ((x - r.x as f64 - 10.0) / cw.max(1.0) as f64).round().max(0.0) as usize;
                self.body_cur = pos_of(&self.body, line, col);
            }
            Hit::Send => self.send(),
            Hit::Cancel => self.cancel(),
            Hit::Server(i) => self.pick_server(i),
            Hit::Scheme(i) => self.auth = i,
            Hit::Eye => self.show_secret = !self.show_secret,
            Hit::Lang(i) => self.set_lang(i),
            Hit::LangGroup(g) => {
                let i = match self.lang_memo.get(g) {
                    Some(&i) => i,
                    None => snippets::of_group(g).first().copied().unwrap_or(0),
                };
                self.set_lang(i);
            }
            Hit::CopySnippet
            | Hit::CopyBody
            | Hit::CopyResp
            | Hit::CopyAs(_)
            | Hit::CopyUrl
            | Hit::CopyParams
            | Hit::CopyReqHeaders
            | Hit::Copy(..) => self.run_copy(&hit),
            Hit::RespTab(i) => self.resp_tab = i,
            Hit::Resp(i) => {
                self.resp_sel = i;
                self.toggled.clear();
            }
            Hit::Toggle(p) => {
                if !self.toggled.remove(&p) {
                    self.toggled.insert(p);
                }
            }
            Hit::Source(line) => self.jump = Some(line),
            Hit::ShowSource => self.want_source = true,
            Hit::ClearSearch => {
                self.query = TextField::default();
                self.rebuild_nav();
            }
            Hit::Divider => self.drag = true,
            Hit::Pane(t) => self.try_pane = t,
            Hit::Media(i) => {
                self.body_media = i;
                self.reset_body();
            }
            Hit::Example => self.reset_body(),
        }
        true
    }

    fn set_lang(&mut self, i: usize) {
        self.lang = i.min(snippets::TARGETS.len() - 1);
        self.lang_memo.insert(snippets::group_of(self.lang), self.lang);
        save_lang(self.lang);
    }

    /// The (location, name, value) of every parameter field of the selected operation.
    pub fn param_rows(&self) -> Vec<(String, String, String)> {
        let Some(op) = self.op() else { return Vec::new() };
        op.params
            .iter()
            .map(|p| {
                let v = self.fields.get(&Self::field_id(&p.loc, &p.name)).map(|f| f.text.clone()).unwrap_or_default();
                (p.loc.clone(), p.name.clone(), v)
            })
            .collect()
    }

    /// What a copy action puts on the clipboard, and its name for the toast. `None` when there
    /// is nothing to copy (no answer yet, no operation).
    pub fn copy_text(&self, hit: &Hit) -> Option<(String, String)> {
        let some = |t: String, w: &str| Some((t, w.to_string()));
        match hit {
            Hit::Copy(t, what) => some(t.to_string(), what),
            Hit::CopySnippet => {
                let r = self.build_req(false).ok()?;
                some(snippets::render(self.lang, &r), &snippets::label(self.lang))
            }
            Hit::CopyAs(i) => {
                let r = self.build_req(false).ok()?;
                some(snippets::render(*i, &r), &snippets::label(*i))
            }
            Hit::CopyUrl => some(self.build_req(false).ok()?.url, "URL"),
            Hit::CopyParams => {
                let t = copy::params(&self.param_rows());
                if t.is_empty() { None } else { some(t, "parameters") }
            }
            Hit::CopyReqHeaders => {
                let r = self.build_req(false).ok()?;
                if r.headers.is_empty() { None } else { some(copy::headers(&r.headers), "request headers") }
            }
            Hit::CopyBody => {
                let l = self.last.as_ref()?;
                match &l.result {
                    Ok(r) => some(r.text(), "response body"),
                    Err(e) => some(e.clone(), "error"),
                }
            }
            Hit::CopyResp => {
                let l = self.last.as_ref()?;
                match &l.result {
                    Ok(r) => {
                        let status = format!("{} {}", r.status, r.reason);
                        some(copy::response(status.trim(), &r.headers, &r.text()), "response")
                    }
                    Err(e) => some(e.clone(), "error"),
                }
            }
            _ => None,
        }
    }

    /// Queue a copy for the app (clipboard + toast) and flash the button.
    pub fn run_copy(&mut self, hit: &Hit) {
        if let Some((text, what)) = self.copy_text(hit) {
            self.flash = Some(Flash::new(flash_key(hit)));
            self.copy = Some((text, what));
        }
    }

    /// Ctrl+C: the focused field (its selection, else all of it), else the copyable block under
    /// the mouse. False when there is nothing to copy.
    pub fn copy_key(&mut self) -> bool {
        if let Some(id) = self.focus.clone() {
            let text = if id == "body" {
                Some(self.body.clone())
            } else {
                self.field_mut(&id).map(|f| f.selected_text().unwrap_or_else(|| f.text.clone()))
            };
            return match text.filter(|t| !t.is_empty()) {
                Some(t) => {
                    self.copy = Some((t, "field".into()));
                    true
                }
                None => false,
            };
        }
        let (x, y) = self.mouse;
        let hit = self.blocks.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone());
        match hit {
            Some(h) => {
                self.run_copy(&h);
                self.copy.is_some()
            }
            None => false,
        }
    }

    fn field_mut(&mut self, id: &str) -> Option<&mut TextField> {
        match id {
            "body" => None,
            "search" => Some(&mut self.query),
            _ => Some(self.fields.entry(id.to_string()).or_default()),
        }
    }

    /// Text typed or pasted into the focused field.
    pub fn insert(&mut self, text: &str) {
        match self.focus.clone() {
            Some(id) if id == "body" => {
                let b = byte_of(&self.body, self.body_cur);
                self.body.insert_str(b, text);
                self.body_cur += text.chars().count();
            }
            Some(id) => {
                let one_line: String = text.chars().filter(|c| !c.is_control()).collect();
                if let Some(f) = self.field_mut(&id) {
                    f.insert(&one_line);
                }
                if id == "search" {
                    self.nav_scroll = 0.0;
                    self.rebuild_nav();
                }
            }
            None => {
                self.focus = Some("search".into());
                self.insert(text);
            }
        }
    }

    /// An editing key of the body editor; false when it is not one (text goes through `insert`).
    fn body_key(&mut self, key: &NamedKey) -> bool {
        let n = self.body.chars().count();
        match key {
            NamedKey::Enter => self.insert("\n"),
            NamedKey::Tab => self.insert("  "),
            NamedKey::Backspace if self.body_cur > 0 => {
                let (a, b) = (byte_of(&self.body, self.body_cur - 1), byte_of(&self.body, self.body_cur));
                self.body.replace_range(a..b, "");
                self.body_cur -= 1;
            }
            NamedKey::Delete if self.body_cur < n => {
                let (a, b) = (byte_of(&self.body, self.body_cur), byte_of(&self.body, self.body_cur + 1));
                self.body.replace_range(a..b, "");
            }
            NamedKey::ArrowLeft => self.body_cur = self.body_cur.saturating_sub(1),
            NamedKey::ArrowRight => self.body_cur = (self.body_cur + 1).min(n),
            NamedKey::Home => {
                let (l, _) = line_col(&self.body, self.body_cur);
                self.body_cur = pos_of(&self.body, l, 0);
            }
            NamedKey::End => {
                let (l, _) = line_col(&self.body, self.body_cur);
                self.body_cur = pos_of(&self.body, l, usize::MAX);
            }
            NamedKey::ArrowUp | NamedKey::ArrowDown => {
                let (l, c) = line_col(&self.body, self.body_cur);
                let nl = if *key == NamedKey::ArrowUp { l.saturating_sub(1) } else { l + 1 };
                self.body_cur = pos_of(&self.body, nl, c);
            }
            _ => return false,
        }
        // Keep the caret in view.
        let (l, _) = line_col(&self.body, self.body_cur);
        let rows = self.body_rows.max(1);
        if l < self.body_top {
            self.body_top = l;
        } else if l >= self.body_top + rows {
            self.body_top = l + 1 - rows;
        }
        true
    }

    /// A key for the view. `cmd` = Ctrl / Alt / Super held (never typed text).
    pub fn key(&mut self, ev: &KeyIn, cmd: bool) -> bool {
        let focus = self.focus.clone();
        match &ev.key {
            Key::Named(NamedKey::Escape) => {
                if focus.is_some() {
                    self.focus = None;
                } else {
                    self.cancel();
                }
            }
            Key::Named(k) if focus.as_deref() == Some("body") && !cmd && self.body_key(k) => {}
            Key::Named(NamedKey::Enter) if focus.as_deref().is_some_and(|f| f != "search") => {
                if cmd || self.focus.as_deref() != Some("body") {
                    self.send();
                }
            }
            Key::Named(NamedKey::Backspace) if focus.is_some() => {
                if let Some(f) = focus.as_deref().and_then(|id| self.field_mut(id)) {
                    f.backspace();
                }
                self.after_edit(focus.as_deref());
            }
            Key::Named(NamedKey::Delete) if focus.is_some() => {
                if let Some(f) = focus.as_deref().and_then(|id| self.field_mut(id)) {
                    f.delete();
                }
                self.after_edit(focus.as_deref());
            }
            Key::Named(NamedKey::ArrowLeft) if focus.is_some() => {
                if let Some(f) = focus.as_deref().and_then(|id| self.field_mut(id)) {
                    f.left();
                }
            }
            Key::Named(NamedKey::ArrowRight) if focus.is_some() => {
                if let Some(f) = focus.as_deref().and_then(|id| self.field_mut(id)) {
                    f.right();
                }
            }
            Key::Named(NamedKey::Home) if focus.is_some() => {
                if let Some(f) = focus.as_deref().and_then(|id| self.field_mut(id)) {
                    f.home();
                }
            }
            Key::Named(NamedKey::End) if focus.is_some() => {
                if let Some(f) = focus.as_deref().and_then(|id| self.field_mut(id)) {
                    f.end();
                }
            }
            Key::Named(NamedKey::ArrowUp) => self.scroll_c = (self.scroll_c - SCROLL_STEP).max(0.0),
            Key::Named(NamedKey::ArrowDown) => self.scroll_c += SCROLL_STEP,
            Key::Named(NamedKey::PageUp) => self.scroll_c = (self.scroll_c - self.center_r.h as f64).max(0.0),
            Key::Named(NamedKey::PageDown) => self.scroll_c += self.center_r.h as f64,
            Key::Named(NamedKey::Tab) if focus.is_some() => self.next_field(),
            _ => {
                if !cmd && let Some(t) = ev.text.as_deref().filter(|t| !t.chars().any(char::is_control)) {
                    self.insert(t);
                }
            }
        }
        true
    }

    fn after_edit(&mut self, id: Option<&str>) {
        if id == Some("search") {
            self.nav_scroll = 0.0;
            self.rebuild_nav();
        }
    }

    /// Tab: the next input of the "Try it" form.
    fn next_field(&mut self) {
        let Some(op) = self.op() else { return };
        let mut order: Vec<String> = vec!["base".into()];
        order.extend(op.params.iter().map(|p| Self::field_id(&p.loc, &p.name)));
        if op.body.is_some() {
            order.push("body".into());
        }
        let cur = self.focus.as_deref().and_then(|f| order.iter().position(|o| o == f));
        let next = cur.map_or(0, |i| (i + 1) % order.len());
        self.focus = Some(order[next].clone());
    }
}

/// Below this width the "Try it" column becomes a tab of the center column.
pub const NARROW: i32 = 980;

impl App {
    fn api_active(&mut self) -> Option<&mut ApiView> {
        self.docs.get_mut(self.active).and_then(|d| d.api.as_deref_mut()).filter(|v| v.preview)
    }

    /// Clipboard and source jumps the view asked for.
    fn api_after(&mut self) {
        let (copy, jump, src) = match self.api_active() {
            Some(v) => (v.copy.take(), v.jump.take(), std::mem::take(&mut v.want_source)),
            None => return,
        };
        if let Some((t, what)) = copy {
            self.clip.set(&t);
            self.say(format!("Copied {what}"));
        }
        if src {
            self.api_set_preview(false, None);
        }
        if let Some(line) = jump {
            self.api_set_preview(false, Some(line));
        }
    }

    pub fn api_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (x, y) = self.mouse;
        self.api_active().is_some_and(|v| v.wheel(x, y, delta))
    }

    pub fn api_mouse_down(&mut self, x: f64, y: f64) -> bool {
        let taken = self.api_active().is_some_and(|v| v.mouse_down(x, y));
        if taken {
            self.focus = Focus::Editor;
            self.api_after();
        }
        taken
    }

    pub fn api_mouse_move(&mut self, x: f64, y: f64) -> bool {
        let moved = self.api_active().is_some_and(|v| v.mouse_move(x, y));
        if moved {
            self.request_redraw();
        }
        moved
    }

    pub fn api_release(&mut self) {
        if let Some(v) = self.api_active() {
            v.release();
        }
    }

    pub fn api_cursor(&self, x: f64, y: f64) -> Option<CursorIcon> {
        let v = self.docs.get(self.active)?.api.as_deref().filter(|v| v.preview)?;
        v.cursor(x, y)
    }

    pub fn api_key(&mut self, ev: &KeyIn) -> bool {
        let (ctrl, cmd) = (
            self.mods.control_key() || self.mods.super_key(),
            self.mods.control_key() || self.mods.alt_key() || self.mods.super_key(),
        );
        if self.api_active().is_none() {
            return false;
        }
        let copy = ctrl && matches!(&ev.key, Key::Character(c) if c.eq_ignore_ascii_case("c"));
        if copy {
            let done = self.api_active().is_some_and(|v| v.copy_key());
            if done {
                self.api_after();
                return true;
            }
        }
        let paste = ctrl && matches!(&ev.key, Key::Character(c) if c.eq_ignore_ascii_case("v"));
        if paste {
            let text = self.clip.get();
            if let Some(v) = self.api_active() {
                v.insert(&text);
            }
            return true;
        }
        let r = self.api_active().is_some_and(|v| v.key(ev, cmd));
        self.api_after();
        r
    }

    /// Switch the active tab between the explorer and the text; `line` moves the caret there.
    pub fn api_set_preview(&mut self, preview: bool, line: Option<usize>) -> bool {
        let Some(d) = self.docs.get_mut(self.active) else { return false };
        let Some(v) = d.api.as_mut() else { return false };
        v.preview = preview;
        v.drag = false;
        d.buf.read_only = preview;
        d.reveal = true;
        if preview {
            let text = d.buf.slice(0, d.buf.len_chars());
            if let Some(v) = d.api.as_mut() {
                v.load(text);
            }
        } else if let Some(l) = line {
            let l = l.min(d.buf.line_count().saturating_sub(1));
            let p = d.buf.pos(l, 0);
            d.ed.click(&mut d.buf, p, false);
            d.target = l.saturating_sub(3) as f64;
            d.top = d.target;
        }
        self.request_redraw();
        true
    }

    /// "OpenAPI: Open as API explorer" / "OpenAPI: Show source".
    pub fn api_command(&mut self, preview: bool) {
        let is_spec = self.docs.get(self.active).is_some_and(|d| d.api.is_some());
        if !is_spec {
            // A JSON / YAML file that was opened as text: make it an explorer if it is a spec.
            let ok = self.docs.get(self.active).is_some_and(|d| {
                let head = d.buf.slice(0, d.buf.len_chars().min(4000));
                crate::openapi::spec::sniff(&head)
            });
            if !ok || !preview {
                return self.say("This file is not an OpenAPI / Swagger document");
            }
            let wake = self.wake();
            let Some(d) = self.docs.get_mut(self.active) else { return };
            let text = d.buf.slice(0, d.buf.len_chars());
            d.api = Some(Box::new(ApiView::new(text, wake)));
            d.buf.read_only = true;
            self.request_redraw();
            return;
        }
        self.api_set_preview(preview, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits() -> Vec<(Rect, Hit)> {
        vec![
            (Rect::new(0, 0, 100, 30), Hit::Nav(Sel::Overview)),
            (Rect::new(10, 40, 80, 24), Hit::Field("base".into())),
            (Rect::new(98, 0, 4, 200), Hit::Divider),
            (Rect::new(10, 100, 40, 20), Hit::Send),
            (Rect::new(10, 130, 40, 20), Hit::Body),
        ]
    }

    #[test]
    fn point_to_cursor_mapping() {
        let view = Rect::new(0, 0, 500, 400);
        let h = hits();
        let at = |x: f64, y: f64| cursor_at(view, &h, false, x, y);
        assert_eq!(at(20.0, 10.0), Some(Cur::Pointer), "nav item");
        assert_eq!(at(20.0, 50.0), Some(Cur::Text), "input field");
        assert_eq!(at(20.0, 110.0), Some(Cur::Pointer), "send button");
        assert_eq!(at(20.0, 140.0), Some(Cur::Text), "body editor");
        assert_eq!(at(100.0, 150.0), Some(Cur::Resize), "divider");
        assert_eq!(at(300.0, 300.0), Some(Cur::Default), "nothing there");
        assert_eq!(at(600.0, 10.0), None, "outside the view");
        assert_eq!(cursor_at(view, &h, true, 300.0, 300.0), Some(Cur::Resize), "while dragging the divider");
        assert_eq!(Cur::Resize.icon(), CursorIcon::ColResize);
        assert_eq!(Cur::Text.icon(), CursorIcon::Text);
    }

    #[test]
    fn every_action_has_a_sensible_cursor() {
        for hit in [
            Hit::Send,
            Hit::Cancel,
            Hit::Eye,
            Hit::CopySnippet,
            Hit::Toggle("a".into()),
            Hit::Group("g".into()),
            Hit::Pane(true),
            Hit::Source(3),
        ] {
            assert_eq!(hit.cursor(), Cur::Pointer, "{hit:?}");
        }
        assert_eq!(Hit::Field("x".into()).cursor(), Cur::Text);
        assert_eq!(Hit::Divider.cursor(), Cur::Resize);
    }

    #[test]
    fn copy_actions_have_a_pointer_and_text() {
        for hit in [
            Hit::Copy(Arc::from("x"), "x"),
            Hit::CopyBody,
            Hit::CopyResp,
            Hit::CopyAs(0),
            Hit::CopyUrl,
            Hit::CopyParams,
            Hit::CopyReqHeaders,
            Hit::LangGroup("Python"),
            Hit::Lang(3),
        ] {
            assert_eq!(hit.cursor(), Cur::Pointer, "{hit:?}");
        }
        let mut v = loaded(crate::openapi::spec::PETSTORE);
        let show = v.spec.as_ref().unwrap().ops.iter().position(|o| o.op_id == "showPetById").unwrap();
        v.select(Sel::Op(show));
        v.fields.insert(ApiView::field_id("path", "petId"), TextField::with("42"));
        let (url, _) = v.copy_text(&Hit::CopyUrl).unwrap();
        assert_eq!(url, "https://api.example.com/v1/pets/42");
        let (p, _) = v.copy_text(&Hit::CopyParams).unwrap();
        assert_eq!(p, "[path] petId: 42");
        let (c, what) = v.copy_text(&Hit::CopyAs(crate::openapi::snippets::CURL)).unwrap();
        assert!(c.starts_with("curl 'https://api.example.com/v1/pets/42'") && what == "Shell · cURL", "{c}");
        assert!(v.copy_text(&Hit::CopyBody).is_none(), "no answer yet");
        v.run_copy(&Hit::CopyUrl);
        assert!(v.copy.is_some() && v.flash.is_some_and(|f| f.on(flash_key(&Hit::CopyUrl))));
        v.set_lang(9);
        assert_eq!(v.lang_memo.get("Python"), Some(&9));
    }

    #[test]
    fn body_caret_math() {
        let t = "{\n  \"a\": 1,\n  \"b\": 2\n}";
        assert_eq!(line_col(t, 0), (0, 0));
        assert_eq!(line_col(t, 4), (1, 2));
        assert_eq!(pos_of(t, 1, 2), 4);
        assert_eq!(pos_of(t, 1, 999), 11, "clamped to the end of the line");
        assert_eq!(pos_of(t, 9, 0), t.chars().count(), "past the last line");
        assert_eq!(line_col(t, t.chars().count()), (3, 1));
    }

    fn wake() -> Wake {
        Arc::new(|| {})
    }

    fn loaded(text: &str) -> ApiView {
        let mut v = ApiView::new(text.to_string(), wake());
        for _ in 0..200 {
            v.poll();
            if !v.loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        v
    }

    #[test]
    fn the_form_is_built_from_the_selected_operation() {
        let mut v = loaded(crate::openapi::spec::PETSTORE);
        assert!(v.spec.is_some(), "{:?}", v.load_err);
        assert_eq!(v.fields.get("base").map(|f| f.text.as_str()), Some("https://api.example.com/v1"));
        let show = v.spec.as_ref().unwrap().ops.iter().position(|o| o.op_id == "showPetById").unwrap();
        v.select(Sel::Op(show));
        assert_eq!(v.auth, Some(0), "the operation's bearer scheme is preselected");
        assert!(v.build_req(true).is_err(), "petId is empty");
        v.fields.insert(ApiView::field_id("path", "petId"), TextField::with("42"));
        v.fields.insert("auth:value".into(), TextField::with("secret"));
        let r = v.build_req(true).unwrap();
        assert_eq!(r.url, "https://api.example.com/v1/pets/42");
        assert!(r.headers.contains(&("Authorization".into(), "Bearer secret".into())));
        let create = v.spec.as_ref().unwrap().ops.iter().position(|o| o.op_id == "createPet").unwrap();
        v.select(Sel::Op(create));
        assert!(v.body.contains("\"name\""), "the body starts from the generated example: {}", v.body);
    }

    #[test]
    fn the_list_filters_and_collapses() {
        let mut v = loaded(crate::openapi::spec::PETSTORE);
        let items =
            |v: &ApiView| v.nav.iter().filter(|r| matches!(r.kind, NavKind::Item { sel: Sel::Op(_), .. })).count();
        assert_eq!(items(&v), 3);
        v.collapsed.insert("pets".into());
        v.rebuild_nav();
        assert_eq!(items(&v), 0);
        v.collapsed.clear();
        v.query = TextField::with("create");
        v.rebuild_nav();
        assert_eq!(items(&v), 1);
        assert!(v.nav.windows(2).all(|w| w[1].y == w[0].y + w[0].h), "rows are stacked");
    }

    #[test]
    fn clear_search_brings_the_list_back() {
        let mut v = loaded(crate::openapi::spec::PETSTORE);
        v.query = TextField::with("zzz-no-such-endpoint");
        v.rebuild_nav();
        assert!(v.nav.is_empty(), "nothing matches: the empty state shows");
        v.rect = Rect::new(0, 0, 400, 300);
        v.hits = vec![(Rect::new(10, 10, 50, 20), Hit::ClearSearch)];
        assert_eq!(Hit::ClearSearch.cursor(), Cur::Pointer);
        assert!(v.mouse_down(12.0, 12.0));
        assert!(v.query.text.is_empty() && !v.nav.is_empty());
    }

    #[test]
    fn a_broken_document_reports_why() {
        let v = loaded("{\"hello\": 1}");
        assert!(v.spec.is_none() && v.load_err.is_some());
    }

    #[test]
    fn the_real_world_spec_loads_quickly() {
        let Ok(text) = std::fs::read_to_string(
            "/tmp/claude-1000/-home-castro-www-lumen-text/3db4dd13-4bbc-40c8-affa-3cb7983c8d86/scratchpad/scalar/openapi.json",
        ) else {
            return; // the fixture only exists on the author's machine
        };
        let t0 = Instant::now();
        let s = Spec::parse(&text).unwrap();
        assert!(s.ops.len() > 100, "{}", s.ops.len());
        let f = lint::lint(&s);
        assert!(t0.elapsed() < Duration::from_secs(2), "parse + lint took {:?}", t0.elapsed());
        assert!(
            !f.iter().any(|x| x.code == "broken-ref"),
            "{:?}",
            f.iter().filter(|x| x.code == "broken-ref").collect::<Vec<_>>()
        );
    }
}
