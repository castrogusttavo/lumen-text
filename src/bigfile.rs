//! Making big files first-class: semantic tokens by window and a cheap outline source.
//!
//! * Above [`BIG_LINES`] lines the whole-document `semanticTokens/full` (millions of integers) is
//!   never requested. Instead [`SemWin`] tracks the window around the viewport for which tokens were
//!   requested with `semanticTokens/range`, asks for a new one when the view nears the edge
//!   (debounced), and [`merge_range`] keeps the per-line cache bounded.
//! * [`indent_chain`] derives the breadcrumb chain of a line from indentation alone (scanning up
//!   to a bounded distance), used while the server's outline is missing or was trimmed.

use crate::{
    app::App,
    buffer::Buffer,
    doc::Sym,
    lsp::Pending,
};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::HashMap,
    time::{Duration, Instant},
};

/// Files with more lines than this use windowed semantic tokens and a trimmed outline.
pub const BIG_LINES: usize = 20_000;
/// Lines requested above and below the viewport.
pub const SEM_MARGIN: usize = 200;
/// A new window is requested when the view comes within this many lines of the covered edge.
pub const SEM_SLACK: usize = 60;
/// Most lines with semantic tokens kept in the cache.
pub const SEM_MAX_CACHED: usize = 3000;
/// An unanswered range request older than this is considered lost.
pub const SEM_INFLIGHT_TTL: Duration = Duration::from_secs(10);
/// Debounce between the view leaving the covered window and the new request.
pub const SEM_DEBOUNCE: Duration = Duration::from_millis(120);
/// Lines scanned upward when computing an indentation chain.
pub const CHAIN_MAX_BACK: usize = 20_000;

pub type LineToks = Vec<(usize, usize, u8, bool)>;
pub type SemMap = HashMap<usize, LineToks>;

/// Window bookkeeping for `semanticTokens/range` on one document.
#[derive(Default)]
pub struct SemWin {
    /// Lines covered by the last applied response.
    pub cover: Option<(usize, usize)>,
    /// (first, last, document version, sent at) of the request in flight.
    pub inflight: Option<(usize, usize, i32, Instant)>,
    /// When the next request is due (debounce).
    pub due: Option<Instant>,
}

impl SemWin {
    /// Lines to request for a viewport `vf..=vl` of a document with `count` lines.
    pub fn window(vf: usize, vl: usize, count: usize) -> (usize, usize) {
        (vf.saturating_sub(SEM_MARGIN), (vl + SEM_MARGIN).min(count.saturating_sub(1)))
    }

    /// Does the viewport need a new window? True when none was applied or the view nears its edge.
    pub fn needs(&self, vf: usize, vl: usize, count: usize) -> bool {
        let Some((a, b)) = self.cover else { return true };
        let lo_ok = a == 0 || vf >= a + SEM_SLACK;
        let hi_ok = b + 1 >= count || vl + SEM_SLACK <= b;
        !(lo_ok && hi_ok && vf >= a && vl <= b)
    }

    /// Forget everything about the window (the text changed, the server restarted...).
    pub fn reset(&mut self) {
        *self = SemWin::default();
    }

    pub fn inflight_alive(&self, now: Instant) -> bool {
        self.inflight.is_some_and(|(_, _, _, t)| now.duration_since(t) < SEM_INFLIGHT_TTL)
    }
}

/// Apply a range response covering lines `a..=b` to the per-line cache: the described lines are
/// replaced, `reset` also forgets the rest (edits made it stale), and the cache stays within
/// [`SEM_MAX_CACHED`] lines by dropping the ones farthest from the window.
pub fn merge_range(cache: &mut SemMap, new: SemMap, (a, b): (usize, usize), reset: bool) {
    if reset {
        cache.clear();
    } else {
        cache.retain(|l, _| *l < a || *l > b);
    }
    cache.extend(new);
    if cache.len() > SEM_MAX_CACHED {
        let mid = (a + b) / 2;
        let mut by_dist: Vec<(usize, usize)> = cache.keys().map(|&l| (l.abs_diff(mid), l)).collect();
        by_dist.sort_unstable();
        for &(_, l) in by_dist.iter().skip(SEM_MAX_CACHED) {
            cache.remove(&l);
        }
    }
}

// ───────────────────────────── indentation chain ─────────────────────────────

/// Per-document state for big files, held by `Doc::big`.
#[derive(Default)]
pub struct Big {
    pub semwin: SemWin,
    chain: RefCell<Option<((usize, i32), Vec<Sym>)>>,
}

impl Big {
    /// [`indent_chain`] of `line`, memoized per (line, document version): breadcrumbs ask on every frame.
    pub fn chain(&self, buf: &Buffer, line: usize, version: i32) -> Vec<Sym> {
        if let Some((key, c)) = self.chain.borrow().as_ref() {
            if *key == (line, version) {
                return c.clone();
            }
        }
        let c = indent_chain(buf, line, CHAIN_MAX_BACK);
        *self.chain.borrow_mut() = Some(((line, version), c.clone()));
        c
    }
}

fn indent_cols(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// Enclosing scopes of `line` found by walking up to lines with smaller indentation (at most
/// `max_back` lines), outermost first. Only lines that look like declarations count.
pub fn indent_chain(buf: &Buffer, line: usize, max_back: usize) -> Vec<Sym> {
    let n = buf.line_count();
    if line >= n {
        return Vec::new();
    }
    let cur = buf.line(line);
    // A blank line takes the indentation of the next code line below it.
    let mut indent = if cur.trim().is_empty() {
        (line + 1..n.min(line + 200)).map(|k| buf.line(k)).find(|t| !t.trim().is_empty()).map_or(0, |t| indent_cols(&t))
    } else {
        indent_cols(&cur)
    };
    let mut out: Vec<Sym> = Vec::new();
    let (mut l, mut scanned) = (line, 0usize);
    while l > 0 && indent > 0 && scanned < max_back {
        l -= 1;
        scanned += 1;
        let t = buf.line(l);
        let body = t.trim_start();
        // Blank lines, comments and closers say nothing about the scope we are in.
        if body.is_empty() || body.starts_with("//") || body.starts_with("/*") || body.starts_with('*') || body.starts_with(['}', ')', ']', '#']) {
            continue;
        }
        let ind = indent_cols(&t);
        if ind < indent {
            indent = ind;
            if let Some((name, kind, off)) = header_name(body) {
                let lead = t.len() - body.len();
                let sel_c = t[..lead + off].encode_utf16().count();
                out.push(Sym { name, kind, depth: 0, sl: l, el: n.saturating_sub(1), sel_l: l, sel_c });
            }
        }
    }
    out.reverse();
    for (i, s) in out.iter_mut().enumerate() {
        s.depth = i;
    }
    out
}

const MODIFIERS: &[&str] = &["export", "default", "declare", "abstract", "async", "public", "private", "protected", "static", "readonly", "override", "get", "set", "pub", "unsafe", "final"];
const NOT_METHODS: &[&str] = &["if", "for", "while", "switch", "catch", "return", "else", "do", "try", "with", "new", "typeof", "await", "super", "function", "elif", "match", "loop"];

fn ident_len(s: &str) -> usize {
    s.char_indices().find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '$')).map_or(s.len(), |(i, _)| i)
}

/// (name, LSP symbol kind, byte offset of the name in `body`) when `body` (a line without its
/// indentation) looks like the start of a declaration that opens a block.
fn header_name(body: &str) -> Option<(String, u32, usize)> {
    let opens = body.trim_end().ends_with(['{', '(', ':', '[']) || body.trim_end().ends_with("=>");
    if !opens {
        return None;
    }
    let mut rest = body;
    let mut off = 0usize;
    // Leading modifiers ("export default async ...").
    loop {
        let w = &rest[..ident_len(rest)];
        if !w.is_empty() && MODIFIERS.contains(&w) && rest[w.len()..].starts_with(' ') {
            let skip = w.len() + (rest[w.len()..].len() - rest[w.len()..].trim_start().len());
            // `get(`/`set(`/`static(` are method names, only skip real modifiers followed by a word.
            if rest[skip..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '*') {
                rest = &rest[skip..];
                off += skip;
                continue;
            }
        }
        break;
    }
    let kw_end = ident_len(rest);
    let kw = &rest[..kw_end];
    let kind = match kw {
        "class" | "struct" | "trait" | "impl" => Some(5),
        "interface" => Some(11),
        "enum" => Some(10),
        "namespace" | "module" | "mod" => Some(3),
        "function" | "fn" | "def" => Some(12),
        "type" => Some(26),
        "const" | "let" | "var" => Some(13),
        _ => None,
    };
    if let Some(kind) = kind {
        let after = rest[kw_end..].trim_start_matches([' ', '*']);
        let skipped = rest.len() - after.len();
        let n = ident_len(after);
        if n == 0 {
            return None;
        }
        // Variables only count when they hold a function or object ("const f = (…) => {").
        if kind == 13 && !(body.contains("=>") || body.contains("function") || body.trim_end().ends_with('{')) {
            return None;
        }
        return Some((after[..n].to_string(), if kind == 13 && body.contains("=>") { 12 } else { kind }, off + skipped));
    }
    // Method / constructor: `name(...) {`, `name<T>(...) {`, `*gen(...) {`.
    let start = rest.trim_start_matches('*');
    let lead = rest.len() - start.len();
    let n = ident_len(start);
    if n == 0 || NOT_METHODS.contains(&&start[..n]) {
        return None;
    }
    let tail = &start[n..];
    if tail.starts_with('(') || tail.starts_with('<') || tail.starts_with("?(") {
        let kind = if &start[..n] == "constructor" { 9 } else { 6 };
        return Some((start[..n].to_string(), kind, off + lead));
    }
    None
}

// ───────────────────────────── App glue ─────────────────────────────

impl App {
    /// Keep a `semanticTokens/range` window around the viewport of document `i` (a big file),
    /// debounced while scrolling. Called from `lsp_tick` for the active document.
    pub fn sem_range_tick(&mut self, i: usize, li: usize, now: Instant, want: &mut impl FnMut(Instant)) {
        let d = &mut self.docs[i];
        let count = d.buf.line_count();
        let (vf, vl) = d.visible_line_span();
        if let Some((_, _, _, sent)) = d.big.semwin.inflight {
            if d.big.semwin.inflight_alive(now) {
                want(sent + SEM_INFLIGHT_TTL); // retry if the answer never comes
                return;
            }
        }
        if d.big.semwin.due.is_none() && d.big.semwin.needs(vf, vl, count) {
            d.big.semwin.due = Some(now + SEM_DEBOUNCE);
        }
        let Some(t) = d.big.semwin.due else { return };
        if t > now {
            want(t);
            return;
        }
        d.big.semwin.due = None;
        let Some(uri) = d.uri() else { return };
        let (a, b) = SemWin::window(vf, vl, count);
        let end_col: usize = d.buf.line(b).chars().map(char::len_utf16).sum();
        let version = d.version;
        d.big.semwin.inflight = Some((a, b, version, now));
        let params = json!({"textDocument": {"uri": uri}, "range": {"start": {"line": a, "character": 0}, "end": {"line": b, "character": end_col}}});
        self.lsps[li].request("textDocument/semanticTokens/range", params, Pending::SemRange { uri, version, first: a, last: b });
    }

    /// Semantic tokens arrived (`full` or `range`), already packed by the reader thread.
    pub fn on_tokens(&mut self, li: usize, what: Pending, data: Vec<u32>) {
        let legend = self.lsps[li].caps["semanticTokensProvider"]["legend"].clone();
        let names = |k: &str| legend[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
        match what {
            Pending::SemTokens { uri } => {
                if let Some(i) = self.doc_by_uri(&uri) {
                    let d = &mut self.docs[i];
                    d.sem = crate::doc::decode_semantic(&data, &names("tokenTypes"), &names("tokenModifiers"));
                    d.sem_valid_until = usize::MAX;
                    self.request_redraw();
                }
            }
            Pending::SemRange { uri, version, first, last } => {
                if let Some(i) = self.doc_by_uri(&uri) {
                    let d = &mut self.docs[i];
                    let took = d.big.semwin.inflight.map(|x| x.3.elapsed());
                    d.big.semwin.inflight = None;
                    if d.version != version {
                        return; // edited meanwhile: the edit scheduled a fresh request
                    }
                    let toks = crate::doc::decode_semantic(&data, &names("tokenTypes"), &names("tokenModifiers"));
                    let stale = d.sem_valid_until != usize::MAX;
                    merge_range(&mut d.sem, toks, (first, last), stale);
                    d.sem_valid_until = usize::MAX;
                    d.big.semwin.cover = Some((first, last));
                    if std::env::var_os("LUMEN_DEBUG").is_some() {
                        eprintln!("[lsp] semantic range {first}..={last}: {} ints, cache {} lines, {:?}", data.len(), d.sem.len(), took);
                    }
                    self.request_redraw();
                }
            }
            _ => {}
        }
    }

    /// A range request failed or was dropped (too large): do not ask for the same window again.
    pub fn sem_range_failed(&mut self, what: &Pending) {
        if let Pending::SemRange { uri, first, last, .. } = what {
            if let Some(i) = self.doc_by_uri(uri) {
                let w = &mut self.docs[i].big.semwin;
                w.inflight = None;
                w.cover = Some((*first, *last));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_covers_viewport_with_margin_and_clamps() {
        assert_eq!(SemWin::window(1000, 1060, 100_000), (800, 1260));
        assert_eq!(SemWin::window(10, 70, 100_000), (0, 270));
        assert_eq!(SemWin::window(99_990, 99_999, 100_000), (99_790, 99_999));
    }

    #[test]
    fn refresh_is_requested_before_the_edge_shows() {
        let mut w = SemWin::default();
        assert!(w.needs(1000, 1060, 100_000), "nothing applied yet");
        w.cover = Some(SemWin::window(1000, 1060, 100_000));
        assert!(!w.needs(1000, 1060, 100_000));
        assert!(!w.needs(900, 960, 100_000), "small scrolls reuse the window");
        assert!(w.needs(850, 910, 100_000), "within SEM_SLACK of the top edge");
        assert!(w.needs(1150, 1210, 100_000), "within SEM_SLACK of the bottom edge");
        assert!(w.needs(5000, 5060, 100_000), "jumped far away");
        // Edges at the file boundaries never ask for more.
        w.cover = Some(SemWin::window(0, 60, 100_000));
        assert!(!w.needs(0, 60, 100_000));
        w.cover = Some(SemWin::window(99_940, 99_999, 100_000));
        assert!(!w.needs(99_940, 99_999, 100_000));
    }

    #[test]
    fn stale_inflight_requests_expire() {
        let mut w = SemWin::default();
        let t0 = Instant::now();
        w.inflight = Some((0, 10, 1, t0));
        assert!(w.inflight_alive(t0));
        assert!(!w.inflight_alive(t0 + SEM_INFLIGHT_TTL + Duration::from_secs(1)));
    }

    fn toks(l: usize) -> (usize, LineToks) {
        (l, vec![(0, 3, 9, false)])
    }

    #[test]
    fn merge_replaces_the_window_and_keeps_neighbours() {
        let mut cache: SemMap = (0..10).map(toks).collect();
        let new: SemMap = [toks(5), toks(6)].into_iter().collect();
        merge_range(&mut cache, new, (4, 8), false);
        let mut keys: Vec<_> = cache.keys().copied().collect();
        keys.sort();
        assert_eq!(keys, vec![0, 1, 2, 3, 5, 6, 9], "lines 4..=8 are described by the response; 7 and 8 had no tokens");
        merge_range(&mut cache, [toks(50)].into_iter().collect(), (48, 52), true);
        assert_eq!(cache.len(), 1, "reset drops stale lines");
    }

    #[test]
    fn cache_stays_bounded_around_the_window() {
        let mut cache: SemMap = SemMap::new();
        // Walk down a 100k-line file window by window, like scrolling.
        for start in (0..100_000).step_by(400) {
            let (a, b) = (start, start + 459);
            let new: SemMap = (a..=b).map(toks).collect();
            merge_range(&mut cache, new, (a, b), false);
            assert!(cache.len() <= SEM_MAX_CACHED, "{} lines cached at {start}", cache.len());
        }
        assert!(cache.contains_key(&99_800) || cache.keys().any(|&l| l > 99_000), "the latest window is kept");
        assert!(!cache.contains_key(&10), "far-away lines were evicted");
    }

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    const SRC: &str = "\
export class Service<T> extends Base {
  private cache = new Map()

  async fetch(id: string): Promise<T> {
    if (id) {
      const run = async (x: number) => {
        return x
      }
    }
    return null
  }

  constructor(a: number) {
    this.a = a
  }
}

export function top(a: number) {
  return a
}

const handler = (req: Request) => {
  // comment
  done()
}
";

    fn names(c: &[Sym]) -> Vec<&str> {
        c.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn chain_follows_indentation_and_names_declarations() {
        let b = buf(SRC);
        let c = indent_chain(&b, 6, 1000); // `return x`
        assert_eq!(names(&c), vec!["Service", "fetch", "run"], "{c:?}");
        assert_eq!(c[0].depth, 0);
        assert_eq!(c[2].depth, 2);
        assert_eq!(c[1].sl, 3);
        assert_eq!(c[1].sel_c, 8, "column of the name after `async `");
        assert_eq!(c[0].kind, 5);
        assert_eq!(c[2].kind, 12, "arrow function assigned to a const is a function");
    }

    #[test]
    fn chain_handles_blank_lines_closers_and_top_level() {
        let b = buf(SRC);
        assert_eq!(names(&indent_chain(&b, 13, 1000)), vec!["Service", "constructor"]);
        assert!(indent_chain(&b, 16, 1000).is_empty(), "column 0 has no parent");
        assert_eq!(names(&indent_chain(&b, 18, 1000)), vec!["top"]);
        assert_eq!(names(&indent_chain(&b, 23, 1000)), vec!["handler"], "comment lines are ignored");
        assert!(indent_chain(&b, 999, 1000).is_empty(), "out of range");
    }

    #[test]
    fn chain_scan_is_bounded() {
        let mut s = String::from("class Far {\n");
        for _ in 0..500 {
            s.push_str("  x()\n");
        }
        let b = buf(&s);
        assert_eq!(names(&indent_chain(&b, 400, 1000)), vec!["Far"]);
        assert!(indent_chain(&b, 400, 100).is_empty(), "gave up after max_back lines");
    }

    #[test]
    fn chain_is_memoized_per_line_and_version() {
        let big = Big::default();
        let b = buf(SRC);
        let a = big.chain(&b, 6, 1);
        assert_eq!(names(&a), vec!["Service", "fetch", "run"]);
        let b2 = buf("x\n");
        // Same key: the cached answer is returned without looking at the buffer again.
        assert_eq!(names(&big.chain(&b2, 6, 1)), vec!["Service", "fetch", "run"]);
        assert!(big.chain(&b2, 6, 2).is_empty(), "a new version recomputes");
    }

    #[test]
    fn header_detection_skips_statements() {
        assert_eq!(header_name("if (a) {"), None);
        assert_eq!(header_name("for (const x of y) {"), None);
        assert_eq!(header_name("return foo({"), None);
        assert_eq!(header_name("foo(bar)"), None, "does not open a block");
        assert_eq!(header_name("interface Props {").map(|h| (h.0, h.1)), Some(("Props".into(), 11)));
        assert_eq!(header_name("export default async function* gen(a) {").map(|h| h.0), Some("gen".into()));
        assert_eq!(header_name("static get value() {").map(|h| h.0), Some("value".into()));
        assert_eq!(header_name("get(key: string) {").map(|h| h.0), Some("get".into()), "a method called get");
        assert_eq!(header_name("class A:").map(|h| h.0), Some("A".into()));
        assert_eq!(header_name("const x = 1 {"), Some(("x".into(), 13, 6)));
    }
}
