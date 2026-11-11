//! History tab model: the commit graph streamed from one `git log --topo-order` process on a
//! worker thread, a page at a time, and the row arithmetic that lets the tab draw only the
//! visible rows (a repository with 100k commits never builds a 100k-entry list per frame).
//!
//! The worker reads a page, lays it out ([`Lanes`]), sends it and then waits until the panel asks
//! for more (the pipe holds git back meanwhile), so only what has been scrolled to is ever loaded.
//! Dropping the [`HistLoader`] (history reset, panel closed) ends the worker and kills git.

use super::*;
use std::io::{BufRead, BufReader};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

/// Commits in the first page, and in every page after it.
pub(super) const FIRST_PAGE: usize = 300;
pub(super) const NEXT_PAGE: usize = 600;
/// Ask for the next page when the last visible row is this close to the end of what is loaded.
pub(super) const LOAD_AHEAD: usize = 200;

/// One page laid out by the worker.
#[derive(Default)]
pub struct Batch {
    pub entries: Vec<LogEntry>,
    pub rows: Vec<GRow>,
    pub segs: Vec<Seg>,
    /// (index in this page, labels), only for commits that have labels.
    pub chips: Vec<(usize, Vec<Chip>)>,
    /// git has no more commits to give.
    pub done: bool,
}

pub struct HistLoader {
    rx: Receiver<Batch>,
    want: Sender<usize>,
    /// Commits requested so far (the worker's target).
    pub asked: usize,
}

/// Arguments of the graph's `git log`: every branch, remote branch and tag, plus HEAD when there
/// is one (the stash and other refs stay out).
pub fn graph_args(with_head: bool) -> Vec<&'static str> {
    let mut v = vec![
        "-c",
        "log.showSignature=false",
        "log",
        "--topo-order",
        "--decorate=full",
        "--no-color",
        GRAPH_FORMAT,
        "--branches",
        "--remotes",
        "--tags",
    ];
    if with_head {
        v.push("HEAD");
    }
    v.push("--");
    v
}

impl HistLoader {
    /// Start `git log` in `top`; the first `first` commits are laid out and sent right away.
    /// `wake` (the event loop) is called after every page.
    pub fn spawn(top: &Path, first: usize, wake: Option<crate::lsp::Wake>) -> HistLoader {
        let (tx, rx) = channel::<Batch>();
        let (want, want_rx) = channel::<usize>();
        let top = top.to_path_buf();
        std::thread::spawn(move || {
            let notify = || {
                if let Some(w) = &wake {
                    w();
                }
            };
            let with_head = git_run(&top, &["rev-parse", "-q", "--verify", "HEAD"]).is_ok();
            let child = Command::new("git")
                .arg("-C")
                .arg(&top)
                .args(graph_args(with_head))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(_) => {
                    let _ = tx.send(Batch { done: true, ..Batch::default() });
                    return notify();
                }
            };
            let Some(out) = child.stdout.take() else {
                let _ = child.kill();
                let _ = tx.send(Batch { done: true, ..Batch::default() });
                return notify();
            };
            let mut lines = BufReader::new(out).split(b'\n');
            let mut lanes = Lanes::default();
            let (mut target, mut loaded) = (first, 0usize);
            loop {
                let mut b = Batch::default();
                while loaded < target {
                    let Some(Ok(raw)) = lines.next() else {
                        b.done = true;
                        break;
                    };
                    let line = String::from_utf8_lossy(&raw);
                    let Some((e, parents, chips)) = parse_graph_line(line.trim_end_matches('\r')) else { continue };
                    b.rows.push(lanes.push(&e.hash, &parents, &mut b.segs));
                    if !chips.is_empty() {
                        b.chips.push((b.entries.len(), chips));
                    }
                    b.entries.push(e);
                    loaded += 1;
                }
                let done = b.done;
                if tx.send(b).is_err() {
                    break;
                }
                notify();
                if done {
                    break;
                }
                // Wait for the panel to scroll near the end (or to go away).
                match want_rx.recv() {
                    Ok(n) => target = target.max(n),
                    Err(_) => break,
                }
                while let Ok(n) = want_rx.try_recv() {
                    target = target.max(n);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });
        HistLoader { rx, want, asked: first }
    }
}

impl GitPanel {
    /// Ask for the next page of the history (non-blocking; starts the loader the first time).
    pub fn load_more(&mut self) {
        if self.log_done {
            return;
        }
        let Some(top) = self.top.clone() else {
            self.log_done = true;
            return;
        };
        let have = self.commits.len();
        let gone = match self.loader.as_mut() {
            None => {
                self.loader = Some(HistLoader::spawn(&top, FIRST_PAGE, self.wake.clone()));
                false
            }
            // One request at a time: the next one once the previous page has arrived.
            Some(l) if l.asked <= have => {
                l.asked = have + NEXT_PAGE;
                l.want.send(l.asked).is_err()
            }
            Some(_) => false,
        };
        if gone {
            self.log_done = true;
            self.loader = None;
        }
    }

    /// Take the pages the worker finished; true when something arrived.
    pub fn poll_history(&mut self) -> bool {
        let Some(l) = self.loader.as_mut() else { return false };
        let mut got = false;
        let mut done = false;
        loop {
            match l.rx.try_recv() {
                Ok(b) => {
                    got = true;
                    let base = self.commits.len();
                    self.commits.extend(b.entries);
                    self.graph.append(b.rows, b.segs);
                    for (i, c) in b.chips {
                        self.chips.insert(base + i, c);
                    }
                    done |= b.done;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = true;
                    got = true;
                    break;
                }
            }
        }
        if done {
            self.log_done = true;
            self.loader = None;
        }
        got
    }

    /// Wait (up to `secs`) until every requested page has arrived. Tests only.
    #[cfg(test)]
    pub fn wait_history(&mut self, secs: u64) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            self.poll_history();
            match &self.loader {
                Some(l) if self.commits.len() < l.asked => std::thread::sleep(Duration::from_millis(5)),
                _ => return,
            }
        }
    }

    /// Labels of commit `i`.
    pub fn chips_of(&self, i: usize) -> &[Chip] {
        self.chips.get(&i).map_or(&[], Vec::as_slice)
    }

    /// Commit `i` is the checked-out one.
    pub fn is_head(&self, i: usize) -> bool {
        self.chips_of(i).iter().any(|c| matches!(c.kind, ChipKind::Head | ChipKind::Current))
    }

    // ───────────── row arithmetic (History tab) ─────────────
    //
    // Rows are the commits, with the files of the expanded commit (`open_commit`) inserted after
    // it. Commit rows are COMMIT_H tall and file rows CFILE_H, so positions are computed, never
    // listed.

    pub fn hist_len(&self) -> usize {
        self.commits.len() + self.open_commit.map_or(0, |_| self.files.len())
    }

    pub fn hist_row(&self, k: usize) -> Option<Row> {
        if k >= self.hist_len() {
            return None;
        }
        Some(match self.open_commit {
            Some(o) if k > o && k <= o + self.files.len() => Row::CFile(k - o - 1),
            Some(o) if k > o => Row::Commit(k - self.files.len()),
            _ => Row::Commit(k),
        })
    }

    pub fn hist_index(&self, r: Row) -> Option<usize> {
        let k = match (r, self.open_commit) {
            (Row::Commit(i), Some(o)) if i > o => i + self.files.len(),
            (Row::Commit(i), _) => i,
            (Row::CFile(j), Some(o)) if j < self.files.len() => o + 1 + j,
            _ => return None,
        };
        (k < self.hist_len()).then_some(k)
    }

    /// Top of row `k` in content coordinates.
    pub fn hist_top(&self, k: usize) -> i32 {
        match self.open_commit {
            Some(o) if k > o => {
                let f = (k - o - 1).min(self.files.len());
                (o as i32 + 1) * COMMIT_H + f as i32 * CFILE_H + (k - o - 1 - f) as i32 * COMMIT_H
            }
            _ => k as i32 * COMMIT_H,
        }
    }

    /// The row at content height `y` (clamped to the rows that exist).
    pub fn hist_at_y(&self, y: i32) -> usize {
        let y = y.max(0);
        let k = match self.open_commit {
            Some(o) if y >= (o as i32 + 1) * COMMIT_H => {
                let base = (o as i32 + 1) * COMMIT_H;
                let fh = self.files.len() as i32 * CFILE_H;
                if y < base + fh {
                    o + 1 + ((y - base) / CFILE_H) as usize
                } else {
                    o + 1 + self.files.len() + ((y - base - fh) / COMMIT_H) as usize
                }
            }
            _ => (y / COMMIT_H) as usize,
        };
        k.min(self.hist_len().saturating_sub(1))
    }

    /// Lane palette for the panel background (recomputed only when the theme changes).
    pub fn palette(&mut self, bg: Rgba, on: &[Rgba]) -> [Rgba; LANE_COLORS] {
        match self.lane_palette {
            Some((b, p)) if b == bg => p,
            _ => {
                let p = lane_colors(bg, on);
                self.lane_palette = Some((bg, p));
                p
            }
        }
    }
}
