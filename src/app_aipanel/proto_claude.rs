//! Claude Code's structured mode: `claude -p --input-format stream-json --output-format
//! stream-json --verbose --include-partial-messages --permission-prompt-tool stdio`. One long
//! lived process; each user message is a JSON line on its stdin and every event comes back as a
//! JSON line on stdout. This module only turns those lines into [`Event`]s (and builds the lines
//! we write); the process itself is `backend`. The samples in the tests are real output of
//! Claude Code 2.1.

use super::chat::{Block, DeltaKind, Event, Perm, ToolCall, Usage};
use serde_json::{Value, json};

/// Command line of the persistent process (`resume`: the session to continue).
pub fn args(resume: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        "acceptEdits",
        "--permission-prompt-tool",
        "stdio",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(id) = resume {
        a.extend(["--resume".to_string(), id.to_string()]);
    }
    a
}

/// The line that sends a user message.
pub fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}).to_string()
}

/// The answer to a permission request (`can_use_tool`): run the tool (with its input as is) or not.
pub fn permission_reply(request_id: &str, allow: bool, input: &Value) -> String {
    let body = if allow {
        json!({"behavior": "allow", "updatedInput": input})
    } else {
        json!({"behavior": "deny", "message": "The user declined this action in Lumen."})
    };
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": body}})
        .to_string()
}

/// The request that stops the turn in progress (the process stays up).
pub fn interrupt_request(id: &str) -> String {
    json!({"type": "control_request", "request_id": id, "request": {"subtype": "interrupt"}}).to_string()
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

fn block_of(b: &Value) -> Option<Block> {
    let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match b.get("type").and_then(Value::as_str)? {
        "text" => Some(Block::Text(s("text"))),
        "thinking" => Some(Block::Thinking(s("thinking"))),
        "tool_use" => {
            Some(Block::Tool(ToolCall::new(&s("id"), &s("name"), b.get("input").cloned().unwrap_or(Value::Null))))
        }
        _ => None,
    }
}

fn u64_of(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// Events of one stdout line (none for the ones the panel does not show; an unparsable line is
/// not an event either).
pub fn parse_line(line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    // What a sub-agent (the Task tool) does inside stays out of the transcript.
    if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
        return Vec::new();
    }
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "system" if v["subtype"] == "init" => vec![Event::Init {
            session: v["session_id"].as_str().map(String::from),
            model: v["model"].as_str().map(String::from),
        }],
        "stream_event" => parse_stream(&v),
        "assistant" => {
            // A failed request (not logged in, ...) comes as an assistant text and a failed result:
            // the result says it once.
            if v.get("error").is_some_and(|e| !e.is_null()) {
                return Vec::new();
            }
            let msg = v["message"]["id"].as_str().unwrap_or("").to_string();
            v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(block_of)
                .map(|block| Event::Snapshot { msg: msg.clone(), block })
                .collect()
        }
        "user" => v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "tool_result")
            .map(|b| Event::ToolResult {
                id: b["tool_use_id"].as_str().unwrap_or("").to_string(),
                text: text_of(&b["content"]),
                is_error: b["is_error"].as_bool().unwrap_or(false),
            })
            .collect(),
        "result" => {
            let u = &v["usage"];
            let usage = Usage {
                input: u64_of(u, "input_tokens")
                    + u64_of(u, "cache_creation_input_tokens")
                    + u64_of(u, "cache_read_input_tokens"),
                output: u64_of(u, "output_tokens"),
                cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
                duration_ms: v["duration_ms"].as_u64().unwrap_or(0),
                turns: 0,
            };
            let error = v["is_error"].as_bool().unwrap_or(false).then(|| {
                let r = v["result"].as_str().unwrap_or("").trim();
                if r.is_empty() { v["subtype"].as_str().unwrap_or("error").replace('_', " ") } else { r.to_string() }
            });
            vec![Event::Done { usage, cumulative: true, session: v["session_id"].as_str().map(String::from), error }]
        }
        "control_request" if v["request"]["subtype"] == "can_use_tool" => {
            let r = &v["request"];
            vec![Event::Permission(Perm {
                request_id: v["request_id"].as_str().unwrap_or("").to_string(),
                tool_id: r["tool_use_id"].as_str().map(String::from),
                name: r["tool_name"].as_str().unwrap_or("").to_string(),
                input: r.get("input").cloned().unwrap_or(Value::Null),
            })]
        }
        _ => Vec::new(),
    }
}

fn parse_stream(v: &Value) -> Vec<Event> {
    let msg = v["api_message_id"].as_str().unwrap_or("").to_string();
    let e = &v["event"];
    let index = e["index"].as_u64().unwrap_or(0) as usize;
    match e["type"].as_str().unwrap_or("") {
        "content_block_start" => match block_of(&e["content_block"]) {
            Some(block) => vec![Event::Start { msg, index, block }],
            None => Vec::new(),
        },
        "content_block_delta" => {
            let d = &e["delta"];
            let (kind, text) = match d["type"].as_str().unwrap_or("") {
                "text_delta" => (DeltaKind::Text, d["text"].as_str()),
                "thinking_delta" => (DeltaKind::Thinking, d["thinking"].as_str()),
                "input_json_delta" => (DeltaKind::Json, d["partial_json"].as_str()),
                _ => return Vec::new(),
            };
            match text {
                Some(t) if !t.is_empty() => vec![Event::Delta { msg, index, kind, text: t.to_string() }],
                _ => Vec::new(),
            }
        }
        "content_block_stop" => vec![Event::Stop { msg, index }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::chat::{Chat, Phase, ToolStatus};
    use super::*;

    // Real lines of `claude -p --output-format stream-json --verbose --include-partial-messages`
    // (Claude Code 2.1.330) for "run `echo hi` with Bash, then say done": uuids and bulky
    // bookkeeping fields trimmed.
    const INIT: &str = r#"{"type":"system","subtype":"init","cwd":"/tmp/p","session_id":"9ca00868-0a39-424c-8ffc-ccb598e59914","tools":["Bash","Edit"],"model":"claude-sonnet-5-5","permissionMode":"acceptEdits","claude_code_version":"2.1.330"}"#;
    const HOOK: &str = r#"{"type":"system","subtype":"hook_started","hook_id":"6b6d","hook_name":"SessionStart:startup","session_id":"9ca0"}"#;
    const TOOL_START: &str = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01AFEh3uqpSaiLk1VPG19cPh","name":"Bash","input":{},"caller":{"type":"direct"}}},"session_id":"9ca0","parent_tool_use_id":null,"api_message_id":"msg_011ChGWyjCZuz1jefZXmyiwV"}"#;
    const JSON_D1: &str = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"echo hi"}},"session_id":"9ca0","parent_tool_use_id":null,"api_message_id":"msg_011ChGWyjCZuz1jefZXmyiwV"}"#;
    const JSON_D2: &str = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\", \"description\": \"Run echo hi\"}"}},"session_id":"9ca0","parent_tool_use_id":null,"api_message_id":"msg_011ChGWyjCZuz1jefZXmyiwV"}"#;
    const TOOL_STOP: &str = r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0},"session_id":"9ca0","parent_tool_use_id":null,"api_message_id":"msg_011ChGWyjCZuz1jefZXmyiwV"}"#;
    const ASSISTANT_TOOL: &str = r#"{"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_011ChGWyjCZuz1jefZXmyiwV","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01AFEh3uqpSaiLk1VPG19cPh","name":"Bash","input":{"command":"echo hi","description":"Run echo hi"},"caller":{"type":"direct"}}],"stop_reason":null},"parent_tool_use_id":null,"session_id":"9ca0"}"#;
    const TOOL_RESULT: &str = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01AFEh3uqpSaiLk1VPG19cPh","type":"tool_result","content":"hi","is_error":false}]},"parent_tool_use_id":null,"session_id":"9ca0","tool_use_result":{"stdout":"hi","stderr":"","interrupted":false}}"#;
    const TEXT_START: &str = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"session_id":"9ca0","parent_tool_use_id":null,"api_message_id":"msg_011ChGWyvdQzNLxHVWBAfXpe"}"#;
    const TEXT_DELTA: &str = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Pronto."}},"session_id":"9ca0","parent_tool_use_id":null,"api_message_id":"msg_011ChGWyvdQzNLxHVWBAfXpe"}"#;
    const ASSISTANT_TEXT: &str = r#"{"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_011ChGWyvdQzNLxHVWBAfXpe","type":"message","role":"assistant","content":[{"type":"text","text":"Pronto."}],"stop_reason":null},"parent_tool_use_id":null,"session_id":"9ca0"}"#;
    const MSG_DELTA: &str = r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}},"session_id":"9ca0","parent_tool_use_id":null,"api_message_id":"msg_011ChGWyvdQzNLxHVWBAfXpe"}"#;
    const RATE: &str =
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour"}}"#;
    const RESULT: &str = r#"{"duration_api_ms":1754,"stop_reason":"end_turn","session_id":"9ca00868-0a39-424c-8ffc-ccb598e59914","total_cost_usd":0.08944830000000001,"usage":{"input_tokens":4,"cache_creation_input_tokens":21020,"cache_read_input_tokens":45303,"output_tokens":83},"permission_denials":[],"is_error":false,"num_turns":2,"subtype":"success","result":"Pronto.","type":"result","duration_ms":4136}"#;
    const PERMISSION: &str = r#"{"type":"control_request","request_id":"258286d6-390d-4d25-8386-15a13c0fa5e8","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"touch /tmp/zz && rm /tmp/zz","description":"Create and remove a temporary file"},"description":"Create and remove a temporary file","decision_reason_type":"subcommandResults","tool_use_id":"toolu_01MqZuALX2o2X7VcGCUSCYC4"}}"#;
    const THINKING: &str = r#"{"type":"assistant","message":{"id":"msg_x","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"","signature":"CAQSiwcK"}]},"parent_tool_use_id":null,"session_id":"9ca0"}"#;
    const FAILED: &str = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0},"session_id":"s","duration_ms":10}"#;
    const NOT_LOGGED: &str = r#"{"type":"assistant","message":{"id":"m","role":"assistant","content":[{"type":"text","text":"Not logged in · Please run /login"}]},"error":"authentication_failed","parent_tool_use_id":null,"session_id":"s"}"#;
    const SUBAGENT: &str = r#"{"type":"assistant","message":{"id":"m","role":"assistant","content":[{"type":"text","text":"inner"}]},"parent_tool_use_id":"toolu_parent","session_id":"s"}"#;

    #[test]
    fn single_lines_become_events() {
        assert_eq!(
            parse_line(INIT),
            vec![Event::Init {
                session: Some("9ca00868-0a39-424c-8ffc-ccb598e59914".into()),
                model: Some("claude-sonnet-5-5".into())
            }]
        );
        assert!(parse_line(HOOK).is_empty() && parse_line(RATE).is_empty() && parse_line(MSG_DELTA).is_empty());
        assert!(parse_line("not json").is_empty() && parse_line("").is_empty());
        assert!(parse_line(SUBAGENT).is_empty(), "what a sub-agent does is not shown");
        assert!(parse_line(NOT_LOGGED).is_empty(), "the failed result says it once");
        let [Event::Done { usage, cumulative, error, .. }] = &parse_line(RESULT)[..] else { panic!("result") };
        assert!(*cumulative && error.is_none());
        assert_eq!((usage.input, usage.output, usage.duration_ms), (4 + 21020 + 45303, 83, 4136));
        assert!((usage.cost_usd - 0.0894483).abs() < 1e-9);
        let [Event::Done { error, .. }] = &parse_line(FAILED)[..] else { panic!("failed") };
        assert_eq!(error.as_deref(), Some("Not logged in · Please run /login"));
        let [Event::Permission(p)] = &parse_line(PERMISSION)[..] else { panic!("permission") };
        assert_eq!((p.name.as_str(), p.tool_id.as_deref()), ("Bash", Some("toolu_01MqZuALX2o2X7VcGCUSCYC4")));
        assert_eq!(p.request_id, "258286d6-390d-4d25-8386-15a13c0fa5e8");
        assert_eq!(p.input["command"], "touch /tmp/zz && rm /tmp/zz");
        let [Event::Snapshot { block, .. }] = &parse_line(THINKING)[..] else { panic!("thinking") };
        assert_eq!(block, &Block::Thinking(String::new()));
    }

    #[test]
    fn a_whole_session_folds_into_the_transcript() {
        let mut c = Chat::new();
        c.user_says("run echo hi");
        for line in [
            INIT,
            HOOK,
            TOOL_START,
            JSON_D1,
            JSON_D2,
            ASSISTANT_TOOL,
            TOOL_STOP,
            RATE,
            TOOL_RESULT,
            TEXT_START,
            TEXT_DELTA,
            ASSISTANT_TEXT,
            MSG_DELTA,
        ] {
            for e in parse_line(line) {
                c.apply(e);
            }
        }
        assert_eq!(c.phase, Phase::Working, "the turn is not over until the result");
        assert_eq!(c.model.as_deref(), Some("claude-sonnet-5-5"));
        let blocks = &c.messages[1].blocks;
        assert_eq!(blocks.len(), 2, "one tool card and one text: {blocks:?}");
        let Block::Tool(t) = &blocks[0] else { panic!("tool") };
        assert_eq!((t.name.as_str(), t.status, t.result.as_deref()), ("Bash", ToolStatus::Ok, Some("hi")));
        assert_eq!(t.input["description"], "Run echo hi");
        assert_eq!(blocks[1], Block::Text("Pronto.".into()));
        for e in parse_line(RESULT) {
            c.apply(e);
        }
        assert_eq!(c.phase, Phase::Idle);
        assert_eq!(c.session_id.as_deref(), Some("9ca00868-0a39-424c-8ffc-ccb598e59914"));
        assert_eq!(c.usage().output, 83);
        assert_eq!(c.usage().turns, 1);
    }

    #[test]
    fn what_we_write_is_what_claude_expects() {
        let u: Value = serde_json::from_str(&user_message("oi \"x\"\nlinha")).unwrap();
        assert_eq!(u["type"], "user");
        assert_eq!(u["message"]["content"][0]["text"], "oi \"x\"\nlinha");
        assert!(!user_message("a\nb").contains('\n'), "one message, one line");
        let input = json!({"command": "ls"});
        let ok: Value = serde_json::from_str(&permission_reply("r1", true, &input)).unwrap();
        assert_eq!(ok["type"], "control_response");
        assert_eq!(ok["response"]["request_id"], "r1");
        assert_eq!(ok["response"]["response"]["behavior"], "allow");
        assert_eq!(ok["response"]["response"]["updatedInput"], input);
        let no: Value = serde_json::from_str(&permission_reply("r1", false, &input)).unwrap();
        assert_eq!(no["response"]["response"]["behavior"], "deny");
        assert!(no["response"]["response"]["message"].is_string());
        let i: Value = serde_json::from_str(&interrupt_request("q")).unwrap();
        assert_eq!(
            (i["type"].as_str(), i["request"]["subtype"].as_str()),
            (Some("control_request"), Some("interrupt"))
        );
        let a = args(Some("abc"));
        assert!(
            a.contains(&"--include-partial-messages".to_string()) && a.ends_with(&["--resume".into(), "abc".into()])
        );
        assert!(!args(None).contains(&"--resume".to_string()));
    }
}
