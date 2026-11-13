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
    /// Run the adapter with the project's Python (detected virtualenv, see `pyenv`), not a `PATH` command.
    #[serde(default)]
    pub python: bool,
    /// Arguments run with the resolved command before starting; a failure means the adapter's module
    /// is not installed (`python -c "import debugpy"`).
    #[serde(default)]
    pub probe: Vec<String>,
    /// Adapters tried when this one's command is not installed (`lldb-dap`, then `codelldb`).
    #[serde(default)]
    pub fallback: Vec<String>,
    /// The request template for `"request": "attach"` in launch.json; none means no attach support.
    #[serde(default)]
    pub attach: Option<Value>,
    /// The attach argument that carries a process id (`processId` for debugpy, `pid` for LLDB).
    #[serde(default)]
    pub pid_key: String,
    /// The launch `program` must exist (a clear error instead of the adapter's own).
    #[serde(default)]
    pub check_program: bool,
    /// The adapter wants `env` as `KEY=VALUE` strings (lldb-dap).
    #[serde(default)]
    pub env_list: bool,
}

/// A build step run in the project root before the debuggee is launched. `${file}`, `${dir}` and
/// `${root}` are replaced in `args`.
#[derive(Clone, Debug, Deserialize)]
pub struct Prelaunch {
    /// `cargo`: the build prints `--message-format=json` and the binary it reports is what is debugged.
    #[serde(default)]
    pub kind: String,
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

    /// Adapter `id`, or the first of its `fallback`s whose command is installed (auto-detection of
    /// `lldb-dap` vs `codelldb`); the adapter itself when none is, so its install hint is shown.
    pub fn installed(&self, id: &str, root: &Path) -> Option<&Adapter> {
        let first = self.get(id)?;
        std::iter::once(first)
            .chain(first.fallback.iter().filter_map(|f| self.get(f)))
            .find(|a| a.resolve(root).is_some())
            .or(Some(first))
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
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    s.replace("${file}", &file.to_string_lossy())
        .replace("${fileStem}", &stem)
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
        if self.python {
            // The project's virtualenv first, else whatever Python the PATH has.
            if let Some(i) = crate::pyenv::active(root).filter(|i| i.python.is_file()) {
                return Some(i.python);
            }
            return crate::lspreg::resolve_command("python3", root)
                .or_else(|| crate::lspreg::resolve_command("python", root));
        }
        crate::lspreg::resolve_command(&self.command, root)
    }

    pub fn supports_attach(&self) -> bool {
        self.attach.is_some()
    }

    /// `(request, arguments)` of the launch/attach request for debugging `file`.
    pub fn launch_request(&self, file: &Path, root: &Path) -> (String, Value) {
        self.launch_request_with(file, root, None)
    }

    /// `launch_request` with the project's `.lumen/launch.json` entry applied (program, args, env, cwd;
    /// `request: attach` with host/port/pid for adapters that have an attach template).
    pub fn launch_request_with(&self, file: &Path, root: &Path, ov: Option<&LaunchOverride>) -> (String, Value) {
        let attach = ov.is_some_and(|o| o.request.as_deref() == Some("attach"));
        let template = match (&self.attach, attach) {
            (Some(a), true) => a,
            _ => &self.launch,
        };
        let mut args = substitute(template, file, root);
        let request = args
            .as_object_mut()
            .and_then(|o| o.remove("request"))
            .and_then(|r| r.as_str().map(str::to_string))
            .unwrap_or_else(|| "launch".into());
        if let (Some(ov), Some(o)) = (ov, args.as_object_mut()) {
            let remote = o.contains_key("connect");
            if let Some(p) = ov.program.as_ref().filter(|_| !remote) {
                o.insert("program".into(), Value::String(project_path(p, file, root)));
            }
            if let Some(a) = ov.args.as_ref().filter(|_| !remote) {
                let list = a.iter().map(|x| Value::String(substitute_str(x, file, root)));
                o.insert("args".into(), Value::Array(list.collect()));
            }
            if !ov.env.is_empty() && !remote {
                let env = ov.env.iter().map(|(k, v)| (k.clone(), substitute_str(v, file, root)));
                let env = if self.env_list {
                    Value::Array(env.map(|(k, v)| Value::String(format!("{k}={v}"))).collect())
                } else {
                    Value::Object(env.map(|(k, v)| (k, Value::String(v))).collect())
                };
                o.insert("env".into(), env);
            }
            if let Some(c) = &ov.cwd {
                o.insert("cwd".into(), Value::String(project_path(c, file, root)));
            }
            if attach {
                if let Some(Value::Object(c)) = o.get_mut("connect") {
                    if let Some(h) = &ov.host {
                        c.insert("host".into(), Value::String(h.clone()));
                    }
                    if let Some(p) = ov.port {
                        c.insert("port".into(), Value::from(p));
                    }
                }
                if let (Some(pid), false) = (ov.pid, self.pid_key.is_empty()) {
                    o.insert(self.pid_key.clone(), Value::from(pid));
                }
            }
        }
        (request, args)
    }

    /// Why the launch cannot work, found before the adapter is started: a `program` that does not exist
    /// (when the adapter asks for the check) or attach without an attach template.
    pub fn preflight(&self, file: &Path, root: &Path, ov: Option<&LaunchOverride>) -> Result<(), String> {
        if ov.is_some_and(|o| o.request.as_deref() == Some("attach")) && self.attach.is_none() {
            return Err(format!("{} cannot attach (\"request\": \"attach\" in launch.json)", self.id));
        }
        let (request, args) = self.launch_request_with(file, root, ov);
        if self.check_program
            && request == "launch"
            && let Some(p) = args.get("program").and_then(Value::as_str)
            && !Path::new(p).exists()
        {
            return Err(format!(
                "program not found: {p} (build it first, or set \"program\" for \"{}\" in .lumen/launch.json)",
                self.id
            ));
        }
        Ok(())
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
        self.run_prelaunch_program(file, root, ov).map(|_| ())
    }

    /// Run the prelaunch step. A `cargo` step returns the binary cargo built (the one of `file`'s package
    /// when several), to be launched as `program`.
    pub fn run_prelaunch_program(
        &self,
        file: &Path,
        root: &Path,
        ov: Option<&LaunchOverride>,
    ) -> io::Result<Option<PathBuf>> {
        let Some(p) = &self.prelaunch else { return Ok(None) };
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
        let cargo = p.kind == "cargo";
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
        if !cargo && let Some(dir) = debug_binary(root).parent() {
            // The output folder keeps itself out of git: a `*` .gitignore inside it.
            if std::fs::create_dir_all(dir).is_ok() && !dir.join(".gitignore").exists() {
                let _ = std::fs::write(dir.join(".gitignore"), "*\n");
            }
        }
        if !cargo && let Some(dir) = program.as_deref().and_then(|p| Path::new(p).parent()) {
            let _ = std::fs::create_dir_all(dir);
        }
        let cwd = if cargo { cargo_root(file, root) } else { root.to_path_buf() };
        let out = std::process::Command::new(exe)
            .args(&args)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if out.status.success() {
            if !cargo {
                return Ok(None);
            }
            return match cargo_executable(&stdout, file) {
                Some(bin) => Ok(Some(bin)),
                None if program.is_some() => Ok(None),
                None => Err(io::Error::other(
                    "cargo built no executable (add a [[bin]] target, or set \"program\" in .lumen/launch.json)",
                )),
            };
        }
        let mut text = if cargo { cargo_errors(&stdout).join("\n") } else { stdout.into_owned() };
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let tail: Vec<&str> = text.lines().rev().take(20).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        Err(io::Error::other(format!("`{} {}` failed ({}):\n{tail}", p.command, args.join(" "), out.status)))
    }

    fn name_or_id(&self) -> &str {
        if self.name.is_empty() { &self.id } else { &self.name }
    }

    fn install_hint(&self) -> String {
        if self.install.is_empty() { String::new() } else { format!(" (install: {})", self.install) }
    }

    /// Start the adapter process and connect to it.
    pub fn start(&self, root: &Path, wake: Wake) -> io::Result<Dap> {
        let exe = self.resolve(root).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{} not found{}", self.command, self.install_hint()))
        })?;
        if !self.probe.is_empty() {
            let ok = std::process::Command::new(&exe)
                .args(&self.probe)
                .current_dir(root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} is not set up for {}{}", self.name_or_id(), exe.display(), self.install_hint()),
                ));
            }
        }
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

/// The folder `cargo build` runs in for `file`: the nearest ancestor with a `Cargo.toml`, else the project root.
pub fn cargo_root(file: &Path, root: &Path) -> PathBuf {
    file.ancestors().skip(1).find(|d| d.join("Cargo.toml").is_file()).unwrap_or(root).to_path_buf()
}

/// The executable to debug from the JSON lines of `cargo build --message-format=json`: bin and example
/// artifacts that have an executable (never test harnesses). The one whose source is `file` wins, then one
/// of the package `file` is in, then the first.
pub fn cargo_executable(stdout: &str, file: &Path) -> Option<PathBuf> {
    let mut best: Option<(u8, PathBuf)> = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] == true {
            continue;
        }
        let Some(exe) = v["executable"].as_str() else { continue };
        let kinds = v["target"]["kind"].as_array().map(Vec::as_slice).unwrap_or_default();
        if !kinds.iter().any(|k| k == "bin" || k == "example") {
            continue;
        }
        let src = v["target"]["src_path"].as_str().map(Path::new);
        let pkg = v["manifest_path"].as_str().and_then(|m| Path::new(m).parent());
        let rank = if src == Some(file) {
            3
        } else if pkg.is_some_and(|p| file.starts_with(p)) {
            2
        } else {
            1
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, PathBuf::from(exe)));
        }
    }
    best.map(|b| b.1)
}

/// The compiler errors (rendered, as `rustc` prints them) of `cargo build --message-format=json`.
pub fn cargo_errors(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["reason"] == "compiler-message" && v["message"]["level"] == "error")
        .filter_map(|v| v["message"]["rendered"].as_str().map(|s| s.trim_end().to_string()))
        .collect()
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
        a.prelaunch = Some(Prelaunch {
            kind: String::new(),
            command: "no-such-ghp-binary".into(),
            args: vec![],
            install: "go build".into(),
        });
        let e = a.run_prelaunch(Path::new("/p/a.ghp"), Path::new("/")).unwrap_err();
        assert!(e.to_string().contains("not found (install: go build)"), "{e}");
        a.prelaunch = None;
        assert!(a.run_prelaunch(Path::new("/p/a.ghp"), Path::new("/")).is_ok());
        #[cfg(unix)]
        {
            a.prelaunch =
                Some(Prelaunch { kind: String::new(), command: "false".into(), args: vec![], install: String::new() });
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

    #[test]
    fn python_rust_and_c_adapters_are_registered() {
        let r = Registry::builtin();
        assert_eq!(r.for_language("python").unwrap().id, "debugpy");
        assert_eq!(r.for_language("rust").unwrap().id, "lldb-rust");
        assert_eq!(r.for_language("c").unwrap().id, "lldb-dap");
        assert_eq!(r.for_language("cpp").unwrap().id, "lldb-dap");
        let d = r.get("debugpy").unwrap();
        assert!(d.python && d.args == ["-m", "debugpy.adapter"] && d.transport == Transport::Stdio);
        assert!(d.install.contains("pip install debugpy"));
        for id in ["codelldb", "codelldb-rust"] {
            let c = r.get(id).unwrap();
            assert!(c.experimental, "{id} is only used as a fallback");
            assert_eq!(
                (c.transport, c.args.as_slice()),
                (Transport::TcpListen, ["--port".to_string(), "0".to_string()].as_slice())
            );
        }
        assert_eq!(r.get("lldb-dap").unwrap().fallback, ["codelldb"]);
        assert_eq!(r.get("lldb-rust").unwrap().prelaunch.as_ref().unwrap().kind, "cargo");
    }

    #[test]
    fn debugpy_launch_runs_the_file_and_attach_connects() {
        let a = Registry::builtin().get("debugpy").unwrap().clone();
        let (req, args) = a.launch_request(Path::new("/p/app/main.py"), Path::new("/p"));
        assert_eq!(req, "launch");
        assert_eq!((args["program"].as_str(), args["cwd"].as_str()), (Some("/p/app/main.py"), Some("/p")));
        assert_eq!(args["console"], "internalConsole");
        let ov = LaunchOverride {
            config: "debugpy".into(),
            request: Some("attach".into()),
            host: Some("10.0.0.5".into()),
            port: Some(6000),
            program: Some("ignored.py".into()),
            ..Default::default()
        };
        let (req, args) = a.launch_request_with(Path::new("/p/main.py"), Path::new("/p"), Some(&ov));
        assert_eq!(req, "attach");
        assert_eq!(
            (args["connect"]["host"].as_str(), args["connect"]["port"].as_u64()),
            (Some("10.0.0.5"), Some(6000))
        );
        assert!(args.get("program").is_none(), "a remote attach has no program");
        let pid = LaunchOverride { request: Some("attach".into()), pid: Some(77), ..Default::default() };
        let (_, args) = a.launch_request_with(Path::new("/p/main.py"), Path::new("/p"), Some(&pid));
        assert_eq!(args["processId"], 77);
        assert_eq!(args["connect"]["port"], 5678, "default debugpy port");
    }

    #[test]
    fn lldb_launch_defaults_attach_and_env() {
        let r = Registry::builtin();
        let c = r.get("lldb-dap").unwrap();
        let (req, args) = c.launch_request(Path::new("/p/src/hello.c"), Path::new("/p"));
        assert_eq!(req, "launch");
        assert_eq!(args["program"], "/p/src/hello", "the binary next to the source, named after it");
        let ov = LaunchOverride {
            env: [("A".to_string(), "1".to_string())].into(),
            args: Some(vec!["x".into()]),
            ..Default::default()
        };
        let (_, args) = c.launch_request_with(Path::new("/p/hello.c"), Path::new("/p"), Some(&ov));
        assert_eq!(args["env"], serde_json::json!(["A=1"]), "lldb-dap wants KEY=VALUE strings");
        assert_eq!(args["args"][0], "x");
        let (_, args) =
            r.get("codelldb").unwrap().launch_request_with(Path::new("/p/hello.c"), Path::new("/p"), Some(&ov));
        assert_eq!(args["env"]["A"], "1", "CodeLLDB takes an object");
        let at = LaunchOverride { request: Some("attach".into()), pid: Some(9), ..Default::default() };
        let (req, args) = c.launch_request_with(Path::new("/p/hello.c"), Path::new("/p"), Some(&at));
        assert_eq!((req.as_str(), args["pid"].as_i64()), ("attach", Some(9)));
    }

    #[test]
    fn preflight_reports_a_missing_program_and_unsupported_attach() {
        let r = Registry::builtin();
        let err = r.get("lldb-dap").unwrap().preflight(Path::new("/nope/a.c"), Path::new("/nope"), None).unwrap_err();
        assert!(err.contains("program not found: /nope/a") && err.contains("launch.json"), "{err}");
        let here = std::env::current_exe().unwrap();
        let ov = LaunchOverride { program: Some(here.to_string_lossy().into_owned()), ..Default::default() };
        assert!(r.get("lldb-dap").unwrap().preflight(Path::new("/nope/a.c"), Path::new("/nope"), Some(&ov)).is_ok());
        let at = LaunchOverride { request: Some("attach".into()), ..Default::default() };
        let err = r.get("delve").unwrap().preflight(Path::new("/p/main.go"), Path::new("/p"), Some(&at)).unwrap_err();
        assert!(err.contains("cannot attach"), "{err}");
        assert!(r.get("debugpy").unwrap().preflight(Path::new("/p/a.py"), Path::new("/p"), Some(&at)).is_ok());
    }

    const CARGO_OUT: &str = r#"{"reason":"compiler-artifact","manifest_path":"/w/lib/Cargo.toml","target":{"kind":["lib"],"src_path":"/w/lib/src/lib.rs"},"profile":{"test":false},"executable":null}
{"reason":"compiler-artifact","manifest_path":"/w/tool/Cargo.toml","target":{"kind":["bin"],"src_path":"/w/tool/src/main.rs"},"profile":{"test":false},"executable":"/w/target/debug/tool"}
{"reason":"compiler-artifact","manifest_path":"/w/app/Cargo.toml","target":{"kind":["bin"],"src_path":"/w/app/src/main.rs"},"profile":{"test":true},"executable":"/w/target/debug/deps/app-test"}
{"reason":"compiler-artifact","manifest_path":"/w/app/Cargo.toml","target":{"kind":["bin"],"src_path":"/w/app/src/main.rs"},"profile":{"test":false},"executable":"/w/target/debug/app"}
{"reason":"compiler-artifact","manifest_path":"/w/app/Cargo.toml","target":{"kind":["bin"],"src_path":"/w/app/src/bin/other.rs"},"profile":{"test":false},"executable":"/w/target/debug/other"}
not json
{"reason":"build-finished","success":true}"#;

    #[test]
    fn cargo_executable_picks_the_binary_of_the_file() {
        let pick = |f: &str| cargo_executable(CARGO_OUT, Path::new(f)).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(pick("/w/app/src/bin/other.rs").as_deref(), Some("/w/target/debug/other"), "the file's own target");
        assert_eq!(
            pick("/w/app/src/util.rs").as_deref(),
            Some("/w/target/debug/app"),
            "a module of the package: its first bin, never the test harness"
        );
        assert_eq!(pick("/w/elsewhere/x.rs").as_deref(), Some("/w/target/debug/tool"), "else the first binary");
        assert_eq!(cargo_executable(r#"{"reason":"build-finished"}"#, Path::new("/w/a.rs")), None);
    }

    #[test]
    fn cargo_errors_are_the_rendered_compiler_messages() {
        let out = concat!(
            r#"{"reason":"compiler-message","message":{"level":"warning","rendered":"warning: unused\n"}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","rendered":"error[E0425]: cannot find value `x`\n --> src/main.rs:2:5\n"}}"#
        );
        let e = cargo_errors(out);
        assert_eq!(e.len(), 1);
        assert!(e[0].starts_with("error[E0425]") && e[0].contains("src/main.rs:2:5"));
    }

    #[test]
    fn cargo_root_is_the_nearest_manifest_folder() {
        let dir = std::env::temp_dir().join(format!("lumen-cargo-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        std::fs::write(dir.join("crates/a/Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        assert_eq!(cargo_root(&dir.join("crates/a/src/main.rs"), &dir), dir.join("crates/a"));
        assert_eq!(cargo_root(&dir.join("other/x.rs"), &dir), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_falls_back_to_the_next_adapter() {
        let dir = std::env::temp_dir().join(format!("lumen-dap-fallback-{}", std::process::id()));
        let bin = dir.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mk = |id: &str, command: &str, fallback: &[&str]| Adapter {
            id: id.into(),
            command: command.into(),
            fallback: fallback.iter().map(|s| s.to_string()).collect(),
            ..serde_json::from_value(serde_json::json!({"id": id, "command": command, "languages": [], "launch": {}}))
                .unwrap()
        };
        let reg = Registry {
            adapters: vec![mk("first", "lumen-no-such-adapter", &["second"]), mk("second", "lumen-fake-lldb", &[])],
        };
        assert_eq!(
            reg.installed("first", &dir).unwrap().id,
            "first",
            "none installed: the preferred one, for its install hint"
        );
        let exe = bin.join("lumen-fake-lldb");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(reg.installed("first", &dir).unwrap().id, "second");
        }
        assert!(reg.installed("missing", &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn fake_tool(dir: &Path, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cargo_prelaunch_returns_the_built_binary_or_the_compiler_errors() {
        let dir = std::env::temp_dir().join(format!("lumen-cargo-pre-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"demo\"\n").unwrap();
        let file = dir.join("src/main.rs");
        let a = Registry::builtin().get("lldb-rust").unwrap().clone();
        let art = format!(
            r#"{{"reason":"compiler-artifact","manifest_path":"{0}/Cargo.toml","target":{{"kind":["bin"],"src_path":"{0}/src/main.rs"}},"profile":{{"test":false}},"executable":"{0}/target/debug/demo"}}"#,
            dir.display()
        );
        fake_tool(
            &dir,
            "cargo",
            &format!("[ \"$1\" = build ] && [ \"$2\" = --message-format=json ] || exit 9\necho '{art}'"),
        );
        let got = a.run_prelaunch_program(&file, &dir, None).unwrap();
        assert_eq!(got, Some(dir.join("target/debug/demo")));
        fake_tool(
            &dir,
            "cargo",
            "echo '{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"rendered\":\"error: boom\\n\"}}'\nexit 101",
        );
        let e = a.run_prelaunch_program(&file, &dir, None).unwrap_err().to_string();
        assert!(e.contains("error: boom") && e.contains("failed"), "{e}");
        fake_tool(&dir, "cargo", "echo '{\"reason\":\"build-finished\",\"success\":true}'");
        let e = a.run_prelaunch_program(&file, &dir, None).unwrap_err().to_string();
        assert!(e.contains("no executable"), "{e}");
        let ov = LaunchOverride { program: Some("out/bin".into()), ..Default::default() };
        assert_eq!(a.run_prelaunch_program(&file, &dir, Some(&ov)).unwrap(), None, "launch.json names the program");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_adapter_module_shows_the_install_hint() {
        let mut a = Registry::builtin().get("debugpy").unwrap().clone();
        a.python = false;
        a.command = "sh".into();
        a.probe = vec!["-c".into(), "exit 1".into()];
        let e = a.start(Path::new("/"), std::sync::Arc::new(|| {})).err().expect("probe fails").to_string();
        assert!(e.contains("not set up") && e.contains("pip install debugpy"), "{e}");
        a.command = "lumen-no-such-python".into();
        let e = a.start(Path::new("/"), std::sync::Arc::new(|| {})).err().expect("missing").to_string();
        assert!(e.contains("not found (install: python -m pip install debugpy"), "{e}");
    }

    #[test]
    fn file_stem_placeholder() {
        assert_eq!(substitute_str("${dir}/${fileStem}", Path::new("/p/s/a.test.c"), Path::new("/p")), "/p/s/a.test");
    }
}
