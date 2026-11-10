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
fn message_editing_is_multiline() {
    let mut f = TextField::default();
    ml_insert(&mut f, "titulo");
    ml_insert(&mut f, "\n\ncorpo ç\r\nfim\tx");
    assert_eq!(f.text, "titulo\n\ncorpo ç\nfim    x");
    assert_eq!(ml_pos(&f), (3, 8));
    ml_vertical(&mut f, -1);
    assert_eq!(ml_pos(&f), (2, 7), "clamped to the shorter line");
    ml_vertical(&mut f, -1);
    assert_eq!(ml_pos(&f), (1, 0), "short line clamps the column");
    ml_vertical(&mut f, -1);
    assert_eq!(ml_pos(&f), (0, 0));
    ml_vertical(&mut f, -1);
    assert_eq!(f.cursor, 0, "up on the first line goes home");
    ml_end(&mut f);
    assert_eq!(ml_pos(&f), (0, 6));
    ml_vertical(&mut f, 1);
    ml_home(&mut f);
    assert_eq!(ml_pos(&f), (1, 0));
    assert!(f.backspace(), "backspace joins lines");
    assert_eq!(f.text, "titulo\ncorpo ç\nfim    x");
    assert_eq!(ml_index(&f, 9, 99), f.len(), "clamped to the end");
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
    assert_eq!(rows.len(), 6, "3 headers + 3 files: {rows:?}");
    assert!(matches!(rows[0], Row::Header(Group::Staged)));
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
    assert_eq!(gp.commits.len(), 1);
    assert!(gp.log_done, "a short page is the last one");
    gp.open_commit = Some(0);
    gp.files = commit_files(gp.top.as_ref().unwrap(), &gp.commits[0].hash);
    assert_eq!(gp.rows(), vec![Row::Commit(0), Row::CFile(0)]);
    gp.reset_history();
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
        vec![Row::Header(Group::Staged), Row::File(0), Row::Header(Group::Changes), Row::File(1)],
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
