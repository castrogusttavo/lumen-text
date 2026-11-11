//! The agent CLIs the AI panel can host, and finding them on the machine. Lumen only starts the
//! user's own installed CLI (which uses its own stored login); it never handles keys or tokens.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Agent {
    /// Stable id (remembered per project).
    pub id: &'static str,
    pub name: &'static str,
    /// Executable name looked up on PATH.
    pub bin: &'static str,
    pub icon: &'static str,
    /// How to install it.
    pub install: &'static str,
}

pub const AGENTS: &[Agent] = &[
    Agent {
        id: "claude",
        name: "Claude Code",
        bin: "claude",
        icon: "ui/ai_claude",
        install: "npm i -g @anthropic-ai/claude-code",
    },
    Agent {
        id: "gemini",
        name: "Gemini CLI",
        bin: "gemini",
        icon: "ui/ai_gemini",
        install: "npm i -g @google/gemini-cli",
    },
    Agent { id: "codex", name: "Codex CLI", bin: "codex", icon: "ui/ai_open_ai", install: "npm i -g @openai/codex" },
];

pub fn agent(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

/// Where to look for agent executables: PATH first, then the per-user install folders a desktop
/// launch often lacks on its PATH (npm prefixes, nvm / volta / bun, Claude's local install).
pub fn search_dirs(path: Option<&OsStr>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    if let Some(h) = home {
        for sub in [".local/bin", ".claude/local", ".npm-global/bin", ".npm/bin", ".bun/bin", ".volta/bin", "bin"] {
            dirs.push(h.join(sub));
        }
        // nvm: every installed node version, newest name last (so the newest is tried last but
        // still found when it is the only one with the CLI).
        if let Ok(rd) = std::fs::read_dir(h.join(".nvm/versions/node")) {
            let mut vers: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
            vers.sort();
            dirs.extend(vers.into_iter().rev());
        }
        if cfg!(windows) {
            dirs.push(h.join("AppData/Roaming/npm"));
        }
    }
    if cfg!(unix) {
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    dirs
}

#[cfg(unix)]
fn exec_bit(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn exec_bit(_: &std::fs::Metadata) -> bool {
    true
}

/// Is `p` a file we may run?
fn runnable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && exec_bit(&m))
}

/// The first runnable `bin` in `dirs` (on Windows also `bin.exe` / `bin.cmd`, npm's shims).
pub fn find_in(dirs: &[PathBuf], bin: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{bin}.exe"), format!("{bin}.cmd"), format!("{bin}.bat"), bin.to_string()]
    } else {
        vec![bin.to_string()]
    };
    dirs.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| runnable(p))
}

/// Every known agent with the executable found for it (if any).
pub fn detect(dirs: &[PathBuf]) -> Vec<(&'static Agent, Option<PathBuf>)> {
    AGENTS.iter().map(|a| (a, find_in(dirs, a.bin))).collect()
}

/// Program and arguments that run the executable at `path` (Windows `.cmd` shims go through cmd).
pub fn command_for(path: &Path) -> (String, Vec<String>) {
    let p = path.to_string_lossy().into_owned();
    let shim = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if cfg!(windows) && shim { ("cmd.exe".to_string(), vec!["/c".to_string(), p]) } else { (p, Vec::new()) }
}

/// PATH for the agent: its own folder first (so `#!/usr/bin/env node` finds the node it was
/// installed with), then the user's PATH.
pub fn path_with(dir: Option<&Path>, path: Option<&OsStr>) -> Option<String> {
    let mut parts: Vec<PathBuf> = dir.map(Path::to_path_buf).into_iter().collect();
    if let Some(p) = path {
        parts.extend(std::env::split_paths(p).filter(|d| Some(d.as_path()) != dir));
    }
    std::env::join_paths(parts).ok().map(|s| s.to_string_lossy().into_owned())
}

/// Where the agent chosen for a project is remembered (`None` in tests: nothing is written).
pub fn state_path(root: &Path) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let base = crate::paths::state_home()?;
    let h =
        root.to_string_lossy().bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    Some(base.join("lumen-text").join(format!("ai-{h:016x}.json")))
}

pub fn load_choice(path: &Path) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?).ok()?;
    v["agent"].as_str().map(str::to_string)
}

pub fn save_choice(path: &Path, id: &str) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(path, serde_json::json!({ "agent": id }).to_string());
}
