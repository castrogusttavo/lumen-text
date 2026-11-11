//! File tree model for the sidebar: lazily read directories, hidden junk folders, and the
//! explorer order (`explorer_sort`: name / type / modified / mixed, folders first or not, numbers
//! compared by value so `file2` comes before `file10`).

use std::{
    cmp::Ordering,
    collections::HashSet,
    path::{Path, PathBuf},
    time::SystemTime,
};

const HIDDEN_DIRS: &[&str] =
    &[".git", "node_modules", "target", "dist", ".next", ".turbo", ".cache", "__pycache__", ".stryker"];

#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    /// Git ignores it (or it is one of the junk folders): drawn dimmed, shown only when the explorer
    /// does not hide ignored entries.
    pub ignored: bool,
}

/// A mouse press on a row that may turn into a drag-and-drop of the selection.
#[derive(Clone, Debug)]
pub struct Drag {
    /// Where the button went down.
    pub origin: (f64, f64),
    /// What is being dragged (the selection at press time, without nested duplicates).
    pub paths: Vec<PathBuf>,
    /// The pointer moved far enough: it is a drag, not a click.
    pub active: bool,
    /// The press landed on a row of a multi-selection: a plain click (no drag) collapses the
    /// selection to that row on release.
    pub click_row: Option<usize>,
    /// Folder that the press toggled open/closed; a drag puts it back.
    pub toggled: Option<PathBuf>,
    /// Folder the item would land in (None = not a valid drop spot).
    pub over: Option<PathBuf>,
    /// Ctrl held at the last move: copy instead of move.
    pub copy: bool,
}

pub struct Tree {
    pub root: PathBuf,
    expanded: HashSet<PathBuf>,
    pub rows: Vec<Row>,
    /// Cursor row (keyboard focus, and the selection when `selected` is empty).
    pub sel: usize,
    pub scroll: f64,
    /// Horizontal scroll of the sidebar in px (long names, deep indentation).
    pub hscroll: f64,
    /// Explicit multi-selection (Ctrl/Shift+click); empty = just the cursor row.
    pub selected: HashSet<PathBuf>,
    /// Fixed end of a Shift range.
    anchor: Option<PathBuf>,
    pub drag: Option<Drag>,
    /// What `.gitignore` and friends ignore (cached per folder).
    filter: crate::ignore_filter::IgnoreFilter,
    /// Leave ignored entries out of the list (off: show them dimmed).
    hide_ignored: bool,
    /// Order of the entries of each folder.
    sort: SortMode,
    folders_first: bool,
}

/// How the explorer orders the entries of a folder (`explorer_sort`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortMode {
    /// By name (natural order), folders first unless `explorer_folders_first` is off.
    #[default]
    Name,
    /// By extension, then by name.
    Type,
    /// Newest first.
    Modified,
    /// Folders and files interleaved by name, whatever `explorer_folders_first` says.
    Mixed,
}

impl SortMode {
    pub fn from_name(s: &str) -> SortMode {
        match s {
            "type" => SortMode::Type,
            "modified" => SortMode::Modified,
            "mixed" => SortMode::Mixed,
            _ => SortMode::Name,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Type => "type",
            SortMode::Modified => "modified",
            SortMode::Mixed => "mixed",
        }
    }
}

/// One directory entry as listed (before it becomes a [`Row`]).
#[derive(Clone, Debug)]
pub struct Listed {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub ignored: bool,
    /// Modification time, read only when sorting by it.
    pub mtime: Option<SystemTime>,
}

/// Case-insensitive "natural" order: runs of digits compare by value (`file2` < `file10`,
/// `v1.9` < `v1.10`); equal-looking names fall back to a plain comparison so the order is total.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let (rx, ry) = (digits(&mut x), digits(&mut y));
                let (nx, ny) = (rx.trim_start_matches('0'), ry.trim_start_matches('0'));
                let o = nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// The run of ASCII digits at the front of `it` (consumed).
fn digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(ch) = it.peek().copied().filter(char::is_ascii_digit) {
        s.push(ch);
        it.next();
    }
    s
}

/// Lowercased extension used by the "type" order ("" for folders and dot files).
fn type_of(e: &Listed) -> String {
    if e.is_dir {
        return String::new();
    }
    Path::new(&e.name).extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Sort the entries of one folder.
pub fn sort_listed(v: &mut [Listed], mode: SortMode, folders_first: bool) {
    v.sort_by(|a, b| {
        let dirs = if folders_first && mode != SortMode::Mixed { b.is_dir.cmp(&a.is_dir) } else { Ordering::Equal };
        dirs.then_with(|| match mode {
            SortMode::Name | SortMode::Mixed => natural_cmp(&a.name, &b.name),
            SortMode::Type => natural_cmp(&type_of(a), &type_of(b)).then_with(|| natural_cmp(&a.name, &b.name)),
            SortMode::Modified => b.mtime.cmp(&a.mtime).then_with(|| natural_cmp(&a.name, &b.name)),
        })
    });
}

impl Tree {
    #[cfg(test)]
    pub fn new(root: PathBuf) -> Self {
        Self::with_ignore(root, true)
    }

    pub fn with_ignore(root: PathBuf, hide_ignored: bool) -> Self {
        let mut t = Self {
            filter: crate::ignore_filter::IgnoreFilter::new(&root),
            hide_ignored,
            root: root.clone(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            sel: 0,
            scroll: 0.0,
            hscroll: 0.0,
            selected: HashSet::new(),
            anchor: None,
            drag: None,
            sort: SortMode::Name,
            folders_first: true,
        };
        t.expanded.insert(root);
        t.rebuild();
        t
    }

    /// A tree that follows the explorer settings of `c` (ignored files, order).
    pub fn for_config(root: PathBuf, c: &crate::config::Config) -> Self {
        let mut t = Self::with_ignore(root, c.explorer_hide_gitignored);
        t.set_sort(SortMode::from_name(&c.view.explorer_sort), c.view.explorer_folders_first);
        t
    }

    /// Change the order of the entries (re-reads the open folders when it changed).
    pub fn set_sort(&mut self, mode: SortMode, folders_first: bool) {
        if (self.sort, self.folders_first) != (mode, folders_first) {
            self.sort = mode;
            self.folders_first = folders_first;
            self.rebuild();
        }
    }

    pub fn sort(&self) -> SortMode {
        self.sort
    }

    pub fn folders_first(&self) -> bool {
        self.folders_first
    }

    /// Hide or show (dimmed) the ignored entries.
    pub fn set_hide_ignored(&mut self, hide: bool) {
        if self.hide_ignored != hide {
            self.hide_ignored = hide;
            self.rebuild();
        }
    }

    /// An ignore file changed: re-read the rules on the next rebuild.
    pub fn invalidate_ignore(&mut self) {
        self.filter.invalidate();
    }

    /// The entries to list in `dir`, in the explorer order: `.git` never, ignored ones only when
    /// they are shown.
    fn entries(&self, dir: &Path, parent_ignored: bool) -> Vec<Listed> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let by_time = self.sort == SortMode::Modified;
        let mut v: Vec<Listed> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir =
                    e.file_type().map(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir())).unwrap_or(false);
                if is_dir && name == ".git" {
                    return None;
                }
                let path = e.path();
                let ignored = parent_ignored
                    || (is_dir && HIDDEN_DIRS.contains(&name.as_str()))
                    || self.filter.is_ignored(&path, is_dir);
                if ignored && self.hide_ignored {
                    return None;
                }
                let mtime = if by_time { e.metadata().and_then(|m| m.modified()).ok() } else { None };
                Some(Listed { name, path, is_dir, ignored, mtime })
            })
            .collect();
        sort_listed(&mut v, self.sort, self.folders_first);
        v
    }

    pub fn rebuild(&mut self) {
        let mut rows = Vec::new();
        self.walk(&self.root.clone(), 0, false, &mut rows);
        self.rows = rows;
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    fn walk(&self, dir: &Path, depth: usize, parent_ignored: bool, out: &mut Vec<Row>) {
        for Listed { name, path, is_dir, ignored, .. } in self.entries(dir, parent_ignored) {
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(Row { path: path.clone(), name, depth, is_dir, expanded, ignored });
            if expanded {
                self.walk(&path, depth + 1, ignored, out);
            }
        }
    }

    pub fn toggle(&mut self, i: usize) {
        let Some(r) = self.rows.get(i) else { return };
        if !r.is_dir {
            return;
        }
        let p = r.path.clone();
        if !self.expanded.remove(&p) {
            self.expanded.insert(p);
        }
        self.rebuild();
    }

    /// Expand every ancestor of `path` and select it.
    pub fn reveal(&mut self, path: &Path) {
        let mut cur = path.parent();
        while let Some(d) = cur {
            if d.starts_with(&self.root) {
                self.expanded.insert(d.to_path_buf());
            }
            if d == self.root {
                break;
            }
            cur = d.parent();
        }
        self.rebuild();
        self.selected.clear();
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.sel = i;
            self.anchor = Some(path.to_path_buf());
        }
    }

    // ── multi-selection ──

    /// Is row `k` drawn as selected?
    pub fn row_selected(&self, k: usize) -> bool {
        if self.selected.is_empty() {
            k == self.sel
        } else {
            self.rows.get(k).is_some_and(|r| self.selected.contains(&r.path))
        }
    }

    pub fn is_selected(&self, p: &Path) -> bool {
        if self.selected.is_empty() {
            self.rows.get(self.sel).is_some_and(|r| r.path == p)
        } else {
            self.selected.contains(p)
        }
    }

    /// The selected paths in row order. Items inside another selected folder are left out
    /// (copying the folder already copies them).
    pub fn selection(&self) -> Vec<PathBuf> {
        let all: Vec<PathBuf> = if self.selected.is_empty() {
            self.rows.get(self.sel).map(|r| r.path.clone()).into_iter().collect()
        } else {
            self.rows.iter().filter(|r| self.selected.contains(&r.path)).map(|r| r.path.clone()).collect()
        };
        prune_nested(all)
    }

    /// Plain click: only row `i`.
    pub fn select_only(&mut self, i: usize) {
        self.selected.clear();
        self.anchor = self.rows.get(i).map(|r| r.path.clone());
        self.sel = i.min(self.rows.len().saturating_sub(1));
    }

    /// Ctrl+click: toggle row `i` in the selection.
    pub fn toggle_select(&mut self, i: usize) {
        let Some(path) = self.rows.get(i).map(|r| r.path.clone()) else { return };
        if self.selected.is_empty() {
            // The cursor row was the implicit selection: it joins before the toggle.
            if let Some(cur) = self.rows.get(self.sel) {
                self.selected.insert(cur.path.clone());
            }
        }
        if !self.selected.remove(&path) {
            self.selected.insert(path.clone());
        }
        self.sel = i;
        self.anchor = Some(path);
    }

    /// Shift+click: everything between the anchor and row `i`.
    pub fn select_range(&mut self, i: usize) {
        if self.rows.is_empty() {
            return;
        }
        let i = i.min(self.rows.len() - 1);
        let a = self
            .anchor
            .as_ref()
            .and_then(|p| self.rows.iter().position(|r| &r.path == p))
            .unwrap_or(self.sel.min(self.rows.len() - 1));
        let (lo, hi) = (a.min(i), a.max(i));
        self.anchor = Some(self.rows[a].path.clone());
        self.selected = self.rows[lo..=hi].iter().map(|r| r.path.clone()).collect();
        self.sel = i;
    }

    pub fn select_all(&mut self) {
        self.selected = self.rows.iter().map(|r| r.path.clone()).collect();
    }

    /// Select exactly `paths` (those that have a row); the cursor goes to the last one.
    pub fn select_paths(&mut self, paths: &[PathBuf]) {
        self.selected = paths.iter().filter(|p| self.rows.iter().any(|r| &r.path == *p)).cloned().collect();
        if let Some(i) = paths.last().and_then(|l| self.rows.iter().position(|r| &r.path == l)) {
            self.sel = i;
            self.anchor = Some(self.rows[i].path.clone());
        }
        if self.selected.len() == 1 {
            self.selected.clear();
        }
    }

    /// Esc: back to the single cursor row. True when there was a multi-selection to drop.
    pub fn clear_selection(&mut self) -> bool {
        let multi = self.selection().len() > 1;
        self.selected.clear();
        multi
    }

    /// Arrow key: move the cursor; with `extend` (Shift) the range from the anchor follows it.
    pub fn step(&mut self, delta: isize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = self.rows.get(self.sel).map(|r| r.path.clone());
            }
            let n = self.rows.len() as isize;
            if n > 0 {
                let to = (self.sel as isize + delta).clamp(0, n - 1) as usize;
                self.select_range(to);
            }
        } else {
            self.selected.clear();
            self.move_sel(delta);
            self.anchor = self.rows.get(self.sel).map(|r| r.path.clone());
        }
    }

    pub fn move_sel(&mut self, delta: isize) {
        let n = self.rows.len() as isize;
        if n > 0 {
            self.sel = (self.sel as isize + delta).clamp(0, n - 1) as usize;
        }
    }

    pub fn collapse_or_parent(&mut self) {
        let Some(r) = self.rows.get(self.sel).cloned() else { return };
        if r.is_dir && r.expanded {
            self.toggle(self.sel);
        } else if let Some(i) = self.rows.iter().position(|x| Some(x.path.as_path()) == r.path.parent()) {
            self.sel = i;
        }
    }
}

/// Drop every path that lives inside another path of the list.
pub fn prune_nested(all: Vec<PathBuf>) -> Vec<PathBuf> {
    all.iter().filter(|p| !all.iter().any(|q| q != *p && p.starts_with(q))).cloned().collect()
}

/// Can `paths` be dropped into folder `dir`? Not into themselves or their own subfolders, and not
/// when every one of them already lives there (nothing would move).
pub fn drop_valid(paths: &[PathBuf], dir: &Path) -> bool {
    !paths.is_empty() && paths.iter().all(|p| !dir.starts_with(p)) && paths.iter().any(|p| p.parent() != Some(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_dirs_first_hides_junk_and_expands() {
        let root = std::env::temp_dir().join(format!("lumen-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("b.ts"), "").unwrap();
        std::fs::write(root.join("A.ts"), "").unwrap();
        std::fs::write(root.join("src/m.ts"), "").unwrap();
        let mut t = Tree::new(root.clone());
        let names: Vec<_> = t.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["src", "A.ts", "b.ts"], "dirs first, case-insensitive, node_modules hidden");
        t.toggle(0);
        let names: Vec<_> = t.rows.iter().map(|r| (r.name.as_str(), r.depth)).collect();
        assert_eq!(names, [("src", 0), ("deep", 1), ("m.ts", 1), ("A.ts", 0), ("b.ts", 0)]);
        t.reveal(&root.join("src/m.ts"));
        assert_eq!(t.rows[t.sel].name, "m.ts");
        t.collapse_or_parent();
        assert_eq!(t.rows[t.sel].name, "src");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gitignored_entries_are_hidden_or_shown_dimmed_and_ignored_folders_are_not_read() {
        let root = std::env::temp_dir().join(format!("lumen-treeign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("build/inner")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n*.log\n").unwrap();
        for f in ["a.ts", "debug.log", "build/out.js", "src/x.log", "src/y.ts"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let mut t = Tree::new(root.clone());
        t.toggle(0); // src
        let names: Vec<_> = t.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["src", "y.ts", ".gitignore", "a.ts"], "build/, *.log and .git are gone");
        assert!(t.rows.iter().all(|r| !r.ignored));
        t.set_hide_ignored(false);
        let seen: Vec<_> = t.rows.iter().map(|r| (r.name.as_str(), r.ignored)).collect();
        assert_eq!(
            seen,
            [
                ("build", true),
                ("src", false),
                ("x.log", true),
                ("y.ts", false),
                (".gitignore", false),
                ("a.ts", false),
                ("debug.log", true)
            ],
            "shown, flagged, and .git still hidden"
        );
        // Opening an ignored folder lists its content as ignored too.
        let b = t.rows.iter().position(|r| r.name == "build").unwrap();
        t.toggle(b);
        assert!(t.rows.iter().filter(|r| r.depth == 1 && r.path.starts_with(root.join("build"))).all(|r| r.ignored));
        // Editing .gitignore is picked up after an invalidation (the watcher does it).
        std::fs::write(root.join(".gitignore"), "a.ts\n").unwrap();
        t.invalidate_ignore();
        t.set_hide_ignored(true);
        assert!(t.rows.iter().all(|r| r.name != "a.ts") && t.rows.iter().any(|r| r.name == "debug.log"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn junk_folders_stay_out_of_the_default_list_without_a_gitignore() {
        let root = std::env::temp_dir().join(format!("lumen-treejunk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let mut t = Tree::new(root.clone());
        assert_eq!(t.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["src"]);
        t.set_hide_ignored(false);
        assert_eq!(
            t.rows.iter().map(|r| (r.name.as_str(), r.ignored)).collect::<Vec<_>>(),
            [("node_modules", true), ("src", false)]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    fn sample_tree(tag: &str) -> (PathBuf, Tree) {
        let root = std::env::temp_dir().join(format!("lumen-treesel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("d1")).unwrap();
        std::fs::create_dir_all(root.join("d2")).unwrap();
        for f in ["a.ts", "b.ts", "c.ts", "d1/x.ts"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let mut t = Tree::new(root.clone());
        t.toggle(0); // d1
        (root, t)
    }

    fn names(t: &Tree) -> Vec<String> {
        t.selection().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn ctrl_shift_click_and_keyboard_selection() {
        let (root, mut t) = sample_tree("sel");
        // rows: d1, x.ts, d2, a.ts, b.ts, c.ts
        assert_eq!(
            t.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["d1", "x.ts", "d2", "a.ts", "b.ts", "c.ts"]
        );
        t.select_only(3);
        assert_eq!(names(&t), ["a.ts"]);
        t.toggle_select(5);
        assert_eq!(names(&t), ["a.ts", "c.ts"], "ctrl+click adds, the cursor row joins first");
        t.toggle_select(3);
        assert_eq!(names(&t), ["c.ts"], "ctrl+click again removes");
        assert!(t.row_selected(5) && !t.row_selected(3));
        t.select_only(2);
        t.select_range(4);
        assert_eq!(names(&t), ["d2", "a.ts", "b.ts"], "shift+click: anchor..row");
        t.select_range(3);
        assert_eq!(names(&t), ["d2", "a.ts"], "the anchor stays when shift+clicking again");
        t.select_only(5);
        t.select_range(3);
        assert_eq!(names(&t), ["a.ts", "b.ts", "c.ts"], "ranges also go upwards");
        // Shift+arrows extend from the anchor, plain arrows collapse.
        t.select_only(3);
        t.step(1, true);
        t.step(1, true);
        assert_eq!(names(&t), ["a.ts", "b.ts", "c.ts"]);
        t.step(-1, true);
        assert_eq!(names(&t), ["a.ts", "b.ts"]);
        t.step(-1, false);
        assert_eq!(names(&t), ["a.ts"]);
        assert!(!t.clear_selection(), "a single row is no multi-selection");
        t.select_all();
        assert_eq!(t.selection().len(), 5, "d1 d2 a b c: x.ts is inside the selected d1 and is dropped");
        assert!(t.clear_selection());
        assert_eq!(names(&t), [t.rows[t.sel].name.clone()]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nested_selection_is_pruned_and_drops_are_validated() {
        let (root, mut t) = sample_tree("prune");
        t.select_only(0);
        t.toggle_select(1);
        assert_eq!(names(&t), ["d1"], "x.ts is inside d1");
        let p = |s: &str| root.join(s);
        assert!(drop_valid(&[p("a.ts")], &p("d2")));
        assert!(!drop_valid(&[p("a.ts")], &root), "already there");
        assert!(drop_valid(&[p("a.ts"), p("d1/x.ts")], &root), "one of them would move");
        assert!(!drop_valid(&[p("d1")], &p("d1")), "into itself");
        assert!(!drop_valid(&[p("d1")], &p("d1/sub")), "into its own subfolder");
        assert!(!drop_valid(&[], &p("d2")));
        t.select_paths(&[p("a.ts"), p("b.ts")]);
        assert_eq!(names(&t), ["a.ts", "b.ts"]);
        assert_eq!(t.rows[t.sel].name, "b.ts");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn natural_order_compares_numbers_by_value() {
        let mut v = vec!["file10", "File2", "file1", "file02", "a", "file2b", "v1.10", "v1.9", "B"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "B", "file1", "File2", "file02", "file2b", "file10", "v1.9", "v1.10"]);
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
        assert_ne!(natural_cmp("File2", "file2"), Ordering::Equal, "total order");
    }

    fn listed(name: &str, is_dir: bool, age: u64) -> Listed {
        let mtime = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 - age));
        Listed { name: name.into(), path: PathBuf::from(name), is_dir, ignored: false, mtime }
    }

    fn order(mode: SortMode, folders_first: bool) -> Vec<String> {
        let mut v = vec![
            listed("b.rs", false, 5),
            listed("src", true, 1),
            listed("a10.ts", false, 2),
            listed("a9.ts", false, 9),
            listed("docs", true, 7),
            listed("Makefile", false, 3),
            listed("c.md", false, 0),
        ];
        sort_listed(&mut v, mode, folders_first);
        v.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn sort_orders() {
        assert_eq!(order(SortMode::Name, true), ["docs", "src", "a9.ts", "a10.ts", "b.rs", "c.md", "Makefile"]);
        assert_eq!(order(SortMode::Name, false), ["a9.ts", "a10.ts", "b.rs", "c.md", "docs", "Makefile", "src"]);
        assert_eq!(
            order(SortMode::Mixed, true),
            ["a9.ts", "a10.ts", "b.rs", "c.md", "docs", "Makefile", "src"],
            "mixed ignores folders-first"
        );
        assert_eq!(order(SortMode::Type, true), ["docs", "src", "Makefile", "c.md", "b.rs", "a9.ts", "a10.ts"]);
        assert_eq!(order(SortMode::Modified, true), ["src", "docs", "c.md", "a10.ts", "Makefile", "b.rs", "a9.ts"]);
        assert_eq!(order(SortMode::Modified, false), ["c.md", "src", "a10.ts", "Makefile", "b.rs", "docs", "a9.ts"]);
        for m in [SortMode::Name, SortMode::Type, SortMode::Modified, SortMode::Mixed] {
            assert_eq!(SortMode::from_name(m.name()), m);
        }
        assert_eq!(SortMode::from_name("size"), SortMode::Name);
    }

    #[test]
    fn tree_follows_the_sort_setting() {
        let root = std::env::temp_dir().join(format!("lumen-treesort-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("z")).unwrap();
        for f in ["f10.txt", "f2.txt", "a.rs"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let mut t = Tree::new(root.clone());
        let names = |t: &Tree| t.rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&t), ["z", "a.rs", "f2.txt", "f10.txt"]);
        t.set_sort(SortMode::Type, true);
        assert_eq!(names(&t), ["z", "a.rs", "f2.txt", "f10.txt"]);
        t.set_sort(SortMode::Mixed, true);
        assert_eq!(names(&t), ["a.rs", "f2.txt", "f10.txt", "z"]);
        assert_eq!((t.sort(), t.folders_first()), (SortMode::Mixed, true));
        let mut c = crate::config::Config::default();
        c.view.explorer_sort = "name".into();
        c.view.explorer_folders_first = false;
        assert_eq!(names(&Tree::for_config(root.clone(), &c)), ["a.rs", "f2.txt", "f10.txt", "z"]);
        std::fs::remove_dir_all(&root).ok();
    }
}
