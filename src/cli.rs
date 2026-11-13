//! Command-line parsing (kept apart from `main` so it can be tested).
//!
//! Window flags (see `instance` and `app_window`):
//! * `--new-window`: open a new window even when one already shows the project (no handoff); it
//!   starts without the project's saved tabs.
//! * `--slot <n>`: internal, used when reopening the windows of the last run: restore the session
//!   of the project's n-th window (implies `--new-window`).
//! * `--take <file>`: internal, a tab moved out of another window (path, caret, scroll and any
//!   unsaved text) to open in this new one (implies `--new-window`).

use std::path::PathBuf;

/// Flags that make the process print or install something and leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Early {
    Version,
    InstallUser,
    CheckUpdate,
    Update,
}

/// How this process relates to the other windows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WinArgs {
    pub new_window: bool,
    pub slot: Option<u32>,
    pub take: Option<PathBuf>,
}

impl WinArgs {
    /// This invocation must become a window of its own (never hand its arguments over).
    pub fn forces_new(&self) -> bool {
        self.new_window || self.slot.is_some() || self.take.is_some()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    /// Files, `file:line:col` targets and folders.
    pub args: Vec<String>,
    pub keys: Option<String>,
    pub snap: PathBuf,
    pub size: (u32, u32),
    pub scale: Option<f32>,
    pub early: Option<Early>,
    /// `--profile-startup`: print the startup timeline after the first frame.
    pub profile_startup: bool,
    pub win: WinArgs,
}

pub fn parse(argv: impl IntoIterator<Item = String>) -> Cli {
    let mut cli = Cli {
        args: Vec::new(),
        keys: None,
        snap: if cfg!(unix) { PathBuf::from("/tmp/lumen-snap") } else { std::env::temp_dir().join("lumen-snap") },
        size: (1000, 700),
        scale: None,
        early: None,
        profile_startup: false,
        win: WinArgs::default(),
    };
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" | "-V" => return Cli { early: Some(Early::Version), ..cli },
            "--install-user" => return Cli { early: Some(Early::InstallUser), ..cli },
            "--check-update" => return Cli { early: Some(Early::CheckUpdate), ..cli },
            "--update" => return Cli { early: Some(Early::Update), ..cli },
            "--profile-startup" => cli.profile_startup = true,
            "--keys" => cli.keys = it.next(),
            "--scale" => cli.scale = it.next().as_deref().and_then(crate::ui::parse_scale),
            "--snap" => cli.snap = PathBuf::from(it.next().unwrap_or_default()),
            "--size" => {
                if let Some((w, h)) = it.next().as_deref().and_then(|s| s.split_once('x')) {
                    cli.size = (w.parse().unwrap_or(1000), h.parse().unwrap_or(700));
                }
            }
            "--new-window" | "-n" => cli.win.new_window = true,
            "--slot" => cli.win.slot = it.next().and_then(|s| s.parse().ok()),
            "--take" => cli.win.take = it.next().map(PathBuf::from),
            _ if a.starts_with("--") => {}
            _ => cli.args.push(a),
        }
    }
    cli
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Cli {
        parse(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn plain_targets_and_defaults() {
        let c = p(&["proj/", "src/a.ts:3:4"]);
        assert_eq!(c.args, ["proj/", "src/a.ts:3:4"]);
        assert_eq!(c.win, WinArgs::default());
        assert!(!c.win.forces_new());
        assert_eq!(c.size, (1000, 700));
        assert!(c.early.is_none() && c.keys.is_none());
    }

    #[test]
    fn window_flags() {
        let c = p(&["--new-window", "/p", "/p/a.rs:1:2"]);
        assert!(c.win.new_window && c.win.forces_new());
        assert_eq!(c.args, ["/p", "/p/a.rs:1:2"]);
        assert!(p(&["-n"]).win.new_window);
        let c = p(&["--slot", "2", "/p"]);
        assert_eq!(c.win.slot, Some(2));
        assert!(c.win.forces_new() && !c.win.new_window);
        assert_eq!(c.args, ["/p"]);
        let c = p(&["--new-window", "--take", "/s/h.json", "/p"]);
        assert_eq!(c.win.take, Some(PathBuf::from("/s/h.json")));
        assert_eq!(c.args, ["/p"], "the handoff file is not a target");
        // A bad slot number is ignored, and its value is still consumed.
        let c = p(&["--slot", "x", "/p"]);
        assert_eq!((c.win.slot, c.args.clone()), (None, vec!["/p".to_string()]));
    }

    #[test]
    fn other_flags_keep_working() {
        let c = p(&["--keys", "ctrl+p", "--size", "800x600", "--snap", "/s", "--unknown", "f.txt"]);
        assert_eq!(c.keys.as_deref(), Some("ctrl+p"));
        assert_eq!(c.size, (800, 600));
        assert_eq!(c.snap, PathBuf::from("/s"));
        assert_eq!(c.args, ["f.txt"]);
        assert_eq!(p(&["a", "--version", "b"]).early, Some(Early::Version));
        assert_eq!(p(&["--update"]).early, Some(Early::Update));
        assert!(p(&["--profile-startup", "x"]).profile_startup);
        assert!(!p(&["x"]).profile_startup);
    }
}
