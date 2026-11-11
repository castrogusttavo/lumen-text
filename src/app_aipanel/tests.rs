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
            .term()
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
    assert_eq!(app.ai.agent().map(|a| a.id), Some("claude"));
    assert!(app.ai.term().is_some(), "the installed agent starts by itself");
    assert_eq!(app.ai.sessions.len(), 1);
    assert_eq!(app.ai.sessions[0].name, "Claude Code");
    assert_eq!(app.ai.sessions[0].cwd, app.root, "sessions run in the project by default");
    let first = wait_screen(&mut app, "stub-agent ready");
    assert!(first.contains("stub-agent ready"), "{first:?}");
    assert_eq!(app.ai.term().map(|t| t.grid.title.as_str()), Some("✳ stub"));
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
    assert!(app.ai.hits.iter().any(|(_, h)| *h == AiHit::Session(0)), "the session's tab is drawn");
    assert!(app.ai.hits.iter().any(|(_, h)| *h == AiHit::New), "and the + for another one");
    assert_eq!(app.layout.editor.right(), p.x, "the editor makes room for the slot");
    // Hiding keeps the agent alive; the badge appears when it rings while hidden.
    app.run_bound("AiPanel"); // focused -> hide
    assert!(!app.ai.open);
    assert!(app.ai.term().is_some_and(|t| t.alive), "hiding never stops the agent");
    // Stop hangs it up; the status turns into Exited.
    app.ai_stop();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && app.ai.term().is_some_and(|t| t.alive) {
        app.pump_ai();
        std::thread::sleep(Duration::from_millis(20));
    }
    let st = app.ai.cur().map(Session::status);
    assert!(matches!(st, Some(Status::Exited(_))), "{st:?}");
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

fn obs(now: Instant, bells: u64) -> Obs<'static> {
    Obs {
        alive: true,
        exit: None,
        started: now - Duration::from_secs(10),
        last_output: Some(now - Duration::from_secs(5)),
        last_input: None,
        bells,
        title: "",
    }
}

#[test]
fn one_turn_one_notification_and_unseen_marks() {
    let now = Instant::now();
    let mut t = Track::default();
    let step = |t: &mut Track, hook: Status, watching: bool| {
        t.hook = Some(hook);
        if hook == Status::Waiting {
            t.attention = true;
        }
        let c = t.observe(now, &obs(now, 0), watching);
        let notice = t.wants_notice(c);
        if notice {
            t.notified = true;
        }
        (c, notice)
    };
    let (c, n) = step(&mut t, Status::Working, false);
    assert!(c.changed && !c.alert && !n, "working: nothing to tell");
    let (c, n) = step(&mut t, Status::Done, false);
    assert!(c.alert && n, "the turn ended: one notification");
    assert!(t.unseen, "nobody looked: the tab and the badge show it");
    let (c, n) = step(&mut t, Status::Waiting, false);
    assert!(c.alert && !n, "the idle reminder of the same turn stays quiet");
    t.on_input();
    assert!(!t.unseen && !t.notified && t.hook.is_none(), "typing starts a new turn");
    step(&mut t, Status::Working, true);
    let (c, n) = step(&mut t, Status::Waiting, true);
    assert!(c.alert && n, "a permission prompt in the new turn notifies again");
    assert!(!t.unseen, "watched: no unseen mark");
    // Without hooks, a bell after the output settles is an alert too.
    let mut h = Track::default();
    let mut o = obs(now, 0);
    o.last_output = Some(now - Duration::from_millis(100));
    h.observe(now, &o, false);
    assert_eq!(h.status, Status::Working, "output streaming");
    o.bells = 1;
    let c = h.observe(now + Duration::from_secs(2), &o, false);
    assert!(h.attention && c.alert && h.status == Status::Waiting && h.wants_notice(c));
}

#[test]
fn badge_names_and_next_waiting() {
    use Status::*;
    assert_eq!(badge(&[]), None);
    assert_eq!(badge(&[(Waiting, false), (Done, false)]), None, "seen ones are not counted");
    assert_eq!(badge(&[(Waiting, true)]), Some(("Waiting for you".into(), Waiting)));
    assert_eq!(badge(&[(Waiting, true), (Waiting, true), (Done, true)]), Some(("2 waiting · 1 done".into(), Waiting)));
    assert_eq!(badge(&[(Done, true), (Working, true)]), Some(("Agent done".into(), Done)));
    assert_eq!(badge(&[(Done, true), (Done, true)]), Some(("2 done".into(), Done)));
    assert_eq!(badge(&[(Exited(1), true)]), Some(("Agent exited".into(), Exited(0))));
    let s = [Waiting, Working, Done, Waiting];
    assert_eq!(next_waiting(&s, 0), Some(3));
    assert_eq!(next_waiting(&s, 3), Some(0), "wraps around");
    assert_eq!(next_waiting(&[Working, Waiting], 1), Some(1), "the session shown itself comes last");
    assert_eq!(next_waiting(&[Working, Done], 0), None);
    assert_eq!(next_waiting(&[], 0), None);
    let claude = agent("claude").unwrap();
    let root = Path::new("/p/app");
    assert_eq!(session_name(claude, root, root, &[]), "Claude Code");
    assert_eq!(session_name(claude, root, root, &["Claude Code"]), "Claude Code 2");
    assert_eq!(session_name(claude, root, root, &["Claude Code", "Claude Code 2"]), "Claude Code 3");
    assert_eq!(session_name(claude, Path::new("/p/app-wt/fix-login"), root, &[]), "Claude Code · fix-login");
    assert_eq!(title_task("✳ Fixing the login bug "), "Fixing the login bug");
    assert_eq!(title_task("⠂ "), "");
    assert_eq!(ppid_from_stat("4242 (node) S 4100 4242 4242 0 -1"), Some(4100));
    assert_eq!(ppid_from_stat("7 (a) b (c)) R 1 7 7"), Some(1), "names may hold parentheses and spaces");
    assert_eq!(ppid_from_stat("garbage"), None);
    assert!(same_or_child(std::process::id(), std::process::id()));
    #[cfg(target_os = "linux")]
    {
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        assert!(same_or_child(child.id(), std::process::id()), "a child of ours descends from us");
        assert!(!same_or_child(std::process::id(), child.id()));
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[test]
fn pastes_go_whole_in_one_bracketed_paste() {
    assert_eq!(paste_payload("a\nb", false), b"a\rb".to_vec());
    assert_eq!(paste_payload("a\r\nb", true), b"\x1b[200~a\rb\x1b[201~".to_vec());
    // A paste-end marker inside the text cannot end the paste early.
    assert_eq!(paste_payload("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~".to_vec());
    let big: String = (0..3000).map(|i| format!("line {i} {}\n", "x".repeat(30))).collect();
    assert!(big.len() > 64 * 1024);
    let p = paste_payload(&big, true);
    assert_eq!(p.len(), big.len() + 12, "nothing cut, nothing added but the two markers");
    assert_eq!(p.windows(6).filter(|w| *w == b"\x1b[200~").count(), 1);
    assert_eq!(p.windows(6).filter(|w| *w == b"\x1b[201~").count(), 1);
    assert!(!p.contains(&b'\n'), "line ends are Enter inside the paste, not separate submits");
    assert_eq!(paste_path(Path::new("/tmp/shot-1.png")), "/tmp/shot-1.png");
    if cfg!(unix) {
        assert_eq!(paste_path(Path::new("/tmp/my shot.png")), "'/tmp/my shot.png'");
        assert_eq!(paste_path(Path::new("/tmp/it's.png")), r"'/tmp/it'\''s.png'");
    }
}

/// A fake agent that turns on bracketed paste, then stores exactly `n` bytes of input.
#[cfg(unix)]
fn paste_stub(n: usize) -> String {
    format!(
        "#!/bin/sh\nstty raw -echo 2>/dev/null\nprintf '\\033[?2004hpaste-ready\\r\\n'\n\
         head -c {n} > pasted.bin\nprintf 'got-it\\r\\n'\nexec cat > /dev/null\n"
    )
}

#[test]
#[cfg(unix)]
fn a_long_paste_reaches_the_agent_whole() {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    let _g = lock();
    let dir = tmp("ai-paste");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let text: String = (0..2500).map(|i| format!("fn line_{i}() {{ {} }}\n", "y".repeat(24))).collect();
    assert!(text.len() > 64 * 1024, "{}", text.len());
    let want = paste_payload(&text, true);
    stub(&bin, "codex", &paste_stub(want.len()), true);
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.ai.search = Some(vec![bin.clone()]);
    app.ai_pick_new("codex");
    assert_eq!(app.ai.sessions.len(), 1);
    let screen = wait_screen(&mut app, "paste-ready");
    assert!(screen.contains("paste-ready"), "{screen:?}");
    assert!(app.ai.term().is_some_and(|t| t.grid.bracketed_paste));
    app.clip.set(&text);
    app.mods = winit::keyboard::ModifiersState::CONTROL;
    app.handle_key(&KeyIn { key: Key::Character("v".into()), text: None });
    app.mods = winit::keyboard::ModifiersState::empty();
    let done = wait_screen(&mut app, "got-it");
    assert!(done.contains("got-it"), "the agent read every byte: {done:?}");
    let got = std::fs::read(app.root.join("pasted.bin")).unwrap();
    assert_eq!(got.len(), want.len());
    assert!(got == want, "one bracketed paste, byte for byte");
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn ctrl_v_leaves_images_to_the_agent_and_pastes_files_as_paths() {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    let _g = lock();
    let dir = tmp("ai-clip");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "gemini", STUB_AGENT, true);
    let clipdir = dir.join("clip");
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.clip = crate::clipboard::Clipboard::with_fake_tools(&clipdir);
    app.ai.search = Some(vec![bin.clone()]);
    app.ai_pick_new("gemini");
    let first = wait_screen(&mut app, "stub-agent ready");
    assert!(first.contains("stub-agent ready"), "{first:?}");
    std::thread::sleep(Duration::from_millis(300));
    let ctrl_v = |app: &mut App| {
        app.mods = winit::keyboard::ModifiersState::CONTROL;
        app.handle_key(&KeyIn { key: Key::Character("v".into()), text: None });
        app.mods = winit::keyboard::ModifiersState::empty();
    };
    // A screenshot on the clipboard: the agent gets the raw Ctrl+V and reads the image itself.
    std::fs::write(clipdir.join("mime"), "image/png\n").unwrap();
    std::fs::write(clipdir.join("data"), "\u{89}PNG").unwrap();
    ctrl_v(&mut app);
    let s = wait_screen(&mut app, "[16]");
    assert!(s.contains("[16]"), "Ctrl+V forwarded: {s:?}");
    // Files copied in a file manager: their paths, quoted, as typed text.
    std::fs::write(clipdir.join("mime"), "text/uri-list\n").unwrap();
    std::fs::write(clipdir.join("data"), "file:///tmp/a%20b.png\r\n").unwrap();
    ctrl_v(&mut app);
    let want = "[27][2f][74][6d][70][2f][61][20][62][2e][70][6e][67][27][20]";
    let s = wait_screen(&mut app, want);
    assert!(s.contains(want), "'/tmp/a b.png' pasted: {s:?}");
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn a_session_can_run_in_another_folder() {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    let _g = lock();
    let dir = tmp("ai-cwd");
    let bin = dir.join("bin");
    let wt = dir.join("wt-fix");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    stub(&bin, "gemini", "#!/bin/sh\nprintf 'cwd=%s\\r\\n' \"$(pwd)\"\nexec cat\n", true);
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.ai.search = Some(vec![bin.clone()]);
    app.ai_session_in_worktree(&wt);
    assert_eq!(app.ai.sessions.len(), 1);
    assert_eq!(app.ai.sessions[0].cwd, wt);
    assert_eq!(app.ai.sessions[0].name, "Gemini CLI · wt-fix");
    assert!(app.ai.open && app.ai.focused);
    let s = wait_screen(&mut app, "wt-fix");
    assert!(s.contains("cwd=") && s.contains("wt-fix"), "the agent runs in the worktree: {s:?}");
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
