//! The common model of an agent conversation, filled by the adapters of each CLI (`proto_claude`,
//! `proto_codex`, `proto_gemini`): messages made of blocks (markdown text, thinking, tool calls,
//! errors), the status of the turn, and the usage (tokens, cost). Plain code with no App and no
//! drawing: every adapter turns the structured output of its CLI into [`Event`]s and [`Chat::apply`]
//! folds them in, so the panel never looks at the CLI's screen.

use serde_json::Value;
use std::{collections::HashMap, time::Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolStatus {
    Running,
    /// Waiting for the user's Allow / Deny.
    Asking,
    Ok,
    Error,
    Denied,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub status: ToolStatus,
    pub result: Option<String>,
}

impl ToolCall {
    pub fn new(id: &str, name: &str, input: Value) -> ToolCall {
        ToolCall { id: id.into(), name: name.into(), input, status: ToolStatus::Running, result: None }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    /// Markdown.
    Text(String),
    Thinking(String),
    Tool(ToolCall),
    Error(String),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

/// Where the turn is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    #[default]
    Idle,
    Working,
    /// The agent asks for a permission.
    Waiting,
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub turns: u32,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cost_usd += o.cost_usd;
        self.duration_ms += o.duration_ms;
        self.turns += o.turns;
    }
}

/// A permission the agent asks for before running a tool.
#[derive(Clone, PartialEq, Debug)]
pub struct Perm {
    pub request_id: String,
    pub tool_id: Option<String>,
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeltaKind {
    Text,
    Thinking,
    /// A piece of the JSON of a tool's input.
    Json,
}

/// What an adapter reports.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    Init {
        session: Option<String>,
        model: Option<String>,
    },
    /// A block starts streaming (`msg` is the id of the API message, `index` its block number).
    Start {
        msg: String,
        index: usize,
        block: Block,
    },
    Delta {
        msg: String,
        index: usize,
        kind: DeltaKind,
        text: String,
    },
    Stop {
        msg: String,
        index: usize,
    },
    /// The finished block, as the CLI reports it (one per block, in order).
    Snapshot {
        msg: String,
        block: Block,
    },
    ToolResult {
        id: String,
        text: String,
        is_error: bool,
    },
    Permission(Perm),
    /// The turn ended. `cumulative`: the usage is the total of the process so far (Claude Code),
    /// otherwise it is this turn's alone.
    Done {
        usage: Usage,
        cumulative: bool,
        session: Option<String>,
        error: Option<String>,
    },
    /// The CLI could not run (or died): the turn ends with this error.
    Fatal(String),
}

/// One conversation with one agent.
#[derive(Default)]
pub struct Chat {
    pub messages: Vec<Message>,
    pub phase: Phase,
    /// Bumped by every change: views re-lay out only when it moved.
    pub rev: u64,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub perms: Vec<Perm>,
    /// Usage of finished processes plus the running one.
    base: Usage,
    proc: Usage,
    /// Turns that ended.
    pub turns: u32,
    pub turn_started: Option<Instant>,
    streams: HashMap<(String, usize), usize>,
    json: HashMap<(String, usize), String>,
    ordinals: HashMap<String, usize>,
}

impl Chat {
    pub fn new() -> Chat {
        Chat::default()
    }

    /// Everything spent so far.
    pub fn usage(&self) -> Usage {
        let mut u = self.base.clone();
        u.add(&self.proc);
        u.turns = self.turns;
        u
    }

    /// A new process starts: what the old one reported is kept.
    pub fn new_process(&mut self) {
        let p = std::mem::take(&mut self.proc);
        self.base.add(&p);
    }

    /// The user sends a message: it joins the transcript and the turn begins.
    pub fn user_says(&mut self, text: &str) {
        self.messages.push(Message { role: Role::User, blocks: vec![Block::Text(text.to_string())] });
        self.begin_turn();
    }

    pub fn begin_turn(&mut self) {
        self.phase = Phase::Working;
        self.turn_started = Some(Instant::now());
        self.streams.clear();
        self.json.clear();
        self.ordinals.clear();
        self.rev += 1;
    }

    pub fn working(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// The blocks of the assistant's message of the turn (started when there is none).
    fn blocks(&mut self) -> &mut Vec<Block> {
        if self.messages.last().is_none_or(|m| m.role != Role::Assistant) {
            self.messages.push(Message { role: Role::Assistant, blocks: Vec::new() });
        }
        let i = self.messages.len() - 1;
        &mut self.messages[i].blocks
    }

    fn push(&mut self, b: Block) -> usize {
        let v = self.blocks();
        v.push(b);
        v.len() - 1
    }

    pub fn tool_mut(&mut self, id: &str) -> Option<&mut ToolCall> {
        self.messages.iter_mut().rev().flat_map(|m| m.blocks.iter_mut().rev()).find_map(|b| match b {
            Block::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    /// Fold one event in.
    pub fn apply(&mut self, ev: Event) {
        self.rev += 1;
        match ev {
            Event::Init { session, model } => {
                if session.is_some() {
                    self.session_id = session;
                }
                if model.is_some() {
                    self.model = model;
                }
            }
            Event::Start { msg, index, block } => {
                if let Block::Tool(t) = &block
                    && self.tool_mut(&t.id).is_some()
                {
                    return;
                }
                let at = self.push(block);
                self.streams.insert((msg, index), at);
            }
            Event::Delta { msg, index, kind, text } => {
                if kind == DeltaKind::Json {
                    self.json.entry((msg, index)).or_default().push_str(&text);
                    return;
                }
                let at = match self.streams.get(&(msg.clone(), index)) {
                    Some(&at) => at,
                    None => {
                        let b = if kind == DeltaKind::Text {
                            Block::Text(String::new())
                        } else {
                            Block::Thinking(String::new())
                        };
                        let at = self.push(b);
                        self.streams.insert((msg, index), at);
                        at
                    }
                };
                match self.blocks().get_mut(at) {
                    Some(Block::Text(s)) | Some(Block::Thinking(s)) => s.push_str(&text),
                    _ => {}
                }
            }
            Event::Stop { msg, index } => {
                let Some(raw) = self.json.remove(&(msg.clone(), index)) else { return };
                let Some(&at) = self.streams.get(&(msg, index)) else { return };
                if let (Some(Block::Tool(t)), Ok(v)) = (self.blocks().get_mut(at), serde_json::from_str::<Value>(&raw))
                {
                    t.input = v;
                }
            }
            Event::Snapshot { msg, block } => self.snapshot(msg, block),
            Event::ToolResult { id, text, is_error } => {
                if let Some(t) = self.tool_mut(&id) {
                    if t.status != ToolStatus::Denied {
                        t.status = if is_error { ToolStatus::Error } else { ToolStatus::Ok };
                    }
                    t.result = Some(text);
                }
            }
            Event::Permission(p) => {
                if let Some(id) = &p.tool_id
                    && let Some(t) = self.tool_mut(id)
                {
                    t.status = ToolStatus::Asking;
                }
                self.perms.push(p);
                self.phase = Phase::Waiting;
            }
            Event::Done { usage, cumulative, session, error } => {
                self.turns += 1;
                if cumulative {
                    self.proc = usage;
                } else {
                    self.base.add(&usage);
                }
                if session.is_some() {
                    self.session_id = session;
                }
                if let Some(e) = error {
                    self.push(Block::Error(e));
                }
                self.end_turn();
            }
            Event::Fatal(e) => {
                self.push(Block::Error(e));
                self.end_turn();
            }
        }
    }

    fn snapshot(&mut self, msg: String, block: Block) {
        if let Block::Tool(t) = &block {
            if let Some(old) = self.tool_mut(&t.id) {
                if !t.name.is_empty() {
                    old.name = t.name.clone();
                }
                if !t.input.is_null() && t.input != Value::Object(Default::default()) {
                    old.input = t.input.clone();
                }
                return;
            }
            self.push(block);
            return;
        }
        let k = self.ordinals.entry(msg.clone()).or_insert(0);
        let ordinal = *k;
        *k += 1;
        let at = self.streams.get(&(msg, ordinal)).copied();
        let same = at.and_then(|at| self.blocks().get(at)).is_some_and(|slot| {
            matches!((slot, &block), (Block::Text(_), Block::Text(_)) | (Block::Thinking(_), Block::Thinking(_)))
        });
        if let (true, Some(at)) = (same, at) {
            match (self.blocks().get_mut(at), block) {
                (Some(Block::Text(old)), Block::Text(new)) => *old = new,
                (Some(Block::Thinking(old)), Block::Thinking(new)) if !new.is_empty() => *old = new,
                _ => {}
            }
            return;
        }
        if !matches!(&block, Block::Text(s) | Block::Thinking(s) if s.is_empty()) {
            self.push(block);
        }
    }

    fn end_turn(&mut self) {
        self.phase = Phase::Idle;
        self.perms.clear();
        self.streams.clear();
        self.json.clear();
        self.ordinals.clear();
        self.turn_started = None;
        for m in &mut self.messages {
            for b in &mut m.blocks {
                if let Block::Tool(t) = b
                    && matches!(t.status, ToolStatus::Running | ToolStatus::Asking)
                {
                    t.status = ToolStatus::Error;
                }
            }
        }
    }

    /// The user answered permission `request_id`: the card goes, the tool runs or is denied.
    pub fn resolve_perm(&mut self, request_id: &str, allow: bool) -> Option<Perm> {
        let at = self.perms.iter().position(|p| p.request_id == request_id)?;
        let p = self.perms.remove(at);
        if let Some(id) = &p.tool_id
            && let Some(t) = self.tool_mut(id)
        {
            t.status = if allow { ToolStatus::Running } else { ToolStatus::Denied };
        }
        if self.perms.is_empty() && self.phase == Phase::Waiting {
            self.phase = Phase::Working;
        }
        self.rev += 1;
        Some(p)
    }

    /// The user interrupted: the turn ends where it is.
    pub fn interrupted(&mut self) {
        self.rev += 1;
        self.push(Block::Error("Interrupted".into()));
        self.end_turn();
    }
}

// ───────────────────────────── tools: names, icons, one-line summaries ─────────────────────────────

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// A path made relative to `cwd` when it lives under it.
pub fn rel_path(path: &str, cwd: &std::path::Path) -> String {
    std::path::Path::new(path).strip_prefix(cwd).map_or_else(|_| path.to_string(), |p| p.display().to_string())
}

/// The file a tool call works on, if any.
pub fn tool_file(name: &str, input: &Value) -> Option<String> {
    if !matches!(name, "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
        return None;
    }
    ["file_path", "notebook_path", "path"].iter().map(|k| str_of(input, k)).find(|s| !s.is_empty()).map(String::from)
}

/// The one line shown on a collapsed tool card.
pub fn tool_summary(name: &str, input: &Value) -> String {
    let first = |s: &str| s.lines().next().unwrap_or("").trim().to_string();
    match name {
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => tool_file(name, input).unwrap_or_default(),
        "Bash" => {
            let d = str_of(input, "description");
            first(if d.is_empty() { str_of(input, "command") } else { d })
        }
        "Grep" => {
            let (p, d) = (str_of(input, "pattern"), str_of(input, "path"));
            if d.is_empty() { p.to_string() } else { format!("{p} in {d}") }
        }
        "Glob" => str_of(input, "pattern").to_string(),
        "LS" => str_of(input, "path").to_string(),
        "WebFetch" => str_of(input, "url").to_string(),
        "WebSearch" => str_of(input, "query").to_string(),
        "Task" | "Agent" => {
            let d = str_of(input, "description");
            if d.is_empty() { first(str_of(input, "prompt")) } else { d.to_string() }
        }
        "TodoWrite" => {
            let n = input.get("todos").and_then(Value::as_array).map_or(0, Vec::len);
            format!("{n} item{}", if n == 1 { "" } else { "s" })
        }
        _ => input.as_object().and_then(|o| o.values().find_map(Value::as_str)).map(first).unwrap_or_default(),
    }
}

/// The icon of a tool (names of the bundled `ui/` set).
pub fn tool_icon(name: &str) -> &'static str {
    match name {
        "Read" => "ui/file_code",
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "ui/tool_pencil",
        "Bash" | "BashOutput" | "KillShell" => "ui/tool_terminal",
        "Grep" | "Glob" | "LS" => "ui/tool_search",
        "WebFetch" | "WebSearch" => "ui/tool_web",
        "Task" | "Agent" => "ui/sparkle",
        "TodoWrite" => "ui/list_todo",
        _ => "ui/tool_hammer",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    Add,
    Del,
}

/// The lines an Edit / Write / MultiEdit call changes, as `-` and `+` rows (capped).
pub fn diff_lines(name: &str, input: &Value) -> Vec<(DiffKind, String)> {
    const CAP: usize = 120;
    let mut out: Vec<(DiffKind, String)> = Vec::new();
    let mut edit = |old: &str, new: &str| {
        if !old.is_empty() {
            out.extend(old.lines().map(|l| (DiffKind::Del, l.to_string())));
        }
        if !new.is_empty() {
            out.extend(new.lines().map(|l| (DiffKind::Add, l.to_string())));
        }
    };
    match name {
        "Edit" => edit(str_of(input, "old_string"), str_of(input, "new_string")),
        "Write" => edit("", str_of(input, "content")),
        "MultiEdit" => {
            for e in input.get("edits").and_then(Value::as_array).into_iter().flatten() {
                edit(str_of(e, "old_string"), str_of(e, "new_string"));
            }
        }
        _ => {}
    }
    out.truncate(CAP);
    out
}

/// Tool names whose card opens by itself (what changed matters more than the call).
pub fn opens_by_default(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit")
}

// ───────────────────────────── formatting ─────────────────────────────

pub fn format_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

pub fn format_cost(c: f64) -> String {
    if c < 0.01 { format!("${c:.4}") } else { format!("${c:.2}") }
}

/// The usage footer: "$0.09 · 1.2k in · 340 out · 3 turns" (empty before the first turn ended).
pub fn usage_line(u: &Usage) -> String {
    if u.turns == 0 && u.cost_usd == 0.0 && u.output == 0 {
        return String::new();
    }
    let mut parts = Vec::new();
    if u.cost_usd > 0.0 {
        parts.push(format_cost(u.cost_usd));
    }
    if u.input > 0 {
        parts.push(format!("{} in", format_tokens(u.input)));
    }
    if u.output > 0 {
        parts.push(format!("{} out", format_tokens(u.output)));
    }
    if u.turns > 0 {
        parts.push(format!("{} turn{}", u.turns, if u.turns == 1 { "" } else { "s" }));
    }
    parts.join(" · ")
}

// ───────────────────────────── text helpers ─────────────────────────────

/// Greedy word wrap at `width` px. `measure(line)` gives the x of each of its `n + 1` char
/// boundaries; a word longer than the width is cut where it overflows.
pub fn wrap_text(text: &str, width: f32, measure: &mut dyn FnMut(&str) -> Vec<f32>) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let chars: Vec<char> = para.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut xs = measure(para);
        if xs.len() != chars.len() + 1 {
            xs = (0..=chars.len()).map(|i| i as f32 * 8.0).collect();
        }
        let n = chars.len();
        let mut start = 0;
        while start < n {
            let mut end = start + 1;
            while end < n && xs[end + 1] - xs[start] <= width {
                end += 1;
            }
            if end >= n {
                out.push(chars[start..n].iter().collect());
                break;
            }
            match (start + 1..=end).rev().find(|&i| chars[i - 1] == ' ') {
                Some(i) => {
                    out.push(chars[start..i].iter().collect::<String>().trim_end().to_string());
                    start = i;
                    while start < n && chars[start] == ' ' {
                        start += 1;
                    }
                }
                None => {
                    out.push(chars[start..end].iter().collect());
                    start = end;
                }
            }
        }
    }
    out
}

/// `path:12:3` split into the path and its line (1-based).
pub fn split_line_suffix(s: &str) -> (&str, Option<usize>) {
    let mut rest = s;
    let mut nums = Vec::new();
    while let Some((head, tail)) = rest.rsplit_once(':') {
        match tail.parse::<usize>() {
            Ok(n) if nums.len() < 2 => {
                nums.push(n);
                rest = head;
            }
            _ => break,
        }
    }
    (rest, nums.last().copied())
}

fn path_like(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 240
        && !s.contains(char::is_whitespace)
        && !s.contains("://")
        && !s.starts_with('-')
        && (s.contains('/') || s.rsplit_once('.').is_some_and(|(a, e)| !a.is_empty() && !e.is_empty() && e.len() <= 6))
        && s.chars().all(|c| c.is_alphanumeric() || "/\\._-+@~".contains(c))
}

/// Inline code that names an existing file (optionally `:line`) becomes a link to it, so a click
/// opens it in the editor. Code blocks are left alone. `exists` says whether a path is a file.
pub fn linkify_paths(text: &str, exists: &mut dyn FnMut(&str) -> bool) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut fenced = false;
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            out.push_str(line);
            continue;
        }
        if fenced || !line.contains('`') {
            out.push_str(line);
            continue;
        }
        for (i, seg) in line.split('`').enumerate() {
            // Odd segments sit between backticks (only trustworthy when they pair up).
            if i % 2 == 1 && line.matches('`').count() % 2 == 0 {
                let (path, _) = split_line_suffix(seg);
                if path_like(path) && exists(path) {
                    out.push_str(&format!("[`{seg}`]({seg})"));
                } else {
                    out.push('`');
                    out.push_str(seg);
                    out.push('`');
                }
            } else if i > 0 && i % 2 == 0 && line.matches('`').count() % 2 == 0 {
                out.push_str(seg);
            } else if i > 0 {
                out.push('`');
                out.push_str(seg);
            } else {
                out.push_str(seg);
            }
        }
    }
    out
}

/// Last lines of a tool's output, with how many were left out above.
pub fn tail_lines(text: &str, max: usize) -> (Vec<&str>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return (lines, 0);
    }
    let skip = lines.len() - max;
    (lines[skip..].to_vec(), skip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev_text(msg: &str, i: usize, t: &str) -> Event {
        Event::Delta { msg: msg.into(), index: i, kind: DeltaKind::Text, text: t.into() }
    }

    #[test]
    fn streamed_text_then_snapshot_is_one_block() {
        let mut c = Chat::new();
        c.user_says("hi");
        c.apply(Event::Start { msg: "m1".into(), index: 0, block: Block::Text(String::new()) });
        c.apply(ev_text("m1", 0, "Hel"));
        c.apply(ev_text("m1", 0, "lo"));
        assert_eq!(c.messages[1].blocks, vec![Block::Text("Hello".into())]);
        c.apply(Event::Snapshot { msg: "m1".into(), block: Block::Text("Hello".into()) });
        assert_eq!(c.messages[1].blocks.len(), 1, "the snapshot replaces the streamed block");
        c.apply(Event::Done {
            usage: Usage { output: 5, cost_usd: 0.01, turns: 1, ..Default::default() },
            cumulative: true,
            session: Some("s".into()),
            error: None,
        });
        assert_eq!(c.phase, Phase::Idle);
        assert_eq!(c.session_id.as_deref(), Some("s"));
        assert_eq!(c.usage().output, 5);
    }

    #[test]
    fn tool_input_streams_as_json_and_results_attach() {
        let mut c = Chat::new();
        c.user_says("run it");
        let tool = Block::Tool(ToolCall::new("t1", "Bash", Value::Null));
        c.apply(Event::Start { msg: "m".into(), index: 0, block: tool });
        for p in ["{\"command\": \"ec", "ho hi\"}"] {
            c.apply(Event::Delta { msg: "m".into(), index: 0, kind: DeltaKind::Json, text: p.into() });
        }
        c.apply(Event::Stop { msg: "m".into(), index: 0 });
        let Some(Block::Tool(t)) = c.messages[1].blocks.first() else { panic!("no tool") };
        assert_eq!(t.input["command"], "echo hi");
        // The finished snapshot of the same tool does not add another card.
        let again = Block::Tool(ToolCall::new("t1", "Bash", json!({"command": "echo hi"})));
        c.apply(Event::Snapshot { msg: "m".into(), block: again });
        assert_eq!(c.messages[1].blocks.len(), 1);
        c.apply(Event::ToolResult { id: "t1".into(), text: "hi".into(), is_error: false });
        let Some(Block::Tool(t)) = c.messages[1].blocks.first() else { panic!("no tool") };
        assert_eq!((t.status, t.result.as_deref()), (ToolStatus::Ok, Some("hi")));
    }

    #[test]
    fn snapshots_without_streaming_append_in_order_and_skip_empty_thinking() {
        let mut c = Chat::new();
        c.user_says("x");
        c.apply(Event::Snapshot { msg: "m".into(), block: Block::Thinking(String::new()) });
        c.apply(Event::Snapshot { msg: "m".into(), block: Block::Text("a".into()) });
        c.apply(Event::Snapshot { msg: "n".into(), block: Block::Text("b".into()) });
        assert_eq!(c.messages[1].blocks, vec![Block::Text("a".into()), Block::Text("b".into())]);
    }

    #[test]
    fn permissions_hold_the_turn_until_answered() {
        let mut c = Chat::new();
        c.user_says("x");
        c.apply(Event::Snapshot {
            msg: "m".into(),
            block: Block::Tool(ToolCall::new("t", "Bash", json!({"command": "rm x"}))),
        });
        c.apply(Event::Permission(Perm {
            request_id: "r1".into(),
            tool_id: Some("t".into()),
            name: "Bash".into(),
            input: json!({"command": "rm x"}),
        }));
        assert_eq!(c.phase, Phase::Waiting);
        assert!(matches!(&c.messages[1].blocks[0], Block::Tool(t) if t.status == ToolStatus::Asking));
        assert!(c.resolve_perm("nope", true).is_none());
        let p = c.resolve_perm("r1", false).expect("the permission");
        assert_eq!(p.name, "Bash");
        assert_eq!(c.phase, Phase::Working);
        assert!(matches!(&c.messages[1].blocks[0], Block::Tool(t) if t.status == ToolStatus::Denied));
    }

    #[test]
    fn a_turn_that_ends_early_closes_its_running_tools() {
        let mut c = Chat::new();
        c.user_says("x");
        c.apply(Event::Snapshot { msg: "m".into(), block: Block::Tool(ToolCall::new("t", "Read", json!({}))) });
        c.interrupted();
        assert_eq!(c.phase, Phase::Idle);
        assert!(matches!(&c.messages[1].blocks[0], Block::Tool(t) if t.status == ToolStatus::Error));
        assert!(matches!(c.messages[1].blocks.last(), Some(Block::Error(e)) if e == "Interrupted"));
    }

    #[test]
    fn usage_is_cumulative_per_process_and_additive_across() {
        let mut c = Chat::new();
        let u = |o, cost| Usage { output: o, cost_usd: cost, turns: 1, ..Default::default() };
        c.apply(Event::Done { usage: u(10, 0.1), cumulative: true, session: None, error: None });
        c.apply(Event::Done { usage: u(25, 0.3), cumulative: true, session: None, error: None });
        assert_eq!(c.usage().output, 25, "a running process reports totals");
        assert_eq!(c.usage().turns, 2, "turns are counted here");
        c.new_process();
        c.apply(Event::Done { usage: u(5, 0.05), cumulative: true, session: None, error: None });
        assert_eq!(c.usage().output, 30);
        c.apply(Event::Done { usage: u(2, 0.0), cumulative: false, session: None, error: None });
        assert_eq!(c.usage().output, 32);
    }

    #[test]
    fn summaries_icons_and_diffs() {
        assert_eq!(tool_summary("Bash", &json!({"command": "ls -la\nfoo"})), "ls -la");
        assert_eq!(tool_summary("Bash", &json!({"command": "ls", "description": "List files"})), "List files");
        assert_eq!(tool_summary("Read", &json!({"file_path": "/a/b.rs"})), "/a/b.rs");
        assert_eq!(tool_summary("Grep", &json!({"pattern": "foo", "path": "src"})), "foo in src");
        assert_eq!(tool_summary("TodoWrite", &json!({"todos": [1, 2]})), "2 items");
        assert_eq!(tool_summary("mcp__x__y", &json!({"q": "hello"})), "hello");
        assert_eq!(tool_icon("Edit"), "ui/tool_pencil");
        assert_eq!(tool_icon("Whatever"), "ui/tool_hammer");
        let d = diff_lines("Edit", &json!({"old_string": "a\nb", "new_string": "c"}));
        assert_eq!(d, vec![(DiffKind::Del, "a".into()), (DiffKind::Del, "b".into()), (DiffKind::Add, "c".into())]);
        assert_eq!(diff_lines("Write", &json!({"content": "x\ny"})).len(), 2);
        assert_eq!(rel_path("/p/src/a.rs", std::path::Path::new("/p")), "src/a.rs");
        assert_eq!(rel_path("/q/a.rs", std::path::Path::new("/p")), "/q/a.rs");
        assert_eq!(tool_file("Edit", &json!({"file_path": "/a"})), Some("/a".into()));
        assert_eq!(tool_file("Bash", &json!({"file_path": "/a"})), None);
    }

    #[test]
    fn numbers_read_well() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(1234), "1.2k");
        assert_eq!(format_tokens(2_500_000), "2.5M");
        assert_eq!(format_cost(0.0894), "$0.09");
        assert_eq!(format_cost(0.004), "$0.0040");
        assert_eq!(usage_line(&Usage::default()), "");
        let u = Usage { input: 1500, output: 40, cost_usd: 0.05, duration_ms: 1, turns: 2 };
        assert_eq!(usage_line(&u), "$0.05 · 1.5k in · 40 out · 2 turns");
    }

    fn fixed(s: &str) -> Vec<f32> {
        (0..=s.chars().count()).map(|i| i as f32 * 10.0).collect()
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_cuts_long_words() {
        let w = wrap_text("hello brave new world", 100.0, &mut fixed);
        assert_eq!(w, vec!["hello", "brave new", "world"]);
        let w = wrap_text("abcdefghijklmnop", 50.0, &mut fixed);
        assert_eq!(w, vec!["abcde", "fghij", "klmno", "p"]);
        let w = wrap_text("a\n\nb", 100.0, &mut fixed);
        assert_eq!(w, vec!["a", "", "b"]);
        assert_eq!(wrap_text("short", 1000.0, &mut fixed), vec!["short"]);
    }

    #[test]
    fn line_suffixes_split_off() {
        assert_eq!(split_line_suffix("src/a.rs:12:3"), ("src/a.rs", Some(12)));
        assert_eq!(split_line_suffix("src/a.rs:7"), ("src/a.rs", Some(7)));
        assert_eq!(split_line_suffix("src/a.rs"), ("src/a.rs", None));
        assert_eq!(split_line_suffix("C:/x"), ("C:/x", None));
    }

    #[test]
    fn existing_files_in_inline_code_become_links() {
        let mut yes = |p: &str| p == "src/a.rs" || p == "Cargo.toml";
        let t = "See `src/a.rs:12` and `nope.rs`, also `Cargo.toml`.\n```\n`src/a.rs`\n```\nplain `x y`";
        let out = linkify_paths(t, &mut yes);
        assert!(out.contains("[`src/a.rs:12`](src/a.rs:12)"), "{out}");
        assert!(out.contains("`nope.rs`,"), "{out}");
        assert!(out.contains("[`Cargo.toml`](Cargo.toml)."), "{out}");
        assert!(out.contains("```\n`src/a.rs`\n```"), "code blocks stay: {out}");
        assert!(out.ends_with("plain `x y`"), "{out}");
        // Odd backticks (an unfinished span while streaming) are left as they are.
        assert_eq!(linkify_paths("half `src/a.rs", &mut yes), "half `src/a.rs");
    }

    #[test]
    fn tails_report_what_was_cut() {
        let (l, skipped) = tail_lines("1\n2\n3\n4", 2);
        assert_eq!((l, skipped), (vec!["3", "4"], 2));
        assert_eq!(tail_lines("1\n2", 5), (vec!["1", "2"], 0));
    }
}
