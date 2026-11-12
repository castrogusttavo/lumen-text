//! The real `ghp-lsp` binary against a real gopls on the demo module: the first start happens in
//! the background, completion works inside an empty `<go= />` and after `user.`, and gopls'
//! diagnostics arrive on their own, without an edit.
//!
//! Ignored by default (needs `gopls` and `go` on PATH); CI runs it with `--ignored`.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Client, WAIT, pos};
use serde_json::{Value, json};

fn demo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/demo")
}

fn uri_of(rel: &str) -> String {
    format!("file://{}", demo().join(rel).display())
}

/// LSP position of `delta` bytes after the first `needle` (ASCII text).
fn after(text: &str, needle: &str, delta: usize) -> Value {
    let off = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not found")) + delta;
    let line = text[..off].matches('\n').count();
    let col = off - text[..off].rfind('\n').map_or(0, |i| i + 1);
    pos(line as u32, col as u32)
}

fn labels(r: &Value) -> Vec<String> {
    let list = &r["result"];
    let items = if list.is_array() { list } else { &list["items"] };
    items
        .as_array()
        .map(|a| a.iter().filter_map(|i| i["label"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// Ask for completion until `want` is among the labels (gopls needs a while to load the module).
fn complete_until(c: &mut Client, uri: &str, at: &Value, want: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let r = c.call("textDocument/completion", json!({"textDocument": {"uri": uri}, "position": at}));
        assert!(r.get("error").is_none(), "{r}");
        let got = labels(&r);
        if got.iter().any(|l| l == want) {
            return got;
        }
        assert!(Instant::now() < deadline, "no {want:?} in completion at {at}: {got:?}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[test]
#[ignore = "needs gopls on PATH"]
fn completion_and_late_diagnostics_through_the_real_binary() {
    let mut c = Client::start(&[]);
    c.handshake();

    let uri = uri_of("pages/home.ghp");
    let mut text = std::fs::read_to_string(demo().join("pages/home.ghp")).unwrap();
    text.push_str("\n<p><go= /></p>\n<p><go= user.Na /></p>\n<a href=\"<go= user.Em />\">x</a>\n");
    c.open(&uri, &text);
    c.wait_for("textDocument/publishDiagnostics", |_| true, WAIT).expect("initial diagnostics");

    // Member completion right after the dot (gopls filters by what precedes the cursor): only gopls
    // can do it, so this also proves gopls has started.
    let got = complete_until(&mut c, &uri, &after(&text, "user.Na", 5), "Name");
    assert!(got.contains(&"Admin".to_string()) && got.contains(&"Greeting".to_string()), "{got:?}");
    // The same inside an attribute value.
    let got = complete_until(&mut c, &uri, &after(&text, "user.Em", 5), "Email");
    assert!(got.contains(&"Name".to_string()), "{got:?}");

    // The cursor inside an empty tag is a Go position: the names in scope come back.
    let got = complete_until(&mut c, &uri, &after(&text, "<p><go= /></p>", 7), "user");
    assert!(got.contains(&"items".to_string()), "{got:?}");

    // Hover on a Go expression is gopls' too.
    let h = c.call(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri}, "position": after(&text, "<go= user.Name", 12)}),
    );
    assert!(h["result"].to_string().contains("Name"), "{h}");

    // A page with a Go error: the diagnostics come from gopls without any didChange.
    let broken = uri_of("pages/broken.ghp");
    let broken_text = std::fs::read_to_string(demo().join("pages/broken.ghp")).unwrap();
    c.open(&broken, &broken_text);
    let d = c
        .wait_for(
            "textDocument/publishDiagnostics",
            |p| {
                p["uri"] == broken.as_str()
                    && p["diagnostics"].as_array().is_some_and(|d| d.iter().any(|d| d["source"] == "gopls"))
            },
            Duration::from_secs(120),
        )
        .expect("gopls diagnostics for the broken page");
    let msgs: Vec<String> =
        d["diagnostics"].as_array().unwrap().iter().filter_map(|d| d["message"].as_str().map(str::to_owned)).collect();
    assert!(msgs.iter().any(|m| m.contains("undefinedThing")), "{msgs:?}");
    assert_eq!(c.finish(), Some(0));
}
