//! App glue of three Git features that need a worker thread and a popup:
//!
//! * the **blame popover**: a click on the inline blame text (or "Git: Show blame card") opens the
//!   commit's card (`gitblame`) as a hover popup; its `lumen:git:` links copy the hash, open a
//!   file's diff in that commit, or list the history of the line (`git log -L`);
//! * **pull request awareness** (`ghpr`, optional, silent without the `gh` CLI): the status chip of
//!   the checked-out branch's PR, gutter markers on the lines review comments were made on (click
//!   for the popover) and "Create pull request";
//! * the one-shot **worker results** ([`BgMsg`]) the rebase planner also uses.
//!
//! Everything slow (git, gh) runs on a thread started by [`App::git_bg`]; the UI thread only
//! collects the answers in [`App::git_ext_tick`].

use crate::{
    app::App,
    app_gitpanel::{GitAsk, git_run},
    app_overlay::{Action, Item},
    app_topbar::StatusBtn,
    ghpr::{self, PrState, Snapshot, Tone},
    gitblame::{self, GitLink},
    gitrebase::RebasePlan,
    gitx::Commit,
    ui::{Canvas, Rect},
};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{TryRecvError, channel},
    time::{Duration, Instant},
};

/// How often the pull request of the branch is asked for again.
pub const PR_POLL: Duration = Duration::from_secs(120);

/// An answer from a worker thread.
pub enum BgMsg {
    /// The blame card of `line` of document `doc`.
    Blame { doc: usize, line: usize, md: Result<String, String> },
    /// History of one line of a file.
    LineLog { path: PathBuf, line: usize, log: Vec<Commit> },
    /// A rebase plan read from the repository.
    Rebase(Result<Box<RebasePlan>, String>),
    /// The pull request of the branch.
    Pr(Box<Snapshot>),
    /// A pull request was created (its address) or could not be.
    PrCreated(Result<String, String>),
}

/// What is known about the pull request of the checked-out branch.
pub struct PrCache {
    pub snap: Option<Snapshot>,
    /// Bumped when `snap` changes: documents re-read their markers.
    pub rev: u64,
    pub next_poll: Instant,
    pub inflight: bool,
    /// `gh` is not installed: never ask again.
    pub gh_missing: bool,
    /// The branch `snap` is about.
    pub branch: Option<String>,
}

impl Default for PrCache {
    fn default() -> Self {
        // Not at once: opening the window costs nothing extra.
        PrCache {
            snap: None,
            rev: 0,
            next_poll: Instant::now() + Duration::from_secs(2),
            inflight: false,
            gh_missing: false,
            branch: None,
        }
    }
}

/// `path` relative to the repository root, `/` separated (empty when it is outside).
pub fn rel_path(top: &Path, path: &Path) -> String {
    let rel =
        path.strip_prefix(top).map(Path::to_path_buf).ok().or_else(|| {
            crate::paths::canonical(path).ok().and_then(|c| c.strip_prefix(top).map(Path::to_path_buf).ok())
        });
    rel.map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default()
}

impl App {
    /// Run `f` on a worker thread; its answer is handled by [`App::git_ext_tick`].
    pub(crate) fn git_bg<F>(&mut self, f: F)
    where
        F: FnOnce() -> BgMsg + Send + 'static,
    {
        let (tx, rx) = channel();
        let wake = self.wake();
        self.gitbg.bg.push(rx);
        std::thread::spawn(move || {
            let _ = tx.send(f());
            wake();
        });
    }

    /// Per-event hook (from `git_bg_tick`): collect worker answers, keep the pull request fresh,
    /// hand a rebase stopped on conflicts to the merge editor.
    pub fn git_ext_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        let mut msgs: Vec<BgMsg> = Vec::new();
        let mut finished: Vec<usize> = Vec::new();
        let mut lost = false;
        for (i, rx) in self.gitbg.bg.iter().enumerate() {
            match rx.try_recv() {
                Ok(m) => {
                    msgs.push(m);
                    finished.push(i);
                }
                Err(TryRecvError::Disconnected) => {
                    finished.push(i);
                    lost = true;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        for i in finished.into_iter().rev() {
            self.gitbg.bg.remove(i);
        }
        // A worker that died without answering must not block the polling for good.
        if lost && msgs.is_empty() {
            self.gitbg.pr.inflight = false;
        }
        if !self.gitbg.bg.is_empty() {
            want(now + Duration::from_millis(200));
        }
        for m in msgs {
            self.git_bg_msg(m, now);
        }
        self.pr_poll(now, want);
        self.pr_sync_marks();
        if std::mem::take(&mut self.gitbg.rebase_merge) {
            if self.gitpanel.is_none() {
                self.open_git_panel();
            }
            self.rebase_act(crate::app_gitpanel::RebaseAct::OpenMerge);
        }
    }

    fn git_bg_msg(&mut self, m: BgMsg, now: Instant) {
        match m {
            BgMsg::Blame { doc, line, md } => self.blame_card_show(doc, line, md),
            BgMsg::LineLog { path, line, log } => self.line_history_show(path, line, log),
            BgMsg::Rebase(Ok(plan)) => self.rebase_begin(*plan),
            BgMsg::Rebase(Err(e)) => self.git_report(e, false),
            BgMsg::Pr(snap) => self.pr_apply(*snap, now),
            BgMsg::PrCreated(Ok(url)) => {
                self.git_report(format!("Pull request created: {url}"), true);
                crate::paths::open_url(&url);
                self.gitbg.pr.next_poll = now;
            }
            BgMsg::PrCreated(Err(e)) => self.git_report(e, false),
        }
    }

    // ───────────── blame popover ─────────────

    /// Open the commit card of the cursor line (click on the inline blame, or the palette).
    pub fn blame_card_open(&mut self) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(Path::to_path_buf) else { return self.say("Save the file to see its blame") };
        if d.git_base.is_none() {
            return self.say("This file is not tracked by git");
        }
        if d.buf.is_dirty() {
            return self.say("Save the file first: the card describes the committed line");
        }
        let line = d.buf.line_col(d.ed.head()).0;
        let doc = self.active;
        self.git_bg(move || BgMsg::Blame { doc, line, md: gitblame::blame_markdown(&path, line) });
    }

    fn blame_card_show(&mut self, doc: usize, line: usize, md: Result<String, String>) {
        let text = match md {
            Ok(t) => t,
            Err(e) => return self.say(e),
        };
        // The cursor may have moved while git was working.
        let Some(d) = self.docs.get(doc) else { return };
        if doc != self.active || d.buf.line_col(d.ed.head()).0 != line {
            return;
        }
        // Anchored on the end of the line, where the inline blame starts: the pointer on the blame
        // text counts as being on the anchor, so the popup stays while it travels to it.
        let end = d.buf.pos(line, usize::MAX);
        self.hover_show_markdown(&text, (end, end));
    }

    /// A `lumen:git:` link of the blame card was clicked.
    pub(crate) fn git_link(&mut self, url: &str) {
        match gitblame::parse_link(url) {
            Some(GitLink::CopyHash(h)) => {
                self.clip.set(&h);
                self.say(format!("Copied {h}"));
            }
            Some(GitLink::Diff { hash, path }) => {
                self.hover = None;
                let subject = path
                    .parent()
                    .and_then(|dir| git_run(dir, &["log", "-1", "--format=%s", &hash]).ok())
                    .unwrap_or_default();
                let short = &hash[..hash.len().min(8)];
                self.open_diff_commit(&path, short, subject.trim());
            }
            Some(GitLink::History { path, line }) => {
                self.hover = None;
                self.say("Reading the history of the line…");
                self.git_bg(move || {
                    let log = gitblame::line_history(&path, line);
                    BgMsg::LineLog { path, line, log }
                });
            }
            None => {}
        }
        self.request_redraw();
    }

    fn line_history_show(&mut self, path: PathBuf, line: usize, log: Vec<Commit>) {
        if log.is_empty() {
            return self.say("No history found for that line");
        }
        let items: Vec<Item> = log
            .into_iter()
            .map(|c| Item {
                label: c.subject.clone(),
                detail: format!("{} · {} · {}", c.short, c.author, c.when),
                action: Action::GitShow(path.clone(), c.hash, c.subject),
            })
            .collect();
        self.open_locations(&format!("History of line {}", line + 1), items);
    }

    // ───────────── pull request ─────────────

    /// Ask for the branch's pull request now and then, and again when the branch changes.
    fn pr_poll(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if self.gitbg.pr.gh_missing || self.branch.is_none() {
            return;
        }
        if self.gitbg.pr.branch != self.branch {
            self.gitbg.pr.branch = self.branch.clone();
            self.gitbg.pr.next_poll = now;
            if self.gitbg.pr.snap.take().is_some() {
                self.gitbg.pr.rev += 1;
                self.request_redraw();
            }
        }
        if self.gitbg.pr.inflight || !self.gitbg.focused {
            return;
        }
        if self.gitbg.pr.next_poll <= now {
            self.pr_fetch();
        } else {
            want(self.gitbg.pr.next_poll);
        }
    }

    fn pr_fetch(&mut self) {
        let root = self.root.clone();
        self.gitbg.pr.inflight = true;
        self.git_bg(move || BgMsg::Pr(Box::new(ghpr::snapshot(&root))));
    }

    /// The worker's answer.
    pub(crate) fn pr_apply(&mut self, snap: Snapshot, now: Instant) {
        self.gitbg.pr.inflight = false;
        self.gitbg.pr.next_poll = now + PR_POLL;
        if snap.root != self.root {
            return;
        }
        // The branch changed while the question was out: ask again for the new one.
        if self.gitbg.pr.branch.as_ref().is_some_and(|b| *b != snap.branch) {
            self.gitbg.pr.next_poll = now;
            return;
        }
        if snap.gh_missing {
            self.gitbg.pr.gh_missing = true;
        }
        let snap = Some(snap);
        if self.gitbg.pr.snap != snap {
            self.gitbg.pr.snap = snap;
            self.gitbg.pr.rev += 1;
            self.request_redraw();
        }
    }

    /// Refresh the active document's comment markers when the pull request data changed.
    pub fn pr_sync_marks(&mut self) {
        let rev = self.gitbg.pr.rev;
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if d.pr_rev == rev {
            return;
        }
        d.pr_rev = rev;
        d.pr_lines = match (&self.gitbg.pr.snap, d.path()) {
            (Some(s), Some(p)) => ghpr::marker_lines(&s.comments, &rel_path(&s.top, p)),
            _ => Vec::new(),
        };
        self.request_redraw();
    }

    /// Markdown of the review comments on the 0-based `line` of the active document, with the
    /// char index the popup is anchored on.
    pub fn pr_comment_card(&self, line: usize) -> Option<(String, usize)> {
        let d = self.docs.get(self.active)?;
        let snap = self.gitbg.pr.snap.as_ref()?;
        let rel = rel_path(&snap.top, d.path()?);
        let list = ghpr::comments_on(&snap.comments, &rel, line);
        if list.is_empty() {
            return None;
        }
        Some((ghpr::comments_markdown(&list), d.buf.pos(line, usize::MAX)))
    }

    /// A click in the gutter column of the comment markers: show the popover. True when it hit one.
    pub fn pr_marker_click(&mut self, x: f64, y: f64) -> bool {
        let Some(d) = self.docs.get(self.active) else { return false };
        if d.pr_lines.is_empty() {
            return false;
        }
        let gw0 = d.gutter_w() - crate::docview::FOLD_W;
        let (x0, x1) = ((d.rect.x + gw0 - 16) as f64, (d.rect.x + gw0 - 1) as f64);
        if x < x0 || x >= x1 || y < d.rect.y as f64 || y >= d.rect.bottom() as f64 {
            return false;
        }
        let (p, _) = d.hit_at((d.rect.x + d.gutter_w() + 1) as f64, y);
        let line = d.buf.line_col(p).0;
        if !d.pr_lines.contains(&line) {
            return false;
        }
        let Some((md, at)) = self.pr_comment_card(line) else { return false };
        self.hover_show_markdown(&md, (at, at));
        true
    }

    /// The PR chip of the status bar (left group); returns the next x.
    pub fn draw_pr_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        let Some(info) = self.gitbg.pr.snap.as_ref().and_then(|s| s.info.clone()) else { return x };
        let r = self.layout.status;
        let c = self.theme.chrome;
        let label = info.chip();
        let lw = self.tx.ui_width(&label, false);
        let col = match info.tone() {
            Tone::Good => c.git_added,
            Tone::Bad => c.error,
            Tone::Merged => c.info,
            Tone::Neutral => c.tab_inactive_fg,
        };
        let rect = Rect::new(x, r.y + 3, 8 + 6 + 6 + lw + 8, r.h - 6);
        if self.overlay.is_none() && rect.contains(self.mouse.0, self.mouse.1) {
            cv.fill_round(rect, 6, c.list_hover_bg);
        }
        cv.fill_round(Rect::new(rect.x + 8, r.y + r.h / 2 - 3, 6, 6), 3, col);
        self.tx.draw_ui(cv, rect.x + 8 + 6 + 6, r.y + (r.h - 18) / 2, &label, c.tab_inactive_fg, r, false);
        self.layout.status_btns.push((rect, StatusBtn::Pr));
        rect.right() + 2
    }

    /// Tooltip of the chip.
    pub fn pr_tip(&self) -> Option<String> {
        self.gitbg.pr.snap.as_ref().and_then(|s| s.info.as_ref()).map(|i| i.tip())
    }

    /// Open the branch's pull request in the browser (chip click, palette).
    pub fn pr_open(&mut self) {
        match self.gitbg.pr.snap.as_ref().and_then(|s| s.info.as_ref()).map(|i| i.url.clone()) {
            Some(url) if !url.is_empty() => crate::paths::open_url(&url),
            _ => self.say("No pull request for this branch (or the GitHub CLI is not set up)"),
        }
    }

    /// "GitHub: Create pull request…": ask for the title, then create it on a worker.
    pub fn pr_create_prompt(&mut self, draft: bool) {
        let Some(top) = self.git_top() else { return };
        if self.gitbg.pr.gh_missing {
            return self.git_report("The GitHub CLI (gh) is not installed: https://cli.github.com", false);
        }
        if let Some(i) = self.gitbg.pr.snap.as_ref().and_then(|s| s.info.as_ref())
            && matches!(i.state, PrState::Open | PrState::Draft)
        {
            let n = i.number;
            return self.git_report(format!("Pull request #{n} already exists for this branch"), false);
        }
        let subject = git_run(&top, &["log", "-1", "--format=%s"]).unwrap_or_default();
        let title = if draft {
            "Draft pull request title (the branch is pushed if needed)"
        } else {
            "Pull request title (the branch is pushed if needed)"
        };
        self.git_prompt(GitAsk::PrTitle { draft }, title, subject.trim());
    }

    /// The title was typed: create the pull request (pushing the branch first when needed).
    pub(crate) fn pr_create(&mut self, title: String, draft: bool) {
        if title.trim().is_empty() {
            return self.git_report("A pull request needs a title", false);
        }
        let root = self.root.clone();
        self.say("Creating the pull request…");
        self.git_bg(move || BgMsg::PrCreated(ghpr::create_pr(&root, &title, draft)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app_overlay::Overlay,
        app_unsaved::testkit::{lock, test_app, tmp},
        ghpr::{PrInfo, Review, ReviewComment},
    };
    use std::process::Command;

    fn g(d: &Path, args: &[&str]) {
        Command::new("git").arg("-C").arg(d).args(args).output().unwrap();
    }

    fn wait_bg(app: &mut App) {
        for _ in 0..250 {
            app.git_ext_tick(Instant::now(), &mut |_| {});
            if app.gitbg.bg.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the worker never answered");
    }

    fn comment(path: &str, line: Option<usize>, id: u64) -> ReviewComment {
        ReviewComment {
            id,
            path: path.into(),
            line,
            author: "bo".into(),
            body: "please rename".into(),
            url: "https://github.com/o/r/pull/1#discussion_r1".into(),
            date: "2026-11-10".into(),
            is_reply: false,
        }
    }

    #[test]
    fn rel_paths_are_repository_relative() {
        assert_eq!(rel_path(Path::new("/r"), Path::new("/r/src/a.ts")), "src/a.ts");
        assert_eq!(rel_path(Path::new("/r"), Path::new("/elsewhere/a.ts")), "");
    }

    #[test]
    fn the_pr_answer_feeds_the_chip_and_the_gutter_markers() {
        let _g = lock();
        let d = tmp("pr-app");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.ts"), "one\ntwo\nthree\nfour\n").unwrap();
        let mut app = test_app(&d);
        let i = app.open_path(&d.join("src/a.ts"), None).unwrap();
        app.active = i;
        let info = PrInfo {
            number: 7,
            title: "Add thing".into(),
            state: PrState::Open,
            url: "https://github.com/o/r/pull/7".into(),
            review: Review::None,
            checks: Default::default(),
            head: "feat".into(),
        };
        let snap = Snapshot {
            root: app.root.clone(),
            top: app.root.clone(),
            branch: "feat".into(),
            gh_missing: false,
            info: Some(info),
            comments: vec![
                comment("src/a.ts", Some(3), 1),
                comment("src/a.ts", None, 2),
                comment("src/b.ts", Some(1), 3),
            ],
        };
        app.gitbg.pr.branch = Some("feat".into());
        let now = Instant::now();
        // An answer for another branch is dropped and asks again at once.
        app.pr_apply(Snapshot { branch: "old".into(), ..snap.clone() }, now);
        assert!(app.gitbg.pr.snap.is_none() && app.gitbg.pr.next_poll <= now);
        app.pr_apply(snap.clone(), now);
        assert_eq!(app.gitbg.pr.snap.as_ref().and_then(|s| s.info.as_ref()).map(|i| i.number), Some(7));
        assert_eq!(app.gitbg.pr.next_poll, now + PR_POLL);
        let rev = app.gitbg.pr.rev;
        app.pr_apply(snap, now);
        assert_eq!(app.gitbg.pr.rev, rev, "the same answer changes nothing");
        assert!(app.pr_tip().unwrap().contains("Add thing"));

        app.pr_sync_marks();
        assert_eq!(app.docs[i].pr_lines, [2], "line 3 (1-based) of src/a.ts; the outdated comment has none");
        let (md, at) = app.pr_comment_card(2).unwrap();
        assert!(md.contains("**bo**") && md.contains("please rename"), "{md}");
        assert_eq!(app.docs[i].buf.line_col(at).0, 2, "anchored on the commented line");
        assert!(app.pr_comment_card(0).is_none());

        // The branch changes: the old data goes away until the new answer arrives.
        app.branch = Some("other".into());
        app.pr_poll(now, &mut |_| {});
        assert!(app.gitbg.pr.snap.is_none());
        app.pr_sync_marks();
        assert!(app.docs[i].pr_lines.is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_missing_gh_stops_the_polling() {
        let _g = lock();
        let d = tmp("pr-nogh");
        let mut app = test_app(&d);
        app.branch = Some("main".into());
        app.gitbg.pr.next_poll = Instant::now();
        let snap = Snapshot {
            root: app.root.clone(),
            top: app.root.clone(),
            branch: "main".into(),
            gh_missing: true,
            info: None,
            comments: Vec::new(),
        };
        app.gitbg.pr.branch = Some("main".into());
        app.pr_apply(snap, Instant::now());
        assert!(app.gitbg.pr.gh_missing);
        app.gitbg.pr.next_poll = Instant::now();
        app.pr_poll(Instant::now(), &mut |_| {});
        assert!(!app.gitbg.pr.inflight, "gh is not asked for again");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn card_links_copy_open_diffs_and_list_line_history() {
        let _g = lock();
        let d = tmp("blame-app");
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "Ana"],
            &["config", "commit.gpgsign", "false"],
        ] {
            g(&d, a);
        }
        std::fs::write(d.join(".gitignore"), "seed.txt\n.config/\n.state/\n").unwrap();
        std::fs::write(d.join("f.ts"), "one\ntwo\n").unwrap();
        g(&d, &["add", "."]);
        g(&d, &["commit", "-q", "-m", "first"]);
        std::fs::write(d.join("f.ts"), "one\nTWO\n").unwrap();
        g(&d, &["commit", "-q", "-am", "second"]);
        let hash = gitblame::blame_hash(&d.join("f.ts"), 1).unwrap();

        let mut app = test_app(&d);
        app.git_link(&gitblame::link_copy(&hash));
        assert_eq!(app.clip.get(), hash);

        app.git_link(&gitblame::link_diff(&hash, &d.join("f.ts")));
        assert!(app.diff.is_some(), "the diff of that file in that commit opens");

        app.git_link(&gitblame::link_history(&d.join("f.ts"), 1));
        wait_bg(&mut app);
        match &app.overlay {
            Overlay::Picker(p) => assert!(p.title.contains("History of line 2"), "{}", p.title),
            _ => panic!("the line history is listed"),
        }
        std::fs::remove_dir_all(&d).ok();
    }
}
