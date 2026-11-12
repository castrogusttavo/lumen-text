//! Python interpreters: the project's virtualenv and the system pythons.
//!
//! Python language servers do not look for the project's virtualenv themselves (VS Code's Python
//! extension tells them); without it every installed dependency is "could not be resolved". Found,
//! cheapest first: `.venv`/`venv`/`env`/`.env` (anything holding a `pyvenv.cfg`) in the project and
//! its ancestors (uv, `python -m venv`, Poetry with `in-project`), `$UV_PROJECT_ENVIRONMENT`, other
//! direct sub-folders with a `pyvenv.cfg`, `$VIRTUAL_ENV`, then Poetry's cache folder (matched by
//! project name; no `poetry` process is started).
//!
//! The user can pin another interpreter per project ("Python: Select interpreter…"): the choice is
//! kept in memory here (read by `lspreg`) and stored in the state directory by the app.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub struct Interp {
    /// The interpreter executable.
    pub python: PathBuf,
    /// Its virtualenv folder (`None` for a system python).
    pub venv: Option<PathBuf>,
    /// Where it was found: ".venv", "VIRTUAL_ENV", "poetry", "uv", "system", "selected".
    pub source: String,
    /// "3.12.1" from `pyvenv.cfg` (venvs only).
    pub version: Option<String>,
}

impl Interp {
    /// "venv-name (Python 3.12)" / "python3.12".
    pub fn label(&self) -> String {
        let name = match &self.venv {
            Some(v) => v.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            None => self.python.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        match &self.version {
            Some(ver) => format!("{name} (Python {ver})"),
            None => name,
        }
    }
}

/// The interpreter inside a virtualenv folder. Windows venvs keep it in `Scripts\python.exe`,
/// POSIX ones in `bin/python`.
pub fn venv_python(venv: &Path) -> Option<PathBuf> {
    let (scripts, bin) = (venv.join("Scripts"), venv.join("bin"));
    let mut c = vec![bin.join("python"), bin.join("python3"), scripts.join("python.exe")];
    if cfg!(windows) {
        c.rotate_right(1);
    }
    c.into_iter().find(|p| p.is_file())
}

/// Folder of a venv's executables (`bin`, or `Scripts` on Windows).
pub fn venv_bin(venv: &Path) -> PathBuf {
    if cfg!(windows) && venv.join("Scripts").is_dir() { venv.join("Scripts") } else { venv.join("bin") }
}

/// The virtualenv an interpreter belongs to (its `pyvenv.cfg` is one or two levels up).
pub fn venv_of(python: &Path) -> Option<PathBuf> {
    python.ancestors().skip(1).take(2).find(|d| d.join("pyvenv.cfg").is_file()).map(Path::to_path_buf)
}

/// `version` (or `version_info`) from a venv's `pyvenv.cfg`.
pub fn venv_version(venv: &Path) -> Option<String> {
    let text = std::fs::read_to_string(venv.join("pyvenv.cfg")).ok()?;
    let mut info = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "version" => return Some(v.trim().to_string()),
            "version_info" => info = Some(v.trim().to_string()),
            _ => {}
        }
    }
    info
}

fn interp_in(venv: &Path, source: &str) -> Option<Interp> {
    if !venv.join("pyvenv.cfg").is_file() {
        return None;
    }
    Some(Interp {
        python: venv_python(venv)?,
        venv: Some(venv.to_path_buf()),
        source: source.to_string(),
        version: venv_version(venv),
    })
}

/// The process environment discovery looks at (passed in so tests stay hermetic).
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub virtual_env: Option<PathBuf>,
    pub uv_project_environment: Option<PathBuf>,
    pub poetry_virtualenvs: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Env {
    pub fn from_process() -> Env {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = crate::paths::home();
        let poetry = var("POETRY_VIRTUALENVS_PATH").or_else(|| {
            if cfg!(target_os = "macos") {
                home.as_ref().map(|h| h.join("Library/Caches/pypoetry/virtualenvs"))
            } else if cfg!(windows) {
                var("LOCALAPPDATA").map(|d| d.join("pypoetry").join("Cache").join("virtualenvs"))
            } else {
                crate::paths::cache_home().map(|c| c.join("pypoetry/virtualenvs"))
            }
        });
        Env {
            virtual_env: var("VIRTUAL_ENV"),
            uv_project_environment: var("UV_PROJECT_ENVIRONMENT"),
            poetry_virtualenvs: poetry,
            home,
        }
    }
}

/// Names a project's own virtualenv usually has.
const VENV_NAMES: &[&str] = &[".venv", "venv", "env", ".env", "virtualenv"];
/// Sub-folders never worth looking into for a venv.
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "dist", "build", "__pycache__", ".mypy_cache", ".tox"];

/// The project name in `pyproject.toml` (`[tool.poetry]` or `[project]` `name = "…"`).
fn pyproject_name(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if (section == "[tool.poetry]" || section == "[project]")
            && let Some(rest) = line.strip_prefix("name")
            && let Some(v) = rest.trim_start().strip_prefix('=')
        {
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Poetry's cache venvs of the project: `<name>-<hash>-py<X.Y>` (the hash is of the project path;
/// all candidates of that name are returned, newest first).
fn poetry_venvs(root: &Path, env: &Env) -> Vec<Interp> {
    let uses_poetry = root.join("poetry.lock").is_file()
        || std::fs::read_to_string(root.join("pyproject.toml")).is_ok_and(|t| t.contains("[tool.poetry"));
    let (Some(dir), true) = (env.poetry_virtualenvs.as_ref(), uses_poetry) else { return Vec::new() };
    let Some(name) = pyproject_name(root) else { return Vec::new() };
    // Poetry's sanitizing: blanks and shell-special characters become `_`, cut at 42 characters.
    let clean: String = name
        .to_lowercase()
        .replace(['_', '.'], "-")
        .chars()
        .map(|c| if " $`!*@\"\\\r\n\t".contains(c) { '_' } else { c })
        .take(42)
        .collect();
    let prefix = format!("{clean}-");
    let mut found: Vec<(std::time::SystemTime, Interp)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_lowercase().starts_with(&prefix))
        .filter_map(|e| {
            let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            interp_in(&e.path(), "poetry").map(|i| (t, i))
        })
        .collect();
    found.sort_by_key(|x| std::cmp::Reverse(x.0));
    found.into_iter().map(|x| x.1).collect()
}

/// Every virtualenv of the project at `root`, best first (see the module docs for the order).
pub fn discover(root: &Path, env: &Env) -> Vec<Interp> {
    fn push(i: Interp, out: &mut Vec<Interp>) {
        if !out.iter().any(|o| o.python == i.python) {
            out.push(i);
        }
    }
    let mut out: Vec<Interp> = Vec::new();
    // The project folder and its ancestors (a monorepo's subproject uses the repo's venv), never
    // above the home folder.
    for dir in root.ancestors() {
        if env.home.as_deref() == Some(dir) && dir != root {
            break;
        }
        for n in VENV_NAMES {
            let src = if root.join("uv.lock").is_file() && *n == ".venv" { "uv" } else { *n };
            if let Some(i) = interp_in(&dir.join(n), src) {
                push(i, &mut out);
            }
        }
        if dir == root {
            if let Some(p) = &env.uv_project_environment {
                let p = if p.is_relative() { root.join(p) } else { p.clone() };
                if let Some(i) = interp_in(&p, "uv") {
                    push(i, &mut out);
                }
            }
            // Any other direct sub-folder holding a `pyvenv.cfg` (`.venv311`, `py-env`, ...).
            let mut extra: Vec<PathBuf> = std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !SKIP_DIRS.contains(&n) && !VENV_NAMES.contains(&n))
                })
                .collect();
            extra.sort();
            for p in extra {
                if let Some(i) = interp_in(&p, "pyvenv.cfg") {
                    push(i, &mut out);
                }
            }
        }
    }
    if let Some(v) = &env.virtual_env
        && let Some(i) = interp_in(v, "VIRTUAL_ENV")
    {
        push(i, &mut out);
    }
    for i in poetry_venvs(root, env) {
        push(i, &mut out);
    }
    out
}

/// Pythons on the `PATH` (`python3`, `python`, `python3.X`), without duplicates (symlinks to the
/// same file count once). For the interpreter picker only: this reads every `PATH` folder.
pub fn system_pythons() -> Vec<Interp> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    system_pythons_in(&dirs)
}

pub fn system_pythons_in(dirs: &[PathBuf]) -> Vec<Interp> {
    let mut out: Vec<Interp> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let exe = if cfg!(windows) { ".exe" } else { "" };
    for d in dirs {
        let mut names: Vec<String> = vec![format!("python3{exe}"), format!("python{exe}")];
        let mut versioned: Vec<String> = std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| {
                let stem = n.strip_suffix(exe).unwrap_or(n);
                stem.strip_prefix("python3.").is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
            })
            .collect();
        versioned.sort();
        names.extend(versioned);
        for n in names {
            let p = d.join(&n);
            if !p.is_file() {
                continue;
            }
            let real = crate::paths::canonical(&p).unwrap_or_else(|_| p.clone());
            if seen.contains(&real) {
                continue;
            }
            seen.push(real);
            out.push(Interp { python: p, venv: None, source: "system".into(), version: None });
        }
    }
    out
}

// ───────────────────────────── the user's choice ─────────────────────────────

static CHOSEN: Mutex<Option<HashMap<PathBuf, PathBuf>>> = Mutex::new(None);

/// Pin `python` for the project at `root` (`None` = back to automatic). In memory only.
pub fn set_choice(root: &Path, python: Option<PathBuf>) {
    let mut g = CHOSEN.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    match python {
        Some(p) => m.insert(root.to_path_buf(), p),
        None => m.remove(root),
    };
}

/// The interpreter pinned for `dir` or the nearest project above it, if it still exists.
pub fn choice_for(dir: &Path) -> Option<PathBuf> {
    let g = CHOSEN.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.as_ref()?;
    dir.ancestors().find_map(|a| m.get(a)).filter(|p| p.is_file()).cloned()
}

/// The interpreter the language servers of `root` get: the pinned one, else the best venv found.
pub fn active(root: &Path) -> Option<Interp> {
    active_with(root, &Env::from_process())
}

pub fn active_with(root: &Path, env: &Env) -> Option<Interp> {
    if let Some(p) = choice_for(root) {
        let venv = venv_of(&p);
        let version = venv.as_deref().and_then(venv_version);
        return Some(Interp { python: p, venv, source: "selected".into(), version });
    }
    discover(root, env).into_iter().next()
}

/// The stored choices (`python-interpreters.json` in the state directory): `{root: python}`.
pub fn load_choice(file: &Path, root: &Path) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    v.get(&*root.to_string_lossy())?.as_str().map(PathBuf::from)
}

pub fn store_choice(file: &Path, root: &Path, python: Option<&Path>) -> std::io::Result<()> {
    let mut v: serde_json::Value = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let key = root.to_string_lossy().into_owned();
    match python {
        Some(p) => v[&key] = serde_json::json!(p.to_string_lossy()),
        None => {
            if let Some(o) = v.as_object_mut() {
                o.remove(&key);
            }
        }
    }
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(file, serde_json::to_string_pretty(&v).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-pyenv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        crate::paths::canonical(&d).unwrap()
    }

    /// A fake venv as `python -m venv` lays it out on this OS.
    fn mkvenv(dir: &Path, version: &str) -> PathBuf {
        let py = if cfg!(windows) { dir.join("Scripts").join("python.exe") } else { dir.join("bin").join("python") };
        std::fs::create_dir_all(py.parent().unwrap()).unwrap();
        std::fs::write(dir.join("pyvenv.cfg"), format!("home = /usr/bin\nversion = {version}\n")).unwrap();
        std::fs::write(&py, "").unwrap();
        py
    }

    fn quiet(home: &Path) -> Env {
        Env { home: Some(home.to_path_buf()), ..Env::default() }
    }

    #[test]
    fn finds_the_project_venvs_best_first() {
        let d = tmp("discover");
        let root = d.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        assert!(discover(&root, &quiet(&d)).is_empty(), "no venv");
        let dot = mkvenv(&root.join(".venv"), "3.12.1");
        let plain = mkvenv(&root.join("venv"), "3.11.0");
        let odd = mkvenv(&root.join("py311"), "3.11.9");
        std::fs::create_dir_all(root.join("src")).unwrap(); // not a venv
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        // `.env` that is a dotenv file, not a venv: ignored.
        std::fs::write(root.join(".env.example"), "A=1").unwrap();
        let found = discover(&root, &quiet(&d));
        let pys: Vec<&Path> = found.iter().map(|i| i.python.as_path()).collect();
        assert_eq!(pys, [dot.as_path(), plain.as_path(), odd.as_path()]);
        assert_eq!(found[0].version.as_deref(), Some("3.12.1"));
        assert_eq!(found[0].label(), ".venv (Python 3.12.1)");
        assert_eq!(found[0].source, ".venv");
        assert_eq!(found[2].source, "pyvenv.cfg");
        assert_eq!(active_with(&root, &quiet(&d)).unwrap().python, dot);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_subproject_uses_the_venv_of_its_repository() {
        let d = tmp("mono");
        let repo = d.join("repo");
        let sub = repo.join("services/api");
        std::fs::create_dir_all(&sub).unwrap();
        let py = mkvenv(&repo.join(".venv"), "3.12");
        assert_eq!(discover(&sub, &quiet(&d))[0].python, py);
        // ...but never a venv in the home folder for a project below it.
        let home_venv = d.join(".venv");
        mkvenv(&home_venv, "3.10");
        let other = d.join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        assert!(discover(&other, &quiet(&d)).is_empty(), "the home folder's venv is not the project's");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn environment_venvs_uv_and_poetry() {
        let d = tmp("env");
        let root = d.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let active = mkvenv(&d.join("activated"), "3.9.1");
        let uvenv = mkvenv(&root.join("envs/uv"), "3.13.0");
        std::fs::write(root.join("pyproject.toml"), "[tool.poetry]\nname = \"My_App\"\nversion = \"1\"\n").unwrap();
        let cache = d.join("poetry");
        let poetry = mkvenv(&cache.join("my-app-AbCd1234-py3.12"), "3.12.0");
        mkvenv(&cache.join("other-AbCd1234-py3.12"), "3.12.0");
        let env = Env {
            virtual_env: Some(d.join("activated")),
            uv_project_environment: Some(PathBuf::from("envs/uv")),
            poetry_virtualenvs: Some(cache.clone()),
            home: Some(d.clone()),
        };
        let found = discover(&root, &env);
        let pys: Vec<&Path> = found.iter().map(|i| i.python.as_path()).collect();
        assert_eq!(pys, [uvenv.as_path(), active.as_path(), poetry.as_path()], "{found:?}");
        assert_eq!(found.iter().map(|i| i.source.as_str()).collect::<Vec<_>>(), ["uv", "VIRTUAL_ENV", "poetry"]);
        // A project's own `.venv` comes first; with a `uv.lock` it is labelled as uv's.
        std::fs::write(root.join("uv.lock"), "").unwrap();
        mkvenv(&root.join(".venv"), "3.13.0");
        let first = &discover(&root, &env)[0];
        assert_eq!((first.source.as_str(), first.venv.as_deref()), ("uv", Some(root.join(".venv").as_path())));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_pinned_interpreter_wins_and_is_stored() {
        let d = tmp("pin");
        let root = d.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        mkvenv(&root.join(".venv"), "3.12");
        let other = mkvenv(&d.join("other-env"), "3.10.4");
        set_choice(&root, Some(other.clone()));
        let a = active_with(&root.join("pkg"), &quiet(&d)).unwrap();
        assert_eq!((a.python.as_path(), a.source.as_str()), (other.as_path(), "selected"), "applies below the root");
        assert_eq!(a.venv.as_deref(), Some(d.join("other-env").as_path()));
        assert_eq!(a.version.as_deref(), Some("3.10.4"));
        set_choice(&root, None);
        assert_eq!(active_with(&root, &quiet(&d)).unwrap().source, ".venv", "back to automatic");
        // A pinned interpreter that was deleted no longer counts.
        set_choice(&root, Some(d.join("gone/bin/python")));
        assert_eq!(active_with(&root, &quiet(&d)).unwrap().source, ".venv");
        set_choice(&root, None);

        let file = d.join("state/python-interpreters.json");
        assert_eq!(load_choice(&file, &root), None);
        store_choice(&file, &root, Some(&other)).unwrap();
        store_choice(&file, Path::new("/x"), Some(Path::new("/usr/bin/python3"))).unwrap();
        assert_eq!(load_choice(&file, &root), Some(other.clone()));
        store_choice(&file, &root, None).unwrap();
        assert_eq!(load_choice(&file, &root), None);
        assert_eq!(load_choice(&file, Path::new("/x")), Some(PathBuf::from("/usr/bin/python3")));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn system_pythons_are_listed_once() {
        let d = tmp("sys");
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let exe = if cfg!(windows) { ".exe" } else { "" };
        for n in ["python3", "python3.12", "python3-config", "python3.12-config"] {
            std::fs::write(a.join(format!("{n}{exe}")), "").unwrap();
        }
        std::fs::write(b.join(format!("python{exe}")), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(a.join("python3"), b.join("python3")).unwrap();
        let found = system_pythons_in(&[a.clone(), b.clone()]);
        let names: Vec<String> = found.iter().map(|i| i.label()).collect();
        let want: Vec<String> = ["python3", "python3.12", "python"].iter().map(|n| format!("{n}{exe}")).collect();
        assert_eq!(names, want, "the symlink to a listed python is not repeated; -config scripts are not pythons");
        assert!(found.iter().all(|i| i.venv.is_none() && i.source == "system"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn versions_and_venv_of_an_interpreter() {
        let d = tmp("ver");
        let py = mkvenv(&d.join("e"), "3.8.10");
        assert_eq!(venv_of(&py).as_deref(), Some(d.join("e").as_path()));
        assert_eq!(venv_of(Path::new("/usr/bin/python3")), None);
        std::fs::write(d.join("e/pyvenv.cfg"), "home = /x\nversion_info = 3.11.2.final.0\n").unwrap();
        assert_eq!(venv_version(&d.join("e")).as_deref(), Some("3.11.2.final.0"));
        assert_eq!(venv_bin(&d.join("e")), if cfg!(windows) { d.join("e/Scripts") } else { d.join("e/bin") });
        std::fs::remove_dir_all(&d).ok();
    }
}
