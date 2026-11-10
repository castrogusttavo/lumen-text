//! Single-instance support: the first process listens on a Unix socket (on Windows: a localhost TCP
//! port recorded in a file); later invocations hand their file arguments to it and exit.

use crate::lsp::Wake;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, channel},
    thread,
};

#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let uid = unsafe { libc::getuid() };
    dir.join(format!("lumen-text-{uid}.sock"))
}

#[cfg(windows)]
pub fn socket_path() -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_default();
    std::env::temp_dir().join(format!("lumen-text-{user}.port"))
}

#[cfg(unix)]
fn connect(sock: &Path) -> std::io::Result<impl Write> {
    std::os::unix::net::UnixStream::connect(sock)
}

/// Bind the listener; yields one stream per incoming connection.
#[cfg(unix)]
fn bind(sock: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<impl Read>> + Send + 'static> {
    let l = std::os::unix::net::UnixListener::bind(sock)?;
    Ok(std::iter::from_fn(move || Some(l.accept().map(|c| c.0))))
}

#[cfg(windows)]
fn connect(sock: &Path) -> std::io::Result<impl Write> {
    let port: u16 = std::fs::read_to_string(sock)?.trim().parse().map_err(std::io::Error::other)?;
    std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
}

#[cfg(windows)]
fn bind(sock: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<impl Read>> + Send + 'static> {
    let l = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    std::fs::write(sock, l.local_addr()?.port().to_string())?;
    Ok(std::iter::from_fn(move || Some(l.accept().map(|c| c.0))))
}

pub enum Role {
    /// We own the socket; file arguments from later invocations arrive on the receiver.
    Primary(Receiver<Vec<String>>),
    /// Another instance took the arguments; this process should exit.
    Secondary,
}

/// Make `file:line:col` arguments absolute so the other process (different cwd) can open them.
pub fn absolutize(arg: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    // A Windows drive prefix (`C:\`) is not a `:line` suffix.
    let skip = if cfg!(windows) && arg.as_bytes().get(1) == Some(&b':') { 2 } else { 0 };
    let (path, suffix) = match arg[skip..].find(':').map(|i| i + skip) {
        Some(i) if !Path::new(arg).exists() => (&arg[..i], &arg[i..]),
        _ => (arg, ""),
    };
    let p = Path::new(path);
    let abs = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    format!("{}{suffix}", abs.display())
}

pub fn claim_at(sock: &Path, args: &[String], wake: Wake) -> Role {
    if let Ok(mut s) = connect(sock) {
        let msg = args.iter().map(|a| absolutize(a)).collect::<Vec<_>>().join("\n");
        if s.write_all(msg.as_bytes()).is_ok() {
            return Role::Secondary;
        }
    }
    // Nobody answered: any socket file left behind is stale.
    let _ = std::fs::remove_file(sock);
    let Ok(listener) = bind(sock) else {
        // Could not bind (read-only dir…): just run as an independent instance.
        let (_tx, rx) = channel();
        return Role::Primary(rx);
    };
    let (tx, rx) = channel();
    thread::spawn(move || {
        for conn in listener.flatten() {
            let mut conn = conn;
            let mut text = String::new();
            if conn.read_to_string(&mut text).is_ok() {
                let args: Vec<String> = text.lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
                if tx.send(args).is_err() {
                    return;
                }
                wake();
            }
        }
    });
    Role::Primary(rx)
}

pub fn claim(args: &[String], wake: Wake) -> Role {
    claim_at(&socket_path(), args, wake)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_process_hands_over_its_files() {
        let sock = std::env::temp_dir().join(format!("lumen-inst-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let wake: Wake = std::sync::Arc::new(|| {});
        let Role::Primary(rx) = claim_at(&sock, &[], wake.clone()) else { panic!("first claim must be primary") };
        let args = vec!["/tmp/a.ts:3:4".to_string(), "/tmp/b.ts".to_string()];
        assert!(matches!(claim_at(&sock, &args, wake), Role::Secondary));
        let got = rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        assert_eq!(got, args);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn stale_socket_is_replaced() {
        let sock = std::env::temp_dir().join(format!("lumen-stale-{}.sock", std::process::id()));
        std::fs::write(&sock, "").unwrap(); // a leftover file, nobody listening
        let wake: Wake = std::sync::Arc::new(|| {});
        assert!(matches!(claim_at(&sock, &[], wake), Role::Primary(_)));
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn relative_paths_become_absolute_and_keep_line_suffix() {
        let a = absolutize("Cargo.toml:12:3");
        assert!(Path::new(&a).is_absolute() && a.ends_with("Cargo.toml:12:3"), "{a}");
        #[cfg(unix)]
        assert_eq!(absolutize("/x/y.ts"), "/x/y.ts");
    }
}
