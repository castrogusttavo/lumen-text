//! End to end against a real gopls on the demo module in `tests/demo`.
//!
//! Ignored by default (needs `gopls` and `go` on PATH); CI runs it with `--ignored`.

use ghp_lsp::gopls::adapter::regions_of;
use ghp_lsp::gopls::sourcemap::TextIndex;
use ghp_lsp::gopls::{Bridge, CancelToken, Config, Event, Position};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn demo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/demo")
}

fn open(b: &Bridge, rel: &str) -> (PathBuf, String) {
    let path = demo().join(rel);
    let src = std::fs::read_to_string(&path).unwrap();
    b.update(&path, &src, &regions_of(&src));
    (path, src)
}

/// Position `delta` bytes after the first occurrence of `needle`.
fn pos_after(src: &str, needle: &str, delta: usize) -> Position {
    TextIndex::new(src).position(src.find(needle).unwrap() + delta)
}

#[test]
#[ignore = "needs gopls on PATH"]
fn hover_completion_definition_and_diagnostics() {
    let (b, rx) = Bridge::new(Config::default());
    assert!(b.gopls_available(), "gopls not found on PATH");
    let cancel = CancelToken::default();
    let (home, src) = open(&b, "pages/home.ghp");
    let (broken, _) = open(&b, "pages/broken.ghp");

    // Hover on `Greeting` in `<go= user.Greeting() />`.
    let hover = b.hover(&home, pos_after(&src, "user.Greeting", 6), &cancel).unwrap().expect("hover");
    assert!(hover.to_string().contains("Greeting"), "{hover}");

    // Completion right after `user.`.
    let completion = b.completion(&home, pos_after(&src, "user.Greeting", 5), &cancel).unwrap().expect("completion");
    let labels = completion.to_string();
    assert!(labels.contains("Greeting") && labels.contains("Email"), "{labels}");

    // Definition of a model method lives in a real .go file.
    let def = b.definition(&home, pos_after(&src, "user.Greeting", 6), &cancel).unwrap().expect("definition");
    assert!(def.to_string().contains("models/user.go"), "{def}");

    // Definition of a variable declared in a code block maps back into the page itself.
    let def = b.definition(&home, pos_after(&src, "<go= user.Name", 6), &cancel).unwrap().expect("definition");
    let loc = if def.is_array() { def[0].clone() } else { def };
    assert!(loc["uri"].as_str().unwrap().ends_with("pages/home.ghp"), "{loc}");
    assert_eq!(loc["range"]["start"]["line"], 3, "{loc}");

    // A position in plain HTML is not Go: no request is made.
    let html = pos_after(&src, "<!doctype", 3);
    assert!(b.hover(&home, html, &cancel).unwrap().is_none());

    // Diagnostics: errors land on the offending expressions, scaffolding noise is gone.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut diags = Vec::new();
    while Instant::now() < deadline {
        diags = b.diagnostics(&broken);
        if diags.len() >= 2 {
            break;
        }
        let _ = rx.recv_timeout(Duration::from_millis(250));
    }
    let msgs: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(msgs.iter().any(|m| m.contains("undefined: undefinedThing")), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("n.Nope undefined")), "{msgs:?}");
    assert!(diags.iter().all(|d| d.range.start.line >= 3), "{diags:?}");

    // The healthy page has no diagnostics.
    std::thread::sleep(Duration::from_secs(1));
    assert!(b.diagnostics(&home).is_empty(), "{:?}", b.diagnostics(&home));

    b.close(&home);
    b.close(&broken);
    assert!(rx.try_iter().all(|e| !matches!(e, Event::Status(_))));
}
