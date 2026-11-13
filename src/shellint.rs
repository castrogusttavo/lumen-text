//! Shell integration for the terminal: makes bash, zsh and fish tell Lumen where each prompt, each
//! command's output and each exit code are (OSC 133) and which folder they are in (OSC 7).
//!
//! Nothing of the user's is modified: the snippets are written under the user's cache folder and
//! picked up through the shell's own start-up switches (`bash --rcfile`, `ZDOTDIR` for zsh,
//! `fish --init-command`); each snippet first runs the user's usual start-up files. Set
//! `LUMEN_NO_SHELL_INTEGRATION=1` to start shells plainly.

#![cfg_attr(not(unix), allow(dead_code))]

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bash,
    Zsh,
    Fish,
}

/// How to start a shell: extra arguments and environment variables.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Launch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The shell family of a `$SHELL` value, when it has a snippet.
pub fn kind(shell: &str) -> Option<Kind> {
    match Path::new(shell).file_name()?.to_str()? {
        "bash" => Some(Kind::Bash),
        "zsh" => Some(Kind::Zsh),
        "fish" => Some(Kind::Fish),
        _ => None,
    }
}

const BASH_RC: &str = r#"# Lumen shell integration (generated at start-up; your own files are not touched).
[ -r /etc/bash.bashrc ] && . /etc/bash.bashrc
[ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
__lumen_pc() {
  local s=$?
  printf '\033]133;D;%s\a\033]7;file://%s%s\a\033]133;A\a' "$s" "${HOSTNAME:-}" "$PWD"
  return $s
}
if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
  PROMPT_COMMAND=(__lumen_pc "${PROMPT_COMMAND[@]}")
else
  PROMPT_COMMAND="__lumen_pc${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
PS0="${PS0-}"$'\033]133;C\a'
"#;

const ZSH_ENV: &str = r#"# Lumen shell integration (generated at start-up; your own files are not touched).
typeset -g __lumen_zd=$ZDOTDIR
if [[ -n $LUMEN_ORIG_ZDOTDIR ]]; then ZDOTDIR=$LUMEN_ORIG_ZDOTDIR; else unset ZDOTDIR; fi
[[ -r ${ZDOTDIR:-$HOME}/.zshenv ]] && builtin source "${ZDOTDIR:-$HOME}/.zshenv"
LUMEN_ORIG_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__lumen_zd
"#;

const ZSH_RC: &str = r#"# Lumen shell integration (generated at start-up; your own files are not touched).
if [[ -n $LUMEN_ORIG_ZDOTDIR ]]; then ZDOTDIR=$LUMEN_ORIG_ZDOTDIR; else unset ZDOTDIR; fi
unset LUMEN_ORIG_ZDOTDIR
[[ -r ${ZDOTDIR:-$HOME}/.zshrc ]] && builtin source "${ZDOTDIR:-$HOME}/.zshrc"
__lumen_precmd() {
  local s=$?
  printf '\033]133;D;%s\a\033]7;file://%s%s\a\033]133;A\a' "$s" "$HOST" "$PWD"
  return $s
}
__lumen_preexec() { printf '\033]133;C\a' }
precmd_functions=(__lumen_precmd $precmd_functions)
preexec_functions=($preexec_functions __lumen_preexec)
"#;

const FISH_INIT: &str = r#"function __lumen_preexec --on-event fish_preexec; printf '\e]133;C\a'; end; function __lumen_postexec --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end; function __lumen_prompt --on-event fish_prompt; printf '\e]7;file://%s%s\a\e]133;A\a' $hostname $PWD; end"#;

/// Where the snippets live: the user's cache folder (private to them, unlike `/tmp`).
fn dir() -> Option<PathBuf> {
    crate::paths::cache_home().map(|c| c.join("lumen-text").join("shell-integration"))
}

fn write_if_changed(p: &Path, text: &str) -> bool {
    if std::fs::read_to_string(p).is_ok_and(|t| t == text) {
        return true;
    }
    p.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok()) && std::fs::write(p, text).is_ok()
}

/// How to start `shell` with the integration, writing its snippet if needed; `None` for shells
/// without one, when the user opted out, or when the snippet could not be written.
pub fn launch(shell: &str) -> Option<Launch> {
    if std::env::var_os("LUMEN_NO_SHELL_INTEGRATION").is_some_and(|v| !v.is_empty() && v != "0") {
        return None;
    }
    launch_in(shell, &dir()?, std::env::var("ZDOTDIR").ok())
}

fn launch_in(shell: &str, dir: &Path, zdotdir: Option<String>) -> Option<Launch> {
    match kind(shell)? {
        Kind::Bash => {
            let rc = dir.join("bash.rc");
            write_if_changed(&rc, BASH_RC)
                .then(|| Launch { args: vec!["--rcfile".into(), rc.to_string_lossy().into_owned()], env: Vec::new() })
        }
        Kind::Zsh => {
            let z = dir.join("zsh");
            let ok = write_if_changed(&z.join(".zshenv"), ZSH_ENV) && write_if_changed(&z.join(".zshrc"), ZSH_RC);
            ok.then(|| Launch {
                args: Vec::new(),
                env: vec![
                    ("ZDOTDIR".into(), z.to_string_lossy().into_owned()),
                    ("LUMEN_ORIG_ZDOTDIR".into(), zdotdir.unwrap_or_default()),
                ],
            })
        }
        Kind::Fish => Some(Launch { args: vec!["--init-command".into(), FISH_INIT.into()], env: Vec::new() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_are_recognised_by_name() {
        assert_eq!(kind("/bin/bash"), Some(Kind::Bash));
        assert_eq!(kind("/usr/bin/zsh"), Some(Kind::Zsh));
        assert_eq!(kind("/opt/homebrew/bin/fish"), Some(Kind::Fish));
        assert_eq!(kind("/bin/sh"), None);
        assert_eq!(kind("pwsh"), None);
    }

    #[test]
    fn snippets_are_written_outside_the_users_files() {
        let d = std::env::temp_dir().join(format!("lumen-shellint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let b = launch_in("/bin/bash", &d, None).unwrap();
        assert_eq!(b.args[0], "--rcfile");
        let rc = std::fs::read_to_string(&b.args[1]).unwrap();
        assert!(rc.contains("133;A") && rc.contains("133;C") && rc.contains("133;D"));
        assert!(rc.contains(".bashrc"), "the user's own rc still runs");
        let z = launch_in("/usr/bin/zsh", &d, Some("/home/u/.zsh".into())).unwrap();
        assert!(z.args.is_empty());
        assert!(z.env.contains(&("LUMEN_ORIG_ZDOTDIR".to_string(), "/home/u/.zsh".to_string())));
        let zdir = &z.env.iter().find(|(k, _)| k == "ZDOTDIR").unwrap().1;
        assert!(std::fs::read_to_string(format!("{zdir}/.zshrc")).unwrap().contains("preexec_functions"));
        let f = launch_in("/usr/bin/fish", &d, None).unwrap();
        assert_eq!(f.args[0], "--init-command");
        assert!(f.args[1].contains("fish_postexec"));
        assert!(launch_in("/bin/dash", &d, None).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The generated bash rc really reports prompts and exit codes (skipped without bash).
    #[test]
    #[cfg(unix)]
    fn bash_integration_emits_the_marks() {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        if Command::new("bash").arg("--version").output().is_err() {
            return;
        }
        let d = std::env::temp_dir().join(format!("lumen-shellint-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let l = launch_in("bash", &d, None).unwrap();
        let home = d.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut child = Command::new("bash")
            .args(&l.args)
            .arg("-i")
            .env("HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"false\nexit\n").unwrap();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(text.contains("\x1b]133;A\x07"), "prompt mark in {text:?}");
        assert!(text.contains("\x1b]133;D;1\x07"), "exit code 1 reported in {text:?}");
        assert!(text.contains("\x1b]7;file://"), "cwd reported");
        let _ = std::fs::remove_dir_all(&d);
    }
}
