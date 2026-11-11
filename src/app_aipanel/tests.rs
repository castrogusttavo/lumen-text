use super::*;
use crate::keymap::Keymap;
use winit::keyboard::NamedKey;

fn sig(alive: bool, idle_ms: u64) -> Signals<'static> {
    Signals {
        alive,
        exit: None,
        any_output: true,
        since_start_ms: 10_000,
        idle_ms,
        title: "",
        attention: false,
        worked: false,
        hook: None,
    }
}

#[test]
fn status_follows_process_output_title_and_attention() {
    assert_eq!(status(&Signals { any_output: false, ..sig(true, u64::MAX) }), Status::Starting);
    assert_eq!(status(&Signals { since_start_ms: 400, ..sig(true, 10) }), Status::Starting, "first screen painting");
    assert_eq!(status(&sig(true, 100)), Status::Working, "output streaming");
    assert_eq!(status(&sig(true, 5_000)), Status::Waiting, "idle at its prompt, nothing done yet");
    assert_eq!(status(&Signals { worked: true, ..sig(true, 5_000) }), Status::Done, "quiet after working");
    let spinner = Signals { title: "⠂ Fixing the login bug", ..sig(true, 5_000) };
    assert_eq!(status(&spinner), Status::Working, "a spinner in the title means busy even when quiet");
    let idle_title = Signals { title: "✳ Fixing the login bug", worked: true, ..sig(true, 5_000) };
    assert_eq!(status(&idle_title), Status::Done);
    let asks = Signals { attention: true, worked: true, ..sig(true, 800) };
    assert_eq!(status(&asks), Status::Waiting, "a bell / notification after the output settles");
    assert_eq!(status(&Signals { attention: true, ..sig(true, 50) }), Status::Working, "still printing");
    assert_eq!(status(&Signals { exit: Some(0), ..sig(false, 0) }), Status::Exited(0));
    assert_eq!(status(&Signals { exit: Some(129), ..sig(false, 0) }), Status::Exited(129));
    assert_eq!(status(&sig(false, 0)), Status::Exited(-1));
    assert_eq!(Status::Waiting.label(), "Waiting for you");
    assert_eq!(Status::Exited(2).label(), "Exited (2)");
    assert!(title_busy("⠋ x") && !title_busy("✳ x") && !title_busy(""));
    // The agent's own hooks win over every heuristic, but not over the process having ended.
    assert_eq!(
        status(&Signals { hook: Some(Status::Done), ..sig(true, 10) }),
        Status::Done,
        "streaming, but Stop fired"
    );
    assert_eq!(
        status(&Signals { hook: Some(Status::Working), ..sig(true, 60_000) }),
        Status::Working,
        "quiet tool run"
    );
    let asks = Signals { hook: Some(Status::Waiting), worked: true, ..sig(true, 5) };
    assert_eq!(status(&asks), Status::Waiting, "a permission prompt");
    assert_eq!(status(&Signals { hook: Some(Status::Working), exit: Some(1), ..sig(false, 0) }), Status::Exited(1));
}

#[cfg(unix)]
pub(super) fn stub(dir: &std::path::Path, name: &str, body: &str, exec: bool) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
}

#[test]
#[cfg(unix)]
fn agents_are_detected_in_a_fake_path() {
    let base = std::env::temp_dir().join(format!("lumen-ai-detect-{}", std::process::id()));
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    stub(&a, "codex", "#!/bin/sh\n", true);
    stub(&a, "gemini", "not runnable\n", false);
    stub(&b, "claude", "#!/bin/sh\n", true);
    stub(&b, "gemini", "#!/bin/sh\n", true);
    std::fs::create_dir_all(a.join("claude")).unwrap(); // a directory is not an executable
    let path = std::env::join_paths([&a, &b]).unwrap();
    let dirs = search_dirs(Some(path.as_os_str()), None);
    assert_eq!(&dirs[..2], &[a.clone(), b.clone()], "PATH order first");
    let found = detect(&dirs);
    let at = |id: &str| found.iter().find(|(x, _)| x.id == id).and_then(|(_, p)| p.clone());
    assert_eq!(at("claude"), Some(b.join("claude")), "the directory named claude is skipped");
    assert_eq!(at("gemini"), Some(b.join("gemini")), "the non-executable one is skipped");
    assert_eq!(at("codex"), Some(a.join("codex")));
    let empty = detect(&[base.join("nothing")]);
    assert!(empty.iter().all(|(_, p)| p.is_none()));
    assert_eq!(agent("claude").map(|x| x.name), Some("Claude Code"));
    assert!(agent("nope").is_none());
    // The agent's own folder goes first on its PATH (node next to the npm-installed CLI).
    let p = path_with(Some(b.as_path()), Some(path.as_os_str())).unwrap();
    let parts: Vec<std::path::PathBuf> = std::env::split_paths(&p).collect();
    assert_eq!(parts, vec![b.clone(), a.clone()]);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn remembered_choice_roundtrips() {
    let p = std::env::temp_dir().join(format!("lumen-ai-state-{}/ai.json", std::process::id()));
    assert_eq!(load_choice(&p), None);
    save_choice(&p, "gemini");
    assert_eq!(load_choice(&p).as_deref(), Some("gemini"));
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn agent_keeps_ctrl_letters_and_escape_lumen_keeps_its_few() {
    let km = Keymap::default();
    let ch = |c: &str| Key::Character(c.into());
    // Ctrl+J (new line in the agent) and Ctrl+B reach the agent even though Ctrl+J is bound.
    for k in ["j", "b", "c", "r", "l", "o", "t", "e", "p", "f", "g", "k", "w"] {
        assert!(!ai_global(&km, true, false, false, &ch(k)), "ctrl+{k} must reach the agent");
    }
    assert!(!ai_global(&km, false, false, false, &Key::Named(NamedKey::Escape)), "Esc interrupts the agent");
    assert!(!ai_global(&km, false, true, false, &Key::Named(NamedKey::Enter)));
    assert!(!ai_global(&km, false, true, false, &Key::Named(NamedKey::Tab)));
    assert!(ai_global(&km, true, false, false, &ch("q")), "Ctrl+Q still quits");
    assert!(ai_global(&km, true, true, false, &ch("P")), "the command palette");
    assert!(ai_global(&km, true, true, false, &ch("A")), "the panel toggle");
    assert!(ai_global(&km, true, false, false, &ch("1")), "Ctrl+1 back to the editor");
    assert!(!ai_global(&km, true, true, false, &ch("G")), "Git panel shortcut is not kept");
    // A rebinding to a non-letter combo keeps working; a plain Ctrl+letter binding does not.
    let km = Keymap::from_json(r#"{"ctrl+alt+o": "ToggleOutput", "ctrl+y": "AiPanel"}"#);
    assert!(ai_global(&km, true, false, true, &ch("o")));
    assert!(!ai_global(&km, true, false, false, &ch("y")));
}

/// A fake agent: prints a prompt, then shows every byte it receives as `[xx]` (hex).
#[cfg(unix)]
const STUB_AGENT: &str = "#!/bin/sh\n\
printf 'stub-agent ready\\r\\n'\n\
printf '\\033]0;\\342\\234\\263 stub\\007'\n\
stty raw -echo 2>/dev/null\n\
while :; do\n\
  c=$(dd bs=1 count=1 2>/dev/null | od -An -tx1)\n\
  [ -z \"$c\" ] && exit 0\n\
  printf '[%s]' $c\n\
done\n";

#[cfg(unix)]
pub(super) fn wait_screen(app: &mut App, want: &str) -> String {
    let end = Instant::now() + Duration::from_secs(15);
    let mut text = String::new();
    while Instant::now() < end {
        app.pump_ai();
        text = app
            .ai
            .term
            .as_ref()
            .map(|t| (0..t.grid.rows).map(|r| t.grid.view_row_text(r, 0)).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        if text.contains(want) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    text
}

#[test]
#[cfg(unix)]
fn ai_panel_runs_a_stub_agent_end_to_end() {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    let _g = lock();
    let dir = tmp("ai-e2e");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "claude", STUB_AGENT, true);
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.ai.search = Some(vec![bin.clone()]);
    app.run_bound("AiPanel");
    assert!(app.ai.open && app.ai.focused, "the toggle shows and focuses the panel");
    assert_eq!(app.ai.agent.map(|a| a.id), Some("claude"));
    assert!(app.ai.term.is_some(), "the installed agent starts by itself");
    let first = wait_screen(&mut app, "stub-agent ready");
    assert!(first.contains("stub-agent ready"), "{first:?}");
    assert_eq!(app.ai.term.as_ref().map(|t| t.grid.title.as_str()), Some("✳ stub"));
    // Let the stub switch its tty to raw mode before typing.
    std::thread::sleep(Duration::from_millis(300));
    for c in ["h", "i"] {
        app.handle_key(&KeyIn { key: Key::Character(c.into()), text: Some(c.into()) });
    }
    let typed = wait_screen(&mut app, "[68][69]");
    assert!(typed.contains("[68][69]"), "typing reaches the agent: {typed:?}");
    app.mods = winit::keyboard::ModifiersState::SHIFT;
    app.handle_key(&KeyIn { key: Key::Named(NamedKey::Enter), text: Some("\r".into()) });
    app.mods = winit::keyboard::ModifiersState::empty();
    let enter = wait_screen(&mut app, "[1b][5b][31][33][3b][32][75]");
    assert!(enter.contains("[1b][5b][31][33][3b][32][75]"), "Shift+Enter is ESC [13;2u: {enter:?}");
    // Esc goes to the agent too (it interrupts), Ctrl+J is a newline for it.
    app.handle_key(&KeyIn { key: Key::Named(NamedKey::Escape), text: None });
    app.mods = winit::keyboard::ModifiersState::CONTROL;
    app.handle_key(&KeyIn { key: Key::Character("j".into()), text: None });
    app.mods = winit::keyboard::ModifiersState::empty();
    let more = wait_screen(&mut app, "[1b][0a]");
    assert!(more.contains("[1b][0a]"), "{more:?}");
    assert!(!app.panel.open, "Ctrl+J did not toggle the output panel");
    // Drawing a frame lays the terminal out (no panic, sizes follow the panel).
    let (w, h) = (1200, 800);
    let mut px = vec![0u32; (w * h) as usize];
    let mut cv = crate::ui::Canvas { px: &mut px, w, h };
    app.render_into(&mut cv, 0.016);
    let p = app.aipanel_rect();
    assert!(p.w > 0 && app.ai.hits.iter().any(|(_, h)| *h == AiHit::Stop), "header buttons are laid out");
    assert_eq!(app.layout.editor.right(), p.x, "the editor makes room for the slot");
    // Hiding keeps the agent alive; the badge appears when it rings while hidden.
    app.run_bound("AiPanel"); // focused -> hide
    assert!(!app.ai.open);
    assert!(app.ai.term.as_ref().is_some_and(|t| t.alive), "hiding never stops the agent");
    // Stop hangs it up; the status turns into Exited.
    app.ai_stop();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && app.ai.term.as_ref().is_some_and(|t| t.alive) {
        app.pump_ai();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(app.ai.status, Status::Exited(_)), "{:?}", app.ai.status);
    // The Git panel and the AI panel share the slot: opening one hides the other.
    app.ai_show();
    assert!(app.ai.open);
    app.open_git_panel();
    assert!(!app.ai.open && app.gitpanel.is_some());
    app.ai_show();
    assert!(app.ai.open && app.gitpanel.is_none());
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
