use super::*;

fn dark() -> Theme {
    let assets = crate::config::assets_dir();
    Theme::load(&assets.join("themes/dark-plus.json")).unwrap()
}

fn laid(blocks: &[Blk], w: i32) -> Laid {
    let mut tx = TextCtx::new(None);
    let th = dark();
    let mut hl = |_: &str, _: &str| None;
    let mut env = Env::new(&mut tx, Pal::new(&th), &mut hl);
    layout(blocks, w, &mut env)
}

#[test]
fn prose_wraps_to_width_and_keeps_links() {
    let long = "palavra ".repeat(60);
    let blocks = vec![Blk::Para(vec![
        Span::plain(long),
        Span { text: "link".into(), link: Some("http://x".into()), ..Default::default() },
    ])];
    let l = laid(&blocks, 300);
    assert!(l.lines.len() > 3, "wrapped into {} lines", l.lines.len());
    for ln in &l.lines {
        let right = ln.segs.iter().map(|s| s.x + s.w).max().unwrap_or(0);
        assert!(right <= 300 + 6, "line too wide: {right}");
    }
    assert_eq!(l.links.len(), 1);
    assert_eq!(l.h, l.lines.len() as i32 * prose_lh());
}

#[test]
fn code_wraps_at_commas_and_never_loses_text() {
    let line = "function configure(alpha: string, beta: number, gamma: boolean, delta: string[]): Promise<void>";
    let chars = code_chars(line, None, Rgba(1, 2, 3, 255));
    let parts = wrap_code(chars, 40);
    assert!(parts.len() >= 3);
    for p in &parts {
        assert!(p.len() <= 40);
    }
    let joined: String = parts
        .iter()
        .map(|p| p.iter().map(|c| c.0).collect::<String>().trim().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(joined.split_whitespace().collect::<Vec<_>>().join(" "), line);
    assert!(parts[0].iter().map(|c| c.0).collect::<String>().trim_end().ends_with(','));
}

#[test]
fn code_block_gets_background_and_lines() {
    let l = laid(&[Blk::Code { text: "const a = 1;\nconst b = 2;".into(), lang: "ts".into() }], 400);
    assert_eq!(l.lines.len(), 2);
    assert_eq!(l.decos.len(), 1);
    assert!(l.h >= 2 * line_h() as i32 + 10);
}

#[test]
fn placement_stays_inside_bounds() {
    let b = Rect::new(0, 30, 800, 500);
    for (ax, ay) in [(0, 40), (790, 40), (400, 520), (400, 300), (790, 540), (-20, 300)] {
        for want in [40, 200, 480, 900] {
            for w in [100, 500, 900] {
                let (r, _) = place((ax, ay), 20, b, w, want, None);
                assert!(r.x >= b.x && r.right() <= b.right(), "x {r:?} for {ax},{ay},{want},{w}");
                assert!(r.y >= b.y && r.bottom() <= b.bottom(), "y {r:?} for {ax},{ay},{want},{w}");
            }
        }
    }
    // prefers above when it fits, below otherwise
    let (r, below) = place((100, 300), 20, b, 300, 100, None);
    assert!(!below && r.bottom() <= 300);
    let (r, below) = place((100, 60), 20, b, 300, 200, None);
    assert!(below && r.y >= 80);
    // keeps its side while still fitting
    let (_, below) = place((100, 300), 20, b, 300, 100, Some(true));
    assert!(below);
}

#[test]
fn footer_rows_wrap() {
    assert_eq!(footer_rows(&[100, 100, 100], 10, 400), vec![vec![0, 1, 2]]);
    assert_eq!(footer_rows(&[100, 100, 100], 10, 250), vec![vec![0, 1], vec![2]]);
    assert_eq!(footer_rows(&[300], 10, 100), vec![vec![0]]);
}

#[test]
fn byte_offsets_pos_from_lspitions() {
    let t = "ab\ncdé😀x\nz";
    assert_eq!(byte_off(t, 0, 1), Some(1));
    assert_eq!(byte_off(t, 1, 0), Some(3));
    // é is 1 utf16 unit, 😀 is 2
    assert_eq!(byte_off(t, 1, 5), Some(3 + 1 + 1 + 2 + 4));
    assert_eq!(byte_off(t, 2, 1), Some(t.len()));
    assert_eq!(byte_off(t, 5, 0), None);
}

#[test]
fn literals_under_the_pointer() {
    let line = r#"  name: "Ana", age: 42, ok: true, p: 3.14,"#;
    let at = |s: &str| line.find(s).unwrap();
    let (a, b) = literal_at(line, at("Ana")).unwrap();
    assert_eq!(&line[a..b], "\"Ana\"");
    let (a, b) = literal_at(line, at("42")).unwrap();
    assert_eq!(&line[a..b], "42");
    assert_eq!(literal_at(line, at("ok")), None);
    let (a, b) = literal_at(line, at("true")).unwrap();
    assert_eq!(&line[a..b], "true");
    let (a, b) = literal_at(line, at("3.14")).unwrap();
    assert_eq!(&line[a..b], "3.14");
    assert_eq!(describe_literal("\"Ana\""), "string · 3 caracteres");
}

#[test]
fn tip_composes_params_returns_and_footer() {
    let md = "```typescript\nfunction sum(a: number, b?: number): number\n```\nSoma.\n\n*@param* `a` — primeiro\n\n*@returns* — total\n\n*@deprecated* — use `add`";
    let t = HoverTip::new(md, (0, 3), (0, 0));
    assert_eq!(t.kind, Kind::Function);
    assert!(matches!(t.blocks[0], Blk::Note { .. }), "deprecated first: {:?}", t.blocks[0]);
    let heads: Vec<String> = t
        .blocks
        .iter()
        .filter_map(|b| if let Blk::Item { head: Some(h), .. } = b { Some(h.clone()) } else { None })
        .collect();
    assert_eq!(heads, ["a: number", "b?: number", "number"]);
    let acts = t.actions();
    assert_eq!(acts, [Act::Definition, Act::References, Act::CopyType]);
    assert_eq!(t.type_text(), "function sum(a: number, b?: number): number");
}

#[test]
fn expanded_declaration_replaces_the_summary() {
    let mut t = HoverTip::new("```typescript\ninterface User\n```\nUm usuário.", (0, 4), (0, 0));
    assert_eq!(t.kind, Kind::Interface);
    t.extra.decl = Some(Decl { text: "interface User {\n  id: number;\n}".into(), more: 3, kind: Kind::Interface });
    t.compose();
    let codes: Vec<&str> = t
        .blocks
        .iter()
        .filter_map(|b| if let Blk::Code { text, .. } = b { Some(text.as_str()) } else { None })
        .collect();
    assert_eq!(codes, ["interface User {\n  id: number;\n}"]);
    assert!(t.blocks.iter().any(|b| matches!(b, Blk::Dim(_))));
    assert_eq!(t.type_text(), "interface User {\n  id: number;\n}");
}

#[test]
fn diagnostics_come_first_with_quick_fix() {
    let mut t = HoverTip::diag_only(
        vec![DiagInfo {
            sev: 1,
            msg: "Wrong type".into(),
            code: "2322".into(),
            url: Some("https://typescript.tv/errors/#TS2322".into()),
            line: 0,
            col: 0,
        }],
        (0, 1),
        (0, 0),
    );
    assert!(matches!(&t.blocks[0], Blk::Diag { code, .. } if code == "2322"));
    assert_eq!(t.actions(), [Act::QuickFix]);
    t.extra.diags.clear();
    t.compose();
    assert!(t.is_empty());
}

#[test]
fn scroll_is_clamped() {
    let mut t = HoverTip::new("```ts\nconst a: 1\n```\n", (0, 1), (0, 0));
    t.laid = Some(((0, 0, 0, 0, 0, 0), Laid { h: 500, ..Default::default() }));
    t.view = Rect::new(0, 0, 100, 200);
    t.scroll = 900.0;
    clamp_scroll(&mut t);
    assert_eq!(t.scroll, 300.0);
    t.scroll = -5.0;
    clamp_scroll(&mut t);
    assert_eq!(t.scroll, 0.0);
}

// ───────────── per-character selection ─────────────

fn tip_with(blocks: &[Blk], tx: &mut TextCtx) -> HoverTip {
    let th = dark();
    let mut hl = |_: &str, _: &str| None;
    let l = {
        let mut env = Env::new(tx, Pal::new(&th), &mut hl);
        layout(blocks, 400, &mut env)
    };
    let mut t = HoverTip::new("", (0, 0), (0, 0));
    t.laid = Some(((7, 7, 7, 7, 7, 0), l));
    t.view = Rect::new(10, 10, 400, 300);
    t
}

#[test]
fn selection_is_per_character_and_copies_exactly() {
    let mut tx = TextCtx::new(None);
    let blocks = vec![
        Blk::Para(vec![Span::plain("hello wonderful world")]),
        Blk::Code { text: "const a = 1;\nconst b = 22;".into(), lang: "ts".into() },
    ];
    let mut t = tip_with(&blocks, &mut tx);
    let boxes = boxes_of(&mut t, &mut tx, 0).to_vec();
    assert_eq!(boxes.iter().map(|b| b.ch).collect::<String>(), "hello wonderful world");
    assert!(
        boxes.windows(2).all(|w| w[0].x0 < w[1].x0 && w[0].x1 <= w[1].x0 + 1),
        "chars advance left to right: {boxes:?}"
    );
    // inside one line
    t.sel = Some(((0, 6), (0, 15)));
    assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("wonderful"));
    t.sel = Some(((0, 15), (0, 6)));
    assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("wonderful"), "order does not matter");
    let rects = sel_rects(&mut t, &mut tx);
    assert_eq!(rects.len(), 1);
    assert_eq!((rects[0].x, rects[0].right()), (boxes[6].x0, boxes[14].x1));
    // across lines: tail of the prose, a whole code line, head of the next
    t.sel = Some(((0, 16), (2, 5)));
    assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("world\nconst a = 1;\nconst"));
    assert_eq!(sel_rects(&mut t, &mut tx).len(), 3);
    // an empty selection copies nothing
    t.sel = Some(((1, 3), (1, 3)));
    assert_eq!(sel_text(&mut t, &mut tx), None);
    t.sel = None;
    assert_eq!(sel_text(&mut t, &mut tx), None);
}

#[test]
fn pointer_maps_to_the_nearest_char_boundary() {
    let mut tx = TextCtx::new(None);
    let mut t = tip_with(&[Blk::Para(vec![Span::plain("abcdef ghij")])], &mut tx);
    let b = boxes_of(&mut t, &mut tx, 0).to_vec();
    let (vx, vy) = (t.view.x as f64, t.view.y as f64 + 3.0);
    // just inside the left half of char 3 -> boundary 3; right half -> boundary 4
    assert_eq!(pos_at(&mut t, &mut tx, vx + b[3].x0 as f64 + 0.5, vy), Some((0, 3)));
    assert_eq!(pos_at(&mut t, &mut tx, vx + b[3].x1 as f64 - 0.5, vy), Some((0, 4)));
    // before the text / past the end / far below
    assert_eq!(pos_at(&mut t, &mut tx, vx - 30.0, vy), Some((0, 0)));
    assert_eq!(pos_at(&mut t, &mut tx, vx + 390.0, vy), Some((0, b.len())));
    assert_eq!(pos_at(&mut t, &mut tx, vx + 5.0, vy + 250.0).map(|p| p.0), Some(0));
}

#[test]
fn code_lines_select_on_the_monospace_grid() {
    let mut tx = TextCtx::new(None);
    let mut t = tip_with(&[Blk::Code { text: "let value = 42;".into(), lang: "ts".into() }], &mut tx);
    t.sel = Some(((0, 4), (0, 9)));
    assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("value"));
    let b = boxes_of(&mut t, &mut tx, 0).to_vec();
    let step = b[1].x0 - b[0].x0;
    assert!(step > 3 && b.windows(2).all(|w| (w[1].x0 - w[0].x0 - step).abs() <= 1), "fixed-width cells");
}

#[test]
fn inline_code_and_styled_runs_keep_their_text() {
    let mut tx = TextCtx::new(None);
    let sp = vec![
        Span::plain("use "),
        Span::code("add"),
        Span::plain(" or "),
        Span { text: "sum".into(), bold: true, ..Default::default() },
        Span::plain(" now"),
    ];
    let mut t = tip_with(&[Blk::Para(sp)], &mut tx);
    let b = boxes_of(&mut t, &mut tx, 0).to_vec();
    assert_eq!(b.iter().map(|c| c.ch).collect::<String>(), "use add or sum now");
    t.sel = Some(((0, 4), (0, 10)));
    assert_eq!(sel_text(&mut t, &mut tx).as_deref(), Some("add or"));
}
