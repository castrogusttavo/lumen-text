//! "Git: Compare with branch or commit…", "Compare two refs…" and "Compare file with…": pick a
//! ref (branches, tags, recent commits, or anything typed: `HEAD~3`, `main...`), then a list of
//! the files that differ, each opening the side-by-side diff. In the diff `]` / `[` step through
//! the files and `f` brings the list back. The list's first entry switches between "every
//! difference" and "since the branches diverged" (merge base, like a pull request).

use crate::{
    app::{App, Focus},
    app_gitx::DiffView,
    app_overlay::{Action, Item, Overlay, choice_picker, typed_picker},
    git,
    gitcmp::{self, CmpFile, RefKind, Resolved, Spec},
};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A resolved comparison and the files that differ.
#[derive(Debug)]
pub struct Compare {
    pub top: PathBuf,
    /// As picked / typed (kept to redo it with the other merge-base choice).
    pub spec: Spec,
    pub res: Resolved,
    pub files: Vec<CmpFile>,
}

/// The comparison a diff belongs to, and which of its files it shows.
#[derive(Clone, Debug)]
pub struct Nav {
    pub cmp: Rc<Compare>,
    pub at: usize,
}

/// Picker choices (`Action::Compare`).
#[derive(Clone, Debug)]
pub enum CmpPick {
    /// The working tree against this ref.
    Base(String),
    /// First ref of "Compare two refs…": the second is picked next.
    First(String),
    /// Old side, new side.
    Two(String, String),
    /// The current file against this ref.
    File(String),
    /// Open file `i` of a comparison.
    Open(Rc<Compare>, usize),
    /// Redo the comparison with the other merge-base choice.
    Flip(Rc<Compare>),
}

/// Files larger than this are listed but not diffed.
const MAX_DIFF_BYTES: u64 = 3_000_000;

impl App {
    fn cmp_top(&mut self) -> Option<PathBuf> {
        let top = git::toplevel(&self.root);
        if top.is_none() {
            self.say("This folder is not a git repository");
        }
        top
    }

    /// The ref picker: branches, remote branches, tags, the last commits, or what is typed.
    fn cmp_ref_picker(&mut self, title: &str, make: Rc<dyn Fn(&str) -> Action>) {
        let Some(top) = self.cmp_top() else { return };
        let items: Vec<Item> = gitcmp::refs(&top, 100)
            .into_iter()
            .map(|r| {
                let (label, detail) = match r.kind {
                    RefKind::Commit => (format!("{}  {}", r.short, r.subject), "commit".to_string()),
                    k => (r.name.clone(), format!("{} · {}  {}", k.word(), r.short, r.subject)),
                };
                Item { label, detail, action: make(&r.name) }
            })
            .collect();
        self.overlay = Overlay::Picker(typed_picker(title, items, make));
    }

    /// Palette: the working tree against a branch, tag or commit.
    pub fn git_compare_pick(&mut self) {
        self.cmp_ref_picker(
            "Compare the working tree with… (branch, tag, commit; main... = since diverged)",
            Rc::new(|r: &str| Action::Compare(CmpPick::Base(r.to_string()))),
        );
    }

    /// Palette: two refs (old side first).
    pub fn git_compare_two_pick(&mut self) {
        self.cmp_ref_picker(
            "Compare two refs: the old side…",
            Rc::new(|r: &str| Action::Compare(CmpPick::First(r.to_string()))),
        );
    }

    /// Palette: the current file against a ref.
    pub fn git_compare_file_pick(&mut self) {
        let Some(name) = self.docs.get(self.active).filter(|d| d.path().is_some()).map(|d| d.name.clone()) else {
            return self.say("Open a saved file to compare it");
        };
        self.cmp_ref_picker(
            &format!("Compare {name} with…"),
            Rc::new(|r: &str| Action::Compare(CmpPick::File(r.to_string()))),
        );
    }

    pub fn cmp_pick(&mut self, p: CmpPick) {
        match p {
            CmpPick::Base(r) => self.cmp_run(gitcmp::parse_spec(&r)),
            CmpPick::First(a) => {
                let title = format!("Compare {} with… (the new side)", short_ref(&a));
                self.cmp_ref_picker(
                    &title,
                    Rc::new(move |b: &str| Action::Compare(CmpPick::Two(a.clone(), b.to_string()))),
                );
            }
            CmpPick::Two(a, b) => {
                let mut spec = gitcmp::parse_spec(&a);
                spec.target = Some(b.trim().to_string());
                self.cmp_run(spec);
            }
            CmpPick::File(r) => self.cmp_file(&r),
            CmpPick::Open(c, i) => self.cmp_open(c, i),
            CmpPick::Flip(c) => {
                let mut spec = c.spec.clone();
                spec.merge_base = !spec.merge_base;
                self.cmp_run(spec);
            }
        }
    }

    /// Resolve `spec`, list what differs and show the list.
    fn cmp_run(&mut self, spec: Spec) {
        let Some(top) = self.cmp_top() else { return };
        let res = match gitcmp::resolve_spec(&top, &spec) {
            Ok(r) => r,
            Err(e) => return self.say(e),
        };
        let files = match gitcmp::changed_files(&top, &res.from, res.to.as_deref()) {
            Ok(f) => f,
            Err(e) => return self.say(format!("git: {e}")),
        };
        if files.is_empty() {
            self.say(format!("No differences between {} and {}", res.from_label, res.to_label));
        }
        self.cmp_show_files(Rc::new(Compare { top, spec, res, files }), 1);
    }

    /// The list of differing files, row `sel` selected (row 0 switches the merge-base choice).
    pub fn cmp_show_files(&mut self, c: Rc<Compare>, sel: usize) {
        let n = c.files.len();
        let title = format!("{} → {}  ·  {n} file{}", c.res.from_label, c.res.to_label, if n == 1 { "" } else { "s" });
        let other = c.spec.target.as_deref().unwrap_or("HEAD");
        let flip = if c.spec.merge_base {
            Item {
                label: format!("⇄ Show every difference with {}", c.spec.base),
                detail: "not only what changed since the branches diverged".into(),
                action: Action::Compare(CmpPick::Flip(c.clone())),
            }
        } else {
            Item {
                label: format!("⇄ Only changes since {other} diverged from {}", c.spec.base),
                detail: "start from the merge base, like a pull request".into(),
                action: Action::Compare(CmpPick::Flip(c.clone())),
            }
        };
        let mut items = vec![flip];
        items.extend(c.files.iter().enumerate().map(|(i, f)| Item {
            label: f.path.clone(),
            detail: match &f.old {
                Some(o) => format!("{} from {o}", f.what()),
                None => f.what().to_string(),
            },
            action: Action::Compare(CmpPick::Open(c.clone(), i)),
        }));
        let sel = if n == 0 { 0 } else { sel.min(n) };
        self.overlay = Overlay::Picker(choice_picker(&title, items, sel));
    }

    /// Open file `i` of comparison `c` in the side-by-side diff.
    pub fn cmp_open(&mut self, c: Rc<Compare>, i: usize) {
        let Some(f) = c.files.get(i).cloned() else { return };
        if c.res.to.is_none() && std::fs::metadata(c.top.join(&f.path)).is_ok_and(|m| m.len() > MAX_DIFF_BYTES) {
            return self.say("File too large for the side-by-side diff");
        }
        let (old, new) = gitcmp::texts(&c.top, &c.res.from, c.res.to.as_deref(), &f);
        let title = match &f.old {
            Some(o) => format!("{o} → {}", f.path),
            None => f.path.clone(),
        };
        let (left, right) = (c.res.from_label.clone(), c.res.to_label.clone());
        let mut dv = DiffView::of_texts(title, left, right, old, new, self.config.diff_ignore_whitespace);
        dv.nav = Some(Nav { cmp: c, at: i });
        self.diff = Some(dv);
        self.focus = Focus::Editor;
    }

    /// `]` / `[` / `f` in a diff that belongs to a comparison.
    pub fn cmp_nav_key(&mut self, ch: &str) {
        let Some(nav) = self.diff.as_ref().and_then(|d| d.nav.clone()) else { return };
        let n = nav.cmp.files.len();
        match ch {
            "]" if nav.at + 1 < n => self.cmp_open(nav.cmp, nav.at + 1),
            "]" => self.say("last file of the comparison"),
            "[" if nav.at > 0 => self.cmp_open(nav.cmp, nav.at - 1),
            "[" => self.say("first file of the comparison"),
            _ => self.cmp_show_files(nav.cmp, nav.at + 1),
        }
    }

    /// The current file (its buffer, unsaved edits included) against `r`. `a..b` compares the
    /// file between two refs, `r...` from the merge base.
    fn cmp_file(&mut self, r: &str) {
        let Some(d) = self.docs.get(self.active) else { return };
        let Some(path) = d.path().map(Path::to_path_buf) else { return };
        let (buf, name) = (d.buf.to_string(), d.name.clone());
        let Some(top) = path.parent().and_then(git::toplevel) else {
            return self.say("This file is not in a git repository");
        };
        let rel = match path.strip_prefix(&top) {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(_) => return self.say("This file is not in a git repository"),
        };
        let res = match gitcmp::resolve_spec(&top, &gitcmp::parse_spec(r)) {
            Ok(x) => x,
            Err(e) => return self.say(e),
        };
        let old = gitcmp::text_at(&top, &res.from, &rel);
        if old.is_none() {
            self.say(format!("{name} does not exist in {}: all of it shows as added", res.from_label));
        }
        let (new, right) = match &res.to {
            Some(t) => (gitcmp::text_at(&top, t, &rel).unwrap_or_default(), res.to_label.clone()),
            None => (buf, "Current".to_string()),
        };
        let old = old.unwrap_or_default();
        if old == new {
            return self.say(format!("{name}: no differences with {}", res.from_label));
        }
        let ws = self.config.diff_ignore_whitespace;
        self.diff = Some(DiffView::of_texts(rel, res.from_label, right, old, new, ws));
        self.focus = Focus::Editor;
    }
}

/// A ref for titles: short hashes for full ones.
fn short_ref(r: &str) -> &str {
    if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) { gitcmp::short(r) } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_gitpanel::git_run;
    use crate::app_overlay::Picker;
    use crate::app_unsaved::testkit::{lock, test_app, tmp};
    use crate::gitx::RowKind;

    /// main: base -> m1; feature: base -> f1 (checked out), plus an uncommitted edit.
    fn forked(d: &Path) {
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "T"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git_run(d, a).unwrap();
        }
        let commit = |files: &[(&str, &str)], msg: &str| {
            for (p, c) in files {
                std::fs::write(d.join(p), c).unwrap();
            }
            git_run(d, &["add", "-A"]).unwrap();
            git_run(d, &["commit", "-q", "-m", msg]).unwrap();
        };
        // The test app writes its own files into the folder: keep them out of the comparisons.
        commit(&[("a.txt", "a\n"), ("b.txt", "b\n"), (".gitignore", "/*\n!/a.txt\n!/b.txt\n!/.gitignore\n")], "base");
        git_run(d, &["checkout", "-q", "-b", "feature"]).unwrap();
        commit(&[("b.txt", "b\nfeature\n")], "f1");
        git_run(d, &["checkout", "-q", "main"]).unwrap();
        commit(&[("a.txt", "A on main\n")], "m1");
        git_run(d, &["checkout", "-q", "feature"]).unwrap();
        std::fs::write(d.join("b.txt"), "b\nfeature\nwip\n").unwrap();
    }

    fn picker(app: &App) -> &Picker {
        match &app.overlay {
            Overlay::Picker(p) => p,
            _ => panic!("no picker open"),
        }
    }

    fn labels(app: &App) -> Vec<String> {
        let p = picker(app);
        p.filtered.iter().map(|(i, _)| p.items[*i].label.clone()).collect()
    }

    fn type_in(app: &mut App, text: &str) {
        if let Overlay::Picker(p) = &mut app.overlay {
            p.field.insert(text);
            p.refilter();
        }
    }

    #[test]
    fn compare_working_tree_with_a_branch_then_since_diverged() {
        let _g = lock();
        let dir = tmp("cmpwt");
        forked(&dir);
        let mut app = test_app(&dir);

        app.git_compare_pick();
        let l = labels(&app);
        assert!(l.contains(&"main".to_string()) && l.contains(&"feature".to_string()), "{l:?}");
        assert!(l.iter().any(|x| x.ends_with("  f1")), "recent commits are listed: {l:?}");
        type_in(&mut app, "main");
        assert_eq!(labels(&app)[0], "main", "a ref that starts with the text wins over the typed item");
        app.overlay_accept();

        // Every difference with main: a.txt (main's own change) and b.txt.
        let l = labels(&app);
        assert!(l[0].starts_with("⇄ Only changes since HEAD diverged from main"), "{l:?}");
        assert_eq!(&l[1..], ["a.txt", "b.txt"]);
        assert!(picker(&app).title.starts_with("main → Working tree"), "{}", picker(&app).title);
        app.overlay_accept(); // row 1 is preselected: a.txt
        let dv = app.diff.as_ref().expect("diff open");
        assert_eq!(
            (dv.left_name.as_str(), dv.right_name.as_str(), dv.title.as_str()),
            ("main", "Working tree", "a.txt")
        );
        assert_eq!(dv.nav.as_ref().map(|n| n.at), Some(0));
        assert_eq!(dv.rows[0].kind, RowKind::Changed);

        // `]` goes to b.txt, `[` back, `f` shows the list with the current file selected.
        app.diff_key(&winit::keyboard::Key::Character("]".into()));
        assert_eq!(app.diff.as_ref().unwrap().title, "b.txt");
        app.diff_key(&winit::keyboard::Key::Character("]".into()));
        assert_eq!(app.diff.as_ref().unwrap().title, "b.txt", "stays on the last file");
        app.diff_key(&winit::keyboard::Key::Character("f".into()));
        assert_eq!(picker(&app).sel, 2);

        // The first row switches to "since diverged": a.txt (changed on main only) is gone.
        if let Overlay::Picker(p) = &mut app.overlay {
            p.sel = 0;
        }
        app.overlay_accept();
        let l = labels(&app);
        assert!(l[0].starts_with("⇄ Show every difference with main"), "{l:?}");
        assert_eq!(&l[1..], ["b.txt"]);
        assert!(picker(&app).title.starts_with("merge base of main ("));
        app.overlay_accept();
        let dv = app.diff.as_ref().unwrap();
        let added: Vec<_> = dv.rows.iter().filter(|r| r.kind == RowKind::Added).collect();
        assert_eq!(added.len(), 2, "feature + wip, relative to the fork point");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typed_refs_two_refs_and_one_file() {
        let _g = lock();
        let dir = tmp("cmptwo");
        forked(&dir);
        let mut app = test_app(&dir);

        // Two refs, the second one typed as an expression.
        app.git_compare_two_pick();
        type_in(&mut app, "main");
        app.overlay_accept();
        assert!(picker(&app).title.starts_with("Compare main with"));
        type_in(&mut app, "feature~0");
        assert_eq!(labels(&app)[0], "Use “feature~0”", "nothing starts with it: the typed item comes first");
        app.overlay_accept();
        let l = labels(&app);
        assert_eq!(&l[1..], ["a.txt", "b.txt"]);
        assert!(picker(&app).title.starts_with("main → feature~0"));

        // An unknown ref says so and opens nothing.
        app.overlay = Overlay::None;
        app.cmp_pick(CmpPick::Base("no-such-ref".into()));
        assert!(app.overlay.is_none());

        // The current file (its buffer) against main.
        app.open_path(&dir.join("a.txt"), None);
        app.git_compare_file_pick();
        assert!(picker(&app).title.starts_with("Compare a.txt with"));
        type_in(&mut app, "main");
        app.overlay_accept();
        let dv = app.diff.as_ref().expect("file diff");
        assert_eq!((dv.left_name.as_str(), dv.right_name.as_str()), ("main", "Current"));
        assert!(dv.nav.is_none());
        assert_eq!(dv.rows.len(), 1);
        assert_eq!(dv.rows[0].left.as_ref().map(|l| l.1.as_str()), Some("A on main"));
        assert_eq!(dv.rows[0].right.as_ref().map(|l| l.1.as_str()), Some("a"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
