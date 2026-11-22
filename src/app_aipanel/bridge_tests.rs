//! The IDE bridge against the real server: a fake Claude Code over TCP, and a stub `claude` the
//! panel starts (environment, `--settings`, hooks driving the status chip).

use super::*;
use crate::{
    app_gitx::DiffBtn,
    app_unsaved::testkit::{lock, test_app, tmp},
    ide::{MAX_MESSAGE, ws},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    sync::mpsc,
};
use winit::keyboard::{ModifiersState, NamedKey};

/// Claude Code's side of the WebSocket: masks its frames, reads ours on a thread.
struct Fake {
    s: TcpStream,
    rx: mpsc::Receiver<Value>,
    stash: Vec<Value>,
    next_id: u64,
}

impl Fake {
    fn connect(port: u16, token: &str) -> Fake {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: x3JJHMbDL1EzLkh9GBhXDw==\r\nSec-WebSocket-Version: 13\r\n\
             X-Claude-Code-Ide-Authorization: {token}\r\n\r\n"
        );
        s.write_all(req.as_bytes()).unwrap();
        // The head byte by byte: nothing past it may be eaten.
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).unwrap();
            head.push(b[0]);
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        assert!(head.contains("HSmrc0sMlYUkAGmm5OPpG2HaGWk="), "accept key: {head}");
        let (tx, rx) = mpsc::channel();
        let mut r = s.try_clone().unwrap();
        std::thread::spawn(move || {
            while let Ok(f) = ws::read_frame(&mut r, MAX_MESSAGE, false) {
                if f.op != ws::OP_TEXT {
                    continue;
                }
                let Ok(v) = serde_json::from_slice::<Value>(&f.payload) else { continue };
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Fake { s, rx, stash: Vec::new(), next_id: 100 }
    }

    fn send(&mut self, v: Value) {
        let f = ws::encode(ws::OP_TEXT, v.to_string().as_bytes(), Some([3, 1, 4, 1]));
        self.s.write_all(&f).unwrap();
    }

    /// Pump the app until a message matching `want` arrives (others wait in the stash).
    fn wait(&mut self, app: &mut App, what: &str, want: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(i) = self.stash.iter().position(&want) {
                return self.stash.remove(i);
            }
            assert!(Instant::now() < end, "timed out waiting for {what}; got {:?}", self.stash);
            app.pump_ide();
            app.ide_tick(Instant::now(), &mut |_| {});
            if let Ok(v) = self.rx.recv_timeout(Duration::from_millis(10)) {
                self.stash.push(v);
            }
        }
    }

    fn reply(&mut self, app: &mut App, id: u64) -> Value {
        self.wait(app, &format!("the reply to {id}"), |v| v["id"] == json!(id))
    }

    fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// `tools/call`: its result.
    fn call(&mut self, app: &mut App, name: &str, args: Value) -> Value {
        let id = self.request("tools/call", json!({"name": name, "arguments": args}));
        self.reply(app, id)["result"].clone()
    }

    /// The JSON inside a tool result's text.
    fn call_json(&mut self, app: &mut App, name: &str, args: Value) -> Value {
        let r = self.call(app, name, args);
        serde_json::from_str(r["content"][0]["text"].as_str().unwrap_or("null")).unwrap()
    }
}

fn text_of(result: &Value, i: usize) -> String {
    result["content"][i]["text"].as_str().unwrap_or_default().to_string()
}

/// Pump until `done` holds (or time runs out).
fn pump_until(app: &mut App, done: impl Fn(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done(app) && Instant::now() < end {
        app.pump_ide();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn press(app: &mut App, k: NamedKey, mods: ModifiersState) {
    app.mods = mods;
    app.handle_key(&KeyIn { key: Key::Named(k), text: None });
    app.mods = ModifiersState::empty();
}

#[test]
fn ide_bridge_end_to_end_with_a_fake_claude() {
    let _g = lock();
    let dir = tmp("ide-e2e");
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    let locks = dir.join("claude-config").join("ide");
    app.ide_start_in(locks.clone());
    let (port, token) = {
        let s = app.ai.bridge.server.as_ref().expect("the bridge listens");
        (s.port(), s.token().to_string())
    };
    assert!(locks.join(format!("{port}.lock")).exists(), "lock file published");
    let mut c = Fake::connect(port, &token);
    // The handshake, as Claude Code does it.
    let id = c.request("initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}}));
    assert_eq!(c.reply(&mut app, id)["result"]["serverInfo"]["name"], "lumen-text");
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    c.send(json!({"jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": 1}}));
    let id = c.request("tools/list", json!({}));
    assert!(c.reply(&mut app, id)["result"]["tools"].as_array().is_some_and(|t| t.len() >= 11));
    assert!(app.ai.bridge.connected(), "the client counts once its handshake is through");

    // Workspace, editors, selection (UTF-16 columns past an emoji).
    let wsf = c.call_json(&mut app, "getWorkspaceFolders", json!({}));
    assert_eq!(wsf["success"], true);
    assert_eq!(wsf["rootPath"].as_str().map(std::path::PathBuf::from), Some(app.root.clone()));
    assert_eq!(wsf["folders"][0]["path"], wsf["rootPath"]);
    let u = dir.join("u.txt");
    std::fs::write(&u, "a😀bc\nsecond line\n").unwrap();
    let ui = app.open_path(&u, None).unwrap();
    {
        let d = &mut app.docs[ui];
        d.ed.select_range(&mut d.buf, 2, 4); // "bc"
    }
    let ed = c.call_json(&mut app, "getOpenEditors", json!({}));
    let tabs = ed["tabs"].as_array().unwrap();
    assert!(tabs.iter().any(|t| t["label"] == "seed.txt" && t["isActive"] == false), "{tabs:?}");
    assert!(tabs.iter().any(|t| t["label"] == "u.txt" && t["isActive"] == true && t["isDirty"] == false));
    let sel = c.call_json(&mut app, "getCurrentSelection", json!({}));
    assert_eq!(sel["text"], "bc");
    assert_eq!(sel["selection"]["start"], json!({"line": 0, "character": 3}), "a + a surrogate pair");
    assert_eq!(sel["selection"]["end"], json!({"line": 0, "character": 5}));
    assert_eq!(sel["selection"]["isEmpty"], false);
    assert!(sel["filePath"].as_str().is_some_and(|p| p.ends_with("u.txt")));
    // The selection is also pushed, debounced, to the connected agent.
    let note =
        c.wait(&mut app, "selection_changed", |v| v["method"] == "selection_changed" && v["params"]["text"] == "bc");
    assert!(note["params"]["fileUrl"].as_str().is_some_and(|u| u.starts_with("file://")));
    assert!(note["params"].get("success").is_none());
    assert_eq!(c.call_json(&mut app, "getLatestSelection", json!({}))["text"], "bc");
    // "AI: Send Selection to Agent": an @-mention of the selected lines (0-based, last included).
    {
        let d = &mut app.docs[ui];
        let end = d.buf.pos(1, 3);
        d.ed.select_range(&mut d.buf, 0, end);
    }
    app.run_bound("AiSendSelection");
    let at = c.wait(&mut app, "at_mentioned", |v| v["method"] == "at_mentioned");
    assert_eq!((at["params"]["lineStart"].clone(), at["params"]["lineEnd"].clone()), (json!(0), json!(1)));
    assert!(at["params"]["filePath"].as_str().is_some_and(|p| p.ends_with("u.txt")));

    // Diagnostics: what the open documents show.
    app.docs[ui].set_source_diags(
        false,
        vec![crate::doc::Diag {
            sl: 1,
            sc: 0,
            el: 1,
            ec: 6,
            severity: 1,
            message: "bad thing".into(),
            code: "E1".into(),
            source: String::new(),
        }],
    );
    let all = c.call_json(&mut app, "getDiagnostics", json!({}));
    let entry =
        all.as_array().unwrap().iter().find(|e| e["uri"].as_str().is_some_and(|u| u.ends_with("u.txt"))).cloned();
    let entry = entry.expect("u.txt has diagnostics");
    assert_eq!(entry["diagnostics"][0]["message"], "bad thing");
    assert_eq!(entry["diagnostics"][0]["severity"], "Error");
    assert_eq!(entry["diagnostics"][0]["code"], "E1");
    assert_eq!(entry["diagnostics"][0]["range"]["end"], json!({"line": 1, "character": 6}));
    let uri = crate::doc::path_to_uri(app.docs[ui].path().unwrap());
    let one = c.call_json(&mut app, "getDiagnostics", json!({"uri": uri}));
    assert_eq!(one.as_array().map(Vec::len), Some(1));

    // Dirty check and save.
    app.docs[ui].replace_text("changed\n");
    let up = app.docs[ui].path().unwrap().to_string_lossy().into_owned();
    assert_eq!(c.call_json(&mut app, "checkDocumentDirty", json!({"filePath": up}))["isDirty"], true);
    assert_eq!(c.call_json(&mut app, "saveDocument", json!({"filePath": up}))["saved"], true);
    assert_eq!(std::fs::read_to_string(&u).unwrap(), "changed\n");
    assert_eq!(c.call_json(&mut app, "checkDocumentDirty", json!({"filePath": "/nope/x"}))["success"], false);

    // openFile selects from startText to endText.
    let g = dir.join("g.txt");
    std::fs::write(&g, "alpha\nbeta gamma\ndelta\n").unwrap();
    let r = c.call(&mut app, "openFile", json!({"filePath": g, "startText": "beta", "endText": "gamma"}));
    assert!(text_of(&r, 0).starts_with("Opened file:"), "{r}");
    let d = &app.docs[app.active];
    assert_eq!((d.name.as_str(), d.ed.selected_text(&d.buf).as_str()), ("g.txt", "beta gamma"));
    assert_eq!(c.call(&mut app, "executeCode", json!({"code": "1"}))["isError"], true);

    // openDiff blocks until the user decides. Accept with the header's pill.
    let f = dir.join("f.txt");
    std::fs::write(&f, "one\ntwo\n").unwrap();
    let diff = |tab: &str, text: &str| {
        json!({"name": "openDiff", "arguments": {
            "old_file_path": f, "new_file_path": f, "new_file_contents": text, "tab_name": tab}})
    };
    let showing = |app: &App| app.diff.as_ref().is_some_and(|d| d.proposal.is_some());
    let d1 = c.request("tools/call", diff("t1", "one\nTWO\n"));
    pump_until(&mut app, showing);
    assert!(showing(&app), "the proposal is on screen");
    std::thread::sleep(Duration::from_millis(50));
    while let Ok(v) = c.rx.try_recv() {
        c.stash.push(v);
    }
    assert!(!c.stash.iter().any(|v| v["id"] == json!(d1)), "no answer before the user decides");
    let (w, h) = (1200, 800);
    let mut px = vec![0u32; (w * h) as usize];
    let mut cv = crate::ui::Canvas { px: &mut px, w, h };
    app.render_into(&mut cv, 0.016);
    let btns = app.diff.as_ref().map(|d| d.btns.clone()).unwrap_or_default();
    let accept = btns.iter().find(|(_, b)| *b == DiffBtn::Accept).map(|(r, _)| *r).expect("Accept pill drawn");
    assert!(btns.iter().any(|(_, b)| *b == DiffBtn::Reject), "Reject pill drawn");
    // A hand over the pills (not the text I-beam of the editor area below them).
    for (r, _) in &btns {
        app.mouse = ((r.x + r.w / 2) as f64, (r.y + r.h / 2) as f64);
        assert_eq!(app.cursor_for(), winit::window::CursorIcon::Pointer, "pointer over the diff buttons");
    }
    assert!(app.diff_click((accept.x + 6) as f64, (accept.y + 6) as f64));
    let r = c.reply(&mut app, d1);
    assert_eq!((text_of(&r["result"], 0), text_of(&r["result"], 1)), ("FILE_SAVED".into(), "one\nTWO\n".into()));
    assert!(app.diff.is_none(), "accepted: the diff closes (the agent writes the file)");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "one\ntwo\n", "Lumen never writes it itself");

    // Ctrl+Enter accepts, Esc rejects (while the diff has the keyboard).
    let d2 = c.request("tools/call", diff("t2", "uno\n"));
    pump_until(&mut app, showing);
    press(&mut app, NamedKey::Enter, ModifiersState::CONTROL);
    assert_eq!(text_of(&c.reply(&mut app, d2)["result"], 0), "FILE_SAVED");
    let d3 = c.request("tools/call", diff("t3", "tres\n"));
    pump_until(&mut app, showing);
    press(&mut app, NamedKey::Escape, ModifiersState::empty());
    let r = c.reply(&mut app, d3);
    assert_eq!((text_of(&r["result"], 0), text_of(&r["result"], 1)), ("DIFF_REJECTED".into(), "t3".into()));
    assert!(app.diff.is_none());

    // Two at once: the second waits its turn; close_tab / closeAllDiffTabs answer them as rejected.
    let d4 = c.request("tools/call", diff("t4", "4\n"));
    let d5 = c.request("tools/call", diff("t5", "5\n"));
    pump_until(&mut app, |a| a.ai.bridge.proposals.len() == 2);
    let first = |app: &App| app.ai.bridge.proposals.first().map(|p| p.key);
    assert_eq!(app.diff.as_ref().and_then(|d| d.proposal), first(&app), "the first stays on screen");
    assert_eq!(text_of(&c.call(&mut app, "close_tab", json!({"tab_name": "t4"})), 0), "TAB_CLOSED");
    assert_eq!(text_of(&c.reply(&mut app, d4)["result"], 0), "DIFF_REJECTED");
    assert_eq!(app.diff.as_ref().and_then(|d| d.proposal), first(&app), "then the next one");
    assert_eq!(text_of(&c.call(&mut app, "closeAllDiffTabs", json!({})), 0), "CLOSED_1_DIFF_TABS");
    assert_eq!(text_of(&c.reply(&mut app, d5)["result"], 0), "DIFF_REJECTED");
    assert!(app.diff.is_none() && app.ai.bridge.proposals.is_empty());

    // The agent goes away: its pending diff closes.
    c.request("tools/call", diff("t6", "6\n"));
    pump_until(&mut app, |a| !a.ai.bridge.proposals.is_empty());
    assert_eq!(app.ai.bridge.proposals.len(), 1);
    let _ = c.s.shutdown(Shutdown::Both);
    pump_until(&mut app, |a| !a.ai.bridge.connected());
    assert!(!app.ai.bridge.connected() && app.ai.bridge.proposals.is_empty() && app.diff.is_none());
    // A wrong token never gets in.
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nUpgrade: websocket\r\nSec-WebSocket-Key: x3JJHMbDL1EzLkh9GBhXDw==\r\nX-Claude-Code-Ide-Authorization: no\r\n\r\n")
        .unwrap();
    let mut buf = [0u8; 12];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"HTTP/1.1 401");
    drop(app);
    assert!(!locks.join(format!("{port}.lock")).exists(), "the lock file goes with Lumen");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fake `claude` showing what Lumen started it with, then echoing.
#[cfg(unix)]
const STUB_CLAUDE: &str = "#!/bin/sh\n\
printf 'port=%s ide=%s tok=%s arg=%s\\r\\n' \"$CLAUDE_CODE_SSE_PORT\" \"$ENABLE_IDE_INTEGRATION\" \"${LUMEN_HOOK_TOKEN:+set}\" \"$1\"\n\
exec cat\n";

#[test]
#[cfg(unix)]
fn claude_gets_the_bridge_and_its_hooks_drive_the_status() {
    use super::tests::{stub, wait_screen};
    let _g = lock();
    let dir = tmp("ide-hooks");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "claude", STUB_CLAUDE, true);
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.ai.search = Some(vec![bin.clone()]);
    app.run_bound("AiPanel");
    let (port, hook) = {
        let s = app.ai.bridge.server.as_ref().expect("starting Claude Code starts the bridge");
        (s.port(), s.hook_token().to_string())
    };
    let want = format!("port={port} ide=true tok=set arg=--settings");
    let screen = wait_screen(&mut app, &want);
    assert!(screen.contains(&want), "{screen:?}");
    let post = |route: &str, token: &str| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = r#"{"hook_event_name":"x","tool_input":{"file_path":"seed.txt"}}"#;
        let req = format!(
            "POST /lumen/hook/{route} HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = [0u8; 12];
        s.read_exact(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };
    let st = |app: &App| app.ai.cur().map_or(Status::Starting, Session::status);
    let until = |app: &mut App, want: Status| {
        let end = Instant::now() + Duration::from_secs(10);
        while st(app) != want && Instant::now() < end {
            app.pump_ide();
            app.pump_ai();
            std::thread::sleep(Duration::from_millis(10));
        }
        st(app)
    };
    assert_eq!(post("working", "wrong"), "HTTP/1.1 401");
    assert_eq!(post("working", &hook), "HTTP/1.1 200");
    assert_eq!(until(&mut app, Status::Working), Status::Working, "UserPromptSubmit: working");
    assert_eq!(post("waiting", &hook), "HTTP/1.1 200");
    assert_eq!(until(&mut app, Status::Waiting), Status::Waiting, "a permission prompt: waiting for you");
    assert_eq!(post("edited", &hook), "HTTP/1.1 200");
    assert_eq!(until(&mut app, Status::Working), Status::Working, "an edit: still working");
    assert_eq!(post("done", &hook), "HTTP/1.1 200");
    assert_eq!(until(&mut app, Status::Done), Status::Done, "Stop: done");
    assert_eq!(app.ai.cur().unwrap().track.hook, Some(Status::Done));
    app.ai.on_input();
    assert_eq!(app.ai.cur().unwrap().track.hook, None, "typing hands the chip back to the heuristic");
    app.ai_stop();
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clients_are_matched_to_their_sessions() {
    let mut b = bridge::Bridge::default();
    b.expect_session(1);
    b.expect_session(2);
    b.on_open(10);
    b.on_open(11);
    assert_eq!((b.owner_of(10), b.owner_of(11)), (Some(1), Some(2)), "by start order until they say who they are");
    // 10 announces the pid of session 2's agent: the guess for session 1 is open again.
    b.claim(10, Some(2));
    assert_eq!(b.owner_of(10), Some(2));
    assert_eq!(b.expect, vec![1]);
    b.claim(11, Some(1));
    assert_eq!((b.owner_of(10), b.owner_of(11)), (Some(2), Some(1)));
    assert!(b.expect.is_empty());
    // A Claude Code attached with /ide belongs to no session.
    b.on_open(12);
    b.claim(12, None);
    assert_eq!(b.owner_of(12), None);
    assert_eq!(b.clients_of(None), vec![12]);
    assert_eq!(b.clients_of(Some(1)), vec![11]);
    assert!(b.session_connected(1) && b.session_connected(2));
    assert_eq!(b.on_closed(11), Some(1));
    assert!(!b.session_connected(1));
    b.forget_session(2);
    assert_eq!(b.owner_of(10), None, "a restarted session's old client is nobody's");
}

/// A fake `claude` showing its session and whether its hook URLs name it, then echoing.
#[cfg(unix)]
const STUB_SESSION: &str = "#!/bin/sh\n\
case \"$2\" in *\"/lumen/hook/done/$LUMEN_AI_SESSION\\\"\"*) mine=yes;; *) mine=no;; esac\n\
printf 'session=%s hooks-mine=%s\\r\\n' \"$LUMEN_AI_SESSION\" \"$mine\"\n\
exec cat\n";

#[test]
#[cfg(unix)]
fn sessions_get_their_own_hooks_notifications_and_client() {
    use super::tests::{stub, wait_screen};
    let _g = lock();
    let dir = tmp("ai-sessions");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "claude", STUB_SESSION, true);
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.ai.search = Some(vec![bin.clone()]);
    app.run_bound("AiPanel");
    let s1 = app.ai.sessions[0].id;
    let screen = wait_screen(&mut app, &format!("session={s1} hooks-mine=yes"));
    assert!(screen.contains(&format!("session={s1} hooks-mine=yes")), "{screen:?}");
    app.run_bound("AiNewSession");
    app.overlay_accept(); // Claude Code: the current agent comes first
    assert_eq!(app.ai.sessions.len(), 2, "a second session runs alongside");
    assert_eq!(app.ai.active, 1);
    let s2 = app.ai.sessions[1].id;
    assert_ne!(s1, s2);
    assert_eq!(app.ai.sessions[1].name, "Claude Code 2");
    let screen = wait_screen(&mut app, &format!("session={s2} hooks-mine=yes"));
    assert!(screen.contains(&format!("session={s2} hooks-mine=yes")), "{screen:?}");
    let (port, hook, token) = {
        let s = app.ai.bridge.server.as_ref().expect("the bridge listens");
        (s.port(), s.hook_token().to_string(), s.token().to_string())
    };
    assert_eq!(app.ai.bridge.expect, vec![s1, s2], "both wait for their Claude Code to connect");

    // Claude Code connects and says its pid: it is session 2's, whatever the order.
    let pid2 = app.ai.sessions[1].term.pid().unwrap();
    let mut c = Fake::connect(port, &token);
    c.send(json!({"jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": pid2}}));
    pump_until(&mut app, |a| a.ai.bridge.session_connected(s2));
    assert!(app.ai.bridge.session_connected(s2) && !app.ai.bridge.session_connected(s1));
    assert_eq!(app.ai.bridge.expect, vec![s1]);
    assert!(app.ai_bridge_connected(), "the session shown is connected");
    app.ai_activate(0);
    assert!(!app.ai_bridge_connected(), "the other one is not");
    app.ai_activate(1);

    // Hooks reach the session they name.
    let post = |route: &str, sid: u64| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = r#"{"hook_event_name":"x"}"#;
        let req = format!(
            "POST /lumen/hook/{route}/{sid} HTTP/1.1\r\nAuthorization: Bearer {hook}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = [0u8; 12];
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"HTTP/1.1 200");
    };
    let st = |app: &App, i: usize| app.ai.sessions[i].status();
    let until = |app: &mut App, i: usize, want: Status| {
        let end = Instant::now() + Duration::from_secs(10);
        while st(app, i) != want && Instant::now() < end {
            app.pump_ide();
            app.pump_ai();
            std::thread::sleep(Duration::from_millis(10));
        }
        st(app, i)
    };
    app.ai_window_focus(false);
    post("working", s1);
    assert_eq!(until(&mut app, 0, Status::Working), Status::Working);
    post("waiting", s1);
    assert_eq!(until(&mut app, 0, Status::Waiting), Status::Waiting, "session 1 asks");
    assert_ne!(st(&app, 1), Status::Waiting, "session 2 is untouched");
    assert_eq!(app.ai.sent, vec![("Claude Code".to_string(), "Waiting for you".to_string())], "one notification");
    assert!(app.ai.sessions[0].track.unseen, "not the session shown: its tab is marked");
    assert_eq!(app.ai.badge().map(|b| b.0).as_deref(), Some("Waiting for you"));
    post("working", s2);
    assert_eq!(until(&mut app, 1, Status::Working), Status::Working);
    post("done", s2);
    assert_eq!(until(&mut app, 1, Status::Done), Status::Done);
    assert_eq!(app.ai.sent.len(), 2);
    assert_eq!(app.ai.sent[1], ("Claude Code 2".to_string(), "Done".to_string()));
    // The idle reminder after the turn: the same turn, no second notification.
    post("waiting", s2);
    assert_eq!(until(&mut app, 1, Status::Waiting), Status::Waiting);
    assert_eq!(app.ai.sent.len(), 2, "one turn, one notification");
    // With Lumen focused nothing pops up.
    app.ai_window_focus(true);
    app.ai.sessions[0].track.on_input();
    post("working", s1);
    until(&mut app, 0, Status::Working);
    post("done", s1);
    assert_eq!(until(&mut app, 0, Status::Done), Status::Done);
    assert_eq!(app.ai.sent.len(), 2);

    // "AI: Next Session Waiting for You" goes to the one that asks.
    app.ai_activate(0);
    app.run_bound("AiNextWaiting");
    assert_eq!(app.ai.active, 1, "session 2 waits");
    // Closing a session ends it and forgets it in the bridge.
    app.ai_close_session(1);
    assert_eq!(app.ai.sessions.len(), 1);
    assert_eq!(app.ai.active, 0);
    assert!(!app.ai.bridge.session_connected(s2));
    // Rename.
    app.ai_rename(s1, "  refactor  ");
    assert_eq!(app.ai.sessions[0].name, "refactor");
    app.ai_rename(s1, "");
    assert_eq!(app.ai.sessions[0].name, "Claude Code", "empty gives the agent's name back");
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
