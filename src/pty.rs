//! Pseudo-terminal backend for the integrated terminal: the Unix PTY (Linux, macOS) through `libc`,
//! ConPTY on Windows through `portable-pty`.

#[cfg(unix)]
mod imp {
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
        child: Child,
    }

    fn winsize(rows: usize, cols: usize) -> libc::winsize {
        libc::winsize { ws_row: rows as u16, ws_col: cols as u16, ws_xpixel: 0, ws_ypixel: 0 }
    }

    impl Pty {
        pub fn spawn(cwd: &Path, rows: usize, cols: usize) -> io::Result<(Pty, Box<dyn Read + Send>)> {
            let (mut m, mut s) = (0, 0);
            let ws = winsize(rows, cols);
            // SAFETY: plain libc call with valid out-pointers.
            if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), &ws) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let (master, slave) = unsafe { (File::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
            let fallback = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" };
            let shell = std::env::var("SHELL").unwrap_or_else(|_| fallback.into());
            let mut cmd = Command::new(shell);
            cmd.current_dir(cwd)
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // The shell gets the PTY as its controlling terminal in its own session.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                    Ok(())
                });
            }
            let child = cmd.spawn()?;
            let reader = master.try_clone()?;
            Ok((Pty { master, child }, Box::new(reader)))
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
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(windows)]
mod imp {
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
            let pair = native_pty_system().openpty(size(rows, cols)).map_err(io::Error::other)?;
            let shell = std::env::var("LUMEN_SHELL").unwrap_or_else(|_| "powershell.exe".into());
            let mut cmd = CommandBuilder::new(shell);
            cmd.cwd(cwd);
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
            drop(pair.slave);
            let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
            let writer = pair.master.take_writer().map_err(io::Error::other)?;
            Ok((Pty { master: pair.master, writer, child }, reader))
        }

        pub fn write(&mut self, bytes: &[u8]) {
            let _ = self.writer.write_all(bytes);
            let _ = self.writer.flush();
        }

        pub fn resize(&mut self, rows: usize, cols: usize) {
            let _ = self.master.resize(size(rows, cols));
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
