use super::*;
use std::{
    io::{BufRead, BufReader},
    path::Path,
};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn sha1_base64_and_the_rfc_accept_key() {
    assert_eq!(hex(&ws::sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(hex(&ws::sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
    // 56 bytes: the padding spills into a second block.
    assert_eq!(
        hex(&ws::sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
    assert_eq!(hex(&ws::sha1(&[b'a'; 1000])), "291e9a6c66994949b57ba5e650361e98fc36b1ba");
    assert_eq!(ws::base64(b""), "");
    assert_eq!(ws::base64(b"f"), "Zg==");
    assert_eq!(ws::base64(b"fo"), "Zm8=");
    assert_eq!(ws::base64(b"foo"), "Zm9v");
    assert_eq!(ws::base64(b"foobar"), "Zm9vYmFy");
    // RFC 6455 section 1.3.
    assert_eq!(ws::accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

#[test]
fn frames_roundtrip_with_masks_and_every_length_form() {
    let key = [0x37, 0xfa, 0x21, 0x3d];
    // RFC 6455 5.7: a masked "Hello" from a client.
    let hello = ws::encode(ws::OP_TEXT, b"Hello", Some(key));
    assert_eq!(hello, [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]);
    let f = ws::read_frame(&mut &hello[..], 1 << 20, true).unwrap();
    assert_eq!(f, ws::Frame { fin: true, op: ws::OP_TEXT, payload: b"Hello".to_vec() });
    // An unmasked server frame is refused where a mask is required, read where it is not.
    let plain = ws::encode(ws::OP_TEXT, b"Hello", None);
    assert_eq!(plain, [0x81, 0x05, b'H', b'e', b'l', b'l', b'o']);
    assert!(ws::read_frame(&mut &plain[..], 1 << 20, true).is_err());
    assert_eq!(ws::read_frame(&mut &plain[..], 1 << 20, false).unwrap().payload, b"Hello");
    // 16-bit and 64-bit lengths.
    for n in [125usize, 126, 65535, 65536, 200_000] {
        let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        let enc = ws::encode(ws::OP_BINARY, &data, Some(key));
        let expect_len_byte = match n {
            0..=125 => n as u8,
            126..=65535 => 126,
            _ => 127,
        };
        assert_eq!(enc[1] & 0x7f, expect_len_byte, "length form for {n}");
        let f = ws::read_frame(&mut &enc[..], 1 << 20, true).unwrap();
        assert_eq!(f.payload, data, "{n} bytes");
    }
    // Over the cap: refused before reading the payload.
    let big = ws::encode(ws::OP_BINARY, &[0u8; 300], Some(key));
    assert!(ws::read_frame(&mut &big[..], 200, true).is_err());
    // Reserved bits (no extension negotiated) and fragmented control frames are errors.
    let mut rsv = hello.clone();
    rsv[0] |= 0x40;
    assert!(ws::read_frame(&mut &rsv[..], 1 << 20, true).is_err());
    let frag_ping = ws::encode_frame(false, ws::OP_PING, b"x", Some(key));
    assert!(ws::read_frame(&mut &frag_ping[..], 1 << 20, true).is_err());
}

#[test]
fn fragments_join_and_control_frames_interleave() {
    let k = Some([1, 2, 3, 4]);
    let mut wire = Vec::new();
    wire.extend(ws::encode_frame(false, ws::OP_TEXT, b"{\"a\":", k));
    wire.extend(ws::encode(ws::OP_PING, b"hi", k));
    wire.extend(ws::encode_frame(false, ws::OP_CONT, b" 1", k));
    wire.extend(ws::encode_frame(true, ws::OP_CONT, b"}", k));
    wire.extend(ws::encode(ws::OP_PONG, b"", k));
    wire.extend(ws::encode(ws::OP_CLOSE, &ws::close_payload(1001), k));
    let mut r = &wire[..];
    let mut asm = ws::Assembler::new(1 << 20);
    assert_eq!(ws::read_message(&mut r, &mut asm, true).unwrap(), ws::Msg::Ping(b"hi".to_vec()));
    assert_eq!(ws::read_message(&mut r, &mut asm, true).unwrap(), ws::Msg::Text("{\"a\": 1}".into()));
    assert_eq!(ws::read_message(&mut r, &mut asm, true).unwrap(), ws::Msg::Pong);
    assert_eq!(ws::read_message(&mut r, &mut asm, true).unwrap(), ws::Msg::Close(Some(1001)));
    // A continuation with nothing to continue, and a new message inside a fragmented one.
    let mut asm = ws::Assembler::new(1 << 20);
    let lone = ws::encode(ws::OP_CONT, b"x", k);
    assert!(ws::read_message(&mut &lone[..], &mut asm, true).is_err());
    let mut asm = ws::Assembler::new(1 << 20);
    let mut two = ws::encode_frame(false, ws::OP_TEXT, b"a", k);
    two.extend(ws::encode(ws::OP_TEXT, b"b", k));
    assert!(ws::read_message(&mut &two[..], &mut asm, true).is_err());
    // The message cap holds across fragments.
    let mut asm = ws::Assembler::new(4);
    let mut long = ws::encode_frame(false, ws::OP_TEXT, b"abc", k);
    long.extend(ws::encode(ws::OP_CONT, b"de", k));
    assert!(ws::read_message(&mut &long[..], &mut asm, true).is_err());
}

#[test]
fn mcp_routes_the_handshake_and_hands_tool_calls_over() {
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}});
    let mcp::Route::Reply(r) = mcp::route(&init) else { panic!("initialize is answered at once") };
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "lumen-text");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    let mcp::Route::Reply(r) = mcp::route(&json!({"jsonrpc": "2.0", "id": "a", "method": "tools/list"})) else {
        panic!()
    };
    let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    for want in [
        "openFile",
        "openDiff",
        "getCurrentSelection",
        "getLatestSelection",
        "getOpenEditors",
        "getWorkspaceFolders",
        "getDiagnostics",
        "checkDocumentDirty",
        "saveDocument",
        "close_tab",
        "closeAllDiffTabs",
    ] {
        assert!(names.contains(&want), "{want} in tools/list");
    }
    assert!(!names.contains(&"executeCode"));
    let call = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "getOpenEditors"}});
    assert_eq!(mcp::route(&call), mcp::Route::Call { id: json!(7), name: "getOpenEditors".into(), args: json!({}) });
    let note = json!({"jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": 42}});
    assert_eq!(mcp::route(&note), mcp::Route::Note { method: "ide_connected".into(), params: json!({"pid": 42}) });
    let mcp::Route::Reply(r) = mcp::route(&json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})) else { panic!() };
    assert_eq!(r["error"]["code"], -32601);
    assert_eq!(mcp::route(&json!({"jsonrpc": "2.0", "id": 3, "result": {}})), mcp::Route::Ignore);
    assert_eq!(mcp::texts(&["FILE_SAVED", "x"])["content"][1]["text"], "x");
    assert_eq!(mcp::failure("no")["isError"], true);
}

#[test]
fn lock_files_are_private_written_removed_and_swept() {
    let base = std::env::temp_dir().join(format!("lumen-ide-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    // CLAUDE_CONFIG_DIR wins over the home folder.
    assert_eq!(lock::ide_dir_from(Some(base.as_os_str()), Some(Path::new("/home/u"))), Some(base.join("ide")));
    assert_eq!(lock::ide_dir_from(None, Some(Path::new("/home/u"))), Some(PathBuf::from("/home/u/.claude/ide")));
    assert_eq!(lock::ide_dir_from(Some(std::ffi::OsStr::new("")), None), None);
    let dir = base.join("ide");
    let p = lock::write(&dir, 23456, 777, &[PathBuf::from("/w/proj")], "tok").unwrap();
    assert_eq!(p, dir.join("23456.lock"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["pid"], 777);
    assert_eq!(v["workspaceFolders"], json!(["/w/proj"]));
    assert_eq!(v["ideName"], "Lumen");
    assert_eq!(v["transport"], "ws");
    assert_eq!(v["authToken"], "tok");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Sweep: only our own files whose process is gone.
    lock::write(&dir, 23457, 4_000_000, &[], "t").unwrap();
    std::fs::write(dir.join("23458.lock"), r#"{"pid": 4000001, "ideName": "VS Code"}"#).unwrap();
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    let alive = |pid: u32| pid == 777;
    assert_eq!(lock::sweep_with(&dir, alive), 1);
    assert!(p.exists(), "a live Lumen keeps its file");
    assert!(!dir.join("23457.lock").exists(), "a dead Lumen's file goes");
    assert!(dir.join("23458.lock").exists(), "another IDE's file is never touched");
    assert!(lock::pid_alive(std::process::id()) && !lock::pid_alive(0));
    assert!(!lock::pid_alive(u32::MAX - 3), "no such process");
    // A real server writes its file and removes it on drop.
    let srv = Server::start(vec![PathBuf::from("/w/proj")], Some(dir.clone()), Arc::new(|| {})).unwrap();
    let lp = srv.lock_path().unwrap().to_path_buf();
    assert_eq!(lp, dir.join(format!("{}.lock", srv.port())));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&lp).unwrap()).unwrap();
    assert_eq!(v["pid"], std::process::id());
    assert_eq!(v["authToken"].as_str().map(str::len), Some(32), "128-bit hex token");
    assert!((10000..=65535).contains(&srv.port()) || srv.port() > 0);
    drop(srv);
    assert!(!lp.exists(), "the lock file goes with the server");
    let _ = std::fs::remove_dir_all(&base);
}

/// Read an HTTP response head (status line + headers).
fn read_status(s: &TcpStream) -> String {
    let mut r = BufReader::new(s);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    head
}

fn upgrade(port: u16, token: Option<&str>) -> (TcpStream, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let auth = token.map(|t| format!("{AUTH_HEADER}: {t}\r\n")).unwrap_or_default();
    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{auth}\r\n"
    );
    s.write_all(req.as_bytes()).unwrap();
    let head = read_status(&s);
    (s, head)
}

#[test]
fn the_server_checks_the_token_and_answers_the_handshake() {
    let srv = Server::start(vec![], None, Arc::new(|| {})).unwrap();
    let (_, head) = upgrade(srv.port(), Some("wrong-token-wrong-token-wrong-to"));
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");
    let (_, head) = upgrade(srv.port(), None);
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");
    let (mut s, head) = upgrade(srv.port(), Some(srv.token()));
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");
    let mask = Some([9, 8, 7, 6]);
    let send =
        |s: &mut TcpStream, v: Value| s.write_all(&ws::encode(ws::OP_TEXT, v.to_string().as_bytes(), mask)).unwrap();
    let recv = |s: &mut TcpStream| -> Value {
        let f = ws::read_frame(s, MAX_MESSAGE, false).unwrap();
        serde_json::from_slice(&f.payload).unwrap()
    };
    send(&mut s, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
    let r = recv(&mut s);
    assert_eq!((r["id"].clone(), r["result"]["protocolVersion"].clone()), (json!(1), json!(mcp::PROTOCOL)));
    // A ping is answered with a pong carrying the same data.
    s.write_all(&ws::encode(ws::OP_PING, b"zz", mask)).unwrap();
    let f = ws::read_frame(&mut s, 1024, false).unwrap();
    assert_eq!((f.op, f.payload), (ws::OP_PONG, b"zz".to_vec()));
    // Garbage JSON gets a parse error, a batch gets one answer per request.
    s.write_all(&ws::encode(ws::OP_TEXT, b"{nope", mask)).unwrap();
    assert_eq!(recv(&mut s)["error"]["code"], -32700);
    send(
        &mut s,
        json!([{"jsonrpc": "2.0", "id": 2, "method": "ping"}, {"jsonrpc": "2.0", "id": 3, "method": "tools/list"}]),
    );
    assert_eq!(recv(&mut s)["id"], 2);
    assert_eq!(recv(&mut s)["id"], 3);
    // Tool calls and notifications reach the UI side as events.
    send(&mut s, json!({"jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": 1}}));
    send(
        &mut s,
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "getWorkspaceFolders", "arguments": {}}}),
    );
    let mut got = Vec::new();
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while got.len() < 3 && std::time::Instant::now() < end {
        match srv.try_recv() {
            Some(e) => got.push(e),
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(matches!(got[0], Event::Open(_)), "{got:?}");
    assert!(matches!(&got[1], Event::Note { method, .. } if method == "ide_connected"), "{got:?}");
    let Event::Call { client, id, name, .. } = &got[2] else { panic!("{got:?}") };
    assert_eq!((id.clone(), name.as_str()), (json!(9), "getWorkspaceFolders"));
    // The UI's answer goes back on the same connection.
    srv.send(*client, &mcp::reply(id, mcp::text("ok")));
    assert_eq!(recv(&mut s)["result"]["content"][0]["text"], "ok");
    // Close: the server echoes it and reports the client gone.
    s.write_all(&ws::encode(ws::OP_CLOSE, &ws::close_payload(1000), mask)).unwrap();
    let f = ws::read_frame(&mut s, 1024, false).unwrap();
    assert_eq!(f.op, ws::OP_CLOSE);
    let end = std::time::Instant::now() + Duration::from_secs(10);
    let mut closed = false;
    while !closed && std::time::Instant::now() < end {
        match srv.try_recv() {
            Some(Event::Closed(c)) => closed = c == *client,
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(closed);
}

#[test]
fn hooks_need_their_bearer_token() {
    let srv = Server::start(vec![], None, Arc::new(|| {})).unwrap();
    let post = |path: &str, auth: &str, body: &str| -> String {
        let mut s = TcpStream::connect(("127.0.0.1", srv.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nAuthorization: {auth}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        read_status(&s)
    };
    let good = format!("Bearer {}", srv.hook_token());
    assert!(post("/lumen/hook/working", "Bearer nope", "{}").starts_with("HTTP/1.1 401"));
    assert!(post("/elsewhere", &good, "{}").starts_with("HTTP/1.1 404"));
    let body = r#"{"hook_event_name":"Stop","session_id":"s"}"#;
    assert!(post("/lumen/hook/done", &good, body).starts_with("HTTP/1.1 200"));
    let end = std::time::Instant::now() + Duration::from_secs(10);
    let mut hook = None;
    while hook.is_none() && std::time::Instant::now() < end {
        match srv.try_recv() {
            Some(Event::Hook { route, body }) => hook = Some((route, body)),
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    let (route, body) = hook.expect("the hook reached the UI side");
    assert_eq!(route, "done");
    assert_eq!(body["hook_event_name"], "Stop");
    // The settings Claude Code is started with point its hooks here, token from the environment.
    let v: Value = serde_json::from_str(&hook_settings(srv.port())).unwrap();
    let stop = &v["hooks"]["Stop"][0]["hooks"][0];
    assert_eq!(stop["type"], "http");
    assert_eq!(stop["url"], format!("http://127.0.0.1:{}/lumen/hook/done", srv.port()));
    assert_eq!(stop["headers"]["Authorization"], "Bearer $LUMEN_HOOK_TOKEN");
    assert_eq!(stop["allowedEnvVars"], json!(["LUMEN_HOOK_TOKEN"]));
    assert_eq!(v["hooks"]["Notification"][0]["matcher"], "permission_prompt");
    assert_eq!(v["hooks"]["Notification"][1]["matcher"], "idle_prompt");
    assert_eq!(v["hooks"]["PostToolUse"][0]["matcher"], "Edit|Write|MultiEdit");
    assert!(v["hooks"]["UserPromptSubmit"][0]["hooks"][0]["url"].as_str().unwrap().ends_with("/working"));
}

#[test]
fn small_helpers() {
    assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"abc", b"ab"));
    let t = random_hex(16);
    assert_eq!(t.len(), 32);
    assert!(t.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_ne!(t, random_hex(16));
    // UTF-16 columns: astral characters count twice, BMP ones once.
    assert_eq!(utf16_col("abc", 2), 2);
    assert_eq!(utf16_col("日本語x", 2), 2);
    assert_eq!(utf16_col("a😀b", 2), 3);
    assert_eq!(utf16_col("a😀b", 3), 4);
    assert_eq!(utf16_col("ab", 10), 2, "past the end clamps");
    let r = parse_head("GET /x HTTP/1.1\r\nUpgrade: WebSocket\r\nX-Claude-Code-Ide-Authorization: t").unwrap();
    assert_eq!((r.method.as_str(), r.path.as_str()), ("GET", "/x"));
    assert_eq!(r.header("upgrade"), Some("WebSocket"));
    assert_eq!(r.header(AUTH_HEADER), Some("t"));
}
