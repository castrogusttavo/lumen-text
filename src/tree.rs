//! File tree model for the sidebar: lazily read directories, hidden junk folders.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
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
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        let mut t = Self {
            root: root.clone(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            sel: 0,
            scroll: 0.0,
            hscroll: 0.0,
            selected: HashSet::new(),
            anchor: None,
            drag: None,
        };
        t.expanded.insert(root);
        t.rebuild();
        t
    }

    fn entries(dir: &Path) -> Vec<(String, PathBuf, bool)> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut v: Vec<(String, PathBuf, bool)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir =
                    e.file_type().map(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir())).unwrap_or(false);
                if is_dir && HIDDEN_DIRS.contains(&name.as_str()) {
                    return None;
                }
                Some((name, e.path(), is_dir))
            })
            .collect();
        v.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        v
    }

    pub fn rebuild(&mut self) {
        let mut rows = Vec::new();
        self.walk(&self.root.clone(), 0, &mut rows);
        self.rows = rows;
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        for (name, path, is_dir) in Self::entries(dir) {
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(Row { path: path.clone(), name, depth, is_dir, expanded });
            if expanded {
                self.walk(&path, depth + 1, out);
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
}
