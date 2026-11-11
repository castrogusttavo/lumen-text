//! Supervision of registry language servers: restart-with-backoff bookkeeping, dynamic capability
//! registration, `workspace/configuration` answers and `$/progress` text. Pure logic, no processes.

use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// A server that keeps dying is restarted this many times, then reported as crashed.
pub const MAX_RESTARTS: u32 = 4;
/// How long a server with no open document is kept running before it is stopped.
pub const IDLE_GRACE: Duration = Duration::from_secs(45);
/// A server that ran at least this long was healthy: its crash counter starts over.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);

/// First restart delay; doubled for every further crash up to 16x.
pub const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Delay before the `n`th restart (1-based): 0.5 s, 1 s, 2 s, 4 s, then 8 s.
pub fn backoff(n: u32, base: Duration) -> Duration {
    base * (1u32 << n.saturating_sub(1).min(4))
}

/// The timings of the supervisor (shortened by the tests).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub backoff: Duration,
    pub idle_grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { backoff: BACKOFF_BASE, idle_grace: IDLE_GRACE }
    }
}

/// Crash history of one (server, workspace root).
#[derive(Clone, Debug, Default)]
pub struct Health {
    pub crashes: u32,
    pub next_try: Option<Instant>,
    pub gave_up: bool,
    /// Last lines the server printed before dying (for the status listing).
    pub last_error: String,
}

impl Health {
    /// The server exited unexpectedly after running for `uptime`. Returns the delay before the
    /// restart, or `None` when it has crashed too often (`gave_up`).
    pub fn on_exit(&mut self, now: Instant, uptime: Duration, error: String, base: Duration) -> Option<Duration> {
        if uptime >= STABLE_AFTER {
            self.crashes = 0;
        }
        self.crashes += 1;
        self.last_error = error;
        if self.crashes > MAX_RESTARTS {
            self.gave_up = true;
            self.next_try = None;
            return None;
        }
        let d = backoff(self.crashes, base);
        self.next_try = Some(now + d);
        Some(d)
    }

    /// May the server be started now?
    pub fn may_start(&self, now: Instant) -> bool {
        !self.gave_up && self.next_try.is_none_or(|t| t <= now)
    }
}

// ───────────────────────────── dynamic registration ─────────────────────────────

/// The `ServerCapabilities` key a dynamically registered method turns on.
pub fn capability_key(method: &str) -> Option<&'static str> {
    Some(match method {
        "textDocument/hover" => "hoverProvider",
        "textDocument/completion" => "completionProvider",
        "textDocument/signatureHelp" => "signatureHelpProvider",
        "textDocument/definition" => "definitionProvider",
        "textDocument/typeDefinition" => "typeDefinitionProvider",
        "textDocument/implementation" => "implementationProvider",
        "textDocument/references" => "referencesProvider",
        "textDocument/rename" => "renameProvider",
        "textDocument/formatting" => "documentFormattingProvider",
        "textDocument/rangeFormatting" => "documentRangeFormattingProvider",
        "textDocument/codeAction" => "codeActionProvider",
        "textDocument/documentSymbol" => "documentSymbolProvider",
        "textDocument/foldingRange" => "foldingRangeProvider",
        "textDocument/selectionRange" => "selectionRangeProvider",
        "textDocument/inlayHint" => "inlayHintProvider",
        "textDocument/codeLens" => "codeLensProvider",
        "textDocument/semanticTokens" | "textDocument/semanticTokens/full" => "semanticTokensProvider",
        "textDocument/diagnostic" => "diagnosticProvider",
        "textDocument/prepareCallHierarchy" => "callHierarchyProvider",
        "workspace/symbol" => "workspaceSymbolProvider",
        _ => return None,
    })
}

/// `client/registerCapability`: switch the capabilities on and remember id -> method.
pub fn register(caps: &mut Value, regs: &mut HashMap<String, String>, params: &Value) {
    for r in params["registrations"].as_array().into_iter().flatten() {
        let (Some(id), Some(method)) = (r["id"].as_str(), r["method"].as_str()) else { continue };
        regs.insert(id.to_string(), method.to_string());
        if let Some(key) = capability_key(method) {
            let opts = &r["registerOptions"];
            if !caps.is_object() {
                *caps = json!({});
            }
            caps[key] = if opts.is_object() { opts.clone() } else { json!(true) };
        }
    }
}

/// `client/unregisterCapability`.
pub fn unregister(caps: &mut Value, regs: &mut HashMap<String, String>, params: &Value) {
    for r in params["unregisterations"].as_array().into_iter().chain(params["unregistrations"].as_array()).flatten() {
        let Some(id) = r["id"].as_str() else { continue };
        if let Some(method) = regs.remove(id)
            && let Some(key) = capability_key(&method)
            && !regs.values().any(|m| capability_key(m) == Some(key))
        {
            caps[key] = Value::Null;
        }
    }
}

// ───────────────────────────── workspace/configuration ─────────────────────────────

/// The part of `settings` a dotted `section` ("pyright.analysis") names; `Null` when absent.
/// No section asks for everything.
pub fn config_section(settings: &Value, section: Option<&str>) -> Value {
    let Some(sec) = section.filter(|s| !s.is_empty()) else { return settings.clone() };
    // Try the whole dotted path first, then every split point (`{"a.b": {...}}` and `{"a": {"b": ...}}`).
    let mut cur = settings;
    for part in sec.split('.') {
        match cur.get(part) {
            Some(v) => cur = v,
            None => return settings.get(sec).cloned().unwrap_or_else(|| missing_section(sec)),
        }
    }
    cur.clone()
}

/// What an unconfigured section reads as. Like VS Code, a top-level namespace (`css`, `yaml`) is an
/// empty object, so servers that do `settings.lint` with defaults keep working (the CSS server threw
/// on `null` and never validated); a dotted leaf (`python.analysis.mode`) is `null` ("use the default").
fn missing_section(sec: &str) -> Value {
    if sec.contains('.') { Value::Null } else { Value::Object(Default::default()) }
}

/// Answer to `workspace/configuration` for a registry server.
pub fn configuration_reply(settings: &Value, params: &Value) -> Value {
    Value::Array(
        params["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|it| config_section(settings, it["section"].as_str()))
            .collect(),
    )
}

// ───────────────────────────── $/progress ─────────────────────────────

/// Status text for one `$/progress` value; `None` when the work is finished.
pub fn progress_text(prev: Option<&str>, v: &Value) -> Option<String> {
    match v["kind"].as_str()? {
        "end" => None,
        kind => {
            let title = v["title"].as_str().filter(|t| !t.is_empty());
            let msg = v["message"].as_str().filter(|m| !m.is_empty());
            let pct = v["percentage"].as_u64();
            // `report` repeats only what changed: keep the title from `begin`.
            let base: Option<String> = title.map(str::to_string).or_else(|| {
                (kind == "report")
                    .then(|| prev.map(|p| p.split(" — ").next().unwrap_or(p).trim_end_matches('%').to_string()))?
            });
            let mut s = base.unwrap_or_default();
            let tail = match (msg, pct) {
                (Some(m), Some(p)) => format!("{m} {p}%"),
                (Some(m), None) => m.to_string(),
                (None, Some(p)) => format!("{p}%"),
                (None, None) => String::new(),
            };
            if !tail.is_empty() {
                if !s.is_empty() {
                    s.push_str(" — ");
                }
                s.push_str(&tail);
            }
            Some(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let ms: Vec<u128> = (1..=7).map(|n| backoff(n, BACKOFF_BASE).as_millis()).collect();
        assert_eq!(ms, vec![500, 1000, 2000, 4000, 8000, 8000, 8000]);
    }

    #[test]
    fn health_gives_up_after_max_restarts_and_forgives_a_stable_run() {
        let t0 = Instant::now();
        let mut h = Health::default();
        assert!(h.may_start(t0));
        for n in 1..=MAX_RESTARTS {
            let d = h.on_exit(t0, Duration::from_secs(1), format!("boom {n}"), BACKOFF_BASE).expect("restart allowed");
            assert_eq!(d, backoff(n, BACKOFF_BASE));
            assert!(!h.may_start(t0), "must wait out the backoff");
            assert!(h.may_start(t0 + d));
        }
        assert!(h.on_exit(t0, Duration::from_secs(1), "last".into(), BACKOFF_BASE).is_none());
        assert!(h.gave_up && !h.may_start(t0 + Duration::from_secs(3600)));
        assert_eq!(h.last_error, "last");
        // a server that ran for a long time before dying starts the count over
        let mut h = Health { crashes: 3, ..Default::default() };
        assert_eq!(h.on_exit(t0, STABLE_AFTER, String::new(), BACKOFF_BASE), Some(backoff(1, BACKOFF_BASE)));
        assert_eq!(h.crashes, 1);
    }

    #[test]
    fn dynamic_registration_toggles_capabilities() {
        let mut caps = json!({"hoverProvider": true});
        let mut regs = HashMap::new();
        register(
            &mut caps,
            &mut regs,
            &json!({"registrations": [
                {"id": "a", "method": "textDocument/formatting"},
                {"id": "b", "method": "textDocument/completion", "registerOptions": {"triggerCharacters": ["."]}},
                {"id": "c", "method": "workspace/didChangeWatchedFiles", "registerOptions": {"watchers": []}},
            ]}),
        );
        assert_eq!(caps["documentFormattingProvider"], true);
        assert_eq!(caps["completionProvider"]["triggerCharacters"][0], ".");
        assert_eq!(regs.len(), 3);
        unregister(
            &mut caps,
            &mut regs,
            &json!({"unregisterations": [{"id": "a", "method": "textDocument/formatting"}]}),
        );
        assert!(caps["documentFormattingProvider"].is_null());
        assert_eq!(caps["hoverProvider"], true);
        let mut c = Value::Null;
        register(&mut c, &mut HashMap::new(), &json!({"registrations": [{"id": "x", "method": "textDocument/hover"}]}));
        assert_eq!(c["hoverProvider"], true);
    }

    #[test]
    fn configuration_sections_resolve_dotted_paths() {
        let s = json!({"python": {"analysis": {"mode": "off"}}, "rust-analyzer.check": {"command": "clippy"}, "x": 1});
        assert_eq!(config_section(&s, Some("python.analysis.mode")), "off");
        assert_eq!(config_section(&s, Some("python.analysis"))["mode"], "off");
        assert_eq!(config_section(&s, Some("rust-analyzer.check"))["command"], "clippy");
        assert!(config_section(&s, Some("nope.x")).is_null());
        assert_eq!(config_section(&s, None), s);
        let r = configuration_reply(&s, &json!({"items": [{"section": "x"}, {"section": "y"}]}));
        assert_eq!(r, json!([1, {}]), "an unconfigured namespace is an empty object, not null");
    }

    #[test]
    fn progress_text_follows_begin_report_end() {
        let b = progress_text(None, &json!({"kind": "begin", "title": "Indexing", "percentage": 0})).unwrap();
        assert_eq!(b, "Indexing — 0%");
        let r = progress_text(Some(&b), &json!({"kind": "report", "message": "12/340", "percentage": 3})).unwrap();
        assert_eq!(r, "Indexing — 12/340 3%");
        assert_eq!(progress_text(Some(&r), &json!({"kind": "end"})), None);
        assert_eq!(progress_text(None, &json!({"kind": "begin", "title": "Loading"})).unwrap(), "Loading");
    }
}
