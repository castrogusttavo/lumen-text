//! Docker without any UI: parses what the `docker` CLI prints with `--format '{{json .}}'` (one JSON
//! object per line) and runs the CLI. The panel (`app_docker`) calls these on a worker thread.

use serde_json::Value;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// "running", "exited", "paused", "restarting", "created"...
    pub state: String,
    /// Human status, e.g. "Up 2 hours" or "Exited (0) 3 days ago".
    pub status: String,
    pub ports: String,
}

impl Container {
    pub fn running(&self) -> bool {
        matches!(self.state.as_str(), "running" | "restarting" | "paused")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub id: String,
    pub repo: String,
    pub tag: String,
    pub size: String,
    pub created: String,
}

impl Image {
    /// `repo:tag`, or the short id for an untagged (dangling) image.
    pub fn label(&self) -> String {
        if self.repo.is_empty() || self.repo == "<none>" {
            return format!("<untagged> {}", self.id);
        }
        if self.tag.is_empty() || self.tag == "<none>" {
            self.repo.clone()
        } else {
            format!("{}:{}", self.repo, self.tag)
        }
    }
}

fn field(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

fn objects(out: &str) -> impl Iterator<Item = Value> + '_ {
    out.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).filter(Value::is_object)
}

/// The output of `docker ps -a --format '{{json .}}'`.
pub fn parse_containers(out: &str) -> Vec<Container> {
    objects(out)
        .map(|v| {
            let status = field(&v, "Status");
            let mut state = field(&v, "State").to_lowercase();
            if state.is_empty() {
                // Older CLIs have no State: derive it from the status text.
                state = if status.starts_with("Up") {
                    if status.contains("Paused") { "paused" } else { "running" }
                } else if status.starts_with("Exited") {
                    "exited"
                } else {
                    "created"
                }
                .to_string();
            }
            Container {
                id: field(&v, "ID"),
                name: field(&v, "Names").split(',').next().unwrap_or("").to_string(),
                image: field(&v, "Image"),
                state,
                status,
                ports: short_ports(&field(&v, "Ports")),
            }
        })
        .filter(|c| !c.id.is_empty())
        .collect()
}

/// The output of `docker images --format '{{json .}}'`.
pub fn parse_images(out: &str) -> Vec<Image> {
    objects(out)
        .map(|v| Image {
            id: field(&v, "ID"),
            repo: field(&v, "Repository"),
            tag: field(&v, "Tag"),
            size: field(&v, "Size"),
            created: field(&v, "CreatedSince"),
        })
        .filter(|i| !i.id.is_empty())
        .collect()
}

/// `0.0.0.0:8080->80/tcp, :::8080->80/tcp, 5432/tcp` as `8080->80/tcp, 5432/tcp`: the IPv6 twin of
/// a mapping is dropped and the wildcard host omitted.
pub fn short_ports(ports: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in ports.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if p.starts_with(":::") || p.starts_with("[::]:") {
            continue;
        }
        let p = p.strip_prefix("0.0.0.0:").unwrap_or(p).to_string();
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out.join(", ")
}

/// Why a docker call did not work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// No `docker` executable.
    Missing,
    /// It ran and complained (daemon not running, permission, no such container...).
    Failed(String),
}

/// Runs `docker <args>` and returns its stdout. Blocks: call from a worker thread only.
pub fn run(args: &[&str]) -> Result<String, Fail> {
    let mut c = Command::new("docker");
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { Fail::Missing } else { Fail::Failed(e.to_string()) }
    })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(Fail::Failed(first_line(&String::from_utf8_lossy(&out.stderr))))
}

/// The first non-empty line of docker's complaint, without the "Error response from daemon:" noise.
pub fn first_line(stderr: &str) -> String {
    let l = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("docker failed");
    l.strip_prefix("Error response from daemon: ").unwrap_or(l).to_string()
}

/// Everything the panel lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
}

pub fn snapshot() -> Result<Snapshot, Fail> {
    let containers = parse_containers(&run(&["ps", "-a", "--format", "{{json .}}"])?);
    let images = parse_images(&run(&["images", "--format", "{{json .}}"])?);
    Ok(Snapshot { containers, images })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = r#"{"Command":"\"docker-entrypoint.s…\"","CreatedAt":"2025-01-01 10:00:00 +0000 UTC","ID":"3f2a9c1b7d4e","Image":"postgres:16","Labels":"","Names":"db","Ports":"0.0.0.0:5432->5432/tcp, :::5432->5432/tcp","State":"running","Status":"Up 2 hours"}
{"ID":"aa11bb22cc33","Image":"redis:7","Names":"cache,other/alias","Ports":"6379/tcp","State":"exited","Status":"Exited (0) 3 days ago"}
not json at all
{"ID":"","Names":"broken"}
"#;

    const IMAGES: &str = r#"{"Containers":"N/A","CreatedAt":"2025-01-01","CreatedSince":"2 weeks ago","Digest":"<none>","ID":"sha256abc","Repository":"postgres","SharedSize":"N/A","Size":"431MB","Tag":"16","UniqueSize":"N/A","VirtualSize":"431MB"}
{"ID":"deadbeef0000","Repository":"<none>","Tag":"<none>","Size":"12MB","CreatedSince":"1 hour ago"}
"#;

    #[test]
    fn containers_are_parsed_and_junk_lines_skipped() {
        let v = parse_containers(PS);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "db");
        assert_eq!(v[0].image, "postgres:16");
        assert_eq!(v[0].ports, "5432->5432/tcp");
        assert!(v[0].running());
        assert_eq!(v[1].name, "cache");
        assert_eq!(v[1].state, "exited");
        assert!(!v[1].running());
    }

    #[test]
    fn state_falls_back_to_the_status_text() {
        let v = parse_containers(r#"{"ID":"a","Names":"x","Status":"Up 5 minutes (Paused)"}"#);
        assert_eq!(v[0].state, "paused");
        let v = parse_containers(r#"{"ID":"a","Names":"x","Status":"Exited (1) 1 minute ago"}"#);
        assert_eq!(v[0].state, "exited");
    }

    #[test]
    fn images_are_parsed_and_labelled() {
        let v = parse_images(IMAGES);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].label(), "postgres:16");
        assert_eq!(v[0].size, "431MB");
        assert_eq!(v[1].label(), "<untagged> deadbeef0000");
    }

    #[test]
    fn ports_are_shortened() {
        assert_eq!(short_ports("0.0.0.0:80->80/tcp, [::]:80->80/tcp"), "80->80/tcp");
        assert_eq!(short_ports("127.0.0.1:9000->9000/tcp"), "127.0.0.1:9000->9000/tcp");
        assert_eq!(short_ports(""), "");
    }

    #[test]
    fn daemon_errors_lose_their_prefix() {
        assert_eq!(first_line("\nError response from daemon: No such container: x\nmore"), "No such container: x");
        assert_eq!(first_line(""), "docker failed");
    }
}
