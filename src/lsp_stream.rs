//! Bounded-memory reading of LSP frames.
//!
//! A language server may answer with hundreds of megabytes (a `documentSymbol` tree or the
//! `semanticTokens` of a 100k-line file); building a `serde_json::Value` out of that used to cost
//! ~900 MB of transient heap. The reader thread therefore never buffers a big body:
//!
//! * bodies up to [`Limits::stream_above`] are read whole and parsed as a `Value`, as before;
//! * bigger ones are parsed *while they are read* with serde visitors that keep only what the
//!   editor needs: the first levels of a symbol tree, the semantic token integers packed as `u32`,
//!   a generic `Value` when the body is still within [`Limits::max_value`], and nothing (the rest of
//!   the body is skipped and an error response is synthesized) when it is not.
//!
//! The policy for a response is chosen by the request's method ([`policy_for`]), registered by the
//! sender under the request id; the reader looks it up when it meets the `result` key.

use crate::doc::Sym;
use crate::lsp::Msg;
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{
    cell::RefCell,
    fmt,
    io::{self, BufRead, BufReader, Read},
};

/// What to keep of a response, chosen by the method that was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Policy {
    /// A generic `Value` (dropped when the body is over [`Limits::max_value`]).
    #[default]
    Value,
    /// `documentSymbol`: only the first [`Limits::symbol_levels`] levels, at most [`Limits::max_symbols`] entries.
    Symbols,
    /// `semanticTokens/*`: the `data` integers, packed (delivered as [`Msg::Tokens`]).
    Tokens,
    /// `foldingRange`: only `startLine`/`endLine` of at most [`Limits::max_folds`] ranges.
    Folds,
    /// `workspace/diagnostic`: a capped report (delivered as [`Msg::WsDiag`], see `problems`).
    WsDiag,
}

pub fn policy_for(method: &str) -> Policy {
    match method {
        "textDocument/documentSymbol" => Policy::Symbols,
        "textDocument/foldingRange" => Policy::Folds,
        m if m.starts_with("textDocument/semanticTokens/") => Policy::Tokens,
        "workspace/diagnostic" => Policy::WsDiag,
        _ => Policy::Value,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Bodies up to this size are buffered and parsed whole.
    pub stream_above: usize,
    /// Largest body that may become a generic `Value` (anything bigger is skipped).
    pub max_value: usize,
    /// Symbols kept from a streamed `documentSymbol` response.
    pub max_symbols: usize,
    /// Tree levels kept from a streamed `documentSymbol` response (1 = top level only).
    pub symbol_levels: usize,
    /// Semantic token integers kept (5 per token).
    pub max_token_ints: usize,
    /// Folding ranges kept.
    pub max_folds: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            stream_above: 256 * 1024,
            max_value: 4 * 1024 * 1024,
            max_symbols: 50_000,
            symbol_levels: 3,
            max_token_ints: 4_000_000,
            max_folds: 50_000,
        }
    }
}

/// Longest header line accepted (a runaway line without a newline must not grow without bound).
const MAX_HEADER_LINE: u64 = 4096;

/// One step of the reader.
pub enum Frame {
    Eof,
    /// A frame that was malformed or deliberately not delivered (oversized notification...).
    Skipped,
    Msg(Msg),
}

/// Read the next frame from `r`. `policy_of` maps a response id to its [`Policy`].
pub fn read_message<R: BufRead>(r: &mut R, lim: &Limits, policy_of: &dyn Fn(i64) -> Policy) -> io::Result<Frame> {
    // Headers.
    let mut len: Option<usize> = None;
    let mut any = false;
    loop {
        let mut line = Vec::new();
        let n = r.by_ref().take(MAX_HEADER_LINE).read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(Frame::Eof);
        }
        let text = String::from_utf8_lossy(&line).trim().to_ascii_lowercase();
        if text.is_empty() {
            if any {
                break;
            }
            continue; // stray blank line between frames
        }
        any = true;
        if let Some(v) = text.strip_prefix("content-length:") {
            len = v.trim().parse().ok();
        }
    }
    let Some(len) = len else { return Ok(Frame::Skipped) };

    if len <= lim.stream_above {
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf)?;
        let Ok(v) = serde_json::from_slice::<Value>(&buf) else { return Ok(Frame::Skipped) };
        return Ok(match crate::lsp::classify(v) {
            Some(Msg::Response { id, result: Ok(v) }) => match policy_of(id) {
                Policy::Tokens => Msg::Tokens { id, data: tokens_from_value(&v) }.into(),
                Policy::Symbols => Msg::Symbols { id, syms: crate::doc::flatten_symbols(&v) }.into(),
                Policy::WsDiag => Msg::WsDiag { id, report: crate::problems::WsReport::from_value(v) }.into(),
                _ => Msg::Response { id, result: Ok(v) }.into(),
            },
            Some(m) => partial_report(m).into(),
            None => Frame::Skipped,
        });
    }

    // Big body: parse while reading, then skip whatever is left so the framing stays in sync.
    let mut body = r.by_ref().take(len as u64);
    let parsed = {
        let reader = BufReader::with_capacity(32 * 1024, &mut body);
        let mut de = serde_json::Deserializer::from_reader(reader);
        Envelope { lim, len, policy_of }.deserialize(&mut de)
    };
    io::copy(&mut body, &mut io::sink())?;
    match parsed {
        Ok(p) => Ok(match p.into_frame(len) {
            Frame::Msg(m) => partial_report(m).into(),
            f => f,
        }),
        Err(e) if e.is_io() => Err(e.into()),
        Err(_) => Ok(Frame::Skipped),
    }
}

impl From<Msg> for Frame {
    fn from(m: Msg) -> Frame {
        Frame::Msg(m)
    }
}

/// A `$/progress` notification carrying a partial result of one of our `workspace/diagnostic`
/// requests (token [`crate::problems::WS_TOKEN_PREFIX`]) becomes [`Msg::WsPartial`], parsed here
/// on the reader thread; anything else passes through.
fn partial_report(m: Msg) -> Msg {
    match m {
        Msg::Notification { method, mut params }
            if method == "$/progress"
                && params["token"].as_str().is_some_and(|t| t.starts_with(crate::problems::WS_TOKEN_PREFIX)) =>
        {
            let token = params["token"].as_str().unwrap_or_default().to_string();
            let value = params.get_mut("value").map(Value::take).unwrap_or(Value::Null);
            Msg::WsPartial { token, report: crate::problems::WsReport::from_value(value) }
        }
        m => m,
    }
}

fn tokens_from_value(v: &Value) -> Vec<u32> {
    v["data"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect()).unwrap_or_default()
}

// ───────────────────────────── envelope ─────────────────────────────

enum Res {
    Value(Value),
    Symbols(Vec<Sym>),
    Tokens(Vec<u32>),
    WsDiag(crate::problems::WsReport),
    TooLarge,
}

#[derive(Default)]
struct Parsed {
    id: Option<Value>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Res>,
    error: Option<Value>,
    dropped: bool,
}

impl Parsed {
    fn into_frame(self, len: usize) -> Frame {
        let id_i64 = self.id.as_ref().and_then(Value::as_i64);
        match (self.method, self.id) {
            (Some(method), Some(id)) if !self.dropped => {
                Msg::Request { id, method, params: self.params.unwrap_or(Value::Null) }.into()
            }
            (Some(method), None) if !self.dropped => {
                Msg::Notification { method, params: self.params.unwrap_or(Value::Null) }.into()
            }
            (Some(_), _) => Frame::Skipped, // oversized request / notification
            (None, Some(_)) => {
                let Some(id) = id_i64 else { return Frame::Skipped };
                if let Some(e) = self.error {
                    return Msg::Response { id, result: Err(e) }.into();
                }
                match self.result {
                    Some(Res::Value(v)) => Msg::Response { id, result: Ok(v) }.into(),
                    Some(Res::Tokens(data)) => Msg::Tokens { id, data }.into(),
                    Some(Res::Symbols(syms)) => Msg::Symbols { id, syms }.into(),
                    Some(Res::WsDiag(report)) => Msg::WsDiag { id, report }.into(),
                    Some(Res::TooLarge) => Msg::Response { id, result: Err(too_large(len)) }.into(),
                    None => Msg::Response { id, result: Ok(Value::Null) }.into(),
                }
            }
            (None, None) => Frame::Skipped,
        }
    }
}

/// The error delivered in place of a response that was too big to keep.
pub fn too_large(len: usize) -> Value {
    serde_json::json!({"code": -32001, "message": format!("response too large ({} MB), dropped", len / (1024 * 1024)), "data": {"tooLarge": true}})
}

struct Envelope<'a> {
    lim: &'a Limits,
    len: usize,
    policy_of: &'a dyn Fn(i64) -> Policy,
}

impl<'de> DeserializeSeed<'de> for Envelope<'_> {
    type Value = Parsed;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Parsed, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Envelope<'_> {
    type Value = Parsed;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON-RPC message")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Parsed, A::Error> {
        let mut p = Parsed::default();
        let small_enough = self.len <= self.lim.max_value;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "id" => p.id = Some(map.next_value()?),
                "method" => p.method = Some(map.next_value()?),
                "error" => p.error = Some(map.next_value()?),
                "params" => {
                    if small_enough {
                        p.params = Some(map.next_value()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                        p.dropped = true;
                    }
                }
                "result" => {
                    let policy = p.id.as_ref().and_then(Value::as_i64).map(self.policy_of).unwrap_or_default();
                    p.result = Some(match policy {
                        Policy::Symbols => map.next_value_seed(SymbolsSeed { lim: self.lim })?,
                        Policy::Tokens => map.next_value_seed(TokensSeed { lim: self.lim })?,
                        Policy::Folds => map.next_value_seed(FoldsSeed { max: self.lim.max_folds })?,
                        Policy::WsDiag => Res::WsDiag(map.next_value_seed(crate::problems::WsSeed)?),
                        Policy::Value if small_enough => Res::Value(map.next_value()?),
                        Policy::Value => {
                            map.next_value::<IgnoredAny>()?;
                            Res::TooLarge
                        }
                    });
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(p)
    }
}

// ───────────────────────────── semantic tokens ─────────────────────────────

struct TokensSeed<'a> {
    lim: &'a Limits,
}

impl<'de> DeserializeSeed<'de> for TokensSeed<'_> {
    type Value = Res;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Res, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TokensSeed<'_> {
    type Value = Res;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("semantic tokens")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Res, E> {
        Ok(Res::Value(Value::Null))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Res, A::Error> {
        let mut data = Vec::new();
        let mut overflow = false;
        while let Some(key) = map.next_key::<String>()? {
            if key == "data" {
                match map.next_value_seed(IntsSeed { max: self.lim.max_token_ints })? {
                    Some(v) => data = v,
                    None => overflow = true,
                }
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(if overflow { Res::TooLarge } else { Res::Tokens(data) })
    }
}

/// An array of integers packed as `u32`; `None` when it has more than `max` elements.
struct IntsSeed {
    max: usize,
}

impl<'de> DeserializeSeed<'de> for IntsSeed {
    type Value = Option<Vec<u32>>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for IntsSeed {
    type Value = Option<Vec<u32>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of integers")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out: Vec<u32> = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(self.max).min(1 << 16));
        let mut over = false;
        while let Some(n) = seq.next_element::<u32>()? {
            if out.len() < self.max {
                out.push(n);
            } else {
                over = true; // keep reading (to stay in sync) but stop storing
            }
        }
        Ok((!over).then_some(out))
    }
}

// ───────────────────────────── folding ranges ─────────────────────────────

struct FoldsSeed {
    max: usize,
}

impl<'de> DeserializeSeed<'de> for FoldsSeed {
    type Value = Res;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Res, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for FoldsSeed {
    type Value = Res;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("folding ranges")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Res, E> {
        Ok(Res::Value(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Res, A::Error> {
        let mut out = Vec::new();
        while let Some(r) = seq.next_element_seed(FoldItem)? {
            if out.len() < self.max {
                out.extend(r);
            }
        }
        Ok(Res::Value(Value::Array(out)))
    }
}

/// One `{startLine, endLine, ...}`, reduced to those two fields (None when either is missing).
struct FoldItem;

impl<'de> DeserializeSeed<'de> for FoldItem {
    type Value = Option<Value>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for FoldItem {
    type Value = Option<Value>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a folding range")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let (mut s, mut e) = (None, None);
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "startLine" => s = Some(map.next_value::<u64>()?),
                "endLine" => e = Some(map.next_value::<u64>()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(s.zip(e).map(|(s, e)| serde_json::json!({"startLine": s, "endLine": e})))
    }
}

// ───────────────────────────── symbols ─────────────────────────────

#[derive(Deserialize)]
struct Pos {
    line: u64,
    character: u64,
}

#[derive(Deserialize)]
struct Rng {
    start: Pos,
    end: Pos,
}

#[derive(Deserialize)]
struct Loc {
    range: Rng,
}

/// `documentSymbol` result (nested `DocumentSymbol[]` or flat `SymbolInformation[]`) flattened
/// in document order straight into [`Sym`]s: no `Value` is ever built.
struct SymbolsSeed<'a> {
    lim: &'a Limits,
}

impl<'de> DeserializeSeed<'de> for SymbolsSeed<'_> {
    type Value = Res;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Res, D::Error> {
        let out = RefCell::new(Vec::new());
        d.deserialize_any(SymList { depth: 0, lim: self.lim, out: &out })?;
        let mut syms = out.into_inner();
        syms.retain(|s| s.el != usize::MAX && !s.name.is_empty());
        Ok(Res::Symbols(syms))
    }
}

#[derive(Clone, Copy)]
struct SymList<'a> {
    depth: usize,
    lim: &'a Limits,
    out: &'a RefCell<Vec<Sym>>,
}

impl<'de> DeserializeSeed<'de> for SymList<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for SymList<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of symbols")
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(SymItem(self))?.is_some() {}
        Ok(())
    }
}

/// One symbol; its slot is reserved before its fields are read so the order stays pre-order.
struct SymItem<'a>(SymList<'a>);

impl<'de> DeserializeSeed<'de> for SymItem<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for SymItem<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a symbol")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let SymList { depth, lim, out } = self.0;
        let slot = {
            let mut o = out.borrow_mut();
            (o.len() < lim.max_symbols).then(|| {
                o.push(Sym { name: String::new(), kind: 0, depth, sl: 0, el: usize::MAX, sel_l: 0, sel_c: 0 });
                o.len() - 1
            })
        };
        let mut sel: Option<(usize, usize)> = None;
        while let Some(key) = map.next_key::<String>()? {
            let Some(i) = slot else {
                map.next_value::<IgnoredAny>()?;
                continue;
            };
            match key.as_str() {
                "name" => out.borrow_mut()[i].name = map.next_value()?,
                "kind" => out.borrow_mut()[i].kind = map.next_value::<u32>().unwrap_or(0),
                "range" | "location" => {
                    let r: Rng = if key == "range" { map.next_value()? } else { map.next_value::<Loc>()?.range };
                    let mut o = out.borrow_mut();
                    o[i].sl = r.start.line as usize;
                    o[i].el = r.end.line as usize;
                    if sel.is_none() {
                        o[i].sel_l = r.start.line as usize;
                        o[i].sel_c = r.start.character as usize;
                    }
                }
                "selectionRange" => {
                    let r: Rng = map.next_value()?;
                    let mut o = out.borrow_mut();
                    sel = Some((r.start.line as usize, r.start.character as usize));
                    o[i].sel_l = r.start.line as usize;
                    o[i].sel_c = r.start.character as usize;
                }
                "children" if depth + 1 < lim.symbol_levels => {
                    map.next_value_seed(SymList { depth: depth + 1, lim, out })?
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{cell::Cell, io::Cursor};

    // ── per-thread allocation accounting, to assert bounded memory deterministically ──

    thread_local! {
        static LIVE: Cell<i64> = const { Cell::new(0) };
        static PEAK: Cell<i64> = const { Cell::new(0) };
    }

    struct Counting;
    unsafe impl std::alloc::GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
            let _ = LIVE.try_with(|c| {
                let n = c.get() + l.size() as i64;
                c.set(n);
                let _ = PEAK.try_with(|p| p.set(p.get().max(n)));
            });
            unsafe { std::alloc::System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
            let _ = LIVE.try_with(|c| c.set(c.get() - l.size() as i64));
            unsafe { std::alloc::System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, new: usize) -> *mut u8 {
            let _ = LIVE.try_with(|c| {
                let n = c.get() + new as i64 - l.size() as i64;
                c.set(n);
                let _ = PEAK.try_with(|p| p.set(p.get().max(n)));
            });
            unsafe { std::alloc::System.realloc(p, l, new) }
        }
    }

    #[global_allocator]
    static A: Counting = Counting;

    /// Peak live bytes allocated by this thread while running `f` (relative to the start).
    fn peak_of<T>(f: impl FnOnce() -> T) -> (T, i64) {
        let base = LIVE.with(Cell::get);
        PEAK.with(|p| p.set(base));
        let out = f();
        (out, PEAK.with(Cell::get) - base)
    }

    fn framed(body: &str) -> Vec<u8> {
        let mut v = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        v.extend_from_slice(body.as_bytes());
        v
    }

    fn never(_: i64) -> Policy {
        Policy::Value
    }

    fn small_lim() -> Limits {
        Limits {
            stream_above: 64,
            max_value: 4096,
            max_symbols: 100,
            symbol_levels: 3,
            max_token_ints: 1000,
            max_folds: 50,
        }
    }

    fn next(r: &mut impl BufRead, lim: &Limits, pol: &dyn Fn(i64) -> Policy) -> Msg {
        match read_message(r, lim, pol).unwrap() {
            Frame::Msg(m) => m,
            Frame::Eof => panic!("eof"),
            Frame::Skipped => panic!("skipped"),
        }
    }

    /// A reader that hands out `n` bytes at a time, like a slow pipe.
    struct Trickle<R>(R, usize);
    impl<R: Read> Read for Trickle<R> {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = b.len().min(self.1);
            self.0.read(&mut b[..n])
        }
    }

    #[test]
    fn frames_split_across_chunks_and_back_to_back() {
        let mut all = crate::lsp::frame(&json!({"id": 1, "result": "ç"}));
        all.extend(crate::lsp::frame(&json!({"method": "x"})));
        let mut r = BufReader::with_capacity(16, Trickle(Cursor::new(all), 7));
        let lim = Limits::default();
        let Msg::Response { id: 1, result: Ok(v) } = next(&mut r, &lim, &never) else { panic!() };
        assert_eq!(v, "ç", "Content-Length counts bytes, not chars");
        assert!(matches!(next(&mut r, &lim, &never), Msg::Notification { .. }));
        assert!(matches!(read_message(&mut r, &lim, &never).unwrap(), Frame::Eof));
    }

    #[test]
    fn malformed_header_is_skipped_and_reading_resyncs() {
        let mut all = b"X-Junk: 1\r\n\r\n".to_vec();
        all.extend(crate::lsp::frame(&json!({"method": "ok"})));
        let mut r = Cursor::new(all);
        let lim = Limits::default();
        assert!(matches!(read_message(&mut r, &lim, &never).unwrap(), Frame::Skipped));
        assert!(matches!(next(&mut r, &lim, &never), Msg::Notification { .. }));
    }

    #[test]
    fn big_generic_response_is_streamed_into_a_value() {
        let body = json!({"jsonrpc": "2.0", "id": 7, "result": {"items": (0..50).collect::<Vec<_>>()}}).to_string();
        assert!(body.len() > 64);
        let mut bytes = framed(&body);
        bytes.extend(crate::lsp::frame(&json!({"method": "after"})));
        let mut r = Cursor::new(bytes);
        let lim = small_lim();
        let Msg::Response { id: 7, result: Ok(v) } = next(&mut r, &lim, &never) else { panic!() };
        assert_eq!(v["items"].as_array().unwrap().len(), 50);
        assert!(
            matches!(next(&mut r, &lim, &never), Msg::Notification { method, .. } if method == "after"),
            "framing stays in sync"
        );
    }

    #[test]
    fn oversized_generic_response_is_dropped_with_an_error_and_framing_survives() {
        let items: Vec<i32> = (0..5000).collect();
        let body = json!({"jsonrpc": "2.0", "id": 9, "result": items}).to_string();
        assert!(body.len() > small_lim().max_value);
        let mut bytes = framed(&body);
        bytes.extend(crate::lsp::frame(&json!({"id": 10, "result": 1})));
        let mut r = Cursor::new(bytes);
        let lim = small_lim();
        let Msg::Response { id: 9, result: Err(e) } = next(&mut r, &lim, &never) else {
            panic!("expected a synthesized error")
        };
        assert_eq!(e["data"]["tooLarge"], true);
        assert!(matches!(next(&mut r, &lim, &never), Msg::Response { id: 10, result: Ok(_) }));
    }

    #[test]
    fn oversized_notification_and_request_are_skipped() {
        let big = "x".repeat(8000);
        let n =
            json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"blob": big}}).to_string();
        let rq = json!({"jsonrpc": "2.0", "id": 3, "method": "workspace/configuration", "params": {"blob": big}})
            .to_string();
        let mut bytes = framed(&n);
        bytes.extend(framed(&rq));
        bytes.extend(crate::lsp::frame(&json!({"method": "small"})));
        let mut r = Cursor::new(bytes);
        let lim = small_lim();
        assert!(matches!(read_message(&mut r, &lim, &never).unwrap(), Frame::Skipped));
        assert!(matches!(read_message(&mut r, &lim, &never).unwrap(), Frame::Skipped));
        assert!(matches!(next(&mut r, &lim, &never), Msg::Notification { method, .. } if method == "small"));
    }

    #[test]
    fn error_responses_pass_through_when_streamed() {
        let body =
            json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32603, "message": "m".repeat(200)}}).to_string();
        let mut r = Cursor::new(framed(&body));
        let Msg::Response { id: 4, result: Err(e) } = next(&mut r, &small_lim(), &never) else { panic!() };
        assert_eq!(e["code"], -32603);
    }

    fn sym(i: usize, kids: Vec<Value>) -> Value {
        let line = i as u64;
        let r = json!({"start": {"line": line, "character": 0}, "end": {"line": line + 3, "character": 1}});
        json!({"name": format!("s{i}"), "kind": 12, "detail": "x".repeat(40), "range": r, "selectionRange": r, "children": kids})
    }

    fn sym_policy(id: i64) -> Policy {
        if id == 1 { Policy::Symbols } else { Policy::Value }
    }

    fn syms_of(m: Msg) -> Vec<Sym> {
        match m {
            Msg::Symbols { syms, .. } => syms,
            _ => panic!("expected Msg::Symbols"),
        }
    }

    #[test]
    fn symbol_tree_is_cut_to_three_levels() {
        let deep = sym(3, vec![sym(4, vec![sym(5, vec![sym(6, vec![])])])]);
        let top = vec![sym(0, vec![sym(1, vec![sym(2, vec![deep])])]), sym(100, vec![])];
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": top}).to_string();
        let mut r = Cursor::new(framed(&body));
        let syms = syms_of(next(&mut r, &small_lim(), &sym_policy));
        let got: Vec<(&str, usize)> = syms.iter().map(|s| (s.name.as_str(), s.depth)).collect();
        assert_eq!(got, vec![("s0", 0), ("s1", 1), ("s2", 2), ("s100", 0)], "levels 0..=2 kept, pre-order");
        assert_eq!((syms[1].sl, syms[1].el, syms[1].sel_l, syms[1].sel_c, syms[1].kind), (1, 4, 1, 0, 12));
    }

    #[test]
    fn streamed_and_small_symbol_paths_agree() {
        let top: Vec<Value> =
            (0..4).map(|i| sym(i * 10, vec![sym(i * 10 + 1, vec![sym(i * 10 + 2, vec![])])])).collect();
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": top}).to_string();
        let mut r = Cursor::new(framed(&body));
        let streamed = syms_of(next(&mut r, &small_lim(), &sym_policy));
        let mut r = Cursor::new(framed(&body));
        let whole = syms_of(next(&mut r, &Limits { stream_above: usize::MAX, ..small_lim() }, &sym_policy));
        assert_eq!(streamed, whole);
        assert_eq!(whole.len(), 12);
    }

    #[test]
    fn symbol_count_is_capped_and_flat_information_lists_work() {
        let flat: Vec<Value> = (0..500)
            .map(|i| json!({"name": format!("f{i}"), "kind": 12, "containerName": "c", "location": {"uri": "file:///a.ts", "range": {"start": {"line": i, "character": 2}, "end": {"line": i, "character": 5}}}}))
            .collect();
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": flat}).to_string();
        let mut r = Cursor::new(framed(&body));
        let syms = syms_of(next(&mut r, &small_lim(), &sym_policy));
        assert_eq!(syms.len(), 100, "max_symbols");
        assert_eq!(syms[7].name, "f7");
        assert_eq!((syms[7].sl, syms[7].sel_l, syms[7].sel_c), (7, 7, 2));
    }

    #[test]
    fn null_symbol_result_is_empty() {
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": null, "padding": "p".repeat(100)}).to_string();
        let mut r = Cursor::new(framed(&body));
        assert!(syms_of(next(&mut r, &small_lim(), &sym_policy)).is_empty());
    }

    #[test]
    fn folding_ranges_keep_only_their_lines_and_are_capped() {
        let ranges: Vec<Value> = (0..200)
            .map(|i| json!({"startLine": i, "endLine": i + 5, "startCharacter": 3, "kind": "region"}))
            .collect();
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": ranges}).to_string();
        let mut r = Cursor::new(framed(&body));
        let Msg::Response { result: Ok(v), .. } = next(&mut r, &small_lim(), &|_| Policy::Folds) else { panic!() };
        assert_eq!(v.as_array().unwrap().len(), 50, "max_folds");
        assert_eq!(v[3], json!({"startLine": 3, "endLine": 8}));
        assert_eq!(crate::rowmap::fold_regions_from_lsp(&v)[3], (3, 8));
    }

    fn tok_policy(_: i64) -> Policy {
        Policy::Tokens
    }

    #[test]
    fn semantic_tokens_are_packed_whether_small_or_streamed() {
        let small = json!({"jsonrpc": "2.0", "id": 5, "result": {"data": [0, 1, 2, 3, 4]}}).to_string();
        let big =
            json!({"jsonrpc": "2.0", "id": 6, "result": {"resultId": "r", "data": (0..300u32).collect::<Vec<_>>()}})
                .to_string();
        let mut bytes = framed(&small);
        bytes.extend(framed(&big));
        bytes.extend(framed(&json!({"jsonrpc": "2.0", "id": 8, "result": null}).to_string()));
        let mut r = Cursor::new(bytes);
        let lim = Limits { stream_above: 64, ..small_lim() };
        let Msg::Tokens { id: 5, data } = next(&mut r, &lim, &tok_policy) else { panic!("small path") };
        assert_eq!(data, vec![0, 1, 2, 3, 4]);
        let Msg::Tokens { id: 6, data } = next(&mut r, &lim, &tok_policy) else { panic!("streamed path") };
        assert_eq!(data.len(), 300);
        assert_eq!(data[299], 299);
        // A null result (no tokens) stays a plain response.
        let framed_null =
            framed(&json!({"jsonrpc": "2.0", "id": 8, "result": null, "pad": "q".repeat(100)}).to_string());
        let mut r = Cursor::new(framed_null);
        assert!(matches!(next(&mut r, &lim, &tok_policy), Msg::Response { id: 8, result: Ok(Value::Null) }));
    }

    #[test]
    fn too_many_semantic_tokens_become_an_error_instead_of_memory() {
        let big = json!({"jsonrpc": "2.0", "id": 6, "result": {"data": (0..2000u32).collect::<Vec<_>>()}}).to_string();
        let mut r = Cursor::new(framed(&big));
        let Msg::Response { id: 6, result: Err(e) } = next(&mut r, &small_lim(), &tok_policy) else { panic!() };
        assert_eq!(e["data"]["tooLarge"], true);
    }

    /// Generates a documentSymbol response of `n` top-level classes with `m` methods each,
    /// without ever materializing it (it is read through `Read`).
    struct SymGen {
        n: usize,
        m: usize,
        i: usize,
        pending: Vec<u8>,
        pos: usize,
        stage: u8,
        total: usize,
    }

    impl SymGen {
        fn class_json(i: usize, m: usize) -> String {
            let r = |l: usize| {
                format!(r#"{{"start":{{"line":{l},"character":0}},"end":{{"line":{},"character":1}}}}"#, l + 20)
            };
            let methods: Vec<String> = (0..m)
                .map(|k| format!(r#"{{"name":"method{k}","kind":6,"detail":"(a: number) => string","range":{r},"selectionRange":{r},"children":[{{"name":"inner","kind":13,"range":{r},"selectionRange":{r},"children":[{{"name":"deep","kind":13,"range":{r},"selectionRange":{r},"children":[]}}]}}]}}"#, r = r(i * 30 + k)))
                .collect();
            format!(
                r#"{{"name":"Class{i}","kind":5,"range":{r},"selectionRange":{r},"children":[{}]}}"#,
                methods.join(","),
                r = r(i * 30)
            )
        }
    }

    impl Read for SymGen {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            while self.pos >= self.pending.len() {
                self.pos = 0;
                self.pending.clear();
                match self.stage {
                    0 => {
                        self.pending.extend_from_slice(br#"{"jsonrpc":"2.0","id":1,"result":["#);
                        self.stage = 1;
                    }
                    1 if self.i < self.n => {
                        if self.i > 0 {
                            self.pending.push(b',');
                        }
                        self.pending.extend_from_slice(Self::class_json(self.i, self.m).as_bytes());
                        self.i += 1;
                    }
                    1 => {
                        self.pending.extend_from_slice(b"]}");
                        self.stage = 2;
                    }
                    _ => return Ok(0),
                }
                self.total += self.pending.len();
            }
            let k = out.len().min(self.pending.len() - self.pos);
            out[..k].copy_from_slice(&self.pending[self.pos..self.pos + k]);
            self.pos += k;
            Ok(k)
        }
    }

    /// The header must announce the length up front, so measure a generator first.
    fn gen_len(n: usize, m: usize) -> usize {
        let mut g = SymGen { n, m, i: 0, pending: vec![], pos: 0, stage: 0, total: 0 };
        io::copy(&mut g, &mut io::sink()).unwrap();
        g.total
    }

    #[test]
    fn a_hundred_megabyte_symbol_response_is_parsed_in_bounded_memory() {
        // ~100 MB of JSON: 6000 classes x 60 methods, each with a nested child chain.
        let (n, m) = (3500, 60);
        let len = gen_len(n, m);
        assert!(len > 90 * 1024 * 1024, "the synthetic response is really big: {len}");
        let header = format!("Content-Length: {len}\r\n\r\n").into_bytes();
        let src = SymGen { n, m, i: 0, pending: vec![], pos: 0, stage: 0, total: 0 };
        let mut r = BufReader::with_capacity(64 * 1024, Cursor::new(header).chain(src));
        let lim = Limits::default();
        let (frame, peak) = peak_of(|| read_message(&mut r, &lim, &sym_policy).unwrap());
        let Frame::Msg(Msg::Symbols { syms, .. }) = frame else { panic!() };
        let kept = syms.len();
        assert_eq!(kept, lim.max_symbols, "capped");
        assert!(syms.iter().all(|s| s.depth <= 2));
        let mb = peak as f64 / 1048576.0;
        eprintln!("[stream] {} MB response -> {kept} symbols kept, peak heap {mb:.1} MB", len / 1048576);
        assert!(peak < 40 * 1024 * 1024, "peak {mb:.1} MB: memory must not scale with the response size");
        // Reading the next frame still works: the whole body was consumed.
        let tail = crate::lsp::frame(&json!({"method": "tail"}));
        drop(r);
        let mut r = Cursor::new(tail);
        assert!(matches!(next(&mut r, &lim, &sym_policy), Msg::Notification { .. }));
    }

    /// `{"jsonrpc":"2.0","id":2,"result":[1234567,1234567,...]}` with `n` numbers, produced on the fly.
    struct Numbers {
        head: &'static [u8],
        n: usize,
        i: usize,
        buf: Vec<u8>,
        pos: usize,
        total: usize,
    }

    impl Numbers {
        fn new(n: usize) -> Self {
            Numbers { head: br#"{"jsonrpc":"2.0","id":2,"result":["#, n, i: 0, buf: Vec::new(), pos: 0, total: 0 }
        }
        fn len(n: usize) -> usize {
            let mut g = Numbers::new(n);
            io::copy(&mut g, &mut io::sink()).unwrap();
            g.total
        }
    }

    impl Read for Numbers {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.buf.len() {
                self.pos = 0;
                self.buf.clear();
                if self.i == 0 {
                    self.buf.extend_from_slice(self.head);
                }
                if self.i < self.n {
                    for _ in 0..1000.min(self.n - self.i) {
                        if self.i > 0 {
                            self.buf.push(b',');
                        }
                        self.buf.extend_from_slice(b"1234567");
                        self.i += 1;
                    }
                } else if self.i == self.n {
                    self.buf.extend_from_slice(b"]}");
                    self.i += 1;
                } else {
                    return Ok(0);
                }
                self.total += self.buf.len();
            }
            let k = out.len().min(self.buf.len() - self.pos);
            out[..k].copy_from_slice(&self.buf[self.pos..self.pos + k]);
            self.pos += k;
            Ok(k)
        }
    }

    #[test]
    fn a_huge_generic_response_is_skipped_without_buffering_it() {
        // ~50 MB of numbers behind the default value cap, generated lazily.
        let n = 6_000_000;
        let len = Numbers::len(n);
        assert!(len > 40 * 1024 * 1024);
        let header = format!("Content-Length: {len}\r\n\r\n").into_bytes();
        let mut r = BufReader::with_capacity(64 * 1024, Cursor::new(header).chain(Numbers::new(n)));
        let lim = Limits::default();
        let (frame, peak) = peak_of(|| read_message(&mut r, &lim, &never).unwrap());
        let Frame::Msg(Msg::Response { id: 2, result: Err(e) }) = frame else { panic!() };
        assert_eq!(e["data"]["tooLarge"], true);
        assert!(peak < 1024 * 1024, "peak {peak} bytes");
    }

    fn ws_body(files: usize, per_file: usize) -> String {
        let d = json!({"range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 4}}, "severity": 2, "message": "w"});
        let items: Vec<Value> = (0..files)
            .map(|f| json!({"uri": format!("file:///p/f{f}.rs"), "kind": "full", "resultId": "r", "items": vec![d.clone(); per_file]}))
            .collect();
        json!({"jsonrpc": "2.0", "id": 7, "result": {"items": items}}).to_string()
    }

    #[test]
    fn workspace_diagnostics_come_capped_on_both_paths() {
        let ws = |_: i64| Policy::WsDiag;
        // Small: parsed as a Value, then converted.
        let mut r = Cursor::new(framed(&ws_body(2, 3)));
        let Msg::WsDiag { id: 7, report } = next(&mut r, &Limits::default(), &ws) else { panic!("expected WsDiag") };
        assert_eq!(report.files.len(), 2);
        assert_eq!(report.files[1].diags.len(), 3);
        // Big (well past `max_value`): streamed, never a Value, and capped.
        let body = ws_body(60, crate::problems::FILE_CAP + 50);
        assert!(body.len() > small_lim().max_value);
        let mut r = Cursor::new(framed(&body));
        let Msg::WsDiag { report, .. } = next(&mut r, &small_lim(), &ws) else { panic!("expected WsDiag") };
        assert_eq!(report.files.len(), 60);
        assert!(report.files.iter().all(|f| f.diags.len() == crate::problems::FILE_CAP));
        assert_eq!(report.dropped, 60 * 50);
        // An error answer is still an error.
        let err = json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32800, "message": "cancelled"}}).to_string();
        let mut r = Cursor::new(framed(&err));
        assert!(matches!(next(&mut r, &Limits::default(), &ws), Msg::Response { id: 7, result: Err(_) }));
    }

    #[test]
    fn partial_workspace_results_are_parsed_on_the_reader_thread() {
        let value = json!({"items": [{"uri": "file:///p/a.rs", "kind": "full", "items": [
            {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "message": "e"}]}]});
        let body =
            json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "lumen-wsdiag:3", "value": value}});
        let mut r = Cursor::new(framed(&body.to_string()));
        let Msg::WsPartial { token, report } = next(&mut r, &Limits::default(), &never) else {
            panic!("expected WsPartial")
        };
        assert_eq!(token, "lumen-wsdiag:3");
        assert_eq!(report.files[0].diags[0].message, "e");
        // Other progress (work done) is left alone.
        let other =
            json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "end"}}});
        let mut r = Cursor::new(framed(&other.to_string()));
        assert!(matches!(next(&mut r, &Limits::default(), &never), Msg::Notification { .. }));
    }
}
