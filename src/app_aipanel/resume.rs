//! Earlier conversations of a project, for `/resume`: Claude Code keeps each one as a JSON lines
//! file in `~/.claude/projects/<slug>/<session id>.jsonl`. This lists them (title, date, number of
//! messages) and rebuilds the transcript of one, so picking it shows the history on screen and
//! the next message continues it with `--resume <id>`. Plain code with no App: unit-tested below.

use super::chat::{Block, Message, Role, ToolCall, ToolStatus};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::SystemTime,
};

/// One earlier conversation.
#[derive(Clone, PartialEq, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    /// Last write, seconds since the Unix epoch.
    pub modified: u64,
    pub messages: usize,
    pub path: PathBuf,
}

/// The folder name Claude Code gives a project: every character that is not a letter or a digit
/// becomes a dash (`/home/me/app` is `-home-me-app`).
pub fn slug(cwd: &Path) -> String {
    cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Where the conversations of `cwd` are kept.
pub fn dir_of(home: &Path, cwd: &Path) -> PathBuf {
    home.join(".claude/projects").join(slug(cwd))
}

/// The text of a user message when a person typed it: not a tool result, not the harness' own
/// bookkeeping (`<system-reminder>`, hook and command echoes). Command echoes become `/name args`.
pub fn typed_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let texts: Vec<&str> =
                a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
            if texts.is_empty() {
                return None;
            }
            texts.join("\n")
        }
        _ => return None,
    };
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("<command-name>") {
        let name = rest.split("</command-name>").next().unwrap_or("").trim();
        let args =
            t.split("<command-args>").nth(1).and_then(|a| a.split("</command-args>").next()).unwrap_or("").trim();
        return (!name.is_empty()).then(|| if args.is_empty() { name.to_string() } else { format!("{name} {args}") });
    }
    let noise = ["<system-reminder>", "<local-command", "<command-", "<bash-", "[Request interrupted", "Caveat:"];
    (!t.is_empty() && !noise.iter().any(|n| t.starts_with(n))).then(|| t.to_string())
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn short(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > n { format!("{}…", one.chars().take(n - 1).collect::<String>()) } else { one }
}

/// Read one conversation file: its title (the one the user gave, else the generated one, else the
/// last then first prompt) and how many messages it holds. Lines are only parsed when they can
/// matter, so a big file is a cheap read.
pub fn inspect(path: &Path) -> Option<SessionInfo> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let mut rd = BufReader::new(std::fs::File::open(path).ok()?);
    let (mut custom, mut ai, mut last, mut first) = (None, None, None, None);
    let mut messages = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match rd.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let has = |needle: &[u8]| buf.windows(needle.len()).any(|w| w == needle);
        if has(b"\"type\":\"custom-title\"") || has(b"\"type\": \"custom-title\"") {
            let v: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
            custom = v["customTitle"].as_str().map(String::from).or(custom);
        } else if has(b"\"type\":\"ai-title\"") || has(b"\"type\": \"ai-title\"") {
            let v: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
            ai = v["aiTitle"].as_str().map(String::from).or(ai);
        } else if has(b"\"type\":\"last-prompt\"") || has(b"\"type\": \"last-prompt\"") {
            let v: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
            last = v["lastPrompt"].as_str().map(String::from).or(last);
        } else if (has(b"\"type\":\"user\"") || has(b"\"type\": \"user\"")) && !has(b"\"isSidechain\":true") {
            // Tool results and bookkeeping are users' lines too: only a typed prompt counts.
            if !has(b"\"tool_result\"") && !has(b"\"isMeta\":true") {
                let v: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
                if let Some(t) = typed_text(&v["message"]["content"]) {
                    messages += 1;
                    first.get_or_insert(t);
                }
            }
        } else if (has(b"\"type\":\"assistant\"") || has(b"\"type\": \"assistant\"")) && !has(b"\"isSidechain\":true") {
            messages += 1;
        }
    }
    let first = first?;
    // Title helpers Claude Code runs in the background ("Name this chat…") are not conversations.
    if first.starts_with("Name this chat") {
        return None;
    }
    let title = custom.or(ai).or(last).unwrap_or(first);
    Some(SessionInfo { id, title: short(&title, 90), modified: mtime(path), messages, path: path.to_path_buf() })
}

/// The conversations of the project `cwd`, newest first.
pub fn list(home: &Path, cwd: &Path) -> Vec<SessionInfo> {
    let Ok(rd) = std::fs::read_dir(dir_of(home, cwd)) else { return Vec::new() };
    let mut v: Vec<SessionInfo> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| inspect(&p))
        .collect();
    v.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.id.cmp(&b.id)));
    v
}

/// "3 min ago", "yesterday", "12 Nov": how long ago `then` was, as of `now` (both Unix seconds).
pub fn ago(now: u64, then: u64) -> String {
    let d = now.saturating_sub(then);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", d / 86_400),
    }
}

fn block_of(b: &Value) -> Option<Block> {
    let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
    match b["type"].as_str()? {
        "text" => Some(Block::Text(s("text"))),
        "thinking" if !s("thinking").is_empty() => Some(Block::Thinking(s("thinking"))),
        "tool_use" => Some(Block::Tool(ToolCall::new(&s("id"), &s("name"), b["input"].clone()))),
        _ => None,
    }
}

fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// The transcript of a conversation file: what the person typed, the answers (text, thinking,
/// tool calls with their results), in file order. Sub-agents' inner turns, bookkeeping lines and
/// the generated "No response requested" are left out.
pub fn load(path: &Path) -> Vec<Message> {
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    let mut out: Vec<Message> = Vec::new();
    // Tool call id -> (message, block), for the result lines that come after.
    let mut tools: HashMap<String, (usize, usize)> = HashMap::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v["isSidechain"].as_bool().unwrap_or(false) || v["isMeta"].as_bool().unwrap_or(false) {
            continue;
        }
        match v["type"].as_str() {
            Some("user") => {
                let content = &v["message"]["content"];
                if let Some(parts) = content.as_array() {
                    for r in parts.iter().filter(|b| b["type"] == "tool_result") {
                        let id = r["tool_use_id"].as_str().unwrap_or("");
                        if let Some(&(mi, bi)) = tools.get(id)
                            && let Some(Block::Tool(t)) = out.get_mut(mi).and_then(|m| m.blocks.get_mut(bi))
                        {
                            t.status = if r["is_error"].as_bool().unwrap_or(false) {
                                ToolStatus::Error
                            } else {
                                ToolStatus::Ok
                            };
                            t.result = Some(result_text(&r["content"]));
                        }
                    }
                }
                if let Some(t) = typed_text(content) {
                    out.push(Message { role: Role::User, blocks: vec![Block::Text(t)] });
                }
            }
            Some("assistant") => {
                let msg = &v["message"];
                let blocks: Vec<Block> = msg["content"].as_array().into_iter().flatten().filter_map(block_of).collect();
                let blocks: Vec<Block> = blocks
                    .into_iter()
                    .filter(
                        |b| !matches!(b, Block::Text(t) if t.trim().is_empty() || t.trim() == "No response requested."),
                    )
                    .collect();
                if blocks.is_empty() {
                    continue;
                }
                // One answer is several lines (one per content block) with the same message id;
                // consecutive answers without a user message in between join into one turn.
                if out.last().is_none_or(|m| m.role != Role::Assistant) {
                    out.push(Message { role: Role::Assistant, blocks: Vec::new() });
                }
                let mi = out.len() - 1;
                for b in blocks {
                    if let Block::Tool(t) = &b {
                        tools.insert(t.id.clone(), (mi, out[mi].blocks.len()));
                    }
                    out[mi].blocks.push(b);
                }
            }
            _ => {}
        }
    }
    // A call without a result (the turn was cut) is shown as done, not running.
    for m in &mut out {
        for b in &mut m.blocks {
            if let Block::Tool(t) = b
                && t.status == ToolStatus::Running
            {
                t.status = ToolStatus::Ok;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lumen-resume-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    const USER: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"fix the build"},"uuid":"u1","timestamp":"2026-11-19T10:00:00Z"}"#;
    const REMINDER: &str =
        r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<system-reminder>x</system-reminder>"}}"#;
    const TOOL: &str = r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo build"}}]}}"#;
    const THINK: &str = r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"thinking","thinking":"hmm","signature":"s"}]}}"#;
    const RESULT: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"error[E0432]","is_error":true}]}}"#;
    const TEXT: &str = r#"{"type":"assistant","message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"Fixed the import."}]}}"#;
    const SIDE: &str = r#"{"type":"assistant","isSidechain":true,"message":{"id":"m3","role":"assistant","content":[{"type":"text","text":"inner"}]}}"#;
    const NOREPLY: &str = r#"{"type":"assistant","message":{"id":"m4","role":"assistant","content":[{"type":"text","text":"No response requested."}]}}"#;
    const AI_TITLE: &str = r#"{"type":"ai-title","aiTitle":"Fix the build","sessionId":"s"}"#;
    const LAST: &str = r#"{"type":"last-prompt","lastPrompt":"then run the tests","sessionId":"s"}"#;
    const CMD: &str = r#"{"type":"user","message":{"role":"user","content":"<command-name>/compact</command-name>\n<command-args>keep notes</command-args>"}}"#;

    fn write(dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
        let p = dir.join(format!("{id}.jsonl"));
        std::fs::write(&p, lines.join("\n")).unwrap();
        p
    }

    #[test]
    fn the_project_folder_name_is_claudes_slug() {
        assert_eq!(slug(Path::new("/home/castro/www/lumen-text")), "-home-castro-www-lumen-text");
        assert_eq!(slug(Path::new("/tmp/claude-1000/-home/x")), "-tmp-claude-1000--home-x");
        assert_eq!(slug(Path::new("/a/.claude/wt_1")), "-a--claude-wt-1");
        assert_eq!(dir_of(Path::new("/h"), Path::new("/p/q")), Path::new("/h/.claude/projects/-p-q"));
    }

    #[test]
    fn typed_text_skips_what_a_person_did_not_type() {
        assert_eq!(typed_text(&serde_json::json!("hello")).as_deref(), Some("hello"));
        assert_eq!(
            typed_text(&serde_json::json!([{"type":"text","text":"a"},{"type":"text","text":"b"}])).as_deref(),
            Some("a\nb")
        );
        assert_eq!(typed_text(&serde_json::json!([{"type":"tool_result","content":"x"}])), None);
        assert_eq!(typed_text(&serde_json::json!("<system-reminder>x</system-reminder>")), None);
        assert_eq!(typed_text(&serde_json::json!("[Request interrupted by user]")), None);
        assert_eq!(
            typed_text(&serde_json::json!("<command-name>/compact</command-name><command-args>keep</command-args>"))
                .as_deref(),
            Some("/compact keep")
        );
    }

    #[test]
    fn listing_shows_title_count_and_order_and_skips_helpers() {
        let home = tmp("list");
        let cwd = Path::new("/work/app");
        let dir = dir_of(&home, cwd);
        std::fs::create_dir_all(&dir).unwrap();
        let a = write(&dir, "aaaa", &[USER, REMINDER, TOOL, RESULT, TEXT, SIDE, AI_TITLE, LAST]);
        write(&dir, "bbbb", &[USER, TEXT]);
        write(
            &dir,
            "helper",
            &[
                r#"{"type":"user","message":{"role":"user","content":"Name this chat from the user's first request."}}"#,
                TEXT,
            ],
        );
        write(&dir, "empty", &[AI_TITLE]);
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        // Make `aaaa` the older one.
        let f = std::fs::File::options().write(true).open(&a).unwrap();
        f.set_modified(SystemTime::now() - std::time::Duration::from_secs(7200)).unwrap();
        let v = list(&home, cwd);
        assert_eq!(
            v.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["bbbb", "aaaa"],
            "newest first, helpers and empties out"
        );
        assert_eq!(v[0].title, "fix the build", "no title lines: the first prompt");
        assert_eq!(v[1].title, "Fix the build", "the generated title wins over the last prompt");
        assert_eq!(v[1].messages, 3, "typed prompt + 2 assistant lines; sub-agents and results do not count: {v:?}");
        assert!(v[0].modified > v[1].modified);
        assert!(list(&home, Path::new("/other")).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_custom_title_beats_the_generated_one() {
        let home = tmp("custom");
        let p = write(&home, "c1", &[USER, AI_TITLE, r#"{"type":"custom-title","customTitle":"My name"}"#]);
        assert_eq!(inspect(&p).unwrap().title, "My name");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_conversation_comes_back_as_a_transcript() {
        let home = tmp("load");
        let p = write(&home, "s", &[USER, REMINDER, THINK, TOOL, RESULT, TEXT, SIDE, NOREPLY, CMD, AI_TITLE]);
        let m = load(&p);
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!((m[0].role, &m[0].blocks[..]), (Role::User, &[Block::Text("fix the build".into())][..]));
        assert_eq!(m[1].role, Role::Assistant);
        assert_eq!(m[1].blocks.len(), 3, "thinking, tool, text of one turn");
        let Block::Tool(t) = &m[1].blocks[1] else { panic!("tool") };
        assert_eq!((t.name.as_str(), t.status, t.result.as_deref()), ("Bash", ToolStatus::Error, Some("error[E0432]")));
        assert_eq!(m[1].blocks[2], Block::Text("Fixed the import.".into()));
        assert_eq!(m[2].blocks, vec![Block::Text("/compact keep notes".into())], "a command echo is the command");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_call_without_a_result_is_not_left_running() {
        let home = tmp("cut");
        let p = write(&home, "s", &[USER, TOOL]);
        let m = load(&p);
        let Block::Tool(t) = &m[1].blocks[0] else { panic!("tool") };
        assert_eq!(t.status, ToolStatus::Ok);
        assert!(load(&home.join("missing.jsonl")).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(ago(1000, 990), "just now");
        assert_eq!(ago(1000, 1000 - 300), "5 min ago");
        assert_eq!(ago(100_000, 100_000 - 7300), "2 h ago");
        assert_eq!(ago(1_000_000, 1_000_000 - 100_000), "yesterday");
        assert_eq!(ago(1_000_000, 1_000_000 - 500_000), "5 days ago");
    }
}
