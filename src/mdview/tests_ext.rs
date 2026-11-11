//! Tests of the GitHub-flavored extras: setext, HTML subset, sup / sub, emoji, alerts, details,
//! footnotes, Mermaid blocks, remote images, videos, selection / copy and click targets, plus an
//! end-to-end layout of the showcase README.

use super::*;
use std::path::{Path, PathBuf};

const SHOWCASE: &str = include_str!("testdata/showcase.md");

fn theme() -> Theme {
    Theme::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/themes/dark-plus.json")).unwrap()
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
fn spans(v: &MdView, i: usize) -> Vec<Inl> {
    match &v.blocks()[i].kind {
        Kind::Para(inl) | Kind::HtmlPara { inl, .. } => inl.clone(),
        k => panic!("not a paragraph: {k:?}"),
    }
}
fn flags_of(inl: &[Inl], t: &str) -> Fl {
    inl.iter()
        .find_map(|i| match i {
            Inl::Text { text, flags, .. } if text == t => Some(*flags),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{t:?} not in {inl:?}"))
}
fn labels(v: &MdView) -> Vec<String> {
    v.ops().iter().filter_map(|o| if let Op::Label { text, .. } = &o.op { Some(text.clone()) } else { None }).collect()
}
fn texts(v: &MdView) -> Vec<String> {
    v.ops().iter().filter_map(|o| if let Op::Text { text, .. } = &o.op { Some(text.clone()) } else { None }).collect()
}
/// Draw until the background work (decodes, downloads) settles.
fn settle(v: &mut MdView, tx: &mut TextCtx, w: i32, h: i32) -> Vec<u32> {
    let t = theme();
    let mut px = vec![0u32; (w * h) as usize];
    let t0 = std::time::Instant::now();
    loop {
        let mut cv = Canvas { px: &mut px, w, h };
        v.draw(&mut cv, tx, &t, Rect::new(0, 0, w, h), 0.0);
        if !v.pending() || t0.elapsed().as_secs() > 10 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let mut cv = Canvas { px: &mut px, w, h };
    v.draw(&mut cv, tx, &t, Rect::new(0, 0, w, h), 0.0);
    px
}

#[test]
fn setext_headings() {
    let v = view("Title one\n=========\n\nTítulo com sublinhado (setext)\n------------------------------\n\ntext\n");
    let bl = v.blocks();
    let lv: Vec<(u8, String)> = bl
        .iter()
        .filter_map(
            |b| if let Kind::Heading { level, slug, .. } = &b.kind { Some((*level, slug.clone())) } else { None },
        )
        .collect();
    assert_eq!(lv, [(1, "title-one".to_string()), (2, "título-com-sublinhado-setext".to_string())]);
    assert_eq!((bl[1].line, bl[1].end), (3, 4));
}

#[test]
fn superscript_and_subscript() {
    let v = view("E = mc<sup>2</sup>, H<sub>2</sub>O, x^n^ and y~i~ but ~~gone~~.\n");
    let inl = spans(&v, 0);
    let f = |want: Fl| inl.iter().filter(|i| matches!(i, Inl::Text { flags, .. } if flags & want != 0)).count();
    assert_eq!(f(F_SUP), 2, "{inl:?}");
    assert_eq!(f(F_SUB), 2, "{inl:?}");
    assert_eq!(flags_of(&inl, "gone"), F_S, "double tilde stays strikethrough");
    assert_eq!(flags_of(&inl, "n"), F_SUP);
    let (v, _) = laid("mc<sup>2</sup>\n", 600);
    let sup = v.ops().into_iter().find_map(|o| match &o.op {
        Op::Text { text, flags, w, .. } if text == "2" => Some((*flags, *w)),
        _ => None,
    });
    let (fl, w) = sup.expect("the superscript is a run of its own");
    assert!(fl & F_SUP != 0 && w > 0);
}

#[test]
fn html_inline_styles_and_blocks() {
    let v = view(
        "<ins>u1</ins> <u>u2</u> <mark>hi</mark> <kbd>Ctrl</kbd> <b>b</b> <i>i</i> <s>s</s> <del>d</del> <strong>st</strong> <em>em</em> <a href=\"https://x.io\">ln</a> __bold__\n",
    );
    let inl = spans(&v, 0);
    assert_eq!(flags_of(&inl, "u1"), F_U);
    assert_eq!(flags_of(&inl, "u2"), F_U);
    assert_eq!(flags_of(&inl, "hi"), F_MARK);
    assert_eq!(flags_of(&inl, "Ctrl"), F_KBD);
    assert_eq!(flags_of(&inl, "b"), F_B);
    assert_eq!(flags_of(&inl, "i"), F_I);
    assert_eq!(flags_of(&inl, "s"), F_S);
    assert_eq!(flags_of(&inl, "d"), F_S);
    assert_eq!(flags_of(&inl, "st"), F_B);
    assert_eq!(flags_of(&inl, "em"), F_I);
    assert_eq!(flags_of(&inl, "ln"), F_LINK);
    assert_eq!(flags_of(&inl, "bold"), F_B, "__bold__ is bold, not underline");
    // Block HTML: alignment, inline tags and <br>, comments dropped.
    let v = view("<!-- hidden -->\n\n<p align=\"center\">\n  <b>Centered</b><br>\n  <i>second</i>\n</p>\n");
    assert_eq!(v.blocks().len(), 1, "the comment leaves nothing");
    let Kind::HtmlPara { align, inl, head } = &v.blocks()[0].kind else { panic!("{:?}", v.blocks()) };
    assert_eq!((*align, *head), (1, 0));
    assert_eq!(flags_of(inl, "Centered"), F_B);
    assert_eq!(flags_of(inl, "second"), F_I);
    assert!(inl.contains(&Inl::Hard));
    let (v, _) = laid("<p align=\"center\">short</p>\n", 700);
    let x = v.ops().into_iter().find_map(|o| if let Op::Text { x, .. } = &o.op { Some(*x) } else { None }).unwrap();
    let lay = v.lay.as_ref().unwrap();
    assert!(x > lay.col_x + 200, "centered: {x}");
    // A table with a merged cell.
    let v = view(
        "<table>\n  <tr><th>HTML</th><th>Tabela</th></tr>\n  <tr><td colspan=\"2\" align=\"center\">célula mesclada</td></tr>\n</table>\n",
    );
    let Kind::HtmlTable(rows) = &v.blocks()[0].kind else { panic!("{:?}", v.blocks()) };
    assert_eq!(rows.len(), 2);
    assert!(rows[0].iter().all(|c| c.head));
    assert_eq!((rows[1][0].span, rows[1][0].align), (2, 1));
    let (v, _) = laid("<table><tr><th>A</th><th>B</th></tr><tr><td colspan=\"2\">wide cell</td></tr></table>\n", 700);
    assert!(texts(&v).contains(&"wide cell".to_string()));
    assert!(
        v.ops().iter().any(|o| matches!(&o.op, Op::Text { flags, .. } if flags & F_B != 0)),
        "header cells are bold"
    );
}

#[test]
fn kbd_and_mark_lay_out_as_chips() {
    let (v, mut tx) = laid("Press <kbd>Ctrl</kbd> + <kbd>P</kbd>, <mark>marked text</mark>.\n", 700);
    let kbd: Vec<i32> = v
        .ops()
        .iter()
        .filter_map(|o| match &o.op {
            Op::Text { flags, w, .. } if flags & F_KBD != 0 => Some(*w),
            _ => None,
        })
        .collect();
    assert_eq!(kbd.len(), 2);
    assert!(kbd.iter().all(|w| *w > 2 * KBD_PAD), "{kbd:?}");
    assert!(v.ops().iter().any(|o| matches!(&o.op, Op::Text { flags, .. } if flags & F_MARK != 0)));
    let mut v = v;
    settle(&mut v, &mut tx, 700, 200);
}

#[test]
fn emoji_shortcodes_but_not_in_code() {
    let v = view("Go :rocket: :snake: :white_check_mark: :not_an_emoji: `:rocket:`\n\n```\n:snake:\n```\n");
    let inl = spans(&v, 0);
    let plain_text: String = inl
        .iter()
        .filter_map(|i| {
            if let Inl::Text { text, flags, .. } = i { (flags & F_CODE == 0).then_some(text.as_str()) } else { None }
        })
        .collect();
    assert!(plain_text.contains("\u{1f680} \u{1f40d} \u{2705} :not_an_emoji:"), "{plain_text}");
    assert_eq!(flags_of(&inl, ":rocket:"), F_CODE, "inline code keeps the shortcode");
    let Kind::Code { text, .. } = &v.blocks()[1].kind else { panic!() };
    assert_eq!(text, ":snake:");
}

#[test]
fn bare_urls_emails_and_autolinks_are_links() {
    let v = view("Autolink: <https://github.com> e URL nua: https://pypi.org, mail <contato@exemplo.com> or a@b.io.\n");
    assert!(v.urls.list.contains(&"https://github.com".to_string()));
    assert!(v.urls.list.contains(&"https://pypi.org".to_string()), "{:?}", v.urls.list);
    assert!(v.urls.list.contains(&"mailto:contato@exemplo.com".to_string()), "{:?}", v.urls.list);
    assert!(v.urls.list.contains(&"mailto:a@b.io".to_string()), "{:?}", v.urls.list);
    let inl = spans(&v, 0);
    assert_eq!(flags_of(&inl, "https://pypi.org"), F_LINK);
    assert_eq!(
        crate::app_md::classify_link("mailto:contato@exemplo.com", None),
        crate::app_md::LinkAction::Web("mailto:contato@exemplo.com".into())
    );
}

#[test]
fn github_alerts_all_five() {
    let src = "> [!NOTE]\n> Info.\n\n> [!TIP]\n> Tip.\n\n> [!IMPORTANT]\n> Imp.\n\n> [!WARNING]\n> Warn.\n\n> [!CAUTION]\n> Careful.\n\n> plain quote\n";
    let (v, _) = laid(src, 800);
    let kinds: Vec<u8> =
        v.blocks().iter().filter_map(|b| if let Kind::Alert(k, _) = &b.kind { Some(*k) } else { None }).collect();
    assert_eq!(kinds, [0, 1, 2, 3, 4]);
    assert!(matches!(v.blocks()[5].kind, Kind::Quote(_)), "a quote without a marker stays a quote");
    let l = labels(&v);
    for t in ["Note", "Tip", "Important", "Warning", "Caution"] {
        assert!(l.contains(&t.to_string()), "{t} title missing: {l:?}");
    }
    let t = texts(&v);
    assert!(t.iter().any(|s| s.contains("Info.")) && !t.iter().any(|s| s.contains("[!NOTE]")), "{t:?}");
    let icons: Vec<&str> =
        v.ops().iter().filter_map(|o| if let Op::Icon { name, .. } = &o.op { Some(*name) } else { None }).collect();
    assert_eq!(icons, ["ui/info", "ui/sparkle", "ui/chat", "ui/warning", "ui/stop"]);
    // Colored bars, distinct per kind, readable on the page.
    let pal = Pal::new(&theme());
    let cols: Vec<Rgba> = (0..5).map(|k| pal.get(Col::Alert(k))).collect();
    for (i, c) in cols.iter().enumerate() {
        assert!(c.contrast(pal.bg) >= 3.0);
        assert!(cols[i + 1..].iter().all(|o| o != c));
    }
}

const DETAILS: &str = "before\n\n<details>\n<summary>Clique para expandir</summary>\n\nConteúdo escondido com **Markdown** dentro:\n\n- item um\n- item dois\n\n```python\nprint(\"surpresa\")\n```\n\n</details>\n\nafter\n";

#[test]
fn details_are_one_segment_collapsed_by_default_and_toggle() {
    let sp = split(DETAILS);
    assert_eq!(sp.segs.len(), 3, "blank lines inside <details> do not cut it");
    let (mut v, mut tx) = laid(DETAILS, 800);
    let bl = v.blocks();
    let Kind::Details { id, summary, blocks, open } = &bl[1].kind else { panic!("{:?}", kinds_of(&v)) };
    assert!(!open);
    assert_eq!(plain(summary), "Clique para expandir");
    assert_eq!(blocks.len(), 3, "paragraph, list and code fence inside");
    assert!(matches!(blocks[2].kind, Kind::Code { .. }));
    let id = *id;
    let h0 = v.content_height();
    assert!(!texts(&v).iter().any(|t| t.contains("item um")), "closed: content not shown");
    // The summary row is a toggle with a hand cursor.
    let rect = Rect::new(0, 0, 800, 2000);
    let hit = v.all_hits().into_iter().find(|h| h.act == Act::Toggle(id)).expect("toggle hit");
    let (px, py) = ((hit.x + 40) as f64, (hit.y + hit.h / 2) as f64);
    assert_eq!(v.hit_at(px, py, rect, 0.0), Some(Act::Toggle(id)));
    assert!(v.clickable_at(px, py, rect, 0.0));
    v.toggle_details(id);
    v.ensure_layout(&mut tx, 800);
    assert!(v.content_height() > h0 + 60, "open: taller");
    assert!(texts(&v).iter().any(|t| t.contains("item um")));
    assert!(v.ops().iter().any(|o| matches!(&o.op, Op::Code { .. })));
    // The state survives an edit elsewhere.
    v.set_text(&DETAILS.replace("after", "after edit"));
    v.ensure_layout(&mut tx, 800);
    assert!(texts(&v).iter().any(|t| t.contains("item um")), "still open");
    v.toggle_details(id);
    v.ensure_layout(&mut tx, 800);
    assert!(!texts(&v).iter().any(|t| t.contains("item um")));
    // `<details open>` starts open.
    let (v, _) = laid("<details open>\n<summary>S</summary>\n\nshown\n\n</details>\n", 800);
    assert!(texts(&v).iter().any(|t| t.contains("shown")));
}

fn kinds_of(v: &MdView) -> Vec<String> {
    v.blocks().iter().map(|b| format!("{:?}", std::mem::discriminant(&b.kind))).collect()
}

#[test]
fn definition_lists() {
    let v = view("Pyright\n: Verificador de tipos.\n\nRuff\n: Linter e formatador.\n");
    let items: Vec<(String, usize)> = v
        .blocks()
        .iter()
        .filter_map(|b| if let Kind::DefList(it) = &b.kind { Some(it.clone()) } else { None })
        .flatten()
        .map(|it| (plain(&it.title), it.defs.len()))
        .collect();
    assert_eq!(items, [("Pyright".to_string(), 1), ("Ruff".to_string(), 1)]);
}

#[test]
fn mermaid_blocks_draw_diagrams_with_a_code_toggle() {
    let src = "```mermaid\nflowchart LR\n  A[Start] --> B{Ok?}\n  B -- yes --> C[Done]\n```\n\n```mermaid\ngantt\n  title x\n```\n";
    let (mut v, mut tx) = laid(src, 800);
    let dias = v.ops().iter().filter(|o| matches!(&o.op, Op::Diagram { .. })).count();
    assert_eq!(dias, 1);
    assert!(labels(&v).iter().any(|l| l.contains("gantt") && l.contains("not supported")), "{:?}", labels(&v));
    let codes = v.ops().iter().filter(|o| matches!(&o.op, Op::Code { .. })).count();
    assert_eq!(codes, 1, "the unsupported one shows its source");
    let key = v
        .all_hits()
        .into_iter()
        .find_map(|h| if let Act::Code(k) = h.act { Some(k) } else { None })
        .expect("code button");
    v.toggle_code(key);
    v.ensure_layout(&mut tx, 800);
    assert_eq!(v.ops().iter().filter(|o| matches!(&o.op, Op::Code { .. })).count(), 2, "source shown");
    assert!(v.all_hits().iter().any(|h| h.act == Act::Code(key)), "and a button back to the diagram");
    v.toggle_code(key);
    v.ensure_layout(&mut tx, 800);
    assert_eq!(v.ops().iter().filter(|o| matches!(&o.op, Op::Diagram { .. })).count(), 1);
    // The diagram is laid out once and cached across relayouts.
    let n = v.ext.dias.len();
    v.ensure_layout(&mut tx, 640);
    assert_eq!(v.ext.dias.len(), n);
    let px = settle(&mut v, &mut tx, 640, 900);
    let bg = theme().background;
    let bgpx = ((bg.0 as u32) << 16) | ((bg.1 as u32) << 8) | bg.2 as u32;
    assert!(px.iter().filter(|p| **p != bgpx).count() > 3000, "the diagram paints");
}

fn remote_src_png() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-md-remote");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("src.png");
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 30, 12);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&[0u8, 0, 255, 255].repeat(30 * 12)).unwrap();
    }
    std::fs::write(&p, out).unwrap();
    p
}

fn stub_fetch(url: &str, dest: &Path) -> Result<(), String> {
    if url.contains("fail") {
        return Err("404".into());
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-md-remote/src.png");
    std::fs::copy(src, dest).map(|_| ()).map_err(|e| e.to_string())
}

#[test]
fn remote_images_are_fetched_cached_and_can_be_turned_off() {
    use crate::mdimg::{cache_key, is_remote};
    assert!(is_remote("https://x.io/a.png") && is_remote("HTTP://x.io/a.png"));
    assert!(!is_remote("docs/a.png") && !is_remote("data:image/png;base64,AA") && !is_remote("https://"));
    assert_eq!(cache_key("https://x.io/a.png"), cache_key(" https://x.io/a.png "));
    assert_ne!(cache_key("https://x.io/a.png"), cache_key("https://x.io/b.png"));
    assert_eq!(cache_key("https://x.io/a.png").len(), 16);
    remote_src_png();
    let cache =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/test-md-remote/cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let src = "![ok](https://img.example/badge.png)\n\n![broken](https://img.example/fail.png)\n";
    let mut tx = TextCtx::new(None);
    let mut v = view(src);
    v.imgs.set_fetcher(stub_fetch, cache.clone());
    v.ensure_layout(&mut tx, 800);
    assert!(labels(&v).iter().any(|l| l.starts_with("Loading")), "{:?}", labels(&v));
    settle(&mut v, &mut tx, 800, 400);
    v.ensure_layout(&mut tx, 800);
    let imgs: Vec<Rect> =
        v.ops().iter().filter_map(|o| if let Op::Image { r, .. } = &o.op { Some(*r) } else { None }).collect();
    assert_eq!(imgs.len(), 1, "the downloaded one is an image: {:?}", labels(&v));
    assert_eq!((imgs[0].w, imgs[0].h), (30, 12));
    assert!(labels(&v).iter().any(|l| l.starts_with("Couldn't load")), "{:?}", labels(&v));
    assert!(cache.join(cache_key("https://img.example/badge.png")).is_file(), "kept in the cache");
    // Off: nothing is fetched, both show a card saying so.
    let mut v2 = view(src);
    v2.imgs.set_fetcher(stub_fetch, cache.join("off"));
    v2.set_remote_images(false);
    v2.ensure_layout(&mut tx, 800);
    assert!(!v2.pending());
    assert_eq!(labels(&v2).iter().filter(|l| l.starts_with("Remote images are off")).count(), 2);
    assert!(!cache.join("off").exists());
    // The cache is trimmed oldest first.
    let dir = cache.join("prune");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..4 {
        std::fs::write(dir.join(format!("f{i}")), vec![0u8; 1000]).unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(100 - i * 10);
        std::fs::File::options().write(true).open(dir.join(format!("f{i}"))).unwrap().set_modified(t).unwrap();
    }
    crate::mdimg::prune_cache(&dir, 2500);
    assert!(!dir.join("f0").exists() && !dir.join("f1").exists() && dir.join("f2").exists() && dir.join("f3").exists());
    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn missing_local_image_card_says_so() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let mut tx = TextCtx::new(None);
    let mut v = view("![Imagem que não existe](docs/nao-existe.png)\n");
    v.set_base_dir(Some(&dir));
    v.ensure_layout(&mut tx, 800);
    let l = labels(&v);
    assert!(l.contains(&"Imagem que não existe".to_string()), "{l:?}");
    assert!(l.iter().any(|s| s.starts_with("Image not found")), "{l:?}");
}

#[test]
fn videos_are_cards_that_open_externally() {
    let src = "![demo](clip.mp4)\n\n<video src=\"https://x.io/v.webm\" controls></video>\n\nhttps://x.io/movie.mp4\n";
    let mut tx = TextCtx::new(None);
    let mut v = view(src);
    v.set_base_dir(Some(Path::new("/proj")));
    v.ensure_layout(&mut tx, 800);
    let vids: Vec<String> = v
        .blocks()
        .iter()
        .filter_map(|b| if let Kind::Images(i) = &b.kind { Some(i.clone()) } else { None })
        .flatten()
        .filter(|i| i.video)
        .map(|i| i.url)
        .collect();
    assert_eq!(vids, ["clip.mp4", "https://x.io/v.webm", "https://x.io/movie.mp4"]);
    let plays = v.ops().iter().filter(|o| matches!(&o.op, Op::Icon { name: "ui/play_filled", .. })).count();
    assert_eq!(plays, 3);
    let targets: Vec<String> = v.all_links().iter().filter_map(|l| v.urls.get(l.link).cloned()).collect();
    assert!(targets.contains(&"file:///proj/clip.mp4".to_string()), "{targets:?}");
    assert!(targets.contains(&"https://x.io/v.webm".to_string()));
    assert!(labels(&v).iter().any(|l| l.contains("open externally")));
}

#[test]
fn copy_from_code_blocks_and_selections() {
    let src = "Intro paragraph here.\n\n```python\ndef f(x):\n\treturn x  # tab\n```\n\nOutro.\n";
    let (mut v, mut tx) = laid(src, 800);
    let key = v
        .all_hits()
        .into_iter()
        .find_map(|h| if let Act::Copy(k) = h.act { Some(k) } else { None })
        .expect("copy button");
    assert_eq!(v.code_text(key).as_deref(), Some("def f(x):\n\treturn x  # tab"), "raw code, tabs kept");
    v.mark_copied(key);
    assert!(v.ui_due().is_some());
    // Select everything: prose, then the code lines verbatim (as shown), then prose.
    v.select_all();
    let all = v.selected_text();
    assert!(all.starts_with("Intro paragraph here."), "{all}");
    assert!(all.contains("def f(x):\n    return x  # tab"), "{all:?}");
    assert!(all.ends_with("Outro."), "{all}");
    // A drag inside the first paragraph picks part of it.
    let rect = Rect::new(0, 0, 800, 600);
    let first = v
        .ops()
        .into_iter()
        .find_map(|o| if let Op::Text { x, w, .. } = &o.op { Some((*x, *w, o.y0, o.y1)) } else { None })
        .unwrap();
    let (x0, w, y0, y1) = first;
    let my = ((y0 + y1) / 2) as f64;
    v.sel_begin(&mut tx, x0 as f64 + 1.0, my, rect, 0.0);
    v.sel_extend(&mut tx, (x0 + w) as f64 - 1.0, my, rect, 0.0);
    assert!(v.has_selection());
    let part = v.selected_text();
    assert!(!part.is_empty() && "Intro paragraph here.".contains(&part), "{part:?}");
    // Dragging from the paragraph into the code block takes the code's first line.
    let code = v
        .ops()
        .into_iter()
        .find_map(|o| if let Op::Code { r, top, lh, .. } = &o.op { Some((*r, *top, *lh)) } else { None })
        .unwrap();
    let (r, top, lh) = code;
    v.sel_extend(&mut tx, (r.x + 400) as f64, (r.y + top + lh / 2) as f64, rect, 0.0);
    let t = v.selected_text();
    assert!(t.contains("Intro") && t.contains("def f(x):"), "{t:?}");
    // The selection paints.
    let _ = settle(&mut v, &mut tx, 800, 600);
    v.clear_selection();
    assert!(!v.has_selection());
}

#[test]
fn links_and_buttons_are_click_targets_plain_text_is_not() {
    let (v, _) = laid("plain words first then [a link](https://example.com) end\n\n```\ncode\n```\n", 800);
    let rect = Rect::new(10, 20, 800, 600);
    let l = v.all_links()[0];
    let (lx, ly) = ((rect.x + l.x + l.w / 2) as f64, (rect.y + l.y + l.h / 2) as f64);
    assert!(v.clickable_at(lx, ly, rect, 0.0), "hand cursor over the link");
    assert_ne!(v.hover_sig(lx, ly, rect, 0.0), v.hover_sig((rect.x + 30) as f64, ly, rect, 0.0));
    assert!(!v.clickable_at((rect.x + PAD + 30) as f64, ly, rect, 0.0), "I-beam over plain text");
    let b = v.all_hits().into_iter().find(|h| matches!(h.act, Act::Copy(_))).unwrap();
    assert!(v.clickable_at((rect.x + b.x + 4) as f64, (rect.y + b.y + 4) as f64, rect, 0.0));
}

#[test]
fn showcase_readme_end_to_end() {
    let (mut v, mut tx) = laid(SHOWCASE, 900);
    let bl = v.blocks();
    let count = |f: &dyn Fn(&Kind) -> bool| bl.iter().filter(|b| f(&b.kind)).count();
    assert_eq!(count(&|k| matches!(k, Kind::Alert(..))), 5, "five alerts");
    assert_eq!(count(&|k| matches!(k, Kind::Details { .. })), 1);
    assert_eq!(count(&|k| matches!(k, Kind::DefList(_))), 2);
    assert_eq!(count(&|k| matches!(k, Kind::HtmlTable(_))), 1);
    assert_eq!(count(&|k| matches!(k, Kind::HtmlPara { align: 1, .. })), 1);
    assert_eq!(count(&|k| matches!(k, Kind::Rule)), 4);
    assert!(
        bl.iter()
            .any(|b| matches!(&b.kind, Kind::Heading { level: 2, slug, .. } if slug == "título-com-sublinhado-setext"))
    );
    assert!(bl.iter().any(|b| matches!(&b.kind, Kind::List { start: Some(3), .. })), "ordered list from 3");
    assert_eq!(v.ops().iter().filter(|o| matches!(&o.op, Op::Diagram { .. })).count(), 4, "four Mermaid diagrams");
    let t = texts(&v).join(" ");
    assert!(t.contains("\u{1f680}") && t.contains("\u{1f40d}") && t.contains("\u{2705}"), "emoji");
    assert!(!t.contains(":rocket:"));
    assert!(!t.contains("Este comentário HTML"), "comments hidden");
    assert!(v.urls.list.contains(&"https://pypi.org".to_string()));
    assert!(v.urls.list.contains(&"mailto:contato@exemplo.com".to_string()));
    assert!(v.urls.list.contains(&"#fn-1".to_string()) && v.urls.list.contains(&"#fnref-ruff".to_string()));
    let flags: Vec<Fl> =
        v.ops().iter().filter_map(|o| if let Op::Text { flags, .. } = &o.op { Some(*flags) } else { None }).collect();
    for f in [F_SUP, F_SUB, F_KBD, F_MARK, F_B, F_I, F_S, F_CODE] {
        assert!(flags.iter().any(|x| x & f != 0), "flag {f} missing");
    }
    let names: Vec<String> = v.heading_anchors().into_iter().map(|(n, _)| n).collect();
    for a in [
        "tabelas",
        "formatação-de-texto",
        "diagramas-mermaid",
        "citações-e-alertas",
        "notas-de-rodapé",
        "fn-1",
        "fn-ruff",
    ] {
        assert!(names.iter().any(|n| n == a), "anchor {a} missing");
    }
    // Long URL breaks inside the column.
    let lay = v.lay.as_ref().unwrap();
    let right = lay.col_x + lay.col_w;
    assert!(v.ops().iter().all(|o| match &o.op {
        Op::Text { x, w, .. } => x + w <= right + 2,
        _ => true,
    }));
    // Paints without panicking, and every visible thing can be selected and copied.
    let _ = settle(&mut v, &mut tx, 900, 1200);
    v.select_all();
    let all = v.selected_text();
    assert!(all.contains("def antigo():") && all.contains("Vitrine de Markdown"), "{}", all.len());
}

impl MdView {
    /// Button hit boxes with absolute y (tests).
    fn all_hits(&self) -> Vec<Hit> {
        let Some(lay) = self.lay.as_ref() else { return vec![] };
        let mut v: Vec<Hit> = lay
            .segs
            .iter()
            .filter_map(|s| self.seg_lay_of(s).map(|l| (s, l)))
            .flat_map(|(s, l)| l.hits.iter().map(move |h| Hit { y: h.y + s.y0, ..*h }))
            .collect();
        if let Some((l, p)) = self.foot.as_ref() {
            v.extend(l.hits.iter().map(|h| Hit { y: h.y + p.y0, ..*h }));
        }
        v
    }
}
