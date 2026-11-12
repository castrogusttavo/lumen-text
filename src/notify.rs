//! Desktop notifications without a new dependency: the tools every desktop already has.
//! Linux / BSD: `notify-send`, else the freedesktop `org.freedesktop.Notifications` D-Bus service
//! through `gdbus` or `busctl`; macOS: `osascript` (`display notification`). Windows: nothing yet
//! (the status bar badge still shows). Nothing is installed or kept running: one short-lived
//! process per notification, reaped off the UI thread.

use std::process::{Command, Stdio};

/// Which family of tools the system has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Freedesktop,
    Mac,
    Windows,
}

pub fn current_os() -> Os {
    if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Freedesktop
    }
}

/// Control characters (but new lines) would garble the tools' arguments.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() && c != '\n' { ' ' } else { c }).collect()
}

/// A GVariant text-format string literal (what `gdbus call` parses each argument as).
pub fn gvariant_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// An AppleScript string literal.
pub fn applescript_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Program and arguments that show `title` / `body`, with `has` telling which programs are on
/// PATH; `None` when nothing on this system can.
pub fn command(os: Os, title: &str, body: &str, has: impl Fn(&str) -> bool) -> Option<(String, Vec<String>)> {
    let (title, body) = (clean(title), clean(body));
    let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    match os {
        Os::Freedesktop if has("notify-send") => {
            let mut args = v(&["--app-name=Lumen", "--"]);
            args.extend([title, body]);
            Some(("notify-send".into(), args))
        }
        Os::Freedesktop if has("gdbus") => {
            let mut args = v(&[
                "call",
                "--session",
                "--dest",
                "org.freedesktop.Notifications",
                "--object-path",
                "/org/freedesktop/Notifications",
                "--method",
                "org.freedesktop.Notifications.Notify",
                "'Lumen'",
                "0",
                "''",
            ]);
            args.extend([gvariant_str(&title), gvariant_str(&body)]);
            args.extend(v(&["[]", "{}", "5000"]));
            Some(("gdbus".into(), args))
        }
        Os::Freedesktop if has("busctl") => {
            let mut args = v(&[
                "--user",
                "--",
                "call",
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "Notify",
                "susssasa{sv}i",
                "Lumen",
                "0",
                "",
            ]);
            args.extend([title, body]);
            args.extend(v(&["0", "0", "5000"]));
            Some(("busctl".into(), args))
        }
        Os::Mac => {
            let script =
                format!("display notification {} with title {}", applescript_str(&body), applescript_str(&title));
            Some(("osascript".into(), vec!["-e".into(), script]))
        }
        Os::Freedesktop | Os::Windows => None,
    }
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// Show a desktop notification (fire and forget; false when the system has no way to).
pub fn send(title: &str, body: &str) -> bool {
    let Some((prog, args)) = command(current_os(), title, body, on_path) else { return false };
    let Ok(mut child) =
        Command::new(prog).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    else {
        return false;
    };
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freedesktop_prefers_notify_send_then_dbus() {
        let all = |_: &str| true;
        let (p, a) = command(Os::Freedesktop, "Claude Code", "-Waiting for you", all).unwrap();
        assert_eq!(p, "notify-send");
        assert_eq!(a, vec!["--app-name=Lumen", "--", "Claude Code", "-Waiting for you"], "a leading dash is no option");
        let (p, a) = command(Os::Freedesktop, "it's \\ me", "Done", |b: &str| b == "gdbus").unwrap();
        assert_eq!(p, "gdbus");
        assert!(a.contains(&"org.freedesktop.Notifications.Notify".to_string()));
        assert_eq!(a[a.len() - 5], r"'it\'s \\ me'", "GVariant quoting");
        assert_eq!(a[a.len() - 4], "'Done'");
        assert_eq!(&a[a.len() - 3..], ["[]", "{}", "5000"]);
        let (p, a) = command(Os::Freedesktop, "T", "B\u{7}", |b: &str| b == "busctl").unwrap();
        assert_eq!(p, "busctl");
        assert_eq!(a[7], "susssasa{sv}i");
        assert_eq!(&a[a.len() - 5..], ["T", "B ", "0", "0", "5000"], "control characters are blanked");
        assert_eq!(command(Os::Freedesktop, "T", "B", |_: &str| false), None);
    }

    #[test]
    fn macos_uses_osascript_and_windows_nothing() {
        let (p, a) = command(Os::Mac, "Say \"hi\"", "a\\b", |_: &str| false).unwrap();
        assert_eq!(p, "osascript");
        assert_eq!(a, vec!["-e", r#"display notification "a\\b" with title "Say \"hi\"""#]);
        assert_eq!(command(Os::Windows, "T", "B", |_: &str| true), None);
    }
}
