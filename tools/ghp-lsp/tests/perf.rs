//! Performance guards on large inputs.
//!
//! These are not benchmarks: the bounds are generous (they hold in an unoptimized `cargo test`
//! build on a slow CI runner) and only exist to catch complexity regressions. An accidentally
//! quadratic pass over a 5 MB page takes minutes to hours, not seconds.

use std::time::{Duration, Instant};

use ghp_lsp::ast::Span;
use ghp_lsp::parser::parse;
use ghp_lsp::server::Server;
use serde_json::{Value, json};

/// Size of the "large file" of these tests.
const BIG: usize = 5 * 1024 * 1024;

/// Per operation. A linear pass takes a fraction of this even in a debug build.
const LIMIT: Duration = Duration::from_secs(60);

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let out = f();
    let took = t.elapsed();
    eprintln!("perf: {what:<44} {:>8.1} ms", took.as_secs_f64() * 1000.0);
    assert!(took < LIMIT, "{what} took {took:?} (limit {LIMIT:?})");
    (out, took)
}

/// A realistic page repeated up to `size` bytes: markup, `if` / `elif` / `else`, `for`, echoes.
fn big_page(size: usize) -> String {
    const HEAD: &str = "<go:import (\"fmt\")/>\n<go\n    total := 0\n    items := []string{\"a\", \"b\"}\n/>\n";
    const ROW: &str = "<div class=\"row\">\n    <go:if (total > 3)/>\n        <b><go= total /></b>\n    <go:elif (total < 0)/>\n        <i>neg</i>\n    <go:else/>\n        <u>zero</u>\n    <go:endif/>\n    <go:for i, v := range items/>\n        <li><go= fmt.Sprint(i, v) /></li>\n    <go:endfor/>\n</div>\n";
    let mut s = String::with_capacity(size + ROW.len());
    s.push_str(HEAD);
    while s.len() < size {
        s.push_str(ROW);
    }
    s
}

fn repeat_to(unit: &str, size: usize) -> String {
    unit.repeat(size / unit.len() + 1)
}

fn check_tiles(src: &str) {
    let p = parse(src);
    assert_eq!(p.tree.len, src.len());
    let segs = p.tree.segments();
    assert_eq!(segs.last().map(|s| s.span().end), Some(src.len()));
}

#[test]
fn parse_five_megabytes_of_page() {
    let src = big_page(BIG);
    assert!(src.len() >= BIG);
    let (p, _) = timed("parse 5 MB page", || parse(&src));
    assert!(p.diagnostics.is_empty(), "{:?}", &p.diagnostics[..p.diagnostics.len().min(3)]);
    assert_eq!(p.tree.len, src.len());
}

/// Thousands of broken tags, each one used to make the lexer walk to the end of the file.
#[test]
fn parse_pathological_inputs_in_linear_time() {
    let size = 3 * 1024 * 1024;
    let cases: &[(&str, &str)] = &[
        ("unterminated double quote per tag", "<go= \"x\n"),
        ("quote kinds that never pair up", "<go= '\"/>\n"),
        ("unterminated backtick per tag", "<go= `x\n"),
        ("mixed quotes across tags", "<go= \"a\n<go= 'a\n<go= `a\n<go= \"a\n<go= 'b\n"),
        ("unterminated comment per tag", "<go /* \n"),
        ("unclosed paren per tag", "<go:if (\n"),
        ("raw string with closer", "<go= `a/>\n"),
        ("escaped quote at end of line", "<go= \"a\\\n"),
        ("line comments", "<go= // x\n"),
        ("apostrophes in tags", "<go x := 'a\n"),
        ("stray closers", "<go:endif/>\n"),
        ("deep openers", "<go:if a/>\n"),
        ("loose heads", "<go:\n"),
        ("bare lt", "<<<<<<<<\n"),
    ];
    for (name, unit) in cases {
        let src = repeat_to(unit, size);
        timed(name, || check_tiles(&src));
    }
}

/// The same units, each followed by one properly closed tag: recovery must not depend on the
/// broken tags being the last thing in the file.
#[test]
fn parse_pathological_inputs_followed_by_valid_tags() {
    let size = 2 * 1024 * 1024;
    for unit in ["<go= \"x\n<go= y />\n", "<go /* \n<go= y />\n", "<go= `x\n<go= y />\n", "<go:if (\n<go= y />\n"] {
        let src = repeat_to(unit, size);
        timed(&format!("{:?}", unit.lines().next().unwrap()), || check_tiles(&src));
    }
}

fn req(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn pos(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

fn open(uri: &str, text: &str) -> Server {
    let mut s = Server::new();
    s.handle(req(1, "initialize", json!({"capabilities": {}})));
    let (out, _) = timed("didOpen (parse, scope, diagnostics)", || {
        s.handle(json!({"jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": uri, "languageId": "ghp", "version": 1, "text": text}}}))
    });
    assert_eq!(out[0]["method"], "textDocument/publishDiagnostics");
    s
}

fn ask(s: &mut Server, id: i64, what: &str, method: &str, uri: &str, extra: Value) -> Value {
    let mut params = json!({"textDocument": {"uri": uri}});
    for (k, v) in extra.as_object().unwrap() {
        params[k] = v.clone();
    }
    let (out, _) = timed(what, || s.handle(req(id, method, params)));
    assert!(out[0].get("error").is_none(), "{method}: {}", out[0]);
    out[0]["result"].clone()
}

/// Every whole-document request on a 5 MB page, then the incremental ones.
#[test]
fn full_document_operations_on_a_five_megabyte_page() {
    let uri = "file:///perf/page.ghp";
    let text = big_page(BIG);
    let last_line = text.matches('\n').count() as u32;
    let mut s = open(uri, &text);

    let syms = ask(&mut s, 2, "documentSymbol", "textDocument/documentSymbol", uri, json!({}));
    assert!(syms.as_array().unwrap().len() > 1000);
    let folds = ask(&mut s, 3, "foldingRange", "textDocument/foldingRange", uri, json!({}));
    assert!(folds.as_array().unwrap().len() > 1000);
    let opts = json!({"options": {"tabSize": 4, "insertSpaces": true}});
    let fmt = ask(&mut s, 4, "formatting", "textDocument/formatting", uri, opts);
    assert!(fmt.is_array());

    let full = ask(&mut s, 5, "semanticTokens/full", "textDocument/semanticTokens/full", uri, json!({}));
    let data = full["data"].as_array().unwrap().len();
    assert!(data > 5 * 1000);
    let id = full["resultId"].clone();
    let delta = ask(
        &mut s,
        6,
        "semanticTokens/delta (unchanged)",
        "textDocument/semanticTokens/full/delta",
        uri,
        json!({"previousResultId": id}),
    );
    assert_eq!(delta["edits"], json!([]));
    let range = json!({"range": {"start": pos(10, 0), "end": pos(60, 0)}});
    let r = ask(&mut s, 7, "semanticTokens/range", "textDocument/semanticTokens/range", uri, range);
    let n = r["data"].as_array().unwrap().len();
    assert!(n > 0 && n < data, "{n} of {data}");

    let at_end = json!({"position": pos(last_line - 2, 6)});
    ask(&mut s, 8, "hover (tag docs, end of file)", "textDocument/hover", uri, at_end.clone());
    ask(&mut s, 9, "completion (end of file)", "textDocument/completion", uri, at_end);
    let in_go = json!({"position": pos(last_line - 3, 20)});
    ask(&mut s, 10, "completion (Go expression, end of file)", "textDocument/completion", uri, in_go);
    ask(&mut s, 11, "documentHighlight", "textDocument/documentHighlight", uri, json!({"position": pos(6, 6)}));
    ask(
        &mut s,
        12,
        "selectionRange",
        "textDocument/selectionRange",
        uri,
        json!({"positions": [pos(6, 6), pos(last_line / 2, 3)]}),
    );

    // One keystroke in the middle of the file, then the debounced republish.
    let edit = json!({"textDocument": {"uri": uri, "version": 2},
        "contentChanges": [{"range": {"start": pos(last_line / 2, 0), "end": pos(last_line / 2, 0)}, "text": " "}]});
    timed("didChange (one keystroke)", || {
        s.handle(json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": edit}))
    });
    let (out, _) = timed("republish diagnostics", || s.flush_all());
    assert_eq!(out[0]["params"]["version"], 2);
}

/// One enormous line of wide characters: UTF-16 position conversion must not scan the line.
#[test]
fn single_huge_line_of_wide_characters() {
    let uri = "file:///perf/line.ghp";
    let text = repeat_to("😀<go= x />é", 2 * 1024 * 1024);
    let mut s = open(uri, &text);
    let full = ask(&mut s, 2, "semanticTokens/full (one line)", "textDocument/semanticTokens/full", uri, json!({}));
    assert!(full["data"].as_array().unwrap().len() > 5 * 1000);
    ask(&mut s, 3, "foldingRange (one line)", "textDocument/foldingRange", uri, json!({}));
    ask(&mut s, 4, "documentSymbol (one line)", "textDocument/documentSymbol", uri, json!({}));
}

#[test]
fn spans_of_a_big_page_stay_in_bounds() {
    let src = big_page(BIG / 4);
    let p = parse(&src);
    let ok = |s: Span| s.start <= s.end && s.end <= src.len() && src.is_char_boundary(s.start);
    assert!(p.tree.tags().iter().all(|t| ok(t.span) && ok(t.inner)));
}
