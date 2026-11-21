//! Runs an agent CLI in its structured mode, with pipes instead of a terminal, and feeds what it
//! prints to a [`Chat`]. Claude Code is one persistent process (user messages as JSON lines on
//! its stdin); Codex and Gemini start one process per turn and continue the conversation by
//! thread / session id. Reading happens on helper threads that wake the UI through a channel, as
//! the terminal does: nothing here blocks the interface.

use super::{
    chat::{Chat, Event},
    proto_claude, proto_codex, proto_gemini,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How long an interrupt may take before the process is stopped outright.
const INTERRUPT_GRACE: Duration = Duration::from_millis(2500);
/// Lines of stderr kept to explain a process that died.
const ERR_KEEP: usize = 12;

/// The CLIs that have a structured mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Claude,
    Codex,
    Gemini,
}

impl Kind {
    /// The structured adapter for an agent id (`None`: it runs in a terminal).
    pub fn of(agent_id: &str) -> Option<Kind> {
        match agent_id {
            "claude" => Some(Kind::Claude),
            "codex" => Some(Kind::Codex),
            "gemini" => Some(Kind::Gemini),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Claude => "Claude Code",
            Kind::Codex => "Codex",
            Kind::Gemini => "Gemini CLI",
        }
    }

    /// What to do when the CLI says it is not signed in.
    pub fn login_hint(self) -> &'static str {
        match self {
            Kind::Claude => "run `claude` once in a terminal and use /login",
            Kind::Codex => "run `codex login` in a terminal",
            Kind::Gemini => "run `gemini` once in a terminal and sign in",
        }
    }
}

/// An error text with the sign-in hint added when it looks like a missing login.
pub fn with_login_hint(kind: Kind, text: &str) -> String {
    let t = text.to_lowercase();
    let login = ["login", "log in", "logged in", "authenticat", "api key", "api_key", "unauthorized", "sign in"];
    if login.iter().any(|k| t.contains(k)) && !t.contains("sign in first") {
        format!("{}\nSign in first: {}.", text.trim_end(), kind.login_hint())
    } else {
        text.to_string()
    }
}

/// What to show when the process ended in the middle of a turn.
pub fn failure_text(kind: Kind, stderr: &[String], code: Option<i32>) -> String {
    let tail: Vec<&str> = stderr.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let head = match code {
        Some(c) => format!("{} stopped (exit code {c})", kind.name()),
        None => format!("{} stopped", kind.name()),
    };
    let text = if tail.is_empty() { head } else { format!("{head}\n{}", tail.join("\n")) };
    with_login_hint(kind, &text)
}

/// How the process is started (resolved by the panel: executable, folder, environment).
#[derive(Clone)]
pub struct Spawn {
    pub program: String,
    /// Arguments that come before the CLI's own (a Windows `.cmd` shim runs through `cmd /c`).
    pub prefix: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, Option<String>)>,
}

pub enum Line {
    Out(String),
    Err(String),
    /// Stdout closed: the process ended.
    Exit,
}

/// A child process with pipes and reader threads.
pub struct Runner {
    child: Option<Child>,
    input: Option<Sender<Vec<u8>>>,
    rx: Receiver<Line>,
    pub pid: u32,
}

fn read_lines(r: impl Read, mut f: impl FnMut(String) -> bool) {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']).to_string();
                if !f(line) {
                    break;
                }
            }
        }
    }
}

impl Runner {
    /// Start `sp` with `args`; `pipe_stdin`: keep its stdin open for [`write`](Self::write).
    pub fn spawn(sp: &Spawn, args: &[String], pipe_stdin: bool, wake: Wake) -> std::io::Result<Runner> {
        let mut cmd = Command::new(&sp.program);
        cmd.args(&sp.prefix)
            .args(args)
            .current_dir(&sp.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if pipe_stdin { Stdio::piped() } else { Stdio::null() })
            .env("NO_COLOR", "1");
        for (k, v) in &sp.env {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own process group: stopping it also stops what it started.
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let (tx, rx) = channel::<Line>();
        if let Some(out) = child.stdout.take() {
            let (tx, wake) = (tx.clone(), wake.clone());
            thread::spawn(move || {
                read_lines(out, |l| {
                    let ok = tx.send(Line::Out(l)).is_ok();
                    wake();
                    ok
                });
                let _ = tx.send(Line::Exit);
                wake();
            });
        }
        if let Some(err) = child.stderr.take() {
            let wake = wake.clone();
            thread::spawn(move || {
                read_lines(err, |l| {
                    let ok = tx.send(Line::Err(l)).is_ok();
                    wake();
                    ok
                });
            });
        }
        let input = child.stdin.take().map(|mut w| {
            let (itx, irx) = channel::<Vec<u8>>();
            thread::spawn(move || {
                for bytes in irx {
                    if w.write_all(&bytes).is_err() || w.flush().is_err() {
                        break;
                    }
                }
            });
            itx
        });
        Ok(Runner { child: Some(child), input, rx, pid })
    }

    /// Queue bytes for its stdin (a thread writes them: a full pipe never blocks the UI). False
    /// when the process is gone.
    pub fn write(&self, text: &str) -> bool {
        self.input.as_ref().is_some_and(|tx| tx.send(text.as_bytes().to_vec()).is_ok())
    }

    /// Close its stdin (what is queued is still written first).
    pub fn close_input(&mut self) {
        self.input = None;
    }

    pub fn drain(&mut self) -> Vec<Line> {
        self.rx.try_iter().collect()
    }

    /// Exit code, when it already ended.
    pub fn exit_code(&mut self) -> Option<i32> {
        self.child.as_mut()?.try_wait().ok().flatten().and_then(|s| s.code())
    }

    fn signal_group(&self) {
        #[cfg(unix)]
        if self.pid > 0 {
            // SAFETY: plain libc call; the pid is our own child, the leader of its own group.
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGTERM);
            }
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.input = None;
        self.signal_group();
        if let Some(mut c) = self.child.take() {
            #[cfg(not(unix))]
            let _ = c.kill();
            // Reaped on a thread of its own: a process that is slow to end never stalls the UI.
            thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }
}

/// One agent's conversation process.
pub struct Backend {
    pub kind: Kind,
    sp: Spawn,
    runner: Option<Runner>,
    gem: proto_gemini::Parser,
    err_tail: Vec<String>,
    /// An interrupt was asked for at this moment and the turn is still running.
    pub interrupting: Option<Instant>,
    wake: Wake,
    next_req: u64,
}

impl Backend {
    pub fn new(kind: Kind, sp: Spawn, wake: Wake) -> Backend {
        Backend {
            kind,
            sp,
            runner: None,
            gem: proto_gemini::Parser::new(),
            err_tail: Vec::new(),
            interrupting: None,
            wake,
            next_req: 0,
        }
    }

    /// The process is up (always true between turns of the per-turn CLIs: nothing is needed).
    #[cfg(test)]
    pub fn ready(&self) -> bool {
        self.kind != Kind::Claude || self.runner.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.runner.as_ref().map(|r| r.pid)
    }

    fn start(&mut self, chat: &mut Chat, prompt: &str) -> Result<(), String> {
        self.err_tail.clear();
        self.gem = proto_gemini::Parser::new();
        let sid = chat.session_id.clone();
        let (args, pipe) = match self.kind {
            Kind::Claude => (proto_claude::args(sid.as_deref()), true),
            Kind::Codex => (proto_codex::args(sid.as_deref()), true),
            Kind::Gemini => (proto_gemini::args(prompt, sid.as_deref()), false),
        };
        chat.new_process();
        let r = Runner::spawn(&self.sp, &args, pipe, self.wake.clone())
            .map_err(|e| format!("Could not start {}: {e}", self.kind.name()))?;
        self.runner = Some(r);
        Ok(())
    }

    /// Send a user message: it joins the transcript and the CLI starts working on it.
    pub fn send(&mut self, chat: &mut Chat, text: &str) {
        chat.user_says(text);
        self.interrupting = None;
        if let Err(e) = self.deliver(chat, text) {
            chat.apply(Event::Fatal(e));
        }
    }

    fn deliver(&mut self, chat: &mut Chat, text: &str) -> Result<(), String> {
        match self.kind {
            Kind::Claude => {
                let line = format!("{}\n", proto_claude::user_message(text));
                if self.runner.as_ref().is_some_and(|r| r.write(&line)) {
                    return Ok(());
                }
                self.runner = None;
                self.start(chat, text)?;
                match &self.runner {
                    Some(r) if r.write(&line) => Ok(()),
                    _ => Err(format!("Could not talk to {}", self.kind.name())),
                }
            }
            Kind::Codex => {
                self.runner = None;
                self.start(chat, text)?;
                if let Some(r) = self.runner.as_mut() {
                    r.write(text);
                    r.close_input();
                }
                Ok(())
            }
            Kind::Gemini => {
                self.runner = None;
                self.start(chat, text)
            }
        }
    }

    fn parse(&mut self, line: &str) -> Vec<Event> {
        let evs = match self.kind {
            Kind::Claude => proto_claude::parse_line(line),
            Kind::Codex => proto_codex::parse_line(line),
            Kind::Gemini => self.gem.parse_line(line),
        };
        let kind = self.kind;
        evs.into_iter()
            .map(|e| match e {
                Event::Done { usage, cumulative, session, error: Some(t) } => {
                    Event::Done { usage, cumulative, session, error: Some(with_login_hint(kind, &t)) }
                }
                e => e,
            })
            .collect()
    }

    /// Fold what the process printed into the chat; true when something changed.
    pub fn pump(&mut self, chat: &mut Chat) -> bool {
        let lines = match self.runner.as_mut() {
            Some(r) => r.drain(),
            None => return false,
        };
        let changed = !lines.is_empty();
        let mut exited = false;
        for l in lines {
            match l {
                Line::Out(s) => {
                    for e in self.parse(&s) {
                        chat.apply(e);
                    }
                }
                Line::Err(s) => {
                    self.err_tail.push(s);
                    if self.err_tail.len() > ERR_KEEP {
                        self.err_tail.remove(0);
                    }
                }
                Line::Exit => exited = true,
            }
        }
        if !chat.working() {
            self.interrupting = None;
        }
        if exited {
            if chat.working()
                && let Some(r) = self.runner.as_mut()
            {
                // Its last words on stderr may still be on their way.
                while let Ok(l) = r.rx.recv_timeout(Duration::from_millis(60)) {
                    if let Line::Err(s) = l {
                        self.err_tail.push(s);
                    }
                }
            }
            let code = self.runner.as_mut().and_then(Runner::exit_code);
            self.runner = None;
            if chat.working() {
                chat.apply(Event::Fatal(failure_text(self.kind, &self.err_tail, code)));
            }
        }
        changed
    }

    /// The user answers a permission the agent asked for.
    pub fn answer(&mut self, chat: &mut Chat, request_id: &str, allow: bool) {
        let Some(p) = chat.resolve_perm(request_id, allow) else { return };
        if let Some(r) = &self.runner {
            r.write(&format!("{}\n", proto_claude::permission_reply(&p.request_id, allow, &p.input)));
        }
    }

    /// Stop the turn in progress. Claude Code is asked to stop (and is stopped for good when it does
    /// not within a moment, see [`tick`](Self::tick)); the per-turn CLIs are ended at once.
    pub fn interrupt(&mut self, chat: &mut Chat) {
        if !chat.working() {
            return;
        }
        match (self.kind, self.runner.as_ref(), self.interrupting) {
            (Kind::Claude, Some(r), None) => {
                self.next_req += 1;
                r.write(&format!("{}\n", proto_claude::interrupt_request(&format!("lumen-int-{}", self.next_req))));
                self.interrupting = Some(Instant::now());
            }
            _ => {
                self.runner = None;
                self.interrupting = None;
                chat.interrupted();
            }
        }
    }

    /// Time-based work: an interrupt that was not obeyed. Returns when to look again.
    pub fn tick(&mut self, chat: &mut Chat, now: Instant) -> Option<Instant> {
        let at = self.interrupting?;
        if !chat.working() {
            self.interrupting = None;
            return None;
        }
        if now.saturating_duration_since(at) >= INTERRUPT_GRACE {
            self.runner = None;
            self.interrupting = None;
            chat.interrupted();
            return None;
        }
        Some(at + INTERRUPT_GRACE)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::chat::{Block, Phase};
    use super::*;

    fn stub(script: &str) -> Spawn {
        Spawn {
            program: "/bin/sh".into(),
            prefix: vec!["-c".into(), script.into(), "stub".into()],
            cwd: std::env::temp_dir(),
            env: vec![],
        }
    }

    fn wait(be: &mut Backend, chat: &mut Chat, mut done: impl FnMut(&Chat) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end && !done(chat) {
            be.pump(chat);
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn failure_texts_add_the_login_hint_only_when_it_applies() {
        let t = failure_text(Kind::Claude, &["Error: not logged in".into()], Some(1));
        assert!(t.starts_with("Claude Code stopped (exit code 1)\nError: not logged in"), "{t}");
        assert!(t.contains("Sign in first: run `claude`"), "{t}");
        let t = failure_text(Kind::Codex, &[], None);
        assert_eq!(t, "Codex stopped");
        assert_eq!(with_login_hint(Kind::Codex, "disk full"), "disk full");
        assert!(with_login_hint(Kind::Gemini, "401 Unauthorized").contains("gemini"));
    }

    #[test]
    fn a_persistent_process_answers_each_message_on_its_pipe() {
        // Answers every input line with the events of a short turn.
        let script = r#"while read -r line; do
printf '%s\n' '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"pong"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"pong","total_cost_usd":0.01,"usage":{"output_tokens":3},"session_id":"S1","duration_ms":5}'
done"#;
        let mut chat = Chat::new();
        let mut be = Backend::new(Kind::Claude, stub(script), Arc::new(|| {}));
        be.send(&mut chat, "ping");
        assert_eq!(chat.phase, Phase::Working);
        wait(&mut be, &mut chat, |c| c.phase == Phase::Idle);
        assert_eq!(chat.phase, Phase::Idle, "{:?}", chat.messages);
        assert_eq!(chat.messages[1].blocks, vec![Block::Text("pong".into())]);
        assert_eq!(chat.session_id.as_deref(), Some("S1"));
        let pid = be.pid();
        be.send(&mut chat, "again");
        wait(&mut be, &mut chat, |c| c.messages.len() >= 4 && c.phase == Phase::Idle);
        assert_eq!(chat.messages.len(), 4, "the same process serves the second message");
        assert_eq!(be.pid(), pid);
        assert_eq!(chat.usage().turns, 2);
    }

    #[test]
    fn a_process_that_dies_mid_turn_ends_it_with_its_stderr() {
        let mut chat = Chat::new();
        let mut be =
            Backend::new(Kind::Claude, stub("read -r l; echo 'boom: not logged in' >&2; exit 3"), Arc::new(|| {}));
        be.send(&mut chat, "hi");
        wait(&mut be, &mut chat, |c| c.phase == Phase::Idle);
        assert_eq!(chat.phase, Phase::Idle);
        let Some(Block::Error(e)) = chat.messages.last().and_then(|m| m.blocks.last()) else {
            panic!("{:?}", chat.messages)
        };
        assert!(e.contains("boom: not logged in") && e.contains("Sign in first"), "{e}");
        assert!(!be.ready(), "the next message starts it again");
    }

    #[test]
    fn per_turn_clis_get_the_prompt_and_an_interrupt_ends_them() {
        let mut chat = Chat::new();
        // Codex-style: prompt on stdin, events on stdout, exits.
        let script = r#"read -r p; printf '%s\n' '{"type":"thread.started","thread_id":"T1"}' '{"type":"item.completed","item":{"id":"a","type":"agent_message","text":"got it"}}' '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}'"#;
        let mut be = Backend::new(Kind::Codex, stub(script), Arc::new(|| {}));
        be.send(&mut chat, "do it");
        wait(&mut be, &mut chat, |c| c.phase == Phase::Idle);
        assert_eq!(chat.messages[1].blocks, vec![Block::Text("got it".into())]);
        assert_eq!(chat.session_id.as_deref(), Some("T1"));
        // A long turn is cut by the interrupt.
        let mut be = Backend::new(Kind::Codex, stub("sleep 30"), Arc::new(|| {}));
        be.send(&mut chat, "slow");
        assert_eq!(chat.phase, Phase::Working);
        be.interrupt(&mut chat);
        assert_eq!(chat.phase, Phase::Idle);
        assert!(
            matches!(chat.messages.last().and_then(|m| m.blocks.last()), Some(Block::Error(e)) if e == "Interrupted")
        );
    }

    #[test]
    fn a_claude_interrupt_that_is_ignored_stops_the_process_after_the_grace() {
        let mut chat = Chat::new();
        let mut be = Backend::new(Kind::Claude, stub("while read -r l; do :; done; sleep 30"), Arc::new(|| {}));
        be.send(&mut chat, "work");
        be.interrupt(&mut chat);
        assert!(be.interrupting.is_some() && chat.working(), "asked politely first");
        assert!(be.tick(&mut chat, Instant::now()).is_some());
        assert!(be.tick(&mut chat, Instant::now() + INTERRUPT_GRACE + Duration::from_millis(1)).is_none());
        assert_eq!(chat.phase, Phase::Idle);
        assert!(!be.ready());
    }
}
