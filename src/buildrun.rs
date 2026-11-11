//! Build & Run without any UI: the build tools a project is set up for (as [`Target`]s, one shell
//! command each, grouped by tool) and the problem matchers that turn compiler output into
//! [`Problem`]s (file, position, severity, message). The panel lives in `app_build.rs`.

use crate::{
    tasks::Pm,
    testrun::{exec_prefix, read, strip_ansi},
};
use regex::Regex;
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

// ───────────── targets ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tool {
    /// The user's own `build_command` / `run_command`.
    Custom,
    Cargo,
    Node,
    Maven,
    Gradle,
    Dotnet,
    Go,
    Cmake,
    Make,
}

impl Tool {
    fn key(self) -> &'static str {
        match self {
            Tool::Custom => "custom",
            Tool::Cargo => "cargo",
            Tool::Node => "node",
            Tool::Maven => "maven",
            Tool::Gradle => "gradle",
            Tool::Dotnet => "dotnet",
            Tool::Go => "go",
            Tool::Cmake => "cmake",
            Tool::Make => "make",
        }
    }

    /// A file name whose icon stands for the tool (`None`: a generic one).
    pub fn marker(self) -> Option<&'static str> {
        match self {
            Tool::Custom => None,
            Tool::Cargo => Some("Cargo.toml"),
            Tool::Node => Some("package.json"),
            Tool::Maven => Some("pom.xml"),
            Tool::Gradle => Some("build.gradle"),
            Tool::Dotnet => Some("App.csproj"),
            Tool::Go => Some("go.mod"),
            Tool::Cmake => Some("CMakeLists.txt"),
            Tool::Make => Some("Makefile"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Build,
    Run,
    Clean,
    /// Anything else the tool offers (a Makefile's other targets, CMake's configure step).
    Task,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Build => "build",
            Kind::Run => "run",
            Kind::Clean => "clean",
            Kind::Task => "task",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Stable across sessions (remembered per project).
    pub id: String,
    pub tool: Tool,
    /// The header it is listed under: the tool, plus the folder when it is not the root.
    pub group: String,
    pub label: String,
    pub kind: Kind,
    /// Shell command, run in `cwd`.
    pub cmd: String,
    pub cwd: PathBuf,
    /// "release", "Debug", "Release"...: a build and a run of the same variant belong together.
    pub variant: String,
    /// Running it compiles first (`cargo run`, `go run`, `dotnet run`...), so its output is matched
    /// for problems too.
    pub compiles: bool,
}

impl Target {
    /// Does Build start it (a build or a free-form task)?
    pub fn is_build(&self) -> bool {
        matches!(self.kind, Kind::Build | Kind::Task)
    }

    fn variant(mut self, v: &str) -> Self {
        self.variant = v.to_string();
        self
    }

    fn compiling(mut self) -> Self {
        self.compiles = true;
        self
    }
}

/// Folders never searched for nested projects (dependencies, outputs, VCS).
const SKIP: &[&str] =
    &["node_modules", "target", "build", "dist", "out", "bin", "obj", "vendor", "venv", "__pycache__", "coverage"];

fn rel_of(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    !n.starts_with('.') && !SKIP.contains(&n.as_str())
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Where a tool lives: the root when it has the tool's file, else the immediate subfolders that do
/// (a `web/` or `backend/` folder holding the whole project).
fn tool_dirs(root: &Path, has: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    if has(root) {
        return vec![root.to_path_buf()];
    }
    subdirs(root).into_iter().filter(|d| has(d.as_path())).take(4).collect()
}

/// Files whose name passes `pred`, from `root` down to `depth` folders, shallowest first.
fn find_files(root: &Path, depth: usize, pred: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut level = vec![root.to_path_buf()];
    for _ in 0..=depth {
        let mut next = Vec::new();
        for d in &level {
            let mut here: Vec<PathBuf> = std::fs::read_dir(d)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                        .filter(|e| pred(&e.file_name().to_string_lossy()))
                        .map(|e| e.path())
                        .collect()
                })
                .unwrap_or_default();
            here.sort();
            out.extend(here);
            next.extend(subdirs(d));
        }
        if out.len() > 40 || next.is_empty() {
            break;
        }
        level = next;
    }
    out
}

/// Builds the targets of one tool in one folder.
struct Mk<'a> {
    tool: Tool,
    group: String,
    root: &'a Path,
    cwd: PathBuf,
}

impl<'a> Mk<'a> {
    fn new(tool: Tool, name: &str, root: &'a Path, cwd: &Path) -> Mk<'a> {
        let rel = rel_of(root, cwd);
        let group = if rel.is_empty() { name.to_string() } else { format!("{name} · {rel}") };
        Mk { tool, group, root, cwd: cwd.to_path_buf() }
    }

    fn t(&self, kind: Kind, label: impl Into<String>, cmd: impl Into<String>) -> Target {
        let label = label.into();
        Target {
            id: format!("{}:{}:{label}", self.tool.key(), rel_of(self.root, &self.cwd)),
            tool: self.tool,
            group: self.group.clone(),
            label,
            kind,
            cmd: cmd.into(),
            cwd: self.cwd.clone(),
            variant: String::new(),
            compiles: false,
        }
    }
}

/// Id of the custom build / run target (fixed, so editing the command keeps the selection).
pub const CUSTOM_BUILD: &str = "custom:build";
pub const CUSTOM_RUN: &str = "custom:run";

/// Every target this project offers, the user's own commands first, then tool by tool.
pub fn detect(root: &Path, build_cmd: &str, run_cmd: &str) -> Vec<Target> {
    let mut v = Vec::new();
    let custom = Mk::new(Tool::Custom, "Custom", root, root);
    if !build_cmd.trim().is_empty() {
        let mut t = custom.t(Kind::Build, build_cmd.trim(), build_cmd.trim()).compiling();
        t.id = CUSTOM_BUILD.into();
        v.push(t);
    }
    if !run_cmd.trim().is_empty() {
        let mut t = custom.t(Kind::Run, run_cmd.trim(), run_cmd.trim());
        t.id = CUSTOM_RUN.into();
        v.push(t);
    }
    for d in tool_dirs(root, |d| d.join("Cargo.toml").is_file()) {
        cargo(&mut v, root, &d);
    }
    for d in tool_dirs(root, |d| d.join("package.json").is_file()) {
        node(&mut v, root, &d);
    }
    for d in tool_dirs(root, |d| d.join("pom.xml").is_file()) {
        maven(&mut v, root, &d);
    }
    let gradle_files = ["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];
    for d in tool_dirs(root, |d| gradle_files.iter().any(|f| d.join(f).is_file())) {
        gradle(&mut v, root, &d);
    }
    dotnet(&mut v, root);
    for d in tool_dirs(root, |d| d.join("go.mod").is_file()) {
        go(&mut v, root, &d);
    }
    for d in tool_dirs(root, |d| d.join("CMakeLists.txt").is_file()) {
        cmake(&mut v, root, &d);
    }
    for d in tool_dirs(root, |d| MAKEFILES.iter().any(|f| d.join(f).is_file())) {
        make(&mut v, root, &d);
    }
    v
}

fn cargo(v: &mut Vec<Target>, root: &Path, dir: &Path) {
    let toml = read(dir, "Cargo.toml");
    let m = Mk::new(Tool::Cargo, "Cargo", root, dir);
    let bin = dir.join("src/main.rs").is_file()
        || dir.join("src/bin").is_dir()
        || toml.contains("[[bin]]")
        || toml.contains("[workspace]");
    v.push(m.t(Kind::Build, "cargo build", "cargo build --color=always"));
    if bin {
        v.push(m.t(Kind::Run, "cargo run", "cargo run --color=always").compiling());
    }
    v.push(m.t(Kind::Build, "cargo check", "cargo check --color=always"));
    v.push(m.t(Kind::Build, "cargo clippy", "cargo clippy --color=always"));
    v.push(m.t(Kind::Build, "cargo build --release", "cargo build --release --color=always").variant("release"));
    if bin {
        v.push(
            m.t(Kind::Run, "cargo run --release", "cargo run --release --color=always").variant("release").compiling(),
        );
    }
    v.push(m.t(Kind::Clean, "cargo clean", "cargo clean"));
}

fn node(v: &mut Vec<Target>, root: &Path, dir: &Path) {
    let Ok(j) = serde_json::from_str::<Value>(&read(dir, "package.json")) else { return };
    let Some(scripts) = j["scripts"].as_object() else { return };
    let locks = ["package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lockb", "bun.lock"];
    // A package of a workspace uses the lockfile (and so the package manager) of the root.
    let pm = if dir == root || locks.iter().any(|f| dir.join(f).exists()) { Pm::detect(dir) } else { Pm::detect(root) };
    let name = match pm {
        Pm::Npm => "npm",
        Pm::Pnpm => "pnpm",
        Pm::Yarn => "yarn",
        Pm::Bun => "bun",
    };
    let m = Mk::new(Tool::Node, name, root, dir);
    for (s, kind) in [
        ("build", Kind::Build),
        ("compile", Kind::Build),
        ("typecheck", Kind::Build),
        ("dev", Kind::Run),
        ("start", Kind::Run),
        ("preview", Kind::Run),
        ("clean", Kind::Clean),
    ] {
        if scripts.contains_key(s) {
            v.push(m.t(kind, pm.run(s), pm.run(s)));
        }
    }
}

fn maven(v: &mut Vec<Target>, root: &Path, dir: &Path) {
    let pom = read(dir, "pom.xml");
    let mvn = exec_prefix(dir, "mvnw", "mvn");
    let m = Mk::new(Tool::Maven, "Maven", root, dir);
    v.push(m.t(Kind::Build, format!("{mvn} compile"), format!("{mvn} -B -q compile")));
    v.push(m.t(Kind::Build, format!("{mvn} package"), format!("{mvn} -B package")));
    if pom.contains("spring-boot-maven-plugin") {
        v.push(m.t(Kind::Run, format!("{mvn} spring-boot:run"), format!("{mvn} -B spring-boot:run")).compiling());
    } else if pom.contains("exec-maven-plugin") {
        v.push(m.t(Kind::Run, format!("{mvn} exec:java"), format!("{mvn} -B -q compile exec:java")).compiling());
    }
    v.push(m.t(Kind::Clean, format!("{mvn} clean"), format!("{mvn} -B clean")));
}

/// Does a Gradle build script apply the `application` plugin?
pub fn gradle_app(script: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)(\bid\s*\(?\s*["']application["']|apply\s+plugin\s*:\s*["']application["']|^\s*application\s*(\{.*)?$)"#,
        )
        .unwrap()
    })
    .is_match(script)
}

fn gradle(v: &mut Vec<Target>, root: &Path, dir: &Path) {
    let gw = exec_prefix(dir, "gradlew", "gradle");
    // The application plugin often sits in a subproject (`app/build.gradle` of `gradle init`).
    let mut scripts = String::new();
    for d in std::iter::once(dir.to_path_buf()).chain(subdirs(dir)) {
        for f in ["build.gradle", "build.gradle.kts"] {
            scripts.push_str(&read(&d, f));
            scripts.push('\n');
        }
    }
    let m = Mk::new(Tool::Gradle, "Gradle", root, dir);
    for task in ["build", "assemble", "classes"] {
        v.push(m.t(Kind::Build, format!("{gw} {task}"), format!("{gw} {task} --console=plain")));
    }
    if scripts.contains("org.springframework.boot") {
        v.push(m.t(Kind::Run, format!("{gw} bootRun"), format!("{gw} bootRun --console=plain")).compiling());
    }
    if gradle_app(&scripts) {
        v.push(m.t(Kind::Run, format!("{gw} run"), format!("{gw} run --console=plain")).compiling());
    }
    v.push(m.t(Kind::Clean, format!("{gw} clean"), format!("{gw} clean --console=plain")));
}

/// Is this .NET project an executable (a console app, a web app or a worker; never a test project)?
pub fn dotnet_exe(proj: &str) -> bool {
    let l = proj.to_ascii_lowercase();
    if l.contains("microsoft.net.test.sdk") || l.contains("<istestproject>true") {
        return false;
    }
    l.contains("<outputtype>exe</outputtype>")
        || l.contains("<outputtype>winexe</outputtype>")
        || l.contains("microsoft.net.sdk.web")
        || l.contains("microsoft.net.sdk.worker")
}

fn dotnet(v: &mut Vec<Target>, root: &Path) {
    let exts = [".sln", ".slnx", ".csproj", ".fsproj", ".vbproj"];
    let files = find_files(root, 3, &|n: &str| exts.iter().any(|e| n.ends_with(e)));
    if files.is_empty() {
        return;
    }
    let is_sln = |p: &PathBuf| p.extension().is_some_and(|e| e == "sln" || e == "slnx");
    let projects: Vec<&PathBuf> = files.iter().filter(|p| !is_sln(*p)).collect();
    // The shallowest solution builds everything; without one, each project is a target.
    let entries: Vec<&PathBuf> = match files.iter().find(|p| is_sln(*p)) {
        Some(s) => vec![s],
        None => projects.iter().copied().take(4).collect(),
    };
    let exes: Vec<&PathBuf> = projects
        .iter()
        .copied()
        .filter(|p| dotnet_exe(&std::fs::read_to_string(p).unwrap_or_default()))
        .take(6)
        .collect();
    let m = Mk::new(Tool::Dotnet, ".NET", root, root);
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = |p: &Path| p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for cfg in ["Debug", "Release"] {
        for e in &entries {
            let rel = rel_of(root, e);
            v.push(
                m.t(
                    Kind::Build,
                    format!("dotnet build {} ({cfg})", name(e.as_path())),
                    format!("dotnet build \"{rel}\" -c {cfg} -nologo -clp:NoSummary"),
                )
                .variant(cfg),
            );
        }
        for p in &exes {
            let rel = rel_of(root, p);
            v.push(
                m.t(
                    Kind::Run,
                    format!("dotnet run {} ({cfg})", stem(p.as_path())),
                    format!("dotnet run --project \"{rel}\" -c {cfg}"),
                )
                .variant(cfg)
                .compiling(),
            );
        }
    }
    for e in &entries {
        let rel = rel_of(root, e);
        v.push(m.t(
            Kind::Clean,
            format!("dotnet clean {}", name(e.as_path())),
            format!("dotnet clean \"{rel}\" -nologo"),
        ));
    }
}

/// Does `dir` hold a Go `main` package (a non-test `.go` file saying `package main`)?
fn go_main(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(".go")
                && !n.ends_with("_test.go")
                && std::fs::read_to_string(e.path()).is_ok_and(|t| t.lines().any(|l| l.trim() == "package main"))
        })
    })
}

fn go(v: &mut Vec<Target>, root: &Path, dir: &Path) {
    let m = Mk::new(Tool::Go, "Go", root, dir);
    v.push(m.t(Kind::Build, "go build ./...", "go build ./..."));
    v.push(m.t(Kind::Build, "go vet ./...", "go vet ./..."));
    if go_main(dir) {
        v.push(m.t(Kind::Run, "go run .", "go run .").compiling());
    } else {
        for d in subdirs(&dir.join("cmd")).into_iter().filter(|d| go_main(d)).take(4) {
            let r = rel_of(dir, &d);
            v.push(m.t(Kind::Run, format!("go run ./{r}"), format!("go run ./{r}")).compiling());
        }
    }
    v.push(m.t(Kind::Clean, "go clean", "go clean"));
}

/// The executable a CMakeLists.txt builds (its first `add_executable` with a plain name or
/// `${PROJECT_NAME}`).
pub fn cmake_exe(text: &str) -> Option<String> {
    static EXE: OnceLock<Regex> = OnceLock::new();
    static PROJ: OnceLock<Regex> = OnceLock::new();
    let exe =
        EXE.get_or_init(|| Regex::new(r"(?i)add_executable\s*\(\s*([A-Za-z0-9_.+\-]+|\$\{PROJECT_NAME\})").unwrap());
    let name = exe.captures(text)?[1].to_string();
    if name == "${PROJECT_NAME}" {
        let proj = PROJ.get_or_init(|| Regex::new(r"(?i)\bproject\s*\(\s*([A-Za-z0-9_.+\-]+)").unwrap());
        return proj.captures(text).map(|c| c[1].to_string());
    }
    Some(name)
}

fn cmake(v: &mut Vec<Target>, root: &Path, dir: &Path) {
    let text = read(dir, "CMakeLists.txt");
    let m = Mk::new(Tool::Cmake, "CMake", root, dir);
    v.push(m.t(Kind::Build, "cmake --build build", "cmake --build build"));
    v.push(m.t(Kind::Task, "cmake configure", "cmake -S . -B build"));
    if let Some(exe) = cmake_exe(&text) {
        v.push(m.t(Kind::Run, format!("run {exe}"), format!("cmake --build build && ./build/{exe}")).compiling());
    }
    v.push(m.t(Kind::Clean, "cmake clean", "cmake --build build --target clean"));
}

const MAKEFILES: &[&str] = &["GNUmakefile", "makefile", "Makefile"];

/// Explicit targets of a Makefile, in order: no pattern rules, no `.SPECIAL` targets, no files
/// (`main.o`, `out/x`), no variable assignments.
pub fn make_targets(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with(['\t', ' ', '#', '.']) {
            continue;
        }
        let Some(i) = line.find(':') else { continue };
        let (left, rest) = (&line[..i], &line[i + 1..]);
        // `VAR := x`, `VAR ::= x`, `VAR = a:b`, `$(OUT): ...`
        if rest.starts_with('=') || rest.starts_with(":=") || left.contains(['=', '$', '%']) {
            continue;
        }
        for name in left.split_whitespace() {
            let ok = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if ok && !name.starts_with('-') && !out.iter().any(|o| o == name) && out.len() < 40 {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn make(v: &mut Vec<Target>, root: &Path, dir: &Path) {
    let text = MAKEFILES.iter().map(|f| read(dir, f)).find(|t| !t.is_empty()).unwrap_or_default();
    let m = Mk::new(Tool::Make, "Make", root, dir);
    for name in make_targets(&text) {
        let kind = match name.as_str() {
            "clean" | "distclean" | "mrproper" => Kind::Clean,
            "run" | "start" | "serve" | "dev" => Kind::Run,
            "all" | "build" | "compile" | "default" | "release" | "debug" => Kind::Build,
            _ => Kind::Task,
        };
        let t = m.t(kind, format!("make {name}"), format!("make {name}"));
        v.push(if kind == Kind::Run { t.compiling() } else { t });
    }
}

/// The command to start for `t` right now: a CMake build configures `build/` first when it was
/// never configured.
pub fn prepare(t: &Target) -> String {
    if t.tool == Tool::Cmake
        && t.cmd.starts_with("cmake --build build")
        && !t.cwd.join("build").join("CMakeCache.txt").is_file()
    {
        return format!("cmake -S . -B build && {}", t.cmd);
    }
    t.cmd.clone()
}

/// The target selected when the project has no remembered one: the first build (the custom one
/// comes first), else the first task.
pub fn default_target(ts: &[Target]) -> Option<&Target> {
    ts.iter()
        .find(|t| t.kind == Kind::Build)
        .or_else(|| ts.iter().find(|t| t.kind == Kind::Task))
        .or_else(|| ts.iter().find(|t| t.kind != Kind::Clean))
}

/// A target of `kind` next to `sel` (same tool, same folder), the same variant preferred.
fn beside<'a>(ts: &'a [Target], sel: &Target, kind: Kind) -> Option<&'a Target> {
    let near = || ts.iter().filter(|t| t.kind == kind && t.tool == sel.tool && t.cwd == sel.cwd);
    near().find(|t| t.variant == sel.variant).or_else(|| near().next())
}

/// What Build starts with `sel` chosen: `sel` itself when it builds, else the build beside it.
pub fn build_for<'a>(ts: &'a [Target], sel: Option<&Target>) -> Option<&'a Target> {
    match sel {
        Some(s) if s.is_build() => ts.iter().find(|t| t.id == s.id),
        Some(s) => beside(ts, s, Kind::Build).or_else(|| default_target(ts).filter(|t| t.is_build())),
        None => default_target(ts).filter(|t| t.is_build()),
    }
}

/// What Run starts: `sel` when it is a run target, else the run beside it, else the first one.
pub fn run_for<'a>(ts: &'a [Target], sel: Option<&Target>) -> Option<&'a Target> {
    let first = || ts.iter().find(|t| t.kind == Kind::Run);
    match sel {
        Some(s) if s.kind == Kind::Run => ts.iter().find(|t| t.id == s.id),
        Some(s) => beside(ts, s, Kind::Run).or_else(first),
        None => first(),
    }
}

/// What Clean starts: the clean beside `sel`, else the first one.
pub fn clean_for<'a>(ts: &'a [Target], sel: Option<&Target>) -> Option<&'a Target> {
    sel.and_then(|s| beside(ts, s, Kind::Clean)).or_else(|| ts.iter().find(|t| t.kind == Kind::Clean))
}

// ───────────── problems ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum Sev {
    Error,
    Warning,
    Info,
}

impl Sev {
    /// LSP severity (1 error, 2 warning, 3 info).
    pub fn lsp(self) -> u8 {
        match self {
            Sev::Error => 1,
            Sev::Warning => 2,
            Sev::Info => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: PathBuf,
    /// 0-based.
    pub line: usize,
    /// 0-based.
    pub col: usize,
    pub sev: Sev,
    pub message: String,
    pub code: String,
    /// Index of the log line it was reported on (counting from the first line of the run).
    pub log: usize,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

impl Counts {
    pub fn of<'a>(ps: impl IntoIterator<Item = &'a Problem>) -> Counts {
        let mut c = Counts::default();
        for p in ps {
            match p.sev {
                Sev::Error => c.errors += 1,
                Sev::Warning => c.warnings += 1,
                Sev::Info => c.infos += 1,
            }
        }
        c
    }

    pub fn total(&self) -> usize {
        self.errors + self.warnings + self.infos
    }
}

/// "4 errors, 2 warnings", "1 error", "no problems".
pub fn summary(c: Counts) -> String {
    let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
    match (c.errors, c.warnings) {
        (0, 0) => "no problems".to_string(),
        (e, 0) => n(e, "error", "errors"),
        (0, w) => n(w, "warning", "warnings"),
        (e, w) => format!("{}, {}", n(e, "error", "errors"), n(w, "warning", "warnings")),
    }
}

/// What one output line says.
#[derive(Debug, PartialEq)]
enum Line {
    /// A complete diagnostic.
    Hit {
        file: String,
        line: usize,
        col: usize,
        sev: Sev,
        code: String,
        msg: String,
    },
    /// rustc's `error[E0308]: msg`, whose location follows on the next line.
    Head {
        sev: Sev,
        code: String,
        msg: String,
    },
    /// rustc's `  --> src/main.rs:12:5`.
    Arrow {
        file: String,
        line: usize,
        col: usize,
    },
    Nothing,
}

fn sev_of(s: &str) -> Option<Sev> {
    match s.to_ascii_lowercase().as_str() {
        "error" | "fatal error" | "e" => Some(Sev::Error),
        "warning" | "warn" | "w" => Some(Sev::Warning),
        "info" | "message" => Some(Sev::Info),
        _ => None, // note, remark, help: context of the diagnostic before them
    }
}

/// 1-based number to 0-based (a missing one is the first).
fn num(m: Option<regex::Match>) -> usize {
    m.and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1).saturating_sub(1)
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

fn hit(file: &str, line: Option<regex::Match>, col: Option<regex::Match>, sev: &str, code: &str, msg: &str) -> Line {
    match sev_of(sev) {
        Some(sev) => Line::Hit {
            file: file.to_string(),
            line: num(line),
            col: num(col),
            sev,
            code: code.to_string(),
            msg: msg.trim().to_string(),
        },
        None => Line::Nothing,
    }
}

fn classify(l: &str) -> Line {
    static MAVEN: OnceLock<Regex> = OnceLock::new();
    static KOTLIN_OLD: OnceLock<Regex> = OnceLock::new();
    static KOTLIN: OnceLock<Regex> = OnceLock::new();
    static PAREN: OnceLock<Regex> = OnceLock::new();
    static TSC: OnceLock<Regex> = OnceLock::new();
    static ARROW: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static GENERIC: OnceLock<Regex> = OnceLock::new();
    static GO: OnceLock<Regex> = OnceLock::new();
    let t = l.trim_end();
    // Maven: `[ERROR] /abs/Foo.java:[12,5] msg`
    if let Some(c) = re(&MAVEN, r"^\[(ERROR|WARNING|WARN)\]\s+(.+?):\[(\d+),(\d+)\]\s*(.*)$").captures(t) {
        let msg = c[5].trim();
        let msg = msg.strip_prefix("error:").or_else(|| msg.strip_prefix("warning:")).unwrap_or(msg);
        return hit(&c[2], c.get(3), c.get(4), &c[1], "", msg);
    }
    // Kotlin (K1): `e: /abs/Foo.kt: (12, 5): msg`
    if let Some(c) = re(&KOTLIN_OLD, r"^([ew]): (.+?): \((\d+), (\d+)\): (.*)$").captures(t) {
        return hit(&c[2], c.get(3), c.get(4), &c[1], "", &c[5]);
    }
    // Kotlin (K2): `e: file:///abs/Foo.kt:12:5 msg`
    if let Some(c) = re(&KOTLIN, r"^([ew]): (.+?):(\d+):(\d+):? (.*)$").captures(t) {
        return hit(&c[2], c.get(3), c.get(4), &c[1], "", &c[5]);
    }
    // tsc and MSBuild: `src/a.ts(12,5): error TS2322: msg`, `/abs/P.cs(12,5): error CS1002: msg [/abs/App.csproj]`
    if let Some(c) = re(
        &PAREN,
        r"^\s*(.+?)\((\d+),(\d+)(?:,\d+,\d+)?\)\s*:\s*(error|warning|info|message)\s*([A-Za-z]+\d+)?\s*:\s*(.*?)(?:\s+\[[^\[\]]*proj\])?\s*$",
    )
    .captures(t)
    {
        return hit(&c[1], c.get(2), c.get(3), &c[4], c.get(5).map_or("", |m| m.as_str()), &c[6]);
    }
    // tsc --pretty: `src/a.ts:12:5 - error TS2322: msg`
    if let Some(c) = re(&TSC, r"^(.+?):(\d+):(\d+) - (error|warning|info) ([A-Za-z]+\d+): (.*)$").captures(t) {
        return hit(&c[1], c.get(2), c.get(3), &c[4], &c[5], &c[6]);
    }
    if let Some(c) = re(&ARROW, r"^\s*--> (.+?):(\d+):(\d+)$").captures(t) {
        return Line::Arrow { file: c[1].to_string(), line: num(c.get(2)), col: num(c.get(3)) };
    }
    if let Some(c) = re(&HEAD, r"^(error|warning)(?:\[([^\]]+)\])?: (.+)$").captures(t) {
        return match sev_of(&c[1]) {
            Some(sev) => {
                Line::Head { sev, code: c.get(2).map_or("", |m| m.as_str()).to_string(), msg: c[3].trim().to_string() }
            }
            None => Line::Nothing,
        };
    }
    // gcc / clang / javac / rustc --message-format=short: `file.c:12:5: error: msg`, `Foo.java:12: error: msg`
    if let Some(c) = re(
        &GENERIC,
        r"^((?:[A-Za-z]:)?[^\s:][^:]*?):(\d+):(?:(\d+):)? (fatal error|error|warning|note|info|remark)(?:\[([^\]]+)\])?: (.*)$",
    )
    .captures(t)
    {
        return hit(&c[1], c.get(2), c.get(3), &c[4], c.get(5).map_or("", |m| m.as_str()), &c[6]);
    }
    // Go: `./main.go:12:5: undefined: x`
    if let Some(c) = re(&GO, r"^((?:\.{1,2}/)?[^\s:]+\.go):(\d+):(\d+): (.*)$").captures(t) {
        return hit(&c[1], c.get(2), c.get(3), "error", "", &c[4]);
    }
    Line::Nothing
}

fn decode_uri(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `a/./b/../c` -> `a/c`, without touching the disk.
fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The file a diagnostic names: `file://` URIs are decoded, relative paths are taken from the
/// tool's folder (then the root). A path outside the project is kept only when it exists.
pub fn resolve(root: &Path, cwd: &Path, raw: &str) -> Option<PathBuf> {
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() {
        return None;
    }
    let s = match raw.strip_prefix("file://") {
        Some(r) => {
            let d = decode_uri(r);
            let b = d.as_bytes();
            // file:///C:/x -> C:/x
            if b.len() > 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
                d[1..].to_string()
            } else {
                d
            }
        }
        None => raw.to_string(),
    };
    let p = PathBuf::from(&s);
    if p.is_absolute() {
        let p = clean(&p);
        return (p.starts_with(root) || p.is_file()).then_some(p);
    }
    for base in [cwd, root] {
        let c = clean(&base.join(&p));
        if c.is_file() {
            return Some(c);
        }
    }
    let c = clean(&cwd.join(&p));
    c.starts_with(root).then_some(c)
}

/// Turns a tool's output, line by line, into problems (deduplicated: MSBuild and Maven repeat
/// every diagnostic in their summary).
pub struct Matcher {
    root: PathBuf,
    cwd: PathBuf,
    head: Option<(Sev, String, String, usize)>,
    seen: HashSet<(PathBuf, usize, usize, Sev, String)>,
}

impl Matcher {
    pub fn new(root: &Path, cwd: &Path) -> Matcher {
        Matcher { root: root.to_path_buf(), cwd: cwd.to_path_buf(), head: None, seen: HashSet::new() }
    }

    /// Feed output line number `n`; returns the problem it completes, if it is a new one.
    pub fn feed(&mut self, raw: &str, n: usize) -> Option<Problem> {
        let text = strip_ansi(raw);
        let head = self.head.take();
        let (file, line, col, sev, code, msg, at) = match classify(&text) {
            Line::Hit { file, line, col, sev, code, msg } => (file, line, col, sev, code, msg, n),
            Line::Head { sev, code, msg } => {
                self.head = Some((sev, code, msg, n));
                return None;
            }
            Line::Arrow { file, line, col } => {
                let (sev, code, msg, at) = head?;
                (file, line, col, sev, code, msg, at)
            }
            Line::Nothing => return None,
        };
        let path = resolve(&self.root, &self.cwd, &file)?;
        if !self.seen.insert((path.clone(), line, col, sev, msg.clone())) {
            return None;
        }
        Some(Problem { file: path, line, col, sev, message: msg, code, log: at })
    }
}

/// Every problem in `text`.
#[cfg(test)]
pub fn parse(root: &Path, cwd: &Path, text: &str) -> Vec<Problem> {
    let mut m = Matcher::new(root, cwd);
    text.lines().enumerate().filter_map(|(i, l)| m.feed(l, i)).collect()
}

/// The span the editor underlines for a problem at `col` of a line: the word there, one character
/// when it is not on a word, the line's text when the column is past its end.
pub fn span(text: &[char], col: usize) -> (usize, usize) {
    let len = text.len();
    if len == 0 {
        return (0, 0);
    }
    if col >= len {
        let first = text.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
        return (first.min(len - 1), len);
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if word(text[col]) { (col, col + text[col..].iter().take_while(|c| word(**c)).count()) } else { (col, col + 1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/home/u/app")
    }

    fn one(text: &str) -> Vec<(String, usize, usize, Sev, String, String)> {
        parse(&root(), &root(), text)
            .into_iter()
            .map(|p| {
                let f = p.file.strip_prefix(root()).unwrap_or(&p.file).to_string_lossy().into_owned();
                (f, p.line + 1, p.col + 1, p.sev, p.code, p.message)
            })
            .collect()
    }

    fn e(f: &str, l: usize, c: usize, s: Sev, code: &str, m: &str) -> (String, usize, usize, Sev, String, String) {
        (f.to_string(), l, c, s, code.to_string(), m.to_string())
    }

    #[test]
    fn rustc_multi_line_and_short_forms() {
        let out = "\
   Compiling app v0.1.0 (/home/u/app)
warning: unused variable: `x`
 --> src/main.rs:2:9
  |
2 |     let x = 1;
  |         ^ help: if this is intentional, prefix it with an underscore: `_x`
  |
  = note: `#[warn(unused_variables)]` on by default

error[E0308]: mismatched types
  --> src/main.rs:4:18
   |
4  |     let y: u32 = \"a\";
   |            ---   ^^^ expected `u32`, found `&str`
   |            |
   |            expected due to this
note: function defined here
  --> src/lib.rs:10:4

error: could not compile `app` (bin \"app\") due to 1 previous error; 1 warning emitted
src/util.rs:7:3: error[E0425]: cannot find value `z` in this scope
";
        assert_eq!(
            one(out),
            vec![
                e("src/main.rs", 2, 9, Sev::Warning, "", "unused variable: `x`"),
                e("src/main.rs", 4, 18, Sev::Error, "E0308", "mismatched types"),
                e("src/util.rs", 7, 3, Sev::Error, "E0425", "cannot find value `z` in this scope"),
            ]
        );
        // The log line of a multi-line diagnostic is its header.
        let ps = parse(&root(), &root(), out);
        assert_eq!(ps[1].log, 9);
    }

    #[test]
    fn colored_cargo_output_is_matched() {
        let out = "\u{1b}[0m\u{1b}[1m\u{1b}[38;5;9merror[E0425]\u{1b}[0m\u{1b}[0m\u{1b}[1m: cannot find value `q`\u{1b}[0m\n\u{1b}[0m \u{1b}[0m\u{1b}[0m\u{1b}[1m\u{1b}[38;5;12m--> \u{1b}[0m\u{1b}[0msrc/main.rs:3:5\u{1b}[0m\n";
        assert_eq!(one(out), vec![e("src/main.rs", 3, 5, Sev::Error, "E0425", "cannot find value `q`")]);
    }

    #[test]
    fn tsc_both_forms() {
        let out = "\
> app@1.0.0 build
> tsc -p .

src/a.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.
src/b.tsx:3:10 - error TS2304: Cannot find name 'foo'.

3   return foo;
           ~~~
Found 2 errors in 2 files.
";
        assert_eq!(
            one(out),
            vec![
                e("src/a.ts", 12, 5, Sev::Error, "TS2322", "Type 'string' is not assignable to type 'number'."),
                e("src/b.tsx", 3, 10, Sev::Error, "TS2304", "Cannot find name 'foo'."),
            ]
        );
    }

    #[test]
    fn maven_and_javac() {
        let out = "\
[INFO] Scanning for projects...
[INFO] --- compiler:3.13.0:compile (default-compile) @ demo ---
[WARNING] /home/u/app/src/main/java/demo/Util.java:[8,17] [deprecation] Date(int,int,int) in Date has been deprecated
[ERROR] COMPILATION ERROR :
[ERROR] /home/u/app/src/main/java/demo/App.java:[12,5] cannot find symbol
  symbol:   variable foo
  location: class demo.App
[INFO] 1 error
[ERROR] Failed to execute goal org.apache.maven.plugins:maven-compiler-plugin:3.13.0:compile (default-compile) on project demo: Compilation failure
[ERROR] /home/u/app/src/main/java/demo/App.java:[12,5] cannot find symbol
[ERROR] /opt/elsewhere/Gone.java:[1,1] not in this project
src/main/java/demo/Other.java:4: error: ';' expected
";
        assert_eq!(
            one(out),
            vec![
                e(
                    "src/main/java/demo/Util.java",
                    8,
                    17,
                    Sev::Warning,
                    "",
                    "[deprecation] Date(int,int,int) in Date has been deprecated"
                ),
                e("src/main/java/demo/App.java", 12, 5, Sev::Error, "", "cannot find symbol"),
                e("src/main/java/demo/Other.java", 4, 1, Sev::Error, "", "';' expected"),
            ],
            "the summary repeats are dropped and the missing file outside the project is ignored"
        );
    }

    #[test]
    fn gradle_kotlin_and_javac() {
        let out = "\
> Task :app:compileKotlin FAILED
e: file:///home/u/app/app/src/main/kotlin/Main.kt:12:5 Unresolved reference 'foo'.
w: file:///home/u/app/app/src/main/kotlin/My%20Util.kt:3:9 Variable 'x' is never used
e: /home/u/app/app/src/main/kotlin/Old.kt: (7, 13): Type mismatch: inferred type is String but Int was expected
> Task :lib:compileJava
/home/u/app/lib/src/main/java/lib/A.java:5: error: incompatible types: String cannot be converted to int
        int x = \"s\";
                ^
FAILURE: Build failed with an exception.
";
        assert_eq!(
            one(out),
            vec![
                e("app/src/main/kotlin/Main.kt", 12, 5, Sev::Error, "", "Unresolved reference 'foo'."),
                e("app/src/main/kotlin/My Util.kt", 3, 9, Sev::Warning, "", "Variable 'x' is never used"),
                e(
                    "app/src/main/kotlin/Old.kt",
                    7,
                    13,
                    Sev::Error,
                    "",
                    "Type mismatch: inferred type is String but Int was expected"
                ),
                e(
                    "lib/src/main/java/lib/A.java",
                    5,
                    1,
                    Sev::Error,
                    "",
                    "incompatible types: String cannot be converted to int"
                ),
            ]
        );
    }

    #[test]
    fn msbuild_lines_are_deduplicated() {
        let out = "\
  Determining projects to restore...
/home/u/app/Program.cs(12,5): error CS1002: ; expected [/home/u/app/App.csproj]
/home/u/app/Models/User.cs(3,19): warning CS8618: Non-nullable property 'Name' must contain a non-null value when exiting constructor. [/home/u/app/App.csproj]
/home/u/app/Program.cs(12,5): error CS1002: ; expected [/home/u/app/App.csproj]
    1 Warning(s)
    1 Error(s)
MSBUILD : error MSB1009: Project file does not exist.
";
        assert_eq!(
            one(out),
            vec![
                e("Program.cs", 12, 5, Sev::Error, "CS1002", "; expected"),
                e(
                    "Models/User.cs",
                    3,
                    19,
                    Sev::Warning,
                    "CS8618",
                    "Non-nullable property 'Name' must contain a non-null value when exiting constructor."
                ),
            ]
        );
    }

    #[test]
    fn gcc_clang_notes_and_go() {
        let out = "\
main.c: In function 'main':
main.c:12:5: error: 'y' undeclared (first use in this function)
main.c:12:5: note: each undeclared identifier is reported only once for each function it appears in
src/util.c:3:10: warning: unused variable 'z' [-Wunused-variable]
src/fatal.c:1:10: fatal error: missing.h: No such file or directory
make: *** [Makefile:3: all] Error 1
";
        assert_eq!(
            one(out),
            vec![
                e("main.c", 12, 5, Sev::Error, "", "'y' undeclared (first use in this function)"),
                e("src/util.c", 3, 10, Sev::Warning, "", "unused variable 'z' [-Wunused-variable]"),
                e("src/fatal.c", 1, 10, Sev::Error, "", "missing.h: No such file or directory"),
            ]
        );
        let go = "# example.com/m\n./main.go:12:5: undefined: foo\ncmd/x/x.go:3:2: \"os\" imported and not used\n";
        assert_eq!(
            one(go),
            vec![
                e("main.go", 12, 5, Sev::Error, "", "undefined: foo"),
                e("cmd/x/x.go", 3, 2, Sev::Error, "", "\"os\" imported and not used"),
            ]
        );
    }

    #[test]
    fn paths_resolve_against_the_tool_folder() {
        let r = root();
        let web = r.join("web");
        assert_eq!(resolve(&r, &web, "src/a.ts"), Some(r.join("web/src/a.ts")));
        assert_eq!(resolve(&r, &r, "./x/../y.rs"), Some(r.join("y.rs")));
        assert_eq!(resolve(&r, &r, "file:///home/u/app/a%20b.kt"), Some(r.join("a b.kt")));
        assert_eq!(resolve(&r, &r, "/nowhere/x.c"), None, "outside and missing");
        assert_eq!(resolve(&r, &r, "../../etc/zzz-missing"), None);
        assert_eq!(resolve(&r, &r, ""), None);
        // An existing file outside the project is kept.
        let tmp = std::env::temp_dir().join(format!("lumen-build-resolve-{}.c", std::process::id()));
        std::fs::write(&tmp, "x").unwrap();
        assert_eq!(resolve(&r, &r, &tmp.to_string_lossy()), Some(tmp.clone()));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn spans_counts_and_summary() {
        let l: Vec<char> = "    let total = x;".chars().collect();
        assert_eq!(span(&l, 8), (8, 13));
        assert_eq!(span(&l, 14), (14, 15), "the '=' alone");
        assert_eq!(span(&l, 99), (4, l.len()), "past the end: the line's text");
        assert_eq!(span(&[], 3), (0, 0));
        let p = |s| Problem {
            file: "a".into(),
            line: 0,
            col: 0,
            sev: s,
            message: String::new(),
            code: String::new(),
            log: 0,
        };
        let c = Counts::of(&[p(Sev::Error), p(Sev::Warning), p(Sev::Warning), p(Sev::Info)]);
        assert_eq!((c.errors, c.warnings, c.infos, c.total()), (1, 2, 1, 4));
        assert_eq!(summary(c), "1 error, 2 warnings");
        assert_eq!(summary(Counts::default()), "no problems");
        assert_eq!(summary(Counts { errors: 3, ..Default::default() }), "3 errors");
        assert_eq!(summary(Counts { warnings: 1, ..Default::default() }), "1 warning");
    }

    // ───────────── detection ─────────────

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-buildrun-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn labels(ts: &[Target]) -> Vec<String> {
        ts.iter().map(|t| format!("{}:{}", t.kind.label(), t.label)).collect()
    }

    #[test]
    fn detects_cargo_and_node_with_custom_first() {
        let d = scratch("cargo");
        put(&d, "Cargo.toml", "[package]\nname = \"x\"\n");
        put(&d, "src/main.rs", "fn main() {}\n");
        put(&d, "pnpm-lock.yaml", "");
        put(
            &d,
            "package.json",
            r#"{"scripts":{"dev":"vite","build":"vite build","lint":"eslint .","clean":"rm -rf dist"}}"#,
        );
        let ts = detect(&d, "make all", "./run.sh");
        assert_eq!(
            labels(&ts),
            vec![
                "build:make all",
                "run:./run.sh",
                "build:cargo build",
                "run:cargo run",
                "build:cargo check",
                "build:cargo clippy",
                "build:cargo build --release",
                "run:cargo run --release",
                "clean:cargo clean",
                "build:pnpm run build",
                "run:pnpm run dev",
                "clean:pnpm run clean",
            ]
        );
        assert_eq!(ts[0].id, CUSTOM_BUILD);
        assert_eq!(ts[2].cmd, "cargo build --color=always");
        assert_eq!(ts[2].group, "Cargo");
        assert!(ts[3].compiles && !ts[10].compiles, "cargo run compiles, a dev server does not");
        // Selection: Build / Run / Clean of the chosen tool, the variant kept.
        assert_eq!(default_target(&ts).map(|t| t.id.as_str()), Some(CUSTOM_BUILD));
        let release = ts.iter().find(|t| t.label == "cargo build --release").unwrap();
        assert_eq!(run_for(&ts, Some(release)).unwrap().label, "cargo run --release");
        assert_eq!(clean_for(&ts, Some(release)).unwrap().label, "cargo clean");
        assert_eq!(build_for(&ts, Some(release)).unwrap().label, "cargo build --release");
        let dev = ts.iter().find(|t| t.label == "pnpm run dev").unwrap();
        assert_eq!(build_for(&ts, Some(dev)).unwrap().label, "pnpm run build");
        assert_eq!(run_for(&ts, Some(dev)).unwrap().label, "pnpm run dev");
        assert_eq!(run_for(&ts, Some(&ts[0])).unwrap().id, CUSTOM_RUN);
        // Without custom commands the first cargo build is the default.
        let ts = detect(&d, "", " ");
        assert_eq!(default_target(&ts).unwrap().label, "cargo build");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn detects_maven_gradle_and_wrappers() {
        let d = scratch("jvm");
        put(
            &d,
            "pom.xml",
            "<project><build><plugins><plugin><artifactId>spring-boot-maven-plugin</artifactId></plugin></plugins></build></project>",
        );
        put(&d, "mvnw", "#!/bin/sh\n");
        put(&d, "settings.gradle.kts", "include(\"app\")\n");
        put(&d, "app/build.gradle.kts", "plugins {\n    application\n    kotlin(\"jvm\")\n}\n");
        let ts = detect(&d, "", "");
        let l = labels(&ts);
        assert!(l.contains(&"build:./mvnw compile".to_string()), "{l:?}");
        assert!(l.contains(&"run:./mvnw spring-boot:run".to_string()), "{l:?}");
        assert!(l.contains(&"clean:./mvnw clean".to_string()), "{l:?}");
        assert!(l.contains(&"build:gradle build".to_string()), "no wrapper: plain gradle {l:?}");
        assert!(l.contains(&"run:gradle run".to_string()), "application plugin in app/: {l:?}");
        assert!(!l.iter().any(|x| x.contains("bootRun")), "{l:?}");
        let gb = ts.iter().find(|t| t.label == "gradle build").unwrap();
        assert_eq!(gb.cmd, "gradle build --console=plain");
        assert_eq!(ts.iter().find(|t| t.label == "./mvnw compile").unwrap().cmd, "./mvnw -B -q compile");
        assert!(gradle_app("apply plugin: 'application'"));
        assert!(gradle_app("plugins { id 'application' }"));
        assert!(gradle_app("plugins {\n  id(\"application\")\n}"));
        assert!(!gradle_app("plugins { id 'java-library' }\n// application later"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn detects_dotnet_below_the_root() {
        let d = scratch("dotnet");
        put(&d, "src/Api/Api.csproj", r#"<Project Sdk="Microsoft.NET.Sdk.Web"></Project>"#);
        put(&d, "src/Lib/Lib.csproj", r#"<Project Sdk="Microsoft.NET.Sdk"></Project>"#);
        put(
            &d,
            "tests/Api.Tests/Api.Tests.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><OutputType>Exe</OutputType></PropertyGroup><ItemGroup><PackageReference Include=\"Microsoft.NET.Test.Sdk\" /></ItemGroup></Project>",
        );
        let ts = detect(&d, "", "");
        let l = labels(&ts);
        assert!(l.contains(&"build:dotnet build Api.csproj (Debug)".to_string()), "{l:?}");
        assert!(l.contains(&"run:dotnet run Api (Debug)".to_string()), "{l:?}");
        assert!(l.contains(&"run:dotnet run Api (Release)".to_string()), "{l:?}");
        assert!(!l.iter().any(|x| x.contains("run Lib") || x.contains("run Api.Tests")), "{l:?}");
        let api = ts.iter().find(|t| t.label == "dotnet build Api.csproj (Release)").unwrap();
        assert_eq!(api.cmd, "dotnet build \"src/Api/Api.csproj\" -c Release -nologo -clp:NoSummary");
        assert_eq!(run_for(&ts, Some(api)).unwrap().label, "dotnet run Api (Release)");
        // A solution at the root builds everything.
        put(&d, "App.sln", "");
        let ts = detect(&d, "", "");
        let builds: Vec<&str> = ts.iter().filter(|t| t.kind == Kind::Build).map(|t| t.label.as_str()).collect();
        assert_eq!(builds, vec!["dotnet build App.sln (Debug)", "dotnet build App.sln (Release)"]);
        assert!(ts.iter().any(|t| t.cmd == "dotnet clean \"App.sln\" -nologo"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn detects_go_cmake_and_make() {
        let d = scratch("native");
        put(&d, "go.mod", "module example.com/m\n");
        put(&d, "cmd/server/main.go", "package main\n\nfunc main() {}\n");
        put(&d, "lib.go", "package m\n");
        put(
            &d,
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.20)\nproject(demo CXX)\nadd_executable(${PROJECT_NAME} main.cpp)\n",
        );
        put(
            &d,
            "Makefile",
            "CC := gcc\nOUT = bin\n.PHONY: all clean\nall: app\napp: main.o\n\t$(CC) -o app main.o\nmain.o: main.c\n%.o: %.c\n\t$(CC) -c $<\nrun: app\n\t./app\nclean:\n\trm -f app *.o\ntest lint: all\n$(OUT)/x: y\n",
        );
        let ts = detect(&d, "", "");
        let l = labels(&ts);
        for want in [
            "build:go build ./...",
            "run:go run ./cmd/server",
            "clean:go clean",
            "build:cmake --build build",
            "task:cmake configure",
            "run:run demo",
            "build:make all",
            "task:make app",
            "run:make run",
            "clean:make clean",
            "task:make test",
            "task:make lint",
        ] {
            assert!(l.contains(&want.to_string()), "missing {want}: {l:?}");
        }
        assert!(!l.iter().any(|x| x.contains("main.o") || x.contains("PHONY") || x.contains("CC")), "{l:?}");
        let cm = ts.iter().find(|t| t.label == "cmake --build build").unwrap();
        assert_eq!(prepare(cm), "cmake -S . -B build && cmake --build build", "configures once");
        put(&d, "build/CMakeCache.txt", "");
        assert_eq!(prepare(cm), "cmake --build build");
        assert_eq!(cmake_exe("add_executable(tool src/a.c)"), Some("tool".into()));
        assert_eq!(cmake_exe("project(x)"), None);
        assert_eq!(make_targets("a b: c\nVAR ::= 1\nx:=2\n\tnot: me\n-include: x\n"), vec!["a", "b"]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn nested_projects_and_nothing() {
        let d = scratch("nested");
        assert!(detect(&d, "", "").is_empty());
        put(&d, "web/package.json", r#"{"scripts":{"build":"tsc","start":"node ."}}"#);
        put(&d, "node_modules/x/package.json", r#"{"scripts":{"build":"x"}}"#);
        let ts = detect(&d, "", "");
        assert_eq!(labels(&ts), vec!["build:npm run build", "run:npm run start"]);
        assert_eq!(ts[0].group, "npm · web");
        assert_eq!(ts[0].cwd, d.join("web"));
        assert_eq!(ts[0].id, "node:web:npm run build");
        std::fs::remove_dir_all(&d).ok();
    }
}
