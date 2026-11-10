//! File tree model for the sidebar: lazily read directories, hidden junk folders.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

const HIDDEN_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", ".next", ".turbo", ".cache", "__pycache__", ".stryker"];

#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

pub struct Tree {
    pub root: PathBuf,
    expanded: HashSet<PathBuf>,
    pub rows: Vec<Row>,
    pub sel: usize,
    pub scroll: f64,
    /// Horizontal scroll of the sidebar in px (long names, deep indentation).
    pub hscroll: f64,
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        let mut t = Self { root: root.clone(), expanded: HashSet::new(), rows: Vec::new(), sel: 0, scroll: 0.0, hscroll: 0.0 };
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
                let is_dir = e.file_type().map(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir())).unwrap_or(false);
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
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.sel = i;
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
}
