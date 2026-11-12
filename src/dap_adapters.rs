//! Debug-adapter registry: which DAP adapters exist, which languages they debug and how to start
//! them. The built-in table is `dap_adapters.json` (embedded); it is parsed only when a debug
//! session is requested, so it costs nothing at startup. Pure data plus process spawning; the
//! protocol itself lives in `dap`.
#![allow(dead_code)]

use crate::dap::{Dap, Session, Wake};
use crate::debug_model::LaunchOverride;
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

/// Where `ghp build --debug` writes the program the `ghp` adapter debugs: `<root>/.lumen/ghp-debug/app`,
/// with the platform's executable suffix (`.exe` on Windows) and native path separators.
pub fn debug_binary(root: &Path) -> PathBuf {
    root.join(".lumen").join("ghp-debug").join(format!("app{}", std::env::consts::EXE_SUFFIX))
}

/// Replace `${file}`, `${dir}`, `${root}` and `${debug_bin}` (see [`debug_binary`]) in `s`.
fn substitute_str(s: &str, file: &Path, root: &Path) -> String {
    let dir = file.parent().unwrap_or(root);
    s.replace("${file}", &file.to_string_lossy())
        .replace("${dir}", &dir.to_string_lossy())
        .replace("${root}", &root.to_string_lossy())
        .replace("${debug_bin}", &debug_binary(root).to_string_lossy())
}

fn substitute(v: &Value, file: &Path, root: &Path) -> Value {
    match v {
        Value::String(s) => Value::String(substitute_str(s, file, root)),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, file, root)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), substitute(x, file, root))).collect()),
        other => other.clone(),
    }
}

/// A path from `launch.json` (placeholders replaced); relative ones are taken from the project root.
fn project_path(s: &str, file: &Path, root: &Path) -> String {
    let s = substitute_str(s, file, root);
    if Path::new(&s).is_relative() { root.join(&s).to_string_lossy().into_owned() } else { s }
}

impl Adapter {
    /// Absolute path of the adapter executable, or `None` when it is not installed.
    pub fn resolve(&self, root: &Path) -> Option<PathBuf> {
        crate::lspreg::resolve_command(&self.command, root)
    }

    /// `(request, arguments)` of the launch/attach request for debugging `file`.
    pub fn launch_request(&self, file: &Path, root: &Path) -> (String, Value) {
        self.launch_request_with(file, root, None)
    }

    /// `launch_request` with the project's `.lumen/launch.json` entry applied (program, args, env, cwd).
    pub fn launch_request_with(&self, file: &Path, root: &Path, ov: Option<&LaunchOverride>) -> (String, Value) {
        let mut args = substitute(&self.launch, file, root);
        let request = args
            .as_object_mut()
            .and_then(|o| o.remove("request"))
            .and_then(|r| r.as_str().map(str::to_string))
            .unwrap_or_else(|| "launch".into());
        if let (Some(ov), Some(o)) = (ov, args.as_object_mut()) {
            if let Some(p) = &ov.program {
                o.insert("program".into(), Value::String(project_path(p, file, root)));
            }
            if let Some(a) = &ov.args {
                let list = a.iter().map(|x| Value::String(substitute_str(x, file, root)));
                o.insert("args".into(), Value::Array(list.collect()));
            }
            if !ov.env.is_empty() {
                let env = ov.env.iter().map(|(k, v)| (k.clone(), Value::String(substitute_str(v, file, root))));
                o.insert("env".into(), Value::Object(env.collect()));
            }
            if let Some(c) = &ov.cwd {
                o.insert("cwd".into(), Value::String(project_path(c, file, root)));
            }
        }
        (request, args)
    }

    /// A session for debugging `file` (call `Session::start` after adding breakpoints).
    pub fn session(&self, file: &Path, root: &Path) -> Session {
        self.session_with_args(file, root, &[])
    }

    /// `session` with program arguments (`args` of the launch request) set, e.g. a listen address.
    pub fn session_with_args(&self, file: &Path, root: &Path, program_args: &[String]) -> Session {
        self.session_with(file, root, program_args, None)
    }

    /// `session_with_args` with the project's `launch.json` entry applied; `program_args` win over its `args`.
    pub fn session_with(
        &self,
        file: &Path,
        root: &Path,
        program_args: &[String],
        ov: Option<&LaunchOverride>,
    ) -> Session {
        let (request, mut args) = self.launch_request_with(file, root, ov);
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
        self.run_prelaunch_with(file, root, None)
    }

    /// `run_prelaunch`; a `program` override of `launch.json` becomes the build's output too.
    pub fn run_prelaunch_with(&self, file: &Path, root: &Path, ov: Option<&LaunchOverride>) -> io::Result<()> {
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
        let default_prog = self.launch.get("program").and_then(Value::as_str).map(|s| substitute_str(s, file, root));
        let program = ov.and_then(|o| o.program.as_deref()).map(|s| project_path(s, file, root));
        let args: Vec<String> = p
            .args
            .iter()
            .map(|a| {
                let a = substitute_str(a, file, root);
                match (&default_prog, &program) {
                    (Some(d), Some(n)) if !d.is_empty() => a.replace(d.as_str(), n),
                    _ => a,
                }
            })
            .collect();
        if let Some(dir) = debug_binary(root).parent() {
            // The output folder keeps itself out of git: a `*` .gitignore inside it.
            if std::fs::create_dir_all(dir).is_ok() && !dir.join(".gitignore").exists() {
                let _ = std::fs::write(dir.join(".gitignore"), "*\n");
            }
        }
        if let Some(dir) = program.as_deref().and_then(|p| Path::new(p).parent()) {
            let _ = std::fs::create_dir_all(dir);
        }
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
        let want = debug_binary(Path::new("/p")).to_string_lossy().into_owned();
        assert_eq!(args["program"], want.as_str());
        assert!(want.ends_with(&format!("app{}", std::env::consts::EXE_SUFFIX)), "{want}");
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

    #[test]
    fn ghp_binary_has_the_platform_suffix_and_separators() {
        let b = debug_binary(Path::new("/p"));
        let dir: PathBuf = ["/p", ".lumen", "ghp-debug"].iter().collect();
        assert_eq!(b, dir.join(format!("app{}", std::env::consts::EXE_SUFFIX)));
        let g = Registry::builtin().get("ghp").unwrap().clone();
        let pre = g.prelaunch.as_ref().unwrap();
        let out = substitute_str(&pre.args[3], Path::new("/p/a.ghp"), Path::new("/p"));
        assert_eq!(out, b.to_string_lossy(), "the build writes where the debugger execs");
    }

    #[test]
    fn launch_json_overrides_program_args_env_cwd() {
        let g = Registry::builtin().get("ghp").unwrap().clone();
        let ov = LaunchOverride {
            config: "ghp".into(),
            program: Some("out/site".into()),
            args: Some(vec!["-addr=:${root}".into()]),
            env: [("PORT".to_string(), "8081".to_string())].into(),
            cwd: Some("web".into()),
            ..Default::default()
        };
        let (_, a) = g.launch_request_with(Path::new("/p/i.ghp"), Path::new("/p"), Some(&ov));
        assert_eq!(Path::new(a["program"].as_str().unwrap()), Path::new("/p").join("out/site"));
        assert_eq!(a["args"][0], "-addr=:/p");
        assert_eq!(a["env"]["PORT"], "8081");
        assert_eq!(Path::new(a["cwd"].as_str().unwrap()), Path::new("/p").join("web"));
        let (_, plain) = g.launch_request_with(Path::new("/p/i.ghp"), Path::new("/p"), None);
        assert!(plain.get("env").is_none());
    }
}
