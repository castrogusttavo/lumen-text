//! The native chat end to end on the App, against stub agents that speak Claude Code's
//! stream-json: a turn with a tool call and Markdown, and a permission that is answered.

use super::{
    chat::{Block, Phase, ToolStatus},
    chat_view::ChatHit,
    tests::stub,
    *,
};
use crate::app_unsaved::testkit::{lock, test_app, tmp};

const TURN: &str = r#"#!/bin/sh
while read -r line; do
  printf '%s\n' '{"type":"system","subtype":"init","session_id":"S1","model":"claude-test"}'
  printf '%s\n' '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"a.rs","old_string":"x","new_string":"y"}}]}}'
  printf '%s\n' '{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"ok","is_error":false}]}}'
  printf '%s\n' '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Done with **bold** and `code`."}]}}'
  printf '%s\n' '{"type":"result","is_error":false,"result":"x","total_cost_usd":0.02,"usage":{"output_tokens":9},"session_id":"S1","duration_ms":3}'
done
"#;

const ASKS: &str = r#"#!/bin/sh
read -r line
printf '%s\n' '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t9","name":"Bash","input":{"command":"rm -rf build"}}]}}'
printf '%s\n' '{"type":"control_request","request_id":"R1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf build"},"tool_use_id":"t9"}}'
read -r reply
case "$reply" in
  *'"behavior":"allow"'*) printf '%s\n' '{"type":"user","message":{"content":[{"tool_use_id":"t9","type":"tool_result","content":"removed","is_error":false}]}}' ;;
  *) printf '%s\n' '{"type":"user","message":{"content":[{"tool_use_id":"t9","type":"tool_result","content":"denied","is_error":true}]}}' ;;
esac
printf '%s\n' '{"type":"result","is_error":false,"result":"x","total_cost_usd":0,"usage":{"output_tokens":1},"session_id":"S2","duration_ms":1}'
sleep 5
"#;

fn frame(app: &mut App) {
    let (w, h) = (1200, 800);
    let mut px = vec![0u32; (w * h) as usize];
    let mut cv = crate::ui::Canvas { px: &mut px, w, h };
    app.render_into(&mut cv, 0.016);
}

fn pump_until(app: &mut App, mut done: impl FnMut(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end && !done(app) {
        app.pump_ai();
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn phase(app: &App) -> Phase {
    app.ai.cur().and_then(|s| s.chat.as_ref()).map_or(Phase::Idle, |c| c.chat.phase)
}

fn chat_hits(app: &App) -> Vec<(Rect, ChatHit)> {
    app.ai.cur().and_then(|s| s.chat.as_ref()).map(|c| c.view.hits.clone()).unwrap_or_default()
}

fn center(r: Rect) -> (f64, f64) {
    ((r.x + r.w / 2) as f64, (r.y + r.h / 2) as f64)
}

fn chat_app(name: &str, script: &str) -> (App, std::path::PathBuf) {
    let dir = tmp(name);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "claude", script, true);
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.ai.structured = true;
    app.ai.search = Some(vec![bin]);
    app.run_bound("AiPanel");
    (app, dir)
}

#[test]
fn a_turn_becomes_cards_and_markdown_with_the_right_pointers() {
    let _g = lock();
    let (mut app, dir) = chat_app("ai-chat-turn", TURN);
    assert!(app.ai.open);
    let s = app.ai.cur().expect("a session");
    assert!(s.chat.is_some() && s.term.is_none(), "Claude Code runs as a native chat");
    assert!(app.ai.composer_focus);
    frame(&mut app);
    app.ai.composer.set_text("change x to y");
    app.ai_send();
    assert_eq!(app.ai.composer.text(), "");
    assert_eq!(phase(&app), Phase::Working);
    pump_until(&mut app, |a| phase(a) == Phase::Idle);
    assert_eq!(phase(&app), Phase::Idle);
    let c = &app.ai.cur().unwrap().chat.as_ref().unwrap().chat;
    assert_eq!(c.messages.len(), 2);
    let Block::Tool(t) = &c.messages[1].blocks[0] else { panic!("tool card first: {:?}", c.messages[1]) };
    assert_eq!((t.name.as_str(), t.status, t.result.as_deref()), ("Edit", ToolStatus::Ok, Some("ok")));
    assert_eq!(c.usage().output, 9);
    assert_eq!(app.ai.cur().unwrap().status(), Status::Done);
    frame(&mut app);
    let hits = chat_hits(&app);
    let toggle = hits.iter().find(|(_, h)| *h == ChatHit::Toggle("t:t1".into())).expect("the tool card header").0;
    assert!(hits.iter().any(|(_, h)| matches!(h, ChatHit::Md(_))), "the text block is rendered Markdown");
    // The pointer: a hand on the card, the arrow on nothing, an I-beam over the composer.
    let (x, y) = center(toggle);
    assert_eq!(app.aipanel_cursor(x, y), Some(winit::window::CursorIcon::Pointer));
    let b = app.ai.composer_box;
    assert_eq!(app.aipanel_cursor((b.x + 30) as f64, (b.y + 12) as f64), Some(winit::window::CursorIcon::Text));
    // Edits open by themselves; a click closes the card, the next frame has no diff rows.
    let before = app.ai.cur().unwrap().chat.as_ref().unwrap().view.content_h;
    assert!(app.aipanel_mouse(x, y, 0));
    frame(&mut app);
    let after = app.ai.cur().unwrap().chat.as_ref().unwrap().view.content_h;
    assert!(after < before, "collapsed: {after} < {before}");
    // A new conversation starts over.
    app.ai_restart(None);
    assert!(app.ai.cur().unwrap().chat.as_ref().unwrap().chat.messages.is_empty());
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_permission_is_a_card_and_the_answer_reaches_the_agent() {
    let _g = lock();
    let (mut app, dir) = chat_app("ai-chat-perm", ASKS);
    app.ai.composer.set_text("clean up");
    app.ai_send();
    pump_until(&mut app, |a| phase(a) == Phase::Waiting);
    assert_eq!(phase(&app), Phase::Waiting);
    assert_eq!(app.ai.cur().unwrap().status(), Status::Waiting, "the tab asks for the user");
    frame(&mut app);
    let hits = chat_hits(&app);
    let allow = hits.iter().find(|(_, h)| matches!(h, ChatHit::Perm(id, true) if id == "R1")).expect("Allow").0;
    assert!(hits.iter().any(|(_, h)| matches!(h, ChatHit::Perm(_, false))), "and Deny");
    let (x, y) = center(allow);
    assert_eq!(app.aipanel_cursor(x, y), Some(winit::window::CursorIcon::Pointer));
    assert!(app.aipanel_mouse(x, y, 0));
    pump_until(&mut app, |a| phase(a) == Phase::Idle);
    let c = &app.ai.cur().unwrap().chat.as_ref().unwrap().chat;
    let Block::Tool(t) = &c.messages[1].blocks[0] else { panic!("tool") };
    assert_eq!((t.status, t.result.as_deref()), (ToolStatus::Ok, Some("removed")), "the stub saw `allow`");
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stop_interrupts_a_turn_then_ends_the_process() {
    let _g = lock();
    let (mut app, dir) = chat_app("ai-chat-stop", "#!/bin/sh\nread -r l\nsleep 30\n");
    app.ai.composer.set_text("slow");
    app.ai_send();
    assert_eq!(phase(&app), Phase::Working);
    app.ai_stop(); // asks politely
    app.ai_stop(); // the second press ends the process
    assert_eq!(phase(&app), Phase::Idle);
    let c = &app.ai.cur().unwrap().chat.as_ref().unwrap().chat;
    assert!(matches!(c.messages.last().and_then(|m| m.blocks.last()), Some(Block::Error(e)) if e == "Interrupted"));
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
