//! Real language servers, end to end through the editor (`#[ignore]`d: needs the servers installed).
//!
//! `tools/install-lsp-servers.sh`, then `cargo test --release lsp_matrix -- --ignored --nocapture --test-threads=1`
//! (CI job "Language servers")
//! (`LUMEN_LSP_ONLY=java,csharp` runs a subset; `LUMEN_LSP_FILES=a.py:b.go` opens real files instead).
//!
//! Each case writes a tiny project with a deliberate error, opens the file in a headless `App` with
//! the built-in registry, and checks what the user would see: the server starts, diagnostics arrive,
//! hover and completion answer. The result is a table; the test fails when a case that has its
//! server installed does not work.

use crate::app::App;
use crate::app_unsaved::testkit::{lock, scratch_base, test_app};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Case {
    name: &'static str,
    /// Files of the project (relative path, content); the first one is opened.
    files: &'static [(&'static str, &'static str)],
    /// Text the hover/completion anchor is placed on (first occurrence in the opened file).
    hover_on: &'static str,
    /// Diagnostics are expected (some servers only lint with extra tools).
    want_diag: bool,
    /// Seconds to wait for the first answer (JVM / .NET servers index first).
    wait: u64,
}

const CASES: &[Case] = &[
    Case {
        name: "c",
        files: &[
            (
                "main.c",
                "int add(int a, int b) { return a + b; }\nint main(void) {\n    int x = add(1);\n    return x;\n}\n",
            ),
            ("compile_flags.txt", "-std=c11\n"),
        ],
        hover_on: "add(1",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "cpp",
        files: &[
            (
                "main.cpp",
                "#include <string>\nint twice(int v) { return v * 2; }\nint main() {\n    std::string s = twice(2);\n    return 0;\n}\n",
            ),
            ("compile_flags.txt", "-std=c++20\n"),
        ],
        hover_on: "twice(2",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "csharp",
        files: &[
            (
                "Program.cs",
                "class Program {\n    static int Twice(int v) => v * 2;\n    static void Main() {\n        int x = \"text\";\n        System.Console.WriteLine(Twice(x));\n    }\n}\n",
            ),
            (
                "App.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <OutputType>Exe</OutputType>\n    <TargetFramework>net10.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n",
            ),
        ],
        hover_on: "Twice(x",
        want_diag: true,
        wait: 180,
    },
    Case {
        name: "java",
        files: &[(
            "src/Main.java",
            "public class Main {\n    static int twice(int v) { return v * 2; }\n    public static void main(String[] args) {\n        int x = \"text\";\n        System.out.println(twice(x));\n    }\n}\n",
        )],
        hover_on: "twice(x",
        want_diag: true,
        wait: 180,
    },
    Case {
        name: "kotlin",
        files: &[(
            "Main.kt",
            "fun twice(v: Int): Int = v * 2\n\nfun main() {\n    val x: Int = \"text\"\n    println(twice(x))\n}\n",
        )],
        hover_on: "twice(x",
        // kotlin-lsp reports diagnostics for files of an imported Gradle/Maven project only.
        want_diag: false,
        wait: 240,
    },
    Case {
        name: "typescript",
        files: &[
            (
                "src/main.ts",
                "export function twice(v: number): number { return v * 2 }\nconst x: number = \"text\"\nconsole.log(twice(x))\n",
            ),
            ("tsconfig.json", "{\"compilerOptions\":{\"strict\":true,\"noEmit\":true},\"include\":[\"src\"]}\n"),
        ],
        hover_on: "twice(x",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "javascript",
        files: &[(
            "main.js",
            "// @ts-check\nfunction twice(v) { return v * 2 }\n/** @type {number} */\nconst x = \"text\"\nconsole.log(twice(x))\n",
        )],
        hover_on: "twice(x",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "css",
        files: &[("style.css", "body {\n  colr: red;\n  margin: 0;\n}\n")],
        hover_on: "margin",
        want_diag: true,
        wait: 20,
    },
    Case {
        name: "scss",
        files: &[("style.scss", "$c: red;\nbody {\n  colr: $c;\n  margin: 0;\n}\n")],
        hover_on: "margin",
        want_diag: true,
        wait: 20,
    },
    Case {
        name: "less",
        files: &[("style.less", "@c: red;\nbody {\n  colr: @c;\n  margin: 0;\n}\n")],
        hover_on: "margin",
        want_diag: true,
        wait: 20,
    },
    Case {
        name: "html",
        files: &[("index.html", "<!doctype html>\n<html>\n<body>\n  <section>hi</section>\n</body>\n</html>\n")],
        hover_on: "section>hi",
        want_diag: false,
        wait: 20,
    },
    Case {
        name: "json",
        files: &[("package.json", "{\n  \"name\": \"x\",\n  \"version\": 1,\n  \"scripts\": {}\n}\n")],
        hover_on: "scripts",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "yaml",
        files: &[(
            ".github/workflows/ci.yml",
            "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    stepz:\n      - run: echo hi\n",
        )],
        hover_on: "runs-on",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "toml",
        files: &[("Cargo.toml", "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = 2021\n\n[dependencies]\n")],
        hover_on: "edition",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "python",
        files: &[
            (
                "main.py",
                "import os\n\n\ndef twice(v: int) -> int:\n    return v * 2\n\n\nx: int = \"text\"\nprint(twice(x))\n",
            ),
            ("pyproject.toml", "[project]\nname = \"m\"\nversion = \"0\"\n"),
        ],
        hover_on: "twice(x",
        want_diag: true,
        wait: 40,
    },
    Case {
        name: "go",
        files: &[
            (
                "main.go",
                "package main\n\nimport \"fmt\"\n\nfunc twice(v int) int { return v * 2 }\n\nfunc main() {\n\tvar x int = \"text\"\n\tfmt.Println(twice(x))\n}\n",
            ),
            ("go.mod", "module example.com/m\n\ngo 1.22\n"),
        ],
        hover_on: "twice(x",
        want_diag: true,
        wait: 60,
    },
    Case {
        name: "rust",
        files: &[
            (
                "src/main.rs",
                "fn twice(v: i32) -> i32 { v * 2 }\n\nfn main() {\n    let x: i32 = \"text\";\n    println!(\"{}\", twice(x));\n}\n",
            ),
            ("Cargo.toml", "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ],
        hover_on: "twice(x",
        want_diag: true,
        wait: 120,
    },
    Case {
        name: "ruby",
        // Ruby servers only report syntax errors without RuboCop, and a syntax error stops hover:
        // valid code, hover and completion only.
        files: &[("main.rb", "class Greeter\n  def twice(v)\n    v * 2\n  end\nend\n\nputs Greeter.new.twice(2)\n")],
        hover_on: "twice(2",
        want_diag: false,
        wait: 60,
    },
    Case {
        name: "php",
        files: &[(
            "index.php",
            "<?php\nfunction twice(int $v): int { return $v * 2; }\necho twice(2);\necho twice(;\n",
        )],
        hover_on: "twice(2",
        want_diag: true,
        wait: 40,
    },
    Case {
        name: "lua",
        files: &[("main.lua", "local function twice(v)\n  return v * 2\nend\nprint(twice(2), undefined_global)\n")],
        hover_on: "twice(2",
        want_diag: true,
        wait: 30,
    },
    Case {
        name: "zig",
        files: &[(
            "main.zig",
            // zls reports what `zig ast-check` finds (a type error needs build-on-save).
            "const std = @import(\"std\");\nfn twice(v: i32) i32 {\n    return v * 2;\n}\npub fn main() void {\n    const unused = 5;\n    std.debug.print(\"{}\", .{twice(2)});\n}\n",
        )],
        hover_on: "twice(2",
        want_diag: true,
        wait: 60,
    },
    Case {
        name: "bash",
        files: &[("run.sh", "#!/bin/bash\ngreet() { echo \"hi $1\"; }\ngreet world\n")],
        hover_on: "echo",
        want_diag: false,
        wait: 20,
    },
    Case {
        name: "markdown",
        // marksman only analyses inside a workspace (`.git` or `.marksman.toml`).
        files: &[
            ("README.md", "# Title\n\nSee [the guide](missing.md) and [usage](#usage).\n\n## Usage\n"),
            (".marksman.toml", ""),
        ],
        hover_on: "usage)",
        want_diag: true,
        wait: 20,
    },
    Case {
        name: "dockerfile",
        files: &[("Dockerfile", "FROM ubuntu:24.04\nRUNN echo hi\nWORKDIR /app\n")],
        hover_on: "WORKDIR",
        want_diag: true,
        wait: 20,
    },
];

#[derive(Default)]
struct Outcome {
    server: String,
    started: bool,
    diags: Vec<String>,
    hover: bool,
    completion: bool,
    ms_first: u128,
    note: String,
}

fn pump(app: &mut App, ms: u64, mut cond: impl FnMut(&mut App) -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        app.lsp_tick(Instant::now(), &mut |_| {});
        app.pump_lsp();
        if cond(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    false
}

/// Open `file` in `app` and exercise diagnostics, hover and completion at `anchor` (char offset).
fn exercise(app: &mut App, file: &Path, anchor: Option<&str>, want_diag: bool, wait: u64) -> Outcome {
    let mut o = Outcome::default();
    let t0 = Instant::now();
    let Some(i) = app.open_path(file, None) else {
        o.note = "could not open".into();
        return o;
    };
    app.active = i;
    o.started = pump(app, wait * 1000, |a| a.lsps.iter().any(|l| l.ready));
    o.server = app.lsps.iter().map(|l| l.name.clone()).collect::<Vec<_>>().join("+");
    if !o.started {
        let rows = app.lspx_rows();
        let lang = app.docs[i].lang.unwrap_or("-");
        o.note = format!(
            "no server ready (lang {lang}); rows: {}",
            rows.iter()
                .filter(|r| r.state != crate::app_lspx::RowState::NotInstalled
                    && r.state != crate::app_lspx::RowState::Ready)
                .map(|r| format!("{}={:?} ({})", r.id, r.state, r.detail.lines().last().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(" ")
        );
        return o;
    }
    if want_diag {
        pump(app, wait * 1000, |a| !a.docs[i].diags.is_empty());
    } else {
        pump(app, 3000, |a| !a.docs[i].diags.is_empty());
    }
    o.ms_first = t0.elapsed().as_millis();
    o.diags = app.docs[i]
        .diags
        .iter()
        .map(|d| format!("{}:{} [{}] {}", d.sl + 1, d.sc + 1, d.source, d.message.lines().next().unwrap_or("")))
        .collect();

    let text = app.docs[i].buf.to_string();
    let at = anchor.and_then(|a| text.find(a)).map(|b| text[..b].chars().count()).unwrap_or(0);
    // Servers answer hover only once they have analysed the file: retry for a while.
    let hover_end = Instant::now() + Duration::from_secs(wait.min(60));
    while Instant::now() < hover_end && !o.hover {
        app.hover = None;
        let d = &mut app.docs[i];
        d.ed.click(&mut d.buf, at + 1, false);
        app.request_hover(at + 1);
        o.hover = pump(app, 4000, |a| a.hover.as_ref().is_some_and(|h| !h.blocks.is_empty() || !h.sig.is_empty()));
    }
    app.completion = None;
    let d = &mut app.docs[i];
    d.ed.click(&mut d.buf, at + 2, false);
    app.request_completion(None);
    o.completion = pump(app, 8000, |a| a.completion.as_ref().is_some_and(|c| !c.items.is_empty()));
    for l in &app.lsps {
        if std::env::var_os("LUMEN_LSP_LOG").is_some() {
            eprintln!("----- log of {} -----\n{}", l.name, l.log_text());
        }
        if l.log_text().to_ascii_lowercase().contains("panic") {
            o.note = format!("{} log mentions panic", l.name);
        }
    }
    o
}

fn stop(app: &mut App) {
    for l in &mut app.lsps {
        l.shutdown();
    }
}

fn row(name: &str, o: &Outcome) -> String {
    let yn = |b: bool| if b { "yes" } else { "NO" };
    format!(
        "{name:<11} {:<34} start {:<3} diag {:<3} hover {:<3} compl {:<3} {:>6} ms  {}{}",
        if o.server.is_empty() { "-" } else { &o.server },
        yn(o.started),
        o.diags.len(),
        yn(o.hover),
        yn(o.completion),
        o.ms_first,
        o.diags.first().map(|s| s.chars().take(70).collect::<String>()).unwrap_or_default(),
        if o.note.is_empty() { String::new() } else { format!("  [{}]", o.note) },
    )
}

#[test]
#[ignore = "needs real language servers installed"]
fn lsp_matrix() {
    let _g = lock();
    let only: Option<Vec<String>> =
        std::env::var("LUMEN_LSP_ONLY").ok().map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    // Not under ~/.cache: schema globs such as `**/.github/workflows/*.yml` do not cross hidden
    // folders, so a project below `.cache` gets no schema at all.
    let base = std::env::temp_dir().join(format!("lumen-lsp-matrix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut table = Vec::new();
    let mut failed = Vec::new();
    for c in CASES {
        if only.as_ref().is_some_and(|o| !o.iter().any(|n| n == c.name)) {
            continue;
        }
        let dir = base.join(c.name);
        for (rel, text) in c.files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
        }
        let dir = crate::paths::canonical(&dir).unwrap();
        let mut app = test_app(&dir);
        let o = exercise(&mut app, &dir.join(c.files[0].0), Some(c.hover_on), c.want_diag, c.wait);
        stop(&mut app);
        let line = row(c.name, &o);
        eprintln!("{line}");
        let ok = o.started && (!c.want_diag || !o.diags.is_empty()) && o.hover;
        if !ok && !o.note.starts_with("no server ready") {
            failed.push(c.name);
        } else if !o.started {
            failed.push(c.name);
        }
        table.push(line);
    }
    eprintln!("\n===== LSP matrix =====\n{}\n", table.join("\n"));
    let _ = std::fs::remove_dir_all(&base);
    assert!(failed.is_empty(), "failing languages: {failed:?}");
}

/// Completion at char offset `at` of document `i`, the way the editor asks for it (`trigger` is the
/// typed trigger character, if any). Returns the labels of the list that came back.
fn complete_at(app: &mut App, i: usize, at: usize, trigger: Option<char>, wait_ms: u64) -> Vec<String> {
    app.completion = None;
    let d = &mut app.docs[i];
    d.ed.click(&mut d.buf, at, false);
    app.request_completion(trigger);
    pump(app, wait_ms, |a| a.completion.as_ref().is_some_and(|c| !c.items.is_empty()));
    app.completion.as_ref().map(|c| c.items.iter().map(|it| it.label.clone()).collect()).unwrap_or_default()
}

/// Ask for completion at `at` until `want` is among the labels (gopls needs a while to load the
/// module; until then `ghp-lsp` answers from the page's own scope analysis).
fn complete_until(app: &mut App, i: usize, at: usize, trigger: Option<char>, want: &str) -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(90);
    loop {
        let got = complete_at(app, i, at, trigger, 4000);
        if got.iter().any(|l| l == want) || Instant::now() >= end {
            return got;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// GHP templates (`.ghp`): Lumen starts `ghp-lsp` for a template with a Go module around it, shows
/// its diagnostics, and gets hover and completion for the Go expressions through the real client:
/// scope-based names right away, then gopls' members (`user.` -> `Name`, `Admin`) and package-level
/// names (`Current`), also inside an empty `<go= />`.
/// Skips (passes) while `ghp-lsp` is not installed, unless `LUMEN_REQUIRE_GHP_LSP` is set (CI does
/// once the server is in the tree). Build it with `cargo build --release --manifest-path tools/ghp-lsp/Cargo.toml`
/// and put `tools/ghp-lsp/target/release` on PATH.
#[test]
#[ignore = "needs ghp-lsp installed"]
fn lsp_ghp() {
    let _g = lock();
    let Some(exe) = crate::lspreg::installed_anywhere("ghp-lsp") else {
        assert!(
            !std::env::var("LUMEN_REQUIRE_GHP_LSP").is_ok_and(|v| !v.is_empty()),
            "ghp-lsp is required but not installed"
        );
        eprintln!("ghp-lsp not installed: skipped");
        return;
    };
    eprintln!("ghp-lsp: {}", exe.display());
    let dir = std::env::temp_dir().join(format!("lumen-lsp-ghp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pages")).unwrap();
    std::fs::write(dir.join("go.mod"), "module demo\n\ngo 1.22\n").unwrap();
    // The Go the page talks to: a sibling `.go` file names the package of the page's Go code.
    std::fs::write(
        dir.join("pages/models.go"),
        "package pages\n\ntype User struct {\n\tName  string\n\tAdmin bool\n}\n\nfunc Current() User { return User{Name: \"Ada\", Admin: true} }\n",
    )
    .unwrap();
    // `<go:if>` is never closed: the server must say so. `<go= />` is empty on purpose.
    let page = "<!doctype html>\n<html>\n<body>\n    <go user := Current() />\n    <p><go= user.Name /></p>\n    <p><go= /></p>\n    <go:if user.Admin/>\n        <p>admin</p>\n</body>\n</html>\n";
    std::fs::write(dir.join("pages/home.ghp"), page).unwrap();
    let dir = crate::paths::canonical(&dir).unwrap();
    let mut app = test_app(&dir);
    let o = exercise(&mut app, &dir.join("pages/home.ghp"), Some("user.Name"), true, 60);
    eprintln!("\n===== GHP =====\n{}\n", row("ghp", &o));
    assert!(o.started, "ghp-lsp did not start: {}", o.note);
    assert_eq!(o.server, "ghp-lsp");
    assert!(!o.diags.is_empty(), "no diagnostics for an unclosed <go:if>");
    assert!(o.hover, "no hover on a Go expression");
    assert!(o.completion, "no completion at `us|er.Name`");

    let i = app.active;
    let text = app.docs[i].buf.to_string();
    let char_at = |needle: &str, delta: usize| text[..text.find(needle).unwrap()].chars().count() + delta;

    // Right after the dot, only gopls knows the members of `user`.
    let members = complete_until(&mut app, i, char_at("user.Name", 5), Some('.'), "Admin");
    // The cursor inside the empty `<go= />` is a Go position: gopls offers the package-level names
    // (`Current`, from the sibling file) next to the local `user`.
    let in_empty = complete_until(&mut app, i, char_at("<go= /></p>", 5), None, "Current");
    stop(&mut app);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(members.iter().any(|l| l == "Name") && members.iter().any(|l| l == "Admin"), "members: {members:?}");
    assert!(in_empty.iter().any(|l| l == "Current"), "package-level names in `<go= />`: {in_empty:?}");
    assert!(in_empty.iter().any(|l| l == "user"), "local names in `<go= />`: {in_empty:?}");
}

/// The spell checkers (companions, off unless `spell_check` is on): each one, alone, must start
/// for a plain-text file and a Markdown file, flag the misspelled words as "info" (never an error)
/// and put its fixes in the `Ctrl+.` menu ("receive" for "recieve"; harper-ls also offers to add
/// the word to a dictionary, a bare LSP `Command`).
#[test]
#[ignore = "needs real language servers installed"]
fn lsp_matrix_spell() {
    let _g = lock();
    let only: Option<Vec<String>> =
        std::env::var("LUMEN_LSP_ONLY").ok().map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    let base = std::env::temp_dir().join(format!("lumen-lsp-spell-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let prose = "We will recieve the seperate files tomorow.\n";
    let files = [("notes.txt", prose.to_string()), ("README.md", format!("# Notes\n\n{prose}"))];
    let mut table = Vec::new();
    let mut failed = Vec::new();
    for &id in crate::lspreg::SPELL_SERVERS {
        if only.as_ref().is_some_and(|o| !o.iter().any(|n| n == id || n == "spell")) {
            continue;
        }
        for (name, text) in &files {
            let dir = base.join(format!("{id}-{}", name.replace('.', "-")));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), text).unwrap();
            let dir = crate::paths::canonical(&dir).unwrap();
            let mut app = test_app(&dir);
            app.config.spell_check = true;
            // One spell checker at a time, and no Markdown primary: what is seen is this server's.
            let other: Vec<&str> = crate::lspreg::SPELL_SERVERS.iter().copied().filter(|s| *s != id).collect();
            let mut ls = serde_json::json!({"marksman": {"enabled": false}});
            for o in other {
                ls[o] = serde_json::json!({"enabled": false});
            }
            app.config.language_servers = ls;
            let t0 = Instant::now();
            let mut note = String::new();
            let Some(i) = app.open_path(&dir.join(name), None) else {
                failed.push(format!("{id} {name}: could not open"));
                continue;
            };
            app.active = i;
            let started = pump(&mut app, 30_000, |a| a.lsps.iter().any(|l| l.name == id && l.ready));
            let diags = started && pump(&mut app, 30_000, |a| a.docs[i].has_ext_diags(id));
            let ms = t0.elapsed().as_millis();
            let flagged: Vec<String> = app.docs[i]
                .diags
                .iter()
                .map(|d| {
                    let (l, c0, c1) = (d.sl, d.sc, d.ec);
                    let word: String = app.docs[i].buf.line(l).chars().skip(c0).take(c1.saturating_sub(c0)).collect();
                    if d.severity != 3 {
                        note = format!("severity {} for {word}", d.severity);
                    }
                    word
                })
                .collect();
            // Fixes for the first misspelled word.
            let mut labels: Vec<String> = Vec::new();
            if diags {
                let text = app.docs[i].buf.to_string();
                let at = text[..text.find("recieve").unwrap()].chars().count() + 2;
                let d = &mut app.docs[i];
                d.ed.click(&mut d.buf, at, false);
                app.overlay = crate::app_overlay::Overlay::None;
                app.code_actions();
                pump(&mut app, 15_000, |a| matches!(a.overlay, crate::app_overlay::Overlay::Picker(_)));
                if let crate::app_overlay::Overlay::Picker(p) = &app.overlay {
                    labels = p.items.iter().map(|it| it.label.clone()).collect();
                }
            }
            stop(&mut app);
            let fix = labels.iter().any(|l| l.contains("receive"));
            let dict = id != "harper-ls" || labels.iter().any(|l| l.to_lowercase().contains("dictionary"));
            let ok = started && flagged.iter().any(|w| w == "recieve") && note.is_empty() && fix && dict;
            let line = format!(
                "{id:<10} {name:<10} start {:<3} flagged {:<28} fixes {:<3} {:>6} ms  {}{}",
                if started { "yes" } else { "NO" },
                flagged.join(","),
                labels.len(),
                ms,
                labels.iter().take(4).cloned().collect::<Vec<_>>().join(" | "),
                if note.is_empty() { String::new() } else { format!("  [{note}]") },
            );
            eprintln!("{line}");
            table.push(line);
            if !ok {
                failed.push(format!("{id} {name}"));
            }
        }
    }
    eprintln!("\n===== LSP spell matrix =====\n{}\n", table.join("\n"));
    let _ = std::fs::remove_dir_all(&base);
    assert!(failed.is_empty(), "failing spell checks: {failed:?}");
}

/// Open real files (`LUMEN_LSP_FILES=a:b:c`), each in an `App` rooted at its project, and report
/// what the servers say. Never fails: it is a survey for false positives on real code.
#[test]
#[ignore = "needs real language servers and LUMEN_LSP_FILES"]
fn lsp_real_files() {
    let _g = lock();
    let Ok(list) = std::env::var("LUMEN_LSP_FILES") else { return eprintln!("set LUMEN_LSP_FILES") };
    let wait: u64 = std::env::var("LUMEN_LSP_WAIT").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    // The App lives in a scratch folder (test_app writes files there); servers still pick the
    // file's real project root from their root markers.
    let scratch = scratch_base().join(format!("lsp-real-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let scratch = crate::paths::canonical(&scratch).unwrap();
    for f in list.split(':').filter(|s| !s.is_empty()) {
        let file = PathBuf::from(f);
        let mut app = test_app(&scratch);
        let o = exercise(&mut app, &file, None, true, wait);
        stop(&mut app);
        eprintln!("\n### {f}\n{}", row("file", &o));
        for d in o.diags.iter().take(15) {
            eprintln!("    {d}");
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
