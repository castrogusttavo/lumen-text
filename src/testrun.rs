//! Test runners without any UI: which runner a project uses, the command that runs it, and the
//! parsers that turn its report (JSON, JUnit, TRX or a live stream) into one list of [`Case`]s.

use crate::tasks::{Pm, shell_quote};
use regex::Regex;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::SystemTime,
};

// ───────────── model ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Runner {
    /// The user's own command (`test_command` in config.json), reporting through JUnit XML.
    Custom,
    Nextest,
    Cargo,
    Vitest,
    Jest,
    NodeTest,
    Bun,
    Deno,
    Go,
    Pytest,
    Phpunit,
    Pest,
    Rspec,
    Maven,
    Gradle,
    Dotnet,
    Ctest,
}

impl Runner {
    pub fn label(self) -> &'static str {
        match self {
            Runner::Custom => "custom",
            Runner::Nextest => "nextest",
            Runner::Cargo => "cargo test",
            Runner::Vitest => "vitest",
            Runner::Jest => "jest",
            Runner::NodeTest => "node --test",
            Runner::Bun => "bun test",
            Runner::Deno => "deno test",
            Runner::Go => "go test",
            Runner::Pytest => "pytest",
            Runner::Phpunit => "phpunit",
            Runner::Pest => "pest",
            Runner::Rspec => "rspec",
            Runner::Maven => "maven",
            Runner::Gradle => "gradle",
            Runner::Dotnet => "dotnet test",
            Runner::Ctest => "ctest",
        }
    }

    /// Extensions of the files this runner's tests live in.
    pub fn exts(self) -> &'static [&'static str] {
        match self {
            Runner::Custom => &[],
            Runner::Nextest | Runner::Cargo => &["rs"],
            Runner::Vitest | Runner::Jest | Runner::NodeTest | Runner::Bun | Runner::Deno => {
                &["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"]
            }
            Runner::Go => &["go"],
            Runner::Pytest => &["py"],
            Runner::Phpunit | Runner::Pest => &["php"],
            Runner::Rspec => &["rb"],
            Runner::Maven | Runner::Gradle => &["java", "kt", "kts", "scala", "groovy"],
            Runner::Dotnet => &["cs", "fs", "vb"],
            Runner::Ctest => &["c", "cc", "cpp", "cxx", "h", "hpp"],
        }
    }

    /// Can "debug this test" start a session for the runner (Rust through gdb, JS through js-debug)?
    pub fn debuggable(self) -> bool {
        matches!(self, Runner::Nextest | Runner::Cargo | Runner::Vitest | Runner::Jest | Runner::NodeTest)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Status {
    #[default]
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Default)]
pub struct Case {
    /// The header the case is listed under: a file, a module or a suite.
    pub group: String,
    /// Class / binary the runner reported (used to find the file and to re-run).
    pub class: String,
    /// What is shown for the case.
    pub name: String,
    /// Stable identity across runs, also what a re-run filters on.
    pub id: String,
    pub status: Status,
    pub ms: Option<f64>,
    pub file: Option<String>,
    /// 0-based.
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub pending: usize,
    pub running: usize,
}

impl Counts {
    pub fn of(cases: &[Case]) -> Counts {
        let mut c = Counts::default();
        for k in cases {
            match k.status {
                Status::Passed => c.passed += 1,
                Status::Failed => c.failed += 1,
                Status::Skipped => c.skipped += 1,
                Status::Pending => c.pending += 1,
                Status::Running => c.running += 1,
            }
        }
        c
    }
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped + self.pending + self.running
    }
}

/// `1.2s`, `1m 05s`.
pub fn fmt_secs(s: f64) -> String {
    if s < 60.0 { format!("{s:.1}s") } else { format!("{}m {:02}s", s as u64 / 60, s as u64 % 60) }
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' && it.peek() == Some(&'[') {
            it.next();
            for ch in it.by_ref() {
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

// ───────────── detection ─────────────

pub(crate) fn read(root: &Path, f: &str) -> String {
    std::fs::read_to_string(root.join(f)).unwrap_or_default()
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

pub(crate) fn dir_has(root: &Path, pred: impl Fn(&str) -> bool) -> bool {
    std::fs::read_dir(root).is_ok_and(|rd| rd.flatten().any(|e| pred(&e.file_name().to_string_lossy())))
}

fn has_py_tests(root: &Path) -> bool {
    for d in ["tests", "test", "."] {
        let dir = root.join(d);
        if std::fs::read_dir(&dir).is_ok_and(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.ends_with(".py") && (n.starts_with("test_") || n.ends_with("_test.py"))
            })
        }) {
            return true;
        }
    }
    false
}

/// Every runner this project looks set up for, most likely first. `custom` is the configured
/// `test_command`.
pub fn detect(root: &Path, custom: bool) -> Vec<Runner> {
    let mut v = Vec::new();
    if custom {
        v.push(Runner::Custom);
    }
    if root.join("Cargo.toml").exists() {
        v.push(if on_path("cargo-nextest") { Runner::Nextest } else { Runner::Cargo });
    }
    let pkg = read(root, "package.json");
    if !pkg.is_empty() {
        let vitest = pkg.contains("\"vitest\"");
        let jest = pkg.contains("\"jest\"");
        if vitest {
            v.push(Runner::Vitest);
        }
        if jest {
            v.push(Runner::Jest);
        }
        if pkg.contains("node --test") {
            v.push(Runner::NodeTest);
        }
        let bun_lock = root.join("bun.lockb").exists() || root.join("bun.lock").exists();
        if pkg.contains("bun test") || (bun_lock && !vitest && !jest) {
            v.push(Runner::Bun);
        }
    }
    if root.join("deno.json").exists() || root.join("deno.jsonc").exists() {
        v.push(Runner::Deno);
    }
    if root.join("go.mod").exists() {
        v.push(Runner::Go);
    }
    let py = read(root, "pyproject.toml") + &read(root, "setup.cfg") + &read(root, "requirements.txt");
    if root.join("pytest.ini").exists()
        || root.join("conftest.py").exists()
        || root.join("tox.ini").exists() && read(root, "tox.ini").contains("pytest")
        || py.contains("pytest")
        || (has_py_tests(root) && (root.join("pyproject.toml").exists() || root.join("setup.py").exists()))
    {
        v.push(Runner::Pytest);
    }
    let composer = read(root, "composer.json");
    if composer.contains("pestphp/pest") {
        v.push(Runner::Pest);
    } else if composer.contains("phpunit")
        || root.join("phpunit.xml").exists()
        || root.join("phpunit.xml.dist").exists()
    {
        v.push(Runner::Phpunit);
    }
    if read(root, "Gemfile").contains("rspec") || root.join(".rspec").exists() {
        v.push(Runner::Rspec);
    }
    if root.join("pom.xml").exists() {
        v.push(Runner::Maven);
    }
    if ["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"]
        .iter()
        .any(|f| root.join(f).exists())
    {
        v.push(Runner::Gradle);
    }
    if dir_has(root, |n| n.ends_with(".sln") || n.ends_with(".csproj") || n.ends_with(".fsproj")) {
        v.push(Runner::Dotnet);
    }
    let cmake = read(root, "CMakeLists.txt");
    if cmake.contains("enable_testing") || cmake.contains("add_test") || cmake.contains("gtest_discover_tests") {
        v.push(Runner::Ctest);
    }
    v
}

/// The runner among `detected` that owns files with extension `ext`.
pub fn runner_for_ext(detected: &[Runner], ext: &str) -> Option<Runner> {
    detected.iter().copied().find(|r| r.exts().contains(&ext))
}

// ───────────── commands ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamKind {
    CargoText,
    LibtestJson,
    GoJson,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    JestJson,
    Junit,
    Trx,
    RspecJson,
}

pub enum Scope<'a> {
    All,
    /// Path relative to the project root.
    File(&'a str),
    /// One test: the case if the run knows it, else just a name found at the cursor.
    Test {
        file: Option<&'a str>,
        name: &'a str,
        case: Option<&'a Case>,
    },
    Failed(&'a [Case]),
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub runner: Runner,
    pub cmd: String,
    pub stream: Option<StreamKind>,
    pub report: Option<(PathBuf, Format)>,
    /// Directories (by name, searched under the root) whose `*.xml` are JUnit reports (maven, gradle).
    pub report_dirs: &'static [&'static str],
    /// Temp files / directories to delete once the report was read.
    pub cleanup: Vec<PathBuf>,
}

pub(crate) fn exec_prefix(root: &Path, local: &str, bin: &str) -> String {
    if root.join(local).is_file() { format!("./{local}") } else { bin.to_string() }
}

fn regex_any(names: &[String]) -> String {
    format!("({})", names.iter().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|"))
}

fn js_exec(root: &Path, bin: &str) -> String {
    format!("{} {bin}", Pm::detect(root).exec())
}

/// Last path piece of a reported test name (`mod::tests::name`, `Class.method`, `A › title`).
pub fn leaf_of(name: &str) -> String {
    let n = name.rsplit(" › ").next().unwrap_or(name);
    let n = n.split('[').next().unwrap_or(n).trim();
    let n = n.rsplit("::").next().unwrap_or(n);
    let n = if n.contains(' ') { n } else { n.rsplit('.').next().unwrap_or(n) };
    n.to_string()
}

/// Rust module path of a source file: `src/a/b.rs` -> `a::b`, `lib.rs`/`main.rs` -> none.
fn rust_module_of(rel: &str) -> Option<String> {
    let tail = rel.rsplit_once("/src/").map(|x| x.1).or_else(|| rel.strip_prefix("src/"))?;
    let stem = tail.strip_suffix(".rs")?;
    let stem = stem.strip_suffix("/mod").unwrap_or(stem);
    if matches!(stem, "lib" | "main") { None } else { Some(stem.replace('/', "::")) }
}

pub fn plan(runner: Runner, root: &Path, scope: &Scope, report: &Path, custom: (&str, &str)) -> Plan {
    let rp = shell_quote(&report.to_string_lossy());
    let mut p = Plan { runner, cmd: String::new(), stream: None, report: None, report_dirs: &[], cleanup: Vec::new() };
    let file_arg = |s: &Scope| -> Option<String> {
        match s {
            Scope::File(f) => Some(shell_quote(f)),
            Scope::Test { file: Some(f), .. } => Some(shell_quote(f)),
            _ => None,
        }
    };
    match runner {
        Runner::Custom => {
            p.cmd = custom.0.to_string();
            if !custom.1.is_empty() {
                let r = if Path::new(custom.1).is_absolute() { PathBuf::from(custom.1) } else { root.join(custom.1) };
                p.report = Some((r, Format::Junit));
            }
        }
        Runner::Nextest | Runner::Cargo => {
            let nextest = runner == Runner::Nextest;
            let mut filters: Vec<String> = Vec::new();
            let mut exact = false;
            let mut extra = String::new();
            match scope {
                Scope::All => {}
                Scope::File(f) => {
                    if let Some(stem) = f.strip_prefix("tests/").and_then(|t| t.strip_suffix(".rs")) {
                        extra = format!(" --test {}", shell_quote(stem.split('/').next().unwrap_or(stem)));
                    } else if let Some(m) = rust_module_of(f) {
                        filters.push(m);
                    }
                }
                Scope::Test { name, case, .. } => match case {
                    Some(c) => {
                        filters.push(c.id.clone());
                        exact = true;
                    }
                    None => filters.push((*name).to_string()),
                },
                Scope::Failed(cs) => {
                    filters.extend(cs.iter().map(|c| c.id.clone()));
                    exact = !filters.is_empty();
                }
            }
            if nextest {
                p.cmd = format!(
                    "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json{extra}"
                );
                for f in &filters {
                    p.cmd.push(' ');
                    p.cmd.push_str(&shell_quote(f));
                }
                p.stream = Some(StreamKind::LibtestJson);
            } else {
                p.cmd = format!("cargo test --no-fail-fast{extra}");
                if !filters.is_empty() {
                    if exact {
                        p.cmd.push_str(" -- --exact");
                    } else if filters.len() == 1 {
                        p.cmd.push_str(&format!(" {} --", shell_quote(&filters[0])));
                        filters.clear();
                    } else {
                        p.cmd.push_str(" --");
                    }
                    for f in &filters {
                        p.cmd.push(' ');
                        p.cmd.push_str(&shell_quote(f));
                    }
                }
                p.stream = Some(StreamKind::CargoText);
            }
        }
        Runner::Vitest | Runner::Jest => {
            let vitest = runner == Runner::Vitest;
            let mut c = if vitest { format!("{} run", js_exec(root, "vitest")) } else { js_exec(root, "jest") };
            if let Some(f) = file_arg(scope) {
                c.push(' ');
                c.push_str(&f);
            }
            match scope {
                Scope::Test { name, case, .. } => {
                    let n = case.map(leaf_title).unwrap_or_else(|| (*name).to_string());
                    c.push_str(&format!(" -t {}", shell_quote(&regex::escape(&n))));
                }
                Scope::Failed(cs) if !cs.is_empty() => {
                    let names: Vec<String> = cs.iter().map(leaf_title).collect();
                    c.push_str(&format!(" -t {}", shell_quote(&regex_any(&names))));
                }
                _ => {}
            }
            if vitest {
                c.push_str(&format!(" --reporter=default --reporter=json --outputFile.json={rp}"));
            } else {
                c.push_str(&format!(" --json --outputFile={rp}"));
            }
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::JestJson));
        }
        Runner::NodeTest => {
            let mut c = String::from("node --test");
            if let Scope::Test { name, case, .. } = scope {
                let n = case.map(leaf_title).unwrap_or_else(|| (*name).to_string());
                c.push_str(&format!(" --test-name-pattern={}", shell_quote(&regex::escape(&n))));
            }
            c.push_str(&format!(
                " --test-reporter=spec --test-reporter-destination=stdout --test-reporter=junit --test-reporter-destination={rp}"
            ));
            if let Some(f) = file_arg(scope) {
                c.push(' ');
                c.push_str(&f);
            }
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::Junit));
        }
        Runner::Bun => {
            let mut c = String::from("bun test");
            if let Some(f) = file_arg(scope) {
                c.push(' ');
                c.push_str(&f);
            }
            if let Scope::Test { name, case, .. } = scope {
                let n = case.map(leaf_title).unwrap_or_else(|| (*name).to_string());
                c.push_str(&format!(" -t {}", shell_quote(&n)));
            }
            c.push_str(&format!(" --reporter=junit --reporter-outfile={rp}"));
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::Junit));
        }
        Runner::Deno => {
            let mut c = format!("deno test --junit-path={rp}");
            if let Some(f) = file_arg(scope) {
                c.push(' ');
                c.push_str(&f);
            }
            if let Scope::Test { name, case, .. } = scope {
                let n = case.map(leaf_title).unwrap_or_else(|| (*name).to_string());
                c.push_str(&format!(" --filter {}", shell_quote(&n)));
            }
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::Junit));
        }
        Runner::Go => {
            let dir_of = |f: &str| match Path::new(f).parent().and_then(|d| d.to_str()) {
                Some("") | None => "./".to_string(),
                Some(d) => format!("./{d}"),
            };
            let anchored =
                |name: &str| name.split('/').map(|s| format!("^{}$", regex::escape(s))).collect::<Vec<_>>().join("/");
            let mut c = String::from("go test -json");
            match scope {
                Scope::All => c.push_str(" ./..."),
                Scope::File(f) => c.push_str(&format!(" {}", shell_quote(&dir_of(f)))),
                Scope::Test { file, name, case } => {
                    let n = case.map(|c| c.name.clone()).unwrap_or_else(|| (*name).to_string());
                    let pkg = file.map(dir_of).unwrap_or_else(|| "./...".into());
                    c.push_str(&format!(" {} -run {}", shell_quote(&pkg), shell_quote(&anchored(&n))));
                }
                Scope::Failed(cs) => {
                    let tops: Vec<String> = cs
                        .iter()
                        .map(|c| c.name.split('/').next().unwrap_or(&c.name).to_string())
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    c.push_str(" ./...");
                    if !tops.is_empty() {
                        c.push_str(&format!(" -run {}", shell_quote(&format!("^{}$", regex_any(&tops)))));
                    }
                }
            }
            p.cmd = c;
            p.stream = Some(StreamKind::GoJson);
        }
        Runner::Pytest => {
            let bin = ["venv", ".venv"]
                .iter()
                .map(|d| format!("{d}/bin/pytest"))
                .find(|b| root.join(b).is_file())
                .map(|b| format!("./{b}"))
                .unwrap_or_else(|| if on_path("pytest") { "pytest".into() } else { "python3 -m pytest".into() });
            let mut c = format!("{bin} --junitxml={rp} -o junit_family=xunit1");
            match scope {
                Scope::All => {}
                Scope::File(f) => c.push_str(&format!(" {}", shell_quote(f))),
                Scope::Test { file, name, case } => match case.filter(|c| c.id.contains("::")) {
                    Some(k) => c.push_str(&format!(" {}", shell_quote(&k.id))),
                    None => {
                        if let Some(f) = file {
                            c.push_str(&format!(" {}", shell_quote(f)));
                        }
                        c.push_str(&format!(" -k {}", shell_quote(&leaf_of(name))));
                    }
                },
                Scope::Failed(cs) => {
                    let ids: Vec<&String> = cs.iter().map(|k| &k.id).filter(|i| i.contains("::")).collect();
                    if ids.is_empty() {
                        c.push_str(" --lf");
                    }
                    for i in ids {
                        c.push_str(&format!(" {}", shell_quote(i)));
                    }
                }
            }
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::Junit));
        }
        Runner::Phpunit | Runner::Pest => {
            let (local, bin) =
                if runner == Runner::Pest { ("vendor/bin/pest", "pest") } else { ("vendor/bin/phpunit", "phpunit") };
            let mut c = format!("{} --log-junit {rp}", exec_prefix(root, local, bin));
            if let Scope::Test { name, case, .. } = scope {
                let n = case.map(leaf_title).unwrap_or_else(|| (*name).to_string());
                c.push_str(&format!(" --filter {}", shell_quote(&n)));
            }
            if let Scope::Failed(cs) = scope
                && !cs.is_empty()
            {
                let names: Vec<String> = cs.iter().map(leaf_title).collect();
                c.push_str(&format!(" --filter {}", shell_quote(&regex_any(&names))));
            }
            if let Some(f) = file_arg(scope) {
                c.push(' ');
                c.push_str(&f);
            }
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::Junit));
        }
        Runner::Rspec => {
            let bundle = if root.join("Gemfile").exists() { "bundle exec " } else { "" };
            let mut c = format!("{bundle}rspec --format progress --format json --out {rp}");
            match scope {
                Scope::All => {}
                Scope::File(f) => c.push_str(&format!(" {}", shell_quote(f))),
                Scope::Test { file, name, case } => {
                    if let Some(k) = case.filter(|k| k.id.contains(':')) {
                        c.push_str(&format!(" {}", shell_quote(&k.id)));
                    } else {
                        if let Some(f) = file {
                            c.push_str(&format!(" {}", shell_quote(f)));
                        }
                        c.push_str(&format!(" -e {}", shell_quote(name)));
                    }
                }
                Scope::Failed(cs) => {
                    for k in cs.iter() {
                        c.push_str(&format!(" {}", shell_quote(&k.id)));
                    }
                }
            }
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::RspecJson));
        }
        Runner::Maven => {
            let mvn = exec_prefix(root, "mvnw", "mvn");
            let mut c = format!("{mvn} test -Dsurefire.failIfNoSpecifiedTests=false");
            match scope {
                Scope::File(f) => c.push_str(&format!(" -Dtest={}", shell_quote(&file_stem(f)))),
                Scope::Test { name, case, file } => {
                    let class =
                        case.map(|k| class_simple(&k.class)).or_else(|| file.map(file_stem)).unwrap_or_default();
                    let m = case.map(|k| leaf_of(&k.name)).unwrap_or_else(|| (*name).to_string());
                    c.push_str(&format!(
                        " -Dtest={}",
                        shell_quote(&if class.is_empty() { m } else { format!("{class}#{m}") })
                    ));
                }
                Scope::Failed(cs) if !cs.is_empty() => {
                    let items: Vec<String> =
                        cs.iter().map(|k| format!("{}#{}", class_simple(&k.class), leaf_of(&k.name))).collect();
                    c.push_str(&format!(" -Dtest={}", shell_quote(&items.join(","))));
                }
                _ => {}
            }
            p.cmd = c;
            p.report_dirs = &["surefire-reports"];
        }
        Runner::Gradle => {
            let g = if root.join("gradlew").is_file() { "./gradlew" } else { "gradle" };
            let mut c = format!("{g} cleanTest test --continue");
            match scope {
                Scope::File(f) => c.push_str(&format!(" --tests {}", shell_quote(&format!("*{}", file_stem(f))))),
                Scope::Test { name, case, file } => {
                    let class =
                        case.map(|k| class_simple(&k.class)).or_else(|| file.map(file_stem)).unwrap_or_default();
                    let m = case.map(|k| leaf_of(&k.name)).unwrap_or_else(|| (*name).to_string());
                    c.push_str(&format!(" --tests {}", shell_quote(&format!("*{class}.{m}"))));
                }
                Scope::Failed(cs) => {
                    for k in cs.iter() {
                        c.push_str(&format!(
                            " --tests {}",
                            shell_quote(&format!("*{}.{}", class_simple(&k.class), leaf_of(&k.name)))
                        ));
                    }
                }
                Scope::All => {}
            }
            p.cmd = c;
            p.report_dirs = &["test-results"];
        }
        Runner::Dotnet => {
            let dir = report.with_extension("d");
            let mut c = format!(
                "dotnet test --results-directory {} --logger \"trx;LogFileName=report.trx\"",
                shell_quote(&dir.to_string_lossy())
            );
            let filter = match scope {
                Scope::Test { name, case, .. } => {
                    Some(case.map(|k| k.id.clone()).unwrap_or_else(|| (*name).to_string()))
                }
                Scope::File(f) => Some(file_stem(f)),
                Scope::Failed(cs) if !cs.is_empty() => {
                    Some(cs.iter().map(|k| k.id.clone()).collect::<Vec<_>>().join("|FullyQualifiedName~"))
                }
                _ => None,
            };
            if let Some(f) = filter {
                c.push_str(&format!(" --filter {}", shell_quote(&format!("FullyQualifiedName~{f}"))));
            }
            p.cmd = c;
            p.report = Some((dir.join("report.trx"), Format::Trx));
            p.cleanup.push(dir);
        }
        Runner::Ctest => {
            let build = ["build", "cmake-build-debug", "cmake-build-release", "out/build"]
                .iter()
                .find(|d| root.join(d).join("CTestTestfile.cmake").exists())
                .copied()
                .unwrap_or("build");
            let mut c = format!("ctest --test-dir {} --output-on-failure --output-junit {rp}", shell_quote(build));
            match scope {
                Scope::Test { name, case, .. } => {
                    let n = case.map(|k| k.name.clone()).unwrap_or_else(|| (*name).to_string());
                    c.push_str(&format!(" -R {}", shell_quote(&format!("^{}$", regex::escape(&n)))));
                }
                Scope::Failed(cs) if !cs.is_empty() => {
                    let names: Vec<String> = cs.iter().map(|k| k.name.clone()).collect();
                    c.push_str(&format!(" -R {}", shell_quote(&format!("^{}$", regex_any(&names)))));
                }
                _ => {}
            }
            p.cmd = c;
            p.report = Some((report.to_path_buf(), Format::Junit));
        }
    }
    if let Some((r, _)) = &p.report {
        p.cleanup.push(r.clone());
    }
    p
}

fn file_stem(f: &str) -> String {
    Path::new(f).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `com.acme.FooTest$Inner` -> `FooTest`.
fn class_simple(class: &str) -> String {
    let c = class.rsplit('.').next().unwrap_or(class);
    c.split('$').next().unwrap_or(c).to_string()
}

/// The title a name filter (`-t`, `--filter`) should use: the test's own title without its suites.
fn leaf_title(c: &Case) -> String {
    let n = c.name.rsplit(" › ").next().unwrap_or(&c.name);
    n.to_string()
}

impl Plan {
    pub fn remove_temp(&self) {
        for p in &self.cleanup {
            let _ = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
        }
    }
}

/// Read what the run left behind: the report file, or the JUnit files of maven / gradle.
pub fn read_report(plan: &Plan, root: &Path, since: SystemTime) -> Vec<Case> {
    let mut out = Vec::new();
    if let Some((path, fmt)) = &plan.report
        && let Ok(text) = std::fs::read_to_string(path)
    {
        out = match fmt {
            Format::JestJson => parse_jest_json(&text),
            Format::Junit => parse_junit(&text),
            Format::Trx => parse_trx(&text),
            Format::RspecJson => parse_rspec_json(&text),
        };
    }
    if !plan.report_dirs.is_empty() {
        let since = since - std::time::Duration::from_secs(2);
        for dir in find_dirs(root, plan.report_dirs) {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "xml") {
                    continue;
                }
                if e.metadata().and_then(|m| m.modified()).is_ok_and(|m| m < since) {
                    continue;
                }
                if let Ok(t) = std::fs::read_to_string(&p) {
                    out.extend(parse_junit(&t));
                }
            }
        }
    }
    for c in &mut out {
        fill_file(root, plan.runner, c);
    }
    out
}

fn find_dirs(root: &Path, names: &[&str]) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let walker = ignore::WalkBuilder::new(root).hidden(true).git_ignore(false).max_depth(Some(6)).build();
    for e in walker.flatten() {
        if e.file_type().is_some_and(|t| t.is_dir())
            && e.file_name().to_str().is_some_and(|n| names.contains(&n))
            && !e.path().components().any(|c| c.as_os_str() == "node_modules")
        {
            v.push(e.path().to_path_buf());
        }
    }
    v
}

// ───────────── parsers ─────────────

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn jest_status(s: &str) -> Status {
    match s {
        "passed" => Status::Passed,
        "failed" => Status::Failed,
        _ => Status::Skipped,
    }
}

/// vitest (`--reporter=json`) and jest (`--json`) share this shape.
pub fn parse_jest_json(text: &str) -> Vec<Case> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let mut out = Vec::new();
    for tr in v["testResults"].as_array().into_iter().flatten() {
        let file = tr["name"].as_str().unwrap_or("").to_string();
        let asserts = tr["assertionResults"].as_array().cloned().unwrap_or_default();
        if asserts.is_empty() && tr["status"] == "failed" {
            out.push(Case {
                group: file.clone(),
                name: "Test file failed to run".into(),
                id: format!("{file}::<file>"),
                status: Status::Failed,
                file: Some(file.clone()),
                message: strip_ansi(tr["message"].as_str().unwrap_or("")),
                ..Default::default()
            });
        }
        for a in &asserts {
            let title = a["title"].as_str().unwrap_or("").to_string();
            let mut parts: Vec<String> = a["ancestorTitles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(str::to_string))
                .filter(|s| !s.is_empty())
                .collect();
            parts.push(title);
            let name = parts.join(" › ");
            let msgs: Vec<String> = a["failureMessages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str())
                .map(strip_ansi)
                .collect();
            out.push(Case {
                group: file.clone(),
                id: format!("{file}::{name}"),
                name,
                status: jest_status(a["status"].as_str().unwrap_or("")),
                ms: num(&a["duration"]),
                file: Some(file.clone()),
                line: a["location"]["line"].as_u64().map(|l| (l as usize).saturating_sub(1)),
                message: msgs.join("\n"),
                ..Default::default()
            });
        }
    }
    out
}

pub fn parse_rspec_json(text: &str) -> Vec<Case> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let mut out = Vec::new();
    for e in v["examples"].as_array().into_iter().flatten() {
        let file = e["file_path"].as_str().unwrap_or("").trim_start_matches("./").to_string();
        let line = e["line_number"].as_u64().map(|l| l as usize);
        let status = match e["status"].as_str() {
            Some("passed") => Status::Passed,
            Some("failed") => Status::Failed,
            _ => Status::Skipped,
        };
        let mut msg = String::new();
        if let Some(ex) = e["exception"].as_object() {
            msg = strip_ansi(ex.get("message").and_then(|m| m.as_str()).unwrap_or(""));
            for b in ex.get("backtrace").and_then(|b| b.as_array()).into_iter().flatten().take(8) {
                if let Some(s) = b.as_str() {
                    msg.push_str(&format!("\n  {s}"));
                }
            }
        }
        out.push(Case {
            group: file.clone(),
            name: e["full_description"].as_str().or(e["description"].as_str()).unwrap_or("").to_string(),
            id: format!("{file}:{}", line.unwrap_or(0)),
            status,
            ms: num(&e["run_time"]).map(|s| s * 1000.0),
            file: Some(file),
            line: line.map(|l| l.saturating_sub(1)),
            message: msg,
            ..Default::default()
        });
    }
    out
}

/// JUnit XML: pytest, node, bun, deno, phpunit, pest, maven, gradle, ctest, GoogleTest and many more.
pub fn parse_junit(text: &str) -> Vec<Case> {
    let Ok(doc) = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "testcase") {
        let class = n.attribute("classname").unwrap_or("").to_string();
        let name = n.attribute("name").unwrap_or("").to_string();
        let suite = n
            .ancestors()
            .skip(1)
            .find(|a| a.is_element() && a.tag_name().name() == "testsuite")
            .and_then(|s| s.attribute("name"))
            .unwrap_or("")
            .to_string();
        let mut status = Status::Passed;
        let mut msg = String::new();
        for c in n.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "failure" | "error" => {
                    status = Status::Failed;
                    let head = c.attribute("message").unwrap_or("");
                    let body = c.text().unwrap_or("").trim();
                    msg = if body.is_empty() || body.starts_with(head) {
                        if body.is_empty() { head.to_string() } else { body.to_string() }
                    } else {
                        format!("{head}\n{body}")
                    };
                }
                "skipped" => status = Status::Skipped,
                "system-err" | "system-out" if status == Status::Failed && msg.is_empty() => {
                    msg = c.text().unwrap_or("").trim().to_string();
                }
                _ => {}
            }
        }
        let file = n.attribute("file").map(str::to_string);
        let group = file
            .clone()
            .filter(|f| !f.is_empty())
            .or_else(|| (!class.is_empty()).then(|| class.clone()))
            .unwrap_or(suite);
        out.push(Case {
            group,
            id: if class.is_empty() { name.clone() } else { format!("{class}::{name}") },
            class,
            name,
            status,
            ms: n.attribute("time").and_then(|t| t.parse::<f64>().ok()).map(|s| s * 1000.0),
            file,
            line: n.attribute("line").and_then(|l| l.parse::<usize>().ok()).map(|l| l.saturating_sub(1)),
            message: strip_ansi(&msg),
        });
    }
    out
}

fn trx_duration(s: &str) -> Option<f64> {
    let mut it = s.split(':');
    let (h, m, sec) =
        (it.next()?.parse::<f64>().ok()?, it.next()?.parse::<f64>().ok()?, it.next()?.parse::<f64>().ok()?);
    Some((h * 3600.0 + m * 60.0 + sec) * 1000.0)
}

/// Visual Studio test results (`dotnet test --logger trx`).
pub fn parse_trx(text: &str) -> Vec<Case> {
    let Ok(doc) = roxmltree::Document::parse(text) else { return Vec::new() };
    let mut classes: HashMap<&str, &str> = HashMap::new();
    for u in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "UnitTest") {
        let (Some(id), Some(tm)) =
            (u.attribute("id"), u.children().find(|c| c.is_element() && c.tag_name().name() == "TestMethod"))
        else {
            continue;
        };
        if let Some(cn) = tm.attribute("className") {
            classes.insert(id, cn);
        }
    }
    let mut out = Vec::new();
    for r in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "UnitTestResult") {
        let name = r.attribute("testName").unwrap_or("").to_string();
        let class = r.attribute("testId").and_then(|i| classes.get(i)).copied().unwrap_or("").to_string();
        let status = match r.attribute("outcome") {
            Some("Passed") => Status::Passed,
            Some("Failed") => Status::Failed,
            _ => Status::Skipped,
        };
        let mut msg = String::new();
        if let Some(info) = r.descendants().find(|n| n.is_element() && n.tag_name().name() == "ErrorInfo") {
            for part in ["Message", "StackTrace"] {
                if let Some(t) = info.children().find(|c| c.is_element() && c.tag_name().name() == part) {
                    if !msg.is_empty() {
                        msg.push('\n');
                    }
                    msg.push_str(t.text().unwrap_or("").trim());
                }
            }
        }
        let full = if class.is_empty() || name.contains('.') { name.clone() } else { format!("{class}.{name}") };
        out.push(Case {
            group: class.clone(),
            class,
            id: full,
            name,
            status,
            ms: r.attribute("duration").and_then(trx_duration),
            message: msg,
            ..Default::default()
        });
    }
    out
}

// ───────────── live streams ─────────────

/// Feeds on the output of runners that report as they go (cargo, nextest, go) and yields the
/// cases that changed with each line.
pub struct Collector {
    kind: StreamKind,
    bin: String,
    capture: Option<String>,
    seen: HashMap<String, Case>,
    go_out: HashMap<String, Vec<String>>,
    go_pkg_out: HashMap<String, Vec<String>>,
    go_pkgs_with_tests: HashSet<String>,
}

thread_local! {
    static CARGO_TEST: Regex = Regex::new(r"^test (.+?)(?: - should panic)? \.\.\. (ok|FAILED|ignored.*|bench.*)$").unwrap();
}

fn split_module(full: &str) -> (String, String) {
    match full.rsplit_once("::") {
        Some((m, n)) => (m.to_string(), n.to_string()),
        None => (String::new(), full.to_string()),
    }
}

impl Collector {
    pub fn new(kind: StreamKind) -> Self {
        Self {
            kind,
            bin: String::new(),
            capture: None,
            seen: HashMap::new(),
            go_out: HashMap::new(),
            go_pkg_out: HashMap::new(),
            go_pkgs_with_tests: HashSet::new(),
        }
    }

    pub fn feed(&mut self, raw: &str) -> Vec<Case> {
        match self.kind {
            StreamKind::CargoText => self.feed_cargo(raw),
            StreamKind::LibtestJson => self.feed_libtest(raw),
            StreamKind::GoJson => self.feed_go(raw),
        }
    }

    fn rust_case(&mut self, full: &str, status: Status, ms: Option<f64>) -> Case {
        let (module, name) = split_module(full);
        let doc = full.contains(" - ") && full.contains("(line ");
        let group = if doc {
            "doc-tests".to_string()
        } else if !module.is_empty() {
            module
        } else if !self.bin.is_empty() {
            self.bin.clone()
        } else {
            "tests".to_string()
        };
        let c = Case {
            group,
            class: self.bin.clone(),
            name: if doc { full.to_string() } else { name },
            id: full.to_string(),
            status,
            ms,
            ..Default::default()
        };
        let prev = self.seen.get(full).map(|p| p.message.clone()).unwrap_or_default();
        let c = Case { message: prev, ..c };
        self.seen.insert(full.to_string(), c.clone());
        c
    }

    fn end_capture(&mut self, out: &mut Vec<Case>) {
        if let Some(id) = self.capture.take()
            && let Some(c) = self.seen.get(&id)
        {
            out.push(c.clone());
        }
    }

    fn feed_cargo(&mut self, raw: &str) -> Vec<Case> {
        let line = strip_ansi(raw);
        let l = line.trim_end();
        let mut out = Vec::new();
        if self.capture.is_some() {
            if l.starts_with("---- ") || l == "failures:" || l.starts_with("test result:") {
                self.end_capture(&mut out);
            } else if let Some(id) = self.capture.clone()
                && let Some(c) = self.seen.get_mut(&id)
            {
                if !c.message.is_empty() || !l.is_empty() {
                    if !c.message.is_empty() {
                        c.message.push('\n');
                    }
                    c.message.push_str(l);
                }
                return out;
            }
        }
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("Running ").or_else(|| t.strip_prefix("Doc-tests ")) {
            self.bin = rest.split(" (").next().unwrap_or(rest).trim_start_matches("unittests ").to_string();
            return out;
        }
        if let Some(rest) = l.strip_prefix("---- ")
            && let Some(name) = rest.strip_suffix(" stdout ----")
        {
            let name = name.to_string();
            if self.seen.contains_key(&name) {
                self.capture = Some(name);
            }
            return out;
        }
        if let Some((name, res)) = CARGO_TEST.with(|re| re.captures(l).map(|c| (c[1].to_string(), c[2].to_string()))) {
            let status = if res == "ok" {
                Status::Passed
            } else if res == "FAILED" {
                Status::Failed
            } else {
                Status::Skipped
            };
            out.push(self.rust_case(&name, status, None));
        }
        out
    }

    fn feed_libtest(&mut self, raw: &str) -> Vec<Case> {
        let l = raw.trim();
        if !l.starts_with('{') {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(l) else { return Vec::new() };
        if v["type"] != "test" {
            return Vec::new();
        }
        let full = v["name"].as_str().unwrap_or("");
        let (bin, name) = match full.split_once('$') {
            Some((b, n)) => (b.to_string(), n.to_string()),
            None => (String::new(), full.to_string()),
        };
        if !bin.is_empty() {
            self.bin = bin;
        }
        let status = match v["event"].as_str() {
            Some("started") => Status::Running,
            Some("ok") => Status::Passed,
            Some("failed") | Some("timeout") => Status::Failed,
            Some("ignored") => Status::Skipped,
            _ => return Vec::new(),
        };
        let mut c = self.rust_case(&name, status, num(&v["exec_time"]).map(|s| s * 1000.0));
        if let Some(out) = v["stdout"].as_str().or(v["message"].as_str()) {
            let m = strip_ansi(out).trim().to_string();
            if !m.is_empty() {
                c.message = m;
                self.seen.insert(name, c.clone());
            }
        }
        vec![c]
    }

    fn feed_go(&mut self, raw: &str) -> Vec<Case> {
        let l = raw.trim();
        if !l.starts_with('{') {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(l) else { return Vec::new() };
        let pkg = v["Package"].as_str().unwrap_or("").to_string();
        let test = v["Test"].as_str().map(str::to_string);
        let action = v["Action"].as_str().unwrap_or("");
        if action == "output" {
            let text = v["Output"].as_str().unwrap_or("").to_string();
            match test {
                Some(t) => self.go_out.entry(format!("{pkg}\u{1}{t}")).or_default().push(text),
                None => self.go_pkg_out.entry(pkg).or_default().push(text),
            }
            return Vec::new();
        }
        let status = match action {
            "run" => Status::Running,
            "pass" => Status::Passed,
            "fail" => Status::Failed,
            "skip" => Status::Skipped,
            _ => return Vec::new(),
        };
        let Some(test) = test else {
            // Package-level result: a package that failed without running a test did not build.
            if status == Status::Failed && !self.go_pkgs_with_tests.contains(&pkg) {
                let msg = self.go_pkg_out.get(&pkg).map(|o| o.concat()).unwrap_or_default();
                let c = Case {
                    group: pkg.clone(),
                    name: "package failed to build".into(),
                    id: format!("{pkg}::build"),
                    status: Status::Failed,
                    message: msg.trim().to_string(),
                    ..Default::default()
                };
                return vec![c];
            }
            return Vec::new();
        };
        self.go_pkgs_with_tests.insert(pkg.clone());
        let key = format!("{pkg}\u{1}{test}");
        let msg = if status == Status::Failed {
            self.go_out
                .get(&key)
                .map(|o| {
                    o.iter()
                        .map(|s| s.trim_end_matches('\n'))
                        .filter(|s| !s.starts_with("=== ") && !s.trim_start().starts_with("--- "))
                        .map(|s| s.trim_start())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        vec![Case {
            group: pkg.clone(),
            class: pkg,
            name: test.clone(),
            id: key.replace('\u{1}', "."),
            status,
            ms: num(&v["Elapsed"]).map(|s| s * 1000.0),
            message: msg.trim().to_string(),
            ..Default::default()
        }]
    }
}

// ───────────── failure diff ─────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffPair {
    pub a_label: &'static str,
    pub a: String,
    pub b_label: &'static str,
    pub b: String,
}

/// Pull an expected/actual pair out of a failure message when the runner printed one.
pub fn diff_pair(message: &str) -> Option<DiffPair> {
    let clean = strip_ansi(message);
    let find = |keys: &[&str]| -> Option<String> {
        clean.lines().find_map(|l| {
            let t = l.trim();
            keys.iter().find_map(|k| t.strip_prefix(k)).map(|r| r.trim().to_string())
        })
    };
    if let (Some(l), Some(r)) = (find(&["left:"]), find(&["right:"])) {
        return Some(DiffPair { a_label: "left", a: l, b_label: "right", b: r });
    }
    if let (Some(e), Some(r)) =
        (find(&["Expected:", "expected:", "Expected :"]), find(&["Received:", "actual  :", "actual:", "Actual:"]))
    {
        return Some(DiffPair { a_label: "expected", a: e, b_label: "received", b: r });
    }
    None
}

// ───────────── locating tests in source ─────────────

/// The `/// leaf` identifier plus how to spot its definition line.
pub fn find_def_line(lines: &[String], name: &str) -> Option<usize> {
    let leaf = leaf_of(name);
    if leaf.is_empty() {
        return None;
    }
    let ident = leaf.chars().all(|c| c.is_alphanumeric() || c == '_');
    if ident {
        let re = Regex::new(&format!(r"\b{}\b\s*[(<]", regex::escape(&leaf))).ok()?;
        const DEFS: [&str; 9] = ["fn ", "func ", "def ", "function ", "void ", "fun ", "public ", "private ", "async "];
        if let Some(i) = lines.iter().position(|l| re.is_match(l) && DEFS.iter().any(|d| l.contains(d))) {
            return Some(i);
        }
    }
    // A title in `it('...')` / `test("...")`, or any call that spells the name.
    crate::tasks::find_test_line(lines, &leaf).or_else(|| {
        let re = Regex::new(&format!(r"\b{}\b\s*[(<]", regex::escape(&leaf))).ok()?;
        if ident {
            lines.iter().position(|l| re.is_match(l))
        } else {
            lines.iter().position(|l| l.contains(leaf.as_str()))
        }
    })
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Fill in `case.file` when the report only gave a class name (python, java, c# ...).
fn fill_file(root: &Path, runner: Runner, c: &mut Case) {
    if c.file.as_deref().is_some_and(|f| !f.is_empty()) {
        return;
    }
    c.file = None;
    if c.class.is_empty() {
        return;
    }
    if runner == Runner::Pytest {
        let parts: Vec<&str> = c.class.split('.').collect();
        for k in (1..=parts.len()).rev() {
            let rel = format!("{}.py", parts[..k].join("/"));
            if root.join(&rel).is_file() {
                c.file = Some(rel.clone());
                c.group = rel.clone();
                let chain = parts[k..].join("::");
                c.id =
                    if chain.is_empty() { format!("{rel}::{}", c.name) } else { format!("{rel}::{chain}::{}", c.name) };
                return;
            }
        }
    }
}

fn walk_files(root: &Path) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(root)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .take(20_000)
}

/// Where a case is defined: the reported file, else the class' file, else a search of the project.
pub fn locate(root: &Path, runner: Runner, c: &Case) -> Option<(PathBuf, usize)> {
    let read_lines = |p: &Path| -> Option<Vec<String>> {
        std::fs::read_to_string(p).ok().map(|t| t.lines().map(str::to_string).collect())
    };
    let direct = c.file.as_deref().filter(|f| !f.is_empty()).map(|f| {
        let p = PathBuf::from(f);
        if p.is_absolute() { p } else { root.join(f.trim_start_matches("./")) }
    });
    if let Some(p) = direct.filter(|p| p.is_file()) {
        let line = c.line.or_else(|| read_lines(&p).and_then(|l| find_def_line(&l, &c.name))).unwrap_or(0);
        return Some((p, line));
    }
    let exts = runner.exts();
    // Class-based: com.acme.FooTest -> .../FooTest.<ext>
    if !c.class.is_empty() && runner != Runner::Go {
        let stem = class_simple(&c.class);
        let rel_tail = c.class.replace('.', "/");
        let mut best: Option<PathBuf> = None;
        for p in walk_files(root) {
            let ok_ext = p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.is_empty() || exts.contains(&e));
            if ok_ext && p.file_stem().and_then(|s| s.to_str()) == Some(stem.as_str()) {
                let s = p.to_string_lossy();
                if s.contains(rel_tail.split('$').next().unwrap_or(&rel_tail)) {
                    best = Some(p);
                    break;
                }
                best.get_or_insert(p);
            }
        }
        if let Some(p) = best {
            let line = c.line.or_else(|| read_lines(&p).and_then(|l| find_def_line(&l, &c.name))).unwrap_or(0);
            return Some((p, line));
        }
    }
    // Content search (rust, go and anything the report gave no file for).
    let leaf = leaf_of(&c.name);
    if leaf.is_empty() || exts.is_empty() {
        return None;
    }
    let seg: Vec<String> = c.group.split("::").filter(|s| !s.is_empty()).map(str::to_string).collect();
    let mut best: Option<(i32, PathBuf, usize)> = None;
    for p in walk_files(root) {
        if !p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.contains(&e)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        if !text.contains(&leaf) {
            continue;
        }
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let Some(l) = find_def_line(&lines, &c.name) else { continue };
        let ps = p.to_string_lossy();
        let score = seg.iter().filter(|s| ps.contains(s.as_str())).count() as i32
            + i32::from(runner == Runner::Go && p.to_string_lossy().ends_with("_test.go")) * 2;
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, p, l));
        }
    }
    best.map(|(_, p, l)| (p, l))
}

/// Gutter marks for one open document: `(line, Some(passed))`, `None` for skipped / pending.
pub fn marks_for(
    root: &Path,
    runner: Runner,
    cases: &[Case],
    doc: &Path,
    lines: &[String],
) -> Vec<(usize, Option<bool>)> {
    let doc = canon(doc);
    let ext = doc.extension().and_then(|e| e.to_str()).unwrap_or("");
    let stem = doc.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut out: Vec<(usize, Option<bool>)> = Vec::new();
    for c in cases {
        let ok = match c.status {
            Status::Passed => Some(true),
            Status::Failed => Some(false),
            Status::Skipped => None,
            _ => continue,
        };
        let by_file = c.file.as_deref().filter(|f| !f.is_empty()).map(|f| {
            let p = PathBuf::from(f);
            canon(&if p.is_absolute() { p } else { root.join(f.trim_start_matches("./")) })
        });
        let line = if let Some(f) = by_file {
            if f != doc {
                continue;
            }
            c.line.or_else(|| find_def_line(lines, &c.name))
        } else if matches!(runner, Runner::Nextest | Runner::Cargo | Runner::Go) && runner.exts().contains(&ext) {
            find_def_line(lines, &c.name).filter(|l| {
                (l.saturating_sub(3)..*l)
                    .any(|i| lines[i].trim_start().starts_with("#[") || lines[i].contains("func Test"))
                    || lines[*l].contains("func Test")
            })
        } else if !c.class.is_empty() && class_simple(&c.class) == stem {
            find_def_line(lines, &c.name)
        } else {
            None
        };
        if let Some(l) = line
            && l < lines.len()
        {
            out.push((l, ok));
        }
    }
    // One mark per line: a failure wins.
    out.sort_by_key(|(l, ok)| (*l, *ok != Some(false)));
    out.dedup_by_key(|(l, _)| *l);
    out
}

// ───────────── test name at the cursor ─────────────

/// The test around `line` (0-based) in a file with extension `ext`.
pub fn test_name_at_cursor(ext: &str, lines: &[String], line: usize) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let upto = line.min(lines.len() - 1);
    let above = |re: &Regex| (0..=upto).rev().find_map(|i| re.captures(&lines[i]).map(|c| (i, c)));
    let attr_above =
        |i: usize, marks: &[&str]| (i.saturating_sub(3)..=i).any(|k| marks.iter().any(|m| lines[k].contains(m)));
    match ext {
        "rs" => {
            let re = Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)").ok()?;
            let (i, c) = above(&re)?;
            attr_above(i, &["#[test]", "::test]", "#[rstest", "#[test_case"]).then(|| c[1].to_string())
        }
        "py" => {
            let re = Regex::new(r"^\s*(?:async\s+)?def\s+(test\w*)\s*\(").ok()?;
            above(&re).map(|(_, c)| c[1].to_string())
        }
        "go" => {
            let re = Regex::new(r"^func\s+(Test\w+|Example\w+)\s*\(").ok()?;
            above(&re).map(|(_, c)| c[1].to_string())
        }
        "php" => {
            let re = Regex::new(r#"function\s+(test\w+)\s*\(|\b(?:it|test)\(\s*['"](.+?)['"]"#).ok()?;
            above(&re).map(|(_, c)| c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()).unwrap_or_default())
        }
        "rb" => {
            let re = Regex::new(r#"^\s*(?:it|specify|scenario)\s*\(?\s*['"](.+?)['"]"#).ok()?;
            above(&re).map(|(_, c)| c[1].to_string())
        }
        "java" | "kt" | "kts" | "scala" => {
            let re = Regex::new(r"\b(?:void|fun)\s+(\w+)\s*\(").ok()?;
            let (i, c) = above(&re)?;
            attr_above(i, &["@Test", "@ParameterizedTest", "@RepeatedTest"]).then(|| c[1].to_string())
        }
        "cs" | "fs" => {
            let re = Regex::new(r"\b(?:void|Task)\s+(\w+)\s*\(").ok()?;
            let (i, c) = above(&re)?;
            attr_above(i, &["[Fact", "[Theory", "[Test", "[TestMethod"]).then(|| c[1].to_string())
        }
        "c" | "cc" | "cpp" | "cxx" => {
            let re = Regex::new(r"^\s*TEST(?:_F|_P)?\s*\(\s*(\w+)\s*,\s*(\w+)").ok()?;
            above(&re).map(|(_, c)| format!("{}.{}", &c[1], &c[2]))
        }
        _ => crate::tasks::test_name_at(lines, line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-testrun-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn jest_and_vitest_json() {
        let json = r#"{"testResults":[{"name":"/p/a.test.ts","status":"failed","assertionResults":[
          {"ancestorTitles":["math"],"title":"adds","status":"passed","duration":2.5,"failureMessages":[]},
          {"ancestorTitles":[],"title":"breaks","status":"failed","duration":10,"failureMessages":["\u001b[31mExpected: 1\nReceived: 2\u001b[0m"],"location":{"line":12,"column":3}},
          {"ancestorTitles":[],"title":"later","status":"pending","failureMessages":[]}]},
          {"name":"/p/b.test.ts","status":"failed","message":"SyntaxError","assertionResults":[]}]}"#;
        let v = parse_jest_json(json);
        assert_eq!(v.len(), 4);
        assert_eq!((v[0].name.as_str(), v[0].status, v[0].ms), ("math › adds", Status::Passed, Some(2.5)));
        assert_eq!((v[1].status, v[1].line), (Status::Failed, Some(11)));
        assert!(v[1].message.contains("Expected: 1") && !v[1].message.contains('\u{1b}'));
        assert_eq!(v[2].status, Status::Skipped);
        assert_eq!((v[3].name.as_str(), v[3].status), ("Test file failed to run", Status::Failed));
        let d = diff_pair(&v[1].message).unwrap();
        assert_eq!((d.a.as_str(), d.b.as_str()), ("1", "2"));
    }

    #[test]
    fn junit_variants() {
        let xml = r#"<?xml version="1.0"?><testsuites><testsuite name="s">
          <testcase classname="tests.test_a.TestX" name="test_ok" time="0.012" file="tests/test_a.py" line="4"/>
          <testcase classname="tests.test_a.TestX" name="test_bad" time="1.5"><failure message="assert 1 == 2">trace here</failure></testcase>
          <testcase classname="C" name="skipme"><skipped/></testcase>
          <testcase classname="C" name="boom"><error message="npe"/></testcase></testsuite></testsuites>"#;
        let v = parse_junit(xml);
        assert_eq!(v.len(), 4);
        assert_eq!((v[0].status, v[0].ms, v[0].line), (Status::Passed, Some(12.0), Some(3)));
        assert_eq!(v[0].group, "tests/test_a.py");
        assert_eq!(v[1].status, Status::Failed);
        assert!(v[1].message.contains("assert 1 == 2") && v[1].message.contains("trace here"));
        assert_eq!(v[2].status, Status::Skipped);
        assert_eq!((v[3].status, v[3].message.as_str()), (Status::Failed, "npe"));
    }

    #[test]
    fn trx_results() {
        let xml = r#"<TestRun xmlns="x"><Results>
          <UnitTestResult testId="1" testName="Adds" outcome="Passed" duration="00:00:00.0120000"/>
          <UnitTestResult testId="2" testName="Subs" outcome="Failed" duration="00:00:01.5"><Output><ErrorInfo><Message>boom</Message><StackTrace>at X</StackTrace></ErrorInfo></Output></UnitTestResult>
          <UnitTestResult testId="3" testName="Nope" outcome="NotExecuted"/></Results>
          <TestDefinitions><UnitTest id="1"><TestMethod className="App.MathTests" name="Adds"/></UnitTest></TestDefinitions></TestRun>"#;
        let v = parse_trx(xml);
        assert_eq!(v.len(), 3);
        assert_eq!((v[0].class.as_str(), v[0].id.as_str()), ("App.MathTests", "App.MathTests.Adds"));
        assert_eq!(v[0].ms, Some(12.0));
        assert_eq!((v[1].status, v[1].message.as_str()), (Status::Failed, "boom\nat X"));
        assert_eq!(v[2].status, Status::Skipped);
    }

    #[test]
    fn rspec_json() {
        let json = r#"{"examples":[
          {"full_description":"Calc adds","status":"passed","file_path":"./spec/calc_spec.rb","line_number":5,"run_time":0.002},
          {"full_description":"Calc breaks","status":"failed","file_path":"./spec/calc_spec.rb","line_number":9,"run_time":0.1,"exception":{"message":"expected 1 got 2","backtrace":["a.rb:1"]}}]}"#;
        let v = parse_rspec_json(json);
        assert_eq!((v[0].file.as_deref(), v[0].line, v[0].ms), (Some("spec/calc_spec.rb"), Some(4), Some(2.0)));
        assert_eq!(v[1].id, "spec/calc_spec.rb:9");
        assert!(v[1].message.contains("expected 1 got 2") && v[1].message.contains("a.rb:1"));
    }

    #[test]
    fn cargo_text_stream() {
        let mut c = Collector::new(StreamKind::CargoText);
        let mut all: Vec<Case> = Vec::new();
        for l in [
            "     Running unittests src/lib.rs (target/debug/deps/x-1)",
            "running 3 tests",
            "test tasks::tests::good ... ok",
            "test tasks::tests::bad ... FAILED",
            "test tasks::tests::slow ... ignored, takes long",
            "",
            "failures:",
            "",
            "---- tasks::tests::bad stdout ----",
            "thread 'tasks::tests::bad' panicked at src/tasks.rs:9:5:",
            "assertion `left == right` failed",
            "  left: 1",
            " right: 2",
            "",
            "failures:",
            "    tasks::tests::bad",
            "",
            "test result: FAILED. 1 passed; 1 failed; 1 ignored",
        ] {
            all.extend(c.feed(l));
        }
        let last = |id: &str| all.iter().rev().find(|k| k.id == id).unwrap().clone();
        assert_eq!(last("tasks::tests::good").status, Status::Passed);
        assert_eq!(
            (last("tasks::tests::good").group.as_str(), last("tasks::tests::good").name.as_str()),
            ("tasks::tests", "good")
        );
        assert_eq!(last("tasks::tests::slow").status, Status::Skipped);
        let bad = last("tasks::tests::bad");
        assert_eq!(bad.status, Status::Failed);
        assert!(
            bad.message.contains("panicked at src/tasks.rs:9:5") && bad.message.contains("right: 2"),
            "{}",
            bad.message
        );
        assert!(!bad.message.contains("failures:"));
        let d = diff_pair(&bad.message).unwrap();
        assert_eq!((d.a_label, d.a.as_str(), d.b.as_str()), ("left", "1", "2"));
    }

    #[test]
    fn libtest_json_stream() {
        let mut c = Collector::new(StreamKind::LibtestJson);
        let a = c.feed(r#"{"type":"test","event":"started","name":"pkg::bin/x$m::t1"}"#);
        assert_eq!((a[0].status, a[0].id.as_str(), a[0].group.as_str()), (Status::Running, "m::t1", "m"));
        let b =
            c.feed(r#"{"type":"test","name":"pkg::bin/x$m::t1","event":"failed","exec_time":0.25,"stdout":"oops"}"#);
        assert_eq!((b[0].status, b[0].ms, b[0].message.as_str()), (Status::Failed, Some(250.0), "oops"));
        assert!(c.feed("not json").is_empty());
        assert!(c.feed(r#"{"type":"suite","event":"started"}"#).is_empty());
    }

    #[test]
    fn go_json_stream() {
        let mut c = Collector::new(StreamKind::GoJson);
        let mut all = Vec::new();
        for l in [
            r#"{"Action":"run","Package":"m/p","Test":"TestA"}"#,
            r#"{"Action":"output","Package":"m/p","Test":"TestA","Output":"=== RUN   TestA\n"}"#,
            r#"{"Action":"output","Package":"m/p","Test":"TestA","Output":"    a_test.go:7: nope\n"}"#,
            r#"{"Action":"output","Package":"m/p","Test":"TestA","Output":"--- FAIL: TestA (0.00s)\n"}"#,
            r#"{"Action":"fail","Package":"m/p","Test":"TestA","Elapsed":0.5}"#,
            r#"{"Action":"pass","Package":"m/p","Test":"TestB","Elapsed":0.1}"#,
            r#"{"Action":"fail","Package":"m/q"}"#,
        ] {
            all.extend(c.feed(l));
        }
        let a = all.iter().find(|k| k.name == "TestA" && k.status == Status::Failed).unwrap();
        assert_eq!((a.ms, a.message.as_str()), (Some(500.0), "a_test.go:7: nope"));
        assert!(all.iter().any(|k| k.name == "TestB" && k.status == Status::Passed));
        assert!(all.iter().any(|k| k.group == "m/q" && k.status == Status::Failed));
    }

    #[test]
    fn detection_by_project_files() {
        let d = tmp("detect");
        std::fs::write(d.join("go.mod"), "module x").unwrap();
        std::fs::write(d.join("package.json"), r#"{"devDependencies":{"vitest":"1","jest":"1"}}"#).unwrap();
        std::fs::write(d.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(d.join("conftest.py"), "").unwrap();
        std::fs::write(d.join("App.csproj"), "").unwrap();
        let v = detect(&d, true);
        assert_eq!(v[0], Runner::Custom);
        for r in [Runner::Vitest, Runner::Jest, Runner::Go, Runner::Pytest, Runner::Maven, Runner::Dotnet] {
            assert!(v.contains(&r), "{r:?} in {v:?}");
        }
        assert!(!v.contains(&Runner::Gradle));
        assert_eq!(runner_for_ext(&v, "py"), Some(Runner::Pytest));
        assert_eq!(runner_for_ext(&v, "tsx"), Some(Runner::Vitest));
        assert_eq!(runner_for_ext(&v, "rs"), None);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn commands_per_runner() {
        let d = tmp("plan");
        let rp = d.join("r.json");
        let p = |r, s: &Scope| plan(r, &d, s, &rp, ("", "")).cmd;
        let case = Case { id: "m::t".into(), name: "t".into(), class: "com.acme.FooTest".into(), ..Default::default() };
        assert!(p(Runner::Cargo, &Scope::All).starts_with("cargo test --no-fail-fast"));
        assert!(p(Runner::Cargo, &Scope::File("src/a/b.rs")).contains("'a::b' --"));
        assert!(p(Runner::Cargo, &Scope::File("tests/it.rs")).contains("--test 'it'"));
        assert!(
            p(Runner::Cargo, &Scope::Test { file: None, name: "t", case: Some(&case) }).ends_with("-- --exact 'm::t'")
        );
        assert!(p(Runner::Nextest, &Scope::All).contains("NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run"));
        let v = p(Runner::Vitest, &Scope::Test { file: Some("src/a.test.ts"), name: "adds it", case: None });
        assert!(v.contains("vitest run 'src/a.test.ts' -t 'adds it'") && v.contains("--outputFile.json="), "{v}");
        assert!(p(Runner::Jest, &Scope::File("a.test.js")).contains("jest 'a.test.js' --json --outputFile="));
        assert!(p(Runner::Go, &Scope::All).starts_with("go test -json ./..."));
        assert!(
            p(Runner::Go, &Scope::Test { file: Some("pkg/a_test.go"), name: "TestA/sub", case: None })
                .contains("'./pkg' -run '^TestA$/^sub$'")
        );
        assert!(p(Runner::Pytest, &Scope::File("tests/test_a.py")).contains("--junitxml="));
        assert!(
            p(Runner::Maven, &Scope::Test { file: None, name: "t", case: Some(&case) }).contains("-Dtest='FooTest#t'")
        );
        assert!(
            p(Runner::Gradle, &Scope::Test { file: None, name: "t", case: Some(&case) })
                .contains("--tests '*FooTest.t'")
        );
        assert!(p(Runner::Dotnet, &Scope::All).contains("trx;LogFileName=report.trx"));
        assert!(p(Runner::Ctest, &Scope::All).starts_with("ctest --test-dir 'build'"));
        assert!(p(Runner::Rspec, &Scope::File("spec/a_spec.rb")).contains("--format json --out"));
        assert!(p(Runner::Phpunit, &Scope::All).contains("--log-junit"));
        let c = plan(Runner::Custom, &d, &Scope::All, &rp, ("make test", "out/junit.xml"));
        assert_eq!(c.cmd, "make test");
        assert_eq!(c.report.unwrap().0, d.join("out/junit.xml"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn names_at_the_cursor() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            test_name_at_cursor("rs", &l(&["#[test]", "fn works() {", "  assert!(true)"]), 2).as_deref(),
            Some("works")
        );
        assert_eq!(test_name_at_cursor("rs", &l(&["fn helper() {", "  1"]), 1), None);
        assert_eq!(
            test_name_at_cursor("py", &l(&["class T:", "    def test_a(self):", "        pass"]), 2).as_deref(),
            Some("test_a")
        );
        assert_eq!(test_name_at_cursor("go", &l(&["func TestX(t *testing.T) {", "}"]), 1).as_deref(), Some("TestX"));
        assert_eq!(test_name_at_cursor("java", &l(&["@Test", "void adds() {", "}"]), 2).as_deref(), Some("adds"));
        assert_eq!(test_name_at_cursor("cs", &l(&["[Fact]", "public void Adds() {", "}"]), 2).as_deref(), Some("Adds"));
        assert_eq!(test_name_at_cursor("cpp", &l(&["TEST(Math, Adds) {", "}"]), 1).as_deref(), Some("Math.Adds"));
        assert_eq!(test_name_at_cursor("ts", &l(&["it('does it', () => {", "})"]), 1).as_deref(), Some("does it"));
        assert_eq!(test_name_at_cursor("rb", &l(&["  it 'adds' do", "  end"]), 1).as_deref(), Some("adds"));
    }

    #[test]
    fn locating_and_gutter_marks() {
        let d = tmp("locate");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(
            d.join("src/lib.rs"),
            "pub fn a() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {}\n}\n",
        )
        .unwrap();
        std::fs::write(d.join("a.test.ts"), "describe('m', () => {\n  it('adds', () => {})\n})\n").unwrap();
        let rust = Case {
            group: "tests".into(),
            name: "adds".into(),
            id: "tests::adds".into(),
            status: Status::Failed,
            ..Default::default()
        };
        let (f, l) = locate(&d, Runner::Cargo, &rust).unwrap();
        assert!(f.ends_with("src/lib.rs"));
        assert_eq!(l, 5);
        let js = Case {
            name: "m › adds".into(),
            file: Some(d.join("a.test.ts").to_string_lossy().into_owned()),
            status: Status::Passed,
            ..Default::default()
        };
        assert_eq!(locate(&d, Runner::Vitest, &js).unwrap().1, 1);
        let lines: Vec<String> =
            std::fs::read_to_string(d.join("src/lib.rs")).unwrap().lines().map(str::to_string).collect();
        let m = marks_for(&d, Runner::Cargo, &[rust.clone()], &d.join("src/lib.rs"), &lines);
        assert_eq!(m, vec![(5, Some(false))]);
        let jl: Vec<String> =
            std::fs::read_to_string(d.join("a.test.ts")).unwrap().lines().map(str::to_string).collect();
        assert_eq!(marks_for(&d, Runner::Vitest, &[js], &d.join("a.test.ts"), &jl), vec![(1, Some(true))]);
        assert!(marks_for(&d, Runner::Vitest, &[rust], &d.join("a.test.ts"), &jl).is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn counts_and_leafs() {
        let mk = |s| Case { status: s, ..Default::default() };
        let c = Counts::of(&[mk(Status::Passed), mk(Status::Passed), mk(Status::Failed), mk(Status::Running)]);
        assert_eq!((c.passed, c.failed, c.running, c.total()), (2, 1, 1, 4));
        assert_eq!(leaf_of("a::b::c"), "c");
        assert_eq!(leaf_of("suite › sub › does it"), "does it");
        assert_eq!(leaf_of("com.acme.Foo.method"), "method");
        assert_eq!(leaf_of("test_x[param-1]"), "test_x");
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m\r"), "red");
    }
}
