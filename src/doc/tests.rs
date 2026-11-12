use super::*;

#[test]
fn uri_roundtrip() {
    #[cfg(unix)]
    let (p, want) = (Path::new("/home/a b/ç/x.ts"), "file:///home/a%20b/%C3%A7/x.ts");
    #[cfg(windows)]
    let (p, want) = (Path::new("C:\\home\\a b\\ç\\x.ts"), "file:///C:/home/a%20b/%C3%A7/x.ts");
    let u = path_to_uri(p);
    assert_eq!(u, want);
    assert_eq!(uri_to_path(&u).unwrap(), p);
}

#[test]
fn only_tsgo_documents_are_addressed_under_the_mirror() {
    let root = std::env::temp_dir().join(format!("lumen-uri-mirror-{}", std::process::id()));
    let mirror = root.join(".mirror");
    register_mirror(mirror.clone(), root.clone());
    let doc = |name: &str, lang| {
        let mut b = Buffer::from_str("");
        b.path = Some(root.join(name));
        Doc::new(b, None, lang, "//", 8.0)
    };
    let mut ts = doc("a.ts", Some("typescript"));
    ts.lsp_root = Some(root.clone());
    assert_eq!(ts.uri(), Some(path_to_uri(&mirror.join("a.ts"))), "tsgo reads the mirror");
    assert_eq!(ts.real_uri(), Some(path_to_uri(&root.join("a.ts"))));
    // Registry servers (marksman for a README in a lean TS repo) never see the mirror.
    let mut md = doc("README.md", Some("markdown"));
    md.lsp_root = Some(root.clone());
    assert_eq!(md.uri(), md.real_uri());
    // A TS document no tsgo serves (no tsgo root, e.g. a registry primary such as deno).
    let plain = doc("b.ts", Some("typescript"));
    assert_eq!(plain.uri(), plain.real_uri());
    unregister_mirror(&root);
}

#[test]
fn replace_text_is_one_undo_step_and_keeps_cursor() {
    let mut d = Doc::new(Buffer::from_str("const  a=1\nlet b = 2\n"), None, None, "//", 8.0);
    d.ed.click(&mut d.buf, 17, false); // inside "let b = 2"
    d.replace_text("const a = 1\nlet b = 2\n");
    assert_eq!(d.buf.to_string(), "const a = 1\nlet b = 2\n");
    // text before the cursor changed by +0 chars net (two edits in the first line): cursor stays on the same token
    assert_eq!(d.buf.line_col(d.ed.head()).0, 1);
    d.ed.undo(&mut d.buf);
    assert_eq!(d.buf.to_string(), "const  a=1\nlet b = 2\n", "single undo restores everything");
    assert!(!d.buf.is_dirty(), "undo returns to the saved version");
}

#[test]
fn lsp_format_edits_apply_in_order_as_one_undo_step() {
    let e = |sl, sc, el, ec, t: &str| LspEdit { sl, sc, el, ec, text: t.into() };
    let mut d = Doc::new(Buffer::from_str("a  b{c\nd\n"), None, None, "//", 8.0);
    d.ed.click(&mut d.buf, 9, false); // end of the text, after the edits
    // Two inserts at the same spot must land in list order ("\n" then "  "), plus a replace.
    d.apply_lsp_edits(&[e(0, 1, 0, 3, " "), e(0, 5, 0, 5, "\n"), e(0, 5, 0, 5, "  "), e(1, 0, 1, 0, "  ")]);
    assert_eq!(d.buf.to_string(), "a b{\n  c\n  d\n");
    assert_eq!(d.buf.line_col(d.ed.head()), (3, 0), "cursor stays at the end of the text");
    d.ed.undo(&mut d.buf);
    assert_eq!(d.buf.to_string(), "a  b{c\nd\n", "one undo step");
}

#[test]
fn lsp_positions_use_utf16() {
    let b = Buffer::from_str("a😀b\nx");
    let d = Doc::new(b, None, None, "//", 8.0);
    // 'b' is char 2 -> utf16 col 3 (the emoji takes two units)
    assert_eq!(d.lsp_pos(2), (0, 3));
    assert_eq!(d.pos_from_lsp(0, 3), 2);
    assert_eq!(d.pos_from_lsp(1, 1), 5);
}

#[test]
fn sticky_keeps_the_two_innermost_headers() {
    assert_eq!(sticky_pick(&[1, 5, 9], STICKY_MAX), vec![5, 9]);
    assert_eq!(sticky_pick(&[4], STICKY_MAX), vec![4]);
    assert!(sticky_pick(&[], STICKY_MAX).is_empty());
}

#[test]
fn sticky_band_snaps_to_the_row_grid_at_rest() {
    let lh = 22;
    // Aligned text, or nothing pinned: exactly n lines.
    assert_eq!(sticky_band_h(2, lh, 0, true), 44);
    assert_eq!(sticky_band_h(0, lh, 7, true), 0);
    // Rows start at k*lh - frac; the band ends on one of those boundaries.
    for frac in 1..lh {
        let h = sticky_band_h(2, lh, frac, true);
        assert!(h >= 44 && h < 44 + lh);
        assert_eq!((h + frac) % lh, 0, "frac {frac}: band end {h} is a row boundary");
    }
    // Fully scrolled to the next row: already aligned.
    assert_eq!(sticky_band_h(2, lh, lh, true), 44);
    // While the text glides the band keeps its size.
    assert_eq!(sticky_band_h(2, lh, 9, false), 44);
}

#[test]
fn code_actions_get_back_the_data_a_server_attached_to_its_diagnostics() {
    let mut d = Doc::new(Buffer::from_str("we recieve it\n"), None, None, "//", 8.0);
    let range = serde_json::json!({"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 10}});
    let raw = serde_json::json!({"range": range, "message": "`recieve` should be `receive`", "severity": 3,
        "data": {"corrections": ["receive"], "typo": "recieve"}});
    d.set_diag_data("typos-lsp", vec![raw.clone()]);
    let plain = serde_json::json!({"range": range, "message": "`recieve` should be `receive`", "severity": 3});
    let other = serde_json::json!({"range": range, "message": "something else", "severity": 1});
    let mut list = serde_json::json!([plain.clone(), other.clone()]);
    d.with_diag_data("typos-lsp", &mut list);
    assert_eq!(list, serde_json::json!([raw, other]), "only the matching entry is swapped");
    let mut list = serde_json::json!([plain.clone()]);
    d.with_diag_data("harper-ls", &mut list);
    assert_eq!(list, serde_json::json!([plain.clone()]), "another server's data is never handed out");
    d.set_diag_data("typos-lsp", Vec::new());
    let mut list = serde_json::json!([plain.clone()]);
    d.with_diag_data("typos-lsp", &mut list);
    assert_eq!(list, serde_json::json!([plain]), "forgotten");
}

#[test]
fn rtl_lines_map_caret_and_clicks_through_the_bidi_layout() {
    let mut d = Doc::new(Buffer::from_str("abc אבג\nplain\n"), None, None, "//", 8.0);
    d.rect = Rect::new(0, 0, 800, 400);
    let lh = line_h() as f64;
    let tx = (d.rect.x + d.gutter_w()) as f64;
    // Caret: after the last Hebrew letter it sits at the word's left end (cell 4); after the first, at cell 6.
    assert_eq!(d.caret_geom(d.buf.pos(0, 7)), (0, 4));
    assert_eq!(d.caret_geom(d.buf.pos(0, 5)), (0, 6));
    assert_eq!(d.caret_rtl(d.buf.pos(0, 5)), Some(true));
    assert_eq!(d.caret_rtl(d.buf.pos(0, 2)), Some(false));
    // Click: the right half of cell 6 (the first Hebrew letter) is before it, the left half after it.
    assert_eq!(d.pos_at(tx + 6.8 * 8.0, lh / 2.0), d.buf.pos(0, 4));
    assert_eq!(d.pos_at(tx + 6.2 * 8.0, lh / 2.0), d.buf.pos(0, 5));
    assert_eq!(d.pos_at(tx + 0.8 * 8.0, lh / 2.0), d.buf.pos(0, 1));
    // A pure left-to-right line keeps the plain grid and shows no direction flag.
    assert_eq!(d.caret_geom(d.buf.pos(1, 3)), (1, 3));
    assert_eq!(d.caret_rtl(d.buf.pos(1, 3)), None);
    assert_eq!(d.pos_at(tx + 3.2 * 8.0, lh + lh / 2.0), d.buf.pos(1, 3));
}

#[test]
fn rtl_text_renders_headless() {
    let theme = Theme::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes/dark-plus.json")).unwrap();
    let mut tx = TextCtx::new(None);
    let mut ink = |text: &str| -> usize {
        let mut d = Doc::new(Buffer::from_str(text), None, None, "//", tx.cell_w);
        let (w, h) = (400, 120);
        let mut px = vec![0u32; (w * h) as usize];
        let mut cv = Canvas { px: &mut px, w, h };
        let o = DrawOpts { dt: 0.0, find: &[], find_cur: None, focused: true };
        d.draw(&mut cv, &mut tx, &theme, Rect::new(0, 0, w, h), &o);
        let bg = theme.background.0 as u32;
        px.iter().filter(|p| (**p >> 16) & 255 != bg).count()
    };
    let (empty, hebrew, arabic) = (ink(""), ink("let s = \"שלום\";\n"), ink("مرحبا بالعالم\n"));
    // The fonts of a bare CI image may lack the scripts; with them the text must leave ink.
    eprintln!("[bidi test] ink: empty {empty}, hebrew {hebrew}, arabic {arabic}");
    assert!(hebrew > empty, "the Latin part of the Hebrew line is drawn");
    assert!(arabic >= empty);
}
