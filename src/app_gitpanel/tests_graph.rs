//! Tests of the commit graph (lane assignment on synthetic histories, labels, colours, the
//! streamed history and its row arithmetic) and of the stash flows against temporary
//! repositories.

use super::*;

fn tmp_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-gg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "Ana"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git_run(&dir, a).unwrap();
    }
    dir
}

fn commit_all(dir: &Path, msg: &str) {
    git_run(dir, &["add", "-A"]).unwrap();
    git_run(dir, &["commit", "-q", "-m", msg]).unwrap();
}

fn rev(dir: &Path, r: &str) -> String {
    git_run(dir, &["rev-parse", r]).unwrap().trim().to_string()
}

// ───────────── lane assignment ─────────────

fn layout(commits: &[(&str, Vec<&str>)]) -> Graph {
    let mut lanes = Lanes::default();
    let (mut rows, mut segs) = (Vec::new(), Vec::new());
    for (h, p) in commits {
        rows.push(lanes.push(h, p, &mut segs));
    }
    let mut g = Graph::default();
    g.append(rows, segs);
    g
}

/// The columns and colours leaving each row at the bottom are exactly those entering the next
/// row at the top; bends end / start at the node; nothing enters the first row; and, when the
/// history is complete, nothing leaves the last one.
fn assert_continuous(g: &Graph, complete: bool) {
    let n = g.rows.len();
    assert!(g.segs_of(0).iter().all(|s| s.lower), "nothing comes from above the first row");
    for i in 0..n {
        let col = g.rows[i].col;
        for s in g.segs_of(i) {
            if !s.lower && s.from != s.to {
                assert_eq!(s.to, col, "row {i}: a bend from above ends at the node");
            }
            if s.lower && s.from != s.to {
                assert_eq!(s.from, col, "row {i}: a bend below starts at the node");
            }
            assert!(s.from.max(s.to) < g.rows[i].width, "row {i}: width covers every segment");
        }
        let mut below: Vec<(u16, u8)> = g.segs_of(i).iter().filter(|s| s.lower).map(|s| (s.to, s.color)).collect();
        below.sort_unstable();
        below.dedup();
        if i + 1 < n {
            let mut above: Vec<(u16, u8)> =
                g.segs_of(i + 1).iter().filter(|s| !s.lower).map(|s| (s.from, s.color)).collect();
            above.sort_unstable();
            above.dedup();
            assert_eq!(below, above, "rows {i} and {}", i + 1);
        } else if complete {
            assert!(below.is_empty(), "the root closes every lane: {below:?}");
        }
    }
}

#[test]
fn linear_history_is_one_straight_lane() {
    let g = layout(&[("d", vec!["c"]), ("c", vec!["b"]), ("b", vec!["a"]), ("a", vec![])]);
    assert!(g.rows.iter().all(|r| r.col == 0 && r.width == 1 && !r.merge));
    assert!(g.rows.iter().all(|r| r.color == g.rows[0].color), "one lane, one colour");
    assert!(g.segs_of(3).iter().all(|s| !s.lower), "the root ends the lane");
    assert_continuous(&g, true);
}

#[test]
fn a_merge_opens_a_lane_that_joins_back_at_the_fork_point() {
    // M merges feature B into main A; both grew from Base.
    let g = layout(&[("M", vec!["A", "B"]), ("B", vec!["Base"]), ("A", vec!["Base"]), ("Base", vec![])]);
    let cols: Vec<u16> = g.rows.iter().map(|r| r.col).collect();
    assert_eq!(cols, [0, 1, 0, 0]);
    assert!(g.rows[0].merge && g.rows[0].width == 2);
    assert!(g.segs_of(0).contains(&Seg { from: 0, to: 1, color: g.rows[1].color, lower: true }), "merge bend");
    assert_ne!(g.rows[0].color, g.rows[1].color, "the branch has its own colour");
    // At the fork point the feature lane bends back into Base's node.
    assert!(g.segs_of(3).iter().any(|s| !s.lower && s.from == 1 && s.to == 0));
    assert_eq!(g.rows[3].width, 2);
    assert_continuous(&g, true);
}

#[test]
fn octopus_merge_fans_out_to_one_lane_per_parent() {
    let g = layout(&[("O", vec!["A", "B", "C"]), ("C", vec!["R"]), ("B", vec!["R"]), ("A", vec!["R"]), ("R", vec![])]);
    assert!(g.rows[0].merge);
    assert_eq!(g.rows[0].width, 3);
    let bends: Vec<u16> = g.segs_of(0).iter().filter(|s| s.lower && s.from != s.to).map(|s| s.to).collect();
    assert_eq!(bends, [1, 2]);
    let cols: Vec<u16> = g.rows.iter().map(|r| r.col).collect();
    assert_eq!(cols, [0, 2, 1, 0, 0]);
    let into_root = g.segs_of(4).iter().filter(|s| !s.lower && s.to == 0).count();
    assert_eq!(into_root, 3, "three lanes end in the root");
    assert_continuous(&g, true);
}

#[test]
fn many_parallel_branches_get_one_column_each_then_close() {
    let names: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
    let mut hist: Vec<(&str, Vec<&str>)> = names.iter().map(|n| (n.as_str(), vec!["base"])).collect();
    hist.push(("base", vec![]));
    let mut lanes = Lanes::default();
    let (mut rows, mut segs) = (Vec::new(), Vec::new());
    for (h, p) in &hist {
        rows.push(lanes.push(h, p, &mut segs));
    }
    assert_eq!(lanes.active(), 0, "every lane is closed after the root");
    let mut g = Graph::default();
    g.append(rows, segs);
    for i in 0..20 {
        assert_eq!(g.rows[i].col as usize, i, "tip {i} opens the next column");
        assert_eq!(g.rows[i].width as usize, i + 1);
    }
    assert_eq!(g.rows[20].col, 0);
    assert_eq!(g.rows[20].width, 20);
    assert_eq!(g.segs_of(20).iter().filter(|s| !s.lower).count(), 20, "all twenty lanes end in the base");
    assert_continuous(&g, true);
}

#[test]
fn columns_freed_by_merged_branches_are_reused() {
    // 50 feature branches merged one after the other: never wider than two lanes.
    let mut names: Vec<(String, Vec<String>)> = Vec::new();
    for i in (0..50).rev() {
        let below = if i > 0 { vec![format!("m{}", i - 1)] } else { vec![] };
        names.push((format!("m{i}"), vec![format!("b{i}"), format!("f{i}")]));
        names.push((format!("f{i}"), vec![format!("b{i}")]));
        names.push((format!("b{i}"), below));
    }
    let hist: Vec<(&str, Vec<&str>)> =
        names.iter().map(|(h, p)| (h.as_str(), p.iter().map(String::as_str).collect())).collect();
    let g = layout(&hist);
    assert!(g.rows.iter().all(|r| r.width <= 2), "{:?}", g.rows.iter().map(|r| r.width).max());
    assert_continuous(&g, true);
}

#[test]
fn random_histories_keep_every_line_continuous() {
    // A small LCG: deterministic, no dependency.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for round in 0..20 {
        let n = 150 + round * 10;
        let names: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
        // Parents are always older (a larger index): a valid topological order.
        let mut parents: Vec<Vec<usize>> = Vec::new();
        for i in 0..n {
            let mut ps = Vec::new();
            if i + 1 < n && rnd(40) != 0 {
                // Mostly one parent, sometimes a merge, now and then an octopus.
                let k = match rnd(10) {
                    0 => 3,
                    1 | 2 => 2,
                    _ => 1,
                };
                for _ in 0..k {
                    let p = i + 1 + rnd(((n - i - 1).min(25)) as u64) as usize;
                    if !ps.contains(&p) {
                        ps.push(p);
                    }
                }
            }
            parents.push(ps);
        }
        let hist: Vec<(&str, Vec<&str>)> =
            (0..n).map(|i| (names[i].as_str(), parents[i].iter().map(|&p| names[p].as_str()).collect())).collect();
        let g = layout(&hist);
        assert_eq!(g.rows.len(), n);
        assert_continuous(&g, true);
        // Laid out in pages, the result is the same.
        let mut lanes = Lanes::default();
        let mut paged = Graph::default();
        for chunk in hist.chunks(37) {
            let (mut rows, mut segs) = (Vec::new(), Vec::new());
            for (h, p) in chunk {
                rows.push(lanes.push(h, p, &mut segs));
            }
            paged.append(rows, segs);
        }
        for i in 0..n {
            assert_eq!(paged.segs_of(i), g.segs_of(i), "round {round}, row {i}");
            assert_eq!(paged.rows[i].col, g.rows[i].col);
        }
    }
}

// ───────────── labels, log lines, colours ─────────────

#[test]
fn decorations_become_labels() {
    let v = parse_decorations(
        "HEAD -> refs/heads/main, tag: refs/tags/v1.0, refs/remotes/origin/main, refs/remotes/origin/HEAD, refs/heads/feat, refs/stash",
    );
    let got: Vec<(ChipKind, &str)> = v.iter().map(|c| (c.kind, c.name.as_str())).collect();
    assert_eq!(
        got,
        [
            (ChipKind::Current, "main"),
            (ChipKind::Branch, "feat"),
            (ChipKind::Tag, "v1.0"),
            (ChipKind::Remote, "origin/main")
        ]
    );
    assert_eq!(parse_decorations("HEAD")[0].kind, ChipKind::Head);
    assert!(parse_decorations("").is_empty());
    let (e, parents, chips) =
        parse_graph_line("aaaa\0aa\0bbbb cccc\0Ana\01700000000\0tag: refs/tags/x\0fix: a, b").unwrap();
    assert_eq!((e.hash.as_str(), e.short.as_str(), e.author.as_str(), e.ts), ("aaaa", "aa", "Ana", 1_700_000_000));
    assert_eq!(e.subject, "fix: a, b");
    assert_eq!(parents, ["bbbb", "cccc"]);
    assert_eq!(chips, vec![Chip { kind: ChipKind::Tag, name: "x".into() }]);
    let (_, parents, _) = parse_graph_line("root\0r\0\0Ana\01\0\0first").unwrap();
    assert!(parents.is_empty());
    assert!(parse_graph_line("garbage").is_none());
}

#[test]
fn lane_colours_read_on_every_bundled_theme() {
    let dir = crate::config::assets_dir().join("themes");
    let mut checked = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let Ok(t) = crate::theme::Theme::load(&e.path()) else { continue };
        let c = t.chrome;
        let hover = c.tab_active_fg.with_alpha(0x1e);
        let pal = lane_colors(c.sidebar_bg, &[hover]);
        for (i, col) in pal.iter().enumerate() {
            for bg in [c.sidebar_bg, hover.over(c.sidebar_bg)] {
                let k = col.contrast(bg);
                assert!(k >= LANE_CONTRAST - 0.01, "{}: lane {i} {col:?} on {bg:?} is {k:.2}:1", e.path().display());
            }
        }
        // Still eight different colours.
        for i in 0..LANE_COLORS {
            for j in i + 1..LANE_COLORS {
                assert_ne!(pal[i], pal[j], "{}", e.path().display());
            }
        }
        checked += 1;
    }
    assert!(checked > 10, "light and dark themes were checked ({checked})");
    for bg in [Rgba(255, 255, 255, 255), Rgba(0, 0, 0, 255), Rgba(0x80, 0x80, 0x80, 255)] {
        assert!(lane_colors(bg, &[]).iter().all(|c| c.contrast(bg) >= LANE_CONTRAST - 0.01), "{bg:?}");
    }
}

// ───────────── the History tab ─────────────

fn fake_panel(n: usize) -> GitPanel {
    let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gg-no-such-dir"));
    gp.tab = Tab::History;
    gp.commits = (0..n)
        .map(|i| LogEntry {
            hash: format!("h{i}"),
            short: format!("s{i}"),
            author: "a".into(),
            ts: 0,
            subject: "x".into(),
        })
        .collect();
    gp
}

#[test]
fn history_rows_are_computed_not_listed() {
    let mut gp = fake_panel(100_000);
    assert_eq!(gp.hist_len(), 100_000);
    assert_eq!(gp.hist_top(50_000), 50_000 * COMMIT_H);
    assert_eq!(gp.hist_at_y(50_000 * COMMIT_H + 3), 50_000);
    assert_eq!(gp.hist_at_y(i32::MAX), 99_999, "clamped to the last row");
    gp.files = (0..3).map(|i| CommitFile { code: 'M', path: format!("f{i}"), old: None }).collect();
    gp.open_commit = Some(10);
    assert_eq!(gp.hist_len(), 100_003);
    assert_eq!(gp.hist_row(10), Some(Row::Commit(10)));
    assert_eq!(gp.hist_row(11), Some(Row::CFile(0)));
    assert_eq!(gp.hist_row(13), Some(Row::CFile(2)));
    assert_eq!(gp.hist_row(14), Some(Row::Commit(11)));
    assert_eq!(gp.hist_row(100_003), None);
    for k in [0, 9, 10, 11, 12, 13, 14, 15, 5000, 100_002] {
        let r = gp.hist_row(k).unwrap();
        assert_eq!(gp.hist_index(r), Some(k), "{r:?}");
        let top = gp.hist_top(k);
        assert_eq!(gp.hist_at_y(top), k, "row {k} starts at {top}");
        assert_eq!(gp.hist_at_y(top + GitPanel::row_h(r) - 1), k);
    }
    assert_eq!(gp.hist_top(14), 11 * COMMIT_H + 3 * CFILE_H);
    assert_eq!(gp.content_h(), 100_000 * COMMIT_H + 3 * CFILE_H + 16);
    // The generic accessors follow the History tab.
    assert_eq!(gp.row_count(), 100_003);
    assert_eq!(gp.row_at(12), Some(Row::CFile(1)));
    assert_eq!(gp.index_of(Row::Commit(11)), Some(14));
    gp.cursor = 10;
    gp.move_cursor(1);
    assert_eq!(gp.cursor, 11);
}

/// A repository with `n` commits on main, made in one go with `git fast-import`.
fn big_repo(tag: &str, n: usize) -> PathBuf {
    use std::io::Write;
    let dir = tmp_repo(tag);
    let mut stream = String::new();
    for i in 1..=n {
        let msg = format!("commit {i}");
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter Ana <t@t> {} +0000\ndata {}\n{msg}\n",
            1_700_000_000 + i,
            msg.len()
        ));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["fast-import", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    dir
}

#[test]
fn history_streams_in_pages_as_the_view_asks() {
    let dir = big_repo("pages", 1000);
    let mut gp = GitPanel::new(&dir);
    gp.tab = Tab::History;
    gp.load_more();
    gp.wait_history(20);
    assert_eq!(gp.commits.len(), FIRST_PAGE, "only the first page is read");
    assert!(!gp.log_done);
    assert_eq!(gp.graph.rows.len(), gp.commits.len(), "every commit has its graph row");
    assert_eq!(gp.commits[0].subject, "commit 1000");
    assert!(gp.is_head(0) && !gp.is_head(1));
    gp.load_more();
    gp.load_more(); // a second request while one is pending is ignored
    gp.wait_history(20);
    assert_eq!(gp.commits.len(), FIRST_PAGE + NEXT_PAGE);
    gp.load_more();
    gp.wait_history(20);
    assert_eq!(gp.commits.len(), 1000);
    assert!(gp.log_done && gp.loader.is_none(), "the end is known and the worker is gone");
    assert_eq!(gp.commits[999].subject, "commit 1");
    assert!(gp.graph.rows.iter().all(|r| r.col == 0 && r.width == 1));
    // A reset drops the worker and starts over.
    gp.reset_history();
    assert!(gp.commits.is_empty() && gp.loader.is_some());
    gp.wait_history(20);
    assert_eq!(gp.commits.len(), FIRST_PAGE);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn branches_merges_and_tags_reach_the_history_graph() {
    let dir = tmp_repo("merge");
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    commit_all(&dir, "base");
    git_run(&dir, &["switch", "-q", "-c", "feature"]).unwrap();
    std::fs::write(dir.join("f.txt"), "f\n").unwrap();
    commit_all(&dir, "feature work");
    git_run(&dir, &["switch", "-q", "main"]).unwrap();
    std::fs::write(dir.join("a.txt"), "2\n").unwrap();
    commit_all(&dir, "main work");
    git_run(&dir, &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]).unwrap();
    git_run(&dir, &["tag", "v1"]).unwrap();
    git_run(&dir, &["switch", "-q", "-c", "side", "HEAD~1"]).unwrap();
    std::fs::write(dir.join("s.txt"), "s\n").unwrap();
    commit_all(&dir, "side work");
    git_run(&dir, &["switch", "-q", "main"]).unwrap();
    let mut gp = GitPanel::new(&dir);
    gp.tab = Tab::History;
    gp.load_more();
    gp.wait_history(10);
    assert!(gp.log_done);
    let subjects: Vec<&str> = gp.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects.len(), 5, "every branch is in the graph: {subjects:?}");
    let at = |s: &str| subjects.iter().position(|x| *x == s).unwrap();
    let m = at("merge feature");
    assert!(gp.graph.rows[m].merge);
    assert!(gp.graph.rows.iter().map(|r| r.width).max().unwrap() >= 2);
    let kinds: Vec<(ChipKind, String)> = gp.chips_of(m).iter().map(|c| (c.kind, c.name.clone())).collect();
    assert!(kinds.contains(&(ChipKind::Current, "main".into())) && kinds.contains(&(ChipKind::Tag, "v1".into())));
    assert!(gp.is_head(m));
    assert_eq!(gp.chips_of(at("feature work"))[0], Chip { kind: ChipKind::Branch, name: "feature".into() });
    assert_eq!(gp.chips_of(at("side work"))[0].name, "side");
    // Children come before their parents.
    assert!(at("merge feature") < at("main work") && at("feature work") < at("base"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn revert_and_checkout_from_the_commit_menu() {
    let dir = tmp_repo("revert");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    commit_all(&dir, "one");
    std::fs::write(dir.join("a.txt"), "two\n").unwrap();
    commit_all(&dir, "two");
    let two = rev(&dir, "HEAD");
    let line = revert_commit(&dir, &two).unwrap();
    assert!(line.contains("Revert"), "{line}");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    git_run(&dir, &["switch", "-q", "-c", "other", "HEAD~2"]).unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    commit_all(&dir, "other");
    let other = rev(&dir, "HEAD");
    git_run(&dir, &["switch", "-q", "main"]).unwrap();
    assert!(matches!(revert_commit(&dir, &other), Err(CherryFail::Other(e)) if e.contains("not part")));
    assert_eq!(switch_branch(&dir, "other").unwrap(), "Switched to “other”");
    assert_eq!(current_branch(&dir).as_deref(), Some("other"));
    assert!(checkout_detached(&dir, &two).unwrap().contains("detached"));
    assert_eq!(current_branch(&dir), None);
    // The menu offers them.
    let c = CommitRef { hash: two.clone(), head: true, branches: vec![] };
    let labels: Vec<&str> =
        crate::app_ctxmenu::entries_for(&crate::app_ctxmenu::Target::Commit(c), &crate::app_ctxmenu::Flags::default())
            .iter()
            .filter_map(|e| match e {
                crate::app_ctxmenu::Entry::Item { label, .. } => Some(*label),
                _ => None,
            })
            .collect();
    for want in ["Checkout", "Create Branch Here…", "Cherry-pick", "Revert", "Copy Hash", "Undo Last Commit"] {
        assert!(labels.contains(&want), "{want} in {labels:?}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

// ───────────── stashes ─────────────

#[test]
fn stash_list_parsing() {
    let out = "aaaa\x1fstash@{0}\x1f1700000000\x1fOn main: fix: a: b\nbbbb\x1fstash@{1}\x1f1690000000\x1fWIP on feat: 1234567 subject\n";
    let v = parse_stash_list(out);
    assert_eq!(v.len(), 2);
    assert_eq!((v[0].index, v[0].branch.as_str(), v[0].message.as_str()), (0, "main", "fix: a: b"));
    assert_eq!((v[1].index, v[1].branch.as_str(), v[1].message.as_str()), (1, "feat", "1234567 subject"));
    assert_eq!(v[1].name(), "stash@{1}");
    assert_eq!(split_stash_subject("custom"), (String::new(), "custom".to_string()));
    assert_eq!(
        stash_push_args("  msg ", &["a b.txt".into()], true),
        ["stash", "push", "--include-untracked", "-m", "msg", "--", "a b.txt"]
    );
    assert_eq!(stash_push_args("", &[], false), ["stash", "push"]);
}

#[test]
fn stash_push_list_show_and_drop() {
    let dir = tmp_repo("stash");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.join("b.txt"), "bee\n").unwrap();
    commit_all(&dir, "init");
    std::fs::write(dir.join("a.txt"), "two\n").unwrap();
    std::fs::write(dir.join("u.txt"), "u\n").unwrap();
    // Tracked changes only: the untracked file stays.
    assert_eq!(stash_push(&dir, "wip", &[], false).unwrap(), "Stashed all changes: “wip”");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    assert!(dir.join("u.txt").exists());
    // Untracked included.
    stash_push(&dir, "", &[], true).unwrap();
    assert!(!dir.join("u.txt").exists());
    let v = list_stashes(&dir);
    assert_eq!(v.len(), 2);
    assert_eq!((v[1].message.as_str(), v[1].branch.as_str(), v[1].index), ("wip", "main", 1));
    assert!(v[0].ts > 1_600_000_000);
    let files = stash_files(&dir, &v[0].sha);
    assert_eq!(files, vec![CommitFile { code: '?', path: "u.txt".into(), old: None }]);
    assert_eq!(stash_pair(&dir, &v[0].sha, &files[0]), (String::new(), "u\n".to_string()));
    let files = stash_files(&dir, &v[1].sha);
    assert_eq!(files, vec![CommitFile { code: 'M', path: "a.txt".into(), old: None }]);
    assert_eq!(stash_pair(&dir, &v[1].sha, &files[0]), ("one\n".to_string(), "two\n".to_string()));
    assert_eq!(stash_push(&dir, "nothing", &[], false).unwrap_err(), "No local changes to save");
    // Only the selected files.
    std::fs::write(dir.join("a.txt"), "three\n").unwrap();
    std::fs::write(dir.join("b.txt"), "bus\n").unwrap();
    assert_eq!(stash_push(&dir, "only a", &["a.txt".into()], false).unwrap(), "Stashed a.txt: “only a”");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "bus\n", "the other file is untouched");
    let v = list_stashes(&dir);
    assert_eq!(v.len(), 3);
    assert_eq!(stash_ref(&dir, &v[2].sha).as_deref(), Some("stash@{2}"), "found by hash");
    // Dropping by hash finds the current index even after the list moved.
    assert_eq!(stash_drop(&dir, &v[0].sha).unwrap(), "Dropped stash@{0}");
    assert_eq!(stash_drop(&dir, &v[2].sha).unwrap(), "Dropped stash@{1}");
    let left = list_stashes(&dir);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].sha, v[1].sha);
    assert!(stash_drop(&dir, &v[0].sha).is_err(), "already gone");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stash_apply_pop_and_conflicts() {
    let dir = tmp_repo("stashapply");
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    commit_all(&dir, "init");
    std::fs::write(dir.join("a.txt"), "mine\n").unwrap();
    stash_push(&dir, "mine", &[], false).unwrap();
    let sha = list_stashes(&dir)[0].sha.clone();
    assert_eq!(stash_apply(&dir, &sha, false).unwrap(), "Applied stash@{0}");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "mine\n");
    assert_eq!(list_stashes(&dir).len(), 1, "apply keeps the stash");
    git_run(&dir, &["checkout", "--", "a.txt"]).unwrap();
    assert_eq!(stash_apply(&dir, &sha, true).unwrap(), "Popped stash@{0}");
    assert!(list_stashes(&dir).is_empty(), "pop drops it");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "mine\n");
    // A stash that clashes with a newer commit: conflicts, and the stash is kept.
    stash_push(&dir, "mine again", &[], false).unwrap();
    let sha = list_stashes(&dir)[0].sha.clone();
    std::fs::write(dir.join("a.txt"), "theirs\n").unwrap();
    commit_all(&dir, "theirs");
    assert_eq!(stash_apply(&dir, &sha, true), Err(StashFail::Conflict(vec!["a.txt".into()])));
    assert_eq!(list_stashes(&dir).len(), 1, "pop keeps the stash when it conflicts");
    let text = std::fs::read_to_string(dir.join("a.txt")).unwrap();
    assert_eq!(crate::gitx::parse_conflicts(&text).len(), 1, "the editor's conflict blocks apply: {text}");
    assert_eq!(unmerged_paths(&dir), ["a.txt"]);
    assert!(matches!(stash_apply(&dir, &sha, false), Err(StashFail::Other(e)) if e.contains("resolve")));
    assert!(matches!(stash_apply(&dir, "0000000", false), Err(StashFail::Other(_))));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ctrl_and_shift_click_select_files() {
    let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gg-no-such-dir"));
    gp.changes = ["a", "b", "c", "d"]
        .iter()
        .map(|p| Change { path: p.to_string(), old: None, code: 'M', group: Group::Changes })
        .collect();
    gp.select_change(0, true, false);
    gp.select_change(2, false, true);
    assert_eq!(gp.selected.iter().map(String::as_str).collect::<Vec<_>>(), ["a", "b", "c"]);
    gp.select_change(1, true, false);
    assert_eq!(gp.selected.iter().map(String::as_str).collect::<Vec<_>>(), ["a", "c"]);
    gp.select_change(3, false, true);
    assert_eq!(gp.selected.iter().map(String::as_str).collect::<Vec<_>>(), ["b", "c", "d"], "from the anchor (b)");
}

#[test]
fn stash_from_the_app_and_conflicts_open_the_file() {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    let _g = lock();
    let dir = tmp("gpstash");
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "Ana"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git_run(&dir, a).unwrap();
    }
    std::fs::write(dir.join(".gitignore"), "seed.txt\n.config/\n").unwrap();
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    commit_all(&dir, "init");
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.open_git_panel();
    std::fs::write(dir.join("a.txt"), "mine\n").unwrap();
    std::fs::write(dir.join("u.txt"), "new\n").unwrap();
    app.gitpanel.as_mut().unwrap().refresh();
    // Palette "Git: Stash…": the message prompt.
    app.git_stash_cmd(false);
    assert!(matches!(
        &app.overlay,
        crate::app_overlay::Overlay::Prompt(p) if matches!(p.kind, crate::app_overlay::PromptKind::Git(GitAsk::StashMessage { .. }))
    ));
    app.overlay = crate::app_overlay::Overlay::None;
    // An untracked file is involved: the panel asks whether to include it.
    app.git_prompt_answer(GitAsk::StashMessage { paths: vec![], untracked: None }, "wip".into());
    let ask = app.gitpanel.as_ref().unwrap().ask.clone().expect("the include-untracked question");
    assert_eq!(ask.buttons[0].label, "Include untracked");
    app.git_act(ask.buttons[0].act.clone());
    assert!(!dir.join("u.txt").exists());
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "base\n");
    {
        let gp = app.gitpanel.as_ref().unwrap();
        assert_eq!(gp.stashes.len(), 1);
        assert_eq!(gp.stashes[0].message, "wip");
        assert!(gp.changes.is_empty());
        assert_eq!(gp.rows(), vec![Row::NoChanges, Row::StashHead, Row::Stash(0)]);
    }
    // The Stashes rows draw, and the stash row's buttons are clickable.
    let (w, h) = (1200, 800);
    let mut px = vec![0u32; (w * h) as usize];
    app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
    let row = app.gitpanel.as_ref().unwrap().hits.iter().find(|(_, h)| *h == Hit::Stash(0)).map(|(r, _)| *r);
    let row = row.expect("the stash row is drawn");
    app.mouse = ((row.x + 40) as f64, (row.y + 10) as f64);
    assert!(app.gitpanel_mouse(app.mouse.0, app.mouse.1));
    {
        let gp = app.gitpanel.as_ref().unwrap();
        assert_eq!(gp.open_stash, Some(0), "a click expands it");
        assert_eq!(gp.stash_files.len(), 2, "{:?}", gp.stash_files);
        assert!(app.diff.is_some(), "and shows the first file's diff");
    }
    // A newer commit clashes with the stash: pop stops on conflicts and opens the file.
    std::fs::write(dir.join("a.txt"), "theirs\n").unwrap();
    commit_all(&dir, "theirs");
    let sha = app.gitpanel.as_ref().unwrap().stashes[0].sha.clone();
    app.git_act(GitAct::StashApply { sha, pop: true });
    let gp = app.gitpanel.as_ref().unwrap();
    assert_eq!(gp.stashes.len(), 1, "the stash is kept");
    assert!(gp.changes.iter().any(|c| c.path == "a.txt" && c.code == 'U'), "{:?}", gp.changes);
    let note = gp.sync_note.as_ref().map(|n| n.0.clone()).unwrap_or_default();
    assert!(note.contains("conflicts in a.txt"), "{note}");
    let doc = &app.docs[app.active];
    assert!(doc.path().is_some_and(|p| p.ends_with("a.txt")), "the conflicting file is opened");
    assert!(doc.buf.to_string().contains("<<<<<<<") && doc.conflicts_due.is_some());
    drop(app);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_history_tab_draws_only_the_visible_rows() {
    use crate::app_unsaved::testkit::{lock, test_app};
    let _g = lock();
    let dir = big_repo("draw", 400);
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.open_git_panel();
    let (w, h) = (1200, 800);
    let mut px = vec![0u32; (w * h) as usize];
    // The first frame takes the repository's signature (it resets the history once).
    app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
    {
        let gp = app.gitpanel.as_mut().unwrap();
        gp.tab = Tab::History;
        gp.load_more();
        gp.wait_history(20);
        assert_eq!(gp.commits.len(), FIRST_PAGE);
    }
    app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
    let gp = app.gitpanel.as_ref().unwrap();
    let drawn = gp.hits.iter().filter(|(_, h)| matches!(h, Hit::Commit(_))).count();
    assert!(drawn > 5 && drawn < 800 / COMMIT_H as usize + 3, "{drawn} commit rows drawn");
    assert!(gp.hits.iter().any(|(_, h)| *h == Hit::Commit(0)));
    // Near the end of the loaded rows the next page is requested.
    let gp = app.gitpanel.as_mut().unwrap();
    gp.scroll[1] = f64::MAX;
    app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
    let gp = app.gitpanel.as_mut().unwrap();
    gp.wait_history(20);
    assert_eq!(gp.commits.len(), 400, "the rest arrived");
    assert!(gp.log_done);
    drop(app);
    std::fs::remove_dir_all(&dir).ok();
}
