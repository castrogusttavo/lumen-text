//! Docker (or Podman) without any UI: parses what the container CLI prints with
//! `--format '{{json .}}'` (one JSON object per line) and runs the CLI. The two engines print the
//! same command differently (Docker flattens every field into a string, Podman dumps its own
//! structs: `Id`, `Names: [..]`, `Ports: [{..}]`, `Created` as a unix time), so the parsers accept
//! both. The panel (`app_docker`) calls these on a worker thread.

use serde_json::Value;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

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

/// The first of `keys` present as a string, a number or a list of strings (its first item).
fn text(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        match v.get(*k) {
            Some(Value::String(s)) if !s.trim().is_empty() => return s.trim().to_string(),
            Some(Value::Number(n)) => return n.to_string(),
            Some(Value::Array(a)) => {
                if let Some(s) = a.iter().find_map(Value::as_str) {
                    return s.trim().to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// Docker prints 12-character ids; Podman prints the full 64.
fn short_id(id: String) -> String {
    id.chars().take(12).collect()
}

fn objects(out: &str) -> impl Iterator<Item = Value> + '_ {
    out.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).filter(Value::is_object)
}

/// Podman's `Ports: [{"host_ip":"","container_port":80,"host_port":8080,"range":1,"protocol":"tcp"}]`
/// as Docker's `8080->80/tcp`; Docker's own string is kept.
fn ports_text(v: &Value) -> String {
    let Some(list) = v.get("Ports").and_then(Value::as_array) else { return field(v, "Ports") };
    let mut out: Vec<String> = Vec::new();
    for p in list {
        let n = |k: &str| p.get(k).and_then(Value::as_u64).unwrap_or(0);
        let (host, cont, range) = (n("host_port"), n("container_port"), n("range").max(1));
        let proto = p.get("protocol").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("tcp");
        let span = |first: u64| if range > 1 { format!("{first}-{}", first + range - 1) } else { first.to_string() };
        out.push(if host == 0 {
            format!("{}/{proto}", span(cont))
        } else {
            format!("{}->{}/{proto}", span(host), span(cont))
        });
    }
    out.join(", ")
}

/// `1.5kB`, `80.7MB`...: decimal units like `docker images`.
fn human_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let (mut n, mut i) = (bytes as f64, 0);
    while n >= 1000.0 && i < U.len() - 1 {
        n /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes}B")
    } else {
        format!("{n:.3}").trim_end_matches('0').trim_end_matches('.').to_string() + U[i]
    }
}

/// `3 days ago` for an age in seconds.
fn ago(secs: u64) -> String {
    let (n, unit) = match secs {
        0..=59 => (secs, "second"),
        60..=3599 => (secs / 60, "minute"),
        3600..=86399 => (secs / 3600, "hour"),
        86400..=604799 => (secs / 86400, "day"),
        604800..=2591999 => (secs / 604800, "week"),
        2592000..=31535999 => (secs / 2592000, "month"),
        _ => (secs / 31536000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// The output of `docker ps -a --format '{{json .}}'` (or Podman's).
pub fn parse_containers(out: &str) -> Vec<Container> {
    objects(out)
        .map(|v| {
            let mut status = field(&v, "Status");
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
            if status.is_empty() {
                // Podman leaves it empty for containers that never ran.
                status = match state.as_str() {
                    "exited" => format!("Exited ({})", v.get("ExitCode").and_then(Value::as_i64).unwrap_or(0)),
                    s => s
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().collect::<String>() + &s[c.len_utf8()..])
                        .unwrap_or_default(),
                };
            }
            Container {
                id: short_id(text(&v, &["ID", "Id"])),
                name: text(&v, &["Names"]).split(',').next().unwrap_or("").to_string(),
                image: field(&v, "Image"),
                state,
                status,
                ports: short_ports(&ports_text(&v)),
            }
        })
        .filter(|c| !c.id.is_empty())
        .collect()
}

/// The output of `docker images --format '{{json .}}'` (or Podman's).
pub fn parse_images(out: &str) -> Vec<Image> {
    objects(out)
        .map(|v| {
            let size = match v.get("Size") {
                Some(Value::Number(n)) => human_size(n.as_u64().unwrap_or(0)),
                _ => field(&v, "Size"),
            };
            let created = match v.get("Created").and_then(Value::as_u64) {
                Some(t) if field(&v, "CreatedSince").is_empty() => {
                    let now =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    ago(now.saturating_sub(t))
                }
                _ => field(&v, "CreatedSince"),
            };
            Image {
                id: short_id(text(&v, &["ID", "Id"])),
                repo: text(&v, &["Repository", "repository"]),
                tag: text(&v, &["Tag", "tag"]),
                size,
                created,
            }
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

/// The container CLI in use: `LUMEN_CONTAINER_ENGINE` (`docker` or `podman`), else `docker` when it
/// is on the `PATH`, else `podman`. Looked up once.
pub fn engine() -> &'static str {
    static E: OnceLock<&'static str> = OnceLock::new();
    E.get_or_init(|| {
        match std::env::var("LUMEN_CONTAINER_ENGINE").ok().as_deref().map(str::trim) {
            Some("podman") => return "podman",
            Some("docker") => return "docker",
            _ => {}
        }
        if on_path("docker") || !on_path("podman") { "docker" } else { "podman" }
    })
}

/// "Docker" or "Podman", for messages.
pub fn engine_name() -> &'static str {
    if engine() == "podman" { "Podman" } else { "Docker" }
}

fn on_path(exe: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| d.join(exe).is_file() || d.join(format!("{exe}.exe")).is_file())
}

/// Why a docker call did not work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// No `docker` / `podman` executable.
    Missing,
    /// It ran and complained (daemon not running, permission, no such container...).
    Failed(String),
}

/// Runs `docker <args>` (or `podman <args>`, see [`engine`]) and returns its stdout. Blocks: call from a worker thread only.
pub fn run(args: &[&str]) -> Result<String, Fail> {
    let mut c = Command::new(engine());
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
    let l = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("container command failed");
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

    /// Real `podman ps -a --format '{{json .}}'` / `podman images ...` lines (trimmed).
    const PODMAN_PS: &str = r#"{"Command":["true"],"Created":"2026-11-23T08:30:25-03:00","Exited":false,"ExitCode":0,"Id":"247d394b2b878d6c82ca23e1f980d3b72ddfd0b3b5a32267190b05f19d791c52","Image":"docker.io/library/ubuntu:24.04","Names":["lumen-probe-tmp"],"Ports":[{"host_ip":"","container_port":80,"host_port":18080,"range":1,"protocol":"tcp"},{"host_ip":"","container_port":9000,"host_port":0,"range":2,"protocol":"udp"}],"State":"created","Status":""}
{"Id":"aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44","Image":"redis:7","Names":["cache"],"Ports":null,"State":"exited","Status":"","ExitCode":137}
"#;

    const PODMAN_IMAGES: &str = r#"{"repository":"docker.io/library/ubuntu","tag":"24.04","Id":"2b147973fd863c93ced60f325461fe0e262b91317b008e6a2d961f13e81af0e4","Created":1794335718,"Size":80725364}
"#;

    #[test]
    fn podman_containers_are_parsed() {
        let v = parse_containers(PODMAN_PS);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].id.as_str(), v[0].name.as_str()), ("247d394b2b87", "lumen-probe-tmp"));
        assert_eq!(v[0].image, "docker.io/library/ubuntu:24.04");
        assert_eq!(v[0].ports, "18080->80/tcp, 9000-9001/udp");
        assert_eq!((v[0].state.as_str(), v[0].status.as_str()), ("created", "Created"));
        assert!(!v[0].running());
        assert_eq!(v[1].ports, "");
        assert_eq!(v[1].status, "Exited (137)");
    }

    #[test]
    fn podman_images_are_parsed() {
        let v = parse_images(PODMAN_IMAGES);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "2b147973fd86");
        assert_eq!(v[0].label(), "docker.io/library/ubuntu:24.04");
        assert_eq!(v[0].size, "80.725MB");
        assert!(v[0].created.ends_with(" ago"), "{}", v[0].created);
    }

    #[test]
    fn sizes_and_ages_read_like_the_docker_cli() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1500), "1.5kB");
        assert_eq!(human_size(431_000_000), "431MB");
        assert_eq!(ago(1), "1 second ago");
        assert_eq!(ago(2 * 86400), "2 days ago");
        assert_eq!(ago(3 * 604800), "3 weeks ago");
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
        assert_eq!(first_line(""), "container command failed");
    }
}
