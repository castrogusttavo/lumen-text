//! Pseudo-terminal backend for the integrated terminal: the Unix PTY (Linux, macOS) through `libc`,
//! ConPTY on Windows through `portable-pty`.
//!
//! `Pty::spawn` runs the user's shell; `Pty::spawn_cmd` runs any program (the AI panel's agent CLI)
//! with extra environment. Dropping a `Pty` ends the whole process group on Unix (SIGHUP, then
//! SIGKILL a moment later), so helpers the program started (an agent's tools, a shell's jobs) go too.

/// Environment changes for [`Pty::spawn_cmd`]: `Some(value)` sets a variable, `None` removes it.
pub type EnvEdit<'a> = (&'a str, Option<&'a str>);

#[cfg(unix)]
mod imp {
    use super::EnvEdit;
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        os::unix::process::CommandExt,
        path::Path,
        process::{Child, Command, Stdio},
    };

    pub struct Pty {
        master: File,
        child: Option<Child>,
        pid: i32,
    }

    fn winsize(rows: usize, cols: usize) -> libc::winsize {
        libc::winsize { ws_row: rows as u16, ws_col: cols as u16, ws_xpixel: 0, ws_ypixel: 0 }
    }

    impl Pty {
        pub fn spawn(cwd: &Path, rows: usize, cols: usize) -> io::Result<(Pty, Box<dyn Read + Send>)> {
            let fallback = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" };
            let shell = std::env::var("SHELL").unwrap_or_else(|_| fallback.into());
            Self::spawn_cmd(cwd, &shell, &[], &[], rows, cols)
        }

        /// Run `program` with `args` in `cwd` on a new PTY, with the user's environment plus
        /// `TERM` / `COLORTERM` and the `env` edits on top.
        pub fn spawn_cmd(
            cwd: &Path,
            program: &str,
            args: &[&str],
            env: &[EnvEdit],
            rows: usize,
            cols: usize,
        ) -> io::Result<(Pty, Box<dyn Read + Send>)> {
            let (mut m, mut s) = (0, 0);
            #[allow(unused_mut)]
            let mut ws = winsize(rows, cols);
            // macOS declares the termios and winsize arguments as mutable pointers, Linux as const.
            #[cfg(target_os = "macos")]
            let (term, wsp) = (std::ptr::null_mut(), &mut ws as *mut libc::winsize);
            #[cfg(not(target_os = "macos"))]
            let (term, wsp) = (std::ptr::null(), &ws as *const libc::winsize);
            // SAFETY: plain libc call with valid out-pointers.
            if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), term, wsp) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let (master, slave) = unsafe { (File::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
            let mut cmd = Command::new(program);
            cmd.args(args)
                .current_dir(cwd)
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            for (k, v) in env {
                match v {
                    Some(v) => cmd.env(k, v),
                    None => cmd.env_remove(k),
                };
            }
            // The program gets the PTY as its controlling terminal in its own session (so it also
            // leads its own process group, which `Drop` signals as a whole).
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                    Ok(())
                });
            }
            let child = cmd.spawn()?;
            let pid = child.id() as i32;
            let reader = master.try_clone()?;
            Ok((Pty { master, child: Some(child), pid }, Box::new(reader)))
        }

        /// The program's process id (it leads its own process group).
        pub fn pid(&self) -> Option<u32> {
            u32::try_from(self.pid).ok()
        }

        pub fn write(&mut self, bytes: &[u8]) {
            let _ = self.master.write_all(bytes);
            let _ = self.master.flush();
        }

        pub fn resize(&mut self, rows: usize, cols: usize) {
            let ws = winsize(rows, cols);
            // SAFETY: valid fd and winsize pointer.
            unsafe {
                libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws);
            }
        }

        /// Hang up: SIGHUP to the program's process group (what closing a terminal window does).
        pub fn hangup(&mut self) {
            // SAFETY: plain signal call on the group this Pty leads (the child is not reaped yet,
            // or the group still has members keeping its id reserved).
            unsafe {
                libc::killpg(self.pid, libc::SIGHUP);
            }
        }

        /// Name of the terminal's foreground process (the shell, or the command it runs), for a
        /// terminal tab's title. Linux only (`/proc`); `None` elsewhere.
        pub fn foreground_name(&self) -> Option<String> {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: plain query on a valid fd.
                let pg = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
                if pg <= 0 {
                    return None;
                }
                let name = std::fs::read_to_string(format!("/proc/{pg}/comm")).ok()?;
                let name = name.trim();
                (!name.is_empty()).then(|| name.to_string())
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = &self.master;
                None
            }
        }

        /// The job in the terminal's foreground when it is not the program itself (a shell running
        /// `npm run dev`): its command line, or `None` while the shell waits at its prompt. The
        /// program leads its own process group (`setsid`), so any other foreground group is a job.
        pub fn foreground_job(&self) -> Option<String> {
            // SAFETY: plain ioctl on our own, open master fd.
            let pg = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
            if pg <= 0 || pg == self.pid || self.child.is_none() {
                return None;
            }
            Some(super::process_command(pg).unwrap_or_else(|| format!("process {pg}")))
        }

        /// Exit code once the program has ended (a signal counts as 128 + its number).
        pub fn try_wait(&mut self) -> Option<i32> {
            use std::os::unix::process::ExitStatusExt;
            let st = self.child.as_mut()?.try_wait().ok()??;
            Some(st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0)))
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            let Some(mut child) = self.child.take() else { return };
            let pid = self.pid;
            // SAFETY: plain signal calls. The child is not reaped yet, so its pid (= the group id,
            // it called setsid) cannot have been reused by an unrelated process group.
            unsafe {
                libc::killpg(pid, libc::SIGHUP);
            }
            // Give the group a moment to exit cleanly, then make sure; reaping happens off the UI thread.
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
                let _ = child.wait();
            });
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::EnvEdit;
    use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
    use std::{
        io::{self, Read, Write},
        path::Path,
    };

    pub struct Pty {
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
    }

    fn size(rows: usize, cols: usize) -> PtySize {
        PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 }
    }

    impl Pty {
        pub fn spawn(cwd: &Path, rows: usize, cols: usize) -> io::Result<(Pty, Box<dyn Read + Send>)> {
            let shell = std::env::var("LUMEN_SHELL").unwrap_or_else(|_| "powershell.exe".into());
            Self::spawn_cmd(cwd, &shell, &[], &[], rows, cols)
        }

        /// Run `program` with `args` in `cwd` on a new ConPTY, with the user's environment plus
        /// `TERM` / `COLORTERM` and the `env` edits on top.
        pub fn spawn_cmd(
            cwd: &Path,
            program: &str,
            args: &[&str],
            env: &[EnvEdit],
            rows: usize,
            cols: usize,
        ) -> io::Result<(Pty, Box<dyn Read + Send>)> {
            let pair = native_pty_system().openpty(size(rows, cols)).map_err(io::Error::other)?;
            let mut cmd = CommandBuilder::new(program);
            cmd.args(args);
            cmd.cwd(cwd);
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            for (k, v) in env {
                match v {
                    Some(v) => cmd.env(k, v),
                    None => cmd.env_remove(k),
                }
            }
            let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
            drop(pair.slave);
            let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
            let writer = pair.master.take_writer().map_err(io::Error::other)?;
            Ok((Pty { master: pair.master, writer, child }, reader))
        }

        /// The program's process id.
        pub fn pid(&self) -> Option<u32> {
            self.child.process_id()
        }

        pub fn write(&mut self, bytes: &[u8]) {
            let _ = self.writer.write_all(bytes);
            let _ = self.writer.flush();
        }

        pub fn resize(&mut self, rows: usize, cols: usize) {
            let _ = self.master.resize(size(rows, cols));
        }

        /// Hang up: ConPTY has no process groups, so the program is terminated.
        pub fn hangup(&mut self) {
            let _ = self.child.kill();
        }

        /// Foreground process name: not available through ConPTY.
        pub fn foreground_name(&self) -> Option<String> {
            None
        }

        /// ConPTY has no foreground process group to ask: never reports a job.
        pub fn foreground_job(&self) -> Option<String> {
            None
        }

        /// Exit code once the program has ended.
        pub fn try_wait(&mut self) -> Option<i32> {
            self.child.try_wait().ok()?.map(|s| s.exit_code() as i32)
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub use imp::Pty;

/// End the process groups led by `pids` before the app exits: SIGHUP + SIGTERM, a short grace
/// period, then SIGKILL. Synchronous on purpose: `Drop` only reaps from a detached thread, and on
/// exit (`process::exit`, or the event loop returning) that thread never gets to send the SIGKILL,
/// which left a terminal's background jobs (dev servers, `tsc`) running after Lumen closed.
/// Pass only leaders this process has not reaped yet (their pid stays reserved), so a recycled
/// pid is never signalled. Signalling a group that is already gone is harmless.
#[cfg(unix)]
pub fn kill_groups(pids: &[u32]) {
    let groups: Vec<i32> = pids.iter().filter_map(|p| i32::try_from(*p).ok()).filter(|p| *p > 1).collect();
    if groups.is_empty() {
        return;
    }
    for g in &groups {
        // SAFETY: plain signal calls on groups this process created.
        unsafe {
            libc::killpg(*g, libc::SIGHUP);
            libc::killpg(*g, libc::SIGTERM);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    for g in &groups {
        unsafe { libc::killpg(*g, libc::SIGKILL) };
    }
}

#[cfg(not(unix))]
pub fn kill_groups(_pids: &[u32]) {}

/// Longest command line [`Pty::foreground_job`] reports (it ends up in a one-line question).
#[cfg(unix)]
const MAX_JOB_NAME: usize = 60;

/// Command line of process `pid` (`/proc` on Linux, `ps` elsewhere), shortened for display.
#[cfg(unix)]
fn process_command(pid: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    let raw = {
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let args: Vec<String> =
            cmd.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        if args.is_empty() { std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()? } else { args.join(" ") }
    };
    #[cfg(not(target_os = "linux"))]
    let raw = {
        let out = std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    short_command(&raw)
}

/// `raw` on one line, without surrounding blanks, cut to [`MAX_JOB_NAME`] characters.
#[cfg(unix)]
fn short_command(raw: &str) -> Option<String> {
    let one: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.is_empty() {
        return None;
    }
    if one.chars().count() <= MAX_JOB_NAME {
        return Some(one);
    }
    let cut: String = one.chars().take(MAX_JOB_NAME - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_until(mut r: Box<dyn Read + Send>, needle: &str) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut out = String::new();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < end && !out.contains(needle) {
            if let Ok(b) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
                out.push_str(&String::from_utf8_lossy(&b));
            }
        }
        out
    }

    #[test]
    #[cfg(unix)]
    fn spawn_cmd_runs_a_program_with_args_and_env() {
        let env = [("LUMEN_PTY_FOO", Some("bar-42")), ("LUMEN_PTY_GONE", None)];
        let (mut pty, reader) = Pty::spawn_cmd(
            &std::env::temp_dir(),
            "sh",
            &["-c", "echo \"[$LUMEN_PTY_FOO|${LUMEN_PTY_GONE:-unset}|$TERM]\"; exit 3"],
            &env,
            10,
            60,
        )
        .unwrap();
        let out = read_until(reader, "]");
        assert!(out.contains("[bar-42|unset|xterm-256color]"), "{out:?}");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut code = None;
        while code.is_none() && std::time::Instant::now() < end {
            code = pty.try_wait();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(code, Some(3), "exit code is reported");
    }

    #[test]
    #[cfg(unix)]
    fn command_lines_are_shortened_to_one_line() {
        assert_eq!(short_command("npm  run\ndev\n").as_deref(), Some("npm run dev"));
        assert_eq!(short_command("  \n"), None);
        let s = short_command(&"x".repeat(200)).unwrap();
        assert_eq!(s.chars().count(), MAX_JOB_NAME);
        assert!(s.ends_with('…'));
    }

    /// Wait up to `secs` for `f` to return `Some`.
    #[cfg(unix)]
    fn poll<T>(secs: u64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if let Some(v) = f() {
                return Some(v);
            }
            if std::time::Instant::now() > end {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_job_in_the_foreground_is_reported_but_an_idle_shell_is_not() {
        let dir = std::env::temp_dir();
        // Job control on (`-m`): the shell runs `sleep` in a process group of its own and hands it
        // the terminal. The trailing `true` keeps the shell from exec'ing `sleep` in its place.
        let (pty, _r) = Pty::spawn_cmd(&dir, "sh", &["-m", "-c", "sleep 30; true"], &[], 10, 60).unwrap();
        let job = poll(10, || pty.foreground_job());
        assert!(job.as_deref().is_some_and(|j| j.contains("sleep")), "{job:?}");
        drop(pty);
        // A shell waiting for input owns the foreground itself: nothing to report.
        let (pty, _r) = Pty::spawn_cmd(&dir, "sh", &["-m", "-c", "read x; true"], &[], 10, 60).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(pty.foreground_job(), None);
    }

    #[test]
    #[cfg(unix)]
    fn dropping_the_pty_ends_the_whole_process_group() {
        // The child starts a background grandchild that would outlive a plain kill of the child.
        let dir = std::env::temp_dir().join(format!("lumen-pty-group-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; echo started; wait", pidfile.display());
        let (pty, reader) = Pty::spawn_cmd(&dir, "sh", &["-c", &script], &[], 10, 60).unwrap();
        let out = read_until(reader, "started");
        assert!(out.contains("started"), "{out:?}");
        let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        drop(pty);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // SAFETY: signal 0 only checks that the process exists.
        while std::time::Instant::now() < end && unsafe { libc::kill(pid, 0) } == 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // A zombie still answers kill(0) until its parent (init) reaps it: look at its state too.
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let gone = unsafe { libc::kill(pid, 0) } != 0 || state.split_whitespace().nth(2) == Some("Z");
        assert!(gone, "the grandchild was signalled with the group");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn kill_groups_ends_a_group_that_ignores_hangup_and_term() {
        // A dev server that traps HUP/TERM (and outlives its shell) must still die when Lumen quits.
        let dir = std::env::temp_dir().join(format!("lumen-pty-kill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let script = format!("(trap '' HUP TERM; sleep 30) & echo $! > '{}'; echo started; wait", pidfile.display());
        let (pty, reader) = Pty::spawn_cmd(&dir, "sh", &["-c", &script], &[], 10, 60).unwrap();
        let out = read_until(reader, "started");
        assert!(out.contains("started"), "{out:?}");
        let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        super::kill_groups(&[pty.pid().unwrap()]);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // SAFETY: signal 0 only checks that the process exists.
        while std::time::Instant::now() < end && unsafe { libc::kill(pid, 0) } == 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let gone = unsafe { libc::kill(pid, 0) } != 0 || state.split_whitespace().nth(2) == Some("Z");
        assert!(gone, "the process that ignores HUP and TERM was killed");
        drop(pty);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
