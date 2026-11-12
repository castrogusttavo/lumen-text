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
        let (request, args) = self.launch_request(file, root);
        let mut s = Session::with_request(&request, args);
        s.exception_bps = false;
        s
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
    fn builtin_parses_with_delve_and_ghp_hook() {
        let r = Registry::builtin();
        assert!(r.get("delve").is_some() && r.get("ghp").is_some());
        let d = r.for_language("go").expect("go adapter");
        assert_eq!(d.id, "delve");
        assert_eq!(d.transport, Transport::TcpListen);
        assert!(r.for_language("ghp").is_none(), "experimental adapters are never automatic");
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
