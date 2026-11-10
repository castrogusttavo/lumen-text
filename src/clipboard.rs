//! System clipboard and primary selection through external tools, with an in-process fallback:
//! `wl-copy`/`wl-paste` on Wayland, `xclip` or `xsel` on X11. Shelling out keeps us free of
//! windowing-protocol dependencies and works on compositors without data-control.

use std::{
    io::Write,
    process::{Command, Stdio},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    Wayland,
    Xclip,
    Xsel,
    None,
}

fn has(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

fn detect() -> Backend {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && has("wl-copy") && has("wl-paste") {
        Backend::Wayland
    } else if has("xclip") {
        Backend::Xclip
    } else if has("xsel") {
        Backend::Xsel
    } else if has("wl-copy") && has("wl-paste") {
        Backend::Wayland
    } else {
        Backend::None
    }
}

pub struct Clipboard {
    backend: Backend,
    internal: String,
    internal_primary: String,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self { backend: detect(), internal: String::new(), internal_primary: String::new() }
    }
}

/// (program, args) that read or write one of the two selections.
fn command(b: Backend, primary: bool, write: bool) -> Option<(&'static str, Vec<&'static str>)> {
    Some(match (b, write) {
        (Backend::Wayland, true) => ("wl-copy", if primary { vec!["--primary"] } else { vec![] }),
        (Backend::Wayland, false) => ("wl-paste", if primary { vec!["--primary", "--no-newline"] } else { vec!["--no-newline"] }),
        (Backend::Xclip, true) => ("xclip", vec!["-selection", if primary { "primary" } else { "clipboard" }, "-i"]),
        (Backend::Xclip, false) => ("xclip", vec!["-selection", if primary { "primary" } else { "clipboard" }, "-o"]),
        (Backend::Xsel, true) => ("xsel", vec![if primary { "-ip" } else { "-ib" }]),
        (Backend::Xsel, false) => ("xsel", vec![if primary { "-op" } else { "-ob" }]),
        (Backend::None, _) => return None,
    })
}

impl Clipboard {
    fn write(&self, primary: bool, text: &str) {
        let Some((prog, args)) = command(self.backend, primary, true) else { return };
        let Ok(mut child) = Command::new(prog).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // Tools that serve the selection fork into the background; reap off-thread.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }

    fn read(&self, primary: bool) -> Option<String> {
        let (prog, args) = command(self.backend, primary, false)?;
        let out = Command::new(prog).args(args).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten()
    }

    pub fn set(&mut self, text: &str) {
        self.internal = text.to_string();
        self.write(false, text);
    }

    pub fn get(&mut self) -> String {
        self.read(false).unwrap_or_else(|| self.internal.clone())
    }

    /// Selecting text copies it to the primary selection (middle-click pastes it).
    pub fn set_primary(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.internal_primary = text.to_string();
        self.write(true, text);
    }

    pub fn get_primary(&mut self) -> String {
        self.read(true).unwrap_or_else(|| self.internal_primary.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_table_covers_each_backend() {
        assert_eq!(command(Backend::Wayland, true, true), Some(("wl-copy", vec!["--primary"])));
        assert_eq!(command(Backend::Wayland, false, false), Some(("wl-paste", vec!["--no-newline"])));
        assert_eq!(command(Backend::Xclip, true, false), Some(("xclip", vec!["-selection", "primary", "-o"])));
        assert_eq!(command(Backend::Xsel, false, true), Some(("xsel", vec!["-ib"])));
        assert_eq!(command(Backend::None, false, true), None);
    }

    #[test]
    fn falls_back_to_the_in_process_copy_without_tools() {
        let mut c = Clipboard { backend: Backend::None, internal: String::new(), internal_primary: String::new() };
        c.set("abc");
        assert_eq!(c.get(), "abc");
        c.set_primary("sel");
        assert_eq!(c.get_primary(), "sel");
        c.set_primary("");
        assert_eq!(c.get_primary(), "sel", "empty selections do not clobber the primary");
    }
}
