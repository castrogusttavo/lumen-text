//! The router against a real gopls on the demo module in `tests/demo`: loops, switches, imports,
//! struct fields, incremental edits, crash + restart, give-up, cancellation and timeout.
//!
//! Ignored by default (needs `gopls` and `go` on PATH); CI runs it with `--ignored`.

use ghp_lsp::gopls::{CancelToken, Config, Event, ProxyError};
use ghp_lsp::line_index::{LineIndex, Position, Range};
use ghp_lsp::router::{ContentChange, RequestKind, Router};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

fn demo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/demo")
}

fn open(r: &Router, rel: &str) -> (PathBuf, String) {
    let path = demo().join(rel);
    let src = std::fs::read_to_string(&path).unwrap();
    r.open(&path, src.clone());
    (path, src)
}

/// Position `delta` bytes after the first occurrence of `needle`.
fn pos_after(src: &str, needle: &str, delta: usize) -> Position {
    LineIndex::new(src).position(src.find(needle).unwrap_or_else(|| panic!("{needle:?} not found")) + delta)
}

fn line_of(src: &str, needle: &str) -> u32 {
    src[..src.find(needle).unwrap()].matches('\n').count() as u32
}

fn ask(r: &Router, path: &Path, pos: Position, kind: RequestKind) -> Option<Value> {
    r.request(path, pos, kind, &CancelToken::default()).unwrap_or_else(|e| panic!("{kind:?}: {e}"))
}

fn labels(completion: &Value) -> Vec<String> {
    let items = if completion.is_array() { completion } else { &completion["items"] };
    items.as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
}

/// Poll `f` until it yields a value (gopls answers asynchronously).
fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn go_messages(r: &Router, path: &Path) -> Vec<String> {
    r.go_diagnostics(path).into_iter().map(|d| d.message).collect()
}

/// Events collected as they arrive, so a test can wait for a specific one.
struct Events {
    rx: Receiver<Event>,
    seen: Vec<Event>,
}

impl Events {
    fn new(r: &Router) -> Events {
        Events { rx: r.take_events().unwrap(), seen: Vec::new() }
    }

    /// Wait for (and consume) the first event satisfying `pred`.
    fn wait(&mut self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(i) = self.seen.iter().position(&pred) {
                return self.seen.remove(i);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}; saw {:?}", self.seen);
            if let Ok(e) = self.rx.recv_timeout(left.min(Duration::from_millis(500))) {
                self.seen.push(e);
            }
        }
    }

    fn drain(&mut self) {
        self.seen.extend(self.rx.try_iter());
    }
}

#[test]
#[ignore = "needs gopls on PATH"]
fn loops_switches_and_struct_fields() {
    let r = Router::new(demo());
    assert!(r.gopls_available(), "gopls not found on PATH");

    // Loops: range variables, the three-clause form and map iteration.
    let (loops, src) = open(&r, "pages/loops.ghp");
    let hover = ask(&r, &loops, pos_after(&src, "member.Name", 2), RequestKind::Hover).expect("hover on range var");
    assert!(hover.to_string().contains("User"), "{hover}");
    let c = ask(&r, &loops, pos_after(&src, "member.Name", 7), RequestKind::Completion).expect("member completion");
    let got = labels(&c);
    assert!(got.contains(&"Email".to_string()) && got.contains(&"Admin".to_string()), "{got:?}");
    let hover = ask(&r, &loops, pos_after(&src, "<go= v ", 5), RequestKind::Hover).expect("hover on map value");
    assert!(hover.to_string().contains("int"), "{hover}");
    let def = ask(&r, &loops, pos_after(&src, "member.Name", 2), RequestKind::Definition).expect("definition");
    let loc = if def.is_array() { def[0].clone() } else { def };
    assert!(loc["uri"].as_str().unwrap().ends_with("pages/loops.ghp"), "{loc}");
    assert_eq!(loc["range"]["start"]["line"], line_of(&src, "<go:for i, member"), "{loc}");
    let refs = ask(&r, &loops, pos_after(&src, "member.Name", 2), RequestKind::References).expect("references");
    assert!(refs.as_array().unwrap().len() >= 2, "{refs}");

    // Struct fields through a chain, pointer receivers and a raw string holding `/>`.
    let (fields, src) = open(&r, "pages/fields.ghp");
    let c = ask(&r, &fields, pos_after(&src, "team.Home.City", 10), RequestKind::Completion).expect("completion");
    let got = labels(&c);
    assert!(got.contains(&"City".to_string()) && got.contains(&"Zip".to_string()), "{got:?}");
    let hover = ask(&r, &fields, pos_after(&src, "lead.Name", 6), RequestKind::Hover).expect("hover");
    assert!(hover.to_string().contains("Name"), "{hover}");

    // Switches: tag expression, case values and the tagless form.
    let (switches, src) = open(&r, "pages/switches.ghp");
    let hover = ask(&r, &switches, pos_after(&src, "<go:switch level", 13), RequestKind::Hover).expect("hover");
    assert!(hover.to_string().contains("int"), "{hover}");
    let c = ask(&r, &switches, pos_after(&src, "u.Name", 2), RequestKind::Completion).expect("completion");
    assert!(labels(&c).contains(&"Greeting".to_string()));

    // Markup is not routed.
    assert!(ask(&r, &switches, pos_after(&src, "<p>big", 4), RequestKind::Hover).is_none());

    // None of these healthy pages has a diagnostic, neither from gopls nor from the parser.
    std::thread::sleep(Duration::from_secs(2));
    for page in [&loops, &fields, &switches] {
        assert!(r.diagnostics(page).is_empty(), "{}: {:?}", page.display(), r.diagnostics(page));
    }
}

#[test]
#[ignore = "needs gopls on PATH"]
fn imports_and_incremental_edits() {
    let r = Router::new(demo());
    let (page, src) = open(&r, "pages/imports.ghp");

    // gopls reports the unused import, on the import line of the page.
    let diags = eventually("unused import diagnostic", || {
        let d = r.go_diagnostics(&page);
        d.iter().any(|d| d.message.contains("imported and not used")).then_some(d)
    });
    let unused: Vec<_> = diags.iter().filter(|d| d.message.contains("imported and not used")).collect();
    assert_eq!(unused.len(), 1, "{diags:?}");
    assert_eq!(unused[0].range.start.line, line_of(&src, "\"example.com/demo/models\""));
    assert_eq!(unused[0].source, "gopls");

    // An aliased import works for hover.
    let hover = ask(&r, &page, pos_after(&src, "str.ToUpper", 6), RequestKind::Hover).expect("hover");
    assert!(hover.to_string().contains("ToUpper"), "{hover}");

    // Incremental edit 1: delete the unused import line.
    let models_line = line_of(&src, "\"example.com/demo/models\"");
    let pos = |line, character| Position { line, character };
    r.change(
        &page,
        &[ContentChange {
            range: Some(Range { start: pos(models_line, 0), end: pos(models_line + 1, 0) }),
            text: String::new(),
        }],
    );
    eventually("diagnostics to clear", || go_messages(&r, &page).is_empty().then_some(()));

    // Incremental edit 2: replace two whole lines (several tags) with one broken expression.
    let text = r.text(&page).unwrap();
    let first = line_of(&text, "<p><go= str.ToUpper");
    let last_len = text.lines().nth(first as usize + 1).unwrap().chars().count() as u32;
    let original: String = text.lines().skip(first as usize).take(2).collect::<Vec<_>>().join("\n");
    let range = Range { start: pos(first, 0), end: pos(first + 1, last_len) };
    r.change(&page, &[ContentChange { range: Some(range), text: "<p><go= undefinedName /></p>".into() }]);
    eventually("undefined name", || {
        go_messages(&r, &page).iter().any(|m| m.contains("undefined: undefinedName")).then_some(())
    });

    // Incremental edit 3: put the original lines back.
    let end = pos(first, "<p><go= undefinedName /></p>".chars().count() as u32);
    r.change(&page, &[ContentChange { range: Some(Range { start: pos(first, 0), end }), text: original }]);
    eventually("healthy again", || go_messages(&r, &page).is_empty().then_some(()));

    // Edits are real edits of the page, not of a copy: the router's text agrees.
    assert!(r.text(&page).unwrap().contains("fmt.Sprint(1)"));
}

#[test]
#[ignore = "needs gopls on PATH"]
fn crash_and_restart_replays_open_pages() {
    let r = Router::new(demo());
    let mut events = Events::new(&r);
    let (home, src) = open(&r, "pages/home.ghp");
    let at = pos_after(&src, "user.Greeting", 6);
    assert!(ask(&r, &home, at, RequestKind::Hover).is_some());

    r.bridge().kill_server_for_test(&home);
    events.wait("crash", |e| matches!(e, Event::Crashed(_)));

    // The next request restarts gopls, replays the page and answers as before.
    let hover = ask(&r, &home, at, RequestKind::Hover).expect("hover after restart");
    assert!(hover.to_string().contains("Greeting"), "{hover}");
    events.wait("restart", |e| matches!(e, Event::Restarted(_)));

    // Edits after a restart go on top of the replayed text, not of the stale one.
    let line = line_of(&src, "user.Greeting");
    let col = LineIndex::new(&src).position(src.find("user.Greeting").unwrap()).character + 5;
    let pos = |character| Position { line, character };
    r.change(
        &home,
        &[ContentChange { range: Some(Range { start: pos(col), end: pos(col + 8) }), text: "Missing".into() }],
    );
    eventually("diagnostic after restart", || {
        go_messages(&r, &home).iter().any(|m| m.contains("Missing")).then_some(())
    });
    events.drain();
    assert!(r.bridge().is_active(&home));
}

#[test]
#[ignore = "needs gopls on PATH"]
fn repeated_crashes_give_up_and_fall_back_to_scope_analysis() {
    let cfg = Config { max_restarts: 1, ..Config::default() };
    let r = Router::with_config(demo(), cfg);
    let mut events = Events::new(&r);
    let (home, src) = open(&r, "pages/home.ghp");
    let local = pos_after(&src, "<go= user.Name", 7);
    let hover = ask(&r, &home, local, RequestKind::Hover).expect("hover");
    assert!(hover.to_string().contains("User"), "{hover}");

    r.bridge().kill_server_for_test(&home);
    events.wait("first crash", |e| matches!(e, Event::Crashed(_)));
    assert!(ask(&r, &home, local, RequestKind::Hover).is_some());
    events.wait("restart", |e| matches!(e, Event::Restarted(_)));

    r.bridge().kill_server_for_test(&home);
    events.wait("second crash", |e| matches!(e, Event::Crashed(_)));

    // Over the limit: the bridge gives up with exactly one status message ...
    let hover = ask(&r, &home, local, RequestKind::Hover).expect("fallback hover");
    events.wait("give-up status", |e| matches!(e, Event::Status(s) if s.contains("keeps crashing")));
    assert!(!r.bridge().is_active(&home));
    // ... and local names are still answered, from the page's own scope analysis.
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(value.contains("user := models.User"), "{hover}");
    let c = ask(&r, &home, pos_after(&src, "<go= user.Name", 5), RequestKind::Completion).expect("fallback completion");
    assert!(labels(&c).contains(&"user".to_string()) && labels(&c).contains(&"items".to_string()));
    events.drain();
    assert!(!events.seen.iter().any(|e| matches!(e, Event::Status(_))), "only one status: {:?}", events.seen);
}

#[test]
#[ignore = "needs gopls on PATH"]
fn cancellation_and_timeout() {
    let r = Router::new(demo());
    let (home, src) = open(&r, "pages/home.ghp");
    let at = pos_after(&src, "user.Greeting", 6);
    assert!(ask(&r, &home, at, RequestKind::Hover).is_some());

    // A request cancelled before (or while) it runs ends in `Cancelled` ...
    let token = CancelToken::default();
    token.cancel();
    assert!(matches!(r.request(&home, at, RequestKind::Hover, &token), Err(ProxyError::Cancelled)));
    // ... and the next one is unaffected.
    assert!(ask(&r, &home, at, RequestKind::Completion).is_some());

    // A request that outlives its deadline ends in `Timeout`; gopls stays up.
    let cfg = Config { request_timeout: Duration::ZERO, ..Config::default() };
    let slow = Router::with_config(demo(), cfg);
    let mut events = Events::new(&slow);
    let (home, _) = open(&slow, "pages/home.ghp");
    let res = slow.request(&home, at, RequestKind::Hover, &CancelToken::default());
    assert!(matches!(res, Err(ProxyError::Timeout)), "{res:?}");
    let res = slow.request(&home, at, RequestKind::Completion, &CancelToken::default());
    assert!(matches!(res, Err(ProxyError::Timeout)), "{res:?}");
    assert!(slow.bridge().is_active(&home));
    events.drain();
    assert!(!events.seen.iter().any(|e| matches!(e, Event::Status(_) | Event::Crashed(_))), "{:?}", events.seen);
}
