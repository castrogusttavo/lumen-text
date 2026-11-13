//! Interactive rebase without a terminal editor.
//!
//! The plan (which commits, in which order, with which action) is edited in the app; running it is
//! plain `git rebase -i` where `GIT_SEQUENCE_EDITOR` is `cp <generated todo>` (git "edits" the todo
//! by overwriting it with ours) and `GIT_EDITOR` is `true` (a squash keeps the joined messages).
//! A reworded commit is a `pick` followed by `exec git commit --amend -F <message file>`, so no
//! editor ever opens. The todo and the message files live in `<git dir>/lumen-rebase/` and stay
//! there while a rebase is stopped on conflicts (`--continue` needs the `exec` lines), until the
//! rebase ends or is aborted.
//!
//! Everything here is plain code over `git`, covered by tests against temporary repositories; the
//! job runs on `gitsync`'s worker thread and the pickers live in `app_gitpanel::rebase`.

use crate::app_gitpanel::{RepoState, git_dir, git_run, repo_state};
use std::path::{Path, PathBuf};

/// What a commit of the plan becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoAction {
    Pick,
    Reword,
    Squash,
    Fixup,
    Drop,
}

impl TodoAction {
    pub const ALL: [TodoAction; 5] =
        [TodoAction::Pick, TodoAction::Reword, TodoAction::Squash, TodoAction::Fixup, TodoAction::Drop];

    /// The word of the git todo list.
    pub fn word(self) -> &'static str {
        match self {
            TodoAction::Pick => "pick",
            TodoAction::Reword => "reword",
            TodoAction::Squash => "squash",
            TodoAction::Fixup => "fixup",
            TodoAction::Drop => "drop",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            TodoAction::Pick => "keep the commit as it is",
            TodoAction::Reword => "keep the commit, change its message",
            TodoAction::Squash => "join it to the commit above, keeping both messages",
            TodoAction::Fixup => "join it to the commit above, discarding its message",
            TodoAction::Drop => "remove the commit",
        }
    }
}

/// One commit of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub action: TodoAction,
    pub hash: String,
    pub subject: String,
    /// Message after the subject line.
    pub body: String,
    /// New subject line of a reworded commit.
    pub new_subject: Option<String>,
}

impl Step {
    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(7)]
    }

    /// The subject as the plan will leave it.
    pub fn shown_subject(&self) -> &str {
        self.new_subject.as_deref().unwrap_or(&self.subject)
    }

    /// Full message of a reworded commit.
    pub fn message(&self) -> String {
        let subject = self.shown_subject().trim();
        let body = self.body.trim();
        if body.is_empty() { format!("{subject}\n") } else { format!("{subject}\n\n{body}\n") }
    }
}

/// The commits to rebase (oldest first, like git's todo list) and what they go onto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebasePlan {
    /// `git rebase -i <upstream>`; None = `--root`.
    pub upstream: Option<String>,
    pub steps: Vec<Step>,
    /// Where the todo and the message files are written.
    pub dir: PathBuf,
}

/// `git log` format of one step: hash, subject and body, ended by a record separator.
pub const STEP_FORMAT: &str = "--format=%H%x1f%s%x1f%b%x1e";

/// Parse `git log STEP_FORMAT` output.
pub fn parse_steps(out: &str) -> Vec<Step> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches(['\n', '\r']);
            let mut p = rec.splitn(3, '\x1f');
            let hash = p.next()?.trim().to_string();
            if hash.is_empty() {
                return None;
            }
            Some(Step {
                action: TodoAction::Pick,
                hash,
                subject: p.next().unwrap_or("").trim().to_string(),
                body: p.next().unwrap_or("").trim().to_string(),
                new_subject: None,
            })
        })
        .collect()
}

/// Quote for `sh` (the sequence editor and the `exec` lines run through it, also on Windows).
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn sh_path(p: &Path) -> String {
    sh_quote(&p.to_string_lossy().replace('\\', "/"))
}

fn check_clean(top: &Path) -> Result<(), String> {
    match repo_state(top) {
        RepoState::Clean => Ok(()),
        st => Err(format!("Cannot rebase: {}", st.busy_text())),
    }
}

fn plan_dir(top: &Path) -> Result<PathBuf, String> {
    git_dir(top).map(|d| d.join("lumen-rebase")).ok_or_else(|| "Not a git repository".to_string())
}

/// The commits of `base..HEAD` (all of HEAD's history when `base` is None), merges left out (a
/// plain rebase flattens them anyway).
fn plan_range(top: &Path, base: Option<&str>, upstream: Option<String>) -> Result<RebasePlan, String> {
    check_clean(top)?;
    let range = match base {
        Some(b) => format!("{b}..HEAD"),
        None => "HEAD".to_string(),
    };
    let out = git_run(top, &["log", "--reverse", "--no-merges", STEP_FORMAT, &range])?;
    let steps = parse_steps(&out);
    if steps.is_empty() {
        return Err("There are no commits to rebase here".into());
    }
    Ok(RebasePlan { upstream, steps, dir: plan_dir(top)? })
}

/// The last `n` commits of the checked-out branch.
pub fn plan_last(top: &Path, n: usize) -> Result<RebasePlan, String> {
    let total = git_run(top, &["rev-list", "--count", "HEAD"])?.trim().parse::<usize>().unwrap_or(0);
    if n == 0 || total == 0 {
        return Err("There are no commits to rebase here".into());
    }
    if n >= total {
        return plan_range(top, None, None);
    }
    let base = git_run(top, &["rev-parse", "--verify", &format!("HEAD~{n}")])?.trim().to_string();
    plan_range(top, Some(&base), Some(base.clone()))
}

/// `hash` and every commit after it (the commit menu's "from here").
pub fn plan_from(top: &Path, hash: &str) -> Result<RebasePlan, String> {
    match git_run(top, &["rev-parse", "--verify", "-q", &format!("{hash}^")]) {
        Ok(p) => {
            let p = p.trim().to_string();
            plan_range(top, Some(&p), Some(p.clone()))
        }
        // A root commit has no parent: everything is rebased.
        Err(_) => plan_range(top, None, None),
    }
}

/// The commits of the checked-out branch that are not in `branch`, to be replayed onto it.
pub fn plan_onto(top: &Path, branch: &str) -> Result<RebasePlan, String> {
    if branch.starts_with('-')
        || git_run(top, &["rev-parse", "--verify", "-q", &format!("{branch}^{{commit}}")]).is_err()
    {
        return Err(format!("“{branch}” is not a branch or commit"));
    }
    plan_range(top, Some(branch), Some(branch.to_string()))
}

impl RebasePlan {
    /// Why the plan cannot run, if so.
    pub fn validate(&self) -> Result<(), String> {
        let first = self.steps.iter().find(|s| s.action != TodoAction::Drop);
        match first {
            None => Err("Every commit is dropped: nothing would be left".into()),
            Some(s) if matches!(s.action, TodoAction::Squash | TodoAction::Fixup) => Err(format!(
                "“{}” is set to {} but there is no commit above it to join (make it a pick, or move it down)",
                s.shown_subject(),
                s.action.word()
            )),
            Some(_) => Ok(()),
        }
    }

    pub fn todo_path(&self) -> PathBuf {
        self.dir.join("todo")
    }

    fn msg_path(&self, i: usize) -> PathBuf {
        self.dir.join(format!("msg-{i}"))
    }

    /// The todo list git is given.
    pub fn render_todo(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.steps.iter().enumerate() {
            let subject = s.subject.replace(['\n', '\r'], " ");
            let reworded = s.action == TodoAction::Reword && s.new_subject.is_some();
            let word = if s.action == TodoAction::Reword { "pick" } else { s.action.word() };
            out.push_str(&format!("{word} {} {subject}\n", s.hash));
            if reworded {
                out.push_str(&format!("exec git commit --amend --allow-empty -q -F {}\n", sh_path(&self.msg_path(i))));
            }
        }
        out
    }

    /// Write the todo and the message files.
    pub fn prepare(&self) -> Result<(), String> {
        self.validate()?;
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        std::fs::write(self.todo_path(), self.render_todo()).map_err(|e| e.to_string())?;
        for (i, s) in self.steps.iter().enumerate() {
            if s.action == TodoAction::Reword && s.new_subject.is_some() {
                std::fs::write(self.msg_path(i), s.message()).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// Arguments of `git` (after `-C <top>`).
    pub fn args(&self) -> Vec<String> {
        let mut v: Vec<String> = ["rebase", "-i", "--autostash"].iter().map(|s| s.to_string()).collect();
        match &self.upstream {
            Some(u) => v.push(u.clone()),
            None => v.push("--root".into()),
        }
        v
    }

    /// Environment that keeps every editor closed.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("GIT_SEQUENCE_EDITOR".into(), format!("cp {}", sh_path(&self.todo_path()))),
            ("GIT_EDITOR".into(), "true".into()),
        ]
    }
}

/// Environment of `git rebase --continue` / `--abort`: no editor for the squash messages.
pub fn resume_env() -> Vec<(String, String)> {
    vec![("GIT_EDITOR".into(), "true".into()), ("GIT_SEQUENCE_EDITOR".into(), "true".into())]
}

/// The rebase is over: forget the generated files.
pub fn cleanup(top: &Path) {
    if let Ok(d) = plan_dir(top) {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumen-rebase-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn git(d: &Path, args: &[&str]) -> String {
        let o = Command::new("git").arg("-C").arg(d).args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn init(tag: &str) -> PathBuf {
        let d = tmp(tag);
        git(&d, &["init", "-q", "-b", "main"]);
        git(&d, &["config", "user.email", "t@t"]);
        git(&d, &["config", "user.name", "Ana"]);
        git(&d, &["config", "commit.gpgsign", "false"]);
        d
    }

    fn commit(d: &Path, file: &str, text: &str, msg: &str) {
        std::fs::write(d.join(file), text).unwrap();
        git(d, &["add", "."]);
        git(d, &["commit", "-q", "-m", msg]);
    }

    fn subjects(d: &Path) -> Vec<String> {
        git(d, &["log", "--format=%s"]).lines().map(str::to_string).collect()
    }

    /// Run the plan the way the job does; true when git exited cleanly.
    fn run(d: &Path, args: Vec<String>, env: Vec<(String, String)>) -> bool {
        let mut c = Command::new("git");
        c.arg("-C").arg(d).args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap().status.success()
    }

    fn run_plan(d: &Path, plan: &RebasePlan) -> bool {
        plan.prepare().unwrap();
        run(d, plan.args(), plan.env())
    }

    #[test]
    fn steps_are_parsed_from_the_log() {
        let out = "aaa\x1fone\x1fbody line\n\nmore\x1e\nbbb\x1ftwo\x1f\x1e\n";
        let s = parse_steps(out);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].hash.as_str(), s[0].subject.as_str()), ("aaa", "one"));
        assert_eq!(s[0].body, "body line\n\nmore");
        assert_eq!((s[1].hash.as_str(), s[1].body.as_str()), ("bbb", ""));
        assert_eq!(s[0].short(), "aaa");
    }

    #[test]
    fn quoting_survives_spaces_and_quotes() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_path(Path::new("C:\\x y\\todo")), "'C:/x y/todo'");
    }

    #[test]
    fn todo_has_one_line_per_commit_and_an_exec_for_rewords() {
        let step = |a, h: &str, ns: Option<&str>| Step {
            action: a,
            hash: h.into(),
            subject: format!("s-{h}"),
            body: String::new(),
            new_subject: ns.map(str::to_string),
        };
        let p = RebasePlan {
            upstream: Some("base".into()),
            steps: vec![
                step(TodoAction::Pick, "a1", None),
                step(TodoAction::Reword, "b2", Some("better")),
                step(TodoAction::Reword, "c3", None),
                step(TodoAction::Squash, "d4", None),
                step(TodoAction::Fixup, "e5", None),
                step(TodoAction::Drop, "f6", None),
            ],
            dir: PathBuf::from("/x/lumen-rebase"),
        };
        let t = p.render_todo();
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "pick a1 s-a1");
        assert_eq!(lines[1], "pick b2 s-b2");
        assert_eq!(lines[2], "exec git commit --amend --allow-empty -q -F '/x/lumen-rebase/msg-1'");
        assert_eq!(lines[3], "pick c3 s-c3", "a reword without a new subject is a pick");
        assert_eq!(&lines[4..], ["squash d4 s-d4", "fixup e5 s-e5", "drop f6 s-f6"]);
        assert_eq!(p.args(), ["rebase", "-i", "--autostash", "base"]);
        let env = p.env();
        assert_eq!(env[0].0, "GIT_SEQUENCE_EDITOR");
        assert_eq!(env[0].1, "cp '/x/lumen-rebase/todo'");
        assert_eq!((env[1].0.as_str(), env[1].1.as_str()), ("GIT_EDITOR", "true"));
        let root = RebasePlan { upstream: None, ..p };
        assert_eq!(root.args().last().map(String::as_str), Some("--root"));
    }

    #[test]
    fn invalid_plans_are_refused() {
        let mk = |actions: &[TodoAction]| RebasePlan {
            upstream: None,
            steps: actions
                .iter()
                .enumerate()
                .map(|(i, a)| Step {
                    action: *a,
                    hash: format!("h{i}"),
                    subject: format!("s{i}"),
                    body: String::new(),
                    new_subject: None,
                })
                .collect(),
            dir: PathBuf::new(),
        };
        assert!(mk(&[TodoAction::Pick, TodoAction::Squash]).validate().is_ok());
        assert!(mk(&[TodoAction::Drop, TodoAction::Reword, TodoAction::Fixup]).validate().is_ok());
        assert!(mk(&[TodoAction::Drop, TodoAction::Drop]).validate().unwrap_err().contains("dropped"));
        let e = mk(&[TodoAction::Drop, TodoAction::Squash, TodoAction::Pick]).validate().unwrap_err();
        assert!(e.contains("s1") && e.contains("squash"), "{e}");
        assert!(mk(&[TodoAction::Fixup]).prepare().is_err());
    }

    #[test]
    fn message_keeps_the_body() {
        let mut s = Step {
            action: TodoAction::Reword,
            hash: "abcdef0123".into(),
            subject: "old".into(),
            body: "why\nand how".into(),
            new_subject: Some("new".into()),
        };
        assert_eq!(s.message(), "new\n\nwhy\nand how\n");
        s.body.clear();
        assert_eq!(s.message(), "new\n");
        assert_eq!(s.shown_subject(), "new");
    }

    #[test]
    fn reword_drop_squash_and_fixup_rewrite_history_without_an_editor() {
        let d = init("ops");
        commit(&d, "f1", "1", "c1");
        commit(&d, "f2", "2", "c2");
        commit(&d, "f3", "3", "c3");
        commit(&d, "f4", "4", "c4 extra words");
        commit(&d, "f5", "5", "c5");
        let mut p = plan_last(&d, 4).unwrap();
        assert_eq!(
            p.steps.iter().map(|s| s.subject.as_str()).collect::<Vec<_>>(),
            ["c2", "c3", "c4 extra words", "c5"]
        );
        p.steps[0].action = TodoAction::Reword;
        p.steps[0].new_subject = Some("second, renamed".into());
        p.steps[1].action = TodoAction::Drop;
        p.steps[2].action = TodoAction::Squash;
        p.steps[3].action = TodoAction::Fixup;
        assert!(run_plan(&d, &p), "{}", git(&d, &["status"]));
        assert_eq!(repo_state(&d), RepoState::Clean);
        let s = subjects(&d);
        assert_eq!(s, ["second, renamed", "c1"], "{s:?}");
        let msg = git(&d, &["log", "-1", "--format=%B"]);
        assert!(msg.contains("second, renamed") && msg.contains("c4 extra words"), "squash keeps both: {msg}");
        assert!(d.join("f2").exists() && d.join("f4").exists() && d.join("f5").exists());
        assert!(!d.join("f3").exists(), "the dropped commit's file is gone");
        assert_eq!(git(&d, &["log", "-1", "--format=%an"]).trim(), "Ana");
        cleanup(&d);
        assert!(!d.join(".git").join("lumen-rebase").exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn reorder_and_root_rebase() {
        let d = init("order");
        commit(&d, "f1", "1", "c1");
        commit(&d, "f2", "2", "c2");
        commit(&d, "f3", "3", "c3");
        // More than the history holds: the root commit is part of the plan.
        let mut p = plan_last(&d, 10).unwrap();
        assert_eq!(p.upstream, None);
        assert_eq!(p.steps.len(), 3);
        p.steps.swap(1, 2);
        assert!(run_plan(&d, &p), "{}", git(&d, &["status"]));
        assert_eq!(subjects(&d), ["c2", "c3", "c1"]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn from_a_commit_and_onto_a_branch() {
        let d = init("onto");
        commit(&d, "f1", "1", "base");
        git(&d, &["branch", "side"]);
        commit(&d, "f2", "2", "main-1");
        commit(&d, "f3", "3", "main-2");
        let hash = git(&d, &["rev-parse", "HEAD~1"]).trim().to_string();
        let p = plan_from(&d, &hash).unwrap();
        assert_eq!(p.steps.len(), 2, "the commit and the one after it");
        // Onto `side`, which has a commit of its own.
        git(&d, &["switch", "-q", "side"]);
        commit(&d, "g", "g", "side-1");
        git(&d, &["switch", "-q", "main"]);
        let mut p = plan_onto(&d, "side").unwrap();
        assert_eq!(p.steps.iter().map(|s| s.subject.as_str()).collect::<Vec<_>>(), ["main-1", "main-2"]);
        p.steps[1].action = TodoAction::Squash;
        assert!(run_plan(&d, &p), "{}", git(&d, &["status"]));
        assert_eq!(subjects(&d)[1..], ["side-1", "base"]);
        assert!(d.join("f2").exists() && d.join("f3").exists() && d.join("g").exists());
        assert!(plan_onto(&d, "nope").is_err());
        assert!(plan_onto(&d, "-x").is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn nothing_to_rebase_and_busy_repositories() {
        let d = init("busy");
        assert!(plan_last(&d, 3).is_err(), "no commits yet");
        commit(&d, "f", "1", "one");
        git(&d, &["branch", "same"]);
        assert!(plan_onto(&d, "same").unwrap_err().contains("no commits"));
        std::fs::create_dir_all(d.join(".git/rebase-merge")).unwrap();
        assert!(plan_last(&d, 1).unwrap_err().contains("rebase is in progress"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_conflict_stops_the_rebase_and_continue_or_abort_finishes_it() {
        let d = init("conflict");
        commit(&d, "x", "a1\n", "c1");
        commit(&d, "x", "a2\n", "c2");
        commit(&d, "x", "a3\n", "c3");
        let before = git(&d, &["rev-parse", "HEAD"]);
        let mut p = plan_last(&d, 2).unwrap();
        p.steps.swap(0, 1);
        p.prepare().unwrap();
        assert!(!run(&d, p.args(), p.env()), "the swapped commits conflict");
        assert_eq!(repo_state(&d), RepoState::Rebase);
        assert!(d.join(".git/lumen-rebase/todo").exists(), "the files stay while the rebase is stopped");
        // Resolve whatever stops, until it ends.
        for i in 0..4 {
            if repo_state(&d) != RepoState::Rebase {
                break;
            }
            std::fs::write(d.join("x"), format!("resolved{i}\n")).unwrap();
            git(&d, &["add", "x"]);
            run(&d, vec!["rebase".into(), "--continue".into()], resume_env());
        }
        assert_eq!(repo_state(&d), RepoState::Clean);
        assert_eq!(subjects(&d)[..2], ["c2", "c3"]);
        // Again, this time aborted: the branch is exactly where it was.
        git(&d, &["reset", "-q", "--hard", before.trim()]);
        let mut p = plan_last(&d, 2).unwrap();
        p.steps.swap(0, 1);
        p.prepare().unwrap();
        assert!(!run(&d, p.args(), p.env()));
        assert!(run(&d, vec!["rebase".into(), "--abort".into()], resume_env()));
        assert_eq!(repo_state(&d), RepoState::Clean);
        assert_eq!(git(&d, &["rev-parse", "HEAD"]), before);
        std::fs::remove_dir_all(&d).ok();
    }
}
