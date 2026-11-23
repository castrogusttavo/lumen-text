//! Slash commands of the chat composer: the catalog (what each agent offers and how Lumen runs
//! it), the commands found on disk (`.claude/commands`, skills), the autocomplete ranking and the
//! parsing of a typed `/command args`. Plain code with no App: unit-tested below.
//!
//! How each command runs was checked against the real `claude -p --input-format stream-json`
//! (Claude Code 2.1): `/cost`, `/context`, `/compact`, `/init`, `/review`, `/usage`, skills and
//! custom commands work in that mode and are sent as they are ([`Handling::Pass`]); `/clear` only
//! resets the conversation inside the process, `/model` needs a name and `/help`, `/status`,
//! `/memory`, `/permissions` answer "isn't available in this environment": those are done
//! natively by Lumen ([`Native`]); the ones the CLI lists as terminal-only are [`Handling::Off`]
//! with the reason.

#![allow(dead_code)]

use super::backend::Kind;
use std::path::Path;

/// Where a command comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// Lumen runs it itself.
    Native,
    /// A command of the agent CLI.
    Agent,
    /// `.claude/commands` of the project.
    Project,
    /// `~/.claude/commands`.
    User,
    /// A skill (`.claude/skills`, `~/.claude/skills`) or a plugin's command.
    Skill,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Native => "Lumen",
            Origin::Agent => "agent",
            Origin::Project => "project",
            Origin::User => "user",
            Origin::Skill => "skill",
        }
    }
}

/// The commands Lumen runs itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Native {
    /// Pick an earlier conversation of this project and continue it.
    Resume,
    /// Start over with an empty conversation.
    Clear,
    /// Pick the model (or `/model name`).
    Model,
    Help,
    /// Tokens and cost of this conversation.
    Cost,
    Status,
    /// Lumen's settings (a Claude `/config key=value` goes to the agent).
    Config,
    /// The agent's sub-agents.
    Agents,
    /// Open CLAUDE.md and the memory files.
    Memory,
    /// Pick the permission mode.
    Permissions,
    /// Plan mode, with an optional first request.
    Plan,
    /// Copy the last answer.
    Copy,
}

/// What running a command does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Handling {
    Native(Native),
    /// Sent to the agent as typed: it expands it itself.
    Pass,
    /// Not available here, and why.
    Off(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cmd {
    /// Without the slash.
    pub name: String,
    pub desc: String,
    /// What it takes after its name (`[name]`), empty for nothing.
    pub hint: String,
    pub origin: Origin,
    pub handling: Handling,
}

/// The models the `/model` picker lists for each CLI: (what `--model` / `set_model` takes, what it
/// is). Claude's are its documented aliases; for the others a few common ids: any other one is
/// `/model <id>`.
pub fn models(kind: Kind) -> Vec<(&'static str, &'static str)> {
    match kind {
        Kind::Claude => vec![
            ("default", "The account's default model"),
            ("sonnet", "Sonnet: everyday coding"),
            ("opus", "Opus: the most capable"),
            ("haiku", "Haiku: fast and cheap"),
            ("fable", "Fable"),
            ("opusplan", "Opus to plan, Sonnet to edit"),
            ("sonnet[1m]", "Sonnet with a 1M-token context"),
            ("opus[1m]", "Opus with a 1M-token context"),
        ],
        Kind::Codex => vec![
            ("gpt-5-codex", "GPT-5 Codex: tuned for coding"),
            ("gpt-5", "GPT-5"),
            ("o3", "o3: deep reasoning"),
            ("o4-mini", "o4-mini: fast"),
        ],
        Kind::Gemini => vec![
            ("gemini-2.5-pro", "Gemini 2.5 Pro: the most capable"),
            ("gemini-2.5-flash", "Gemini 2.5 Flash: fast"),
            ("gemini-2.5-flash-lite", "Gemini 2.5 Flash-Lite: the cheapest"),
        ],
    }
}

/// What the agent reported when it started (the `init` line): its commands, and which ones only
/// work in its own terminal UI.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Meta {
    pub commands: Vec<String>,
    pub terminal_only: Vec<String>,
}

const TERMINAL_ONLY: &str = "only works in the agent's own terminal screen; run it there";

fn cmd(name: &str, desc: &str, hint: &str, origin: Origin, handling: Handling) -> Cmd {
    Cmd { name: name.into(), desc: desc.into(), hint: hint.into(), origin, handling }
}

/// The commands every agent has in the chat, and the ones Claude Code takes.
pub fn builtin(kind: Kind) -> Vec<Cmd> {
    let claude = kind == Kind::Claude;
    let nat = |n: &str, d: &str, h: &str, x: Native| cmd(n, d, h, Origin::Native, Handling::Native(x));
    let only_claude = |_: &str| format!("{} has no such command in its non-interactive mode", kind.name());
    // A command the agent runs: sent as typed for Claude Code, off for the others.
    let pass = |n: &str, d: &str, h: &str| {
        cmd(n, d, h, Origin::Agent, if claude { Handling::Pass } else { Handling::Off(only_claude(n)) })
    };
    let mut v = vec![
        if claude {
            nat("resume", "Continue an earlier conversation of this project", "", Native::Resume)
        } else {
            cmd(
                "resume",
                "Continue an earlier conversation",
                "",
                Origin::Native,
                Handling::Off(format!(
                    "{} keeps the conversation of this tab going by itself; picking an older one works with Claude Code",
                    kind.name()
                )),
            )
        },
        nat("clear", "Start a new conversation", "", Native::Clear),
        pass("compact", "Summarize the conversation to free context", "[instructions]"),
        nat("model", "Choose the model", "[name]", Native::Model),
        nat("help", "What the composer can do", "", Native::Help),
        nat("cost", "Tokens and cost of this conversation", "", Native::Cost),
        nat("status", "Agent, model, mode, folder and session", "", Native::Status),
        pass("context", "Show how full the context window is", ""),
        pass("usage", "Plan usage and limits", ""),
        pass("init", "Create a CLAUDE.md for this project", ""),
        pass("review", "Review the current changes", ""),
        pass("mcp", "MCP servers", ""),
        pass("rename", "Name this conversation", "[name]"),
        nat("copy", "Copy the last answer", "", Native::Copy),
    ];
    if claude {
        v.push(nat("config", "Lumen settings, or /config key=value for Claude Code", "[key=value]", Native::Config));
        v.push(nat("agents", "The sub-agents available here", "", Native::Agents));
        v.push(nat("memory", "Open CLAUDE.md and the memory files", "", Native::Memory));
        v.push(nat("permissions", "Choose what the agent may do without asking", "", Native::Permissions));
        v.push(nat("plan", "Plan mode: the agent proposes a plan before changing anything", "[request]", Native::Plan));
        for n in ["doctor", "login", "logout"] {
            v.push(cmd(n, "Terminal only", "", Origin::Agent, Handling::Off(TERMINAL_ONLY.into())));
        }
    } else {
        v.push(nat("config", "Lumen settings", "", Native::Config));
        v.push(nat("memory", "Open the project's instruction files", "", Native::Memory));
        let why = format!("permission modes are Claude Code's; {} runs with a fixed sandbox here", kind.name());
        v.push(cmd("permissions", "Permission mode", "", Origin::Native, Handling::Off(why.clone())));
        v.push(cmd("plan", "Plan mode", "", Origin::Native, Handling::Off(why)));
    }
    v
}

/// The value of `key:` in a Markdown file's `---` header.
fn front(text: &str, key: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?.trim_start_matches(['\r', '\n']);
    let end = rest.find("\n---")?;
    rest[..end].lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_string()).filter(|v| !v.is_empty())
    })
}

/// First line of the body (after the header), as a fallback description.
fn first_line(text: &str) -> String {
    let body = text.strip_prefix("---").and_then(|r| r.find("\n---").map(|i| &r[i + 4..])).unwrap_or(text);
    let l = body.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("");
    l.chars().take(90).collect()
}

fn collect_commands(dir: &Path, origin: Origin, depth: usize, out: &mut Vec<Cmd>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            if depth < 3 {
                collect_commands(&p, origin, depth + 1, out);
            }
            continue;
        }
        if p.extension().is_none_or(|x| x != "md") {
            continue;
        }
        let Some(name) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let desc = front(&text, "description").unwrap_or_else(|| first_line(&text));
        let hint = front(&text, "argument-hint").unwrap_or_default();
        out.push(cmd(&name, &desc, &hint, origin, Handling::Pass));
    }
}

fn collect_skills(dir: &Path, out: &mut Vec<Cmd>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(text) = std::fs::read_to_string(e.path().join("SKILL.md")) else { continue };
        let name = front(&text, "name").unwrap_or_else(|| e.file_name().to_string_lossy().into_owned());
        let desc = front(&text, "description").unwrap_or_else(|| first_line(&text));
        let hint = front(&text, "argument-hint").unwrap_or_default();
        out.push(cmd(&name, &desc, &hint, Origin::Skill, Handling::Pass));
    }
}

/// The custom commands and skills on disk for a project: `.claude/commands/**/*.md` and
/// `.claude/skills/*/SKILL.md` of the project first, then the user's under `~/.claude`.
pub fn discover(root: &Path, home: Option<&Path>) -> Vec<Cmd> {
    let mut v = Vec::new();
    collect_commands(&root.join(".claude/commands"), Origin::Project, 0, &mut v);
    collect_skills(&root.join(".claude/skills"), &mut v);
    if let Some(h) = home {
        collect_commands(&h.join(".claude/commands"), Origin::User, 0, &mut v);
        collect_skills(&h.join(".claude/skills"), &mut v);
    }
    v
}

/// The whole catalog of a session: Lumen's and the agent's own first, then what is on disk, then
/// whatever else the agent says it has (plugins' commands and skills). A name appears once.
pub fn catalog(kind: Kind, found: Vec<Cmd>, meta: &Meta) -> Vec<Cmd> {
    let mut v = builtin(kind);
    let add = |c: Cmd, v: &mut Vec<Cmd>| {
        if !v.iter().any(|o| o.name.eq_ignore_ascii_case(&c.name)) {
            v.push(c);
        }
    };
    if kind == Kind::Claude {
        for c in found {
            add(c, &mut v);
        }
        for n in &meta.commands {
            let off = meta.terminal_only.iter().any(|t| t == n);
            let handling = if off { Handling::Off(TERMINAL_ONLY.into()) } else { Handling::Pass };
            let origin = if n.contains(':') { Origin::Skill } else { Origin::Agent };
            add(cmd(n, "", "", origin, handling), &mut v);
        }
    }
    v
}

/// The command name being typed: `Some("re")` for `/re`, `None` once there is a space (the
/// arguments), or when the text is a path (`/usr/bin`) or not a command at all.
pub fn query(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    (!rest.contains(char::is_whitespace) && !rest.contains('/')).then_some(rest)
}

/// `/name args` split: the name and the rest (trimmed). `None` when the text is not a command.
pub fn parse(text: &str) -> Option<(&str, &str)> {
    let t = text.trim();
    let rest = t.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    (!name.is_empty() && !name.contains('/')).then_some((name, args.trim()))
}

pub fn find<'a>(cmds: &'a [Cmd], name: &str) -> Option<&'a Cmd> {
    cmds.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// The commands matching what is typed after the slash, best first: names starting with it, then
/// names containing it, then the ones whose letters appear in order, then description matches.
/// Inside a group Lumen's own come first, then the order of the catalog.
pub fn filter<'a>(cmds: &'a [Cmd], q: &str) -> Vec<&'a Cmd> {
    let q = q.to_lowercase();
    let mut scored: Vec<(u8, usize, &Cmd)> = Vec::new();
    for (i, c) in cmds.iter().enumerate() {
        let n = c.name.to_lowercase();
        let rank = if q.is_empty() || n.starts_with(&q) {
            0
        } else if n.contains(&q) {
            1
        } else if subsequence(&q, &n) {
            2
        } else if c.desc.to_lowercase().contains(&q) {
            3
        } else {
            continue;
        };
        scored.push((rank, i, c));
    }
    scored.sort_by_key(|(r, i, c)| (*r, !matches!(c.origin, Origin::Native), *i));
    scored.into_iter().map(|(_, _, c)| c).collect()
}

fn subsequence(q: &str, text: &str) -> bool {
    let mut it = text.chars();
    q.chars().all(|c| it.any(|t| t == c))
}

/// The text a command picked from the menu puts in the composer: its name, and a space when it
/// takes arguments.
pub fn completion(c: &Cmd) -> String {
    if c.hint.is_empty() { format!("/{}", c.name) } else { format!("/{} ", c.name) }
}

/// The `/help` text.
pub fn help_text(cmds: &[Cmd]) -> String {
    let mut s = String::from(
        "**Composer**\n\n- `Enter` sends, `Shift+Enter` adds a line, `Esc` stops the agent\n- `/` opens the commands, `@` mentions a file\n- `Shift+Tab` changes the permission mode\n- `Up` / `Down` in an empty box walk your earlier messages\n- Paste an image or drop a file to attach it\n\n**Commands**\n\n",
    );
    for c in cmds.iter().filter(|c| matches!(c.origin, Origin::Native | Origin::Agent)) {
        let h = if c.hint.is_empty() { String::new() } else { format!(" {}", c.hint) };
        match &c.handling {
            Handling::Off(why) => s.push_str(&format!("- `/{}{h}` is not available here: {why}\n", c.name)),
            _ => s.push_str(&format!("- `/{}{h}` {}\n", c.name, c.desc)),
        }
    }
    let custom = cmds.iter().filter(|c| matches!(c.origin, Origin::Project | Origin::User | Origin::Skill)).count();
    if custom > 0 {
        s.push_str(&format!(
            "\n{custom} more from your project, your `~/.claude` and plugins: type `/` to see them.\n"
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lumen-slash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn what_is_typed_is_a_command_name_only_until_the_first_space() {
        assert_eq!(query("/"), Some(""));
        assert_eq!(query("/res"), Some("res"));
        assert_eq!(query("/resume now"), None, "arguments: the menu closes");
        assert_eq!(query("/usr/bin/ls"), None, "a path is not a command");
        assert_eq!(query("hello"), None);
        assert_eq!(parse("/model opus"), Some(("model", "opus")));
        assert_eq!(parse("  /clear  "), Some(("clear", "")));
        assert_eq!(parse("/compact keep the API notes"), Some(("compact", "keep the API notes")));
        assert_eq!(parse("/src/main.rs"), None);
        assert_eq!(parse("text /x"), None);
    }

    #[test]
    fn claude_gets_its_commands_and_the_others_get_reasons() {
        let c = builtin(Kind::Claude);
        let h = |n: &str| find(&c, n).unwrap().handling.clone();
        assert_eq!(h("resume"), Handling::Native(Native::Resume));
        assert_eq!(h("compact"), Handling::Pass);
        assert_eq!(h("init"), Handling::Pass);
        assert_eq!(h("model"), Handling::Native(Native::Model));
        assert_eq!(h("memory"), Handling::Native(Native::Memory));
        assert!(matches!(h("doctor"), Handling::Off(_)));
        let g = builtin(Kind::Gemini);
        let Handling::Off(why) = &find(&g, "resume").unwrap().handling else { panic!("resume is off for Gemini") };
        assert!(why.contains("Gemini CLI"), "{why}");
        assert!(matches!(find(&g, "compact").unwrap().handling, Handling::Off(_)));
        assert_eq!(find(&g, "clear").unwrap().handling, Handling::Native(Native::Clear), "clear works everywhere");
        assert!(find(&g, "agents").is_none(), "sub-agents are Claude's");
    }

    #[test]
    fn commands_and_skills_on_disk_are_found_with_their_headers() {
        let root = tmp("disk");
        let home = tmp("home");
        let cmds = root.join(".claude/commands/frontend");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(
            root.join(".claude/commands/ship.md"),
            "---\ndescription: Ship it\nargument-hint: [version]\n---\nDo it\n",
        )
        .unwrap();
        std::fs::write(cmds.join("lint.md"), "# Lint the UI\nrun biome\n").unwrap();
        let sk = root.join(".claude/skills/deploy");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(sk.join("SKILL.md"), "---\nname: deploy-prod\ndescription: \"Deploy to production\"\n---\nbody")
            .unwrap();
        std::fs::create_dir_all(home.join(".claude/commands")).unwrap();
        std::fs::write(home.join(".claude/commands/notes.md"), "Take notes").unwrap();
        let v = discover(&root, Some(&home));
        let get = |n: &str| v.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("{n} in {v:?}"));
        assert_eq!((get("ship").desc.as_str(), get("ship").hint.as_str()), ("Ship it", "[version]"));
        assert_eq!(get("ship").origin, Origin::Project);
        assert_eq!(get("lint").desc, "Lint the UI", "no header: the first line");
        assert_eq!(get("deploy-prod").origin, Origin::Skill);
        assert_eq!(get("deploy-prod").desc, "Deploy to production");
        assert_eq!(get("notes").origin, Origin::User);
        assert!(v.iter().all(|c| c.handling == Handling::Pass));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_catalog_has_each_name_once_and_marks_terminal_only_ones() {
        let found = vec![
            cmd("ship", "Ship it", "", Origin::Project, Handling::Pass),
            cmd("init", "mine", "", Origin::Project, Handling::Pass),
        ];
        let meta = Meta {
            commands: vec!["ship".into(), "color".into(), "tray-api:setup".into(), "loop".into()],
            terminal_only: vec!["color".into()],
        };
        let c = catalog(Kind::Claude, found, &meta);
        assert_eq!(c.iter().filter(|x| x.name == "ship").count(), 1);
        assert_eq!(
            find(&c, "init").unwrap().origin,
            Origin::Agent,
            "the agent's own wins over a file of the same name"
        );
        assert!(matches!(find(&c, "color").unwrap().handling, Handling::Off(_)));
        assert_eq!(find(&c, "tray-api:setup").unwrap().origin, Origin::Skill);
        assert_eq!(find(&c, "loop").unwrap().handling, Handling::Pass);
        let g = catalog(Kind::Codex, vec![cmd("ship", "", "", Origin::Project, Handling::Pass)], &meta);
        assert!(find(&g, "ship").is_none(), "Claude's files are not Codex's commands");
    }

    #[test]
    fn autocomplete_ranks_prefix_then_substring_then_letters() {
        let c = builtin(Kind::Claude);
        let names = |q: &str| filter(&c, q).iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("res")[0], "resume");
        assert_eq!(names("cl")[0], "clear");
        let m = names("mo");
        assert_eq!(m[0], "model");
        assert!(names("rsm").contains(&"resume".to_string()), "letters in order");
        assert!(names("zzzz").is_empty());
        assert_eq!(names("").len(), c.len(), "a bare slash lists everything");
        assert!(names("window").contains(&"context".to_string()), "description match: {:?}", names("window"));
        // Lumen's own come first inside a group.
        let both = vec![
            cmd("review", "agent", "", Origin::Agent, Handling::Pass),
            cmd("rev", "lumen", "", Origin::Native, Handling::Native(Native::Help)),
            cmd("revert", "agent", "", Origin::Agent, Handling::Pass),
        ];
        let r: Vec<_> = filter(&both, "re").iter().map(|c| c.name.as_str()).collect();
        assert_eq!(r, ["rev", "review", "revert"]);
    }

    #[test]
    fn picking_a_command_completes_it() {
        let c = builtin(Kind::Claude);
        assert_eq!(completion(find(&c, "clear").unwrap()), "/clear");
        assert_eq!(completion(find(&c, "model").unwrap()), "/model ");
    }

    #[test]
    fn every_cli_lists_models_and_claudes_are_its_documented_aliases() {
        let c: Vec<_> = models(Kind::Claude).iter().map(|m| m.0).collect();
        assert!(c.contains(&"opus") && c.contains(&"sonnet") && c.contains(&"opusplan") && c.contains(&"default"));
        assert!(models(Kind::Codex).len() >= 2 && models(Kind::Gemini).iter().all(|m| m.0.starts_with("gemini-")));
    }

    #[test]
    fn help_lists_what_works_and_why_the_rest_does_not() {
        let t = help_text(&builtin(Kind::Gemini));
        assert!(t.contains("`/clear`") && t.contains("`/resume` is not available here"), "{t}");
        assert!(t.contains("`Shift+Tab`"));
    }
}
