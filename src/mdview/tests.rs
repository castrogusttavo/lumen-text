use super::*;
use std::path::{Path, PathBuf};

const README: &str = r#"# Lumen Text

[![CI](https://img.shields.io/ci.svg)](https://ci.example.com) ![logo](docs/logo.png)

A *fast*, **native** code editor with ~~no~~ little memory use. See the
[docs](https://example.com/docs) or `cargo run --release`.

## Features

- Syntax highlighting
- Multi cursor editing
  - nested one
  - nested two
- deeper
- Live markdown preview

1. First
2. Second
3. Third

- [ ] write tests
- [x] ship it

> Quoted wisdom
> continues here

### Install

```rust
fn main() {
println!("hello");
}
```

| Name | Description | Default |
|:-----|:-----------:|--------:|
| theme | Color theme | dark-plus |
| font | Monospace font family used for the code | `monospace` |

---

## Features

Last paragraph with a hard break
after it.
"#;

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}
fn theme() -> Theme {
    Theme::load(&assets().join("themes/dark-plus.json")).unwrap()
}
fn view(src: &str) -> MdView {
    let mut v = MdView::new();
    v.set_text(src);
    v
}
fn laid(src: &str, w: i32) -> (MdView, TextCtx) {
    let mut tx = TextCtx::new(None);
    let mut v = view(src);
    v.ensure_layout(&mut tx, w);
    (v, tx)
}
fn kinds(v: &MdView) -> Vec<&'static str> {
    v.blocks()
        .iter()
        .map(|b| match &b.kind {
            Kind::Heading { .. } => "h",
            Kind::Para(_) => "p",
            Kind::Code { .. } => "code",
            Kind::Rule => "rule",
            Kind::Quote(_) => "quote",
            Kind::List { .. } => "list",
            Kind::Table { .. } => "table",
            Kind::Images(_) => "img",
            Kind::Math(_) => "math",
            Kind::FootDef { .. } => "foot",
        })
        .collect()
}
fn big_doc(lines: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.lines().count() < lines {
        s.push_str(&format!("## Section {i}\n\nParagraph {i} with **bold** text, a [link](https://x.io/{i}) and `code {i}` plus some more words to wrap around the line a few times over.\n\n- item one {i}\n- item two {i}\n\n```rust\nlet x = {i};\n```\n\n| a | b |\n|---|---|\n| {i} | y |\n\n"));
        i += 1;
    }
    s
}

#[test]
fn parses_block_sequence() {
    let v = view(README);
    assert_eq!(
        kinds(&v),
        ["h", "img", "p", "h", "list", "list", "list", "quote", "h", "code", "table", "rule", "h", "p"]
    );
}

#[test]
fn heading_levels_and_source_lines() {
    let v = view(README);
    let bl = v.blocks();
    let Kind::Heading { level, .. } = &bl[0].kind else { panic!() };
    assert_eq!(*level, 1);
    assert_eq!((bl[0].line, bl[0].end), (0, 0));
    let code = bl.iter().find(|b| matches!(b.kind, Kind::Code { .. })).unwrap();
    let src_line = README.lines().position(|l| l.starts_with("```rust")).unwrap();
    assert_eq!(code.line, src_line);
    assert_eq!(code.end, src_line + 4);
    let hs: Vec<u8> =
        bl.iter().filter_map(|b| if let Kind::Heading { level, .. } = &b.kind { Some(*level) } else { None }).collect();
    assert_eq!(hs, [1, 2, 3, 2]);
}

#[test]
fn paragraph_ranges_cover_wrapped_source_lines() {
    let v = view(README);
    let bl = v.blocks();
    let p = &bl[2];
    let start = README.lines().position(|l| l.starts_with("A *fast*")).unwrap();
    assert_eq!((p.line, p.end), (start, start + 1));
}

#[test]
fn inline_styles_are_recorded() {
    let v = view("a *em* **st** ~~del~~ `co` [ln](http://u) z");
    let bl = v.blocks();
    let Kind::Para(inl) = &bl[0].kind else { panic!() };
    let f = |t: &str| {
        inl.iter().find_map(|i| match i {
            Inl::Text { text, flags, link } if text == t => Some((*flags, *link)),
            _ => None,
        })
    };
    assert_eq!(f("em").unwrap().0, F_I);
    assert_eq!(f("st").unwrap().0, F_B);
    assert_eq!(f("del").unwrap().0, F_S);
    assert_eq!(f("co").unwrap().0, F_CODE);
    assert_eq!(f("ln").unwrap(), (F_LINK, 1));
    assert_eq!(v.urls.list, ["http://u"]);
}

#[test]
fn nested_emphasis_combines_flags() {
    let v = view("***both*** and **bold *inner* text**");
    let bl = v.blocks();
    let Kind::Para(inl) = &bl[0].kind else { panic!() };
    assert!(inl.iter().any(|i| matches!(i, Inl::Text { text, flags, .. } if text == "both" && *flags == F_B | F_I)));
    assert!(inl.iter().any(|i| matches!(i, Inl::Text { text, flags, .. } if text == "inner" && *flags == F_B | F_I)));
}

#[test]
fn tight_list_items_get_implicit_paragraphs_and_nest() {
    let v = view("- one\n- two\n  - sub a\n  - sub b\n    - deep\n- three\n");
    let bl = v.blocks();
    let Kind::List { items, loose, start } = &bl[0].kind else { panic!() };
    assert!(!loose && start.is_none());
    assert_eq!(items.len(), 3);
    assert!(matches!(items[0].blocks[0].kind, Kind::Para(_)));
    let Kind::List { items: sub, .. } = &items[1].blocks[1].kind else { panic!("no nested list") };
    assert_eq!(sub.len(), 2);
    assert!(matches!(&sub[1].blocks[1].kind, Kind::List { .. }));
    // source lines of the implicit paragraph
    assert_eq!((items[2].blocks[0].line, items[2].blocks[0].end), (5, 5));
}

#[test]
fn loose_and_ordered_lists() {
    let v = view("3. a\n\n4. b\n\n5. c\n");
    let bl = v.blocks();
    let Kind::List { items, loose, start } = &bl[0].kind else { panic!() };
    assert!(*loose);
    assert_eq!(*start, Some(3));
    assert_eq!(items.len(), 3);
}

#[test]
fn task_lists() {
    let v = view("- [ ] todo\n- [x] done\n- plain\n");
    let bl = v.blocks();
    let Kind::List { items, .. } = &bl[0].kind else { panic!() };
    assert_eq!(items.iter().map(|i| i.task).collect::<Vec<_>>(), [Some(false), Some(true), None]);
}

#[test]
fn code_blocks_fenced_and_indented() {
    let v = view("```js {1}\nlet a = 1;\n\nlet b;\n```\n\n    indented\n    code\n");
    let bl = v.blocks();
    let Kind::Code { lang, text } = &bl[0].kind else { panic!() };
    assert_eq!(lang, "js");
    assert_eq!(text, "let a = 1;\n\nlet b;");
    let Kind::Code { lang, text } = &bl[1].kind else { panic!() };
    assert_eq!((lang.as_str(), text.as_str()), ("", "indented\ncode"));
}

#[test]
fn table_is_parsed_with_alignment() {
    let v = view(README);
    let bl = v.blocks();
    let Some(Kind::Table { aligns, head, rows }) =
        bl.iter().find_map(|b| if let Kind::Table { .. } = &b.kind { Some(b.kind.clone()) } else { None })
    else {
        panic!()
    };
    assert_eq!(aligns, [Alignment::Left, Alignment::Center, Alignment::Right]);
    assert_eq!(head.len(), 3);
    assert_eq!(rows.len(), 2);
    assert!(rows[1][2].iter().any(|i| matches!(i, Inl::Text { flags, .. } if *flags == F_CODE)));
}

#[test]
fn images_become_boxes_and_keep_links() {
    let v = view("[![CI](a.svg)](http://ci) ![](b.png)\n\ntext ![inline](c.png) tail\n");
    let bl = v.blocks();
    let Kind::Images(im) = &bl[0].kind else { panic!() };
    assert_eq!(im.len(), 2);
    assert_eq!(im[0].alt, "CI");
    assert_eq!(v.urls.list[im[0].link as usize - 1], "http://ci");
    assert_eq!(im[1].link, 0);
    assert_eq!(kinds(&v), ["img", "p", "img", "p"]);
}

#[test]
fn raw_html_img_and_comments() {
    let v = view(
        "<!-- hidden -->\n\n<p align=\"center\"><a href=\"http://h\"><img src=\"x/logo.png\" alt=\"Logo\"></a></p>\n\nafter <br> br\n",
    );
    let bl = v.blocks();
    let Kind::Images(im) = &bl[0].kind else { panic!("{:?}", kinds(&v)) };
    assert_eq!((im[0].alt.as_str(), im[0].url.as_str()), ("Logo", "x/logo.png"));
    assert_eq!(v.urls.list[im[0].link as usize - 1], "http://h");
    assert_eq!(bl.len(), 2);
}

#[test]
fn hard_and_soft_breaks() {
    let v = view("a  \nb\nc\\\nd");
    let bl = v.blocks();
    let Kind::Para(inl) = &bl[0].kind else { panic!() };
    assert_eq!(inl.iter().filter(|i| **i == Inl::Hard).count(), 2);
    assert_eq!(inl.iter().filter(|i| **i == Inl::Soft).count(), 1);
}

#[test]
fn slugify_cases() {
    assert_eq!(slugify("Hello World"), "hello-world");
    assert_eq!(slugify("What's new? (v1.2)"), "whats-new-v12");
    assert_eq!(slugify("  Trim me  "), "trim-me");
    assert_eq!(slugify("snake_case-and-kebab"), "snake_case-and-kebab");
    assert_eq!(slugify("C++ & Rust"), "c--rust");
}

#[test]
fn slugify_keeps_unicode_letters() {
    assert_eq!(slugify("Ação e Reação"), "ação-e-reação");
    assert_eq!(slugify("日本語 見出し"), "日本語-見出し");
    assert_eq!(slugify("Über Straße"), "über-straße");
    assert_eq!(slugify("🚀 Launch"), "-launch");
}

#[test]
fn slugify_duplicates_get_suffixes() {
    let mut seen = HashMap::new();
    let s: Vec<String> =
        ["Intro", "Intro", "Intro", "Other", "intro"].iter().map(|h| slugify_unique(h, &mut seen)).collect();
    assert_eq!(s, ["intro", "intro-1", "intro-2", "other", "intro-3"]);
}

#[test]
fn heading_anchors_in_order_with_dedupe() {
    let (v, _tx) = laid(README, 800);
    let a = v.heading_anchors();
    let names: Vec<&str> = a.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(names, ["lumen-text", "features", "install", "features-1"]);
    assert!(a.windows(2).all(|w| w[0].1 < w[1].1));
    assert_eq!(a[0].1, PAD);
}

#[test]
fn paragraph_height_grows_as_width_shrinks() {
    let para = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. ".repeat(6);
    let mut tx = TextCtx::new(None);
    let mut v = view(&para);
    let mut last = 0;
    for w in [1400, 900, 700, 500, 360, 240] {
        v.ensure_layout(&mut tx, w);
        let h = v.content_height();
        assert!(h >= last, "height {h} at width {w} shrank below {last}");
        last = h;
    }
    v.ensure_layout(&mut tx, 1400);
    let wide = v.content_height();
    assert!(last > wide + LH[BODY] * 3, "narrow {last} vs wide {wide}");
    // 2 * PAD padding + whole body lines
    assert_eq!((wide - 2 * PAD) % LH[BODY], 0);
}

#[test]
fn long_unbreakable_word_is_split() {
    let word = "x".repeat(400);
    let (v, _) = laid(&word, 300);
    assert!(v.content_height() > 2 * PAD + LH[BODY] * 2, "{}", v.content_height());
}

#[test]
fn text_column_is_capped_and_centered() {
    let (v, _) = laid("hello world", 1600);
    let lay = v.lay.as_ref().unwrap();
    assert_eq!(lay.col_w, MAX_COL);
    assert_eq!(lay.col_x, (1600 - MAX_COL) / 2);
    let (v, _) = laid("hello world", 400);
    let lay = v.lay.as_ref().unwrap();
    assert_eq!((lay.col_x, lay.col_w), (PAD, 400 - 2 * PAD));
}

#[test]
fn code_block_does_not_wrap() {
    let long = "let v = ".to_string() + &"1234567890, ".repeat(40);
    let src = format!("```rust\n{long}\nshort\n```\n");
    let (mut v, mut tx) = laid(&src, 1200);
    let h_wide = v.content_height();
    v.ensure_layout(&mut tx, 300);
    assert_eq!(v.content_height(), h_wide);
    // panel = padding + 2 lines + padding (the language label does not take a row), plus the
    // document padding
    assert_eq!(h_wide, 2 * PAD + 2 * CODE_PAD_Y + 2 * code_lh());
}

#[test]
fn list_nesting_indents_text() {
    let (v, _) = laid("- a\n  - b\n    - c\n", 800);
    let _lay = v.lay.as_ref().unwrap();
    let xs: Vec<i32> = v
        .ops()
        .into_iter()
        .filter_map(|o| if let Op::Text { x, text, .. } = &o.op { Some((text.as_str(), *x)) } else { None })
        .map(|(_, x)| x)
        .collect();
    assert_eq!(xs.len(), 3);
    assert_eq!(xs[1] - xs[0], LIST_BULLET_INDENT);
    assert_eq!(xs[2] - xs[1], LIST_BULLET_INDENT);
    // three bullets of increasing depth, hanging to the left of their text
    let bullets: Vec<(i32, u8)> = v
        .ops()
        .into_iter()
        .filter_map(|o| if let Op::Bullet { cx, level, .. } = o.op { Some((cx, level)) } else { None })
        .collect();
    assert_eq!(bullets.iter().map(|b| b.1).collect::<Vec<_>>(), [0, 1, 2]);
    assert!(bullets.iter().zip(&xs).all(|(b, x)| b.0 < *x));
}

#[test]
fn ordered_list_indent_grows_with_number_width() {
    let items: String = (1..=12).map(|i| format!("{i}. item\n")).collect();
    let (v, _) = laid(&items, 800);
    let (v1, _) = laid("1. item\n2. item\n", 800);
    let x = |v: &MdView| {
        v.ops()
            .into_iter()
            .filter_map(|o| if let Op::Text { x, text, .. } = &o.op { (text == "item").then_some(*x) } else { None })
            .next()
            .unwrap()
    };
    assert!(x(&v) > x(&v1));
}

/// The rounded frame of the (first) table.
fn table_frame(v: &MdView) -> Rect {
    v.ops()
        .into_iter()
        .find_map(|o| match &o.op {
            Op::Card { r, rad, .. } if *rad == TABLE_RAD => Some(*r),
            _ => None,
        })
        .expect("table frame")
}

/// Height of a table row whose tallest cell has `n` lines.
fn row_h(n: i32) -> i32 {
    n * LH[CELL] + 2 * CELL_PAD_Y
}

#[test]
fn table_column_widths_follow_content() {
    let src = "| a | long header here |\n|---|---|\n| short | x |\n| much longer cell text | y |\n";
    let (v, _) = laid(src, 900);
    let lay = v.lay.as_ref().unwrap();
    let frame = table_frame(&v);
    assert_eq!(frame.h, 3 * row_h(1), "short cells stay on one line");
    let verts: Vec<i32> = v
        .ops()
        .into_iter()
        .filter_map(|o| if let Op::Fill { r, .. } = &o.op { (r.w == 1 && r.h == frame.h).then_some(r.x) } else { None })
        .collect();
    assert_eq!(verts.len(), 1, "one inner column rule");
    let first = verts[0] - lay.col_x;
    assert!(first > 100, "first column sized by 'much longer cell text', got {first}");
    assert!(frame.w < 900 - 2 * PAD);
    // Header row: tinted, semibold; cells at body size.
    assert!(v.ops().into_iter().any(|o| matches!(o.op, Op::Fill { col: Col::Head, .. })));
    let head = v
        .ops()
        .into_iter()
        .find_map(|o| match &o.op {
            Op::Text { text, flags, si, .. } if text == "a" => Some((*flags, *si)),
            _ => None,
        })
        .unwrap();
    assert!(head.0 & F_B != 0);
    assert_eq!(SIZES[head.1 as usize], SIZES[BODY]);
}

#[test]
fn narrow_table_wraps_cells_instead_of_ellipsizing() {
    let src = "| a | b |\n|---|---|\n| aaaaaaaa bbbbbbbb cccccccc dddddddd eeeeeeee | zzzzzzzz yyyyyyyy xxxxxxxx wwwwwwww |\n";
    let (v, _) = laid(src, 260);
    let frame = table_frame(&v);
    assert!(frame.w <= 260 - 2 * PAD + 2, "table width {}", frame.w);
    let texts: Vec<(i32, i32, i32, String)> = v
        .ops()
        .into_iter()
        .filter_map(|o| if let Op::Text { x, w, text, .. } = &o.op { Some((*x, o.y0, *w, text.clone())) } else { None })
        .collect();
    assert!(texts.iter().all(|t| !t.3.ends_with("...")), "nothing is cut");
    // Every letter is drawn (a word wider than its column breaks between characters).
    let all: String = texts.iter().map(|t| t.3.as_str()).collect();
    for ch in ['a', 'e', 'z', 'w'] {
        assert_eq!(all.chars().filter(|&c| c == ch).count(), 8 + usize::from(ch == 'a'), "{ch} in {all}");
    }
    // The body row grew to fit its wrapped lines, and the frame encloses every line.
    assert!(frame.h > row_h(1) + row_h(2), "frame {}", frame.h);
    assert!(texts.iter().all(|t| t.0 >= frame.x && t.0 + t.2 <= frame.right() && t.1 + LH[CELL] <= frame.bottom()));
    // Following content is placed below the taller table.
    let (v2, _) = laid(&format!("{src}\nafter\n"), 260);
    assert_eq!(v2.content_height(), 2 * PAD + frame.h + BLOCK_GAP + LH[BODY]);
}

#[test]
fn long_cells_cap_their_column_and_wrap() {
    let prose = "word ".repeat(60);
    let src = format!("| key | notes |\n|---|---|\n| k | {} |\n", prose.trim());
    let (v, mut tx) = laid(&src, 1400);
    let frame = table_frame(&v);
    let mut m = Measure::default();
    let cap = (m.w(&mut tx, "0", false, CELL) * CELL_MAX_CH).ceil() as i32;
    assert!(frame.w <= cap + 2 * CELL_PAD_X + 80, "frame {} cap {cap}", frame.w);
    assert!(frame.w < 736, "a single prose column does not take the whole measure");
    let words: Vec<i32> = v
        .ops()
        .into_iter()
        .filter_map(|o| if let Op::Text { text, .. } = &o.op { text.contains("word").then_some(o.y0) } else { None })
        .collect();
    let mut ys = words;
    ys.dedup();
    assert!(ys.len() >= 3, "the notes cell wraps over several lines: {ys:?}");
    assert!(ys.windows(2).all(|w| w[1] - w[0] == LH[CELL]));
}

#[test]
fn links_inside_wrapped_table_cells_are_clickable() {
    let src = "| a | b |\n|---|---|\n| aaaaaaaa bbbbbbbb cccccccc [target](https://t.io) | z |\n";
    let mut tx = TextCtx::new(None);
    let mut v = view(src);
    v.ensure_layout(&mut tx, 260);
    let rect = Rect::new(0, 0, 260, 600);
    let hit = v.all_links()[0];
    // (all_links y is absolute; the table is the first segment, at y = PAD)
    assert!(hit.y >= PAD + row_h(1) + CELL_PAD_Y + LH[CELL], "the link wrapped below the first body line");
    let (px, py) = ((hit.x + hit.w / 2) as f64, (hit.y + hit.h / 2) as f64);
    assert_eq!(v.link_at(px, py, rect, 0.0).as_deref(), Some("https://t.io"));
}

#[test]
fn headings_have_no_rule_and_quotes_are_not_dimmed() {
    let (v, _) = laid("# Title\n\nbody\n\n> quoted text\n\n---\n", 800);
    let lay = v.lay.as_ref().unwrap();
    let rules: Vec<Col> = v
        .ops()
        .into_iter()
        .filter_map(|o| match o.op {
            Op::Fill { r, col, .. } if r.h == 1 && r.w == lay.col_w => Some(col),
            _ => None,
        })
        .collect();
    assert_eq!(rules, [Col::Rule], "only the thematic break draws a full-width line");
    let quoted = v
        .ops()
        .into_iter()
        .find_map(|o| match &o.op {
            Op::Text { x, text, flags, .. } if text.contains("quoted") => Some((*x, *flags)),
            _ => None,
        })
        .unwrap();
    assert_eq!(quoted.1 & F_DIM, 0);
    assert_eq!(quoted.0, lay.col_x + QUOTE_INDENT);
    assert!(v.ops().into_iter().any(|o| matches!(o.op, Op::Fill { r, col: Col::Bar, .. } if r.w == QUOTE_BAR)));
}

#[test]
fn palette_softens_prose_but_keeps_it_readable() {
    let t = theme();
    let p = Pal::new(&t);
    assert_ne!(p.text, p.fg, "body text is softer than headings");
    assert!(p.text.contrast(p.bg) >= 7.0f32.min(p.fg.contrast(p.bg)));
    assert!(p.dim.contrast(p.bg) >= 4.5);
    assert!(p.text.contrast(p.code) >= 4.5, "code reads on its chip");
    // Neutral overlays, faintest first.
    let d = |c: Rgba| c.contrast(p.bg);
    assert!(d(p.code) < d(p.border) && d(p.border) < d(p.rule) && d(p.rule) <= d(p.bar));
}

#[test]
fn task_boxes_are_centered_on_the_first_line() {
    let (v, _) = laid("- [ ] one\n- [x] two\n", 800);
    let boxes: Vec<(i32, i32)> = v
        .ops()
        .into_iter()
        .filter_map(|o| if let Op::Check { x, y, .. } = o.op { Some((x, y)) } else { None })
        .collect();
    assert_eq!(boxes.len(), 2);
    for (c, (bx, by)) in v.all_checks().iter().zip(&boxes) {
        // (op y is relative to the segment, which starts at PAD; hit boxes are absolute)
        assert_eq!(by + PAD - c.y, (LH[BODY] - CHECK) / 2);
        assert!(c.x <= *bx && c.x + c.w >= bx + CHECK, "the hit box covers the drawn box");
    }
}

#[test]
fn fit_columns_water_fills() {
    assert_eq!(fit_columns(&[50, 60], 200), [50, 60]);
    assert_eq!(fit_columns(&[50, 400], 200), [50, 150]);
    let c = fit_columns(&[300, 300], 200);
    assert_eq!(c, [100, 100]);
}

#[test]
fn line_mapping_is_monotonic_and_roughly_inverse() {
    let (v, _) = laid(&big_doc(400), 700);
    let n = v.line_count;
    let mut last = -1;
    for l in 0..n {
        let y = v.y_for_source_line(l).unwrap();
        assert!(y >= last, "y_for_source_line not monotonic at {l}: {y} < {last}");
        last = y;
    }
    assert!(last <= v.content_height());
    let mut prev_line = 0;
    for y in (0..v.content_height()).step_by(3) {
        let l = v.source_line_at_y(y);
        assert!(l >= prev_line, "source_line_at_y not monotonic at y={y}: {l} < {prev_line}");
        assert!(l < n);
        prev_line = l;
    }
}

#[test]
fn line_mapping_round_trips_within_tolerance() {
    let (v, _) = laid(&big_doc(300), 640);
    let mut worst = 0usize;
    for l in 0..v.line_count - 1 {
        let y = v.y_for_source_line(l).unwrap();
        let back = v.source_line_at_y(y);
        worst = worst.max(back.abs_diff(l));
    }
    assert!(worst <= 2, "worst round-trip error {worst} lines");
}

#[test]
fn code_block_lines_interpolate_linearly() {
    let body: String = (0..20).map(|i| format!("line {i}\n")).collect();
    let (v, _) = laid(&format!("```\n{body}```\n"), 700);
    let ys: Vec<i32> = (1..=20).map(|l| v.y_for_source_line(l).unwrap()).collect();
    let lh = code_lh();
    for w in ys.windows(2) {
        assert!((w[1] - w[0] - lh).abs() <= 2, "step {} vs line height {lh}", w[1] - w[0]);
    }
}

#[test]
fn mapping_edges() {
    let (v, _) = laid("# T\n\ntext\n", 600);
    assert_eq!(v.y_for_source_line(0), Some(PAD));
    assert_eq!(v.y_for_source_line(10_000), Some(v.content_height()));
    assert_eq!(v.source_line_at_y(-50), 0);
    assert_eq!(v.source_line_at_y(1_000_000), v.line_count - 1);
    assert_eq!(MdView::new().y_for_source_line(3), None);
}

#[test]
fn link_at_hits_and_misses() {
    let mut tx = TextCtx::new(None);
    let mut v = view("[click me](https://example.com/a) and plain text here\n");
    v.ensure_layout(&mut tx, 800);
    let rect = Rect::new(100, 50, 800, 400);
    let _lay = v.lay.as_ref().unwrap();
    let hit = v.all_links()[0];
    let (px, py) = (rect.x as f64 + (hit.x + hit.w / 2) as f64, rect.y as f64 + (hit.y + hit.h / 2) as f64);
    assert_eq!(v.link_at(px, py, rect, 0.0).as_deref(), Some("https://example.com/a"));
    // scrolled down by 7px the same content point sits 7px higher
    assert_eq!(v.link_at(px, py - 7.0, rect, 7.0).as_deref(), Some("https://example.com/a"));
    // plain text to the right of the link, above, below, and outside the pane
    let right = rect.x as f64 + (hit.x + hit.w + 40) as f64;
    assert_eq!(v.link_at(right, py, rect, 0.0), None);
    assert_eq!(v.link_at(px, rect.y as f64 + 2.0, rect, 0.0), None);
    assert_eq!(v.link_at(px, rect.y as f64 + 300.0, rect, 0.0), None);
    assert_eq!(v.link_at(5.0, py, rect, 0.0), None);
}

#[test]
fn badge_image_links_are_clickable() {
    let mut tx = TextCtx::new(None);
    let mut v = view("[![CI](a.svg)](http://ci)\n");
    v.ensure_layout(&mut tx, 800);
    let rect = Rect::new(0, 0, 800, 300);
    let h = v.all_links()[0];
    assert_eq!(v.link_at((h.x + 5) as f64, (h.y + 5) as f64, rect, 0.0).as_deref(), Some("http://ci"));
}

#[test]
fn content_height_is_stable_and_relayout_is_lazy() {
    let mut tx = TextCtx::new(None);
    let t = theme();
    let mut v = view(README);
    let rect = Rect::new(10, 10, 700, 300);
    let mut px = vec![0u32; 720 * 320];
    let mut cv = Canvas { px: &mut px, w: 720, h: 320 };
    v.draw(&mut cv, &mut tx, &t, rect, 0.0);
    let h1 = v.content_height();
    v.draw(&mut cv, &mut tx, &t, rect, 120.0);
    v.draw(&mut cv, &mut tx, &t, rect, 0.0);
    assert_eq!(v.content_height(), h1);
    assert_eq!(v.layout_count(), 1, "scrolling must not re-layout");
    v.set_text(README);
    v.draw(&mut cv, &mut tx, &t, rect, 0.0);
    assert_eq!(v.layout_count(), 1, "same text is a no-op");
    v.set_text(&format!("{README}\nmore\n"));
    v.draw(&mut cv, &mut tx, &t, rect, 0.0);
    assert_eq!(v.layout_count(), 2);
    v.draw(
        &mut cv,
        &mut tx,
        &Theme::load(&assets().join("themes/dark-plus.json")).unwrap(),
        Rect::new(10, 10, 500, 300),
        0.0,
    );
    assert_eq!(v.layout_count(), 3, "width change re-lays out");
}

#[test]
fn empty_document() {
    let mut tx = TextCtx::new(None);
    let mut v = view("");
    v.ensure_layout(&mut tx, 500);
    assert_eq!(v.content_height(), 2 * PAD);
    assert!(v.heading_anchors().is_empty());
    assert_eq!(v.source_line_at_y(10), 0);
}

#[test]
fn ui_width_sized_scales_with_size() {
    let mut tx = TextCtx::new(None);
    let a = tx.ui_width_sized("Hello world", false, 14.0);
    let b = tx.ui_width_sized("Hello world", false, 28.0);
    assert!(a > 20);
    assert!((b as f32 / a as f32 - 2.0).abs() < 0.15, "{a} vs {b}");
    assert!(tx.ui_width_sized(" ", false, 14.0) > 0);
    assert!(tx.ui_width_sized("Hello world", true, 14.0) >= a);
}

#[test]
fn draw_paints_inside_rect_only() {
    let mut tx = TextCtx::new(None);
    let t = theme();
    let (w, h) = (640, 480);
    let sentinel = 0x00ab_cdefu32;
    let mut px = vec![sentinel; (w * h) as usize];
    let rect = Rect::new(40, 30, 500, 380);
    let mut v = view(README);
    {
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, rect, 0.0);
    }
    let mut inside_changed = 0;
    for y in 0..h {
        for x in 0..w {
            let p = px[(y * w + x) as usize];
            if rect.contains(x as f64, y as f64) {
                inside_changed += (p != sentinel) as i32;
            } else {
                assert_eq!(p, sentinel, "pixel outside the rect was touched at ({x},{y})");
            }
        }
    }
    assert_eq!(inside_changed, rect.w * rect.h, "background must fill the whole rect");
    let bg = t.background.with_alpha(255);
    let bgpx = ((bg.0 as u32) << 16) | ((bg.1 as u32) << 8) | bg.2 as u32;
    let ink = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|&(x, y)| rect.contains(x as f64, y as f64) && px[(y * w + x) as usize] != bgpx)
        .count();
    assert!(ink > 1500, "only {ink} pixels differ from the background");
    // scrolled far down shows a different picture
    let mut px2 = vec![sentinel; (w * h) as usize];
    let mut cv = Canvas { px: &mut px2, w, h };
    let max_scroll = (v.content_height() - rect.h).max(1) as f64;
    v.draw(&mut cv, &mut tx, &t, rect, max_scroll);
    assert_ne!(px, px2);
}

#[test]
fn draw_clips_to_a_rect_inside_the_canvas_for_every_block_kind() {
    // pane in the middle of the canvas, scrolled so blocks straddle both edges
    let mut tx = TextCtx::new(None);
    let t = theme();
    let (w, h) = (500, 400);
    let sentinel = 0x0011_2233u32;
    let rect = Rect::new(60, 50, 300, 200);
    let mut v = view(README);
    for scroll in [0.0, 37.5, 90.0, 210.0, 333.0] {
        let mut px = vec![sentinel; (w * h) as usize];
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, rect, scroll);
        for y in 0..h {
            for x in 0..w {
                if !rect.contains(x as f64, y as f64) {
                    assert_eq!(px[(y * w + x) as usize], sentinel, "leak at ({x},{y}) scroll {scroll}");
                }
            }
        }
    }
}

#[test]
fn layout_time_for_5000_lines() {
    let src = big_doc(5000);
    assert!(src.lines().count() >= 5000);
    let mut tx = TextCtx::new(None);
    let mut v = MdView::new();
    let t0 = std::time::Instant::now();
    v.set_text(&src);
    let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
    v.ensure_layout(&mut tx, 900);
    let cold = v.last_layout_ms();
    v.ensure_layout(&mut tx, 700);
    let warm = v.last_layout_ms();
    eprintln!(
        "[mdview] 5000 lines ({} KB): parse {parse_ms:.1} ms, layout cold {cold:.1} ms, relayout warm {warm:.1} ms, height {} px",
        src.len() / 1024,
        v.content_height()
    );
    assert!(warm < 1500.0 && cold < 15_000.0);
}
#[test]
fn layout_time_for_5000_lines_of_varied_prose() {
    // ~5000 source lines, thousands of distinct words (worst case for the width cache)
    let mut seed = 12345u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let word = |r: usize| {
        let n = 3 + r % 8;
        (0..n).map(|k| (b'a' + ((r / (k + 3) + k * 7) % 26) as u8) as char).collect::<String>()
    };
    let mut src = String::new();
    for i in 0..5000 {
        if i % 40 == 0 {
            src.push_str(&format!("\n## Heading {}\n\n", word(rnd())));
        }
        let n = 8 + rnd() % 14;
        let line: Vec<String> = (0..n).map(|_| word(rnd() % 30_000)).collect();
        src.push_str(&line.join(" "));
        src.push('\n');
    }
    let mut tx = TextCtx::new(None);
    let mut v = MdView::new();
    v.set_text(&src);
    v.ensure_layout(&mut tx, 900);
    let cold = v.last_layout_ms();
    v.ensure_layout(&mut tx, 700);
    let warm = v.last_layout_ms();
    eprintln!(
        "[mdview] varied prose {} KB: layout cold {cold:.0} ms, relayout warm {warm:.0} ms, height {} px",
        src.len() / 1024,
        v.content_height()
    );
    assert!(warm < 1000.0);
}

// ---- incremental layout, footnotes, math, tasks, images, highlighting ----

fn unit(i: usize) -> String {
    format!(
        "## Section {i}\n\nParagraph {i} with **bold** text, a [link](https://x.io/{i}) and `code {i}` plus some more words to wrap around the line a few times over.\n\n- item one {i}\n- item two {i}\n\n```rust\nlet x = {i};\n```\n\n| a | b |\n|---|---|\n| {i} | y |\n\n"
    )
}

fn doc_of(units: usize) -> String {
    (0..units).map(unit).collect()
}

#[test]
fn split_cuts_at_blank_lines_but_keeps_fences_lists_and_indents_together() {
    let t = "# T\n\npara one\nstill\n\n```rust\nlet a;\n\nlet b;\n```\n\n- a\n\n- b\n\n  cont\n\nnext para\n\n    code\n\n<!-- a\n\nb -->\n\nlast\n";
    let sp = split(t);
    let segs: Vec<&str> = sp.segs.iter().map(|s| &t[s.b0..s.b1]).collect();
    assert_eq!(
        segs,
        [
            "# T\n",
            "para one\nstill\n",
            "```rust\nlet a;\n\nlet b;\n```\n",
            "- a\n\n- b\n\n  cont\n",
            "next para\n\n    code\n",
            "<!-- a\n\nb -->\n",
            "last\n"
        ]
    );
    for s in &sp.segs {
        assert_eq!(t[..s.b0].matches('\n').count(), s.l0, "first line of {:?}", &t[s.b0..s.b1]);
        assert_eq!(t[s.b0..s.b1].trim_end_matches('\n').matches('\n').count() + 1, s.nl);
    }
    assert_eq!(sp.lines, t.matches('\n').count() + 1);
}

#[test]
fn split_finds_reference_and_footnote_definitions() {
    let sp = split("[ref]: https://a.dev/x \"t\"\n\n[^n]: note\n\n```\n[hidden]: nope\n```\n");
    assert_eq!(sp.ctx.refs.get("ref").map(String::as_str), Some("https://a.dev/x"));
    assert!(sp.ctx.foot.contains("n"));
    assert!(!sp.ctx.refs.contains_key("hidden"), "definitions inside a fence do not count");
}

/// Everything hit-testing and scroll sync can see must be the same for an incrementally edited
/// view and one built from scratch.
fn assert_same_layout(a: &MdView, b: &MdView, what: &str) {
    assert_eq!(a.content_height(), b.content_height(), "{what}: height");
    assert_eq!(a.heading_anchors(), b.heading_anchors(), "{what}: headings");
    for l in (0..a.line_count).step_by(3) {
        assert_eq!(a.y_for_source_line(l), b.y_for_source_line(l), "{what}: line {l}");
    }
    assert_eq!(a.all_links().len(), b.all_links().len(), "{what}: links");
    for y in (0..a.content_height()).step_by(7) {
        assert_eq!(a.source_line_at_y(y), b.source_line_at_y(y), "{what}: y {y}");
    }
}

#[test]
fn an_edit_reparses_and_relays_out_one_segment_and_matches_a_fresh_layout() {
    let mut tx = TextCtx::new(None);
    let src = doc_of(300);
    let mut v = MdView::new();
    v.set_text(&src);
    v.ensure_layout(&mut tx, 800);
    let (p0, l0) = v.work_counts();
    assert!(p0 >= 1000 && l0 >= 1000, "cold: {p0} parsed, {l0} laid out");
    let edits: Vec<(&str, Box<dyn Fn(&str) -> String>)> = vec![
        ("typing in a paragraph", Box::new(|s| s.replacen("Paragraph 150 with", "Paragraph 150 X with", 1))),
        (
            "making a paragraph longer",
            Box::new(|s| s.replacen("Paragraph 77 with", &format!("Paragraph 77 {} with", "wrap ".repeat(60)), 1)),
        ),
        ("editing a heading", Box::new(|s| s.replacen("## Section 200", "## Section two hundred", 1))),
        (
            "splitting a segment with a blank line",
            Box::new(|s| {
                s.replacen("- item one 40\n- item two 40", "- item one 40\n\nmiddle text\n\n- item two 40", 1)
            }),
        ),
        (
            "joining two segments",
            Box::new(|s| s.replacen("## Section 9\n\nParagraph 9", "## Section 9\nParagraph 9", 1)),
        ),
    ];
    for (what, f) in edits {
        let new = f(&v.text);
        assert_ne!(new, v.text, "{what}: the edit applies");
        let (p, l) = v.work_counts();
        v.set_text(&new);
        v.ensure_layout(&mut tx, 800);
        let (p2, l2) = v.work_counts();
        assert!(p2 - p <= 3 && l2 - l <= 3, "{what}: {} parsed, {} laid out", p2 - p, l2 - l);
        let mut fresh = MdView::new();
        fresh.set_text(&new);
        fresh.ensure_layout(&mut tx, 800);
        assert_same_layout(&v, &fresh, what);
    }
}

#[test]
fn a_width_change_lays_out_again_but_does_not_reparse() {
    let mut tx = TextCtx::new(None);
    let mut v = MdView::new();
    v.set_text(&doc_of(20));
    v.ensure_layout(&mut tx, 800);
    let (p, l) = v.work_counts();
    v.ensure_layout(&mut tx, 500);
    let (p2, l2) = v.work_counts();
    assert_eq!(p2, p);
    assert!(l2 > l);
    v.ensure_layout(&mut tx, 800);
    assert_eq!(v.work_counts().1, l2 + (l2 - l), "going back re-lays out (one cache slot per segment)");
}

/// Median and worst time of one keystroke (rope -> string, `set_text`, draw) in a document of
/// `units` sections, edited in the middle.
fn typing_bench(units: usize, label: &str) -> (f64, f64) {
    let mut tx = TextCtx::new(None);
    let t = theme();
    let src = doc_of(units);
    let mut v = MdView::new();
    let t0 = std::time::Instant::now();
    v.set_text(&src);
    v.ensure_layout(&mut tx, 900);
    let cold = t0.elapsed().as_secs_f64() * 1000.0;
    let mut rope = ropey::Rope::from_str(&src);
    let mid = units / 2;
    let at = rope.byte_to_char(src.find(&format!("Paragraph {mid} with")).unwrap() + 14);
    let (w, h) = (900, 700);
    let mut px = vec![0u32; (w * h) as usize];
    let rect = Rect::new(0, 0, w, h);
    let scroll = v.y_for_source_line(mid * 17).unwrap() as f64;
    let mut times = vec![];
    for i in 0..40 {
        let t0 = std::time::Instant::now();
        rope.insert(at + i, "x");
        let text = rope.to_string(); // what the app does with the buffer on every keystroke
        v.set_text(&text);
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, rect, scroll);
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (median, worst) = (times[times.len() / 2], times[times.len() - 1]);
    eprintln!(
        "[mdview] {label} ({} KB, {} lines): cold {cold:.0} ms; per keystroke median {median:.1} ms, worst {worst:.1} ms",
        src.len() / 1024,
        v.line_count
    );
    drop((v, rope, src, px));
    // Give the memory back: `perf::memory_100k_buffer_and_tokenizer` measures the whole test process.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0)
    };
    (median, worst)
}

#[test]
fn typing_in_a_large_document_stays_under_16ms() {
    // 200 KB here (the full 2 MB run below is ignored by default: it inflates the process
    // memory that `perf::memory_100k_buffer_and_tokenizer` measures when tests run in parallel).
    let (median, _) = typing_bench(850, "200 KB");
    if !cfg!(debug_assertions) {
        assert!(median < 16.0, "median keystroke {median:.1} ms");
    }
}

/// `cargo test --release typing_in_a_2mb -- --ignored --nocapture`
#[test]
#[ignore]
fn typing_in_a_2mb_document_stays_under_16ms() {
    let (median, _) = typing_bench(9500, "2 MB");
    assert!(median < 16.0, "median keystroke {median:.1} ms");
}

#[test]
fn footnotes_are_numbered_by_first_reference_and_collected_at_the_end() {
    let src = "A[^b] and B[^a] again[^b], unknown[^zz].\n\n[^a]: Alpha note.\n[^b]: Beta note.\n\nTail.\n";
    let (v, _) = laid(src, 800);
    let texts: Vec<String> = v
        .ops()
        .iter()
        .filter_map(|o| if let Op::Text { text, .. } = &o.op { Some(text.clone()) } else { None })
        .collect();
    let all = texts.join("|");
    assert!(all.contains("[1]") && all.contains("[2]"), "{all}");
    assert!(all.contains("[^zz]"), "an undefined reference stays literal: {all}");
    let pos = |s: &str| texts.iter().position(|t| t.contains(s)).unwrap_or_else(|| panic!("{s} not in {all}"));
    assert!(pos("Beta note") < pos("Alpha note"), "definitions follow the numbering: {all}");
    assert!(pos("Tail") < pos("Beta note"), "footnotes come after the body: {all}");
    let names: Vec<String> = v.heading_anchors().into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        ["fnref-b", "fnref-a", "fnref-b", "fn-b", "fn-a"],
        "every reference is an anchor for the back-link; the first one wins"
    );
    // the reference links to its definition
    assert!(v.all_links().iter().any(|l| v.urls.get(l.link).is_some_and(|u| u == "#fn-b")));
    assert_eq!(v.blocks().iter().filter(|b| matches!(b.kind, Kind::FootDef { .. })).count(), 2);
}

#[test]
fn footnote_definitions_get_a_back_link_to_the_first_reference() {
    let src = "Intro paragraph.\n\nSecond paragraph cites it[^a] and again[^a].\n\n[^a]: The note.\n";
    let (v, _) = laid(src, 800);
    let arrows: Vec<&Op> =
        v.ops().iter().map(|o| &o.op).filter(|o| matches!(o, Op::Text { text, .. } if text == "\u{21a9}")).collect();
    assert_eq!(arrows.len(), 1, "one return arrow per footnote definition");
    let back = v.all_links().iter().filter(|l| v.urls.get(l.link).is_some_and(|u| u == "#fnref-a")).count();
    assert_eq!(back, 1, "and it is a link");
    let anchors = v.heading_anchors();
    let y = |n: &str| {
        anchors.iter().find(|(a, _)| a == n).map(|(_, y)| *y).unwrap_or_else(|| panic!("{n} missing in {anchors:?}"))
    };
    // The link target is the paragraph that mentions the footnote (not the first one), above the definition.
    assert!(y("fnref-a") > PAD && y("fnref-a") < y("fn-a"));
    // A definition nobody references still gets its arrow, but no anchor to land on.
    let (v, _) = laid("Text.\n\n[^z]: Orphan.\n", 800);
    assert!(!v.heading_anchors().iter().any(|(a, _)| a == "fnref-z"));
}

#[test]
fn edited_image_files_relayout_the_preview() {
    let name = "md-edit-me.png";
    let dir = png_file(name, 64, 32, [255, 0, 0, 255]);
    let mut tx = TextCtx::new(None);
    let mut v = view("![x](md-edit-me.png)\n");
    v.set_base_dir(Some(&dir));
    v.ensure_layout(&mut tx, 800);
    let size_of =
        |v: &MdView| v.ops().iter().find_map(|o| if let Op::Image { r, .. } = &o.op { Some((r.w, r.h)) } else { None });
    assert_eq!(size_of(&v), Some((64, 32)));
    assert!(!v.refresh_images(), "nothing changed on disk");
    png_file(name, 40, 90, [0, 255, 0, 255]);
    assert!(v.refresh_images());
    v.ensure_layout(&mut tx, 800);
    assert_eq!(size_of(&v), Some((40, 90)), "the layout uses the new file's size");
    assert!(!v.refresh_images());
}

#[test]
fn math_inline_and_display() {
    let src = "Euler $e^{i\\pi}+1=0$ costs $5 and $6.\n\n$$\n\\int_0^1 x\\,dx\n$$\n\ntail\n";
    let v = view(src);
    let bl = v.blocks();
    assert_eq!(kinds(&v), ["p", "math", "p"]);
    let Kind::Para(inl) = &bl[0].kind else { panic!() };
    let math: Vec<&str> = inl
        .iter()
        .filter_map(|i| {
            if let Inl::Text { text, flags, .. } = i { (flags & F_MATH != 0).then_some(text.as_str()) } else { None }
        })
        .collect();
    assert_eq!(math, ["e^{i\\pi}+1=0"], "only the formula is math, the dollar amounts are text");
    let Kind::Math(m) = &bl[1].kind else { panic!() };
    assert_eq!(m, "\\int_0^1 x\\,dx");
    let (v, _) = laid(src, 800);
    assert!(v.ops().iter().any(|o| matches!(&o.op, Op::Code { math: true, .. })));
}

#[test]
fn task_checkboxes_report_the_source_offset_of_their_bracket() {
    let src = "para\n\n- [ ] one\n- [x] two\n\n  - [ ] nested\n";
    let mut tx = TextCtx::new(None);
    let mut v = view(src);
    v.ensure_layout(&mut tx, 800);
    let rect = Rect::new(20, 30, 800, 400);
    let checks = v.all_checks();
    assert_eq!(checks.len(), 3);
    let offs: Vec<usize> = checks.iter().map(|c| c.off).collect();
    let at = |s: &str| src.find(s).unwrap();
    assert_eq!(offs, [at("[ ] one"), at("[x] two"), at("[ ] nested")]);
    for c in checks.clone() {
        let (px, py) = (rect.x as f64 + c.x as f64 + 6.0, rect.y as f64 + c.y as f64 + 8.0);
        assert_eq!(v.check_at(px, py, rect, 0.0), Some(c.off));
        assert_eq!(v.check_at(px + 60.0, py, rect, 0.0), None, "text next to the box is not a hit");
        assert_eq!(v.check_at(px, py - 200.0 + 1.0, rect, 0.0).filter(|o| *o == c.off), None);
    }
    // a click through a scrolled view
    let c = checks[0];
    assert_eq!(
        v.check_at(rect.x as f64 + c.x as f64 + 6.0, rect.y as f64 + c.y as f64 + 8.0 - 5.0, rect, 5.0),
        Some(c.off)
    );
}

#[test]
fn task_flip_swaps_the_mark() {
    use crate::buffer::Buffer;
    let b = Buffer::from_str("- [ ] a\n- [X] b\n- [-] c\n");
    assert_eq!(crate::app_md::task_flip(&b, 2), Some((3, "x")));
    assert_eq!(crate::app_md::task_flip(&b, 10), Some((11, " ")));
    assert_eq!(crate::app_md::task_flip(&b, 18), None);
    assert_eq!(crate::app_md::task_flip(&b, 0), None);
}

fn png_file(name: &str, w: u32, h: u32, rgba: [u8; 4]) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-md-images");
    std::fs::create_dir_all(&dir).unwrap();
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&rgba.repeat((w * h) as usize)).unwrap();
    }
    std::fs::write(dir.join(name), out).unwrap();
    dir
}

#[test]
fn local_images_are_drawn_and_remote_ones_stay_placeholders() {
    let dir = png_file("md-red.png", 64, 32, [255, 0, 0, 255]);
    let src = "![local](md-red.png)\n\n![remote](https://x.io/a.png)\n\n![gone](missing.png)\n\n<img src=\"md-red.png\" width=\"20\">\n";
    let mut tx = TextCtx::new(None);
    let t = theme();
    let mut v = view(src);
    v.set_base_dir(Some(&dir));
    v.ensure_layout(&mut tx, 800);
    let imgs: Vec<Rect> =
        v.ops().iter().filter_map(|o| if let Op::Image { r, .. } = &o.op { Some(*r) } else { None }).collect();
    assert_eq!(imgs.len(), 2, "only the two local files are images");
    assert_eq!((imgs[0].w, imgs[0].h), (64, 32), "natural size");
    assert_eq!((imgs[1].w, imgs[1].h), (20, 10), "width attribute keeps the ratio");
    assert_eq!(
        v.ops().iter().filter(|o| matches!(&o.op, Op::Card { .. })).count(),
        2,
        "remote + missing keep the card"
    );
    // the pixels arrive from a worker thread
    let (w, h) = (900, 500);
    let mut px = vec![0u32; (w * h) as usize];
    let t0 = std::time::Instant::now();
    loop {
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0);
        if !v.pending() || t0.elapsed().as_secs() > 10 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let mut cv = Canvas { px: &mut px, w, h };
    v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0);
    let red = px.iter().filter(|p| **p == 0x00ff0000).count();
    assert!(red >= 64 * 32 + 20 * 10 - 20, "{red} red pixels");
}

#[test]
fn fenced_code_asks_the_highlighter_once_per_block_and_paints_its_colors() {
    let mut tx = TextCtx::new(None);
    let t = theme();
    let src = "```ts\nconst a = 1;\n```\n\n```\nplain\n```\n\n```ts\nconst a = 1;\n```\n";
    let mut v = view(src);
    let (w, h) = (700, 400);
    let mut px = vec![0u32; (w * h) as usize];
    let calls = std::cell::Cell::new(0);
    let mut hl = |lang: &str, text: &str| -> Option<HlLines> {
        calls.set(calls.get() + 1);
        assert_eq!(lang, "ts");
        Some(
            text.split('\n')
                .map(|l| {
                    vec![(0, l.len().min(5), Rgba(255, 0, 255, 255), 0u8), (l.len().min(5), l.len(), t.foreground, 0u8)]
                })
                .collect(),
        )
    };
    for _ in 0..3 {
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw_with(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0, Some(&mut hl));
    }
    assert_eq!(calls.get(), 1, "identical blocks share one result, and redraws reuse it");
    let magenta = |p: &u32| {
        let (r, g, b) = ((*p >> 16) & 255, (*p >> 8) & 255, *p & 255);
        r > g + 60 && b > g + 60
    };
    assert!(px.iter().filter(|p| magenta(p)).count() > 5, "the colored run reaches the canvas");
    // another theme asks again
    let other = Theme::load(&assets().join("themes/github-light.json")).unwrap();
    let mut cv = Canvas { px: &mut px, w, h };
    v.draw_with(&mut cv, &mut tx, &other, Rect::new(0, 0, w, h), 0.0, Some(&mut hl));
    assert_eq!(calls.get(), 2);
}

#[test]
fn real_tokenizer_colors_a_fenced_block_like_the_editor() {
    let t = theme();
    let tk = crate::highlight::make_tokenizer(&assets(), "source.ts").unwrap();
    let runs = crate::highlight::highlight_text(&tk, &t, "const a = 1;");
    assert!(runs[0].iter().any(|r| r.2 != t.foreground), "keywords are colored: {:?}", runs[0]);
    assert_eq!(crate::app_md::scope_of(&assets(), "ts").as_deref(), Some("source.ts"));
    assert_eq!(crate::app_md::scope_of(&assets(), "bash").as_deref(), Some("source.shell"));
    assert_eq!(crate::app_md::scope_of(&assets(), "RS").as_deref(), Some("source.rust"));
    assert_eq!(crate::app_md::scope_of(&assets(), "no-such-lang"), None);
}

#[test]
fn reference_style_links_resolve_across_segments() {
    let v = view("see [the docs][d] and [d]\n\n[d]: https://example.com/docs\n");
    let bl = v.blocks();
    let Kind::Para(inl) = &bl[0].kind else { panic!() };
    assert_eq!(inl.iter().filter(|i| matches!(i, Inl::Text { link, .. } if *link != 0)).count(), 2);
    assert_eq!(v.urls.list, ["https://example.com/docs"]);
}

#[test]
#[ignore]
fn profile_keystroke() {
    let mut tx = TextCtx::new(None);
    let t = theme();
    let src = doc_of(9500);
    let mut v = MdView::new();
    v.set_text(&src);
    v.ensure_layout(&mut tx, 900);
    let mut rope = ropey::Rope::from_str(&src);
    let at = rope.byte_to_char(src.find("Paragraph 4700 with").unwrap() + 14);
    let (w, h) = (900, 700);
    let mut px = vec![0u32; (w * h) as usize];
    let scroll = v.y_for_source_line(4700 * 17).unwrap() as f64;
    let ms = |t0: std::time::Instant| t0.elapsed().as_secs_f64() * 1000.0;
    for i in 0..8 {
        rope.insert(at + i, "x");
        let t0 = std::time::Instant::now();
        let text = rope.to_string();
        let a = ms(t0);
        let t0 = std::time::Instant::now();
        let sp = split(&text);
        let b = ms(t0);
        let t0 = std::time::Instant::now();
        let mut hh = 0u64;
        for s in &sp.segs {
            hh ^= fxhash(&text.as_bytes()[s.b0..s.b1], 0);
        }
        let c = ms(t0);
        let t0 = std::time::Instant::now();
        v.set_text(&text);
        let d = ms(t0);
        let t0 = std::time::Instant::now();
        v.ensure_layout(&mut tx, 900);
        let e = ms(t0);
        let t0 = std::time::Instant::now();
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), scroll);
        let f = ms(t0);
        let t0 = std::time::Instant::now();
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), scroll);
        let g = ms(t0);
        let t0 = std::time::Instant::now();
        let _ = tx.ui_width_sized(&format!("wordx{i}y"), false, 14.0 * 8.0);
        let k = ms(t0);
        eprintln!(
            "to_string {a:.1}  split {b:.1}  hash {c:.1} ({hh})  set_text(all) {d:.1}  ensure_layout {e:.1}  draw {f:.1} draw2 {g:.1} width_new_word {k:.1} work {:?}",
            v.work_counts()
        );
    }
}

#[test]
#[ignore]
fn dump_ppm() {
    let mut tx = TextCtx::new(None);
    let t = theme();
    let (w, h) = (900, 1500);
    let mut px = vec![0u32; (w * h) as usize];
    let mut v = view(&std::fs::read_to_string(std::env::var("MD_IN").unwrap()).unwrap());
    let mut cv = Canvas { px: &mut px, w, h };
    v.draw(&mut cv, &mut tx, &t, Rect::new(0, 0, w, h), 0.0);
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in &px {
        out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    std::fs::write(std::env::var("MD_OUT").unwrap(), out).unwrap();
}
