use super::*;

/// A fake text measure: 7 px per char (bold 8), scaled by size.
fn meas() -> impl FnMut(&str, bool, f32) -> f32 {
    |s: &str, bold: bool, size: f32| s.chars().count() as f32 * if bold { 8.0 } else { 7.0 } * size / 13.0
}

fn build_ok(src: &str) -> Diagram {
    let mut m = meas();
    let d = build(src, &mut m).unwrap_or_else(|e| panic!("{e}: {src}"));
    assert!(d.w > 10.0 && d.h > 10.0, "{} x {}", d.w, d.h);
    for l in &d.labels {
        assert!(l.x.is_finite() && l.y.is_finite(), "{l:?}");
        assert!(
            l.x >= -1.0 && l.x <= d.w + 1.0 && l.y >= -1.0 && l.y <= d.h + 1.0,
            "label {l:?} outside {}x{}",
            d.w,
            d.h
        );
    }
    d
}

fn has_label(d: &Diagram, t: &str) -> bool {
    d.labels.iter().any(|l| l.text == t)
}

#[test]
fn flowchart_of_the_demo_lays_out_left_to_right() {
    let src = "flowchart LR\n    A[Nova tarefa] --> B{Prioridade alta?}\n    B -- Sim --> C[Fazer agora]\n    B -- Não --> D[Backlog]\n    C --> E[Concluída]\n    D --> E\n";
    let g = flow::parse(&clean(src));
    assert_eq!(g.dir, layered::Dir::Right);
    assert_eq!(g.nodes.len(), 5);
    assert_eq!(g.edges.len(), 5);
    let b = g.ids["B"];
    assert_eq!(g.nodes[b].shape, flow::Shape::Diamond);
    assert_eq!(g.nodes[b].label, "Prioridade alta?");
    let labels: Vec<&str> = g.edges.iter().map(|e| e.link.label.as_str()).collect();
    assert_eq!(labels, ["", "Sim", "Não", "", ""]);
    assert!(g.edges.iter().all(|e| e.link.end == Mark::Arrow));
    let d = build_ok(src);
    for t in ["Nova tarefa", "Prioridade alta?", "Fazer agora", "Backlog", "Concluída", "Sim", "Não"] {
        assert!(has_label(&d, t), "{t} missing");
    }
    let x = |t: &str| d.labels.iter().find(|l| l.text == t).map(|l| l.x).unwrap();
    assert!(x("Nova tarefa") < x("Prioridade alta?") && x("Prioridade alta?") < x("Concluída"), "LR order");
    assert!(d.w > d.h, "a left-to-right chart is wide");
}

#[test]
fn flowchart_shapes_links_groups_and_subgraphs() {
    let src = "graph TD\n  a([stadium]) -.-> b[(db)]\n  b ==> c((circle))\n  c --o d>flag]\n  d --x e{{hex}}\n  e -->|yes| f[/para/]\n  f --- g[\\alt\\]\n  a & b --> h[[sub]]\n  subgraph S1 [Group one]\n    x1 --> x2\n  end\n  x2 ~~~ a\n  click a callback\n  classDef red fill:#f00\n";
    let g = flow::parse(&clean(src));
    let sh = |id: &str| g.nodes[g.ids[id]].shape;
    assert_eq!(sh("a"), flow::Shape::Stadium);
    assert_eq!(sh("b"), flow::Shape::Cyl);
    assert_eq!(sh("c"), flow::Shape::Circle);
    assert_eq!(sh("d"), flow::Shape::Flag);
    assert_eq!(sh("e"), flow::Shape::Hex);
    assert_eq!(sh("f"), flow::Shape::Para);
    assert_eq!(sh("g"), flow::Shape::ParaAlt);
    assert_eq!(sh("h"), flow::Shape::Sub);
    let e = |a: &str, b: &str| g.edges.iter().find(|e| e.from == g.ids[a] && e.to == g.ids[b]).unwrap();
    assert_eq!(e("a", "b").link.stroke, flow::Stroke::Dotted);
    assert_eq!(e("b", "c").link.stroke, flow::Stroke::Thick);
    assert_eq!(e("c", "d").link.end, Mark::Circle);
    assert_eq!(e("d", "e").link.end, Mark::Cross);
    assert_eq!(e("e", "f").link.label, "yes");
    assert_eq!(e("f", "g").link.end, Mark::None);
    assert!(g.edges.iter().any(|x| x.from == g.ids["a"] && x.to == g.ids["h"]));
    assert!(g.edges.iter().any(|x| x.from == g.ids["b"] && x.to == g.ids["h"]));
    assert_eq!(e("x2", "a").link.stroke, flow::Stroke::Invisible);
    assert_eq!(g.subs.len(), 1);
    assert_eq!(g.subs[0].title, "Group one");
    assert_eq!(g.nodes[g.ids["x1"]].sub, Some(0));
    assert_eq!(g.nodes[g.ids["a"]].sub, None);
    let d = build_ok(src);
    assert!(has_label(&d, "Group one"));
}

#[test]
fn sequence_of_the_demo() {
    let src = "sequenceDiagram\n    participant U as Usuário\n    participant E as Editor\n    participant L as Lumen\n    U->>E: Passa o mouse em adicionar\n    E->>L: textDocument/hover\n    L-->>E: Assinatura e docstring\n    E-->>U: Mostra o tooltip\n";
    let q = seq::parse(&clean(src));
    assert_eq!(q.actors.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), ["Usuário", "Editor", "Lumen"]);
    let msgs: Vec<(usize, usize, bool)> = q
        .items
        .iter()
        .filter_map(
            |i| if let seq::Item::Msg { from, to, dotted, .. } = i { Some((*from, *to, *dotted)) } else { None },
        )
        .collect();
    assert_eq!(msgs, [(0, 1, false), (1, 2, false), (2, 1, true), (1, 0, true)]);
    let d = build_ok(src);
    for t in ["Usuário", "Editor", "Lumen", "textDocument/hover", "Mostra o tooltip"] {
        assert!(has_label(&d, t), "{t}");
    }
    // actors drawn at the top and mirrored at the bottom
    assert_eq!(d.labels.iter().filter(|l| l.text == "Lumen").count(), 2);
}

#[test]
fn sequence_notes_frames_self_messages_and_autonumber() {
    let src = "sequenceDiagram\nautonumber\nactor A as Alice\nA->>A: think\nA-xB: lost\nB--)A: later\nNote right of B: a note\nNote over A,B: both\nloop every day\nA->>B: ping\nalt ok\nB->>A: pong\nelse fail\nB-->>A: error\nend\nend\nrandom garbage line\n";
    let q = seq::parse(&clean(src));
    assert!(q.autonumber);
    assert!(q.actors[0].person);
    assert_eq!(q.items.iter().filter(|i| matches!(i, seq::Item::Note { .. })).count(), 2);
    assert_eq!(q.items.iter().filter(|i| matches!(i, seq::Item::Open { .. })).count(), 2);
    let d = build_ok(src);
    assert!(has_label(&d, "1. think"));
    assert!(has_label(&d, "loop") && has_label(&d, "[every day]") && has_label(&d, "[fail]"));
}

#[test]
fn pie_of_the_demo() {
    let src = "pie title Tempo por tarefa\n    \"README\" : 1.5\n    \"Erros\" : 2\n    \"Café\" : 0.25\n";
    let p = pie::parse(&clean(src));
    assert_eq!(p.title, "Tempo por tarefa");
    assert_eq!(p.slices.len(), 3);
    assert_eq!(p.slices[2], ("Café".to_string(), 0.25));
    let d = build_ok(src);
    assert!(has_label(&d, "Tempo por tarefa"));
    assert!(has_label(&d, "README") && has_label(&d, "Café"));
    // 1.5 / 3.75 = 40%, 2 / 3.75 = 53.3%, 0.25 / 3.75 = 6.7%
    assert!(has_label(&d, "40%") && has_label(&d, "53.3%") && has_label(&d, "6.7%"));
    assert_eq!(
        d.shapes.iter().filter(|s| matches!(s.fill, Some(Paint::Series(_)))).count(),
        6,
        "3 wedges + 3 swatches"
    );
    assert_eq!(pie::pct(100.0), "100%");
    let mut m = meas();
    assert!(build("pie\n", &mut m).is_err(), "no slices");
}

#[test]
fn class_diagram_of_the_demo() {
    let src = "classDiagram\n    class Tarefa {\n        +str titulo\n        +Prioridade prioridade\n        +concluir()\n    }\n    class ListaDeTarefas {\n        +list~Tarefa~ itens\n        +adicionar(Tarefa)\n    }\n    ListaDeTarefas \"1\" --> \"*\" Tarefa\n";
    let g = class::parse(&clean(src));
    assert_eq!(g.classes.len(), 2);
    let t = &g.classes[0];
    assert_eq!((t.name.as_str(), t.attrs.len(), t.methods.len()), ("Tarefa", 2, 1));
    assert_eq!(g.classes[1].attrs, ["+list<Tarefa> itens"]);
    assert_eq!(g.rels.len(), 1);
    let r = &g.rels[0];
    assert_eq!((r.a, r.b, r.mb, r.ma), (1, 0, Mark::Arrow, Mark::None));
    assert_eq!((r.card_a.as_str(), r.card_b.as_str()), ("1", "*"));
    let d = build_ok(src);
    for s in ["Tarefa", "ListaDeTarefas", "+concluir()", "+list<Tarefa> itens", "1", "*"] {
        assert!(has_label(&d, s), "{s}");
    }
}

#[test]
fn class_relations_and_members() {
    let src = "classDiagram\ndirection LR\nAnimal <|-- Duck : is a\nCar *-- Wheel\nPond o-- Duck\nA ..> B\nC ..|> D\nAnimal : +int age\nAnimal : +isMammal() bool\n<<interface>> D\nclass Box~T~\n";
    let g = class::parse(&clean(src));
    let id = |n: &str| g.classes.iter().position(|c| c.id == n).unwrap();
    let rel = |a: &str, b: &str| g.rels.iter().find(|r| r.a == id(a) && r.b == id(b)).unwrap();
    assert_eq!(rel("Animal", "Duck").ma, Mark::Triangle);
    assert_eq!(rel("Animal", "Duck").label, "is a");
    assert_eq!(rel("Car", "Wheel").ma, Mark::Diamond);
    assert_eq!(rel("Pond", "Duck").ma, Mark::DiamondOpen);
    assert!(rel("A", "B").dashed && rel("A", "B").mb == Mark::Arrow);
    assert_eq!(rel("C", "D").mb, Mark::Triangle);
    let an = &g.classes[id("Animal")];
    assert_eq!((an.attrs.len(), an.methods.len()), (1, 1));
    assert_eq!(g.classes[id("D")].stereo, "<<interface>>");
    assert_eq!(g.classes[id("Box")].name, "Box<T>");
    assert_eq!(g.dir, layered::Dir::Right);
    build_ok(src);
}

#[test]
fn unsupported_and_broken_input_never_panics() {
    let mut m = meas();
    for (src, want) in [
        ("gantt\ntitle x\n", "gantt"),
        ("stateDiagram-v2\n[*] --> A\n", "stateDiagram"),
        ("mindmap\n root\n", "mindmap"),
    ] {
        let e = build(src, &mut m).unwrap_err();
        assert!(e.contains(want) && e.contains("not supported"), "{e}");
    }
    assert!(build("", &mut m).is_err());
    assert!(build("%% only a comment\n", &mut m).is_err());
    assert!(build("flowchart TD\n", &mut m).is_err(), "no nodes");
    let junk = [
        "flowchart TD\nA[unclosed --> B\n",
        "flowchart\nA -->\n--> B\n[[[]]]\n((((\n",
        "graph LR\nA-- no closer\nB -.- C -. x\n",
        "sequenceDiagram\n->>: x\nNote over: y\nend\nend\nalt\n",
        "sequenceDiagram\nA->>B\n",
        "classDiagram\nclass {\n}\n<<x>>\n-- \"1\" --\nA : \n",
        "pie title\n\"a\" : -1\n\"b\" : nan\n\"c\" : x\n",
        "flowchart TD\nA --> A\nA --> B --> A\n",
        "---\ntitle: front\n---\nflowchart LR\nA-->B\n",
    ];
    for src in junk {
        let _ = build(src, &mut m);
    }
    // Self loops and cycles still lay out.
    build_ok("flowchart TD\nA --> A\nA --> B --> A\n");
    build_ok("---\ntitle: front\n---\nflowchart LR\nA-->B\n");
}

#[test]
fn shapes_rasterize() {
    let d = build_ok("flowchart LR\nA[One] --> B((Two))\n");
    let col = |p: Paint| match p {
        Paint::Node => Rgba(10, 200, 10, 255),
        _ => Rgba(200, 10, 10, 255),
    };
    let pix = render::raster(&d, (d.w * 2.0) as u32, (d.h * 2.0) as u32, &col).expect("pixels");
    assert_eq!(pix.w, (d.w * 2.0) as u32);
    let green = pix.px.iter().filter(|p| (**p >> 8) & 0xff > 150 && **p >> 24 == 255).count();
    assert!(green > 200, "node fills are painted: {green}");
    assert!(render::raster(&d, 0, 10, &col).is_none());
}

use crate::theme::Rgba;
