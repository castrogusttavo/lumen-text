//! Codex CLI's structured mode: `codex exec --json`, one process per turn that prints one JSON
//! event per line (`thread.started`, `item.started` / `item.completed` for messages, reasoning,
//! commands, file changes, MCP calls and searches, `turn.completed` with the token usage). The
//! prompt goes on stdin; a later turn continues the thread with `exec resume <thread_id>`. This
//! module turns the lines into [`Event`]s (tools take Claude's names, so the panel shows them the
//! same way); the process itself is `backend`.

use super::chat::{Block, Event, ToolCall, Usage};
use serde_json::{Value, json};

/// Command line of one turn (`thread`: the thread of an earlier turn to continue).
pub fn args(thread: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> =
        ["exec", "--json", "--skip-git-repo-check", "-s", "workspace-write"].iter().map(|s| s.to_string()).collect();
    if let Some(t) = thread {
        a.extend(["resume".to_string(), t.to_string()]);
    }
    a
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn tool(id: &str, name: &str, input: Value) -> Event {
    Event::Snapshot { msg: id.to_string(), block: Block::Tool(ToolCall::new(id, name, input)) }
}

fn result(id: &str, text: String, is_error: bool) -> Event {
    Event::ToolResult { id: id.to_string(), text, is_error }
}

/// Events of one stdout line.
pub fn parse_line(line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    match s(&v, "type") {
        "thread.started" => vec![Event::Init { session: Some(s(&v, "thread_id").to_string()), model: None }],
        "turn.completed" => {
            let u = &v["usage"];
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            let usage = Usage { input: n("input_tokens"), output: n("output_tokens"), ..Default::default() };
            vec![Event::Done { usage, cumulative: false, session: None, error: None }]
        }
        "turn.failed" => {
            let m = v["error"]["message"].as_str().unwrap_or("The turn failed").to_string();
            vec![Event::Done { usage: Usage::default(), cumulative: false, session: None, error: Some(m) }]
        }
        kind @ ("item.started" | "item.updated" | "item.completed") => item(&v["item"], kind == "item.completed"),
        _ => Vec::new(),
    }
}

fn item(it: &Value, done: bool) -> Vec<Event> {
    let id = s(it, "id");
    let failed = s(it, "status") == "failed";
    match s(it, "type") {
        "agent_message" if done => {
            vec![Event::Snapshot { msg: id.to_string(), block: Block::Text(s(it, "text").to_string()) }]
        }
        "reasoning" if done => {
            vec![Event::Snapshot { msg: id.to_string(), block: Block::Thinking(s(it, "text").to_string()) }]
        }
        "command_execution" => {
            let mut v = vec![tool(id, "Bash", json!({"command": s(it, "command")}))];
            if done {
                let code = it["exit_code"].as_i64();
                v.push(result(id, s(it, "aggregated_output").to_string(), failed || code.is_some_and(|c| c != 0)));
            }
            v
        }
        "file_change" => {
            let changes: Vec<&Value> = it["changes"].as_array().into_iter().flatten().collect();
            let first = changes.first().map_or("", |c| s(c, "path"));
            let mut v = vec![tool(id, "Edit", json!({"file_path": first, "changes": it["changes"]}))];
            if done {
                let text =
                    changes.iter().map(|c| format!("{} {}", s(c, "kind"), s(c, "path"))).collect::<Vec<_>>().join("\n");
                v.push(result(id, text, failed));
            }
            v
        }
        "mcp_tool_call" => {
            let name = format!("{}.{}", s(it, "server"), s(it, "tool"));
            let mut v = vec![tool(id, &name, it.get("arguments").cloned().unwrap_or(Value::Null))];
            if done {
                let err = it["error"]["message"].as_str();
                let text = match err {
                    Some(e) => e.to_string(),
                    None => it["result"]["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                v.push(result(id, text, failed || err.is_some()));
            }
            v
        }
        "web_search" => {
            let mut v = vec![tool(id, "WebSearch", json!({"query": s(it, "query")}))];
            if done {
                v.push(result(id, String::new(), false));
            }
            v
        }
        "todo_list" => {
            let todos: Vec<Value> = it["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| {
                    let st = if i["completed"].as_bool().unwrap_or(false) { "completed" } else { "pending" };
                    json!({"content": s(i, "text"), "status": st})
                })
                .collect();
            let mut v = vec![tool(id, "TodoWrite", json!({"todos": todos}))];
            if done {
                v.push(result(id, String::new(), false));
            }
            v
        }
        "error" if done => {
            vec![Event::Snapshot { msg: id.to_string(), block: Block::Error(s(it, "message").to_string()) }]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::chat::{Chat, Phase, ToolStatus};
    use super::*;

    // Lines in the shape `codex exec --json` documents (thread / turn / item events).
    const LINES: &[&str] = &[
        r#"{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}"#,
        r#"{"type":"turn.started"}"#,
        r#"{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"**Listing files**"}}"#,
        r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
        r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsdk\n","exit_code":0,"status":"completed"}}"#,
        r#"{"type":"item.completed","item":{"id":"item_2","type":"file_change","changes":[{"path":"src/a.rs","kind":"update"}],"status":"completed"}}"#,
        r#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Repo has **docs** and sdk."}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122}}"#,
    ];

    #[test]
    fn a_turn_folds_into_the_transcript() {
        let mut c = Chat::new();
        c.user_says("what is here?");
        for l in LINES {
            for e in parse_line(l) {
                c.apply(e);
            }
        }
        assert_eq!(c.phase, Phase::Idle);
        assert_eq!(c.session_id.as_deref(), Some("0199a213-81c0-7800-8aa1-bbab2a035a53"));
        let b = &c.messages[1].blocks;
        assert_eq!(b.len(), 4, "{b:?}");
        assert_eq!(b[0], Block::Thinking("**Listing files**".into()));
        let Block::Tool(t) = &b[1] else { panic!("command") };
        assert_eq!((t.name.as_str(), t.status), ("Bash", ToolStatus::Ok));
        assert_eq!(t.input["command"], "bash -lc ls");
        assert_eq!(t.result.as_deref(), Some("docs\nsdk\n"));
        let Block::Tool(f) = &b[2] else { panic!("file change") };
        assert_eq!((f.name.as_str(), f.input["file_path"].as_str()), ("Edit", Some("src/a.rs")));
        assert_eq!(b[3], Block::Text("Repo has **docs** and sdk.".into()));
        assert_eq!((c.usage().input, c.usage().output), (24763, 122));
    }

    #[test]
    fn failures_are_errors() {
        let failed = r#"{"type":"item.completed","item":{"id":"i","type":"command_execution","command":"false","aggregated_output":"","exit_code":1,"status":"failed"}}"#;
        let evs = parse_line(failed);
        assert!(matches!(&evs[1], Event::ToolResult { is_error: true, .. }));
        let tf = r#"{"type":"turn.failed","error":{"message":"stream disconnected"}}"#;
        assert!(matches!(&parse_line(tf)[..], [Event::Done { error: Some(e), .. }] if e == "stream disconnected"));
        assert!(parse_line(r#"{"type":"error","message":"Reconnecting... 1/5"}"#).is_empty());
        assert!(parse_line("garbage").is_empty());
    }

    #[test]
    fn mcp_searches_and_todos() {
        let mcp = r#"{"type":"item.completed","item":{"id":"m","type":"mcp_tool_call","server":"fs","tool":"read","arguments":{"p":"x"},"result":{"content":[{"type":"text","text":"ok"}]},"status":"completed"}}"#;
        let evs = parse_line(mcp);
        assert!(matches!(&evs[0], Event::Snapshot { block: Block::Tool(t), .. } if t.name == "fs.read"));
        assert!(matches!(&evs[1], Event::ToolResult { text, is_error: false, .. } if text == "ok"));
        let todo = r#"{"type":"item.updated","item":{"id":"t","type":"todo_list","items":[{"text":"a","completed":true},{"text":"b","completed":false}]}}"#;
        let evs = parse_line(todo);
        assert_eq!(evs.len(), 1, "updates keep the card current without a result");
        let web = r#"{"type":"item.started","item":{"id":"w","type":"web_search","query":"rust"}}"#;
        assert!(matches!(&parse_line(web)[0], Event::Snapshot { block: Block::Tool(t), .. } if t.name == "WebSearch"));
    }

    #[test]
    fn later_turns_resume_the_thread() {
        assert!(args(None).iter().all(|a| a != "resume"));
        let a = args(Some("abc"));
        assert_eq!(&a[a.len() - 2..], &["resume".to_string(), "abc".to_string()]);
        assert_eq!(a[0], "exec");
    }
}
