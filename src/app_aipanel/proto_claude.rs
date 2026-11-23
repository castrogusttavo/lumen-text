//! Claude Code's structured mode: `claude -p --input-format stream-json --output-format
//! stream-json --verbose --include-partial-messages --permission-prompt-tool stdio`. One long
//! lived process; each user message is a JSON line on its stdin and every event comes back as a
//! JSON line on stdout. This module only turns those lines into [`Event`]s (and builds the lines
//! we write); the process itself is `backend`. The samples in the tests are real output of
//! Claude Code 2.1.

#![allow(dead_code)]

use super::chat::{Block, DeltaKind, Event, Perm, PermMode, ToolCall, Usage};
use serde_json::{Value, json};

/// Command line of the persistent process (`resume`: the session to continue; `mode`: the
/// permission mode to start in; `model`: the model chosen, if any).
/// `--allow-dangerously-skip-permissions` only makes the bypass mode available to
/// `set_permission_mode`: it does not turn it on.
pub fn args(resume: Option<&str>, mode: PermMode, model: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        mode.id(),
        "--allow-dangerously-skip-permissions",
        "--permission-prompt-tool",
        "stdio",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        a.extend(["--model".to_string(), m.to_string()]);
    }
    if let Some(e) = effort.filter(|e| !e.is_empty()) {
        a.extend(["--effort".to_string(), e.to_string()]);
    }
    if let Some(id) = resume {
        a.extend(["--resume".to_string(), id.to_string()]);
    }
    a
}

/// Standard base64 (RFC 4648), for the images a message carries.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// A user message with images (media type, bytes) before its text.
pub fn user_message_with(text: &str, images: &[(String, Vec<u8>)]) -> String {
    let mut content: Vec<Value> = images
        .iter()
        .map(|(media, bytes)| {
            json!({"type": "image", "source": {"type": "base64", "media_type": media, "data": base64(bytes)}})
        })
        .collect();
    content.push(json!({"type": "text", "text": text}));
    json!({"type": "user", "message": {"role": "user", "content": content}}).to_string()
}

/// Change the permission mode of the running process.
pub fn set_mode_request(id: &str, mode: PermMode) -> String {
    json!({"type": "control_request", "request_id": id, "request": {"subtype": "set_permission_mode", "mode": mode.id()}})
        .to_string()
}

/// Change the model of the running process (an alias such as `opus`, or a full id).
pub fn set_model_request(id: &str, model: &str) -> String {
    json!({"type": "control_request", "request_id": id, "request": {"subtype": "set_model", "model": model}})
        .to_string()
}

/// The line that sends a user message.
pub fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}}).to_string()
}

/// The answer to a permission request (`can_use_tool`): run the tool (with its input as is) or not.
#[cfg(test)]
pub fn permission_reply(request_id: &str, allow: bool, input: &Value) -> String {
    permission_reply_for(request_id, allow, input, "")
}

/// [`permission_reply`] knowing the tool: declining a plan says so, so the agent keeps planning.
pub fn permission_reply_for(request_id: &str, allow: bool, input: &Value, tool: &str) -> String {
    let denied = if tool == "ExitPlanMode" {
        "The user did not approve the plan in Lumen. Keep planning: ask what to change, or revise it."
    } else {
        "The user declined this action in Lumen."
    };
    let body = if allow {
        json!({"behavior": "allow", "updatedInput": input})
    } else {
        json!({"behavior": "deny", "message": denied})
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
        "system" if v["subtype"] == "init" => {
            let mut evs = vec![Event::Init {
                session: v["session_id"].as_str().map(String::from),
                model: v["model"].as_str().map(String::from),
            }];
            if v.get("slash_commands").is_some() {
                let names = |k: &str| -> Vec<String> {
                    v[k].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
                };
                // The memory folders come as an object (`{"auto": "/path"}`).
                let memory = v["memory_paths"]
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.values())
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                evs.push(Event::Meta {
                    commands: names("slash_commands"),
                    terminal_only: names("terminal_slash_commands"),
                    agents: names("agents"),
                    memory,
                    mode: v["permissionMode"].as_str().map(String::from),
                });
            }
            evs
        }
        "system" if v["subtype"] == "status" => {
            if let Some(m) = v["permissionMode"].as_str() {
                return vec![Event::Mode(m.to_string())];
            }
            match (v["status"].as_str(), v["compact_result"].as_str()) {
                (Some("compacting"), _) => vec![Event::Compacting(true)],
                (_, Some("failed")) => {
                    let why = v["compact_error"].as_str().unwrap_or("it failed");
                    vec![Event::Compacting(false), Event::Note(format!("Could not compact: {why}"))]
                }
                (_, Some(_)) => vec![Event::Compacting(false)],
                _ => Vec::new(),
            }
        }
        "system" if v["subtype"] == "compact_boundary" => {
            let m = &v["compact_metadata"];
            let (pre, post) = (u64_of(m, "pre_tokens"), u64_of(m, "post_tokens"));
            let text = if pre > 0 {
                format!(
                    "Conversation compacted: {} → {} tokens of context",
                    super::chat::format_tokens(pre),
                    super::chat::format_tokens(post)
                )
            } else {
                "Conversation compacted".to_string()
            };
            vec![Event::Note(text)]
        }
        "control_response" => {
            let r = &v["response"]["response"];
            if let Some(m) = r["mode"].as_str() {
                vec![Event::Mode(m.to_string())]
            } else if let Some(m) = r["model"].as_str() {
                vec![Event::Init { session: None, model: Some(m.to_string()) }]
            } else {
                Vec::new()
            }
        }
        "stream_event" => parse_stream(&v),
        "assistant" => {
            // A failed request (not logged in, ...) comes as an assistant text and a failed result:
            // the result says it once.
            if v.get("error").is_some_and(|e| !e.is_null()) {
                return Vec::new();
            }
            let msg = v["message"]["id"].as_str().unwrap_or("").to_string();
            let mut evs: Vec<Event> = v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(block_of)
                .map(|block| Event::Snapshot { msg: msg.clone(), block })
                .collect();
            // The size of this request is what sits in the context window now.
            let u = &v["message"]["usage"];
            let used = u64_of(u, "input_tokens")
                + u64_of(u, "cache_creation_input_tokens")
                + u64_of(u, "cache_read_input_tokens");
            if used > 0 && v["message"]["model"] != "<synthetic>" {
                evs.push(Event::Context { used: Some(used), window: None });
            }
            evs
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
            let mut evs = Vec::new();
            let window =
                v["modelUsage"].as_object().and_then(|m| m.values().filter_map(|x| x["contextWindow"].as_u64()).max());
            if window.is_some() {
                evs.push(Event::Context { used: None, window });
            }
            evs.push(Event::Done {
                usage,
                cumulative: true,
                session: v["session_id"].as_str().map(String::from),
                error,
            });
            evs
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
        let a = args(Some("abc"), PermMode::Plan, Some("opus"), None);
        assert!(
            a.contains(&"--include-partial-messages".to_string()) && a.ends_with(&["--resume".into(), "abc".into()])
        );
        let at = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].as_str());
        assert_eq!((at("--permission-mode"), at("--model")), (Some("plan"), Some("opus")));
        assert!(!args(None, PermMode::AcceptEdits, None, None).iter().any(|x| x == "--resume" || x == "--model"));
    }

    #[test]
    fn modes_models_and_images_are_written_the_way_claude_reads_them() {
        let m: Value = serde_json::from_str(&set_mode_request("r1", PermMode::Bypass)).unwrap();
        assert_eq!(m["request"]["subtype"], "set_permission_mode");
        assert_eq!(m["request"]["mode"], "bypassPermissions");
        let m: Value = serde_json::from_str(&set_model_request("r2", "haiku")).unwrap();
        assert_eq!(
            (m["request"]["subtype"].as_str(), m["request"]["model"].as_str()),
            (Some("set_model"), Some("haiku"))
        );
        let u: Value =
            serde_json::from_str(&user_message_with("what is this?", &[("image/png".into(), vec![1, 2, 3])])).unwrap();
        assert_eq!(u["message"]["content"][0]["type"], "image");
        assert_eq!(u["message"]["content"][0]["source"]["data"], "AQID");
        assert_eq!(u["message"]["content"][0]["source"]["media_type"], "image/png");
        assert_eq!(u["message"]["content"][1]["text"], "what is this?");
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }

    // Real lines of Claude Code 2.1.330 (trimmed): the init with its commands, a mode change and a
    // model switch answered, and what `/compact` prints.
    const INIT_FULL: &str = r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-sonnet-5-5","permissionMode":"acceptEdits","slash_commands":["clear","compact","doctor","tray-api:setup"],"terminal_slash_commands":["doctor","color"],"agents":["Explore","general-purpose"],"memory_paths":{"auto":"/h/.claude/projects/p/memory/"}}"#;
    const MODE_STATUS: &str =
        r#"{"type":"system","subtype":"status","status":null,"permissionMode":"plan","session_id":"s1"}"#;
    const SET_MODEL_OK: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"r2","response":{"model":"claude-haiku-5-5"}}}"#;
    const SET_MODE_OK: &str =
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"r1","response":{"mode":"plan"}}}"#;
    const COMPACTING: &str = r#"{"type":"system","subtype":"status","status":"compacting","session_id":"s1"}"#;
    const COMPACT_FAILED: &str = r#"{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"Not enough messages to compact.","session_id":"s1"}"#;
    const COMPACT_OK: &str =
        r#"{"type":"system","subtype":"status","status":null,"compact_result":"success","session_id":"s1"}"#;
    const BOUNDARY: &str = r#"{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":30961,"post_tokens":10782},"session_id":"s1"}"#;
    const USAGE_ASSIST: &str = r#"{"type":"assistant","message":{"id":"m","model":"claude-sonnet-5-5","role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":1000,"cache_read_input_tokens":9000,"output_tokens":5}},"parent_tool_use_id":null}"#;
    const WINDOW_RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":2},"modelUsage":{"claude-sonnet-5-5":{"contextWindow":1000000}},"session_id":"s1","duration_ms":5}"#;

    #[test]
    fn init_carries_the_commands_and_modes_and_compaction_is_reported() {
        let evs = parse_line(INIT_FULL);
        assert_eq!(evs.len(), 2);
        let Event::Meta { commands, terminal_only, agents, memory, mode } = &evs[1] else { panic!("meta") };
        assert_eq!(commands, &["clear", "compact", "doctor", "tray-api:setup"]);
        assert_eq!(terminal_only, &["doctor", "color"]);
        assert_eq!(agents, &["Explore", "general-purpose"]);
        assert_eq!(memory, &["/h/.claude/projects/p/memory/"]);
        assert_eq!(mode.as_deref(), Some("acceptEdits"));
        assert_eq!(parse_line(MODE_STATUS), vec![Event::Mode("plan".into())]);
        assert_eq!(parse_line(SET_MODE_OK), vec![Event::Mode("plan".into())]);
        assert_eq!(
            parse_line(SET_MODEL_OK),
            vec![Event::Init { session: None, model: Some("claude-haiku-5-5".into()) }]
        );
        assert_eq!(parse_line(COMPACTING), vec![Event::Compacting(true)]);
        assert_eq!(parse_line(COMPACT_OK), vec![Event::Compacting(false)]);
        assert_eq!(
            parse_line(COMPACT_FAILED),
            vec![Event::Compacting(false), Event::Note("Could not compact: Not enough messages to compact.".into())]
        );
        assert_eq!(
            parse_line(BOUNDARY),
            vec![Event::Note("Conversation compacted: 31.0k → 10.8k tokens of context".into())]
        );
    }

    #[test]
    fn the_context_in_use_comes_from_each_request_and_the_window_from_the_result() {
        let evs = parse_line(USAGE_ASSIST);
        assert_eq!(evs.last(), Some(&Event::Context { used: Some(10_002), window: None }));
        let evs = parse_line(WINDOW_RESULT);
        assert_eq!(evs[0], Event::Context { used: None, window: Some(1_000_000) });
        assert!(matches!(evs[1], Event::Done { .. }));
        let synthetic = USAGE_ASSIST.replace("claude-sonnet-5-5", "<synthetic>");
        assert!(!parse_line(&synthetic).iter().any(|e| matches!(e, Event::Context { .. })));
    }
}
