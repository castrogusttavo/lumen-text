//! Keeping the explorer, open buffers and git status up to date when inotify alone is not enough.
//!
//! * **`.git` watching.** The tree walk never watches `.git`, so a `git checkout` / `commit` /
//!   `pull` typed in a terminal used to leave the branch, the gutter marks and the status badges
//!   stale. The repository's git dir (and its common dir and `refs/heads`) are watched on purpose;
//!   their events are kept away from the explorer and the language servers, debounced
//!   ([`GIT_SETTLE`]) and compared with a cheap [`GitSig`] (the bytes of `HEAD`, the commit it points
//!   to, the size and mtime of `index`). Only a real change refreshes: `HEAD` moved -> branch,
//!   gutter base and status; `index` changed -> status. `git status` runs with
//!   `--no-optional-locks`, so the refresh itself never touches `index` and cannot loop.
//! * **Polling fallback.** Folders on NFS, SMB, FUSE, 9p, FAT/exFAT... (see [`crate::fstype`]) or
//!   that inotify refused (watch limit) are polled by [`crate::poller::Poller`]: the open files and
//!   the expanded folders only, every `watcher_poll_ms`. The status bar says "Polling (NFS)".
//! * **Refresh on demand and on focus.** "File: Refresh explorer and git status" (palette, and the
//!   button in the explorer header) re-reads everything. Getting the focus back checks the open
//!   files' mtimes, the git signature and the expanded folders' mtimes, and refreshes what moved.
//! * **No lost refreshes.** A burst of events right after a refresh used to be dropped by the
//!   400 ms throttle; it is now deferred to the end of the window instead.

use crate::{
    app::{App, SidebarMode, TAB_H},
    fstype::Untrusted,
    poller::Poller,
    ui::{Canvas, Rect},
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// Git writes `index.lock`, `index`, `HEAD`, refs and logs in a burst: wait this long after the
/// last `.git` event before comparing.
pub const GIT_SETTLE: Duration = Duration::from_millis(120);
/// The explorer is rebuilt at most this often (a burst of events is folded into one rebuild).
pub const TREE_EVERY: Duration = Duration::from_millis(400);

/// What a cheap look at the git dir says about the repository state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitSig {
    /// The bytes of `HEAD` ("ref: refs/heads/main" or a detached commit id).
    pub head: String,
    /// The commit `HEAD` resolves to (loose ref or `packed-refs`).
    pub oid: String,
    /// Size and mtime of `index`.
    pub index: Option<(u64, Option<SystemTime>)>,
}

impl GitSig {
    pub fn head_moved(&self, other: &GitSig) -> bool {
        self.head != other.head || self.oid != other.oid
    }
}

/// The ref `HEAD` points to ("refs/heads/main"), if it is not detached.
fn head_ref(head: &str) -> Option<&str> {
    head.trim().strip_prefix("ref:").map(str::trim)
}

/// Read the repository state from the files alone (no `git` process).
pub fn git_sig(gitdir: &Path, common: &Path) -> GitSig {
    let head = std::fs::read_to_string(gitdir.join("HEAD")).unwrap_or_default();
    let oid = match head_ref(&head) {
        Some(r) => std::fs::read_to_string(gitdir.join(r))
            .or_else(|_| std::fs::read_to_string(common.join(r)))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| {
                // Packed: "<oid> <ref>" lines.
                std::fs::read_to_string(common.join("packed-refs"))
                    .unwrap_or_default()
                    .lines()
                    .find_map(|l| l.strip_suffix(r).and_then(|x| x.strip_suffix(' ')).map(str::to_string))
                    .unwrap_or_default()
            }),
        None => head.trim().to_string(),
    };
    let index = std::fs::metadata(gitdir.join("index")).ok().map(|m| (m.len(), m.modified().ok()));
    GitSig { head: head.trim().to_string(), oid, index }
}

/// Is `path` a git event worth looking at? Lock files come and go around every write; the real
/// change follows as a rename onto the final name.
pub fn git_event_matters(path: &Path) -> bool {
    !path.extension().is_some_and(|e| e == "lock")
}

/// Folders of the git dir to watch: the git dir, the common dir (linked worktrees), `refs/heads`
/// and the folder of the current branch's ref when the branch name has slashes.
pub fn git_watch_dirs(gitdir: &Path, common: &Path, head: &str) -> Vec<PathBuf> {
    let mut v = vec![gitdir.to_path_buf()];
    if common != gitdir {
        v.push(common.to_path_buf());
    }
    v.push(common.join("refs").join("heads"));
    if let Some(parent) = head_ref(head).map(|r| common.join(r)).and_then(|p| p.parent().map(Path::to_path_buf))
        && !v.contains(&parent)
    {
        v.push(parent);
    }
    v
}

/// Hash of the mtimes of `dirs`: a folder's mtime moves when an entry is added, removed or renamed.
pub fn dirs_sig<'a>(dirs: impl Iterator<Item = &'a Path>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for d in dirs {
        d.hash(&mut h);
        std::fs::metadata(d).ok().and_then(|m| m.modified().ok()).hash(&mut h);
    }
    h.finish()
}

/// Which explorer item the pointer is on (tooltips).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsTip {
    Refresh,
    Polling,
}

/// State of the watching extras (`App::fswatch`).
#[derive(Default)]
pub struct FsWatch {
    pub poller: Option<Poller>,
    /// Why the first polled folder is polled (the status bar hint), when anything is.
    pub reason: Option<Untrusted>,
    /// Polled folders and files, by reason (tooltip).
    pub polled: Vec<(PathBuf, Untrusted)>,
    /// `statfs` answers per folder (one syscall per folder per project).
    fs_cache: HashMap<PathBuf, Option<Untrusted>>,
    /// Tests and `LUMEN_FORCE_POLL`: treat every folder as this kind of file system.
    pub force: Option<Untrusted>,
    /// The root the git dirs below were found for.
    git_for: Option<PathBuf>,
    /// (git dir, common dir) of the project, when it is a repository.
    pub git: Option<(PathBuf, PathBuf)>,
    pub git_sig: GitSig,
    git_due: Option<Instant>,
    tree_due: Option<Instant>,
    /// `dirs_sig` of the expanded folders when the window lost the focus.
    blur_tree: Option<u64>,
    pub refresh_btn: Option<Rect>,
    pub poll_hint: Option<Rect>,
    /// Refreshes caused by a change under `.git`, and the ones where `HEAD` moved (tests).
    pub git_refreshes: usize,
    pub head_refreshes: usize,
}

impl App {
    fn fs_reason(&mut self, dir: &Path) -> Option<Untrusted> {
        if let Some(f) = self.fswatch.force {
            return Some(f);
        }
        if let Some(r) = self.fswatch.fs_cache.get(dir) {
            return *r;
        }
        let r = crate::fstype::untrusted(dir);
        self.fswatch.fs_cache.insert(dir.to_path_buf(), r);
        r
    }

    /// Is `path` inside the repository's git dir (or common dir)?
    pub fn is_git_path(&self, path: &Path) -> bool {
        self.fswatch.git.as_ref().is_some_and(|(g, c)| path.starts_with(g) || path.starts_with(c))
    }

    /// End of `watch_dirs`: watch the git dir and decide what has to be polled. `refused` are the
    /// folders inotify could not watch (watch limit).
    pub fn fswatch_sync(&mut self, refused: Vec<PathBuf>) {
        // 1. The repository: found once per project (one `git rev-parse`).
        if self.fswatch.git_for.as_ref() != Some(&self.root) {
            self.fswatch.git_for = Some(self.root.clone());
            self.fswatch.fs_cache.clear();
            self.fswatch.git = crate::git::git_dirs(&self.root);
            self.fswatch.git_sig = match &self.fswatch.git {
                Some((g, c)) => git_sig(g, c),
                None => GitSig::default(),
            };
        }
        let git_dirs = match &self.fswatch.git {
            Some((g, c)) => git_watch_dirs(g, c, &self.fswatch.git_sig.head),
            None => Vec::new(),
        };
        let mut refused = refused;
        if let Some(w) = self.watcher.as_mut() {
            for d in &git_dirs {
                if let Err(e) = w.watch_dir_always(d)
                    && crate::watcher::is_limit_error(&e)
                {
                    refused.push(d.clone());
                }
            }
        }
        // 2. What inotify cannot be trusted with: open files and expanded folders, nothing more.
        let mut dirs: Vec<PathBuf> = vec![self.root.clone()];
        dirs.extend(self.tree.rows.iter().filter(|r| r.is_dir && r.expanded).map(|r| r.path.clone()));
        dirs.extend(git_dirs);
        let files: Vec<PathBuf> = self.docs.iter().filter_map(|d| d.path().map(Path::to_path_buf)).collect();
        let mut polled: Vec<(PathBuf, Untrusted)> = Vec::new();
        for d in dirs {
            let why = if refused.contains(&d) { Some(Untrusted::WatchLimit) } else { self.fs_reason(&d) };
            if let Some(why) = why
                && !polled.iter().any(|(p, _)| *p == d)
            {
                polled.push((d, why));
            }
        }
        // An open file whose folder is polled anyway is covered by the folder's listing.
        let mut poll_files: Vec<PathBuf> = Vec::new();
        for f in files {
            let Some(parent) = f.parent().map(Path::to_path_buf) else { continue };
            if polled.iter().any(|(p, _)| *p == parent) {
                continue;
            }
            let why = if refused.contains(&parent) { Some(Untrusted::WatchLimit) } else { self.fs_reason(&parent) };
            if let Some(why) = why {
                poll_files.push(f.clone());
                polled.push((f, why));
            }
        }
        // The root's reason names the hint; else the first one found.
        self.fswatch.reason = polled.iter().find(|(p, _)| *p == self.root).or(polled.first()).map(|(_, w)| *w);
        let poll_dirs: Vec<PathBuf> =
            polled.iter().filter(|(p, _)| !poll_files.contains(p)).map(|(p, _)| p.clone()).collect();
        self.fswatch.polled = polled;
        if poll_dirs.is_empty() && poll_files.is_empty() {
            self.fswatch.poller = None;
            return;
        }
        if self.fswatch.poller.is_none() {
            let every = Duration::from_millis(self.config.watcher_poll_ms.max(crate::poller::MIN_MS));
            self.fswatch.poller = Poller::new(every, self.wake()).ok();
        }
        if let Some(p) = &self.fswatch.poller {
            p.set_targets(&poll_dirs, &poll_files);
        }
    }

    /// Start of `pump_watcher`: add the poller's events to `evs`, then take the `.git` ones out of it
    /// (they schedule a git check instead of a tree rebuild).
    pub fn fswatch_drain(&mut self, evs: &mut Vec<crate::watcher::FsEvent>) {
        if let Some(p) = &self.fswatch.poller {
            while let Ok(e) = p.rx.try_recv() {
                evs.push(e);
            }
        }
        if self.fswatch.git.is_none() {
            return;
        }
        let mut git = false;
        evs.retain(|e| {
            if self.is_git_path(&e.path) {
                git |= git_event_matters(&e.path);
                false
            } else {
                true
            }
        });
        if git {
            self.fswatch.git_due = Some(Instant::now() + GIT_SETTLE);
        }
    }

    /// Rebuild the explorer now, or at the end of the throttle window when one just happened.
    pub fn fswatch_schedule_tree(&mut self) {
        let next = self.last_tree_refresh + TREE_EVERY;
        if next <= Instant::now() {
            self.fswatch_rebuild_tree();
        } else if self.fswatch.tree_due.is_none() {
            self.fswatch.tree_due = Some(next);
        }
    }

    fn fswatch_rebuild_tree(&mut self) {
        self.fswatch.tree_due = None;
        self.last_tree_refresh = Instant::now();
        self.tree.rebuild();
        self.file_index = None;
        self.refresh_git_status();
        self.watch_dirs();
        self.request_redraw();
    }

    /// Timers: a deferred explorer rebuild, a settled `.git` burst.
    pub fn fswatch_tick(&mut self, now: Instant, want: &mut impl FnMut(Instant)) {
        if let Some(t) = self.fswatch.tree_due {
            if t <= now {
                self.fswatch_rebuild_tree();
            } else {
                want(t);
            }
        }
        if let Some(t) = self.fswatch.git_due {
            if t <= now {
                self.fswatch.git_due = None;
                self.fswatch_git_check();
            } else {
                want(t);
            }
        }
    }

    /// Compare the git dir with what was seen last and refresh what moved. Returns whether the
    /// status was refreshed.
    pub fn fswatch_git_check(&mut self) -> bool {
        let Some((g, c)) = self.fswatch.git.clone() else { return false };
        let sig = git_sig(&g, &c);
        if sig == self.fswatch.git_sig {
            return false;
        }
        let head_moved = sig.head_moved(&self.fswatch.git_sig);
        let branch_changed = sig.head != self.fswatch.git_sig.head;
        self.fswatch.git_sig = sig;
        self.fswatch.git_refreshes += 1;
        self.refresh_git_status();
        if head_moved {
            self.fswatch.head_refreshes += 1;
            // The gutter compares with HEAD: new base for every open file.
            self.sync_docs_with_head();
            // Ahead / behind counts and the Git panel follow.
            self.gitbg.next_poll = Instant::now();
            // Files the checkout rewrote reload on the next tick.
            self.last_disk_check = Instant::now() - Duration::from_secs(60);
        }
        if branch_changed {
            // The new branch's ref may live in another folder (`refs/heads/feature/...`).
            self.watch_dirs();
        }
        self.request_redraw();
        true
    }

    fn expanded_sig(&self) -> u64 {
        let dirs: Vec<&Path> = std::iter::once(self.root.as_path())
            .chain(self.tree.rows.iter().filter(|r| r.is_dir && r.expanded).map(|r| r.path.as_path()))
            .collect();
        dirs_sig(dirs.into_iter())
    }

    /// The window lost the focus: remember how the expanded folders looked.
    pub fn fswatch_blur(&mut self) {
        self.fswatch.blur_tree = Some(self.expanded_sig());
    }

    /// The window got the focus back: a cheap look at what may have changed meanwhile (open files'
    /// mtimes on the next tick, the git dir, the expanded folders) and a refresh of what did.
    pub fn fswatch_focus(&mut self) {
        self.last_disk_check = Instant::now() - Duration::from_secs(10);
        if !self.fswatch_git_check() {
            // Work tree changes in folders nobody watches only show up in `git status`.
            self.refresh_git_status();
        }
        if self.fswatch.blur_tree.take().is_some_and(|s| s != self.expanded_sig()) {
            self.fswatch_rebuild_tree();
        } else {
            // Re-arm watches (a folder deleted and recreated while away).
            self.watch_dirs();
        }
        if let Some(p) = &self.fswatch.poller {
            p.poll_now();
        }
    }

    /// "File: Refresh explorer and git status": re-read the tree, the ignore rules, the repository
    /// (it may have been created or moved), git status, the gutter base, and every open file.
    pub fn refresh_explorer(&mut self) {
        self.tree.invalidate_ignore();
        self.fswatch.git_for = None; // find the repository again, `statfs` again
        self.fswatch_rebuild_tree();
        self.sync_docs_with_head();
        self.gitbg.next_poll = Instant::now();
        self.last_disk_check = Instant::now() - Duration::from_secs(60);
        if let Some(p) = &self.fswatch.poller {
            p.poll_now();
        }
        self.say("Explorer and git status refreshed");
    }

    // ───────────── drawing, pointer ─────────────

    /// The refresh button at the right of the explorer header ("Files").
    pub fn draw_explorer_refresh(&mut self, cv: &mut Canvas) {
        self.fswatch.refresh_btn = None;
        if self.sidebar_mode != SidebarMode::Files {
            return;
        }
        let c = self.theme.chrome;
        let r = self.layout.sidebar;
        let b = Rect::new(r.right() - 1 - 10 - 26, r.y + (TAB_H - 26) / 2 + 2, 26, 26);
        let hot = self.overlay.is_none() && b.contains(self.mouse.0, self.mouse.1);
        if hot {
            cv.fill_round(b, 6, c.list_hover_bg);
        }
        let col = if hot { c.tab_active_fg } else { c.tab_inactive_fg };
        self.tx.icon(cv, "ui/rotate_cw", b.x + 6, b.y + 6, 14, col, r);
        self.fswatch.refresh_btn = Some(b);
    }

    /// "Polling (NFS)" in the left group of the status bar while something is polled.
    pub fn draw_fswatch_status(&mut self, cv: &mut Canvas, x: i32) -> i32 {
        self.fswatch.poll_hint = None;
        let Some(reason) = self.fswatch.reason.filter(|_| self.fswatch.poller.is_some()) else { return x };
        let c = self.theme.chrome;
        let r = self.layout.status;
        let label = format!("Polling ({})", reason.label());
        let lw = self.tx.ui_width(&label, false);
        let b = Rect::new(x, r.y + 3, 8 + 12 + 6 + lw + 8, r.h - 6);
        if self.overlay.is_none() && b.contains(self.mouse.0, self.mouse.1) {
            cv.fill_round(b, 6, c.list_hover_bg);
        }
        let dim = c.tab_inactive_fg;
        self.tx.icon(cv, "ui/rotate_cw", b.x + 8, r.y + (r.h - 12) / 2, 12, dim, r);
        self.tx.draw_ui(cv, b.x + 8 + 12 + 6, r.y + (r.h - 18) / 2, &label, dim, r, false);
        self.fswatch.poll_hint = Some(b);
        b.right() + 2
    }

    fn fswatch_hit(&self, x: f64, y: f64) -> Option<(FsTip, Rect)> {
        if let Some(b) = self.fswatch.refresh_btn
            && self.sidebar_open
            && self.sidebar_mode == SidebarMode::Files
            && b.contains(x, y)
        {
            return Some((FsTip::Refresh, b));
        }
        if let Some(b) = self.fswatch.poll_hint
            && self.fswatch.poller.is_some()
            && b.contains(x, y)
        {
            return Some((FsTip::Polling, b));
        }
        None
    }

    /// A click on the refresh button or on the polling hint (both refresh).
    pub fn fswatch_click(&mut self, x: f64, y: f64) -> bool {
        if self.fswatch_hit(x, y).is_none() {
            return false;
        }
        self.refresh_explorer();
        true
    }

    pub fn fswatch_clickable_at(&self, x: f64, y: f64) -> bool {
        self.fswatch_hit(x, y).is_some()
    }

    /// Tooltip target: (what, where, below the target?).
    pub fn fswatch_tip_at(&self, x: f64, y: f64) -> Option<(FsTip, Rect, bool)> {
        self.fswatch_hit(x, y).map(|(k, r)| (k, r, k == FsTip::Refresh))
    }

    pub fn fswatch_tip(&self, k: FsTip) -> crate::app_tooltip::TipText {
        match k {
            FsTip::Refresh => crate::app_tooltip::TipText {
                title: "Refresh explorer and git status".into(),
                keys: self.shortcut_text("RefreshExplorer"),
                detail: Some("Re-read the files, the branch and the changes from disk".into()),
                more: Vec::new(),
            },
            FsTip::Polling => {
                let why = self.fswatch.reason.map(|r| r.why()).unwrap_or_default();
                let n = self.fswatch.polled.len();
                let every = self.fswatch.poller.as_ref().map_or(0, |p| p.every.as_millis());
                crate::app_tooltip::TipText {
                    title: "Checking for changes by polling".into(),
                    keys: None,
                    detail: Some(why.to_string()),
                    more: vec![
                        format!(
                            "The open files and expanded folders ({n}) are checked every {}; click to refresh now.",
                            fmt_ms(every)
                        ),
                        "watcher_poll_ms in config.json sets the interval.".into(),
                    ],
                }
            }
        }
    }
}

/// "2 s", "500 ms".
fn fmt_ms(ms: u128) -> String {
    if ms >= 1000 && ms % 1000 == 0 { format!("{} s", ms / 1000) } else { format!("{ms} ms") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn repo(tag: &str) -> PathBuf {
        let d = tmp(tag);
        git(&d, &["init", "-q", "-b", "main"]);
        git(&d, &["config", "user.email", "t@t"]);
        git(&d, &["config", "user.name", "t"]);
        git(&d, &["config", "commit.gpgsign", "false"]);
        std::fs::write(d.join("f.ts"), "one\n").unwrap();
        std::fs::write(d.join(".gitignore"), ".config/\nseed.txt\n").unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", "init"]);
        d
    }

    /// Run the watcher pump and the timers until `cond` holds (or give up after `ms`).
    fn settle(app: &mut App, ms: u64, mut cond: impl FnMut(&mut App) -> bool) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            app.pump_watcher();
            app.tick();
            if cond(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        false
    }

    #[test]
    fn git_signature_follows_head_refs_packed_refs_and_index() {
        let d = tmp("gitsig");
        let g = d.join(".git");
        std::fs::create_dir_all(g.join("refs/heads/feature")).unwrap();
        std::fs::write(g.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(g.join("refs/heads/main"), "aaaa\n").unwrap();
        let s1 = git_sig(&g, &g);
        assert_eq!((s1.head.as_str(), s1.oid.as_str()), ("ref: refs/heads/main", "aaaa"));
        assert_eq!(s1.index, None);
        // A commit moves the ref: HEAD moved, same branch.
        std::fs::write(g.join("refs/heads/main"), "bbbb\n").unwrap();
        let s2 = git_sig(&g, &g);
        assert!(s2.head_moved(&s1) && s2.head == s1.head);
        // A packed ref (after `git gc`, or a fresh clone).
        std::fs::write(g.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        std::fs::write(
            g.join("packed-refs"),
            "# pack-refs with: peeled\ncccc refs/heads/feature/x\ndddd refs/heads/feature/xy\n",
        )
        .unwrap();
        assert_eq!(git_sig(&g, &g).oid, "cccc");
        // Detached HEAD.
        std::fs::write(g.join("HEAD"), "eeee\n").unwrap();
        assert_eq!(git_sig(&g, &g).oid, "eeee");
        // The index alone changing is not a HEAD move.
        let s3 = git_sig(&g, &g);
        std::fs::write(g.join("index"), "x").unwrap();
        let s4 = git_sig(&g, &g);
        assert!(s4 != s3 && !s4.head_moved(&s3));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn git_watch_folders_and_lock_files() {
        let g = Path::new("/r/.git");
        let w = git_watch_dirs(g, g, "ref: refs/heads/feature/login");
        assert_eq!(w, vec![g.to_path_buf(), g.join("refs/heads"), g.join("refs/heads/feature")]);
        let w = git_watch_dirs(g, g, "ref: refs/heads/main");
        assert_eq!(w, vec![g.to_path_buf(), g.join("refs/heads")], "no duplicate for a flat name");
        // Linked worktree: HEAD and index in the worktree's git dir, refs in the common dir.
        let wt = Path::new("/r/.git/worktrees/b");
        let w = git_watch_dirs(wt, g, "abcd");
        assert_eq!(w, vec![wt.to_path_buf(), g.to_path_buf(), g.join("refs/heads")]);
        assert!(!git_event_matters(Path::new("/r/.git/index.lock")));
        assert!(!git_event_matters(Path::new("/r/.git/refs/heads/main.lock")));
        assert!(git_event_matters(Path::new("/r/.git/index")));
        assert!(git_event_matters(Path::new("/r/.git/HEAD")));
        assert_eq!(fmt_ms(2000), "2 s");
        assert_eq!(fmt_ms(750), "750 ms");
    }

    #[test]
    fn folder_signature_moves_when_entries_change() {
        let d = tmp("dirsig");
        let s0 = dirs_sig(std::iter::once(d.as_path()));
        assert_eq!(s0, dirs_sig(std::iter::once(d.as_path())));
        std::thread::sleep(Duration::from_millis(15));
        std::fs::write(d.join("new.txt"), "x").unwrap();
        let s1 = dirs_sig(std::iter::once(d.as_path()));
        if std::fs::metadata(&d).unwrap().modified().is_ok() {
            assert_ne!(s0, s1);
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn terminal_checkout_commit_and_add_refresh_branch_gutter_and_status() {
        let _g = lock();
        let d = repo("fsw-git");
        let mut app = test_app(&d);
        let f = d.join("f.ts");
        let i = app.open_path(&f, None).unwrap();
        assert_eq!(app.branch.as_deref(), Some("main"));
        assert!(app.fswatch.git.is_some(), "the project is a repository");
        let git_dir = app.fswatch.git.as_ref().unwrap().0.clone();
        assert!(app.watcher.as_ref().unwrap().is_watching(&git_dir), ".git is watched");

        // `git checkout -b` in a terminal: only .git changes.
        git(&d, &["checkout", "-q", "-b", "feature/login"]);
        assert!(
            settle(&mut app, 4000, |a| a.branch.as_deref() == Some("feature/login")),
            "branch after checkout: {:?}",
            app.branch
        );
        let heads = app.fswatch.head_refreshes;
        // The ref of a branch with a slash lives in refs/heads/feature: watched too.
        let common = app.fswatch.git.as_ref().unwrap().1.clone();
        assert!(app.watcher.as_ref().unwrap().is_watching(&common.join("refs/heads/feature")));

        // Edit + commit from the terminal: the file and HEAD move; the gutter base follows.
        std::fs::write(&f, "one\ntwo\n").unwrap();
        git(&d, &["commit", "-q", "-am", "two"]);
        assert!(
            settle(&mut app, 4000, |a| a.fswatch.head_refreshes > heads
                && a.docs[i].git_base.as_deref() == Some("one\ntwo\n")
                && a.docs[i].buf.to_string() == "one\ntwo\n"),
            "gutter base after commit: {:?}",
            app.docs[i].git_base
        );
        assert!(settle(&mut app, 2000, |a| !a.docs[i].git_stale && a.docs[i].git.is_empty()), "no marks left");

        // Index only (`git add` of a new file): the badge goes from new to added, the work tree
        // does not change.
        let n = d.join("n.ts");
        std::fs::write(&n, "n\n").unwrap();
        assert!(settle(&mut app, 4000, |a| a.git_status.get(&n) == Some(&'?')), "untracked file");
        let before = app.fswatch.git_refreshes;
        git(&d, &["add", "n.ts"]);
        assert!(
            settle(&mut app, 4000, |a| a.git_status.get(&n) == Some(&'A')),
            "status after git add: {:?}",
            app.git_status.get(&n)
        );
        assert!(app.fswatch.git_refreshes > before);

        // Nothing changes: nothing refreshes (no loop through `git status` and the index).
        let (n, h) = (app.fswatch.git_refreshes, app.fswatch.head_refreshes);
        settle(&mut app, 600, |_| false);
        assert_eq!((app.fswatch.git_refreshes, app.fswatch.head_refreshes), (n, h), "quiet when idle");
        drop(app);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn branch_switch_back_and_forth_and_reset_are_seen() {
        let _g = lock();
        let d = repo("fsw-switch");
        git(&d, &["branch", "other"]);
        let mut app = test_app(&d);
        git(&d, &["checkout", "-q", "other"]);
        assert!(settle(&mut app, 4000, |a| a.branch.as_deref() == Some("other")));
        git(&d, &["checkout", "-q", "main"]);
        assert!(settle(&mut app, 4000, |a| a.branch.as_deref() == Some("main")));
        // A new commit then `git reset --hard HEAD~1`: HEAD moves twice on the same branch.
        std::fs::write(d.join("g.ts"), "g\n").unwrap();
        git(&d, &["add", "g.ts"]);
        git(&d, &["commit", "-q", "-m", "g"]);
        let sig_after_commit = git_sig(&app.fswatch.git.as_ref().unwrap().0, &app.fswatch.git.as_ref().unwrap().1);
        assert!(settle(&mut app, 4000, |a| a.fswatch.git_sig == sig_after_commit));
        git(&d, &["reset", "-q", "--hard", "HEAD~1"]);
        assert!(settle(&mut app, 4000, |a| a.fswatch.git_sig.oid != sig_after_commit.oid));
        drop(app);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn deleted_and_recreated_folder_with_an_open_file_keeps_being_watched() {
        let _g = lock();
        let d = tmp("fsw-recreate");
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let f = sub.join("a.txt");
        std::fs::write(&f, "v1\n").unwrap();
        let mut app = test_app(&d);
        let i = app.open_path(&f, None).unwrap();
        app.watch_dirs();
        assert!(app.watcher.as_ref().unwrap().is_watching(&sub));
        // `rm -rf sub && mkdir sub && echo v2 > sub/a.txt`, as a checkout or a generator does.
        std::fs::remove_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(&f, "v2\n").unwrap();
        assert!(settle(&mut app, 4000, |a| a.docs[i].buf.to_string() == "v2\n"), "reloaded after recreate");
        assert!(
            settle(&mut app, 2000, |a| a.watcher.as_ref().unwrap().is_watching(&sub)),
            "the recreated folder is watched again"
        );
        // And later edits keep arriving through the new watch (well before the 6 s safety net).
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&f, "v3\n").unwrap();
        assert!(settle(&mut app, 3000, |a| a.docs[i].buf.to_string() == "v3\n"), "edit after recreate");
        drop(app);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn atomic_save_and_delete_recreate_of_an_open_file_reload_it() {
        let _g = lock();
        let d = tmp("fsw-atomic");
        let f = d.join("doc.ts");
        std::fs::write(&f, "a\n").unwrap();
        let mut app = test_app(&d);
        let i = app.open_path(&f, None).unwrap();
        app.watch_dirs();
        // Write a temp file and rename it over the original.
        let t = d.join(".doc.ts.swp");
        std::fs::write(&t, "b\n").unwrap();
        std::fs::rename(&t, &f).unwrap();
        assert!(settle(&mut app, 3000, |a| a.docs[i].buf.to_string() == "b\n"), "rename over");
        // Delete, then create again.
        std::thread::sleep(Duration::from_millis(30));
        std::fs::remove_file(&f).unwrap();
        std::fs::write(&f, "c\n").unwrap();
        assert!(settle(&mut app, 3000, |a| a.docs[i].buf.to_string() == "c\n"), "delete + recreate");
        drop(app);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn untrusted_file_systems_are_polled_and_shown_in_the_status_bar() {
        let _g = lock();
        let d = tmp("fsw-poll");
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let mut app = test_app(&d);
        assert!(app.fswatch.poller.is_none(), "a local disk needs no polling");
        app.fswatch.force = Some(Untrusted::Nfs);
        app.config.watcher_poll_ms = 40; // below the minimum: clamped
        app.watch_dirs();
        let p = app.fswatch.poller.as_ref().expect("polling starts");
        assert_eq!(p.every, Duration::from_millis(crate::poller::MIN_MS));
        assert_eq!(app.fswatch.reason, Some(Untrusted::Nfs));
        assert!(p.is_polling_dir(&d), "the root is polled");
        assert!(!p.is_polling_dir(&sub), "collapsed folders are not");
        assert!(!p.is_polling_file(&d.join("seed.txt")), "an open file in a polled folder is covered by it");
        // An open file in a collapsed folder is polled on its own (a stat, not a listing).
        let deep_dir = d.join("deep");
        std::fs::create_dir_all(&deep_dir).unwrap();
        let deep = deep_dir.join("x.txt");
        std::fs::write(&deep, "x").unwrap();
        app.open_path(&deep, None);
        app.tree.rebuild();
        let k = app.tree.rows.iter().position(|r| r.path == deep_dir).unwrap();
        if app.tree.rows[k].expanded {
            app.tree.toggle(k);
        }
        app.watch_dirs();
        let p = app.fswatch.poller.as_ref().unwrap();
        assert!(p.is_polling_file(&deep) && !p.is_polling_dir(&deep_dir));
        // Expanding a folder adds it.
        let k = app.tree.rows.iter().position(|r| r.path == sub).unwrap();
        app.tree.toggle(k);
        let before = app.fswatch.poller.as_ref().unwrap().passes();
        app.watch_dirs();
        assert!(app.fswatch.poller.as_ref().unwrap().is_polling_dir(&sub));
        // A change only the poller sees (the inotify watcher is gone, as on NFS), made after the
        // pass that recorded the newly expanded folder.
        app.watcher = None;
        let p = app.fswatch.poller.as_ref().unwrap();
        let t0 = Instant::now();
        while p.passes() < before + 2 && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(sub.join("remote.txt"), "x").unwrap();
        assert!(
            settle(&mut app, 4000, |a| a.tree.rows.iter().any(|r| r.name == "remote.txt")),
            "the explorer shows a file created behind the mount"
        );
        // The hint is drawn and explains itself.
        let (w, h) = (900i32, 60i32);
        let mut px = vec![0u32; (w * h) as usize];
        app.layout.status = Rect::new(0, 30, w, 28);
        let x = app.draw_fswatch_status(&mut Canvas { px: &mut px, w, h }, 10);
        assert!(x > 10 && app.fswatch.poll_hint.is_some());
        let tip = app.fswatch_tip(FsTip::Polling);
        assert!(tip.detail.unwrap().contains("network"));
        assert!(tip.more[0].contains("250 ms"));
        // Back on a trusted disk: no poller, no hint.
        app.fswatch.force = None;
        app.watch_dirs();
        assert!(app.fswatch.poller.is_none());
        assert_eq!(app.draw_fswatch_status(&mut Canvas { px: &mut px, w, h }, 10), 10);
        drop(app);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn refresh_command_and_focus_pick_up_changes_nobody_reported() {
        let _g = lock();
        let d = repo("fsw-refresh");
        let mut app = test_app(&d);
        // No watcher at all: only the refresh can see the change.
        app.watcher = None;
        std::fs::write(d.join("added.ts"), "x\n").unwrap();
        assert!(!app.tree.rows.iter().any(|r| r.name == "added.ts"));
        app.run_cmd(crate::app_overlay::Cmd::RefreshExplorer);
        assert!(app.tree.rows.iter().any(|r| r.name == "added.ts"));
        assert_eq!(app.git_status.get(&d.join("added.ts")), Some(&'?'));
        // Focus: a terminal checkout while the window was away.
        app.fswatch_blur();
        git(&d, &["checkout", "-q", "-b", "away"]);
        std::thread::sleep(Duration::from_millis(15));
        std::fs::write(d.join("other.ts"), "y\n").unwrap();
        let heads = app.fswatch.head_refreshes;
        app.fswatch_focus();
        assert_eq!(app.branch.as_deref(), Some("away"));
        assert_eq!(app.fswatch.head_refreshes, heads + 1);
        assert!(app.tree.rows.iter().any(|r| r.name == "other.ts"), "the root changed: tree rebuilt");
        // The palette lists the command.
        assert!(
            crate::app_overlay::COMMANDS
                .iter()
                .any(|(l, _, c)| *c == crate::app_overlay::Cmd::RefreshExplorer
                    && l.starts_with("File: Refresh explorer"))
        );
        drop(app);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn events_right_after_a_refresh_are_deferred_not_dropped() {
        let _g = lock();
        let d = tmp("fsw-defer");
        let mut app = test_app(&d);
        app.last_tree_refresh = Instant::now();
        std::fs::write(d.join("late.txt"), "x").unwrap();
        app.fswatch_schedule_tree();
        assert!(app.fswatch.tree_due.is_some(), "inside the window: deferred");
        assert!(!app.tree.rows.iter().any(|r| r.name == "late.txt"));
        let mut next = None;
        app.fswatch_tick(Instant::now(), &mut |t| next = Some(t));
        assert!(next.is_some(), "a timer is asked for");
        std::thread::sleep(TREE_EVERY);
        app.fswatch_tick(Instant::now(), &mut |_| {});
        assert!(app.tree.rows.iter().any(|r| r.name == "late.txt"));
        assert!(app.fswatch.tree_due.is_none());
        drop(app);
        std::fs::remove_dir_all(&d).ok();
    }
}
