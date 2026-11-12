use super::*;

fn tmp_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-gp-{tag}-{}", std::process::id()));
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

#[test]
fn status_z_groups_and_renames() {
    let out = "M  staged.ts\0 M work.ts\0MM both.ts\0?? new.ts\0R  new name.ts\0old name.ts\0A  añadido.ts\0 D gone.ts\0UU conflict.ts\0";
    let v = parse_status_z(out);
    let find = |p: &str, g: Group| v.iter().find(|c| c.path == p && c.group == g).cloned();
    assert_eq!(find("staged.ts", Group::Staged).unwrap().code, 'M');
    assert_eq!(find("work.ts", Group::Changes).unwrap().code, 'M');
    assert_eq!(find("both.ts", Group::Staged).unwrap().code, 'M');
    assert_eq!(
        find("both.ts", Group::Changes).unwrap().code,
        'M',
        "staged and modified again is listed under Unstaged too"
    );
    assert_eq!(find("new.ts", Group::Untracked).unwrap().code, '?');
    let r = find("new name.ts", Group::Staged).unwrap();
    assert_eq!((r.code, r.old.as_deref()), ('R', Some("old name.ts")));
    assert_eq!(find("añadido.ts", Group::Staged).unwrap().code, 'A');
    assert_eq!(find("gone.ts", Group::Changes).unwrap().code, 'D');
    assert_eq!(find("conflict.ts", Group::Changes).unwrap().code, 'U');
    // Sorted by group, then path.
    let order: Vec<Group> = v.iter().map(|c| c.group).collect();
    assert!(order.windows(2).all(|w| (w[0] as u8) <= (w[1] as u8)));
    assert!(parse_status_z("").is_empty());
}

#[test]
fn log_and_name_status_parsing() {
    let out =
        "aaaa\x1faaa\x1fAna\x1f1700000000\x1ffix: a | b\nbbbb\x1fbbb\x1fBeto\x1f1699990000\x1f\nlixo sem separador";
    let v = parse_log(out);
    assert_eq!(v.len(), 2);
    assert_eq!(
        (v[0].short.as_str(), v[0].author.as_str(), v[0].ts, v[0].subject.as_str()),
        ("aaa", "Ana", 1700000000, "fix: a | b")
    );
    assert_eq!(v[1].subject, "");
    let f = parse_name_status("M\ta.ts\nA\tdir/b.ts\nD\tc.ts\nR087\told.ts\tnew.ts\n");
    assert_eq!(f.len(), 4);
    assert_eq!((f[1].code, f[1].path.as_str()), ('A', "dir/b.ts"));
    assert_eq!((f[3].code, f[3].path.as_str(), f[3].old.as_deref()), ('R', "new.ts", Some("old.ts")));
}

#[test]
fn stage_unstage_commit_discard_in_a_real_repo() {
    let dir = tmp_repo("flow");
    std::fs::write(dir.join("a.ts"), "one\n").unwrap();
    stage(&dir, &["a.ts".into()]).unwrap();
    commit(&dir, "primeiro").unwrap();
    assert!(commit(&dir, "   ").is_err());
    assert_eq!(commit(&dir, "vazio").unwrap_err().is_empty(), false, "nothing to commit explains itself");

    std::fs::write(dir.join("a.ts"), "two\n").unwrap();
    std::fs::write(dir.join("n.ts"), "new\n").unwrap();
    let st = |d: &Path| parse_status_z(&git_run(d, &["status", "--porcelain=v1", "-z", "-uall"]).unwrap());
    let v = st(&dir);
    assert!(v.iter().any(|c| c.path == "a.ts" && c.group == Group::Changes));
    assert!(v.iter().any(|c| c.path == "n.ts" && c.group == Group::Untracked));

    stage(&dir, &["a.ts".into(), "n.ts".into()]).unwrap();
    let v = st(&dir);
    assert!(v.iter().all(|c| c.group == Group::Staged), "{v:?}");
    let (old, new) = working_pair(&dir, v.iter().find(|c| c.path == "a.ts").unwrap());
    assert_eq!((old.as_str(), new.as_str()), ("one\n", "two\n"));
    let (old, new) = working_pair(&dir, v.iter().find(|c| c.path == "n.ts").unwrap());
    assert_eq!((old.as_str(), new.as_str()), ("", "new\n"));

    unstage(&dir, &["n.ts".into()]).unwrap();
    let v = st(&dir);
    assert!(v.iter().any(|c| c.path == "n.ts" && c.group == Group::Untracked));
    assert_eq!(discard(&dir, v.iter().find(|c| c.group == Group::Staged).unwrap()).is_err(), true);
    discard(&dir, v.iter().find(|c| c.path == "n.ts").unwrap()).unwrap();
    assert!(!dir.join("n.ts").exists());

    let line = commit(&dir, "segundo\n\ncorpo do commit").unwrap();
    assert!(line.contains("segundo"), "{line}");
    assert!(st(&dir).is_empty());
    let log = log_page(&dir, 0, 10);
    assert_eq!(log.iter().map(|l| l.subject.as_str()).collect::<Vec<_>>(), ["segundo", "primeiro"]);

    std::fs::write(dir.join("a.ts"), "three\n").unwrap();
    let v = st(&dir);
    discard(&dir, &v[0]).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), "two\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unstage_in_a_repo_without_commits() {
    let dir = tmp_repo("empty");
    std::fs::write(dir.join("a.ts"), "x\n").unwrap();
    stage(&dir, &["a.ts".into()]).unwrap();
    assert_eq!(parse_status_z(&git_run(&dir, &["status", "--porcelain=v1", "-z"]).unwrap())[0].group, Group::Staged);
    unstage(&dir, &["a.ts".into()]).unwrap();
    assert_eq!(parse_status_z(&git_run(&dir, &["status", "--porcelain=v1", "-z"]).unwrap())[0].group, Group::Untracked);
    assert!(log_page(&dir, 0, 5).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn log_pages_and_commit_files() {
    let dir = tmp_repo("log");
    for i in 0..7 {
        std::fs::write(dir.join("f.ts"), format!("v{i}\n")).unwrap();
        if i == 3 {
            std::fs::write(dir.join("old.ts"), "keep\nme\nplease\nthis\nis\nbig\nenough\n").unwrap();
        }
        git_run(&dir, &["add", "-A"]).unwrap();
        commit(&dir, &format!("c{i}")).unwrap();
    }
    let p1 = log_page(&dir, 0, 3);
    let p2 = log_page(&dir, 3, 3);
    let p3 = log_page(&dir, 6, 3);
    let p4 = log_page(&dir, 9, 3);
    assert_eq!((p1.len(), p2.len(), p3.len(), p4.len()), (3, 3, 1, 0));
    assert_eq!(p1[0].subject, "c6");
    assert_eq!(p3[0].subject, "c0");
    assert_eq!(p1[0].author, "Ana");
    assert!(p1[0].ts > 1_600_000_000);
    // Root commit lists its files against the empty tree.
    let f = commit_files(&dir, &p3[0].hash);
    assert_eq!(f, vec![CommitFile { code: 'A', path: "f.ts".into(), old: None }]);
    let (old, new) = commit_pair(&dir, &p3[0].hash, &f[0]);
    assert_eq!((old.as_str(), new.as_str()), ("", "v0\n"));
    // Regular commit: before / after.
    let f = commit_files(&dir, &p1[0].hash);
    let (old, new) = commit_pair(&dir, &p1[0].hash, &f[0]);
    assert_eq!((old.as_str(), new.as_str()), ("v5\n", "v6\n"));
    // Rename is followed.
    git_run(&dir, &["mv", "old.ts", "renamed.ts"]).unwrap();
    commit(&dir, "rename").unwrap();
    let head = &log_page(&dir, 0, 1)[0];
    let f = commit_files(&dir, &head.hash);
    assert_eq!((f[0].code, f[0].path.as_str(), f[0].old.as_deref()), ('R', "renamed.ts", Some("old.ts")));
    let (old, new) = commit_pair(&dir, &head.hash, &f[0]);
    assert_eq!(old, new);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn panel_rows_cursor_and_state() {
    let dir = tmp_repo("state");
    std::fs::write(dir.join("a.ts"), "1\n").unwrap();
    git_run(&dir, &["add", "-A"]).unwrap();
    commit(&dir, "init").unwrap();
    std::fs::write(dir.join("a.ts"), "2\n").unwrap();
    std::fs::write(dir.join("b.ts"), "x\n").unwrap();
    git_run(&dir, &["add", "b.ts"]).unwrap();
    std::fs::write(dir.join("c.ts"), "x\n").unwrap();
    let mut gp = GitPanel::new(&dir);
    assert!(gp.top.is_some());
    let rows = gp.rows();
    assert_eq!(rows.len(), 7, "3 headers + 3 files + the Stashes header: {rows:?}");
    assert!(matches!(rows[0], Row::Header(Group::Staged)));
    assert_eq!(rows[6], Row::StashHead, "offers to stash even before there are stashes");
    assert_eq!(gp.staged(), 1);
    assert_eq!(gp.group_paths(Group::Untracked), vec!["c.ts".to_string()]);
    // The cursor skips headers.
    gp.cursor = 1;
    gp.move_cursor(1);
    assert_eq!(gp.cursor, 3);
    gp.move_cursor(-1);
    assert_eq!(gp.cursor, 1);
    gp.move_cursor(-1);
    assert_eq!(gp.cursor, 1, "does not land on a header");
    // History: loads in pages and tracks the end.
    gp.tab = Tab::History;
    gp.load_more();
    gp.wait_history(10);
    assert_eq!(gp.commits.len(), 1);
    assert!(gp.log_done, "a short page is the last one");
    assert!(gp.is_head(0), "the HEAD label comes with the commit");
    gp.open_commit = Some(0);
    gp.files = commit_files(gp.top.as_ref().unwrap(), &gp.commits[0].hash);
    assert_eq!(gp.rows(), vec![Row::Commit(0), Row::CFile(0)]);
    gp.reset_history();
    assert!(gp.rows().is_empty(), "reset forgets the loaded history");
    gp.wait_history(10);
    assert_eq!(gp.rows().len(), 1, "reset reloads the first page and closes the expansion");
    // Not a repository.
    let plain = std::env::temp_dir().join(format!("lumen-gp-plain-{}", std::process::id()));
    std::fs::create_dir_all(&plain).unwrap();
    let g2 = GitPanel::new(&plain);
    assert!(g2.top.is_none() && g2.rows().is_empty());
    std::fs::remove_dir_all(&plain).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fit_helpers() {
    assert_eq!(split_name("a/b/c.ts"), ("c.ts", "a/b"));
    assert_eq!(split_name("c.ts"), ("c.ts", ""));
}

#[test]
fn staged_and_modified_is_listed_under_both_groups() {
    let v = parse_status_z("MM both.ts\0AD gone.ts\0M  staged.ts\0 M work.ts\0");
    assert_eq!(v.len(), 6, "a staged+modified file has two entries: {v:?}");
    let of = |p: &str, g: Group| v.iter().find(|c| c.path == p && c.group == g).cloned();
    let staged = of("both.ts", Group::Staged).unwrap();
    assert_eq!((staged.code, staged.check_state(), staged.discardable()), ('M', CheckState::On, false));
    let work = of("both.ts", Group::Changes).unwrap();
    assert_eq!((work.code, work.check_state(), work.discardable()), ('M', CheckState::Off, true));
    assert_eq!(of("gone.ts", Group::Staged).unwrap().code, 'A');
    assert_eq!(of("gone.ts", Group::Changes).unwrap().code, 'D');
    // Plain staged / plain modified files stay in one group only.
    assert!(of("staged.ts", Group::Changes).is_none() && of("work.ts", Group::Staged).is_none());
}

#[test]
fn staged_plus_modified_in_a_real_repo() {
    let dir = tmp_repo("both");
    std::fs::write(dir.join("a.ts"), "1\n").unwrap();
    git_run(&dir, &["add", "-A"]).unwrap();
    commit(&dir, "init").unwrap();
    std::fs::write(dir.join("a.ts"), "2\n").unwrap();
    git_run(&dir, &["add", "a.ts"]).unwrap();
    std::fs::write(dir.join("a.ts"), "3\n").unwrap();
    let mut gp = GitPanel::new(&dir);
    assert_eq!(gp.changes.len(), 2);
    assert_eq!(
        gp.rows(),
        vec![Row::Header(Group::Staged), Row::File(0), Row::Header(Group::Changes), Row::File(1), Row::StashHead],
        "one row under each group"
    );
    assert_eq!((gp.changes[0].group, gp.changes[1].group), (Group::Staged, Group::Changes));
    // "Add all" (the Unstaged section) stages the edit made after the first `add`.
    let paths = gp.group_paths(Group::Changes);
    assert_eq!(paths, vec!["a.ts".to_string()]);
    // Discard (Unstaged row) throws away the unstaged edit only; the staged version survives.
    discard(&dir, &gp.changes[1].clone()).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), "2\n");
    gp.refresh();
    assert_eq!(gp.changes.len(), 1);
    assert_eq!(gp.changes[0].check_state(), CheckState::On);
    assert!(discard(&dir, &gp.changes[0].clone()).is_err(), "fully staged: nothing to discard");
    // Edit again, then stage every path of the Unstaged section: one Staged entry is left.
    std::fs::write(dir.join("a.ts"), "4\n").unwrap();
    gp.refresh();
    assert_eq!(gp.changes.len(), 2);
    stage(&dir, &gp.group_paths(Group::Changes)).unwrap();
    gp.refresh();
    assert_eq!(gp.changes.len(), 1);
    assert_eq!(gp.changes[0].group, Group::Staged);
    assert_eq!(git_run(&dir, &["show", ":a.ts"]).unwrap(), "4\n", "the index got the latest edit");
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn a_command_left_running_by_a_closed_panel_shows_a_spinner_in_the_bottom_bar() {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use std::os::unix::fs::PermissionsExt;
    let _g = lock();
    let dir = tmp("gpbg");
    // A repository whose remote is reached through a "ssh" that never answers (until cancelled).
    let slow = dir.join("slow-ssh.sh");
    std::fs::write(&slow, "#!/bin/sh\nsleep 30\nexit 1\n").unwrap();
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["remote", "add", "origin", "ssh://localhost/nothing"],
        &["config", "core.sshCommand", slow.to_str().unwrap()],
    ] {
        git_run(&work, a).unwrap();
    }
    let mut app = test_app(&work);
    let mut gp = Box::new(GitPanel::new(&work));
    let top = gp.top.clone().expect("a repository");
    gp.job = Some(gitsync::spawn(&top, Op::Fetch, app.wake()));
    app.gitpanel = Some(gp);
    // Closing the panel hands the running command over...
    app.close_git_panel();
    assert!(app.gitpanel.is_none() && app.gitbg.detached.is_some());
    // ...the event loop keeps waking up for the spinner...
    let (now, mut due) = (Instant::now(), Vec::new());
    app.git_bg_tick(now, &mut |t| due.push(t));
    assert!(due.iter().any(|t| *t <= now + Duration::from_millis(200)), "wakes up often enough to turn the spinner");
    // ...and the bottom bar draws it (nothing when no command runs or there is no room).
    let (w, h) = (500, 28);
    let mut px = vec![0u32; (w * h) as usize];
    let bar = Rect::new(0, 0, w, h);
    let used = app.draw_git_bg(&mut Canvas { px: &mut px, w, h }, 10, bar, 400);
    assert!(used > 60 && used < 200, "spinner + 'Buscando… 0s': {used}px");
    assert!(px.iter().any(|p| *p != 0), "something was painted");
    let mut blank = vec![0u32; (w * h) as usize];
    assert_eq!(app.draw_git_bg(&mut Canvas { px: &mut blank, w, h }, 10, bar, 30), 0, "no room, no spinner");
    assert!(blank.iter().all(|p| *p == 0));
    // Reopening the panel takes the command back (and the bar goes quiet).
    app.open_git_panel();
    assert!(app.gitbg.detached.is_none() && app.gitpanel.as_ref().is_some_and(|g| g.job.is_some()));
    assert_eq!(app.draw_git_bg(&mut Canvas { px: &mut blank, w, h }, 10, bar, 400), 0);
    // Close it again, cancel the command: the result is reported and the spinner is gone.
    app.close_git_panel();
    app.gitbg.detached.as_mut().unwrap().cancel();
    let t0 = Instant::now();
    while app.gitbg.detached.is_some() && t0.elapsed() < Duration::from_secs(20) {
        app.git_bg_tick(Instant::now(), &mut |_| {});
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(app.gitbg.detached.is_none(), "the finished command is collected");
    let msg = app.status.as_ref().map(|s| s.0.clone()).unwrap_or_default();
    assert!(msg.starts_with("Fetch"), "the result is reported in the bottom bar: {msg:?}");
    assert_eq!(app.draw_git_bg(&mut Canvas { px: &mut blank, w, h }, 10, bar, 400), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn panel_width_clamps() {
    assert_eq!(clamp_panel_width(500, 1400), 500);
    assert_eq!(clamp_panel_width(100, 1400), 280, "not narrower than the minimum");
    assert_eq!(clamp_panel_width(5000, 1400), 840, "at most 60% of the window");
    assert_eq!(clamp_panel_width(300, 400), 240, "tiny window: 60% of it");
    assert_eq!(clamp_panel_width(10, 400), 200, "tiny window: the minimum shrinks to half");
    assert!(clamp_panel_width(0, 100) >= 1);
    assert_eq!(auto_panel_width(1000), 340);
    assert_eq!(auto_panel_width(2000), 440);
}

#[test]
fn expanded_commit_files_are_scrolled_into_view() {
    let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gp-no-such-dir"));
    gp.tab = Tab::History;
    gp.commits = (0..20)
        .map(|i| LogEntry {
            hash: format!("h{i}"),
            short: format!("s{i}"),
            author: "a".into(),
            ts: 0,
            subject: "x".into(),
        })
        .collect();
    gp.files = (0..4).map(|i| CommitFile { code: 'M', path: format!("f{i}.ts"), old: None }).collect();
    gp.open_commit = Some(10);
    let k = gp.rows().iter().position(|r| *r == Row::Commit(10)).unwrap();
    assert_eq!(k, 10);
    // Viewport 300px, scrolled to the top: commit 10 starts at 520.
    gp.reveal_span(k, k + 4, 300);
    let (top, bottom) = (10 * COMMIT_H, 11 * COMMIT_H + 4 * CFILE_H);
    assert_eq!(gp.scroll[1], (bottom - 300) as f64, "the last file's bottom edge is visible");
    assert!(gp.scroll[1] <= top as f64, "and the commit row itself too");
    // Already visible: untouched.
    let s = gp.scroll[1];
    gp.reveal_span(k, k + 4, 300);
    assert_eq!(gp.scroll[1], s);
    // Scrolled past it: back up to the commit row.
    gp.scroll[1] = 2000.0;
    gp.reveal_span(k, k + 4, 300);
    assert_eq!(gp.scroll[1], top as f64);
    // A span taller than the viewport shows the commit row first.
    gp.scroll[1] = 0.0;
    gp.reveal_span(k, k + 4, 100);
    assert_eq!(gp.scroll[1], top as f64);
}

#[test]
fn strip_and_chrome_heights_follow_the_state() {
    let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gp-no-such-dir"));
    assert_eq!(gp.strip_h(), 0);
    assert_eq!(gp.chrome_h(), HEAD_H + BRANCH_H);
    gp.sync_note = Some(("boom".into(), false, Instant::now()));
    gp.note_lines = 2;
    assert_eq!(gp.strip_h(), 2 * STRIP_LINE + 10, "errors stay and grow the strip with their lines");
    gp.sync_note = Some(("ok".into(), true, Instant::now() - Duration::from_secs(30)));
    assert_eq!(gp.strip_h(), 0, "old successes fade out");
    gp.publish = Some(("origin".into(), "main".into()));
    assert_eq!(gp.strip_h(), PROMPT_H);
    assert_eq!(gp.chrome_h(), HEAD_H + BRANCH_H + PROMPT_H);
}

#[test]
fn panel_knows_ahead_and_behind_of_a_real_clone() {
    let base = tmp_repo("syncpanel");
    let (remote, work) = (base.join("remote.git"), base.join("work"));
    git_run(&base, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]).unwrap();
    git_run(&base, &["init", "-q", "-b", "main", work.to_str().unwrap()]).unwrap();
    for a in
        [&["config", "user.email", "t@t"][..], &["config", "user.name", "T"], &["config", "commit.gpgsign", "false"]]
    {
        git_run(&work, a).unwrap();
    }
    git_run(&work, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
    std::fs::write(work.join("a.txt"), "1\n").unwrap();
    git_run(&work, &["add", "."]).unwrap();
    git_run(&work, &["commit", "-q", "-m", "one"]).unwrap();
    let gp = GitPanel::new(&work);
    assert_eq!(gp.sync, SyncInfo::default(), "no upstream: counts are hidden and Push asks to publish");
    git_run(&work, &["push", "-q", "-u", "origin", "main"]).unwrap();
    std::fs::write(work.join("a.txt"), "2\n").unwrap();
    git_run(&work, &["commit", "-qam", "two"]).unwrap();
    let gp = GitPanel::new(&work);
    assert_eq!((gp.sync.upstream.as_deref(), gp.sync.ahead, gp.sync.behind), (Some("origin/main"), 1, 0));
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn button_rows_wrap_from_the_left() {
    assert_eq!(button_rows(&[60, 60, 60], 300, 4), [3]);
    assert_eq!(button_rows(&[100, 100, 100, 90], 300, 4), [2, 2]);
    assert_eq!(button_rows(&[290, 20], 300, 4), [1, 1]);
    assert_eq!(button_rows(&[400], 300, 4), [1], "a button wider than the row still gets its own");
    assert!(button_rows(&[], 300, 4).is_empty());
}

#[test]
fn force_and_strategy_cards_take_over_the_strip() {
    let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gp-no-such-dir"));
    assert_eq!(gp.strip_h(), 0);
    gp.force = Some(ForceWhy::Rejected);
    gp.prompt_h = 120;
    assert_eq!(gp.strip_h(), 120);
    gp.force = None;
    gp.strategy_ask = true;
    assert_eq!(gp.strip_h(), 120);
    gp.prompt_h = 10;
    assert_eq!(gp.strip_h(), PROMPT_H, "never smaller than the one-line prompt");
    gp.strategy_ask = false;
    assert_eq!(gp.strip_h(), 0);
    assert!(ForceWhy::Rejected.text().contains("--force-with-lease") && ForceWhy::Amended.text().contains("amended"));
    let confirm = GitPanel::gp_card_buttons_for(true);
    assert_eq!(confirm.iter().map(|b| b.1).collect::<Vec<_>>(), [Hit::ForceYes, Hit::ForceNo]);
    let strat = GitPanel::gp_card_buttons_for(false);
    assert_eq!(strat.len(), 4);
    assert!(matches!(strat[1].1, Hit::Strategy(PullStrategy::Rebase)) && strat[3].1 == Hit::StrategyNo);
}

#[test]
fn modifier_keys_are_recognised() {
    assert!(is_modifier_key(&Key::Named(NamedKey::Control)));
    assert!(is_modifier_key(&Key::Named(NamedKey::Shift)));
    assert!(!is_modifier_key(&Key::Named(NamedKey::Enter)));
    assert!(!is_modifier_key(&Key::Character("a".into())));
}

// ───────────── branches, tags, cherry-pick, undo, releases ─────────────

fn commit_file(dir: &Path, name: &str, text: &str, msg: &str) {
    std::fs::write(dir.join(name), text).unwrap();
    git_run(dir, &["add", "-A"]).unwrap();
    git_run(dir, &["commit", "-q", "-m", msg]).unwrap();
}

fn head(dir: &Path) -> String {
    git_run(dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
}

#[test]
fn branches_are_created_switched_to_and_deleted() {
    let d = tmp_repo("branches");
    commit_file(&d, "a.txt", "one\n", "first");
    let first = head(&d);
    assert!(check_branch_name(&d, "   ").is_err());
    assert!(check_branch_name(&d, "bad..name").is_err());
    assert!(check_branch_name(&d, "-x").is_err());
    assert!(check_branch_name(&d, "main").unwrap_err().contains("already exists"));
    assert_eq!(check_branch_name(&d, " feat/x ").unwrap(), "feat/x");

    let m = create_branch(&d, "feat/x", None).unwrap();
    assert!(m.contains("feat/x"), "{m}");
    assert_eq!(current_branch(&d).as_deref(), Some("feat/x"));
    assert!(
        matches!(delete_branch(&d, "feat/x", false), Err(DeleteFail::Other(e)) if e.contains("checked out")),
        "the checked-out branch is never deleted"
    );
    commit_file(&d, "b.txt", "b\n", "only on feat");
    git_run(&d, &["switch", "-q", "main"]).unwrap();
    assert_eq!(local_branches(&d).len(), 2);

    // Unmerged work: -d refuses (the UI then offers -D), -D deletes.
    assert_eq!(delete_branch(&d, "feat/x", false), Err(DeleteFail::Unmerged));
    assert!(local_branches(&d).contains(&"feat/x".to_string()), "still there after the refusal");
    delete_branch(&d, "feat/x", true).unwrap();
    assert_eq!(local_branches(&d), ["main"]);

    // From an older commit (History "Create branch here"): merged, so -d is enough.
    commit_file(&d, "c.txt", "c\n", "second");
    create_branch(&d, "old", Some(first.as_str())).unwrap();
    assert_eq!(head(&d), first);
    assert_eq!(current_branch(&d).as_deref(), Some("old"));
    git_run(&d, &["switch", "-q", "main"]).unwrap();
    delete_branch(&d, "old", false).unwrap();
    assert!(matches!(delete_branch(&d, "ghost", false), Err(DeleteFail::Other(_))));

    // Upstream of a branch (the "delete on the remote too" choice).
    assert_eq!(upstream_of(&d, "main"), None);
    git_run(&d, &["config", "branch.main.remote", "origin"]).unwrap();
    git_run(&d, &["config", "branch.main.merge", "refs/heads/trunk"]).unwrap();
    assert_eq!(upstream_of(&d, "main"), Some(("origin".into(), "trunk".into())));
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn tags_are_created_listed_mapped_and_deleted() {
    let d = tmp_repo("tags");
    commit_file(&d, "a.txt", "one\n", "first");
    let first = head(&d);
    commit_file(&d, "a.txt", "two\n", "second");
    let second = head(&d);
    assert_eq!(tag_args("v1", None, None), ["tag", "v1"]);
    assert_eq!(tag_args("v1", Some("  "), None), ["tag", "v1"], "a blank message makes a lightweight tag");
    assert_eq!(tag_args("v1", Some("notes"), Some("abc")), ["tag", "-a", "-m", "notes", "v1", "abc"]);

    create_tag(&d, "v1.0.0", None, None).unwrap();
    assert_eq!(git_run(&d, &["cat-file", "-t", "v1.0.0"]).unwrap().trim(), "commit", "lightweight");
    let m = create_tag(&d, "v0.9.0", Some("first cut"), Some(first.as_str())).unwrap();
    assert!(m.starts_with("Annotated tag"), "{m}");
    assert_eq!(git_run(&d, &["cat-file", "-t", "v0.9.0"]).unwrap().trim(), "tag", "annotated");
    assert_eq!(git_run(&d, &["rev-parse", "v0.9.0^{commit}"]).unwrap().trim(), first);
    assert!(check_tag_name(&d, "v1.0.0").unwrap_err().contains("already exists"));
    assert!(check_tag_name(&d, "bad tag").is_err());
    assert!(check_tag_name(&d, "").is_err());
    assert!(create_tag(&d, "v1.0.0", None, None).is_err());

    let names: Vec<String> = list_tags(&d).into_iter().map(|(n, _)| n).collect();
    assert!(names.contains(&"v1.0.0".to_string()) && names.contains(&"v0.9.0".to_string()), "{names:?}");
    let map = tag_map(&d);
    assert_eq!(map.get(&second), Some(&vec!["v1.0.0".to_string()]));
    assert_eq!(map.get(&first), Some(&vec!["v0.9.0".to_string()]), "annotated tags are peeled to their commit");

    delete_tag(&d, "v1.0.0").unwrap();
    assert!(list_tags(&d).iter().all(|(n, _)| n != "v1.0.0"));
    assert!(delete_tag(&d, "v1.0.0").is_err());

    let m = parse_tag_map("aaa\t\tv1\nbbb\tccc\tv2\nlixo\n");
    assert_eq!(m.get("aaa"), Some(&vec!["v1".to_string()]));
    assert_eq!(m.get("ccc"), Some(&vec!["v2".to_string()]));
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(suggest_next_tag(&s(&["v1.2.3", "v1.2.2"])), "v1.2.4");
    assert_eq!(suggest_next_tag(&s(&["latest", "1.0.9"])), "1.0.10");
    assert_eq!(suggest_next_tag(&[]), "v0.1.0");
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn undo_last_commit_keeps_changes_staged_and_returns_the_message() {
    let d = tmp_repo("undo");
    assert!(undo_blocker(&d).unwrap().contains("no commit"));
    commit_file(&d, "a.txt", "one\n", "first");
    let why = undo_last_commit(&d).unwrap_err();
    assert!(why.contains("first commit"), "{why}");
    commit_file(&d, "b.txt", "b\n", "second\n\nwith a body");
    assert_eq!(undo_blocker(&d), None);
    let msg = undo_last_commit(&d).unwrap();
    assert_eq!(msg, "second\n\nwith a body");
    assert_eq!(git_run(&d, &["rev-list", "--count", "HEAD"]).unwrap().trim(), "1", "only the last commit");
    let st = parse_status_z(&git_run(&d, &["status", "--porcelain=v1", "-z"]).unwrap());
    assert!(st.iter().any(|c| c.path == "b.txt" && c.group == Group::Staged), "{st:?}");
    assert_eq!(std::fs::read_to_string(d.join("b.txt")).unwrap(), "b\n");

    // Published commits are detected (the UI asks before undoing them).
    let remote = d.join("remote.git");
    git_run(&d, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]).unwrap();
    std::fs::write(d.join(".git/info/exclude"), "remote.git\n").unwrap();
    git_run(&d, &["commit", "-q", "-m", "second again"]).unwrap();
    assert!(!head_published(&d), "no upstream");
    git_run(&d, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
    git_run(&d, &["push", "-q", "-u", "origin", "main"]).unwrap();
    assert!(head_published(&d));
    commit_file(&d, "c.txt", "c\n", "local only");
    assert!(!head_published(&d));
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn cherry_pick_applies_conflicts_aborts_and_continues() {
    let d = tmp_repo("cherry");
    commit_file(&d, "a.txt", "one\n", "first");
    let first = head(&d);
    git_run(&d, &["switch", "-q", "-c", "other"]).unwrap();
    commit_file(&d, "c.txt", "c\n", "add c");
    let clean = head(&d);
    commit_file(&d, "a.txt", "theirs\n", "edit a there");
    let clash = head(&d);
    git_run(&d, &["switch", "-q", "main"]).unwrap();
    commit_file(&d, "a.txt", "mine\n", "edit a here");

    let others: Vec<String> = other_commits(&d, 10).into_iter().map(|e| e.hash).collect();
    assert_eq!(others, [clash.clone(), clean.clone()], "commits of other branches, newest first");

    let line = cherry_pick(&d, &clean).unwrap();
    assert!(!line.is_empty());
    assert!(d.join("c.txt").exists());
    assert_eq!(git_run(&d, &["log", "-1", "--format=%s"]).unwrap().trim(), "add c");
    assert!(matches!(cherry_pick(&d, &first), Err(CherryFail::Other(e)) if e.contains("already")));

    // A conflict leaves the cherry-pick in progress; abort puts everything back.
    assert_eq!(cherry_pick(&d, &clash), Err(CherryFail::Conflict));
    assert_eq!(repo_state(&d), RepoState::CherryPick);
    assert!(undo_blocker(&d).unwrap().contains("cherry-pick"), "no undo in the middle of a cherry-pick");
    assert!(matches!(cherry_pick(&d, &clean), Err(CherryFail::Other(_))));
    let gp = GitPanel::new(&d);
    assert_eq!(gp.state, RepoState::CherryPick);
    assert_eq!(gp.card(), Some(cherry_card()), "the panel offers Continue / Abort");
    cherry_abort(&d).unwrap();
    assert_eq!(repo_state(&d), RepoState::Clean);
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "mine\n");

    // Resolve, stage, continue: the commit keeps its message.
    assert_eq!(cherry_pick(&d, &clash), Err(CherryFail::Conflict));
    assert!(cherry_continue(&d).is_err(), "unresolved files block the commit");
    std::fs::write(d.join("a.txt"), "merged\n").unwrap();
    git_run(&d, &["add", "a.txt"]).unwrap();
    cherry_continue(&d).unwrap();
    assert_eq!(repo_state(&d), RepoState::Clean);
    assert_eq!(git_run(&d, &["log", "-1", "--format=%s"]).unwrap().trim(), "edit a there");
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn release_needs_gh_and_a_github_remote() {
    assert_eq!(gh_status("lumen-no-such-gh-program"), GhStatus::Missing);
    assert!(GhStatus::Missing.text().contains("cli.github.com"));
    assert!(GhStatus::NoAuth.text().contains("gh auth login"));
    for (url, want) in [
        ("git@github.com:ana/lumen.git", Some("ana/lumen")),
        ("https://github.com/ana/lumen", Some("ana/lumen")),
        ("https://github.com/ana/lumen.git/", Some("ana/lumen")),
        ("ssh://git@github.com/ana/lumen.git", Some("ana/lumen")),
        ("https://token@github.com/ana/lumen.git", Some("ana/lumen")),
        ("https://gitlab.com/ana/lumen.git", None),
        ("/tmp/remote.git", None),
        ("https://github.com/ana", None),
    ] {
        assert_eq!(github_slug(url).as_deref(), want, "{url}");
    }
    let d = tmp_repo("ghremote");
    git_run(&d, &["remote", "add", "local", "/tmp/x.git"]).unwrap();
    git_run(&d, &["remote", "add", "fork", "git@github.com:beto/lumen.git"]).unwrap();
    git_run(&d, &["remote", "add", "origin", "https://github.com/ana/lumen.git"]).unwrap();
    let g = github_remotes(&d, Some("origin"));
    assert_eq!(g[0], ("origin".to_string(), "ana/lumen".to_string()), "the push remote comes first");
    assert_eq!(g.len(), 2, "non-GitHub remotes are left out");
    std::fs::remove_dir_all(&d).ok();

    let plan = gitsync::ReleasePlan {
        remote: "origin".into(),
        slug: "ana/lumen".into(),
        tag: "v1.2.0".into(),
        title: "Lumen 1.2".into(),
        notes: None,
    };
    let steps = gitsync::steps_for(&Op::Release(plan.clone()), false);
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].0, "git");
    assert_eq!(steps[0].1, ["push", "--progress", "origin", "refs/tags/v1.2.0"], "the tag goes up first");
    assert_eq!(steps[1].0, "gh");
    assert_eq!(
        steps[1].1,
        [
            "release",
            "create",
            "v1.2.0",
            "--repo",
            "ana/lumen",
            "--verify-tag",
            "--title",
            "Lumen 1.2",
            "--generate-notes"
        ]
    );
    let with_notes = gitsync::ReleasePlan { notes: Some(" Fixes. ".into()), ..plan.clone() };
    assert_eq!(gitsync::release_args(&with_notes).last().map(String::as_str), Some("Fixes."));
    assert!(gitsync::release_args(&with_notes).contains(&"--notes".to_string()));
    let s = release_summary(&plan, Some("0123456789abcdef"));
    assert!(
        s.contains("github.com/ana/lumen") && s.contains("v1.2.0") && s.contains("0123456") && s.contains("generated"),
        "{s}"
    );
    let card = release_card(plan.clone(), None);
    assert!(card.warn && card.buttons.len() == 2, "outward-facing: always confirmed");
    assert_eq!(card.buttons[0].act, GitAct::Release { plan, new_tag_at: None });
}

#[test]
fn confirmation_cards_take_over_the_strip() {
    let mut gp = GitPanel::new(&std::env::temp_dir().join("lumen-gp-no-such-dir"));
    assert_eq!(gp.card(), None);
    gp.ask = Some(delete_branch_card("feat", Some(("origin".into(), "feat".into()))));
    gp.prompt_h = 110;
    assert_eq!(gp.strip_h(), 110);
    let card = gp.card().unwrap();
    assert!(card.warn);
    assert_eq!(card.buttons.len(), 3, "delete, delete + remote, cancel");
    assert_eq!(card.buttons[0].act, GitAct::DeleteBranch { name: "feat".into(), force: false, remote: None });
    assert!(matches!(&card.buttons[1].act, GitAct::DeleteBranch { remote: Some(_), .. }), "remote only on request");
    assert_eq!(card.buttons[2].act, GitAct::Dismiss);
    assert_eq!(delete_branch_card("feat", None).buttons.len(), 2);
    let forced = unmerged_card("feat", None);
    assert_eq!(forced.buttons[0].act, GitAct::DeleteBranch { name: "feat".into(), force: true, remote: None });
    assert_eq!(delete_tag_card("v1", Some("origin")).buttons.len(), 3);
    assert_eq!(undo_published_card("origin/main").buttons[0].act, GitAct::UndoCommit);
    gp.ask = None;
    assert_eq!(gp.strip_h(), 0);
    gp.state = RepoState::CherryPick;
    assert_eq!(gp.card(), Some(cherry_card()));
    assert!(gp.strip_h() >= PROMPT_H);
}

#[test]
fn commit_box_wraps_scrolls_selects_all_and_commits() {
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use winit::keyboard::ModifiersState;
    let _g = lock();
    let dir = tmp("gpbox");
    for a in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "Ana"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git_run(&dir, a).unwrap();
    }
    let mut app = test_app(&dir);
    app.size = (1200, 800);
    app.open_git_panel();
    let (w, h) = (1200, 800);
    let mut px = vec![0u32; (w * h) as usize];
    let mut frame = |app: &mut App| {
        app.render_into(&mut Canvas { px: &mut px, w, h }, 0.016);
    };
    frame(&mut app);
    let typ = |app: &mut App, s: &str| {
        for c in s.chars() {
            let t = c.to_string();
            app.handle_key(&KeyIn { key: Key::Character(t.clone().into()), text: Some(t) });
        }
    };
    let ctrl = |app: &mut App, key: Key| {
        app.mods = ModifiersState::CONTROL;
        app.handle_key(&KeyIn { key, text: None });
        app.mods = ModifiersState::empty();
    };
    {
        let gp = app.gitpanel.as_ref().unwrap();
        assert!(gp.focused && gp.msg_focus, "the box has the focus when the panel opens");
        assert_eq!(gp.msg_box.h, 3 * App::gp_msg_line() + 2 * MSG_PAD_Y, "three lines when empty");
    }
    // A long message wraps inside the box: no line is wider than it (spaces may hang).
    let long = "feat: a commit message long enough to wrap many times inside the narrow git panel ".repeat(10);
    typ(&mut app, &long);
    frame(&mut app);
    {
        let gp = app.gitpanel.as_ref().unwrap();
        assert_eq!(gp.msg.text(), long, "wrapping is visual only");
        let inner = (gp.msg_box.w - 2 * MSG_PAD_X) as f32;
        let lines = gp.msg.lines();
        assert!(lines.len() > 10, "{} visual lines", lines.len());
        for l in lines {
            let t = gp.msg.line_text(l);
            let ink = t.trim_end().chars().count();
            assert!(l.xs[ink] <= inner + 0.5, "{t:?} is {}px wide in a {inner}px box", l.xs[ink]);
        }
        let lh = App::gp_msg_line();
        assert_eq!(gp.msg_box.h, MSG_MAX_LINES * lh + 2 * MSG_PAD_Y, "the box grows up to 10 lines");
        assert!(gp.msg.scroll > 0.0 && gp.msg.scroll == gp.msg.max_scroll(), "then scrolls to the caret");
        let (k, _) = gp.msg.caret();
        assert_eq!(k, lines.len() - 1);
    }
    // The wheel over the box scrolls the box.
    let b = app.gitpanel.as_ref().unwrap().msg_box;
    app.mouse = ((b.x + 20) as f64, (b.y + 20) as f64);
    let up = winit::dpi::PhysicalPosition::new(0.0, 40.0);
    assert!(app.gitpanel_wheel(MouseScrollDelta::PixelDelta(up)), "touchpad over the box");
    let gp = app.gitpanel.as_ref().unwrap();
    assert!(gp.msg.scroll < gp.msg.max_scroll(), "scrolled up: {}", gp.msg.scroll);
    // Ctrl+A (bound to the editor's Select All) belongs to the box; typing replaces everything.
    ctrl(&mut app, Key::Character("a".into()));
    typ(&mut app, "fix: wrap the commit box");
    frame(&mut app);
    let gp = app.gitpanel.as_ref().unwrap();
    assert!(gp.msg_focus, "Ctrl+A kept the focus in the box");
    assert_eq!(gp.msg.text(), "fix: wrap the commit box");
    assert_eq!(gp.msg.line_count(), 1);
    assert_eq!(gp.msg.scroll, 0.0);
    // Enter is a new line, Ctrl+Z undoes, Ctrl+Enter commits.
    app.handle_key(&KeyIn { key: Key::Named(NamedKey::Enter), text: Some("\r".into()) });
    typ(&mut app, "oops");
    ctrl(&mut app, Key::Character("z".into()));
    ctrl(&mut app, Key::Character("z".into()));
    assert_eq!(app.gitpanel.as_ref().unwrap().msg.text(), "fix: wrap the commit box");
    ctrl(&mut app, Key::Named(NamedKey::Enter));
    let gp = app.gitpanel.as_ref().unwrap();
    assert_eq!(gp.msg.text(), "", "the box is emptied after the commit");
    assert_eq!(git_run(&dir, &["log", "-1", "--format=%s"]).unwrap().trim(), "fix: wrap the commit box");
    drop(app);
    std::fs::remove_dir_all(&dir).ok();
}
