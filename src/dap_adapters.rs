//! Debug-adapter registry: which DAP adapters exist, which languages they debug and how to start
//! them. The built-in table is `dap_adapters.json` (embedded); it is parsed only when a debug
//! session is requested, so it costs nothing at startup. Pure data plus process spawning; the
//! protocol itself lives in `dap`.
#![allow(dead_code)]

use crate::dap::{Dap, Session, Wake};
use serde::Deserialize;
use serde_json::Value;
use std::{
    io,
    path::{Path, PathBuf},
};

const BUILTIN: &str = include_str!("dap_adapters.json");

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    #[default]
    Stdio,
    /// The adapter listens on a TCP port and prints `listening at: host:port`.
    TcpListen,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Adapter {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub transport: Transport,
    pub languages: Vec<String>,
    #[serde(default)]
    pub adapter_id: String,
    /// Launch request: `request` (`launch`/`attach`) plus its arguments, with `${file}` placeholders.
    pub launch: Value,
    #[serde(default)]
    pub install: String,
    #[serde(default)]
    pub experimental: bool,
    /// A command that must succeed before the adapter starts (e.g. `ghp build --debug`).
    #[serde(default)]
    pub prelaunch: Option<Prelaunch>,
    /// Normalise the source paths the adapter reports and the editor sends (GHP pages).
    #[serde(default)]
    pub normalize_paths: bool,
}

/// A build step run in the project root before the debuggee is launched. `${file}`, `${dir}` and
/// `${root}` are replaced in `args`.
#[derive(Clone, Debug, Deserialize)]
pub struct Prelaunch {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub install: String,
}

#[derive(Deserialize)]
struct Table {
    adapters: Vec<Adapter>,
}

pub struct Registry {
    pub adapters: Vec<Adapter>,
}

impl Registry {
    pub fn builtin() -> Registry {
        // The embedded table is checked by a unit test, so a parse failure cannot ship.
        let t: Table = serde_json::from_str(BUILTIN).unwrap_or(Table { adapters: Vec::new() });
        Registry { adapters: t.adapters }
    }

    /// The first non-experimental adapter for `language` (editor language id, e.g. `go`).
    pub fn for_language(&self, language: &str) -> Option<&Adapter> {
        self.adapters.iter().find(|a| !a.experimental && a.languages.iter().any(|l| l == language))
    }

    pub fn get(&self, id: &str) -> Option<&Adapter> {
        self.adapters.iter().find(|a| a.id == id)
    }
}

fn substitute(v: &Value, file: &Path, root: &Path) -> Value {
    let dir = file.parent().unwrap_or(root);
    match v {
        Value::String(s) => Value::String(
            s.replace("${file}", &file.to_string_lossy())
                .replace("${dir}", &dir.to_string_lossy())
                .replace("${root}", &root.to_string_lossy()),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, file, root)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), substitute(x, file, root))).collect()),
        other => other.clone(),
    }
}

impl Adapter {
    /// Absolute path of the adapter executable, or `None` when it is not installed.
    pub fn resolve(&self, root: &Path) -> Option<PathBuf> {
        crate::lspreg::resolve_command(&self.command, root)
    }

    /// `(request, arguments)` of the launch/attach request for debugging `file`.
    pub fn launch_request(&self, file: &Path, root: &Path) -> (String, Value) {
        let mut args = substitute(&self.launch, file, root);
        let request = args
            .as_object_mut()
            .and_then(|o| o.remove("request"))
            .and_then(|r| r.as_str().map(str::to_string))
            .unwrap_or_else(|| "launch".into());
        (request, args)
    }

    /// A session for debugging `file` (call `Session::start` after adding breakpoints).
    pub fn session(&self, file: &Path, root: &Path) -> Session {
        self.session_with_args(file, root, &[])
    }

    /// `session` with program arguments (`args` of the launch request) set, e.g. a listen address.
    pub fn session_with_args(&self, file: &Path, root: &Path, program_args: &[String]) -> Session {
        let (request, mut args) = self.launch_request(file, root);
        if !program_args.is_empty()
            && let Some(o) = args.as_object_mut()
        {
            o.insert("args".into(), serde_json::json!(program_args));
        }
        let mut s = Session::with_request(&request, args);
        s.exception_bps = false;
        if self.normalize_paths {
            s.source_root = Some(root.to_path_buf());
        }
        s
    }

    /// Run the prelaunch step, if any, and fail with its output when it does not succeed.
    pub fn run_prelaunch(&self, file: &Path, root: &Path) -> io::Result<()> {
        let Some(p) = &self.prelaunch else { return Ok(()) };
        let exe = crate::lspreg::resolve_command(&p.command, root).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} not found{}",
                    p.command,
                    if p.install.is_empty() { String::new() } else { format!(" (install: {})", p.install) }
                ),
            )
        })?;
        let args: Vec<String> = p
            .args
            .iter()
            .map(|a| a.replace("${file}", &file.to_string_lossy()).replace("${root}", &root.to_string_lossy()))
            .collect();
        let out = std::process::Command::new(exe)
            .args(&args)
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .output()?;
        if out.status.success() {
            return Ok(());
        }
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let tail: Vec<&str> = text.lines().rev().take(20).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        Err(io::Error::other(format!("`{} {}` failed ({}):\n{tail}", p.command, args.join(" "), out.status)))
    }

    /// Start the adapter process and connect to it.
    pub fn start(&self, root: &Path, wake: Wake) -> io::Result<Dap> {
        let exe = self.resolve(root).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} not found{}",
                    self.command,
                    if self.install.is_empty() { String::new() } else { format!(" (install: {})", self.install) }
                ),
            )
        })?;
        let exe = exe.to_string_lossy();
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let mut dap = match self.transport {
            Transport::Stdio => Dap::spawn(&exe, &args, root, wake)?,
            Transport::TcpListen => Dap::spawn_listening(&exe, &args, root, wake)?,
        };
        dap.adapter_id = if self.adapter_id.is_empty() { self.id.clone() } else { self.adapter_id.clone() };
        Ok(dap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses_with_delve_and_ghp() {
        let r = Registry::builtin();
        assert!(r.get("delve").is_some() && r.get("ghp").is_some());
        let d = r.for_language("go").expect("go adapter");
        assert_eq!(d.id, "delve");
        assert_eq!(d.transport, Transport::TcpListen);
        let g = r.for_language("ghp").expect("ghp adapter");
        assert_eq!(g.id, "ghp");
        assert_eq!(g.command, "dlv", "GHP pages are debugged by delve, on a binary built by ghp");
        assert!(g.normalize_paths);
        let pre = g.prelaunch.as_ref().expect("ghp builds before launching");
        assert_eq!((pre.command.as_str(), pre.args[0].as_str(), pre.args[1].as_str()), ("ghp", "build", "--debug"));
    }

    #[test]
    fn ghp_launch_execs_the_debug_binary() {
        let r = Registry::builtin();
        let (req, args) = r.get("ghp").unwrap().launch_request(Path::new("/p/pages/index.ghp"), Path::new("/p"));
        assert_eq!(req, "launch");
        assert_eq!(args["mode"], "exec");
        assert_eq!(args["program"], "/p/.lumen/ghp-debug/app");
        assert_eq!(args["cwd"], "/p");
        let s = r.get("ghp").unwrap().session_with_args(
            Path::new("/p/index.ghp"),
            Path::new("/p"),
            &["-addr=127.0.0.1:9".to_string()],
        );
        assert_eq!(s.start_request.unwrap().1["args"][0], "-addr=127.0.0.1:9");
    }

    #[test]
    fn prelaunch_reports_a_missing_command_and_failures() {
        let mut a = Registry::builtin().get("ghp").unwrap().clone();
        a.prelaunch =
            Some(Prelaunch { command: "no-such-ghp-binary".into(), args: vec![], install: "go build".into() });
        let e = a.run_prelaunch(Path::new("/p/a.ghp"), Path::new("/")).unwrap_err();
        assert!(e.to_string().contains("not found (install: go build)"), "{e}");
        a.prelaunch = None;
        assert!(a.run_prelaunch(Path::new("/p/a.ghp"), Path::new("/")).is_ok());
        #[cfg(unix)]
        {
            a.prelaunch = Some(Prelaunch { command: "false".into(), args: vec![], install: String::new() });
            let e = a.run_prelaunch(Path::new("/p/a.ghp"), Path::new("/")).unwrap_err();
            assert!(e.to_string().contains("failed"), "{e}");
        }
    }

    #[test]
    fn launch_request_substitutes_placeholders() {
        let r = Registry::builtin();
        let (req, args) = r.get("delve").unwrap().launch_request(Path::new("/p/cmd/main.go"), Path::new("/p"));
        assert_eq!(req, "launch");
        assert_eq!(args["program"], "/p/cmd");
        assert_eq!(args["cwd"], "/p");
        assert!(args.get("request").is_none());
    }
}
