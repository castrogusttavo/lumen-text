//! Remote side of the Git panel: ahead/behind counts, and fetch / pull / push run on a worker
//! thread so the UI never blocks. Progress lines (`Receiving objects: 45%`) are streamed through a
//! channel; failures are classified (no upstream, rejected, diverged, auth, network…) and turned
//! into short Portuguese messages. Pure logic plus `std::process`; covered by tests against real
//! temporary repositories with a local bare repository as the "remote".

use crate::app_gitpanel::git_run;
use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc::{Receiver, Sender, channel},
    time::Instant,
};

/// Where the current branch stands against its upstream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncInfo {
    /// `origin/main`; None when the branch has no upstream (or HEAD is detached).
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

/// `git rev-list --left-right --count @{u}...HEAD` prints `<behind>\t<ahead>`; returns (ahead, behind).
pub fn parse_counts(out: &str) -> Option<(usize, usize)> {
    let mut it = out.split_whitespace();
    let behind = it.next()?.parse().ok()?;
    let ahead = it.next()?.parse().ok()?;
    Some((ahead, behind))
}

pub fn info(top: &Path) -> SyncInfo {
    let Some((ahead, behind)) = git_run(top, &["rev-list", "--left-right", "--count", "@{u}...HEAD"]).ok().and_then(|o| parse_counts(&o)) else {
        return SyncInfo::default();
    };
    let upstream = git_run(top, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    SyncInfo { upstream, ahead, behind }
}

/// Did the local branch rewrite history that is already on the upstream (amend, rebase, reset)?
/// True when the upstream has commits HEAD lacks and every one of them was once checked out or
/// committed here (it is in HEAD's reflog), i.e. they are the old versions of our own commits and
/// not somebody else's work. Overwriting them (force with lease) is then what the user means;
/// otherwise the right move is to pull first.
pub fn rewrote_published(top: &Path) -> bool {
    let Ok(theirs) = git_run(top, &["rev-list", "HEAD..@{u}"]) else { return false };
    let theirs: Vec<&str> = theirs.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if theirs.is_empty() {
        return false;
    }
    let Ok(log) = git_run(top, &["reflog", "--format=%H", "HEAD"]) else { return false };
    let mine: std::collections::HashSet<&str> = log.lines().map(str::trim).collect();
    theirs.iter().all(|h| mine.contains(h))
}

/// Remote a branch without upstream should be published to: `branch.<b>.pushRemote`,
/// `remote.pushDefault`, `origin`, else the only / first remote. None = no remotes at all.
pub fn default_remote(top: &Path, branch: &str) -> Option<String> {
    let remotes: Vec<String> = git_run(top, &["remote"]).ok()?.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    pick_remote(&remotes, git_run(top, &["config", "--get", &format!("branch.{branch}.pushRemote")]).ok().as_deref(), git_run(top, &["config", "--get", "remote.pushDefault"]).ok().as_deref())
}

pub fn pick_remote(remotes: &[String], branch_push: Option<&str>, push_default: Option<&str>) -> Option<String> {
    let known = |n: Option<&str>| n.map(str::trim).filter(|n| remotes.iter().any(|r| r == n)).map(str::to_string);
    known(branch_push).or_else(|| known(push_default)).or_else(|| known(Some("origin"))).or_else(|| remotes.first().cloned())
}

/// Message of the last commit (what "Amend" prefills).
pub fn last_message(top: &Path) -> Option<String> {
    let m = git_run(top, &["log", "-1", "--format=%B"]).ok()?;
    let m = m.trim_end().to_string();
    (!m.is_empty()).then_some(m)
}

/// `git commit --amend -m msg`: rewrites the last commit with the staged changes and `msg`.
pub fn commit_amend(top: &Path, msg: &str) -> Result<String, String> {
    if msg.trim().is_empty() {
        return Err("escreva uma mensagem de commit".into());
    }
    git_run(top, &["commit", "--amend", "-m", msg.trim()]).map(|o| o.lines().next().unwrap_or("commit emendado").to_string())
}

/// How a pull integrates the remote commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullStrategy {
    Merge,
    Rebase,
    FfOnly,
}

impl PullStrategy {
    /// Name stored in `git_pull_strategy`.
    pub fn name(self) -> &'static str {
        match self {
            PullStrategy::Merge => "merge",
            PullStrategy::Rebase => "rebase",
            PullStrategy::FfOnly => "ff-only",
        }
    }

    pub fn from_name(s: &str) -> Option<PullStrategy> {
        Some(match s {
            "merge" => PullStrategy::Merge,
            "rebase" => PullStrategy::Rebase,
            "ff-only" => PullStrategy::FfOnly,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            PullStrategy::Merge => "Merge",
            PullStrategy::Rebase => "Rebase",
            PullStrategy::FfOnly => "Só avanço (ff-only)",
        }
    }

    /// One line for the picker / tooltip.
    pub fn hint(self) -> &'static str {
        match self {
            PullStrategy::Merge => "Cria um commit de merge quando os históricos divergiram",
            PullStrategy::Rebase => "Reaplica os seus commits locais por cima dos do remoto",
            PullStrategy::FfOnly => "Só avança quando não há divergência; senão, recusa",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Fetch,
    /// Pull with whatever git is configured to do, else fast-forward only.
    Pull,
    /// Pull with an explicit strategy.
    PullWith(PullStrategy),
    Push,
    /// `push --force-with-lease`: overwrites the remote branch only if it is still where the last
    /// fetch left it.
    ForcePush,
    /// First push of a branch: `push --set-upstream <remote> <branch>`.
    Publish { remote: String, branch: String },
}

impl Op {
    /// Name used in messages.
    pub fn label(&self) -> &'static str {
        match self {
            Op::Fetch => "Fetch",
            Op::Pull | Op::PullWith(_) => "Pull",
            Op::Push | Op::ForcePush | Op::Publish { .. } => "Push",
        }
    }

    pub fn busy(&self) -> &'static str {
        match self {
            Op::Fetch => "Buscando",
            Op::Pull | Op::PullWith(_) => "Baixando",
            Op::Push => "Enviando",
            Op::ForcePush => "Forçando o envio",
            Op::Publish { .. } => "Publicando",
        }
    }

    fn done(&self) -> &'static str {
        match self {
            Op::Fetch => "Fetch concluído",
            Op::Pull | Op::PullWith(_) => "Pull concluído",
            Op::Push => "Push concluído",
            Op::ForcePush => "Push forçado concluído",
            Op::Publish { .. } => "Branch publicado",
        }
    }
}

/// Arguments of `op`. A plain `git pull` refuses to guess between merge and rebase when the
/// branches diverged, so without any `pull.rebase` / `pull.ff` setting it only fast-forwards.
pub fn args_for(op: &Op, pull_configured: bool) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match op {
        Op::Fetch => s(&["fetch", "--progress"]),
        Op::Pull if pull_configured => s(&["pull", "--progress", "--no-edit"]),
        Op::Pull => s(&["pull", "--progress", "--no-edit", "--ff-only"]),
        Op::PullWith(PullStrategy::Merge) => s(&["pull", "--progress", "--no-edit", "--no-rebase"]),
        Op::PullWith(PullStrategy::Rebase) => s(&["pull", "--progress", "--no-edit", "--rebase"]),
        Op::PullWith(PullStrategy::FfOnly) => s(&["pull", "--progress", "--no-edit", "--ff-only"]),
        Op::Push => s(&["push", "--progress"]),
        Op::ForcePush => s(&["push", "--progress", "--force-with-lease"]),
        Op::Publish { remote, branch } => {
            let mut v = s(&["push", "--progress", "--set-upstream"]);
            v.extend([remote.clone(), branch.clone()]);
            v
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailKind {
    /// The branch has no upstream: the panel offers to publish it.
    NoUpstream,
    Rejected,
    /// `--force-with-lease` refused: the remote moved since the last fetch.
    StaleLease,
    Diverged,
    /// A merge / rebase stopped on conflicting files.
    Conflict,
    Auth,
    Network,
    Dirty,
    Cancelled,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fail {
    pub kind: FailKind,
    pub text: String,
}

/// Turn git's stderr lines into a classified, short Portuguese message.
pub fn classify(lines: &[String]) -> Fail {
    let all = lines.join("\n");
    let has = |s: &str| all.contains(s);
    let (kind, text) = if has("has no upstream branch") || has("no upstream configured") || has("There is no tracking information") {
        (FailKind::NoUpstream, "O branch ainda não tem upstream")
    } else if has("stale info") {
        (FailKind::StaleLease, "O remoto mudou desde o último fetch: faça fetch e confira o que chegou antes de forçar de novo")
    } else if has("[rejected]") || has("non-fast-forward") || has("fetch first") || has("[remote rejected]") && has("fetch first") {
        (FailKind::Rejected, "Rejeitado: o remoto tem commits que você não tem (ou você reescreveu commits já publicados)")
    } else if has("CONFLICT") || has("Automatic merge failed") || has("Could not apply") || has("could not apply") {
        let how = if has("rebase") { "resolva os arquivos e rode git rebase --continue (ou --abort) no terminal" } else { "resolva os arquivos marcados e faça commit (ou git merge --abort)" };
        return Fail { kind: FailKind::Conflict, text: format!("Conflitos ao integrar: {how}") };
    } else if has("Not possible to fast-forward") || has("divergent branches") || has("need to specify how to reconcile") {
        (FailKind::Diverged, "Os branches divergiram: escolha como integrar (merge, rebase ou só avanço)")
    } else if has("Authentication failed") || has("could not read Username") || has("could not read Password") || has("Permission denied") || has("terminal prompts disabled") || has("Host key verification failed") {
        (FailKind::Auth, "Falha de autenticação: o editor não pergunta senhas; configure o ssh-agent ou um credential helper")
    } else if has("Could not resolve host") || has("Could not read from remote repository") || has("unable to access") || has("Connection timed out") || has("Network is unreachable") || has("Connection refused") {
        (FailKind::Network, "Sem acesso ao remoto (rede ou endereço)")
    } else if has("would be overwritten") || has("commit your changes or stash") || has("You have unstaged changes") || has("unmerged files") {
        (FailKind::Dirty, "Alterações locais impedem a operação: faça commit ou descarte antes")
    } else if has("terminated by signal") || has("Terminated") {
        (FailKind::Cancelled, "Cancelado")
    } else {
        let pick = lines.iter().rev().find(|l| l.starts_with("fatal:") || l.starts_with("error:")).or(lines.iter().rev().find(|l| !l.trim().is_empty()));
        let t = pick.map(|l| l.trim_start_matches("fatal:").trim_start_matches("error:").trim().to_string()).unwrap_or_else(|| "git falhou".into());
        return Fail { kind: FailKind::Other, text: t };
    };
    Fail { kind, text: text.to_string() }
}

/// Success message from the command's output.
pub fn summarize(op: &Op, stdout: &str, stderr: &[String]) -> String {
    let err = stderr.join("\n");
    match op {
        Op::Pull | Op::PullWith(_) if stdout.contains("Already up to date") || stdout.contains("Already up-to-date") => "Já está atualizado".into(),
        Op::Push | Op::ForcePush if err.contains("Everything up-to-date") => "Nada para enviar".into(),
        Op::PullWith(PullStrategy::Rebase) if stdout.contains("Successfully rebased") || err.contains("Successfully rebased") => "Pull concluído (rebase)".into(),
        Op::Publish { remote, branch } => format!("Branch publicado: {remote}/{branch}"),
        _ => op.done().to_string(),
    }
}

/// Split a chunk of stderr into progress lines: git rewrites its progress with `\r`.
pub fn split_progress(chunk: &str) -> Vec<String> {
    chunk.split(['\r', '\n']).map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

pub enum Msg {
    Progress(String),
    Done(Result<String, Fail>),
}

/// A git command running on a worker thread.
pub struct Job {
    pub op: Op,
    pub started: Instant,
    pub rx: Receiver<Msg>,
    /// Last progress line git printed.
    pub progress: String,
    pid: u32,
    pub cancelled: bool,
}

impl Job {
    /// Stop the command (the whole process group: ssh / credential helpers included).
    pub fn cancel(&mut self) {
        if self.pid != 0 && !self.cancelled {
            self.cancelled = true;
            unsafe { libc::kill(-(self.pid as i32), libc::SIGTERM) };
        }
    }

    /// Drain pending messages; Some(result) once the command finished.
    pub fn poll(&mut self) -> Option<Result<String, Fail>> {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Progress(p) => self.progress = p,
                Msg::Done(r) => {
                    return Some(match r {
                        Err(_) if self.cancelled => Err(Fail { kind: FailKind::Cancelled, text: "Cancelado".into() }),
                        r => r,
                    });
                }
            }
        }
        None
    }
}

fn finish(tx: &Sender<Msg>, wake: &(dyn Fn() + Send + Sync), r: Result<String, Fail>) {
    let _ = tx.send(Msg::Done(r));
    wake();
}

/// Start `op` in the repository at `top`. `wake` is called after every message so the event loop
/// redraws. git never prompts: no terminal (new session) and `GIT_TERMINAL_PROMPT=0`.
pub fn spawn(top: &Path, op: Op, wake: crate::lsp::Wake) -> Job {
    let (tx, rx) = channel();
    let pull_configured = git_run(top, &["config", "--get", "pull.rebase"]).is_ok() || git_run(top, &["config", "--get", "pull.ff"]).is_ok();
    let args = args_for(&op, pull_configured);
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(top).args(&args).env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and runs in the forked child before exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut job = Job { op: op.clone(), started: Instant::now(), rx, progress: String::new(), pid: 0, cancelled: false };
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            finish(&tx, &*wake, Err(Fail { kind: FailKind::Other, text: format!("git: {e}") }));
            return job;
        }
    };
    job.pid = child.id();
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    std::thread::spawn(move || {
        let out_thread = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(o) = out.as_mut() {
                let mut b = Vec::new();
                let _ = o.read_to_end(&mut b);
                s = String::from_utf8_lossy(&b).into_owned();
            }
            s
        });
        let mut lines: Vec<String> = Vec::new();
        let mut buf = [0u8; 4096];
        let mut carry = Vec::new();
        if let Some(e) = err.as_mut() {
            loop {
                match e.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        carry.extend_from_slice(&buf[..n]);
                        // Only complete lines (ending in \r or \n) are reported.
                        if let Some(cut) = carry.iter().rposition(|b| *b == b'\r' || *b == b'\n') {
                            let chunk = String::from_utf8_lossy(&carry[..=cut]).into_owned();
                            carry.drain(..=cut);
                            for l in split_progress(&chunk) {
                                let _ = tx.send(Msg::Progress(l.clone()));
                                lines.push(l);
                            }
                            wake();
                        }
                    }
                }
            }
        }
        lines.extend(split_progress(&String::from_utf8_lossy(&carry)));
        let status = child.wait();
        let stdout = out_thread.join().unwrap_or_default();
        let ok = status.as_ref().is_ok_and(|s| s.success());
        // Progress percentages are noise in an error message.
        let keep: Vec<String> = lines.into_iter().filter(|l| !l.contains("% (") && !l.starts_with("remote: Counting") && !l.starts_with("remote: Compressing")).collect();
        let res = if ok {
            Ok(summarize(&op, &stdout, &keep))
        } else {
            let mut f = classify(&keep);
            if status.as_ref().is_ok_and(|s| std::os::unix::process::ExitStatusExt::signal(s).is_some()) {
                f = Fail { kind: FailKind::Cancelled, text: "Cancelado".into() };
            }
            Err(f)
        };
        finish(&tx, &*wake, res);
    });
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::Duration};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn init(dir: &Path, bare: bool) {
        let mut a = vec!["init", "-q", "-b", "main"];
        if bare {
            a.push("--bare");
        }
        a.push(dir.to_str().unwrap());
        assert!(Command::new("git").args(&a).status().unwrap().success());
        for kv in [["user.email", "t@t"], ["user.name", "Ana"], ["commit.gpgsign", "false"]] {
            git_run(dir, &["config", kv[0], kv[1]]).unwrap();
        }
    }

    fn commit_file(dir: &Path, name: &str, text: &str, msg: &str) {
        std::fs::write(dir.join(name), text).unwrap();
        git_run(dir, &["add", "-A"]).unwrap();
        git_run(dir, &["commit", "-q", "-m", msg]).unwrap();
    }

    fn wait(mut job: Job) -> Result<String, Fail> {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(r) = job.poll() {
                return r;
            }
            assert!(Instant::now() < end, "job timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn noop() -> crate::lsp::Wake {
        std::sync::Arc::new(|| {})
    }

    #[test]
    fn counts_and_remote_choice() {
        assert_eq!(parse_counts("2\t5\n"), Some((5, 2)));
        assert_eq!(parse_counts("0\t0"), Some((0, 0)));
        assert_eq!(parse_counts("lixo"), None);
        let r = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_remote(&r(&["up", "origin"]), None, None).as_deref(), Some("origin"));
        assert_eq!(pick_remote(&r(&["up", "fork"]), None, None).as_deref(), Some("up"));
        assert_eq!(pick_remote(&r(&["up", "fork"]), Some("fork\n"), None).as_deref(), Some("fork"));
        assert_eq!(pick_remote(&r(&["up", "fork"]), Some("ghost"), Some("fork")).as_deref(), Some("fork"));
        assert_eq!(pick_remote(&[], None, None), None);
    }

    #[test]
    fn errors_are_classified() {
        let l = |s: &str| s.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(classify(&l("fatal: The current branch x has no upstream branch.\nTo push the current branch...")).kind, FailKind::NoUpstream);
        assert_eq!(classify(&l("To /tmp/r\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs")).kind, FailKind::Rejected);
        assert_eq!(classify(&l("fatal: Not possible to fast-forward, aborting.")).kind, FailKind::Diverged);
        assert_eq!(classify(&l("fatal: could not read Username for 'https://x': terminal prompts disabled")).kind, FailKind::Auth);
        assert_eq!(classify(&l("fatal: unable to access 'https://x/': Could not resolve host: x")).kind, FailKind::Network);
        assert_eq!(classify(&l("error: Your local changes to the following files would be overwritten by merge:\n\ta.ts")).kind, FailKind::Dirty);
        let o = classify(&l("hint: foo\nfatal: something odd"));
        assert_eq!((o.kind, o.text.as_str()), (FailKind::Other, "something odd"));
        assert_eq!(split_progress("Receiving objects:  45% (9/20)\rReceiving objects: 100% (20/20)\nDone\n"), ["Receiving objects:  45% (9/20)", "Receiving objects: 100% (20/20)", "Done"]);
    }

    #[test]
    fn strategies_have_names_and_arguments() {
        for st in [PullStrategy::Merge, PullStrategy::Rebase, PullStrategy::FfOnly] {
            assert_eq!(PullStrategy::from_name(st.name()), Some(st));
            assert!(!st.label().is_empty() && !st.hint().is_empty());
        }
        assert_eq!(PullStrategy::from_name(""), None);
        let has = |st, flag: &str| args_for(&Op::PullWith(st), true).iter().any(|a| a == flag);
        assert!(has(PullStrategy::Merge, "--no-rebase") && !has(PullStrategy::Merge, "--ff-only"));
        assert!(has(PullStrategy::Rebase, "--rebase"));
        assert!(has(PullStrategy::FfOnly, "--ff-only"));
        assert_eq!(args_for(&Op::ForcePush, false), ["push", "--progress", "--force-with-lease"]);
        let l = |s: &str| s.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(classify(&l(" ! [rejected]        main -> main (stale info)\nerror: failed to push")).kind, FailKind::StaleLease);
        assert_eq!(classify(&l("CONFLICT (content): Merge conflict in a.txt\nAutomatic merge failed; fix conflicts")).kind, FailKind::Conflict);
        assert_eq!(classify(&l("error: could not apply 1234abc... local\nhint: Resolve all conflicts manually, then run \"git rebase --continue\"")).kind, FailKind::Conflict);
    }

    #[test]
    fn force_with_lease_overwrites_an_amended_commit_but_not_news_from_the_remote() {
        let base = tmp("force");
        let (remote, a, b) = (base.join("remote.git"), base.join("a"), base.join("b"));
        init(&remote, true);
        init(&a, false);
        git_run(&a, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
        commit_file(&a, "f.txt", "one\n", "first");
        wait(spawn(&a, Op::Publish { remote: "origin".into(), branch: "main".into() }, noop())).unwrap();
        // Amend a published commit: the plain push is rejected, the forced one goes through.
        commit_amend(&a, "first, reworded").unwrap();
        let e = wait(spawn(&a, Op::Push, noop())).unwrap_err();
        assert_eq!(e.kind, FailKind::Rejected, "{e:?}");
        assert_eq!((info(&a).ahead, info(&a).behind), (1, 1), "the amended commit and its old version");
        assert!(rewrote_published(&a), "the commit only the remote has is our own old version");
        assert_eq!(wait(spawn(&a, Op::ForcePush, noop())).unwrap(), "Push forçado concluído");
        assert_eq!(git_run(&remote, &["log", "-1", "--format=%s", "main"]).unwrap().trim(), "first, reworded");
        assert_eq!(wait(spawn(&a, Op::ForcePush, noop())).unwrap(), "Nada para enviar");

        // Someone else pushes after our last fetch: the lease protects their commit.
        assert!(Command::new("git").args(["clone", "-q", remote.to_str().unwrap(), b.to_str().unwrap()]).status().unwrap().success());
        for kv in [["user.email", "t@t"], ["user.name", "Beto"], ["commit.gpgsign", "false"]] {
            git_run(&b, &["config", kv[0], kv[1]]).unwrap();
        }
        commit_file(&b, "g.txt", "theirs\n", "theirs");
        git_run(&b, &["push", "-q"]).unwrap();
        commit_amend(&a, "reworded again").unwrap();
        let e = wait(spawn(&a, Op::ForcePush, noop())).unwrap_err();
        assert_eq!(e.kind, FailKind::StaleLease, "{e:?}");
        assert_eq!(git_run(&remote, &["log", "-1", "--format=%s", "main"]).unwrap().trim(), "theirs", "their commit survived");
        // After fetching, their commit shows up on the remote side and is not ours: no force offered.
        wait(spawn(&a, Op::Fetch, noop())).unwrap();
        assert!(!rewrote_published(&a), "somebody else's commit is among the ones a force would drop");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn diverged_pull_works_with_each_strategy() {
        // Merge and rebase integrate; ff-only refuses.
        for (name, st) in [("merge", PullStrategy::Merge), ("rebase", PullStrategy::Rebase), ("ff", PullStrategy::FfOnly)] {
            let base = tmp(&format!("strat-{name}"));
            let (remote, a, b) = (base.join("remote.git"), base.join("a"), base.join("b"));
            init(&remote, true);
            init(&a, false);
            git_run(&a, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
            commit_file(&a, "f.txt", "one\n", "first");
            wait(spawn(&a, Op::Publish { remote: "origin".into(), branch: "main".into() }, noop())).unwrap();
            assert!(Command::new("git").args(["clone", "-q", remote.to_str().unwrap(), b.to_str().unwrap()]).status().unwrap().success());
            for kv in [["user.email", "t@t"], ["user.name", "Beto"], ["commit.gpgsign", "false"]] {
                git_run(&b, &["config", kv[0], kv[1]]).unwrap();
            }
            commit_file(&b, "b.txt", "b\n", "remote");
            git_run(&b, &["push", "-q"]).unwrap();
            commit_file(&a, "a.txt", "a\n", "local");
            let r = wait(spawn(&a, Op::PullWith(st), noop()));
            match st {
                PullStrategy::FfOnly => assert_eq!(r.unwrap_err().kind, FailKind::Diverged),
                _ => {
                    r.unwrap();
                    assert!(a.join("b.txt").exists() && a.join("a.txt").exists());
                    let merges = git_run(&a, &["rev-list", "--merges", "--count", "HEAD"]).unwrap();
                    assert_eq!(merges.trim(), if st == PullStrategy::Merge { "1" } else { "0" }, "{name}");
                    assert_eq!(info(&a), SyncInfo { upstream: Some("origin/main".into()), ahead: if st == PullStrategy::Merge { 2 } else { 1 }, behind: 0 });
                }
            }
            std::fs::remove_dir_all(&base).ok();
        }
    }

    #[test]
    fn pull_args_respect_configuration() {
        assert!(args_for(&Op::Pull, false).contains(&"--ff-only".to_string()));
        assert!(!args_for(&Op::Pull, true).contains(&"--ff-only".to_string()));
        assert_eq!(args_for(&Op::Publish { remote: "o".into(), branch: "b".into() }, false), ["push", "--progress", "--set-upstream", "o", "b"]);
    }

    #[test]
    fn publish_push_fetch_pull_against_a_local_remote() {
        let base = tmp("flow");
        let (remote, a, b) = (base.join("remote.git"), base.join("a"), base.join("b"));
        init(&remote, true);
        init(&a, false);
        git_run(&a, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
        commit_file(&a, "f.txt", "one\n", "first");
        assert_eq!(info(&a), SyncInfo::default(), "no upstream yet");
        assert_eq!(default_remote(&a, "main").as_deref(), Some("origin"));
        assert_eq!(last_message(&a).as_deref(), Some("first"));

        // A plain push has nothing to push to: the failure says "no upstream".
        let e = wait(spawn(&a, Op::Push, noop())).unwrap_err();
        assert_eq!(e.kind, FailKind::NoUpstream, "{e:?}");

        let ok = wait(spawn(&a, Op::Publish { remote: "origin".into(), branch: "main".into() }, noop())).unwrap();
        assert_eq!(ok, "Branch publicado: origin/main");
        assert_eq!(info(&a), SyncInfo { upstream: Some("origin/main".into()), ahead: 0, behind: 0 });

        commit_file(&a, "f.txt", "one\ntwo\n", "second");
        assert_eq!(info(&a).ahead, 1);
        assert_eq!(wait(spawn(&a, Op::Push, noop())).unwrap(), "Push concluído");
        assert_eq!(info(&a).ahead, 0);
        assert_eq!(wait(spawn(&a, Op::Push, noop())).unwrap(), "Nada para enviar");

        // Second clone pushes a commit; `a` sees it only after the fetch, then pulls it.
        assert!(Command::new("git").args(["clone", "-q", remote.to_str().unwrap(), b.to_str().unwrap()]).status().unwrap().success());
        for kv in [["user.email", "t@t"], ["user.name", "Beto"], ["commit.gpgsign", "false"]] {
            git_run(&b, &["config", kv[0], kv[1]]).unwrap();
        }
        commit_file(&b, "g.txt", "from b\n", "third");
        git_run(&b, &["push", "-q"]).unwrap();
        assert_eq!(info(&a).behind, 0);
        assert_eq!(wait(spawn(&a, Op::Fetch, noop())).unwrap(), "Fetch concluído");
        assert_eq!(info(&a), SyncInfo { upstream: Some("origin/main".into()), ahead: 0, behind: 1 });
        assert_eq!(wait(spawn(&a, Op::Pull, noop())).unwrap(), "Pull concluído");
        assert_eq!(info(&a).behind, 0);
        assert!(a.join("g.txt").exists());
        assert_eq!(wait(spawn(&a, Op::Pull, noop())).unwrap(), "Já está atualizado");

        // Diverge: both sides commit; push is rejected, a fast-forward-only pull refuses.
        commit_file(&a, "a.txt", "a\n", "local");
        commit_file(&b, "b.txt", "b\n", "remote");
        git_run(&b, &["push", "-q"]).unwrap();
        let e = wait(spawn(&a, Op::Push, noop())).unwrap_err();
        assert_eq!(e.kind, FailKind::Rejected, "{e:?}");
        let e = wait(spawn(&a, Op::Pull, noop())).unwrap_err();
        assert_eq!(e.kind, FailKind::Diverged, "{e:?}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn amend_rewrites_the_last_commit() {
        let d = tmp("amend");
        init(&d, false);
        commit_file(&d, "f.txt", "one\n", "typo mesage");
        std::fs::write(d.join("g.txt"), "x\n").unwrap();
        git_run(&d, &["add", "g.txt"]).unwrap();
        commit_amend(&d, "fixed message").unwrap();
        assert_eq!(last_message(&d).as_deref(), Some("fixed message"));
        assert_eq!(git_run(&d, &["rev-list", "--count", "HEAD"]).unwrap().trim(), "1", "amend, not a new commit");
        assert_eq!(git_run(&d, &["show", "--name-only", "--format=", "HEAD"]).unwrap().lines().count(), 2, "staged file joined the commit");
        // Message-only amend with nothing staged.
        commit_amend(&d, "reworded").unwrap();
        assert_eq!(last_message(&d).as_deref(), Some("reworded"));
        assert!(commit_amend(&d, "  ").is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn cancel_stops_a_running_command() {
        let d = tmp("cancel");
        init(&d, false);
        // A remote that hangs: its upload-pack command sleeps.
        git_run(&d, &["config", "remote.origin.url", "/nonexistent"]).unwrap();
        git_run(&d, &["config", "remote.origin.uploadpack", "sleep 30; git-upload-pack"]).unwrap();
        git_run(&d, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]).unwrap();
        let mut job = spawn(&d, Op::Fetch, noop());
        std::thread::sleep(Duration::from_millis(200));
        job.cancel();
        let r = wait(job);
        assert!(r.is_err());
        std::fs::remove_dir_all(&d).ok();
    }
}
