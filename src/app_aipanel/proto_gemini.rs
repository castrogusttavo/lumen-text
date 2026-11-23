//! Gemini CLI's structured mode: `gemini -p <prompt> --output-format stream-json`, one process
//! per turn printing one JSON event per line (`init`, `message` chunks, `tool_use`, `tool_result`,
//! `error`, `result` with the stats). Its tools are mapped onto Claude's names and inputs so the
//! panel draws them the same way (a `replace` is an Edit with its diff). The parser keeps a little
//! state: a message chunk after a tool call starts a new text block.

#![allow(dead_code)]

use super::chat::{Block, DeltaKind, Event, ToolCall, Usage};
use serde_json::{Value, json};

/// Command line of one turn (`session`: an earlier turn's session to continue).
pub fn args(prompt: &str, session: Option<&str>, model: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = ["-p", prompt, "--output-format", "stream-json", "--approval-mode", "auto_edit"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        a.extend(["-m".to_string(), m.to_string()]);
    }
    if let Some(s) = session {
        a.extend(["--resume".to_string(), s.to_string()]);
    }
    a
}

/// A Gemini tool call as (Claude-style name, input).
pub fn normalize_tool(name: &str, p: &Value) -> (String, Value) {
    let pick = |keys: &[&str]| -> Value {
        keys.iter().find_map(|k| p.get(*k).filter(|v| !v.is_null()).cloned()).unwrap_or(Value::Null)
    };
    match name {
        "read_file" | "read_many_files" => {
            ("Read".into(), json!({"file_path": pick(&["absolute_path", "file_path", "path"])}))
        }
        "write_file" => {
            ("Write".into(), json!({"file_path": pick(&["file_path", "path"]), "content": pick(&["content"])}))
        }
        "replace" | "edit" => (
            "Edit".into(),
            json!({"file_path": pick(&["file_path", "path"]), "old_string": pick(&["old_string"]), "new_string": pick(&["new_string"])}),
        ),
        "run_shell_command" | "shell" => {
            ("Bash".into(), json!({"command": pick(&["command"]), "description": pick(&["description"])}))
        }
        "glob" => ("Glob".into(), json!({"pattern": pick(&["pattern"])})),
        "search_file_content" | "grep_search" | "grep" => {
            ("Grep".into(), json!({"pattern": pick(&["pattern"]), "path": pick(&["path", "dir_path"])}))
        }
        "list_directory" | "ls" => ("LS".into(), json!({"path": pick(&["path", "dir_path"])})),
        "web_fetch" => ("WebFetch".into(), json!({"url": pick(&["url", "prompt"])})),
        "google_web_search" | "web_search" => ("WebSearch".into(), json!({"query": pick(&["query"])})),
        other => (other.to_string(), p.clone()),
    }
}

#[derive(Default)]
pub struct Parser {
    /// Number of the text block being streamed.
    block: usize,
    in_text: bool,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Events of one stdout line.
    pub fn parse_line(&mut self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
        match s("type") {
            "init" => vec![Event::Init {
                session: v["session_id"].as_str().map(String::from),
                model: v["model"].as_str().map(String::from),
            }],
            "message" if s("role") == "assistant" => {
                let text = s("content");
                if text.is_empty() {
                    return Vec::new();
                }
                let mut out = Vec::new();
                if !self.in_text {
                    self.block += 1;
                    self.in_text = true;
                    out.push(Event::Start {
                        msg: "gemini".into(),
                        index: self.block,
                        block: Block::Text(String::new()),
                    });
                }
                out.push(Event::Delta {
                    msg: "gemini".into(),
                    index: self.block,
                    kind: DeltaKind::Text,
                    text: text.into(),
                });
                out
            }
            "tool_use" => {
                self.in_text = false;
                let (name, input) = normalize_tool(s("tool_name"), &v["parameters"]);
                let id = s("tool_id");
                vec![Event::Snapshot { msg: id.to_string(), block: Block::Tool(ToolCall::new(id, &name, input)) }]
            }
            "tool_result" => {
                self.in_text = false;
                let text = v["output"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v["error"]["message"].as_str().unwrap_or("").to_string());
                vec![Event::ToolResult { id: s("tool_id").to_string(), text, is_error: s("status") == "error" }]
            }
            "error" if s("severity") == "error" => {
                vec![Event::Snapshot { msg: "gemini-error".into(), block: Block::Error(s("message").to_string()) }]
            }
            "result" => {
                self.in_text = false;
                let st = &v["stats"];
                let n = |k: &str| st.get(k).and_then(Value::as_u64).unwrap_or(0);
                let usage = Usage {
                    input: n("input_tokens"),
                    output: n("output_tokens"),
                    duration_ms: n("duration_ms"),
                    ..Default::default()
                };
                let error = (s("status") == "error")
                    .then(|| v["error"]["message"].as_str().unwrap_or("The turn failed").to_string());
                vec![Event::Done { usage, cumulative: false, session: None, error }]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::chat::{Chat, Phase, ToolStatus};
    use super::*;

    // Lines in the shape `gemini --output-format stream-json` documents.
    const LINES: &[&str] = &[
        r#"{"type":"init","timestamp":"2025-10-10T12:00:00.000Z","session_id":"abc-123","model":"gemini-2.5-pro"}"#,
        r#"{"type":"message","role":"user","content":"fix the bug"}"#,
        r#"{"type":"message","role":"assistant","content":"Let me look","delta":true}"#,
        r#"{"type":"message","role":"assistant","content":" at the file.","delta":true}"#,
        r#"{"type":"tool_use","tool_name":"replace","tool_id":"t1","parameters":{"file_path":"/p/a.rs","old_string":"x","new_string":"y"}}"#,
        r#"{"type":"tool_result","tool_id":"t1","status":"success","output":"done"}"#,
        r#"{"type":"message","role":"assistant","content":"Fixed.","delta":true}"#,
        r#"{"type":"result","status":"success","stats":{"total_tokens":120,"input_tokens":100,"output_tokens":20,"duration_ms":3400,"tool_calls":1}}"#,
    ];

    #[test]
    fn a_turn_folds_into_the_transcript() {
        let mut c = Chat::new();
        c.user_says("fix the bug");
        let mut p = Parser::new();
        for l in LINES {
            for e in p.parse_line(l) {
                c.apply(e);
            }
        }
        assert_eq!(c.phase, Phase::Idle);
        assert_eq!(c.session_id.as_deref(), Some("abc-123"));
        assert_eq!(c.model.as_deref(), Some("gemini-2.5-pro"));
        let b = &c.messages[1].blocks;
        assert_eq!(b.len(), 3, "{b:?}");
        assert_eq!(b[0], Block::Text("Let me look at the file.".into()));
        let Block::Tool(t) = &b[1] else { panic!("tool") };
        assert_eq!((t.name.as_str(), t.status), ("Edit", ToolStatus::Ok));
        assert_eq!(t.input["new_string"], "y");
        assert_eq!(b[2], Block::Text("Fixed.".into()), "a chunk after a tool call is a new block");
        assert_eq!((c.usage().input, c.usage().output, c.usage().duration_ms), (100, 20, 3400));
    }

    #[test]
    fn errors_and_tool_mapping() {
        let mut p = Parser::new();
        let evs = p.parse_line(r#"{"type":"error","severity":"error","message":"quota"}"#);
        assert!(matches!(&evs[..], [Event::Snapshot { block: Block::Error(m), .. }] if m == "quota"));
        assert!(p.parse_line(r#"{"type":"error","severity":"warning","message":"slow"}"#).is_empty());
        let evs = p.parse_line(r#"{"type":"result","status":"error","error":{"message":"boom"}}"#);
        assert!(matches!(&evs[..], [Event::Done { error: Some(e), .. }] if e == "boom"));
        let (n, i) = normalize_tool("run_shell_command", &json!({"command": "ls", "description": "list"}));
        assert_eq!((n.as_str(), i["command"].as_str()), ("Bash", Some("ls")));
        let (n, i) = normalize_tool("read_file", &json!({"absolute_path": "/a"}));
        assert_eq!((n.as_str(), i["file_path"].as_str()), ("Read", Some("/a")));
        let (n, _) = normalize_tool("save_memory", &json!({"fact": "x"}));
        assert_eq!(n, "save_memory");
        let evs = p
            .parse_line(r#"{"type":"tool_result","tool_id":"t","status":"error","error":{"type":"x","message":"no"}}"#);
        assert!(matches!(&evs[..], [Event::ToolResult { text, is_error: true, .. }] if text == "no"));
        assert!(p.parse_line("nope").is_empty());
    }

    #[test]
    fn the_prompt_is_one_argument_and_turns_resume() {
        let a = args("a b\nc", Some("sid"), None);
        assert_eq!(a[1], "a b\nc");
        assert_eq!(&a[a.len() - 2..], &["--resume".to_string(), "sid".to_string()]);
        assert!(!args("x", None, None).contains(&"--resume".to_string()));
    }
}
